//! The new-chat resolver: each step of the order, hidden skipping, and the
//! fork layer's seed, host offer and default model.
use super::new_chat_choice::{fork::DEFAULT_MODEL, *};
use super::*;
use crate::settings::{ModelChoice, Preferences};

const LOCAL: &str = "/work/project";
const REMOTE: &str = "remote://env/work/project";

fn recent(provider: &str, model: Option<&str>) -> ModelChoice {
    ModelChoice {
        provider: provider.into(),
        model: model.map(Into::into),
    }
}

/// The project's own provider, models and hidden list.
fn project(
    prefs: &mut Preferences,
    cwd: &str,
    provider: Option<&str>,
    models: &[(&str, &str)],
    hidden: &[&str],
) {
    let p = prefs.project_providers.entry(cwd.into()).or_default();
    p.provider = provider.map(Into::into);
    p.models = models
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    p.hidden = hidden.iter().map(|s| s.to_string()).collect();
}

fn pick(prefs: &Preferences, cwd: &str, fork: Option<fork::Layer>) -> (Kind, Option<String>, bool) {
    let c = resolve(&Inputs { prefs, cwd, fork });
    (c.kind, c.model, c.verify_catalog)
}

fn layer() -> fork::Layer {
    fork::Layer {
        session_hidden: vec![],
        offered: None,
        seed: None,
        default_listed: None,
    }
}

#[test]
fn nothing_set_starts_codex() {
    assert_eq!(
        pick(&Preferences::default(), LOCAL, None),
        (Kind::Codex, None, false)
    );
}

#[test]
fn project_provider_beats_settings_and_recents() {
    let mut prefs = Preferences {
        default_provider: Some("claude".into()),
        default_models: [("grok".into(), "grok-default".into())].into(),
        recent_models: vec![recent("pi", Some("pi-1"))],
        ..Default::default()
    };
    project(
        &mut prefs,
        LOCAL,
        Some("grok"),
        &[("grok", "grok-project")],
        &[],
    );
    assert_eq!(
        pick(&prefs, LOCAL, None),
        (Kind::Grok, Some("grok-project".into()), false)
    );
    project(&mut prefs, LOCAL, Some("grok"), &[], &[]);
    assert_eq!(
        pick(&prefs, LOCAL, None),
        (Kind::Grok, Some("grok-default".into()), false)
    );
}

#[test]
fn settings_provider_beats_recents_and_takes_its_default_model() {
    let prefs = Preferences {
        default_provider: Some("claude".into()),
        default_models: [("claude".into(), "opus".into())].into(),
        recent_models: vec![recent("pi", Some("pi-1"))],
        ..Default::default()
    };
    assert_eq!(
        pick(&prefs, LOCAL, None),
        (Kind::Claude, Some("opus".into()), false)
    );
}

#[test]
fn last_choice_is_used_and_defaults_win_over_its_model() {
    let mut prefs = Preferences {
        recent_models: vec![recent("claude", Some("opus")), recent("pi", None)],
        ..Default::default()
    };
    assert_eq!(
        pick(&prefs, LOCAL, None),
        (Kind::Claude, Some("opus".into()), false)
    );
    prefs
        .default_models
        .insert("claude".into(), "sonnet".into());
    assert_eq!(
        pick(&prefs, LOCAL, None),
        (Kind::Claude, Some("sonnet".into()), false)
    );
}

#[test]
fn hidden_providers_are_skipped_step_by_step() {
    let mut prefs = Preferences {
        default_provider: Some("claude".into()),
        recent_models: vec![recent("pi", Some("pi-1"))],
        hidden_providers: vec!["claude".into(), "pi".into()],
        ..Default::default()
    };
    assert_eq!(pick(&prefs, LOCAL, None), (Kind::Codex, None, false));
    // The first provider left when Codex is hidden too.
    prefs.hidden_providers.push("codex".into());
    assert_eq!(pick(&prefs, LOCAL, None).0, Kind::Cursor);
    // Everything hidden falls back to Codex.
    prefs.hidden_providers = Kind::ALL.iter().map(|k| k.id().into()).collect();
    assert_eq!(pick(&prefs, LOCAL, None).0, Kind::Codex);
    // A project's own hidden list counts for that project.
    let mut prefs = Preferences {
        recent_models: vec![recent("pi", Some("pi-1"))],
        ..Default::default()
    };
    project(&mut prefs, LOCAL, None, &[], &["pi"]);
    assert_eq!(pick(&prefs, LOCAL, None).0, Kind::Codex);
    assert_eq!(pick(&prefs, "/work/other", None).0, Kind::Pi);
}

#[test]
fn remote_projects_ignore_the_hidden_list_without_the_fork_layer() {
    let prefs = Preferences {
        default_provider: Some("claude".into()),
        hidden_providers: vec!["claude".into()],
        ..Default::default()
    };
    assert_eq!(pick(&prefs, LOCAL, None).0, Kind::Codex);
    assert_eq!(pick(&prefs, REMOTE, None).0, Kind::Claude);
}

#[test]
fn explicit_provider_takes_the_project_then_default_model() {
    let mut prefs = Preferences {
        default_models: [("claude".into(), "sonnet".into())].into(),
        ..Default::default()
    };
    let c = NewChatChoice::explicit(&prefs, LOCAL, Kind::Claude);
    assert_eq!((c.kind, c.model.as_deref()), (Kind::Claude, Some("sonnet")));
    project(&mut prefs, LOCAL, None, &[("claude", "opus")], &[]);
    assert_eq!(
        NewChatChoice::explicit(&prefs, LOCAL, Kind::Claude)
            .model
            .as_deref(),
        Some("opus")
    );
}

#[test]
fn seed_sits_between_settings_and_last_choice() {
    let mut prefs = Preferences {
        recent_models: vec![recent("claude", Some("opus"))],
        default_models: [("hermes".into(), "hermes:default".into())].into(),
        ..Default::default()
    };
    let seeded = |model: Option<&str>| fork::Layer {
        seed: Some((Kind::Hermes, model.map(Into::into))),
        ..layer()
    };
    assert_eq!(
        pick(&prefs, REMOTE, Some(seeded(Some("hermes:live")))),
        (Kind::Hermes, Some("hermes:live".into()), false)
    );
    // Without the live model the Settings default fills in.
    assert_eq!(
        pick(&prefs, REMOTE, Some(seeded(None))),
        (Kind::Hermes, Some("hermes:default".into()), false)
    );
    // A project model for the provider wins over the live model.
    project(
        &mut prefs,
        REMOTE,
        None,
        &[("hermes", "hermes:project")],
        &[],
    );
    assert_eq!(
        pick(&prefs, REMOTE, Some(seeded(Some("hermes:live"))))
            .1
            .as_deref(),
        Some("hermes:project")
    );
    // The Settings provider still wins over the seed.
    prefs.default_provider = Some("grok".into());
    assert_eq!(pick(&prefs, REMOTE, Some(seeded(None))).0, Kind::Grok);
    // A hidden seed falls through to the last choice.
    prefs.default_provider = None;
    let hidden = fork::Layer {
        session_hidden: vec!["hermes".into()],
        ..seeded(None)
    };
    assert_eq!(pick(&prefs, REMOTE, Some(hidden)).0, Kind::Claude);
}

#[test]
fn fork_layer_hides_session_filtered_and_unoffered_providers() {
    let prefs = Preferences {
        recent_models: vec![recent("codex", Some("gpt")), recent("claude", None)],
        hidden_providers: vec!["claude".into()],
        ..Default::default()
    };
    // hidden_for applies on remote projects too under the fork layer.
    let l = || fork::Layer {
        default_listed: Some(false),
        ..layer()
    };
    assert_eq!(pick(&prefs, REMOTE, Some(l())).0, Kind::Codex);
    let filtered = fork::Layer {
        session_hidden: vec!["codex".into()],
        ..l()
    };
    assert_eq!(pick(&prefs, REMOTE, Some(filtered)).0, Kind::Hermes);
    let offered = fork::Layer {
        offered: Some(vec![Kind::Claude, Kind::Pi]),
        ..l()
    };
    // Codex is not offered, so the fallback picks the first offered provider.
    assert_eq!(pick(&prefs, REMOTE, Some(offered)).0, Kind::Pi);
}

#[test]
fn fallback_is_hermes_with_the_default_model_only_when_listed() {
    let prefs = Preferences::default();
    let listed = fork::Layer {
        default_listed: Some(true),
        ..layer()
    };
    assert_eq!(
        pick(&prefs, REMOTE, Some(listed)),
        (Kind::Hermes, Some(DEFAULT_MODEL.into()), false)
    );
    let missing = fork::Layer {
        default_listed: Some(false),
        ..layer()
    };
    assert_eq!(
        pick(&prefs, REMOTE, Some(missing)),
        (Kind::Hermes, None, false)
    );
    // No catalog yet: no model, and the creator is told to read the catalog.
    assert_eq!(
        pick(&prefs, REMOTE, Some(layer())),
        (Kind::Hermes, None, true)
    );
    // A configured default model needs no catalog.
    let configured = Preferences {
        default_models: [("hermes".into(), "hermes:mine".into())].into(),
        ..Default::default()
    };
    assert_eq!(
        pick(&configured, REMOTE, Some(layer())),
        (Kind::Hermes, Some("hermes:mine".into()), false)
    );
    // Hermes unavailable: another provider, never needing a catalog.
    let offered = fork::Layer {
        offered: Some(vec![Kind::Claude]),
        ..layer()
    };
    assert_eq!(
        pick(&prefs, REMOTE, Some(offered)),
        (Kind::Claude, None, false)
    );
}

// --- Through the real Shell ---

use gpui::TestAppContext;
use monocode_core::catalog::{Model, RemoteCatalog, remember_remote_catalog};

/// The Paseo fixture's project is one cache entry shared by these tests.
static CATALOG: Mutex<()> = Mutex::new(());

fn host_catalog(cwd: &str, providers: &[Kind], hermes: &[&str]) {
    let model = |id: &str| Model {
        id: id.into(),
        name: id.into(),
        ..Default::default()
    };
    remember_remote_catalog(
        cwd,
        &RemoteCatalog {
            providers: providers.to_vec(),
            models: [(Kind::Hermes, hermes.iter().map(|id| model(id)).collect())].into(),
            errors: Default::default(),
        },
    );
}

fn paseo_window(cx: &mut TestAppContext) -> gpui::WindowHandle<Shell> {
    cx.update(|cx| {
        let mut preferences = Preferences {
            paced_reveal: false,
            ..Default::default()
        };
        preferences.extra.insert(
            crate::paseo_host::PROJECT_SELECTION.into(),
            serde_json::json!({}),
        );
        crate::theme::configure(&preferences, false);
        cx.set_global(preferences);
        crate::input::bind(cx);
        crate::popover::bind(cx);
        crate::shell::bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    let window = cx.add_window(|_, cx| crate::shell::paseo_shell_fixture(cx));
    cx.run_until_parked();
    window
}

/// The newest session's provider and saved model after a new chat.
fn created(shell: &Shell, cx: &App) -> (String, Option<String>) {
    let id = shell.workspace.active.expect("a new chat is active");
    let provider = shell.session_metadata(id, cx).summary.provider.clone();
    let model = shell
        .store
        .lock()
        .unwrap()
        .agent_settings(id)
        .unwrap()
        .and_then(|settings| settings.model);
    (provider, model)
}

#[gpui::test]
fn paseo_new_chat_follows_the_active_conversation_model(cx: &mut TestAppContext) {
    let _serial = CATALOG.lock().unwrap_or_else(|e| e.into_inner());
    let window = paseo_window(cx);
    window
        .update(cx, |shell, _, cx| {
            let project = shell.workspace.project.clone();
            host_catalog(&project, &[Kind::Hermes, Kind::Claude], &[DEFAULT_MODEL]);
            let id = shell.workspace.active.unwrap();
            shell.sessions[&id].update(cx, |view, _| {
                assert_eq!(view.summary.cwd, project);
                view.settings.model = Some("hermes:seeded-model".into());
            });
            shell.new_chat(cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, _, cx| {
            assert_eq!(
                created(shell, cx),
                ("hermes".into(), Some("hermes:seeded-model".into()))
            );
        })
        .unwrap();
}

#[gpui::test]
fn paseo_new_chat_with_nothing_to_follow_starts_hermes_default_model(cx: &mut TestAppContext) {
    let _serial = CATALOG.lock().unwrap_or_else(|e| e.into_inner());
    let window = paseo_window(cx);
    window
        .update(cx, |shell, _, cx| {
            let project = shell.workspace.project.clone();
            host_catalog(&project, &[Kind::Hermes, Kind::Claude], &[DEFAULT_MODEL]);
            assert!(shell.workspace.preferences.recent_models.is_empty());
            shell.workspace.active = None;
            shell.new_chat(cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, _, cx| {
            assert_eq!(
                created(shell, cx),
                ("hermes".into(), Some(DEFAULT_MODEL.into()))
            );
        })
        .unwrap();
}

#[gpui::test]
fn paseo_project_default_beats_the_active_conversation_and_recents(cx: &mut TestAppContext) {
    let _serial = CATALOG.lock().unwrap_or_else(|e| e.into_inner());
    let window = paseo_window(cx);
    window
        .update(cx, |shell, _, cx| {
            let project = shell.workspace.project.clone();
            host_catalog(&project, &[Kind::Hermes, Kind::Claude], &[DEFAULT_MODEL]);
            project_default(
                &mut shell.workspace.preferences,
                &project,
                "claude",
                "claude:opus",
            );
            shell.workspace.preferences.recent_models = vec![recent("hermes", Some(DEFAULT_MODEL))];
            shell.new_chat(cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |shell, _, cx| {
            assert_eq!(
                created(shell, cx),
                ("claude".into(), Some("claude:opus".into()))
            );
        })
        .unwrap();
}

fn project_default(prefs: &mut Preferences, cwd: &str, provider: &str, model: &str) {
    project(prefs, cwd, Some(provider), &[(provider, model)], &[]);
}

#[gpui::test]
fn paseo_filters_hide_the_providers_a_new_chat_would_use(cx: &mut TestAppContext) {
    let _serial = CATALOG.lock().unwrap_or_else(|e| e.into_inner());
    let window = paseo_window(cx);
    window
        .update(cx, |shell, _, cx| {
            let project = shell.workspace.project.clone();
            host_catalog(&project, &[Kind::Hermes, Kind::Claude], &[DEFAULT_MODEL]);
            shell.workspace.preferences.recent_models = vec![recent("codex", Some("gpt"))];
            shell.workspace.session_filters.hidden_providers = vec!["codex".into()];
            let choice = shell.new_chat_choice(&project, false, cx);
            assert_eq!(choice.kind, Kind::Hermes);
            assert_eq!(choice.model.as_deref(), Some(DEFAULT_MODEL));
            assert!(!choice.verify_catalog);
        })
        .unwrap();
}

#[gpui::test]
fn ordinary_shell_new_chat_follows_the_last_choice(cx: &mut TestAppContext) {
    let (window, _, _) = super::tests::window(cx);
    let new_chat = |cx: &mut TestAppContext, recents: Vec<ModelChoice>, hidden: &[&str]| {
        window
            .update(cx, |shell, _, cx| {
                shell.workspace.preferences.recent_models = recents;
                shell.workspace.preferences.hidden_providers =
                    hidden.iter().map(|id| id.to_string()).collect();
                shell.new_chat(cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| created(shell, cx))
            .unwrap()
    };
    assert_eq!(new_chat(cx, vec![], &[]), ("codex".into(), None));
    assert_eq!(
        new_chat(cx, vec![recent("claude", Some("opus"))], &[]),
        ("claude".into(), Some("opus".into()))
    );
    // A hidden last choice is skipped.
    assert_eq!(
        new_chat(cx, vec![recent("claude", Some("opus"))], &["claude"]),
        ("codex".into(), None)
    );
}
