//! RemoteSession.tsx's host observation and composer restrictions.
use super::*;
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    pub(super) client: Option<Arc<monocode_core::remote::Connections>>,
    clock: Option<gpui::Task<()>>,
    pub(super) refreshing: bool,
    /// A `view` acknowledgement is in flight; `viewed_at` throttles repeats.
    viewing: bool,
    viewed_at: Option<std::time::Instant>,
    pub(super) first_content_trace: Option<std::time::Instant>,
    pub(super) pending: bool,
    pub(super) online: bool,
    /// A send rejected only because a turn is running; not a remote command.
    send_blocked_notice: bool,
    pub(super) unbound: bool,
    pub(super) workspace_choices: Vec<Value>,
    pub(super) worktree_info: Option<Value>,
    pub(super) worktree_info_at: Option<std::time::Instant>,
    pub(super) worktree_info_loading: bool,
    pub(super) worktree_form: std::cell::RefCell<Option<gpui::WeakEntity<PaseoWorktreeForm>>>,
    error: Option<String>,
    rejected: Option<Value>,
    snapshot: Option<Value>,
    pub(super) capabilities: std::collections::HashSet<String>,
}

pub(super) enum PaseoWorktreeEvent {
    Confirmed(monocode_core::agent::RemoteWorktreeSpec),
}

pub(super) struct PaseoWorktreeForm {
    pub(super) branch_input: Entity<input::Input>,
    pub(super) branches: Vec<String>,
    pub(super) base_branch: Option<String>,
    pub(super) error: Option<String>,
    _submitted: Subscription,
    _changed: Subscription,
}

impl EventEmitter<PaseoWorktreeEvent> for PaseoWorktreeForm {}

impl PaseoWorktreeForm {
    pub(super) fn new(
        worktree_info: Option<&Value>,
        current: Option<&monocode_core::agent::RemoteWorktreeSpec>,
        cx: &mut Context<Self>,
    ) -> Self {
        let branch_input = cx.new(|cx| {
            let mut field = input::Input::field(
                "New Paseo worktree branch",
                crate::i18n::t("留空由 Paseo 自动命名，如 feat/login"),
                content(0.4),
                cx,
            );
            if let Some(existing) = current.and_then(|s| s.branch_name.as_deref()) {
                field.set_text(existing.to_owned(), cx);
            }
            field
        });
        let submitted = cx.subscribe(&branch_input, |this, _, _: &input::Submitted, cx| {
            this.confirm(cx);
        });
        let changed = cx.subscribe(&branch_input, |this, _, _: &input::Changed, cx| {
            if this.error.take().is_some() {
                cx.notify();
            }
        });
        let (branches, default_base) = Self::extract_branches(worktree_info);
        let base_branch = current
            .and_then(|s| s.base_branch.clone())
            .filter(|b| !b.trim().is_empty())
            .or(default_base);
        Self {
            branch_input,
            branches,
            base_branch,
            error: None,
            _submitted: submitted,
            _changed: changed,
        }
    }

    fn extract_branches(worktree_info: Option<&Value>) -> (Vec<String>, Option<String>) {
        let current_branch = worktree_info
            .and_then(|info| info["currentBranch"].as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned);
        let mut branches: Vec<String> = worktree_info
            .and_then(|info| info["branches"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| {
                v.as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
            })
            .collect();
        if let Some(curr) = &current_branch
            && !branches.contains(curr)
        {
            branches.insert(0, curr.clone());
        }
        let default_base = current_branch.or_else(|| branches.first().cloned());
        (branches, default_base)
    }

    pub(super) fn set_worktree_info(&mut self, info: &Value, cx: &mut Context<Self>) {
        let (branches, default_base) = Self::extract_branches(Some(info));
        self.branches = branches;
        if self.base_branch.is_none() {
            self.base_branch = default_base;
        }
        cx.notify();
    }

    pub(super) fn focus(&self, cx: &gpui::App) -> gpui::FocusHandle {
        self.branch_input.focus_handle(cx)
    }

    pub(super) fn valid_branch_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 128
            && !name.starts_with('/')
            && !name.ends_with('/')
            && !name.ends_with(".lock")
            && !name.contains("..")
            && !name
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
    }

    pub(super) fn confirm(&mut self, cx: &mut Context<Self>) -> bool {
        let raw_branch = self.branch_input.read(cx).content.trim().to_owned();
        if !raw_branch.is_empty() && !Self::valid_branch_name(&raw_branch) {
            self.error = Some("分支名称格式无效，请检查后重试。".into());
            cx.notify();
            return false;
        }
        let branch_name = (!raw_branch.is_empty()).then_some(raw_branch);
        let base_branch = self
            .base_branch
            .clone()
            .filter(|b| !b.trim().is_empty() && Self::valid_branch_name(b));
        cx.emit(PaseoWorktreeEvent::Confirmed(
            monocode_core::agent::RemoteWorktreeSpec {
                action: "branch-off".into(),
                branch_name,
                base_branch: base_branch.clone(),
                ref_name: base_branch,
            },
        ));
        true
    }
}

impl Render for PaseoWorktreeForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_base = self.base_branch.clone();
        div()
            .id("paseo-worktree-form")
            .debug_selector(|| "paseo-worktree-form".into())
            .w_full()
            .flex()
            .flex_col()
            .gap(px(8.))
            .text_size(px(12.))
            .text_color(content(0.9))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(content(0.5))
                    .child(crate::i18n::t("新建 Paseo Git Worktree")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(content(0.65))
                            .child(crate::i18n::t("新分支名称（可选）")),
                    )
                    .child(
                        div()
                            .id("paseo-worktree-branch-input")
                            .debug_selector(|| "paseo-worktree-branch-input".into())
                            .px(px(8.))
                            .py(px(5.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(content(0.15))
                            .bg(content(0.04))
                            .child(self.branch_input.clone()),
                    ),
            )
            .when(!self.branches.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(content(0.65))
                                .child(crate::i18n::t("基线分支")),
                        )
                        .child(
                            div()
                                .id("paseo-worktree-base-list")
                                .max_h(px(140.))
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .children(self.branches.iter().enumerate().map(|(ix, branch)| {
                                    let branch_name = branch.clone();
                                    let branch_for_click = branch.clone();
                                    let branch_for_selector = branch.clone();
                                    let is_selected =
                                        selected_base.as_deref() == Some(branch.as_str());
                                    div()
                                        .id(("paseo-base-branch", ix))
                                        .debug_selector(move || {
                                            format!("paseo-base-branch-{branch_for_selector}")
                                        })
                                        .px(px(8.))
                                        .py(px(4.))
                                        .rounded(px(6.))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .when(is_selected, |row| row.bg(content(0.08)))
                                        .hover(|row| row.bg(content(0.06)))
                                        .child(icon("git-branch", 12.).text_color(content(0.6)))
                                        .child(div().flex_1().truncate().child(branch_name))
                                        .when(is_selected, |row| {
                                            row.child(icon("check", 12.).text_color(content(0.9)))
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.base_branch = Some(branch_for_click.clone());
                                            cx.notify();
                                        }))
                                })),
                        ),
                )
            })
            .when_some(self.error.clone(), |d, err| {
                d.child(
                    div()
                        .debug_selector(|| "paseo-worktree-error".into())
                        .text_size(px(11.))
                        .text_color(tint(AMBER_400, 0.95))
                        .child(err),
                )
            })
            .child(
                div().flex().justify_end().pt(px(2.)).child(
                    crate::settings::widgets::secondary_button(
                        "paseo-worktree-confirm",
                        crate::i18n::t("确定"),
                        true,
                        false,
                    )
                    .debug_selector(|| "paseo-worktree-confirm".into())
                    .child(crate::i18n::t("确定"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm(cx);
                    })),
                ),
            )
    }
}
impl State {
    fn legacy_recovery_error(&self) -> Option<&str> {
        self.rejected.as_ref().filter(|rejected| rejected["legacyRecovery"] == true)
            .map(|rejected| rejected["error"].as_str().unwrap_or("Older pending requests could not be linked to one conversation. Open the original desktop to recover them."))
    }
}

/// Shared by the initial Conversation worker and the read-only switch probe.
/// No view/clearAttention, model or filesystem command is dispatched here.
pub(crate) struct OpenHistory {
    remote: monocode_core::store::RemoteSession,
    settings: monocode_core::agent::Settings,
    snapshot: Option<Value>,
    history: Option<(
        crate::transcript::PreparedHistory,
        HashMap<usize, monocode_core::agent::TurnInfo>,
    )>,
    /// A `paseo.history-pages` host's window: its blocks' rows and older cursor.
    window: Option<PagedWindow>,
    descriptor: Option<Value>,
    workspace_choices: Option<Vec<Value>>,
    pub(crate) timings: Value,
    machine_id: String,
}
struct PagedWindow {
    layout: crate::transcript::WindowLayout,
    before: Option<i64>,
    epoch: Option<String>,
}
/// Reads the pages above a paged Paseo window. Pages belong to one `epoch`;
/// the host rejects a stale one, which surfaces as the pager's error.
fn history_pager(
    client: Arc<monocode_core::remote::Connections>,
    machine: String,
    environment: String,
    session: String,
    epoch: String,
) -> crate::transcript::HistoryPager {
    Arc::new(move |before, all| {
        if all {
            return Err("Older Paseo history pages load as you scroll".into());
        }
        let page = client.request(
            &machine,
            "sessions.historyBefore",
            json!({"sessionId":session,"epoch":epoch,"before":before,"limit":100}),
        )?;
        if page["epoch"].as_str().is_some_and(|reply| reply != epoch) {
            return Err("Paseo history changed; reopen this conversation".into());
        }
        let mut wrapper = json!({"session":{"blocks":page["blocks"]}});
        monocode_core::remote::attachments::qualify_snapshot(&mut wrapper, &environment, &session);
        let (history, turns) = monocode_core::store::Store::project_remote_history(&wrapper)?;
        Ok(monocode_core::store::HistoryPage {
            rows: history.rows,
            turns,
            before: page["before"].as_i64(),
        })
    })
}
struct OpenTimings {
    started: std::time::Instant,
    phase: std::time::Instant,
    values: serde_json::Map<String, Value>,
}
impl OpenTimings {
    fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            started: now,
            phase: now,
            values: Default::default(),
        }
    }
    fn mark(&mut self, key: &str) {
        self.values.insert(
            key.into(),
            json!(self.phase.elapsed().as_secs_f64() * 1000.),
        );
        self.phase = std::time::Instant::now();
    }
    fn finish(mut self) -> Value {
        self.values.insert(
            "backgroundMs".into(),
            json!(self.started.elapsed().as_secs_f64() * 1000.),
        );
        Value::Object(self.values)
    }
}
/// Best-effort `view` acknowledgement: the host clears the conversation's attention.
fn dispatch_remote_view(
    client: &monocode_core::remote::Connections,
    machine: &str,
    session: &str,
    id: i64,
) {
    let begin = std::time::Instant::now();
    let result = client.request(
        machine,
        "commands.dispatch",
        json!({"type":"view", "sessionId":session, "commandId":monocode_core::mono::new_id()}),
    );
    if std::env::var_os("MONOCODE_NATIVE_TRACE").is_some() {
        eprintln!(
            "native remote-view {}",
            json!({"session":id,"elapsedMs":begin.elapsed().as_secs_f64()*1000.,"ok":result.is_ok()})
        );
    }
}
pub(crate) fn prepare_remote_open(
    store: &Database,
    id: i64,
    known: Option<Value>,
    client: Option<Arc<monocode_core::remote::Connections>>,
    initial: bool,
) -> Result<OpenHistory, String> {
    prepare_remote_open_with(store, id, known, client, initial, false)
}
/// `paged`: the host is already known to advertise `paseo.history-pages`; an
/// initial open reads that from its own descriptor instead.
pub(crate) fn prepare_remote_open_with(
    store: &Database,
    id: i64,
    known: Option<Value>,
    client: Option<Arc<monocode_core::remote::Connections>>,
    initial: bool,
    paged: bool,
) -> Result<OpenHistory, String> {
    let mut timings = OpenTimings::new();
    let (mut remote, mut settings) = {
        let store = locked(store)?;
        timings.mark("storeLockWaitMs");
        (
            store
                .remote_session(id)?
                .ok_or("Remote session no longer exists")?,
            store.agent_settings(id)?.unwrap_or_default(),
        )
    };
    timings.mark("storeReadMs");
    let client = client.ok_or("Remote connections are unavailable")?;
    let machine = remote.machine(&client)?;
    timings.mark("machineMs");
    let descriptor = if initial {
        Some(client.request(&machine.id, "environment.describe", json!({}))?)
    } else {
        None
    };
    timings.mark("describeMs");
    let paged = match &descriptor {
        Some(descriptor) => descriptor["capabilities"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|c| c == "paseo.history-pages")),
        None => paged,
    };
    let workspace_choices = if remote.session_id.is_none()
        && descriptor.as_ref().is_some_and(|d| {
            d["capabilities"]
                .as_array()
                .is_some_and(|caps| caps.iter().any(|c| c == "paseo.workspaces"))
        }) {
        Some(
            client
                .request(
                    &machine.id,
                    "paseo.workspaces",
                    json!({"projectId":remote.project.project_id}),
                )?
                .as_array()
                .ok_or("Invalid Paseo workspace choices")?
                .clone(),
        )
    } else {
        None
    };
    timings.mark("workspaceChoicesMs");
    let snapshot = remote
        .session_id
        .as_deref()
        .map(|session| client.load_session(&machine.id, session, known.as_ref()))
        .transpose()?;
    timings.mark("syncMs");
    let changed = initial
        || known.as_ref().map(|s| &s["revision"]) != snapshot.as_ref().map(|s| &s["revision"]);
    if let Some(snapshot) = snapshot.as_ref().filter(|_| changed) {
        let store = locked(store)?;
        timings.mark("registerLockWaitMs");
        let summary = store.register_remote_summary(&remote.project, snapshot)?;
        remote = store
            .remote_session(summary.id)?
            .ok_or("Remote session identity is missing")?;
        settings = store.agent_settings(summary.id)?.unwrap_or_default();
    }
    timings.mark("registerMs");
    let mut window = None;
    let history = if changed {
        let qualified = snapshot.as_ref().map(|snapshot| {
            let mut snapshot = snapshot.clone();
            if let Some(id) = remote.session_id.as_deref() {
                monocode_core::remote::attachments::qualify_snapshot(
                    &mut snapshot,
                    &remote.project.environment_id,
                    id,
                );
            }
            snapshot
        });
        let (history, turns) = qualified
            .as_ref()
            .map(monocode_core::store::Store::project_remote_history)
            .transpose()?
            .unwrap_or_default();
        timings.mark("projectionMs");
        if paged && let Some(snapshot) = qualified.as_ref() {
            let host = &snapshot["session"]["historyWindow"];
            window = Some(PagedWindow {
                layout: monocode_core::store::Store::remote_window_layout(snapshot)?,
                before: host["before"].as_i64(),
                epoch: host["epoch"].as_str().map(str::to_owned),
            });
        }
        let prepared = crate::transcript::PreparedHistory::new(history.rows, false);
        timings.mark("prepareMs");
        Some((prepared, turns))
    } else {
        None
    };
    let rows = history.as_ref().map_or(0, |(h, _)| h.row_count());
    let mut timings = timings.finish();
    timings["rows"] = json!(rows);
    Ok(OpenHistory {
        remote,
        settings,
        snapshot,
        history,
        window,
        descriptor,
        workspace_choices,
        timings,
        machine_id: machine.id,
    })
}

impl Conversation {
    pub(super) fn paseo_controls(&self) -> bool {
        self.is_remote()
            && (crate::paseo_host::active()
                || self.remote_state.capabilities.contains("paseo.configure"))
    }
    /// A Paseo conversation still waiting for its first snapshot (the daemon may
    /// be resuming the agent). Same prototype gate as the failed-load retry.
    pub(super) fn paseo_cold_open(&self, cx: &gpui::App) -> bool {
        self.is_remote()
            && self.loading
            && (!self.loaded || self.history_unloaded)
            && self.remote_state.error.is_none()
            && cx
                .try_global::<crate::settings::Preferences>()
                .is_some_and(crate::paseo_host::project_ui)
    }
    pub(super) fn paseo_follow_ups(&self) -> bool {
        self.paseo_controls() && self.remote_state.capabilities.contains("paseo.steer")
    }
    pub(super) fn paseo_workspace_draft(&self) -> bool {
        self.paseo_controls()
            && self.remote_state.unbound
            && !self.summary.has_messages
            && self.remote_state.capabilities.contains("paseo.workspaces")
    }
    pub(super) fn is_remote(&self) -> bool {
        self.summary.cwd.starts_with("remote://")
    }
    pub(crate) fn remote_connection_error(&mut self, error: &str, cx: &mut Context<Self>) {
        if self.is_remote() && !self.busy {
            self.remote_state.online = false;
            self.remote_state.error = Some(error.into());
            self.status = error.into();
            cx.notify();
        }
    }
    pub(super) fn load_remote(&mut self, cx: &mut Context<Self>) {
        if self.remote_state.clock.is_none() && !cfg!(test) {
            self.remote_state.clock = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(3))
                        .await;
                    if this
                        .update(cx, |this, cx| {
                            // Live turns have their own 750 ms sync and control loop.
                            if !this.busy && !this.stopping && !this.history_unloaded {
                                this.refresh_remote_snapshot(cx);
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }
        // Failed Paseo loads retry via the clock/button, not SessionChanged/paint.
        let failed = self.remote_state.error.is_some()
            && cx
                .try_global::<crate::settings::Preferences>()
                .is_some_and(crate::paseo_host::project_ui);
        if (!self.loaded || self.history_unloaded) && !failed {
            self.refresh_remote_snapshot(cx);
        }
    }
    /// A loaded Paseo view the user is looking at acknowledges its host attention.
    /// The initial open already does this; here at most one request is in flight
    /// and a stale index cannot repeat it within `RETRY`. Returns whether it was sent.
    pub(crate) fn mark_remote_viewed(&mut self, cx: &mut Context<Self>) -> bool {
        const RETRY: std::time::Duration = std::time::Duration::from_secs(5);
        if !self.paseo_controls()
            || !self.history_loaded()
            || self.remote_state.viewing
            || self
                .remote_state
                .viewed_at
                .is_some_and(|at| at.elapsed() < RETRY)
        {
            return false;
        }
        let Some(client) = self.remote_state.client.clone() else {
            return false;
        };
        self.remote_state.viewing = true;
        self.remote_state.viewed_at = Some(std::time::Instant::now());
        let store = self.store.clone();
        let id = self.summary.id;
        let work = cx.background_executor().spawn(async move {
            let remote = locked(&store)?
                .remote_session(id)?
                .ok_or("Remote session no longer exists")?;
            let session = remote.session_id.clone().ok_or("Unbound remote draft")?;
            let machine = remote.machine(&client)?;
            dispatch_remote_view(&client, &machine.id, &session, id);
            Ok::<(), String>(())
        });
        cx.spawn(async move |this, cx| {
            let _ = work.await;
            let _ = this.update(cx, |this, _| this.remote_state.viewing = false);
        })
        .detach();
        true
    }
    /// Shell tests: a loaded Paseo view whose acknowledgement cannot reach any host.
    #[cfg(test)]
    pub(crate) fn mark_paseo_loaded_for_test(&mut self) {
        self.summary.cwd = "remote://shell-test/project".into();
        self.loaded = true;
        self.remote_state
            .capabilities
            .insert("paseo.configure".into());
        self.remote_state.client = Some(Arc::new(monocode_core::remote::Connections::new(
            "/nonexistent/monocode-shell-test",
        )));
    }
    /// Re-syncs an open idle view now so a host-started turn attaches without waiting for the 3 s clock.
    pub(crate) fn nudge_remote_observation(&mut self, cx: &mut Context<Self>) {
        if self.is_remote()
            && self.history_loaded()
            && !self.busy
            && !self.stopping
            && !self.remote_state.refreshing
        {
            self.refresh_remote_snapshot(cx);
        }
    }
    pub(super) fn refresh_remote_snapshot(&mut self, cx: &mut Context<Self>) {
        if self.remote_state.refreshing || self.busy || self.stopping {
            return;
        }
        self.remote_state.refreshing = true;
        let initial = !self.loaded || self.history_unloaded;
        self.loading = initial;
        let store = self.store.clone();
        let id = self.summary.id;
        let known = self.remote_state.snapshot.clone();
        if self.remote_state.client.is_none() {
            self.remote_state.client = monocode_core::remote::default_connections().ok();
        }
        let client = self.remote_state.client.clone();
        let view_client = client.clone();
        let started = std::time::Instant::now();
        let paged = self
            .remote_state
            .capabilities
            .contains("paseo.history-pages");
        let work = cx.background_executor().spawn(async move {
            prepare_remote_open_with(&store, id, known, client, initial, paged)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let notification = result.as_ref().ok().and_then(|loaded| {
                let paseo = crate::paseo_host::active()
                    || loaded.descriptor.as_ref().is_some_and(|d| {
                        d["capabilities"].as_array().is_some_and(|caps| {
                            caps.iter().any(|c| c == "paseo.configure")
                        })
                    });
                if !initial || !paseo {
                    return None;
                }
                Some((
                    view_client.clone()?,
                    loaded.machine_id.clone(),
                    loaded.remote.session_id.clone()?,
                ))
            });
            let notified = notification.is_some();
            let installed = this.update(cx, |this, cx| {
                this.remote_state.refreshing = false;
                this.loading = false;
                if notified {
                    this.remote_state.viewed_at = Some(std::time::Instant::now());
                }
                match result {
                    Ok(OpenHistory { remote, settings, snapshot, history, window, descriptor, workspace_choices, timings, machine_id }) => {
                        let install_started = std::time::Instant::now();
                        if initial && std::env::var_os("MONOCODE_NATIVE_TRACE").is_some() {
                            this.remote_state.first_content_trace = Some(started);
                        }
                        this.remote_state.unbound = remote.session_id.is_none();
                        if let Some(choices) = workspace_choices {
                            this.remote_state.workspace_choices = choices;
                        }
                        this.remote_state.online = true;
                        this.remote_state.error = remote
                            .rejected
                            .as_ref()
                            .and_then(|rejected| rejected["error"].as_str().map(str::to_owned));
                        this.remote_state.rejected = remote.rejected;
                        this.remote_state.pending = !remote.outbox.is_empty()
                            || this.remote_state.legacy_recovery_error().is_some();
                        this.summary = remote.summary;
                        if let Some(descriptor) = descriptor {
                            this.remote_state.capabilities = descriptor["capabilities"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|v| v.as_str().map(str::to_owned))
                                .collect();
                        }
                        if settings.revision > this.settings.revision
                            || (initial && settings.revision >= this.settings.revision)
                        {
                            this.settings = settings;
                            this.model = this.settings.model.clone();
                        }
                        if let Some((history, turns)) = history {
                            let preserve = this
                                .remote_state
                                .capabilities
                                .contains("paseo.preserve-prose");
                            this.settings.turns = turns.clone();
                            // Rebuilt from every installed window, so the pager always
                            // reads under the epoch of the window it sits above.
                            let pager = window.as_ref().map(|window| {
                                window.epoch.clone().zip(this.remote_state.client.clone()).zip(remote.session_id.clone())
                                    .map(|((epoch, client), session)| history_pager(
                                        client,
                                        machine_id.clone(),
                                        remote.project.environment_id.clone(),
                                        session,
                                        epoch,
                                    ))
                            });
                            this.transcript.update(cx, |view, cx| {
                                if preserve {
                                    view.paseo_mode();
                                }
                                match window {
                                    Some(window) => {
                                        view.install_remote_window(history, window.layout, window.before, cx);
                                        // The window's base is only known once it is installed.
                                        view.set_turns(turns);
                                    }
                                    None => {
                                        view.set_turns(turns);
                                        view.install_prepared_history(history, cx);
                                    }
                                }
                                if let Some(pager) = pager {
                                    view.set_remote_pager(pager);
                                }
                            });
                        }
                        if std::env::var_os("MONOCODE_NATIVE_TRACE").is_some() {
                            eprintln!("native remote-open {}", json!({"session":id,"initial":initial,"phases":timings,
                                "mainInstallMs":install_started.elapsed().as_secs_f64()*1000.,"contentInstalledMs":started.elapsed().as_secs_f64()*1000.}));
                        }
                        this.loaded = true;
                        this.history_unloaded = false;
                        this.status = "Ready".into();
                        let running = snapshot.as_ref().is_some_and(|s| s["status"] == "running");
                        if let Some(snapshot) = &snapshot {
                            this.context = snapshot["session"]
                                .get("context")
                                .cloned()
                                .and_then(|context| serde_json::from_value(context).ok());
                            if let Some(title) = snapshot["session"]["title"].as_str() {
                                this.summary.title = title.into();
                            }
                            this.summary.provider_id = snapshot["session"]["providerSessionId"]
                                .as_str()
                                .map(str::to_owned);
                            this.summary.has_messages = snapshot["session"]["blocks"]
                                .as_array()
                                .is_some_and(|b| !b.is_empty());
                            this.install_remote_draft(snapshot, cx);
                            this.install_remote_plans(snapshot, cx);
                        }
                        this.remote_state.snapshot = snapshot;
                        if initial {
                            this.refresh_catalog(cx);
                            this.refresh_git(cx);
                        }
                        if initial && std::env::var_os("MONOCODE_NATIVE_TRACE").is_some() {
                            eprintln!("native remote-open-main {}", json!({"session":id,
                                "mainCompleteMs":install_started.elapsed().as_secs_f64()*1000.,
                                "loadToNotifyMs":started.elapsed().as_secs_f64()*1000.}));
                        }
                        if let Some((row, query)) = this.pending_search.take() {
                            this.reveal_search(row, query, cx);
                        }
                        if running && !this.busy && !this.stopping {
                            this.settings.remote_action = Some(json!({"type":"attach"}));
                            this.launch_prompt(String::new(), cx);
                        } else {
                            this.run_history_actions(cx);
                        }
                    }
                    Err(error) => this.remote_connection_error(&error, cx),
                }
                cx.emit(SessionChanged);
                cx.notify();
            });
            if installed.is_ok() && let Some((client, machine, session)) = notification {
                cx.background_executor().spawn(async move {
                    dispatch_remote_view(&client, &machine, &session, id);
                }).detach();
            }
        })
        .detach();
    }
    fn install_remote_draft(&mut self, snapshot: &Value, cx: &mut Context<Self>) {
        let draft = snapshot["session"]["blocks"]
            .as_array()
            .and_then(|blocks| blocks.iter().find(|b| b["draft"] == true))
            .map(|block| monocode_core::follow_up::Message {
                id: block["id"].as_str().unwrap_or("").into(),
                text: block["text"].as_str().unwrap_or("").into(),
                at: self.transcript.read(cx).source_rows().len(),
                ..Default::default()
            });
        self.follow_ups.state.draft = draft.clone();
        self.transcript
            .update(cx, |view, cx| view.set_saved_draft(draft, cx));
    }
    pub(super) fn remote_send_guard(&mut self, cx: &mut Context<Self>) -> bool {
        self.remote_state.send_blocked_notice = false;
        let error = if !self.loaded {
            Some("Wait for this remote conversation to load.")
        } else if !self.remote_state.online {
            Some("The machine is offline. Reconnect in Settings → Connections, then Retry.")
        } else if self.remote_state.pending {
            Some(self.remote_state.legacy_recovery_error().unwrap_or(
                "A remote command needs confirmation. Choose Retry before sending another message.",
            ))
        } else if (self.busy || self.stopping) && !self.paseo_follow_ups() {
            Some("Wait for this remote turn to finish before sending another message.")
        } else {
            None
        };
        if let Some(error) = error {
            self.status = error.into();
            self.remote_state.send_blocked_notice = self.loaded
                && self.remote_state.online
                && !self.remote_state.pending
                && (self.busy || self.stopping);
            cx.notify();
            return true;
        }
        if !self.paseo_controls()
            && self.settings.new_worktree
            && self.settings.worktree_cwd.is_none()
        {
            if !self.preparing_worktree {
                self.prepare_worktree(cx);
            }
            return true;
        }
        if !self.attachments.is_empty()
            && !self
                .remote_state
                .capabilities
                .contains("attachments.upload")
        {
            self.status =
                "Update MonoCode Host in Connections settings to send attachments.".into();
            cx.notify();
            return true;
        }
        if self.paseo_controls()
            && self.attachments.iter().any(|path| {
                !monocode_core::remote::attachments::is_attachment_source(&path.to_string_lossy())
                    && monocode_core::composer::image_mime(path).is_none()
            })
        {
            self.status = "Paseo 目前仅支持图片附件（PNG、JPEG、WebP、GIF）。".into();
            cx.notify();
            return true;
        }
        if self.settings.plan && !self.remote_state.capabilities.contains("sessions.plan") {
            self.status = "Update MonoCode Host in Connections settings to use Plan mode.".into();
            cx.notify();
            return true;
        }
        if self.draft_active(cx) && !self.remote_state.capabilities.contains("sessions.draft") {
            self.status = "Update MonoCode Host in Connections settings to save drafts.".into();
            cx.notify();
            return true;
        }
        let text = self.input.read(cx).content.trim();
        if text == "/compact" {
            self.input
                .update(cx, |input, cx| input.set_text(String::new(), cx));
            self.launch_prompt("/compact".into(), cx);
            return true;
        }
        if text.starts_with('/')
            && !["/plan", "/draft"]
                .iter()
                .any(|command| text == *command || text.starts_with(&format!("{command} ")))
        {
            self.status =
                "Only /plan, /compact and saved drafts are supported by this remote host.".into();
            cx.notify();
            return true;
        }
        if self.settings.operator || self.settings.orchestrator {
            self.status = "Operator and orchestration are not available on remote projects.".into();
            cx.notify();
            return true;
        }
        false
    }
    pub(super) fn remote_draft_action(&mut self, send: bool, cx: &mut Context<Self>) {
        if self.busy || self.stopping || self.remote_state.pending || !self.remote_state.online {
            return;
        }
        let Some(draft) = self.follow_ups.state.draft.as_ref() else {
            return;
        };
        let prompt = if send {
            draft.text.clone()
        } else {
            String::new()
        };
        self.settings.remote_action = Some(if send {
            json!({"type":"send","draftBlockId":draft.id,"intent":"default"})
        } else {
            json!({"type":"removeDraft","draftBlockId":draft.id})
        });
        self.launch_prompt(prompt, cx);
    }
    pub(super) fn remote_save_draft(&mut self, cx: &mut Context<Self>) {
        if self.remote_send_guard(cx) {
            return;
        }
        let raw = self.input.read(cx).content.to_string();
        let prompt = monocode_core::composer::consume_command(&raw, "draft")
            .unwrap_or(&raw)
            .to_owned();
        if prompt.trim().is_empty() && self.attachments.is_empty() {
            return;
        }
        self.input
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        self.follow_ups.draft_selected = false;
        self.settings.remote_action = Some(json!({"type":"draft"}));
        self.launch_prompt(prompt, cx);
    }
    pub(super) fn remote_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.is_remote()
            && self.remote_state.send_blocked_notice
            && (self.busy || self.stopping)
            && (!self.input.read(cx).content.trim().is_empty() || !self.attachments.is_empty())
        {
            return Some(
                div()
                    .mx_2()
                    .my_2()
                    .p_3()
                    .rounded_lg()
                    .bg(content(0.04))
                    .text_size(px(12.))
                    .debug_selector(|| "remote-send-notice".into())
                    .child("尚未发送，内容仍保留在输入框。当前任务结束后请再次发送。")
                    .into_any_element(),
            );
        }
        if !self.is_remote()
            || self.busy
            || (self.remote_state.error.is_none() && !self.remote_state.pending)
        {
            return None;
        }
        let legacy = self.remote_state.legacy_recovery_error();
        Some(div().mx_2().my_2().p_3().rounded_lg().bg(content(0.04)).flex().items_center().gap_2()
            .child(div().flex_1().text_size(px(12.)).child(if let Some(error) = legacy {error.to_owned()} else if self.remote_state.pending {"A remote command needs confirmation. Retry uses its original command ID.".to_owned()} else {self.remote_state.error.clone().unwrap_or_default()}))
            .when(legacy.is_none(), |d| d
            .child(crate::settings::widgets::secondary_button("remote-retry",if self.remote_state.rejected.is_some() {"Restore message"} else {"Retry"},true,false).debug_selector(|| "remote-retry".into()).child(if self.remote_state.rejected.is_some() {"Restore message"} else {"Retry"}).on_click(cx.listener(|this,_,_,cx| {
                if this.remote_state.rejected.is_some() { this.restore_remote_message(cx); } else {this.retry_remote(cx);}
            })))).into_any_element())
    }
    fn retry_remote(&mut self, cx: &mut Context<Self>) {
        if let Some(error) = self.remote_state.legacy_recovery_error() {
            self.status = error.to_owned();
            cx.notify();
            return;
        }
        if self.busy || self.stopping {
            return;
        }
        if self.remote_state.pending {
            self.settings.remote_action = Some(json!({"type": "retry"}));
            self.launch_prompt(String::new(), cx);
        } else {
            self.refresh_remote_snapshot(cx);
        }
    }
    pub(super) fn release_remote_snapshot(&mut self) {
        self.remote_state.snapshot = None;
    }
    fn restore_remote_message(&mut self, cx: &mut Context<Self>) {
        if let Some(error) = self.remote_state.legacy_recovery_error() {
            self.status = error.to_owned();
            cx.notify();
            return;
        }
        if !self.input.read(cx).content.trim().is_empty() || !self.attachments.is_empty() {
            self.status = "Clear the composer before restoring the rejected message.".into();
            cx.notify();
            return;
        }
        if let Some(rejected) = self.remote_state.rejected.as_ref() {
            let message = rejected["commands"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .find(|command| {
                    matches!(command["type"].as_str(), Some("send" | "draft" | "compact"))
                });
            if let Some(message) = message {
                let text = if message["type"] == "compact" {
                    "/compact"
                } else {
                    message["text"].as_str().unwrap_or("")
                };
                self.input
                    .update(cx, |input, cx| input.set_text(text.to_owned(), cx));
                self.attachments = rejected["uploads"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|upload| upload["path"].as_str().map(std::path::PathBuf::from))
                    .collect();
            }
        }
        cx.notify();
    }
    pub(super) fn refresh_paseo_worktrees(&mut self, cx: &mut Context<Self>) {
        const TTL: std::time::Duration = std::time::Duration::from_secs(10);
        if !self.paseo_workspace_draft()
            || !self.remote_state.capabilities.contains("paseo.worktrees")
            || self.remote_state.worktree_info_loading
            || self
                .remote_state
                .worktree_info_at
                .is_some_and(|at| at.elapsed() < TTL)
        {
            return;
        }
        if self.remote_state.client.is_none() {
            self.remote_state.client = monocode_core::remote::default_connections().ok();
        }
        let Some(client) = self.remote_state.client.clone() else {
            return;
        };
        self.remote_state.worktree_info_loading = true;
        let store = self.store.clone();
        let id = self.summary.id;
        let work = cx.background_executor().spawn(async move {
            let remote = locked(&store)?
                .remote_session(id)?
                .ok_or("Remote session no longer exists")?;
            let machine = remote.machine(&client)?;
            client.request(
                &machine.id,
                "paseo.worktrees",
                json!({"projectId": remote.project.project_id}),
            )
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.remote_state.worktree_info_loading = false;
                if let Ok(info) = result {
                    if let Some(form) = this
                        .remote_state
                        .worktree_form
                        .borrow()
                        .as_ref()
                        .and_then(|w| w.upgrade())
                    {
                        form.update(cx, |form, cx| form.set_worktree_info(&info, cx));
                    }
                    this.remote_state.worktree_info = Some(info);
                    this.remote_state.worktree_info_at = Some(std::time::Instant::now());
                    cx.notify();
                }
            });
        })
        .detach();
    }
    pub(super) fn apply_paseo_worktree_spec(
        &mut self,
        spec: monocode_core::agent::RemoteWorktreeSpec,
        cx: &mut Context<Self>,
    ) {
        if !self.paseo_workspace_draft() {
            self.status = "已有 Paseo 对话不能在这里变更工作区归属".into();
            cx.notify();
            return;
        }
        self.settings.remote_worktree = Some(spec);
        self.settings.remote_workspace_id = None;
        self.settings.new_worktree = false;
        self.settings.worktree_cwd = None;
        self.settings.worktree_removed = false;
        self.save_settings(cx);
        cx.notify();
    }
    pub(super) fn paseo_workspace_pill_label(&self) -> SharedString {
        if let Some(spec) = &self.settings.remote_worktree {
            let branch = spec
                .branch_name
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("自动命名");
            format!("新 Worktree：{branch}").into()
        } else if let Some(id) = self.settings.remote_workspace_id.as_deref() {
            let title = self
                .remote_state
                .workspace_choices
                .iter()
                .find(|w| w["id"] == id)
                .and_then(|w| w["title"].as_str())
                .unwrap_or(id);
            format!("加入：{title}").into()
        } else if let Some(wt_cwd) = self.settings.worktree_cwd.as_deref() {
            let host_path = monocode_core::remote::parse_remote_path(wt_cwd)
                .map(|r| r.host_path)
                .unwrap_or_else(|| wt_cwd.to_owned());
            let trimmed = host_path.trim_end_matches('/');
            let branch = self
                .remote_state
                .worktree_info
                .as_ref()
                .and_then(|info| info["worktrees"].as_array())
                .and_then(|list| {
                    list.iter().find(|w| {
                        w["path"]
                            .as_str()
                            .is_some_and(|p| p.trim_end_matches('/') == trimmed)
                    })
                })
                .and_then(|w| w["branchName"].as_str())
                .filter(|s| !s.trim().is_empty());
            let label = branch.unwrap_or_else(|| {
                Path::new(trimmed)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(trimmed)
            });
            format!("Worktree：{label}").into()
        } else {
            "新建独立工作区".into()
        }
    }
    pub(super) fn paseo_workspace_menu_items(
        &self,
        events: &mut Vec<Subscription>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<MenuItem> {
        let is_new_default = self.settings.remote_workspace_id.is_none()
            && self.settings.remote_worktree.is_none()
            && self.settings.worktree_cwd.is_none();
        let mut items = vec![
            MenuItem::Header("Paseo 工作区归属".into()),
            MenuAction::new("workspace:paseo:new", "新建独立工作区")
                .description("创建新的独立对话，不作为当前对话的子任务")
                .icon("folder")
                .checked(is_new_default)
                .into(),
        ];
        if self.remote_state.capabilities.contains("paseo.worktrees") {
            let is_git_repo = self
                .remote_state
                .worktree_info
                .as_ref()
                .and_then(|info| info["isGitRepo"].as_bool())
                .unwrap_or(true);
            let form = cx.new(|cx| {
                PaseoWorktreeForm::new(
                    self.remote_state.worktree_info.as_ref(),
                    self.settings.remote_worktree.as_ref(),
                    cx,
                )
            });
            *self.remote_state.worktree_form.borrow_mut() = Some(form.downgrade());
            events.push(cx.subscribe_in(
                &form,
                window,
                |this, _, event: &PaseoWorktreeEvent, window, cx| match event {
                    PaseoWorktreeEvent::Confirmed(spec) => {
                        this.apply_paseo_worktree_spec(spec.clone(), cx);
                        this.close_menu(window, cx);
                    }
                },
            ));
            let focus = form.read(cx).focus(cx);
            items.push(
                MenuAction::new(
                    "workspace:paseo:new-worktree",
                    crate::i18n::t("新建 Paseo Git Worktree…"),
                )
                .description(if is_git_repo {
                    "由 Paseo 创建并管理独立 Git Worktree 分支"
                } else {
                    "当前项目不是 Git 仓库"
                })
                .icon("folder-tree")
                .disabled(!is_git_repo)
                .checked(self.settings.remote_worktree.is_some())
                .flyout(Flyout {
                    view: form.into(),
                    width: 280.,
                    padding: 10.,
                    height: None,
                    max_height: Some(320.),
                    focus: Some(focus),
                    surface: None,
                })
                .into(),
            );
        }
        let non_primary_worktrees: Vec<&Value> = self
            .remote_state
            .worktree_info
            .as_ref()
            .and_then(|info| info["worktrees"].as_array())
            .into_iter()
            .flatten()
            .filter(|w| w["containsCwd"].as_bool() != Some(true) && w["path"].as_str().is_some())
            .collect();
        if !non_primary_worktrees.is_empty() {
            items.push(MenuItem::Separator);
            items.push(MenuItem::Header(crate::i18n::t("已有 Git Worktree").into()));
            let selected_host_path = self
                .settings
                .worktree_cwd
                .as_deref()
                .and_then(monocode_core::remote::parse_remote_path)
                .map(|r| r.host_path);
            for wt in non_primary_worktrees {
                let Some(path) = wt["path"].as_str() else {
                    continue;
                };
                let branch = wt["branchName"].as_str().filter(|s| !s.trim().is_empty());
                let label = branch.unwrap_or(path).to_owned();
                let checked = self.settings.remote_worktree.is_none()
                    && self.settings.remote_workspace_id.is_none()
                    && selected_host_path
                        .as_deref()
                        .is_some_and(|p| p.trim_end_matches('/') == path.trim_end_matches('/'));
                let mut action =
                    MenuAction::new(format!("workspace:paseo:worktree-path:{path}"), label)
                        .description(path.to_owned())
                        .icon("folder-tree")
                        .checked(checked);
                if wt["workspaceId"].as_str().is_some() {
                    action = action.badge(crate::i18n::t("已有工作区"));
                }
                items.push(action.into());
            }
        }
        if !self.remote_state.workspace_choices.is_empty() {
            items.push(MenuItem::Separator);
            items.push(MenuItem::Header("加入已有工作区（同项目）".into()));
            for workspace in &self.remote_state.workspace_choices {
                let Some(id) = workspace["id"].as_str() else {
                    continue;
                };
                let cwd = workspace["cwd"].as_str().unwrap_or(id);
                let enriched = self
                    .remote_state
                    .worktree_info
                    .as_ref()
                    .and_then(|info| info["workspaces"].as_array())
                    .and_then(|list| list.iter().find(|w| w["id"].as_str() == Some(id)));
                let branch = workspace["branch"]
                    .as_str()
                    .or_else(|| enriched.and_then(|w| w["branch"].as_str()))
                    .filter(|s| !s.trim().is_empty());
                let is_wt = workspace["isWorktree"].as_bool().unwrap_or(false)
                    || enriched
                        .and_then(|w| w["isWorktree"].as_bool())
                        .unwrap_or(false);
                let description = match branch {
                    Some(b) => format!("{b} · {cwd}"),
                    None => cwd.to_owned(),
                };
                items.push(
                    MenuAction::new(
                        format!("workspace:paseo:existing:{id}"),
                        workspace["title"].as_str().unwrap_or(id).to_owned(),
                    )
                    .description(description)
                    .icon(if is_wt { "folder-tree" } else { "folder" })
                    .checked(self.settings.remote_workspace_id.as_deref() == Some(id))
                    .into(),
                );
            }
        }
        items
    }
    pub(super) fn paseo_workspace_choice(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.paseo_workspace_draft() {
            self.status = "已有 Paseo 对话不能在这里变更工作区归属".into();
            cx.notify();
            return;
        }
        if id == "workspace:paseo:new" {
            self.settings.remote_workspace_id = None;
            self.settings.remote_worktree = None;
            self.settings.new_worktree = false;
            self.settings.worktree_cwd = None;
            self.settings.worktree_removed = false;
        } else if id == "workspace:paseo:new-worktree" {
            if self
                .remote_state
                .worktree_info
                .as_ref()
                .is_some_and(|info| info["isGitRepo"] == false)
            {
                self.status = "当前 Paseo 项目不是 Git 仓库，无法创建 Git Worktree".into();
                cx.notify();
                return;
            }
            let base_branch = self
                .remote_state
                .worktree_info
                .as_ref()
                .and_then(|info| info["currentBranch"].as_str())
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned);
            self.settings.remote_worktree = Some(monocode_core::agent::RemoteWorktreeSpec {
                action: "branch-off".into(),
                branch_name: None,
                base_branch: base_branch.clone(),
                ref_name: base_branch,
            });
            self.settings.remote_workspace_id = None;
            self.settings.new_worktree = false;
            self.settings.worktree_cwd = None;
            self.settings.worktree_removed = false;
        } else if let Some(path) = id.strip_prefix("workspace:paseo:worktree-path:") {
            let valid = self
                .remote_state
                .worktree_info
                .as_ref()
                .and_then(|info| info["worktrees"].as_array())
                .is_some_and(|list| {
                    list.iter().any(|w| {
                        w["path"]
                            .as_str()
                            .is_some_and(|p| p.trim_end_matches('/') == path.trim_end_matches('/'))
                    })
                });
            if !valid {
                self.status = "所选 Git Worktree 不存在或不属于当前项目".into();
                cx.notify();
                return;
            }
            let Some(project_remote) = monocode_core::remote::parse_remote_path(&self.summary.cwd)
            else {
                return;
            };
            self.settings.worktree_cwd = Some(monocode_core::remote::remote_path(
                &project_remote.environment_id,
                path,
            ));
            self.settings.remote_workspace_id = None;
            self.settings.remote_worktree = None;
            self.settings.new_worktree = false;
            self.settings.worktree_removed = false;
        } else if let Some(workspace) = id.strip_prefix("workspace:paseo:existing:") {
            if !self
                .remote_state
                .workspace_choices
                .iter()
                .any(|w| w["id"] == workspace)
            {
                self.status = "所选 Paseo 工作区已不可用，请重新打开新建页".into();
                cx.notify();
                return;
            }
            self.settings.remote_workspace_id = Some(workspace.to_owned());
            self.settings.remote_worktree = None;
            self.settings.new_worktree = false;
            self.settings.worktree_cwd = None;
            self.settings.worktree_removed = false;
        } else {
            return;
        }
        self.save_settings(cx);
        cx.notify();
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
