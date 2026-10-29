//! Paseo archive / unarchive / rename through the real Shell against a stateful loopback host.
use super::session_groups::RenameTarget;
use super::*;
use gpui::TestAppContext;
use monocode_core::store::Store;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicU64};

static SERIAL: Mutex<()> = Mutex::new(());
static DEFAULT_PINNED: AtomicBool = AtomicBool::new(false);

/// While a fixture lives, `Shell::new` leaves the process-wide default transport alone.
pub(super) fn default_pinned() -> bool {
    DEFAULT_PINNED.load(Ordering::Relaxed)
}

#[derive(Default)]
struct HostState {
    rows: Vec<Value>,
    requests: Vec<Value>,
    reject_update: Option<String>,
    /// The next `sessions.list` captures its rows, then waits for this flag.
    list_gate: Option<Arc<AtomicBool>>,
    /// Paseo closes the agent runtime when it archives it.
    stop_on_archive: bool,
}

struct FakeHost {
    _serial: std::sync::MutexGuard<'static, ()>,
    client: Arc<monocode_core::remote::Connections>,
    environment: String,
    state: Arc<Mutex<HostState>>,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
    directory: std::path::PathBuf,
}

fn host_row(id: &str, title: &str, parent: Option<&str>) -> Value {
    json!({"id":id,"title":title,"harness":"codex","model":"codex:test","cwd":"/host/ws","updatedAt":1000,
        "status":"idle","archived":false,"pinned":false,"hasMessages":true,"paseoBucket":"done",
        "paseoWorkspaceAnchor":parent.is_none(),"paseoParentAgentId":parent})
}

fn snapshot(row: &Value, revision: u64) -> Value {
    let mut value = row.clone();
    let running = row["status"] == "running";
    value["projectId"] = json!("p");
    value["revision"] = json!(revision);
    if running {
        value["runId"] = json!("run-1");
    }
    value["session"] = json!({"id":row["id"],"harness":"codex","model":"codex:test","title":row["title"],
        "runtimeMode":"supervised","modelSettings":{},"cwd":"/host/ws",
        "blocks":[{"id":"u","role":"user","text":"hello","startedAt":1,"durationMs":1}]});
    value
}

fn respond(
    state: &Mutex<HostState>,
    request: &Value,
    revision: &AtomicU64,
) -> Result<Value, String> {
    let method = request["method"].as_str().unwrap_or_default();
    let params = &request["params"];
    let (rows, gate) = {
        let mut state = state.lock().unwrap();
        state.requests.push(request.clone());
        let gate = if method == "sessions.list" {
            state.list_gate.take()
        } else {
            None
        };
        (state.rows.clone(), gate)
    };
    let state_guard = || state.lock().unwrap();
    match method {
        "environment.describe" => Ok(json!({"protocolVersion":1,"environmentId":"",
            "name":"Fake","providers":["codex"],"capabilities":["sessions.sync"]})),
        "sessions.list" => {
            if let Some(gate) = gate {
                // Bounded, so a failed test still drops the host instead of hanging.
                let started = std::time::Instant::now();
                while !gate.load(Ordering::Relaxed) && started.elapsed().as_secs() < 20 {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            Ok(json!(rows))
        }
        "sessions.sync" => {
            let row = rows
                .iter()
                .find(|r| r["id"] == params["sessionId"])
                .ok_or("Unknown session")?;
            Ok(json!({"kind":"snapshot","value":snapshot(row, revision.load(Ordering::Relaxed))}))
        }
        "sessions.update" => {
            let mut state = state_guard();
            if let Some(error) = state.reject_update.clone() {
                return Err(error);
            }
            let stop = state.stop_on_archive;
            let id = params["sessionId"].clone();
            for row in &mut state.rows {
                let child = row["paseoParentAgentId"] == id && params["archived"] == true;
                if row["id"] != id && !child {
                    continue;
                }
                if let Some(archived) = params["archived"].as_bool() {
                    row["archived"] = json!(archived);
                    if archived && stop {
                        row["status"] = json!("idle");
                    }
                }
                if row["id"] == id
                    && let Some(title) = params["title"].as_str()
                {
                    row["title"] = json!(title);
                }
            }
            revision.fetch_add(1, Ordering::Relaxed);
            Ok(json!({"id":id}))
        }
        "commands.dispatch" => {
            if params["type"] == "cancel" {
                let mut state = state_guard();
                if state
                    .rows
                    .iter()
                    .any(|r| r["id"] == params["sessionId"] && r["archived"] == true)
                {
                    return Err("Agent is archived".into());
                }
                for row in &mut state.rows {
                    if row["id"] == params["sessionId"] {
                        row["status"] = json!("idle");
                    }
                }
                revision.fetch_add(1, Ordering::Relaxed);
            }
            Ok(
                json!({"commandId":params["commandId"],"sessionId":params["sessionId"],"revision":revision.load(Ordering::Relaxed)}),
            )
        }
        _ => Ok(json!({})),
    }
}

impl FakeHost {
    fn new(rows: Vec<Value>) -> Self {
        use std::io::{BufRead, Read, Write};
        let serial = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let environment = format!("paseo-archive-{}-{nonce}", std::process::id());
        let directory = std::env::temp_dir().join(&environment);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(HostState {
            rows,
            stop_on_archive: true,
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (served, done, env) = (state.clone(), stop.clone(), environment.clone());
        let server = std::thread::spawn(move || {
            let revision = Arc::new(AtomicU64::new(1));
            let mut workers = Vec::new();
            while !done.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("host accept: {error}"),
                };
                let (state, revision, env) = (served.clone(), revision.clone(), env.clone());
                workers.push(std::thread::spawn(move || {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" {
                            break;
                        }
                        if let Some((key, value)) = line.split_once(':')
                            && key.eq_ignore_ascii_case("content-length")
                        {
                            length = value.trim().parse().unwrap();
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    let outcome = respond(&state, &request, &revision);
                    let body = match outcome {
                        Ok(mut result) => {
                            if request["method"] == "environment.describe" {
                                result["environmentId"] = json!(env);
                            }
                            json!({"result": result})
                        }
                        Err(error) => json!({"error": error}),
                    };
                    let body = body.to_string();
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                }));
            }
            for worker in workers {
                let _ = worker.join();
            }
        });
        let client = Arc::new(monocode_core::remote::Connections::new(&directory));
        client.connect(String::new(), url, "a".repeat(43)).unwrap();
        DEFAULT_PINNED.store(true, Ordering::Relaxed);
        monocode_core::remote::set_default_connections(client.clone());
        Self {
            _serial: serial,
            client,
            environment,
            state,
            stop,
            server: Some(server),
            directory,
        }
    }
    fn sent(&self, method: &str) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["method"] == method)
            .map(|r| r["params"].clone())
            .collect()
    }
}
impl Drop for FakeHost {
    fn drop(&mut self) {
        DEFAULT_PINNED.store(false, Ordering::Relaxed);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

struct Ui {
    window: gpui::WindowHandle<Shell>,
    root: i64,
    kid: i64,
    solo: i64,
}

fn wait(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    for _ in 0..3000 {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("condition not reached");
}

fn rows() -> Vec<Value> {
    vec![
        host_row("root", "Workspace", None),
        host_row("kid", "Research child", Some("root")),
        host_row("solo", "Solo agent", None),
    ]
}

/// A Paseo-mode Shell over the host's three rows with `tabs` (host ids) open; the last is active.
fn open(cx: &mut TestAppContext, host: &FakeHost, tabs: &[&str]) -> Ui {
    cx.update(|cx| {
        let mut preferences = crate::settings::Preferences {
            paced_reveal: false,
            ..Default::default()
        };
        preferences
            .extra
            .insert(crate::paseo_host::PROJECT_SELECTION.into(), json!({}));
        crate::theme::configure(&preferences, false);
        cx.set_global(preferences);
        crate::input::bind(cx);
        crate::popover::bind(cx);
        crate::shell::bind_keys(cx);
    });
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    let project = store
        .remember_remote_project(
            &host.environment,
            &json!({"id":"p","cwd":"/host/ws","name":"WS"}),
        )
        .unwrap();
    let values = host.state.lock().unwrap().rows.clone();
    let registered = store.sync_remote_summaries(&project, &values).unwrap();
    let local = |host_id: &str| {
        registered
            .iter()
            .find(|r| r.session_id.as_deref() == Some(host_id))
            .unwrap()
            .summary
            .id
    };
    let (root, kid, solo) = (local("root"), local("kid"), local("solo"));
    let summaries = store.sessions().unwrap();
    let mut state = Workspace::default();
    state.add_project(&project.key);
    for tab in tabs {
        state.open(local(tab));
    }
    state
        .preferences
        .extra
        .insert(crate::paseo_host::PROJECT_SELECTION.into(), json!({}));
    crate::paseo_host::restrict_workspace(&mut state);
    let saved = serde_json::to_string(&state).unwrap();
    let store = Arc::new(Mutex::new(store));
    let client = host.client.clone();
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| {
                let mut shell = Shell::new(store, summaries, project.key, Some(saved), cx);
                shell.remote = client;
                shell
            })
        })
        .unwrap()
    });
    cx.refresh().unwrap();
    cx.run_until_parked();
    Ui {
        window,
        root,
        kid,
        solo,
    }
}

fn read<R>(
    ui: &Ui,
    cx: &mut TestAppContext,
    f: impl FnOnce(&mut Shell, &mut Context<Shell>) -> R,
) -> R {
    ui.window.update(cx, |shell, _, cx| f(shell, cx)).unwrap()
}

fn listed(ui: &Ui, cx: &mut TestAppContext, id: i64) -> bool {
    read(ui, cx, |shell, cx| {
        shell.sidebar_session_ids(cx).contains(&id)
    })
}

fn archive_label(ui: &Ui, cx: &mut TestAppContext, id: i64) -> String {
    read(ui, cx, |shell, cx| {
        shell
            .session_menu_items(id, cx)
            .into_iter()
            .find_map(|item| match item {
                MenuItem::Action(action) if action.id.as_ref() == "archive" => {
                    Some(action.label.to_string())
                }
                _ => None,
            })
            .unwrap()
    })
}

#[gpui::test]
fn archiving_a_child_row_sends_one_update_and_unarchive_restores_it(cx: &mut TestAppContext) {
    let host = FakeHost::new(rows());
    let ui = open(cx, &host, &["root"]);
    assert!(listed(&ui, cx, ui.kid) && listed(&ui, cx, ui.root));
    assert_eq!(archive_label(&ui, cx, ui.kid), "Archive");
    ui.window
        .update(cx, |shell, w, cx| {
            shell.run_menu(MenuTarget::Session(ui.kid), "archive", None, w, cx)
        })
        .unwrap();
    wait(cx, |_| !host.sent("sessions.update").is_empty());
    wait(cx, |cx| !listed(&ui, cx, ui.kid));
    assert_eq!(
        host.sent("sessions.update"),
        [json!({"sessionId":"kid","projectId":"p","archived":true})]
    );
    assert!(listed(&ui, cx, ui.root) && listed(&ui, cx, ui.solo));
    read(&ui, cx, |shell, cx| {
        assert!(shell.error.is_none(), "{:?}", shell.error);
        shell.workspace.preferences.show_archived = true;
        shell.invalidate_chrome(cx);
    });
    assert!(listed(&ui, cx, ui.kid));
    assert_eq!(archive_label(&ui, cx, ui.kid), "Unarchive");
    ui.window
        .update(cx, |shell, w, cx| {
            shell.run_menu(MenuTarget::Session(ui.kid), "archive", None, w, cx)
        })
        .unwrap();
    wait(cx, |_| host.sent("sessions.update").len() == 2);
    wait(cx, |cx| archive_label(&ui, cx, ui.kid) == "Archive");
    assert_eq!(
        host.sent("sessions.update")[1],
        json!({"sessionId":"kid","projectId":"p","archived":false})
    );
    read(&ui, cx, |shell, _| assert!(shell.error.is_none()));
}

fn refresh(ui: &Ui, cx: &mut TestAppContext) {
    read(ui, cx, |shell, cx| shell.refresh_remote_projects(cx));
}
fn settle_refresh(ui: &Ui, cx: &mut TestAppContext) {
    wait(cx, |cx| !read(ui, cx, |shell, _| shell.remote_refreshing));
}
fn archived(ui: &Ui, cx: &mut TestAppContext, id: i64) -> bool {
    read(ui, cx, |shell, cx| {
        shell.session_metadata(id, cx).summary.archived
    })
}
fn title(ui: &Ui, cx: &mut TestAppContext, id: i64) -> String {
    read(ui, cx, |shell, cx| {
        shell.session_metadata(id, cx).summary.title.to_string()
    })
}
fn host_set(host: &FakeHost, id: &str, key: &str, value: Value) {
    for row in &mut host.state.lock().unwrap().rows {
        if row["id"] == id {
            row[key] = value.clone();
        }
    }
}

#[gpui::test]
fn archiving_a_top_level_row_closes_its_tab_from_the_tab_menu(cx: &mut TestAppContext) {
    let host = FakeHost::new(rows());
    let ui = open(cx, &host, &["root", "solo"]);
    assert!(read(&ui, cx, |s, _| s.workspace.tab_of(ui.solo).is_some()));
    assert_eq!(
        read(&ui, cx, |s, cx| { s.session_menu_items(ui.solo, cx).len() }) > 0,
        true
    );
    ui.window
        .update(cx, |shell, w, cx| {
            shell.run_menu(MenuTarget::Tab(ui.solo), "archive", None, w, cx)
        })
        .unwrap();
    wait(cx, |cx| !listed(&ui, cx, ui.solo));
    assert_eq!(
        host.sent("sessions.update"),
        [json!({"sessionId":"solo","projectId":"p","archived":true})]
    );
    read(&ui, cx, |shell, _| {
        assert!(shell.workspace.tab_of(ui.solo).is_none(), "tab closes");
        assert!(shell.workspace.tab_of(ui.root).is_some());
        assert!(shell.error.is_none(), "{:?}", shell.error);
    });
    // The Archived filter shows it again under the Unarchive label.
    read(&ui, cx, |shell, cx| {
        shell.workspace.preferences.show_archived = true;
        shell.invalidate_chrome(cx);
    });
    assert!(listed(&ui, cx, ui.solo));
    assert_eq!(archive_label(&ui, cx, ui.solo), "Unarchive");
    assert!(archived(&ui, cx, ui.solo) && !archived(&ui, cx, ui.root));
}

/// A host that keeps reporting a running turn would block this test's thread:
/// the GPUI test executor runs the remote observer loop inline. The turn is
/// therefore live only in the conversation; the host's own stop on archive is
/// covered by the adapter tests (`archiveAgent` closes the agent runtime).
#[gpui::test]
fn archiving_a_busy_remote_conversation_stops_it_without_a_visible_error(cx: &mut TestAppContext) {
    let host = FakeHost::new(rows());
    let ui = open(cx, &host, &["solo"]);
    wait(cx, |cx| {
        read(&ui, cx, |s, _| s.sessions.contains_key(&ui.solo))
    });
    read(&ui, cx, |shell, cx| {
        shell.sessions[&ui.solo].update(cx, |view, _| view.busy = true);
        shell.busy_sessions.insert(ui.solo);
    });
    ui.window
        .update(cx, |shell, w, cx| {
            shell.run_menu(MenuTarget::Tab(ui.solo), "archive", None, w, cx)
        })
        .unwrap();
    wait(cx, |cx| !listed(&ui, cx, ui.solo));
    read(&ui, cx, |shell, cx| {
        assert!(shell.error.is_none(), "{:?}", shell.error);
        assert!(shell.workspace.tab_of(ui.solo).is_none(), "tab closes");
        let view = shell.sessions.get(&ui.solo).expect("view").read(cx);
        assert!(!view.busy, "no turn may stay running after archive");
        assert!(view.summary.archived);
    });
    // Reopening the archived conversation shows no host error / Retry notice.
    ui.window
        .update(cx, |shell, w, cx| {
            shell.workspace.preferences.show_archived = true;
            shell.open(ui.solo, w, cx);
        })
        .unwrap();
    let visual = &mut gpui::VisualTestContext::from_window(ui.window.into(), cx);
    for _ in 0..20 {
        visual.run_until_parked();
        visual.refresh().unwrap();
    }
    assert!(
        visual.debug_bounds("remote-retry").is_none(),
        "error notice visible after archive"
    );
    assert_eq!(
        host.sent("sessions.update"),
        [json!({"sessionId":"solo","projectId":"p","archived":true})]
    );
    assert!(
        host.sent("commands.dispatch")
            .iter()
            .all(|c| c["type"] != "cancel")
    );
}

#[gpui::test]
fn a_rejected_archive_leaves_the_row_unchanged_and_shows_the_error(cx: &mut TestAppContext) {
    let host = FakeHost::new(rows());
    host.state.lock().unwrap().reject_update = Some("请在 Paseo 中修改此设置".into());
    let ui = open(cx, &host, &["root", "kid"]);
    ui.window
        .update(cx, |shell, w, cx| {
            shell.run_menu(MenuTarget::Session(ui.kid), "archive", None, w, cx)
        })
        .unwrap();
    wait(cx, |cx| read(&ui, cx, |s, _| s.error.is_some()));
    read(&ui, cx, |shell, _| {
        let error = shell.error.clone().unwrap();
        assert!(error.contains("Could not update archive"), "{error}");
        assert!(error.contains("请在 Paseo 中修改此设置"), "{error}");
        assert!(shell.workspace.tab_of(ui.kid).is_some(), "tab stays open");
        assert!(shell.archiving.is_empty());
    });
    assert!(!archived(&ui, cx, ui.kid) && listed(&ui, cx, ui.kid));
    assert_eq!(archive_label(&ui, cx, ui.kid), "Archive");
    refresh(&ui, cx);
    settle_refresh(&ui, cx);
    assert!(!archived(&ui, cx, ui.kid));
}

#[gpui::test]
fn index_refresh_keeps_archive_state_consistent_with_the_host(cx: &mut TestAppContext) {
    let host = FakeHost::new(rows());
    let ui = open(cx, &host, &["root"]);
    ui.window
        .update(cx, |shell, w, cx| {
            shell.run_menu(MenuTarget::Session(ui.kid), "archive", None, w, cx)
        })
        .unwrap();
    wait(cx, |cx| archived(&ui, cx, ui.kid));
    refresh(&ui, cx);
    settle_refresh(&ui, cx);
    assert!(archived(&ui, cx, ui.kid) && !listed(&ui, cx, ui.kid));
    // Unarchived elsewhere (the Paseo app): the next index brings it back.
    host_set(&host, "kid", "archived", json!(false));
    refresh(&ui, cx);
    settle_refresh(&ui, cx);
    assert!(!archived(&ui, cx, ui.kid) && listed(&ui, cx, ui.kid));
}

/// An index request held by the host on a real thread, as the shell's refresh
/// worker does in the app; it captured the host rows before `release`.
fn stale_index(
    ui: &Ui,
    cx: &mut TestAppContext,
    host: &FakeHost,
) -> (
    Arc<AtomicBool>,
    std::thread::JoinHandle<Result<bool, String>>,
) {
    let gate = Arc::new(AtomicBool::new(false));
    host.state.lock().unwrap().list_gate = Some(gate.clone());
    let (store, client, project) = read(ui, cx, |s, _| {
        (
            s.store.clone(),
            s.remote.clone(),
            s.workspace.project.clone(),
        )
    });
    let worker = std::thread::spawn(move || {
        crate::runtime::remote::refresh_project_index(&store, &client, &project, None, false)
            .map(|index| index.is_some())
    });
    let started = std::time::Instant::now();
    while host.sent("sessions.list").is_empty() {
        assert!(
            started.elapsed().as_secs() < 10,
            "index request never reached the host"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    (gate, worker)
}

#[gpui::test]
fn a_stale_in_flight_index_does_not_flip_an_archive_back(cx: &mut TestAppContext) {
    let host = FakeHost::new(rows());
    let ui = open(cx, &host, &["root"]);
    let (gate, stale) = stale_index(&ui, cx, &host);
    // The list above captured the pre-archive rows. Archive while it waits.
    ui.window
        .update(cx, |shell, w, cx| {
            shell.run_menu(MenuTarget::Session(ui.kid), "archive", None, w, cx)
        })
        .unwrap();
    wait(cx, |cx| archived(&ui, cx, ui.kid));
    gate.store(true, Ordering::Relaxed);
    assert_eq!(
        stale.join().unwrap(),
        Ok(false),
        "a pre-archive index must be discarded"
    );
    assert!(
        archived(&ui, cx, ui.kid),
        "stale index flipped the archive back"
    );
    assert!(!listed(&ui, cx, ui.kid));
    let stored = read(&ui, cx, |shell, _| {
        locked(&shell.store)
            .unwrap()
            .sessions()
            .unwrap()
            .into_iter()
            .find(|s| s.id == ui.kid)
            .unwrap()
            .archived
    });
    assert!(stored, "stale index overwrote the stored archive");
    // The next index confirms it from the host.
    refresh(&ui, cx);
    settle_refresh(&ui, cx);
    assert!(archived(&ui, cx, ui.kid) && !listed(&ui, cx, ui.kid));
}

fn commit_rename(ui: &Ui, cx: &mut TestAppContext, name: &str) {
    ui.window
        .update(cx, |shell, w, cx| {
            let input = shell
                .renaming
                .as_ref()
                .expect("rename started")
                .input
                .clone();
            input.update(cx, |input, cx| input.set_text(name.to_string().into(), cx));
            shell.finish_rename(true, w, cx);
        })
        .unwrap();
}

#[gpui::test]
fn renaming_child_and_top_level_rows_sends_the_title_for_that_session(cx: &mut TestAppContext) {
    let host = FakeHost::new(rows());
    let ui = open(cx, &host, &["root"]);
    for (id, host_id, name) in [
        (ui.kid, "kid", "Renamed child"),
        (ui.solo, "solo", "Renamed solo"),
    ] {
        ui.window
            .update(cx, |shell, w, cx| {
                shell.run_menu(MenuTarget::Session(id), "rename", None, w, cx)
            })
            .unwrap();
        commit_rename(&ui, cx, name);
        assert_eq!(title(&ui, cx, id), name, "optimistic sidebar title");
        wait(cx, |_| {
            host.sent("sessions.update")
                .iter()
                .any(|p| p["sessionId"] == host_id)
        });
        wait(cx, |cx| {
            locked(&read(&ui, cx, |s, _| s.store.clone()))
                .unwrap()
                .sessions()
                .unwrap()
                .iter()
                .any(|s| s.id == id && s.title == name)
        });
    }
    assert_eq!(
        host.sent("sessions.update"),
        [
            json!({"sessionId":"kid","projectId":"p","title":"Renamed child"}),
            json!({"sessionId":"solo","projectId":"p","title":"Renamed solo"}),
        ]
    );
    read(&ui, cx, |s, _| assert!(s.error.is_none(), "{:?}", s.error));
    refresh(&ui, cx);
    settle_refresh(&ui, cx);
    assert_eq!(title(&ui, cx, ui.kid), "Renamed child");
    assert_eq!(title(&ui, cx, ui.solo), "Renamed solo");
    assert_eq!(title(&ui, cx, ui.root), "Workspace");
}

#[gpui::test]
fn f2_renames_the_active_child_tab(cx: &mut TestAppContext) {
    let host = FakeHost::new(rows());
    let ui = open(cx, &host, &["root", "kid"]);
    assert_eq!(read(&ui, cx, |s, _| s.workspace.active), Some(ui.kid));
    ui.window
        .update(cx, |shell, w, cx| {
            shell.session_hover = None;
            shell.rename_sidebar_item(w, cx);
        })
        .unwrap();
    read(&ui, cx, |s, _| {
        assert!(
            matches!(&s.renaming, Some(r) if r.target == RenameTarget::Session(s.workspace.active.unwrap()))
        );
    });
    commit_rename(&ui, cx, "F2 name");
    assert_eq!(title(&ui, cx, ui.kid), "F2 name");
    wait(cx, |_| !host.sent("sessions.update").is_empty());
    assert_eq!(
        host.sent("sessions.update"),
        [json!({"sessionId":"kid","projectId":"p","title":"F2 name"})]
    );
    wait(cx, |cx| {
        read(&ui, cx, |s, _| {
            s.group_writes.as_ref().is_none_or(|t| t.is_ready())
        })
    });
    refresh(&ui, cx);
    settle_refresh(&ui, cx);
    assert_eq!(title(&ui, cx, ui.kid), "F2 name");
}

#[gpui::test]
fn a_stale_in_flight_index_does_not_overwrite_a_fresh_rename(cx: &mut TestAppContext) {
    let host = FakeHost::new(rows());
    let ui = open(cx, &host, &["root"]);
    let (gate, stale) = stale_index(&ui, cx, &host);
    ui.window
        .update(cx, |shell, w, cx| {
            shell.run_menu(MenuTarget::Session(ui.kid), "rename", None, w, cx)
        })
        .unwrap();
    commit_rename(&ui, cx, "Fresh name");
    wait(cx, |_| !host.sent("sessions.update").is_empty());
    // Let the rename's store write finish before the stale index lands.
    wait(cx, |cx| {
        read(&ui, cx, |s, _| {
            s.group_writes.as_ref().is_none_or(|t| t.is_ready())
        })
    });
    gate.store(true, Ordering::Relaxed);
    assert_eq!(
        stale.join().unwrap(),
        Ok(false),
        "a pre-rename index must be discarded"
    );
    assert_eq!(
        title(&ui, cx, ui.kid),
        "Fresh name",
        "stale index reverted the rename"
    );
    let stored = read(&ui, cx, |shell, _| {
        locked(&shell.store)
            .unwrap()
            .sessions()
            .unwrap()
            .into_iter()
            .find(|s| s.id == ui.kid)
            .unwrap()
            .title
    });
    assert_eq!(stored, "Fresh name");
    refresh(&ui, cx);
    settle_refresh(&ui, cx);
    assert_eq!(title(&ui, cx, ui.kid), "Fresh name");
}
