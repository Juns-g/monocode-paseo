//! The provider and model a new conversation starts with. One resolver, so
//! every way of starting a chat agrees.
//!
//! Order: project default, Settings default, last choice (`recent_models[0]`),
//! then Codex (or the first allowed provider). A step is skipped when its
//! provider is hidden. Hidden providers are ignored for remote projects, as
//! the model picker ignores them there.
use super::*;
use crate::settings::Preferences;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewChatChoice {
    pub kind: Kind,
    pub model: Option<String>,
    /// The model is unknown until the host's catalog is read: the worker that
    /// creates the session fetches it first.
    pub verify_catalog: bool,
}

impl NewChatChoice {
    /// A provider picked by the caller: the model follows project then Settings.
    pub(crate) fn explicit(prefs: &Preferences, cwd: &str, kind: Kind) -> Self {
        Self {
            kind,
            model: default_model(prefs, cwd, kind),
            verify_catalog: false,
        }
    }
    fn of(kind: Kind, model: Option<String>) -> Self {
        Self {
            kind,
            model,
            verify_catalog: false,
        }
    }
}

/// Everything the resolver reads, so it runs without a window.
pub(crate) struct Inputs<'a> {
    pub prefs: &'a Preferences,
    pub cwd: &'a str,
    pub fork: Option<fork::Layer>,
}

/// The project's model for the provider, else the Settings default.
fn default_model(prefs: &Preferences, cwd: &str, kind: Kind) -> Option<String> {
    prefs
        .project_providers
        .get(cwd)
        .and_then(|p| p.models.get(kind.id()))
        .or(prefs.default_models.get(kind.id()))
        .cloned()
}

impl Inputs<'_> {
    fn allowed(&self, kind: Kind) -> bool {
        let remote = monocode_core::remote::parse_remote_path(self.cwd).is_some();
        (remote
            || !self
                .prefs
                .hidden_for(self.cwd)
                .iter()
                .any(|id| id == kind.id()))
            && self
                .fork
                .as_ref()
                .is_none_or(|layer| layer.allows(self, kind))
    }
}

pub(crate) fn resolve(inputs: &Inputs) -> NewChatChoice {
    let prefs = inputs.prefs;
    let cwd = inputs.cwd;
    // 1. The project's provider.
    // 2. The Settings provider.
    for id in [
        prefs
            .project_providers
            .get(cwd)
            .and_then(|p| p.provider.as_deref()),
        prefs.default_provider.as_deref(),
    ] {
        if let Some(kind) = id.and_then(Kind::parse).filter(|k| inputs.allowed(*k)) {
            return NewChatChoice::of(kind, default_model(prefs, cwd, kind));
        }
    }
    // Fork-only: the conversation being worked in.
    if let Some(choice) = inputs.fork.as_ref().and_then(|layer| layer.seed(inputs)) {
        return choice;
    }
    // 3. The last model picked anywhere.
    if let Some(recent) = prefs.recent_models.first()
        && let Some(kind) = Kind::parse(&recent.provider).filter(|k| inputs.allowed(*k))
    {
        return NewChatChoice::of(
            kind,
            default_model(prefs, cwd, kind).or_else(|| recent.model.clone()),
        );
    }
    // 4. Codex, or the first provider left.
    if let Some(layer) = &inputs.fork {
        return layer.fallback(inputs);
    }
    let kind = [Kind::Codex]
        .into_iter()
        .chain(Kind::ALL)
        .find(|k| inputs.allowed(*k))
        .unwrap_or(Kind::Codex);
    NewChatChoice::of(kind, default_model(prefs, cwd, kind))
}

impl Shell {
    /// The choice for a new conversation in `cwd`. `seed` lets the conversation
    /// being worked in guide it where the fork layer supports that.
    pub(super) fn new_chat_choice(&self, cwd: &str, seed: bool, cx: &App) -> NewChatChoice {
        let fork = fork::Layer::gather(self, cwd, seed, cx);
        resolve(&Inputs {
            prefs: &self.workspace.preferences,
            cwd,
            fork,
        })
    }
    /// A new chat in the current project.
    pub(super) fn default_choice(&self, cx: &App) -> NewChatChoice {
        self.new_chat_choice(&self.workspace.project, true, cx)
    }
    /// A caller-picked provider in the current project.
    pub(super) fn explicit_choice(&self, kind: Kind) -> NewChatChoice {
        NewChatChoice::explicit(&self.workspace.preferences, &self.workspace.project, kind)
    }
}

// Fork-only layer (Paseo). Everything below is dropped for the upstream port,
// along with `Inputs::fork`, its two call sites in `resolve`, and `gather`.
pub(crate) mod fork {
    use super::*;

    /// The model a Paseo host's default Hermes provider is known by.
    pub(crate) const DEFAULT_MODEL: &str = "hermes:openai-codex:gpt-6.1-sol";

    pub(crate) struct Layer {
        /// Providers hidden in the session filters.
        pub session_hidden: Vec<String>,
        /// Providers the cached host catalog offers, when it is cached.
        pub offered: Option<Vec<Kind>>,
        /// The active conversation in this project: provider and its model.
        pub seed: Option<(Kind, Option<String>)>,
        /// Whether the cached catalog lists `DEFAULT_MODEL`; `None` when uncached.
        pub default_listed: Option<bool>,
    }

    impl Layer {
        pub(crate) fn gather(shell: &Shell, cwd: &str, seed: bool, cx: &App) -> Option<Self> {
            if !crate::paseo_host::project_ui(&shell.workspace.preferences) {
                return None;
            }
            let cached = monocode_core::catalog::cached_remote_catalog(cwd);
            let seed = seed
                .then(|| {
                    let id = shell.workspace.session_for_pane(shell.workspace.active?);
                    let view = shell.sessions.get(&id)?.read(cx);
                    (view.summary.cwd == cwd).then(|| {
                        (
                            Kind::parse(&view.summary.provider),
                            view.settings.model.clone(),
                        )
                    })
                })
                .flatten()
                .and_then(|(kind, model)| Some((kind?, model)));
            Some(Self {
                session_hidden: shell.workspace.session_filters.hidden_providers.clone(),
                default_listed: cached.as_ref().map(|(_, models)| {
                    models
                        .get(&Kind::Hermes)
                        .is_some_and(|ids| ids.iter().any(|id| id == DEFAULT_MODEL))
                }),
                offered: cached.map(|(providers, _)| providers),
                seed,
            })
        }

        pub(super) fn allows(&self, inputs: &Inputs, kind: Kind) -> bool {
            !self.session_hidden.iter().any(|id| id == kind.id())
                && !inputs
                    .prefs
                    .hidden_for(inputs.cwd)
                    .iter()
                    .any(|id| id == kind.id())
                && self.offered.as_ref().is_none_or(|p| p.contains(&kind))
        }

        /// Between the Settings default and the last choice.
        pub(super) fn seed(&self, inputs: &Inputs) -> Option<NewChatChoice> {
            let (kind, model) = self.seed.clone()?;
            self.allows(inputs, kind).then(|| {
                let project = inputs
                    .prefs
                    .project_providers
                    .get(inputs.cwd)
                    .and_then(|p| p.models.get(kind.id()))
                    .cloned();
                NewChatChoice::of(
                    kind,
                    project
                        .or(model)
                        .or_else(|| inputs.prefs.default_models.get(kind.id()).cloned()),
                )
            })
        }

        /// Hermes, with the host's default model when it is known to exist.
        pub(super) fn fallback(&self, inputs: &Inputs) -> NewChatChoice {
            let kind = [Kind::Hermes]
                .into_iter()
                .chain(Kind::ALL)
                .find(|k| inputs.allowed(*k))
                .unwrap_or(Kind::Hermes);
            let model = default_model(inputs.prefs, inputs.cwd, kind);
            let mut choice = NewChatChoice::of(kind, model);
            if choice.model.is_none() && kind == Kind::Hermes {
                match self.default_listed {
                    Some(true) => choice.model = Some(DEFAULT_MODEL.into()),
                    Some(false) => {}
                    None => choice.verify_catalog = true,
                }
            }
            choice
        }
    }

    /// Reads the host's catalog (which caches it) and returns the model to use.
    pub(crate) fn verified_model(cwd: &str) -> Option<String> {
        let catalog = monocode_core::catalog::remote_catalog(cwd).ok()?;
        catalog
            .models
            .get(&Kind::Hermes)?
            .iter()
            .any(|m| m.id == DEFAULT_MODEL)
            .then(|| DEFAULT_MODEL.into())
    }
}
