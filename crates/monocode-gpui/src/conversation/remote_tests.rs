//! UI invariants around the host-owned conversation and the next local draft.
use super::*;
use gpui::TestAppContext;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Mutex;

fn conversation(cx: &mut TestAppContext) -> Entity<Conversation> {
    let store = monocode_core::store::Store::open(Path::new(":memory:")).unwrap();
    let mut summary = store.create("codex", "/test-project").unwrap();
    summary.cwd = "remote://remote-ui-tests/home/me/project".into();
    cx.update(|cx| {
        cx.new(|cx| {
            let mut view = Conversation::new(summary, Arc::new(Mutex::new(store)), cx);
            view.loaded = true;
            view.remote_state.online = true;
            view.remote_state.capabilities =
                ["sessions.draft", "sessions.plan", "attachments.upload"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
            view.input.update(cx, |input, cx| {
                input.set_text("Keep this next message".into(), cx)
            });
            view
        })
    })
}

#[gpui::test]
fn offline_pending_and_busy_guards_preserve_typed_message(cx: &mut TestAppContext) {
    let view = conversation(cx);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            for (loaded, online, pending, busy, expected) in [
                (
                    false,
                    true,
                    false,
                    false,
                    "Wait for this remote conversation",
                ),
                (true, false, false, false, "machine is offline"),
                (true, true, true, false, "command needs confirmation"),
                (true, true, false, true, "remote turn to finish"),
            ] {
                view.loaded = loaded;
                view.remote_state.online = online;
                view.remote_state.pending = pending;
                view.busy = busy;
                view.send(cx);
                assert!(view.status.contains(expected), "{}", view.status);
                assert_eq!(view.input.read(cx).content, "Keep this next message");
                assert!(view.follow_ups.state.messages.is_empty());
                assert!(view.transcript.read(cx).source_rows().is_empty());
            }
            view.busy = false;
        })
    });
}

#[gpui::test]
fn remote_capability_and_command_guards_keep_composer_and_attachments(cx: &mut TestAppContext) {
    let view = conversation(cx);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.remote_state.capabilities.clear();
            view.attachments = vec![PathBuf::from("/home/me/design.png")];
            assert!(view.remote_send_guard(cx));
            assert!(view.status.contains("attachments"));
            assert_eq!(view.attachments.len(), 1);
            view.attachments.clear();
            view.settings.plan = true;
            assert!(view.remote_send_guard(cx));
            assert!(view.status.contains("Plan mode"));
            view.settings.plan = false;
            view.follow_ups.draft_selected = true;
            assert!(view.remote_send_guard(cx));
            assert!(view.status.contains("save drafts"));
            view.follow_ups.draft_selected = false;
            for command in [
                "/operator delegate this",
                "/orchestrate a task",
                "/add-to-folder name",
            ] {
                view.input
                    .update(cx, |input, cx| input.set_text(command.into(), cx));
                assert!(view.remote_send_guard(cx));
                assert_eq!(view.input.read(cx).content.as_ref(), command);
            }
        })
    });
}

#[gpui::test]
fn restoring_host_saved_draft_preserves_the_next_typed_message(cx: &mut TestAppContext) {
    let view = conversation(cx);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.install_remote_draft(&json!({"session": {"blocks": [
            {"id": "saved-remote-draft", "role": "user", "draft": true, "text": "Saved on the host"}
        ]}}), cx);
            let draft = view.follow_ups.state.draft.as_ref().unwrap();
            assert_eq!(draft.id, "saved-remote-draft");
            assert_eq!(draft.text, "Saved on the host");
            assert_eq!(view.input.read(cx).content, "Keep this next message");
            view.install_remote_draft(&json!({"session": {"blocks": []}}), cx);
            assert!(view.follow_ups.state.draft.is_none());
            assert_eq!(view.input.read(cx).content, "Keep this next message");
        })
    });
}

#[gpui::test]
fn dropping_remote_conversation_does_not_cancel_its_host_turn(cx: &mut TestAppContext) {
    let view = conversation(cx);
    let cancel = cx.update(|cx| view.read(cx).cancel.clone());
    cx.update(|_| drop(view));
    cx.run_until_parked();
    assert!(!cancel.load(Ordering::Relaxed));

    let local = conversation(cx);
    let local_cancel = cx.update(|cx| {
        local.update(cx, |view, _| {
            view.summary.cwd = "/local/project".into();
            view.cancel.clone()
        })
    });
    cx.update(|_| drop(local));
    cx.run_until_parked();
    assert!(local_cancel.load(Ordering::Relaxed));
}

struct HostFixture {
    client: Arc<monocode_core::remote::Connections>,
    store: Database,
    summary: monocode_core::store::Session,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
    directory: PathBuf,
    view_barrier: Option<Arc<AtomicBool>>,
    sync_barrier: Option<Arc<AtomicBool>>,
}
impl HostFixture {
    fn new(running: bool, saved_draft: bool) -> Self {
        Self::with_view_barrier(running, saved_draft, None)
    }
    fn with_view_barrier(running: bool, saved_draft: bool, view_barrier: Option<Arc<AtomicBool>>) -> Self {
        Self::with_barriers(running, saved_draft, view_barrier, None)
    }
    /// A Paseo host whose `sessions.sync` blocks until `sync_barrier` is set.
    fn with_sync_barrier(sync_barrier: Arc<AtomicBool>) -> Self {
        Self::with_barriers(false, false, None, Some(sync_barrier))
    }
    fn with_barriers(
        running: bool,
        saved_draft: bool,
        view_barrier: Option<Arc<AtomicBool>>,
        sync_barrier: Option<Arc<AtomicBool>>,
    ) -> Self {
        use std::io::{BufRead, Read, Write};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let environment = format!("remote-ui-{}-{nonce}", std::process::id());
        let directory = std::env::temp_dir().join(&environment);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mut descriptor = json!({"protocolVersion": 1, "environmentId": environment, "name": "UI test host", "providers": ["codex"], "capabilities": ["sessions.draft", "sessions.plan", "attachments.upload", "sessions.sync"]});
        if view_barrier.is_some() || sync_barrier.is_some() {
            descriptor["capabilities"].as_array_mut().unwrap().push(json!("paseo.configure"));
        }
        let snapshot = json!({"projectId": "project", "revision": 1, "status": if running {"running"} else {"idle"}, "runId": if running {json!("host-run")} else {serde_json::Value::Null}, "updatedAt": 42,
            "session": {"id": "host-session", "title": "Conversation on host", "harness": "codex", "model": "codex:gpt-test", "runtimeMode": "supervised", "modelSettings": {}, "blocks": if saved_draft { json!([{ "id": "host-saved-draft", "role": "user", "draft": true, "text": "Saved host draft" }]) } else {json!([])} }
        });
        let initial = snapshot.clone();
        let requests = Arc::new(Mutex::new(vec![]));
        let received = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let blocked_view = view_barrier.clone();
        let blocked_sync = sync_barrier.clone();
        let server = std::thread::spawn(move || {
            let mut snapshot = snapshot;
            let mut syncs = 0;
            while !done.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("host accept: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
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
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                received.lock().unwrap().push(request.clone());
                let result = match request["method"].as_str().unwrap() {
                    "environment.describe" => descriptor.clone(),
                    "sessions.sync" => {
                        if let Some(barrier) = &blocked_sync {
                            while !barrier.load(Ordering::Relaxed) && !done.load(Ordering::Relaxed) {
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                        }
                        syncs += 1;
                        if running && syncs > 1 {
                            snapshot["status"] = json!("idle");
                            snapshot["runId"] = serde_json::Value::Null;
                            snapshot["revision"] = json!(2);
                        }
                        json!({"kind": "snapshot", "value": snapshot})
                    }
                    "commands.dispatch" => {
                        let command = &request["params"];
                        if command["type"] == "view" && let Some(barrier) = &blocked_view {
                            while !barrier.load(Ordering::Relaxed) && !done.load(Ordering::Relaxed) {
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                        }
                        json!({"commandId": command["commandId"], "sessionId": "host-session", "revision": 2})
                    }
                    _ => json!({}),
                };
                let body = json!({"result": result}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        let client = Arc::new(monocode_core::remote::Connections::new(&directory));
        client.connect(String::new(), url, "a".repeat(43)).unwrap();
        let store = monocode_core::store::Store::open(Path::new(":memory:")).unwrap();
        let project = store
            .remember_remote_project(
                &environment,
                &json!({"id": "project", "cwd": "/home/me/project", "name": "project"}),
            )
            .unwrap();
        let summary = store.register_remote_summary(&project, &initial).unwrap();
        Self {
            client,
            store: Arc::new(Mutex::new(store)),
            summary,
            requests,
            stop,
            server: Some(server),
            directory,
            view_barrier,
            sync_barrier,
        }
    }
    fn conversation(&self, cx: &mut TestAppContext) -> Entity<Conversation> {
        cx.update(|cx| {
            cx.new(|cx| {
                let mut view = Conversation::new(self.summary.clone(), self.store.clone(), cx);
                view.remote_state.client = Some(self.client.clone());
                view.input.update(cx, |input, cx| {
                    input.set_text("Keep this next message".into(), cx)
                });
                view.load_remote(cx);
                view
            })
        })
    }
    fn wait(&self, view: &Entity<Conversation>, cx: &mut TestAppContext, settled: bool) {
        for _ in 0..1000 {
            cx.run_until_parked();
            if cx.update(|cx| {
                let view = view.read(cx);
                view.loaded && (!settled || (!view.busy && !view.remote_state.refreshing))
            }) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        cx.update(|cx| {
            panic!(
                "remote conversation did not settle: {}",
                view.read(cx).status
            )
        });
    }
}
impl Drop for HostFixture {
    fn drop(&mut self) {
        if let Some(barrier) = &self.view_barrier {
            barrier.store(true, Ordering::Relaxed);
        }
        if let Some(barrier) = &self.sync_barrier {
            barrier.store(true, Ordering::Relaxed);
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(server) = self.server.take() {
            let result = server.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[gpui::test]
fn snapshot_load_restores_host_draft_without_replacing_unsent_composer(cx: &mut TestAppContext) {
    let host = HostFixture::new(false, true);
    let view = host.conversation(cx);
    host.wait(&view, cx, true);
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.remote_state.online);
        assert_eq!(view.summary.title, "Conversation on host");
        assert_eq!(
            view.follow_ups.state.draft.as_ref().unwrap().text,
            "Saved host draft"
        );
        assert_eq!(view.input.read(cx).content, "Keep this next message");
    });
    assert!(
        !host
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "commands.dispatch")
    );
}

#[gpui::test]
fn opening_running_host_reattaches_without_dispatching_another_turn(cx: &mut TestAppContext) {
    let host = HostFixture::new(true, false);
    let view = host.conversation(cx);
    host.wait(&view, cx, true);
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.remote_state.online);
        assert_eq!(view.input.read(cx).content, "Keep this next message");
        assert_eq!(
            view.remote_state.snapshot.as_ref().unwrap()["status"],
            "idle"
        );
        assert!(!view.cancel.load(Ordering::Relaxed));
    });
    let requests = host.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .filter(|r| r["method"] == "sessions.sync")
            .count()
            >= 2
    );
    assert!(!requests.iter().any(|r| r["method"] == "commands.dispatch"));
}

#[gpui::test]
fn nudging_observation_resyncs_only_loaded_idle_remote_views(cx: &mut TestAppContext) {
    let host = HostFixture::new(false, false);
    let syncs = || {
        host.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "sessions.sync")
            .count()
    };
    let refreshing = |view: &Entity<Conversation>, cx: &mut TestAppContext| {
        cx.update(|cx| view.read(cx).remote_state.refreshing)
    };
    // Unloaded views never start an initial load from a nudge.
    let unloaded = cx.update(|cx| {
        cx.new(|cx| {
            let mut view = Conversation::new(host.summary.clone(), host.store.clone(), cx);
            view.remote_state.client = Some(host.client.clone());
            view
        })
    });
    cx.update(|cx| unloaded.update(cx, |view, cx| view.nudge_remote_observation(cx)));
    assert!(!refreshing(&unloaded, cx));
    cx.run_until_parked();
    assert_eq!(syncs(), 0);

    let view = host.conversation(cx);
    host.wait(&view, cx, true);
    let settled = syncs();
    // Busy views have their own live sync loop.
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.busy = true;
            view.nudge_remote_observation(cx);
            assert!(!view.remote_state.refreshing);
            view.busy = false;
        })
    });
    cx.run_until_parked();
    assert_eq!(syncs(), settled);
    // A loaded idle view re-syncs immediately without dispatching a turn.
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.nudge_remote_observation(cx);
            assert!(view.remote_state.refreshing);
        })
    });
    host.wait(&view, cx, true);
    assert!(syncs() > settled);
    assert!(
        !host
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "commands.dispatch")
    );
}

#[gpui::test]
fn explicit_retry_reuses_pending_command_and_preserves_new_composer_and_attachment(
    cx: &mut TestAppContext,
) {
    let host = HostFixture::new(false, false);
    let view = host.conversation(cx);
    host.wait(&view, cx, true);
    {
        let store = host.store.lock().unwrap();
        let mut remote = store.remote_session(host.summary.id).unwrap().unwrap();
        remote.outbox.push(json!({"type": "send", "commandId": "original-accepted-command", "sessionId": "host-session", "text": "Earlier message"}));
        store.save_remote_session(&remote).unwrap();
    }
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.remote_state.pending = true;
            view.attachments = vec![PathBuf::from("/unsent/new-attachment.png")];
            view.retry_remote(cx);
            assert_eq!(view.input.read(cx).content, "Keep this next message");
            assert_eq!(
                view.attachments,
                [PathBuf::from("/unsent/new-attachment.png")]
            );
        })
    });
    host.wait(&view, cx, true);
    let requests = host.requests.lock().unwrap();
    let dispatched: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "commands.dispatch")
        .collect();
    assert_eq!(dispatched.len(), 1);
    assert_eq!(
        dispatched[0]["params"]["commandId"],
        "original-accepted-command"
    );
    assert_eq!(dispatched[0]["params"]["text"], "Earlier message");
    assert!(!requests.iter().any(|r| r["method"] == "attachments.upload"));
    drop(requests);
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.input.read(cx).content, "Keep this next message");
        assert_eq!(view.attachments.len(), 1);
    });
}

#[gpui::test]
fn rejected_message_restore_preserves_new_draft_then_restores_original_text_and_files(
    cx: &mut TestAppContext,
) {
    let view = conversation(cx);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
        view.remote_state.rejected = Some(json!({
            "error": "Host rejected request: permission denied",
            "commands": [
                {"type": "configure", "commandId": "configure-original"},
                {"type": "send", "commandId": "send-original", "text": "Original rejected message"}
            ],
            "uploads": [{"commandId": "send-original", "path": "/original/rejected.png"}]
        }));
        view.attachments = vec![PathBuf::from("/new/unsent.png")];
        view.restore_remote_message(cx);
        assert_eq!(view.input.read(cx).content, "Keep this next message");
        assert_eq!(view.attachments, [PathBuf::from("/new/unsent.png")]);
        assert!(view.status.contains("Clear the composer"));

        view.input.update(cx, |input, cx| input.set_text(String::new(), cx));
        view.attachments.clear();
        view.restore_remote_message(cx);
        assert_eq!(view.input.read(cx).content, "Original rejected message");
        assert_eq!(view.attachments, [PathBuf::from("/original/rejected.png")]);
    })
    });
}

#[gpui::test]
fn ambiguous_legacy_requests_block_send_retry_and_restore_without_consuming_draft(
    cx: &mut TestAppContext,
) {
    let view = conversation(cx);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.remote_state.rejected = Some(json!({
                "legacyRecovery": true,
                "error": "Open the original desktop to recover older pending requests.",
                "commands": [{"type": "send", "text": "Ambiguous original message"}]
            }));
            view.remote_state.pending = true;
            view.send(cx);
            view.retry_remote(cx);
            view.restore_remote_message(cx);
            assert_eq!(
                view.status,
                "Open the original desktop to recover older pending requests."
            );
            assert_eq!(view.input.read(cx).content, "Keep this next message");
            assert!(!view.busy);
            assert!(view.settings.remote_action.is_none());
            assert!(view.transcript.read(cx).source_rows().is_empty());
        })
    });
}

#[gpui::test]
fn sending_while_remote_turn_runs_shows_a_not_sent_notice_without_a_retry_command(
    cx: &mut TestAppContext,
) {
    for (busy, stopping) in [(true, false), (false, true), (true, true)] {
        let view = conversation(cx);
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, _| view.clone())
                .unwrap()
        });
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        window
            .update(visual, |view, _, cx| {
                view.busy = busy;
                view.stopping = stopping;
                view.attachments = vec![PathBuf::from("/home/me/design.png")];
                view.send(cx);

                assert_eq!(view.input.read(cx).content, "Keep this next message");
                assert_eq!(view.attachments, [PathBuf::from("/home/me/design.png")]);
                assert!(view.follow_ups.state.messages.is_empty());
                assert!(view.settings.remote_action.is_none());
                assert!(!view.cancel.load(Ordering::Relaxed));
                assert_eq!((view.busy, view.stopping), (busy, stopping));
                assert!(view.transcript.read(cx).source_rows().is_empty());
                assert!(!view.remote_state.pending);
                assert!(view.remote_state.error.is_none());
            })
            .unwrap();
        visual.refresh().unwrap();
        assert!(
            visual.debug_bounds("remote-send-notice").is_some(),
            "busy={busy} stopping={stopping}: the rejected send must stay visible while the turn runs"
        );
        assert!(
            visual.debug_bounds("remote-retry").is_none(),
            "busy={busy} stopping={stopping}: a busy rejection is not a retryable remote command"
        );
    }
}

#[gpui::test]
fn only_an_unbound_paseo_draft_offers_workspace_selection_without_local_git(cx: &mut TestAppContext) {
    for bound in [false, true] {
        let store = monocode_core::store::Store::open(Path::new(":memory:")).unwrap();
        let project = store.remember_remote_project("remote-ui-tests", &json!({"id":"paseo:/repo","cwd":"/repo"})).unwrap();
        let summary = store.create("codex", &project.key).unwrap();
        if bound {
            let mut binding = store.remote_session(summary.id).unwrap().unwrap();
            binding.session_id = Some("already-created-empty-agent".into());
            store.save_remote_session(&binding).unwrap();
        }
        let window = cx.open_window(Default::default(), |_, cx| {
            let mut view = Conversation::new(summary, Arc::new(Mutex::new(store)), cx);
            view.loaded = true;
            view.remote_state.online = true;
            view.remote_state.capabilities = ["paseo.configure", "paseo.workspaces"].into_iter().map(str::to_owned).collect();
            view
        });
        window.update(cx, |view, window, cx| {
            assert!(view.git.is_none());
            assert!(!view.summary.has_messages);
            view.remote_state.workspace_choices = vec![json!({
                "id":"chosen-workspace","title":"Existing task","cwd":"/repo"
            })];
            view.toggle_menu(ComposerMenu::Workspace, window, cx);
            if bound {
                assert!(view.menu.is_none(), "an existing empty agent must not offer reparenting");
                view.menu_choice("workspace:paseo:existing:chosen-workspace", cx);
                assert!(view.settings.remote_workspace_id.is_none());
            } else {
                let mut events = Vec::new();
                let (_, items, _, _) = view.menu_items(ComposerMenu::Workspace, &mut events, window, cx);
                let actions: Vec<_> = items.iter().filter_map(|item| match item {
                    MenuItem::Action(action) => Some(action.id.as_ref()),
                    _ => None,
                }).collect();
                assert!(actions.contains(&"workspace:paseo:new"));
                assert!(!actions.contains(&"workspace:new"));
                assert!(!actions.contains(&"workspace:existing"));
                assert!(actions.contains(&"workspace:paseo:existing:chosen-workspace"));
                view.menu_choice("workspace:paseo:existing:chosen-workspace", cx);
                assert_eq!(view.settings.remote_workspace_id.as_deref(), Some("chosen-workspace"));
                view.menu_choice("workspace:paseo:existing:missing", cx);
                assert_eq!(view.settings.remote_workspace_id.as_deref(), Some("chosen-workspace"));
                view.menu_choice("workspace:paseo:new", cx);
                assert!(view.settings.remote_workspace_id.is_none());
                assert!(view.settings.remote_action.is_none());
                assert!(view.follow_ups.state.messages.is_empty());
                assert!(view.transcript.read(cx).source_rows().is_empty());
            }
        }).unwrap();
    }
}

#[gpui::test]
fn paseo_new_draft_loads_workspace_choices_over_real_transport_before_enabling_send(
    cx: &mut TestAppContext,
) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let host = crate::runtime::remote::TestHost::with_capabilities(
        vec!["paseo.configure", "paseo.workspaces"],
        move |request| {
            recorded.lock().unwrap().push(request.clone());
            assert_eq!(request["method"], "paseo.workspaces");
            assert_eq!(request["params"]["projectId"], "paseo:/repo");
            Some(json!([{"id":"chosen-workspace","title":"Existing task","cwd":"/repo"}]))
        },
    );
    let store = monocode_core::store::Store::open(Path::new(":memory:")).unwrap();
    let project = store.remember_remote_project(
        "env", &json!({"id":"paseo:/repo","cwd":"/repo"}),
    ).unwrap();
    let summary = store.create("codex", &project.key).unwrap();
    let store = Arc::new(Mutex::new(store));
    let window = cx.open_window(Default::default(), |_, cx| {
        let mut view = Conversation::new(summary, store.clone(), cx);
        view.remote_state.client = Some(host.client.clone());
        view.seed_new_settings(Settings {
            model: Some("codex:fixture-model".into()), ..Default::default()
        }, cx);
        view.input.update(cx, |input, cx| input.set_text("Test; reply 111".into(), cx));
        view
    });
    window.update(cx, |view, _, cx| view.load(cx)).unwrap();
    cx.run_until_parked();
    window.update(cx, |view, window, cx| {
        assert!(view.loaded, "actual draft startup failed: {}", view.status);
        assert!(view.remote_state.online);
        assert!(view.remote_state.error.is_none());
        assert!(view.paseo_workspace_draft());
        assert_eq!(view.remote_state.workspace_choices.len(), 1);
        assert_eq!(view.input.read(cx).content, "Test; reply 111");
        assert!(!view.remote_send_guard(cx), "{}", view.status);
        assert!(!view.attaching && !view.busy && !view.stopping);
        view.toggle_menu(ComposerMenu::Workspace, window, cx);
        assert!(view.menu.is_some());
        let binding = locked(&store).unwrap().remote_session(view.summary.id).unwrap().unwrap();
        assert!(binding.session_id.is_none());
        assert!(binding.outbox.is_empty(), "loading must not create or send");
    }).unwrap();
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    assert!(visual.debug_bounds("paseo-workspace-selector").is_some());
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[gpui::test]
fn ordinary_remote_hosts_keep_the_existing_worktree_picker(cx: &mut TestAppContext) {
    for (remote, stopping) in [(true, false), (true, true), (false, true)] {
        let view = conversation(cx);
        cx.update(|cx| view.update(cx, |view, _| {
            view.remote_state.capabilities = ["git.worktrees"].into_iter().map(str::to_owned).collect();
            view.git = Some(monocode_core::git::Snapshot::default());
            view.summary.has_messages = false;
            view.stopping = stopping;
            if !remote {
                view.summary.cwd = "/test-project".into();
            }
            assert_eq!(view.is_remote(), remote);
            assert!(!view.paseo_controls());
        }));
        let window = cx.update(|cx| cx.open_window(Default::default(), |_, _| view.clone())).unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let point = window.update(visual, |view, _, _| view.workspace_anchor.get().unwrap().center()).unwrap();
        visual.simulate_click(point, Default::default());
        visual.run_until_parked();
        window.update(visual, |view, _, cx| {
            assert!(view.menu_is(ComposerMenu::Workspace),
                "remote={remote} stopping={stopping}: ordinary workspace control was blocked: {}", view.status);
            view.menu_choice("workspace:new", cx);
            assert!(view.settings.new_worktree);
            assert!(view.settings.remote_workspace_id.is_none());
        }).unwrap();
    }
}

#[gpui::test]
fn paseo_busy_send_uses_the_native_queue_and_steer_controls(cx: &mut TestAppContext) {
    for prefer_steer in [false, true] {
        let view = conversation(cx);
        let (controls, receiver) = std::sync::mpsc::channel();
        cx.update(|cx| {
            cx.set_global(crate::settings::Preferences { follow_up: if prefer_steer {"steer"} else {"queue"}.into(), ..Default::default() });
            view.update(cx, |view, cx| {
                view.remote_state.capabilities.extend(["paseo.configure".into(), "paseo.steer".into()]);
                view.busy = true;
                view.follow_ups.ready = true;
                view.controls = Some(controls);
                view.send(cx);
                assert!(view.input.read(cx).content.is_empty());
                assert_eq!(view.follow_ups.state.messages.len(), 1);
                assert_eq!(view.follow_ups.state.messages[0].text, "Keep this next message");
                assert_eq!(view.follow_ups.state.messages[0].steer, prefer_steer);
                assert!(!view.remote_state.send_blocked_notice);
                assert!(view.remote_state.rejected.is_none());
                assert!(view.transcript.read(cx).source_rows().is_empty());
            });
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.pump_follow_ups(cx);
            if prefer_steer {
                let c = receiver.try_recv().expect("native steer control");
                assert!(matches!(c.decision, Decision::Steer(_)));
                view.follow_up_delivered(&c.id, cx);
                assert!(view.follow_ups.state.messages.is_empty());
            } else {
                assert!(receiver.try_recv().is_err());
                assert!(view.follow_ups.sending.is_none());
            }
        });
    }
}


/// Synthetic 1000-row Paseo window through the real Conversation/Transcript.
/// Timings are diagnostic, not thresholds tied to this machine's load.
#[gpui::test]
fn paseo_switch_install_timing(cx: &mut TestAppContext) {
    use std::time::Instant;
    let view = conversation(cx);
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, _| view.clone()).unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    let mut samples = Vec::new();
    for iteration in 0..3 {
        let rows = switch_fixture_rows(iteration);
        let bytes: usize = rows.iter().map(|row| row.text.len()).sum();
        let begin = Instant::now();
        let prepared = crate::transcript::PreparedHistory::new(rows, false);
        let prepare_ms = begin.elapsed().as_secs_f64()*1000.;
        let begin = Instant::now();
        window.update(visual, |view, _, cx| {
            view.summary.has_messages = true;
            view.transcript.update(cx, |transcript, cx| {
                transcript.paseo_mode();
                transcript.install_prepared_history(prepared, cx);
                assert_eq!(transcript.source_rows().len(),1000);
                assert!(transcript.source_rows()[999].text.contains(&format!("{iteration} / 999")));
            });
        }).unwrap();
        let install_ms = begin.elapsed().as_secs_f64()*1000.;
        let begin = Instant::now();
        visual.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        }).unwrap();
        let layout_ms = begin.elapsed().as_secs_f64()*1000.;
        samples.push(json!({"iteration":iteration,"rows":1000,"bytes":bytes,"prepareMs":prepare_ms,"installMs":install_ms,"testLayoutMs":layout_ms}));
    }
    println!("paseo-switch-install {}",json!({"synthetic":true,"gpu":false,"samples":samples}));
}

fn switch_fixture_rows(iteration: usize) -> Vec<monocode_core::transcript::Row> {
    (0..1000).map(|i| {
            let role = match i % 10 { 0 => "user", 1 => "reasoning", 2..=7 => "tool", _ => "assistant" };
            let text = if role == "tool" {
                let mut tool = monocode_core::tool::ToolCall::new(&format!("tool-{i}"), "read");
                tool.input = json!({"filePath":"src/example.rs"}).to_string();
                tool.output = "Synthetic output\n".repeat(24);
                tool.state = monocode_core::tool::ToolState::Completed;
                serde_json::to_string(&tool).unwrap()
            } else { format!("## Synthetic {iteration} / {i}\n\n{}\n```rust\nfn example() {{}}\n```", "Native Markdown **content**. ".repeat(12)) };
            monocode_core::transcript::Row {role:role.into(),text,first:true,sent_at:None}
        }).collect()
}

#[gpui::test]
fn paseo_history_installs_before_view_acknowledgement(cx: &mut TestAppContext) {
    let barrier = Arc::new(AtomicBool::new(false));
    let host = HostFixture::with_view_barrier(false, true, Some(barrier.clone()));
    let view = host.conversation(cx);
    host.wait(&view, cx, true);
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.loaded && !view.loading && view.remote_state.online);
        assert_eq!(view.follow_ups.state.draft.as_ref().unwrap().text, "Saved host draft");
    });
    for _ in 0..1000 {
        cx.run_until_parked();
        if host.requests.lock().unwrap().iter().any(|r| r["params"]["type"] == "view") { break; }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(!barrier.load(Ordering::Relaxed));
    assert_eq!(host.requests.lock().unwrap().iter().filter(|r| r["params"]["type"] == "view").count(), 1);
    barrier.store(true, Ordering::Relaxed);
}

#[gpui::test]
fn marking_a_loaded_paseo_view_acknowledges_it_once(cx: &mut TestAppContext) {
    // The view barrier fixture advertises Paseo; a released barrier never blocks.
    let host = HostFixture::with_view_barrier(false, false, Some(Arc::new(AtomicBool::new(true))));
    let views = || {
        host.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["params"]["type"] == "view")
            .count()
    };
    // Unloaded views never acknowledge.
    let unloaded = cx.update(|cx| {
        cx.new(|cx| {
            let mut view = Conversation::new(host.summary.clone(), host.store.clone(), cx);
            view.remote_state.client = Some(host.client.clone());
            view.remote_state.capabilities.insert("paseo.configure".into());
            view
        })
    });
    assert!(!cx.update(|cx| unloaded.update(cx, |view, cx| view.mark_remote_viewed(cx))));
    cx.run_until_parked();
    assert_eq!(views(), 0);

    let view = host.conversation(cx);
    host.wait(&view, cx, true);
    // The initial open acknowledged once; wait for that request to land.
    for _ in 0..1000 {
        cx.run_until_parked();
        if views() == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(views(), 1);
    // A repeat right after the initial acknowledgement is throttled.
    assert!(!cx.update(|cx| view.update(cx, |view, cx| view.mark_remote_viewed(cx))));
    cx.update(|cx| view.update(cx, |view, _| view.remote_state.viewed_at = None));
    // One request in flight at a time.
    let (first, second) = cx.update(|cx| {
        view.update(cx, |view, cx| (view.mark_remote_viewed(cx), view.mark_remote_viewed(cx)))
    });
    assert!(first && !second);
    for _ in 0..1000 {
        cx.run_until_parked();
        if views() == 2 && !cx.update(|cx| view.read(cx).remote_state.viewing) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(views(), 2);
    assert!(!cx.update(|cx| view.read(cx).remote_state.viewing));
    assert!(
        !host
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["params"]["type"] == "send")
    );
}

#[test]
#[cfg(target_os = "macos")]
#[ignore = "Requires Metal; same real Conversation, synthetic history, no daemon or OS window"]
fn render_paseo_switch_timing_fixture() {
    use std::time::Instant;
    let mut cx = gpui::HeadlessAppContext::with_platform(
        Arc::new(gpui_apple::AppleTextSystem::new()),
        Arc::new(crate::assets::Assets),
        || Ok(Some(Box::new(gpui_apple::metal_renderer::MetalHeadlessRenderer::new()))),
    );
    let window = cx.open_window(gpui::size(px(1180.), px(800.)), |_, cx| {
        let store = monocode_core::store::Store::open(Path::new(":memory:")).unwrap();
        let mut summary = store.create("codex", "/synthetic").unwrap();
        summary.cwd = "remote://fixture/synthetic".into();
        summary.has_messages = true;
        cx.new(|cx| {
            let mut view = Conversation::new(summary, Arc::new(Mutex::new(store)), cx);
            view.loaded = true;
            view
        })
    }).unwrap();
    let mut samples = Vec::new();
    for i in 0..3 {
        let begin = Instant::now();
        let prepared = crate::transcript::PreparedHistory::new(switch_fixture_rows(i), false);
        let prepare_ms = begin.elapsed().as_secs_f64()*1000.;
        let begin = Instant::now();
        cx.update_window(window.into(), |view, _, cx| {
            view.downcast::<Conversation>().unwrap().update(cx, |view, cx| {
                view.transcript.update(cx, |v, cx| { v.paseo_mode(); v.install_prepared_history(prepared, cx); });
            });
        }).unwrap();
        let install_ms = begin.elapsed().as_secs_f64()*1000.;
        let begin = Instant::now();
        cx.update_window(window.into(), |_, window, cx| {window.refresh();window.draw(cx).clear(cx);}).unwrap();
        cx.run_until_parked();
        let draw_ms = begin.elapsed().as_secs_f64()*1000.;
        samples.push(json!({"iteration":i,"prepareMs":prepare_ms,"installMs":install_ms,"metalDrawMs":draw_ms}));
    }
    if let Some(directory) = std::env::var_os("PASEO_SWITCH_RENDER_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        cx.capture_screenshot(window.into()).unwrap().save(PathBuf::from(directory).join("r14-switch.png")).unwrap();
    }
    println!("paseo-switch-metal {}", json!({"synthetic":true,"rows":1000,"samples":samples}));
}

#[gpui::test]
fn paseo_cold_open_shows_loading_until_first_snapshot_lands(cx: &mut TestAppContext) {
    let barrier = Arc::new(AtomicBool::new(false));
    let host = HostFixture::with_sync_barrier(barrier.clone());
    cx.update(|cx| {
        let mut preferences = crate::settings::Preferences::default();
        preferences
            .extra
            .insert(crate::paseo_host::PROJECT_SELECTION.into(), json!({}));
        cx.set_global(preferences);
    });
    // The sidebar index lists a never-loaded conversation as having content.
    let mut summary = host.summary.clone();
    summary.has_messages = true;
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| {
                let mut view = Conversation::new(summary, host.store.clone(), cx);
                view.remote_state.client = Some(host.client.clone());
                view.load_remote(cx);
                view
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.refresh().unwrap();
    window
        .update(visual, |view, _, _| assert!(view.loading && !view.loaded))
        .unwrap();
    assert!(visual.debug_bounds("paseo-cold-open-loading").is_some());
    barrier.store(true, Ordering::Relaxed);
    for _ in 0..1000 {
        visual.run_until_parked();
        if window.update(visual, |view, _, _| view.loaded).unwrap() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    visual.refresh().unwrap();
    window
        .update(visual, |view, _, _| assert!(view.loaded && !view.loading))
        .unwrap();
    assert!(visual.debug_bounds("paseo-cold-open-loading").is_none());
}

#[gpui::test]
fn cold_open_loading_line_is_paseo_only_and_never_hides_an_error(cx: &mut TestAppContext) {
    let store = monocode_core::store::Store::open(Path::new(":memory:")).unwrap();
    let mut summary = store.create("codex", "/test-project").unwrap();
    summary.cwd = "remote://remote-ui-tests/home/me/project".into();
    summary.has_messages = true;
    cx.update(|cx| cx.set_global(crate::settings::Preferences::default()));
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| {
                let mut view = Conversation::new(summary, Arc::new(Mutex::new(store)), cx);
                view.loading = true;
                view
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.refresh().unwrap();
    // An ordinary remote host (no Paseo prototype) renders as before.
    assert!(visual.debug_bounds("paseo-cold-open-loading").is_none());
    visual.update(|_, cx| {
        let mut preferences = crate::settings::Preferences::default();
        preferences
            .extra
            .insert(crate::paseo_host::PROJECT_SELECTION.into(), json!({}));
        cx.set_global(preferences);
    });
    window.update(visual, |_, _, cx| cx.notify()).unwrap();
    visual.refresh().unwrap();
    assert!(visual.debug_bounds("paseo-cold-open-loading").is_some());
    // A failed load keeps the error path, not a perpetual loading line.
    window
        .update(visual, |view, _, cx| {
            view.remote_state.error = Some("offline".into());
            cx.notify();
        })
        .unwrap();
    visual.refresh().unwrap();
    assert!(visual.debug_bounds("paseo-cold-open-loading").is_none());
}
