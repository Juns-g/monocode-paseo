//! Isolated Paseo client. Never opens Monocode's Store or starts a provider.
mod menus;
#[cfg(test)]
mod replay;
mod sidebar;
use crate::{input, native_shell, theme::*, transcript::TranscriptView};
use gpui::{App, Context, Entity, Focusable, Subscription, Window, div, prelude::*, px};
use monocode_core::tool::{ToolCall, ToolState};
use monocode_core::transcript::Transcript;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
};

pub(crate) fn is_bundle() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent()?.parent().map(|p| p.join("Info.plist")))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .is_some_and(|p| p.contains("<key>PaseoPrototype</key><true/>"))
}
fn resources() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("PASEO_PROTOTYPE_RESOURCES") {
        return Ok(path.into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Ok(exe
        .parent()
        .and_then(|p| p.parent())
        .ok_or("Missing App resources")?
        .join("Resources"))
}
struct Bridge {
    child: Child,
    sender: mpsc::Sender<Value>,
}
impl Bridge {
    fn start() -> Result<(Self, async_channel::Receiver<Value>), String> {
        let resource = resources()?;
        let node = std::fs::read_to_string(resource.join("node-path.txt"))
            .map_err(|e| format!("Node runtime: {e}"))?;
        let mut child = Command::new(node.trim())
            .arg(resource.join("paseo-bridge.mjs"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Bridge: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("Bridge stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("Bridge stdout unavailable")?;
        let (sender, commands) = mpsc::channel::<Value>();
        let (events, receiver) = async_channel::bounded(16);
        std::thread::spawn(move || {
            while let Ok(command) = commands.recv() {
                if writeln!(stdin, "{command}")
                    .and_then(|_| stdin.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str(&line)
                    && events.send_blocking(value).is_err()
                {
                    return;
                }
            }
            let _=events.send_blocking(json!({"event":"error","fatal":true,"error":"Paseo bridge exited. Reopen this prototype; remote agents were not stopped."}));
        });
        Ok((Self { child, sender }, receiver))
    }
    fn send(&self, command: Value) {
        let _ = self.sender.send(command);
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn project_tool(entry: &Value) -> ToolCall {
    let item = &entry["item"];
    let detail = &item["detail"];
    let mut call = ToolCall::new(
        item["callId"].as_str().unwrap_or(""),
        item["name"].as_str().unwrap_or("tool"),
    );
    call.state = match item["status"].as_str() {
        Some("completed") => ToolState::Completed,
        Some("failed") => ToolState::Failed,
        Some("canceled" | "cancelled") => ToolState::Cancelled,
        _ => ToolState::Running,
    };
    let mut input = serde_json::Map::new();
    // Keep presentation arguments separate from potentially enormous results.
    for key in [
        "filePath",
        "path",
        "command",
        "cwd",
        "offset",
        "limit",
        "query",
        "url",
        "description",
    ] {
        if let Some(value) = detail.get(key) {
            input.insert(key.into(), value.clone());
        }
    }
    call.input = Value::Object(input).to_string();
    call.display = item.get("metadata").filter(|m| m.is_object()).cloned();
    let result = match detail["type"].as_str() {
        Some("shell") => &detail["output"],
        Some("read" | "write" | "search") => &detail["content"],
        Some("edit") => &detail["unifiedDiff"],
        Some("fetch") => &detail["result"],
        Some("sub_agent" | "worktree_setup") => &detail["log"],
        Some("plain_text" | "plan") => &detail["text"],
        _ => &detail["output"],
    };
    call.output = if result.is_null() {
        String::new()
    } else {
        result
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| result.to_string())
    };
    if let Some(error) = item.get("error").filter(|e| !e.is_null()) {
        if !call.output.is_empty() {
            call.output.push('\n');
        }
        call.output.push_str(
            error
                .as_str()
                .or_else(|| error["message"].as_str())
                .unwrap_or(&error.to_string()),
        );
    }
    let end = call
        .output
        .char_indices()
        .nth(32000)
        .map_or(call.output.len(), |(at, _)| at);
    if end < call.output.len() || entry["prototypeTruncated"] == true {
        call.output.truncate(end);
        call.output
            .push_str("\n[内容过长，原型截断；完整内容请在 Paseo 查看]");
    }
    call
}

fn project_rows(snapshot: &Value) -> Vec<monocode_core::transcript::Row> {
    let mut transcript = Transcript::default();
    let entries = snapshot
        .get("displayEntries")
        .unwrap_or(&snapshot["entries"]);
    for entry in entries.as_array().into_iter().flatten() {
        let item = &entry["item"];
        let kind = item["type"].as_str().unwrap_or("unknown");
        let role = match kind {
            "user_message" => "user",
            "reasoning" => "reasoning",
            "tool_call" => "tool",
            _ => "assistant",
        };
        let text = match kind {
            "user_message" | "assistant_message" | "reasoning" => {
                item["text"].as_str().unwrap_or("").to_owned()
            }
            "tool_call" => serde_json::to_string(&project_tool(entry)).unwrap(),
            "plugin" => format!(
                "Paseo 插件面板内容（原型暂不支持）：{}",
                item["pluginId"].as_str().unwrap_or("plugin")
            ),
            "error" | "notification" => item["message"].as_str().unwrap_or("").to_owned(),
            _ => format!(
                "{kind}\n{}",
                serde_json::to_string_pretty(item).unwrap_or_default()
            ),
        };
        let first = transcript.begin(role);
        transcript.rows[first].sent_at = entry["timestamp"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|time| time.timestamp_millis().max(0) as u64);
        if role == "tool" {
            // Native phases decode a complete ToolCall from its first row.
            transcript.rows[first].text = text;
            continue;
        }
        // The canonical source stays on Paseo. Bound rendering of enormous tool outputs.
        let end = text
            .char_indices()
            .nth(32000)
            .map_or(text.len(), |(at, _)| at);
        transcript.append(&text[..end]);
        if end < text.len() || entry["prototypeTruncated"] == true {
            transcript.append("\n[内容过长，原型截断；完整内容请在 Paseo 查看]");
        }
    }
    transcript.rows
}
struct Prototype {
    bridge: Option<Bridge>,
    server: String,
    version: String,
    status: String,
    error: String,
    directory: Vec<Value>,
    next: Option<String>,
    directory_cursor: Option<String>,
    agent: Value,
    snapshot: Value,
    transcript: Entity<TranscriptView>,
    transcripts: HashMap<String, Entity<TranscriptView>>,
    input: Entity<input::Input>,
    search: Entity<input::Input>,
    serial: u64,
    pending: HashMap<u64, String>,
    sending: Option<(u64, String)>,
    uncertain: bool,
    drafts: HashMap<String, String>,
    sidebar_visible: bool,
    info_visible: bool,
    search_visible: bool,
    tabs: Vec<Value>,
    collapsed: std::collections::HashSet<String>,
    focus_pending: bool,
    menu: Option<menus::OpenMenu>,
    renaming: Option<menus::Renaming>,
    _subscriptions: Vec<Subscription>,
}
impl Prototype {
    fn new(cx: &mut Context<Self>) -> Self {
        let input = cx.new(input::Input::new);
        let search = cx.new(|cx| {
            input::Input::multiline_field("搜索项目、工作区或伙伴", "搜索标题 / 路径 / ID", cx)
        });
        let transcript = cx.new(|_| TranscriptView::new());
        let submitted = cx.subscribe(&input, |this, _, _: &input::Submitted, cx| this.send(cx));
        let changed = cx.observe(&input, |_, _, cx| cx.notify());
        let searched = cx.subscribe(&search, |this, _, _: &input::Submitted, cx| {
            this.list(false, cx)
        });
        let result = if cfg!(test) {
            Err("Test bridge disabled".into())
        } else {
            Bridge::start()
        };
        let (bridge, error) = match result {
            Ok((bridge, receiver)) => {
                cx.spawn(async move |this, cx| {
                    while let Ok(value) = receiver.recv().await {
                        if this.update(cx, |this, cx| this.receive(value, cx)).is_err() {
                            break;
                        }
                    }
                })
                .detach();
                (Some(bridge), String::new())
            }
            Err(error) => (None, error),
        };
        Self {
            bridge,
            server: String::new(),
            version: String::new(),
            status: if error.is_empty() {
                "connecting"
            } else {
                "error"
            }
            .into(),
            error,
            directory: vec![],
            next: None,
            directory_cursor: None,
            agent: Value::Null,
            snapshot: Value::Null,
            transcript,
            transcripts: HashMap::new(),
            input,
            search,
            serial: 0,
            pending: HashMap::new(),
            sending: None,
            uncertain: false,
            drafts: HashMap::new(),
            sidebar_visible: true,
            info_visible: false,
            search_visible: false,
            tabs: vec![],
            collapsed: Default::default(),
            focus_pending: true,
            menu: None,
            renaming: None,
            _subscriptions: vec![submitted, changed, searched],
        }
    }
    fn command(&mut self, op: &str, mut command: Value) -> u64 {
        self.serial += 1;
        let id = self.serial;
        command["id"] = json!(id);
        command["op"] = json!(op);
        command["serverId"] = json!(self.server);
        if command.get("agentId").is_none() {
            command["agentId"] = self.agent["id"].clone();
        }
        self.pending.insert(id, op.into());
        if let Some(bridge) = &self.bridge {
            bridge.send(command);
        }
        id
    }
    fn list(&mut self, more: bool, cx: &mut Context<Self>) {
        self.load_directory(if more { self.next.clone() } else { None }, cx);
    }
    fn load_directory(&mut self, cursor: Option<String>, cx: &mut Context<Self>) {
        let query = self.search.read(cx).content.to_string();
        self.directory.clear();
        self.directory_cursor = cursor.clone();
        self.command("list", json!({"query":query,"cursor":cursor}));
        cx.notify();
    }
    fn open(&mut self, id: String, cx: &mut Context<Self>) {
        if self.agent["id"] == id {
            return;
        }
        if self.sending.is_some() {
            self.error = "发送结果尚未确认，请稍候再切换伙伴。".into();
            cx.notify();
            return;
        }
        self.renaming = None;
        self.menu = None;
        if let Some(old) = self.agent["id"].as_str() {
            self.drafts
                .insert(old.into(), self.input.read(cx).content.to_string());
        }
        let draft = self.drafts.get(&id).cloned().unwrap_or_default();
        self.input.update(cx, |input, cx| input.set_text(draft, cx));
        let agent = self
            .directory
            .iter()
            .find(|e| e["agent"]["id"] == id)
            .map(|e| e["agent"].clone())
            .or_else(|| self.tabs.iter().find(|a| a["id"] == id).cloned())
            .unwrap_or_else(|| json!({"id":id}));
        if !self.tabs.iter().any(|a| a["id"] == id) {
            self.tabs.push(agent.clone());
        }
        self.agent = agent;
        self.agent["status"] = json!("loading");
        self.focus_pending = true;
        self.snapshot = Value::Null;
        self.transcript
            .update(cx, |view, cx| view.visible(false, cx));
        self.transcript = self
            .transcripts
            .entry(id.clone())
            .or_insert_with(|| Self::new_transcript(&self.agent, cx))
            .clone();
        self.transcript
            .update(cx, |view, cx| view.visible(true, cx));
        self.error.clear();
        self.command("open", json!({"agentId":id}));
        cx.notify();
    }
    fn new_transcript(agent: &Value, cx: &mut Context<Self>) -> Entity<TranscriptView> {
        cx.new(|_| {
            let mut view = TranscriptView::new();
            view.paseo_mode();
            view.identity(
                agent["provider"].as_str().unwrap_or(""),
                agent["model"]
                    .as_str()
                    .unwrap_or("")
                    .rsplit(':')
                    .next()
                    .unwrap_or(""),
            );
            view
        })
    }
    fn busy(&self) -> bool {
        !matches!(
            self.agent["status"].as_str(),
            Some("idle" | "closed" | "error")
        ) || self.agent["providerUnavailable"] == true
            || !self.agent["activeTurn"].is_null()
            || self.agent["pendingPermissions"]
                .as_array()
                .is_some_and(|p| !p.is_empty())
    }
    fn send(&mut self, cx: &mut Context<Self>) {
        if self.status != "connected"
            || self.busy()
            || self.sending.is_some()
            || self.uncertain
            || self.input.read(cx).is_composing()
        {
            return;
        }
        let text = self.input.read(cx).content.to_string();
        if text.trim().is_empty() {
            return;
        }
        let id = self.command(
            "send",
            json!({"text":text,"messageId":monocode_core::mono::new_id(),"acknowledgeRace":true}),
        );
        self.sending = Some((id, text));
        cx.notify();
    }
    fn receive(&mut self, value: Value, cx: &mut Context<Self>) {
        if let Some(event) = value["event"].as_str() {
            match event {
                "connection" => {
                    self.status = value["status"].as_str().unwrap_or("disconnected").into();
                    if let Some(server) = value["serverId"].as_str() {
                        if !self.server.is_empty() && self.server != server {
                            self.agent = Value::Null;
                            self.tabs.clear();
                            self.drafts.clear();
                            self.transcripts.clear();
                            self.menu = None;
                            self.renaming = None;
                            self.transcript = cx.new(|_| TranscriptView::new());
                            self.snapshot = Value::Null;
                            self.error =
                                "Daemon identity changed; select the intended agent again.".into();
                        }
                        self.server = server.into();
                        self.version = value["version"].as_str().unwrap_or("unknown").into();
                        if self.status == "connected" {
                            self.list(false, cx);
                        }
                    }
                }
                "snapshot" => {
                    if value["serverId"] == self.server && value["agentId"] == self.agent["id"] {
                        self.agent = value["agent"].clone();
                        let working = self.agent["status"] == "running"
                            || !self.agent["activeTurn"].is_null();
                        if working {
                            self.transcript
                                .update(cx, |view, cx| view.streaming(true, cx));
                        }
                        if let Some(entry) = self
                            .directory
                            .iter_mut()
                            .find(|e| e["agent"]["id"] == self.agent["id"])
                        {
                            entry["agent"] = self.agent.clone();
                        }
                        if let Some(tab) =
                            self.tabs.iter_mut().find(|a| a["id"] == self.agent["id"])
                        {
                            *tab = self.agent.clone();
                        }
                        if self.snapshot["entries"] != value["entries"] {
                            let rows = project_rows(&value);
                            // Native patch appends/replaces; it does not truncate a shorter tail.
                            if rows.len() < self.transcript.read(cx).source_rows().len() {
                                self.transcript = Self::new_transcript(&self.agent, cx);
                                self.transcripts.insert(
                                    self.agent["id"].as_str().unwrap_or("").into(),
                                    self.transcript.clone(),
                                );
                                self.transcript
                                    .update(cx, |view, cx| view.streaming(working, cx));
                            }
                            let start = self
                                .transcript
                                .read(cx)
                                .source_rows()
                                .iter()
                                .zip(&rows)
                                .take_while(|(a, b)| a == b)
                                .count();
                            self.transcript.update(cx, |view, cx| {
                                view.patch(start, rows[start..].to_vec(), cx)
                            });
                        }
                        if !working {
                            self.transcript
                                .update(cx, |view, cx| view.streaming(false, cx));
                        }
                        let rows = self.transcript.read(cx).source_rows();
                        let mut prompt: Option<(usize, u64)> = None;
                        let mut last: Option<u64> = None;
                        let mut durations = Vec::new();
                        for entry in value["entries"].as_array().into_iter().flatten() {
                            let timestamp = entry["timestamp"]
                                .as_str()
                                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                                .map(|t| t.timestamp_millis().max(0) as u64);
                            if entry["item"]["type"] == "user_message" {
                                if let (Some((turn, start)), Some(end)) = (prompt, last) {
                                    durations.push((turn, end.saturating_sub(start)));
                                }
                                prompt = timestamp.and_then(|start| {
                                    let at = rows.iter().position(|row| {
                                        row.first && row.is_prompt() && row.sent_at == Some(start)
                                    })?;
                                    let turn = (at + 1..rows.len())
                                        .find(|&i| !rows[i].is_prompt())
                                        .unwrap_or(rows.len());
                                    Some((turn, start))
                                });
                            }
                            last = timestamp;
                        }
                        if !working && let (Some((turn, start)), Some(end)) = (prompt, last) {
                            durations.push((turn, end.saturating_sub(start)));
                        }
                        self.transcript.update(cx, |view, _| {
                            for (turn, ms) in durations {
                                view.remote_duration(turn, ms);
                            }
                        });
                        self.snapshot = value;
                    }
                }
                "error" => {
                    self.error = value["error"].as_str().unwrap_or("Bridge error").into();
                    if value["fatal"] == true {
                        self.status = "error".into();
                        if self.sending.take().is_some() {
                            self.uncertain = true;
                        }
                    }
                }
                _ => {}
            }
        } else if let Some(id) = value["id"].as_u64() {
            let operation = self.pending.remove(&id).unwrap_or_default();
            if value["ok"] == true {
                if operation == "open"
                    && value["value"]["agentId"] == self.agent["id"]
                    && self.agent["requiresAttention"] == true
                    && self.agent["attentionReason"] != "permission"
                    && !self.agent["pendingPermissions"]
                        .as_array()
                        .is_some_and(|p| !p.is_empty())
                {
                    self.command("clearAttention", json!({}));
                }
                if operation == "list" {
                    if let Some(entries) = value["value"]["entries"].as_array() {
                        self.directory.extend(entries.clone());
                    }
                    self.next = value["value"]["pageInfo"]["nextCursor"]
                        .as_str()
                        .map(str::to_owned);
                }
                if matches!(
                    operation.as_str(),
                    "renameWorkspace" | "pinWorkspace" | "archiveWorkspace"
                ) && value["value"]["serverId"] == self.server
                {
                    if operation == "archiveWorkspace" {
                        let workspace = &value["value"]["workspaceId"];
                        let ids = self
                            .tabs
                            .iter()
                            .filter(|a| a["workspaceId"] == *workspace)
                            .filter_map(|a| a["id"].as_str().map(str::to_owned))
                            .collect::<Vec<_>>();
                        for id in ids {
                            self.close_tab(&id, cx);
                        }
                        self.directory
                            .retain(|e| e["agent"]["workspaceId"] != *workspace);
                    }
                    self.load_directory(self.directory_cursor.clone(), cx);
                }
                if self
                    .sending
                    .as_ref()
                    .is_some_and(|(pending, _)| *pending == id)
                    && let Some((_, text)) = self.sending.take()
                    && self.input.read(cx).content.as_ref() == text
                {
                    self.input
                        .update(cx, |input, cx| input.set_text(String::new(), cx));
                }
            } else {
                self.error = value["error"].as_str().unwrap_or("Request failed").into();
                if self
                    .sending
                    .as_ref()
                    .is_some_and(|(pending, _)| *pending == id)
                {
                    self.sending = None;
                    self.uncertain =
                        value["uncertain"] == true || self.error.contains("outcome unknown");
                }
            }
        }
        cx.notify();
    }
}
gpui::actions!(paseo_native, [FocusComposer]);
fn bind_keys(cx: &mut App) {
    menus::bind_keys(cx);
    use crate::shell::*;
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let context = Some("Workspace");
    cx.bind_keys([
        gpui::KeyBinding::new(&format!("{modifier}-b"), ToggleSidebar, context),
        gpui::KeyBinding::new(&format!("{modifier}-k"), FocusSearch, context),
        gpui::KeyBinding::new(&format!("{modifier}-l"), FocusComposer, context),
        gpui::KeyBinding::new(&format!("{modifier}-w"), CloseTab, context),
        gpui::KeyBinding::new(&format!("{modifier}-shift-w"), CloseAllTabs, context),
        gpui::KeyBinding::new("ctrl-tab", NextTab, context),
        gpui::KeyBinding::new("ctrl-shift-tab", PreviousTab, context),
        gpui::KeyBinding::new(&format!("{modifier}-shift-]"), NextTab, context),
        gpui::KeyBinding::new(&format!("{modifier}-shift-["), PreviousTab, context),
        gpui::KeyBinding::new("escape", DismissMenu, context),
    ]);
    for n in 1..=9 {
        cx.bind_keys([gpui::KeyBinding::new(
            &format!("{modifier}-{n}"),
            ActivateTab(if n == 9 { -1 } else { n - 1 }),
            context,
        )]);
    }
}
impl Prototype {
    fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.input.focus_handle(cx), cx);
        self.focus_pending = false;
    }
    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        if !self.sidebar_visible {
            self.focus_composer(window, cx);
        }
        cx.notify();
    }
    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = true;
        self.search_visible = true;
        window.focus(&self.search.focus_handle(cx), cx);
        cx.notify();
    }
    fn step_tab(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let at = self
            .tabs
            .iter()
            .position(|a| a["id"] == self.agent["id"])
            .unwrap_or(0);
        let next = (at as isize + step).rem_euclid(self.tabs.len() as isize) as usize;
        self.activate_tab(next as isize, cx);
    }
    fn activate_tab(&mut self, slot: isize, cx: &mut Context<Self>) {
        let index = if slot < 0 {
            self.tabs.len().saturating_sub(1)
        } else {
            slot as usize
        };
        if let Some(id) = self
            .tabs
            .get(index)
            .and_then(|a| a["id"].as_str())
            .map(str::to_owned)
        {
            self.open(id, cx);
        }
    }
    fn close_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.sending.is_some() {
            return;
        }
        let active = self.agent["id"] == id;
        if let Some(at) = self.tabs.iter().position(|a| a["id"] == id) {
            self.tabs.remove(at);
            self.transcripts.remove(id);
            if active {
                if self.tabs.is_empty() {
                    self.drafts
                        .insert(id.into(), self.input.read(cx).content.to_string());
                    self.agent = Value::Null;
                    self.snapshot = Value::Null;
                    self.transcript = cx.new(|_| TranscriptView::new());
                    self.input
                        .update(cx, |input, cx| input.set_text(String::new(), cx));
                } else {
                    self.activate_tab(at.min(self.tabs.len() - 1) as isize, cx);
                }
            }
        }
        cx.notify();
    }
    fn titlebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let lead = if cfg!(target_os = "macos") && !window.is_fullscreen() {
            78.
        } else {
            10.
        };
        let navigation = div()
            .w(px(if self.sidebar_visible {
                260.
            } else {
                lead + 48.
            }))
            .h_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .child(
                crate::shell::titlebar_drag_area()
                    .w(px(lead))
                    .flex_shrink_0(),
            )
            .child(
                icon_button("toggle-sidebar", "Toggle sidebar (⌘B)", "sidebar")
                    .debug_selector(|| "toggle-sidebar".into())
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx))),
            )
            .child(crate::shell::titlebar_drag_area().flex_1())
            .child(
                icon_button("paseo-search", "Search conversations (⌘K)", "search")
                    .on_click(cx.listener(|this, _, window, cx| this.focus_search(window, cx))),
            );
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, agent)| {
                let id = agent["id"].as_str().unwrap_or("").to_owned();
                let close_id = id.clone();
                let menu_id = id.clone();
                let selected = self.agent["id"] == id;
                let title = agent["title"]
                    .as_str()
                    .unwrap_or("Untitled conversation")
                    .to_owned();
                div()
                    .id(("paseo-tab", ix))
                    .debug_selector(move || format!("paseo-tab-{ix}"))
                    .role(gpui::Role::Tab)
                    .aria_label(title.clone())
                    .aria_selected(selected)
                    .group("title-tab")
                    .h(px(TITLE_BAR_HEIGHT))
                    .w(px(TITLE_TAB_MAX_WIDTH))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_context_menu(
                                menus::Target::Tab(menu_id.clone()),
                                event.position,
                                window,
                                cx,
                            );
                        }),
                    )
                    .child(
                        title_tab_plate(selected, false)
                            .w_full()
                            .min_w_0()
                            .gap_1p5()
                            .group_hover("title-tab", move |s| s.bg(title_tab_fill(selected, true)))
                            .child(icon("chat", 14.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(TITLE_TAB_TEXT_SIZE))
                                    .text_color(content(if selected { 1. } else { 0.6 }))
                                    .child(title),
                            )
                            .child(
                                icon_button("close-paseo-tab", "Close local tab", "close")
                                    .size(px(18.))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.close_tab(&close_id, cx);
                                    })),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.open(id.clone(), cx)))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        div()
            .h(px(TITLE_BAR_HEIGHT))
            .flex()
            .items_center()
            .child(navigation)
            .child(
                div()
                    .id("title-tab-strip")
                    .debug_selector(|| "title-tab-strip".into())
                    .role(gpui::Role::TabList)
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .overflow_x_scroll()
                    .children(tabs)
                    .child(
                        crate::shell::titlebar_drag_area()
                            .debug_selector(|| "paseo-tab-drag-blank".into())
                            .min_w(px(24.))
                            .flex_1(),
                    ),
            )
            .child(
                crate::shell::titlebar_drag_area()
                    .w(px(20.))
                    .flex_shrink_0(),
            )
            .child(
                icon_button("paseo-info", "Connection and agent information", "settings")
                    .mr_3()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.info_visible = !this.info_visible;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
    fn composer(&mut self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let stop = self.agent["activeTurn"]["turnId"].is_string();
        let ready =
            self.status == "connected" && !self.busy() && self.sending.is_none() && !self.uncertain;
        let enabled = self.status == "connected"
            && (stop || (ready && !self.input.read(cx).content.trim().is_empty()));
        let permissions = self.agent["pendingPermissions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        div().w_full().max_w(px(COLUMN)).flex().flex_col().px(px(10.)).pb(px(6.))
            .children(permissions.into_iter().enumerate().map(|(request_ix,permission)|{
                let request=permission["id"].as_str().unwrap_or("").to_owned();let actions=permission["actions"].as_array().cloned().unwrap_or_default();
                div().mb_2().p_3().rounded_lg().bg(content(0.07)).text_size(px(12.))
                    .child(format!("审批 · {}\n{}",permission["title"].as_str().unwrap_or(&request),permission["description"].as_str().unwrap_or("")))
                    .child(div().max_h(px(120.)).overflow_hidden().text_size(px(11.)).child(serde_json::to_string_pretty(&permission["detail"]).unwrap_or_default()))
                    .when(permission["kind"]=="question"||actions.is_empty(),|d|d.child("此类型请在 Paseo 处理；原型不会自动批准。"))
                    .when(permission["kind"]!="question",|d|d.child(div().flex().gap_2().children(actions.into_iter().enumerate().map(|(ix,action)|{
                        let request=request.clone();let action_id=action["id"].as_str().unwrap_or("").to_owned();
                        div().id(("paseo-permission",request_ix*100+ix)).px_2().py_1().rounded_md().bg(content(0.1)).cursor_pointer().child(action["label"].as_str().unwrap_or("Action").to_owned())
                            .on_click(cx.listener(move|this,_,_,cx|{this.command("permission",json!({"requestId":request,"actionId":action_id}));cx.notify();}))
                    }))))
            }))
            .when(!self.error.is_empty(),|d|d.child(div().mb_2().text_size(px(12.)).text_color(gpui::rgb(0xf4a080)).child(self.error.clone())))
            .when(self.uncertain,|d|d.child(div().id("paseo-unlock").mb_2().text_size(px(12.)).child("已在 Paseo 核对历史，允许新发送").cursor_pointer().on_click(cx.listener(|this,_,_,cx|{this.uncertain=false;cx.notify();}))))
            .child(native_shell::composer_surface(self.input.focus_handle(cx).is_focused(window))
                .on_mouse_down(gpui::MouseButton::Left,cx.listener(|this,_,window,cx|this.focus_composer(window,cx)))
                .child(native_shell::composer_input().child(self.input.clone()))
                .child(native_shell::composer_toolbar()
                    .child(div().text_size(px(12.)).text_color(content(0.5)).child(format!("{} · {}",self.agent["provider"].as_str().unwrap_or("Paseo"),self.agent["model"].as_str().unwrap_or("Daemon settings"))))
                    .child(div().flex_1())
                    .child(div().text_size(px(11.)).text_color(content(0.4)).mr_2().child(if self.status!="connected"{"Reconnecting…"}else if stop{"Working"}else if self.busy(){"Waiting"}else{""}))
                    .child(native_shell::composer_action(stop,enabled).on_click(cx.listener(move|this,_,_,cx|{
                        if !enabled{return;}
                        if stop{this.command("cancel",json!({"turnId":this.agent["activeTurn"]["turnId"]}));cx.notify();}else{this.send(cx);}
                    })))))
            .into_any_element()
    }
}
impl Render for Prototype {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_pending {
            self.focus_composer(window, cx);
        }
        let titlebar = self.titlebar(window, cx);
        let sidebar = self.sidebar(cx);
        let composer = self.composer(window, cx);
        let selected = self.agent["id"].is_string();
        let conversation = div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .when(!selected && !self.error.is_empty(), |d| {
                d.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_size(px(12.))
                        .text_color(gpui::rgb(0xf4a080))
                        .child(self.error.clone()),
                )
            })
            .when(selected, |d| {
                d.child(
                    div()
                        .h(px(32.))
                        .px_3()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(11.))
                        .text_color(content(0.5))
                        .when(self.snapshot["hasOlder"] == true, |d| {
                            d.child(
                                icon_button("paseo-older", "Read older history", "arrow-up")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.command("older", json!({}));
                                        cx.notify();
                                    })),
                            )
                        })
                        .when(self.snapshot["gap"] == true, |d| {
                            d.child("部分中间历史未载入")
                        })
                        .child(div().flex_1())
                        .child(
                            icon_button("paseo-latest", "Sync latest history", "refresh").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.command("latest", json!({}));
                                    cx.notify();
                                }),
                            ),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(selected, |d| {
                        d.child(
                            self.transcript
                                .clone()
                                .cached(gpui::StyleRefinement::default().size_full()),
                        )
                    })
                    .when(!selected, |d| {
                        d.flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(18.))
                            .text_color(content(0.5))
                            .child("选择一个 Paseo 伙伴继续对话")
                    }),
            )
            .when(selected, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .w_full()
                        .flex()
                        .justify_center()
                        .child(composer),
                )
            });
        let footer = div()
            .h(px(28.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(10.))
            .text_color(content(0.45))
            .child(
                div()
                    .size(px(5.))
                    .rounded_full()
                    .bg(if self.status == "connected" {
                        gpui::rgb(0x78b88b)
                    } else {
                        gpui::rgb(0xc99565)
                    }),
            )
            .child(format!("Paseo · {}", self.status))
            .child(div().flex_1())
            .child("请交替发送；双端同时发送仍有中断竞态。");
        let body = native_shell::inset_layout(
            titlebar,
            sidebar,
            conversation.into_any_element(),
            footer.into_any_element(),
            260.,
            if self.sidebar_visible { 1. } else { 0. },
            vec![],
        );
        div().id("monocode-workspace").debug_selector(||"monocode-workspace".into()).key_context("Workspace").role(gpui::Role::Application).aria_label("Monocode Paseo Prototype")
            .size_full().relative().flex().flex_col().bg(shell_fill()).text_color(content(1.)).font_family(crate::fonts::UI)
            .on_action(cx.listener(|this,_:&crate::shell::ToggleSidebar,window,cx|this.toggle_sidebar(window,cx)))
            .on_action(cx.listener(|this,_:&crate::shell::FocusSearch,window,cx|this.focus_search(window,cx)))
            .on_action(cx.listener(|this,_:&FocusComposer,window,cx|this.focus_composer(window,cx)))
            .on_action(cx.listener(|this,_:&menus::RenameSelected,window,cx|this.rename_selected(window,cx)))
            .on_action(cx.listener(|this,_:&crate::shell::NextTab,_,cx|this.step_tab(1,cx)))
            .on_action(cx.listener(|this,_:&crate::shell::PreviousTab,_,cx|this.step_tab(-1,cx)))
            .on_action(cx.listener(|this,slot:&crate::shell::ActivateTab,_,cx|this.activate_tab(slot.0,cx)))
            .on_action(cx.listener(|this,_:&crate::shell::CloseAllTabs,_,cx|{let ids=this.tabs.iter().filter_map(|a|a["id"].as_str().map(str::to_owned)).collect::<Vec<_>>();for id in ids{this.close_tab(&id,cx);}}))
            .on_action(cx.listener(|this,_:&crate::shell::CloseTab,_,cx|{if let Some(id)=this.agent["id"].as_str().map(str::to_owned){this.close_tab(&id,cx);}}))
            .on_action(cx.listener(|this,_:&crate::shell::DismissMenu,window,cx|{this.info_visible=false;this.search_visible=false;this.focus_composer(window,cx);cx.notify();}))
            .children(body)
            .children(self.context_menu(cx))
            .when(self.info_visible,|d|d.child(div().id("paseo-connection-info").absolute().top(px(TITLE_BAR_HEIGHT)).right(px(12.)).w(px(350.)).p_4().rounded_lg().border_1().border_color(content(0.15)).bg({let mut fill=pane_fill();fill.a=1.;fill}).text_size(px(12.))
                .child(format!("Paseo {} · {}\nServer: {}\nAgent: {}\n{}\n\n关闭窗口和标签只释放客户端资源，不停止伙伴。私有 Bot 面板、文件和终端请使用 Paseo。",self.version,self.status,self.server,self.agent["id"].as_str().unwrap_or("—"),self.agent["cwd"].as_str().unwrap_or("")))))
    }
}
pub(crate) fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join("Library/Application Support/Monocode Paseo Prototype")
}
pub(crate) fn run() {
    // Dedicated client-only location; no Monocode Store or provider settings are loaded.
    let _ = std::fs::create_dir_all(data_dir());
    let platform = gpui_platform::current_platform(false);
    gpui::Application::with_platform(platform)
        .with_assets(crate::assets::Assets)
        .run(|cx: &mut App| {
            let prefs = crate::settings::Preferences {
                paced_reveal: false,
                ..Default::default()
            };
            crate::theme::configure(&prefs, false);
            cx.set_global(prefs);
            input::bind(cx);
            bind_keys(cx);
            crate::popover::bind(cx);
            cx.bind_keys([gpui::KeyBinding::new("cmd-q", crate::Quit, None)]);
            cx.on_action(|_: &crate::Quit, cx| cx.quit());
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.open_window(
                native_shell::window_options(
                    "Monocode Paseo Prototype",
                    Some("dev.usemono.paseo-prototype"),
                    cx,
                ),
                |window, cx| {
                    let view = cx.new(Prototype::new);
                    view.update(cx, |this, cx| {
                        native_shell::pin_titlebar(window, cx);
                        this.focus_composer(window, cx);
                    });
                    window.activate_window();
                    view
                },
            )
            .expect("open prototype");
            cx.activate(true);
        });
}
// This uses the identical packaged bridge, parser and transcript projection as the GUI.
pub(crate) fn probe() -> Result<(), String> {
    let (bridge, receiver) = Bridge::start()?;
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if let Ok(command) = serde_json::from_str(&line) {
                bridge.send(command);
            }
        }
        // EOF releases only this SDK sidecar; it never cancels the daemon agent.
    });
    while let Ok(mut value) = receiver.recv_blocking() {
        if value["event"] == "snapshot" {
            let rows = project_rows(&value);
            value["nativeRowCount"] = json!(rows.len());
        }
        println!("{value}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn completed_turn() -> Value {
        let items = vec![
            json!({"type":"user_message","text":"这是隔离的渲染样例。希望在同一个伙伴里，从另一个客户端继续。"}),
            json!({"type":"assistant_message","text":"收到，我先检查离线样例。"}),
            json!({"type":"reasoning","text":"先检查样例文件，再整理结论。"}),
            json!({"type":"tool_call","callId":"read-1","name":"read","status":"completed","metadata":{"kind":"read","title":"Read sample.rs"},"detail":{"type":"read","filePath":"/fixture/sample.rs","content":"fn sample() {}"}}),
            json!({"type":"tool_call","callId":"run-2","name":"execute","status":"completed","metadata":{"kind":"execute","title":"Run checks"},"detail":{"type":"shell","command":"cargo check","output":"synthetic check passed"}}),
            json!({"type":"tool_call","callId":"edit-3","name":"edit","status":"completed","metadata":{"kind":"edit"},"detail":{"type":"edit","filePath":"/fixture/sample.rs","unifiedDiff":"-old\n+new"}}),
            json!({"type":"assistant_message","text":"样例检查完成，再确认一处引用。"}),
            json!({"type":"tool_call","callId":"search-4","name":"search","status":"completed","metadata":{"kind":"search"},"detail":{"type":"search","query":"sample","content":"/fixture/sample.rs:1"}}),
            json!({"type":"assistant_message","text":"可以。这里复用 **Monocode Native** 的输入框、Markdown 和工作区框架。\n\n```rust\nlet partner = (server_id, agent_id);\n```\n\n消息仍由 Paseo daemon 管理。这个画面使用离线样例，没有发送任何真实请求。"}),
            json!({"type":"assistant_message","text":"补充：后台通知已收到，上一段报告仍然保留。"}),
        ];
        json!({"entries":items.into_iter().enumerate().map(|(i,item)| json!({
            "seqStart":i+1,"seqEnd":i+1,"turnId":if i>=6 {Value::Null} else {json!("synthetic-turn")},
            "timestamp":if i==9 {"2026-10-25T00:12:36Z".into()} else {format!("2026-10-25T00:00:{i:02}Z")},"item":item
        })).collect::<Vec<_>>()})
    }

    #[gpui::test]
    fn projected_tools_keep_all_prose_and_collapse_only_work(cx: &mut gpui::TestAppContext) {
        cx.update(configure_test);
        let turn = completed_turn();
        let rows = project_rows(&turn);
        let calls: Vec<ToolCall> = rows
            .iter()
            .filter(|r| r.role == "tool")
            .map(|r| serde_json::from_str(&r.text).unwrap())
            .collect();
        assert_eq!(calls.len(), 4);
        for (call, id, action, target) in [
            (&calls[0], "read-1", "Read", "/fixture/sample.rs"),
            (&calls[1], "run-2", "Run", "cargo check"),
            (&calls[2], "edit-3", "Edit", "/fixture/sample.rs"),
            (&calls[3], "search-4", "Find", "sample"),
        ] {
            assert_eq!(call.id, id);
            assert_eq!(call.state, ToolState::Completed);
            assert_eq!(call.summary(), (action.into(), target.into()));
        }
        let view = cx.new(Prototype::new);
        view.update(cx, |view, cx| {
            view.server = "s".into();
            view.open("a".into(), cx);
            let snapshot = |entries: Value, running: bool| json!({"event":"snapshot", "serverId":"s", "agentId":"a",
                "agent":{"id":"a","status":if running {"running"} else {"idle"},"activeTurn":if running {json!({"turnId":"t"})} else {Value::Null}}, "entries":entries});
            view.receive(snapshot(json!(turn["entries"].as_array().unwrap()[..6]), true), cx);
            view.transcript.update(cx, |view, cx| {
                let (streaming, built) = view.test_work_rows(false, cx);
                assert!(streaming);
                assert!(built.contains(&(1, "work-line")));
                assert!(built.iter().any(|(_, kind)| *kind == "live-phase"));
                assert!(built.iter().any(|(_, kind)| *kind == "tool"));
            });
            view.receive(snapshot(turn["entries"].clone(), false), cx);
            view.transcript.update(cx, |view, cx| {
                let (streaming, built) = view.test_work_rows(false, cx);
                assert!(!streaming);
                assert!(built.contains(&(0, "plain")), "user prompt remains visible");
                let mut prose = built.iter().filter(|(_, kind)| *kind == "markdown").map(|(at, _)| *at).collect::<Vec<_>>();
                prose.dedup();
                assert_eq!(prose, [1, 6, 8, 9]);
                for at in [1, 6, 8, 9] {
                    assert!(built.contains(&(at, "markdown")), "every assistant reply visible in order: {built:?}");
                }
                assert!(built.iter().any(|(_, kind)| *kind=="phase"));
                assert!(!built.iter().any(|(_, kind)| matches!(*kind, "tool"|"reasoning"|"work-fold")));
                assert_eq!(view.turns[&1].duration_ms, 756_000);
            });
            // Busy/idle transitions must reach the view even without a history delta.
            view.receive(snapshot(turn["entries"].clone(), true), cx);
            view.transcript.update(cx, |view, cx| assert!(view.test_work_rows(false, cx).0));
            view.receive(snapshot(turn["entries"].clone(), false), cx);
            view.transcript.update(cx, |view, cx| assert!(!view.test_work_rows(false, cx).0));
        });
    }

    #[test]
    fn failed_cancelled_and_truncated_tools_stay_valid_native_records() {
        let mut entry = json!({"item":{"type":"tool_call","callId":"failed","name":"execute","status":"failed","error":{"message":"synthetic failure"},"detail":{"type":"shell","command":"test","output":"partial output"}}});
        let call = project_tool(&entry);
        assert_eq!(call.state, ToolState::Failed);
        assert_eq!(call.output, "partial output\nsynthetic failure");
        for status in ["canceled", "cancelled", "running"] {
            entry["item"]["status"] = json!(status);
            assert_eq!(
                project_tool(&entry).state,
                if status == "running" {
                    ToolState::Running
                } else {
                    ToolState::Cancelled
                }
            );
        }
        entry["item"]["error"] = Value::Null;
        entry["item"]["detail"] = json!({"type":"plain_text","text":"界".repeat(40000)});
        entry["prototypeTruncated"] = json!(true);
        let rows = project_rows(&json!({"entries":[entry]}));
        assert_eq!(
            rows.len(),
            1,
            "tool JSON must not be split or truncated after serialization"
        );
        let call: ToolCall = serde_json::from_str(&rows[0].text).unwrap();
        let notice = "\n[内容过长，原型截断；完整内容请在 Paseo 查看]";
        assert_eq!(call.output, format!("{}{notice}", "界".repeat(32000)));
    }
    #[test]
    fn canonical_projection_keeps_chinese_markdown_time_and_tool_detail() {
        let rows = project_rows(&json!({"entries":[
            {"timestamp":"2026-10-25T00:00:00Z","seqStart":1,"item":{"type":"user_message","text":"你好，同一个伙伴"}},
            {"seqStart":2,"item":{"type":"assistant_message","text":"**答复**\n```rust\nlet x = 1;\n```"}},
            {"seqStart":3,"item":{"type":"tool_call","name":"Read","status":"completed","detail":{"path":"safe.txt"}}}
        ]}));
        assert_eq!(rows[0].role, "user");
        assert_eq!(rows[0].text, "你好，同一个伙伴");
        assert_eq!(rows[0].sent_at, Some(1792886400000));
        assert!(rows.iter().any(|row| row.text.contains("safe.txt")));
    }
    #[gpui::test]
    fn native_receive_rejects_other_agent_snapshot_and_keeps_rejected_draft(
        cx: &mut gpui::TestAppContext,
    ) {
        let view = cx.new(Prototype::new);
        view.update(cx,|view,cx|{
            view.server="s".into();view.agent=json!({"id":"a","status":"idle"});
            view.input.update(cx,|input,cx|input.set_text("保留草稿".into(),cx));
            view.receive(json!({"event":"snapshot","serverId":"s","agentId":"b","agent":{"id":"b"},"entries":[]}),cx);
            assert_eq!(view.agent["id"],"a");
            view.sending=Some((7,"保留草稿".into()));
            view.receive(json!({"id":7,"ok":false,"error":"Send outcome unknown","uncertain":true}),cx);
            assert!(view.uncertain);assert!(view.sending.is_none());
            assert_eq!(view.input.read(cx).content,"保留草稿");
        });
    }
    #[gpui::test]
    fn tabs_keep_agent_transcripts_separate_and_duplicate_open_is_a_noop(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(configure_test);
        let view = cx.new(Prototype::new);
        view.update(cx, |view, cx| {
            view.server = "s".into();
            view.status = "connected".into();
            let snapshot = |id: &str, entries: Value| json!({"event":"snapshot","serverId":"s","agentId":id,"agent":{"id":id,"status":"idle"},"entries":entries});
            view.open("a".into(), cx);
            let a = view.transcript.entity_id();
            let entries = (0..30).map(|i| json!({"item":{"type":"assistant_message","text":format!("A-only-{i}")}})).collect::<Vec<_>>();
            view.receive(snapshot("a", json!(entries)), cx);
            assert_eq!(view.transcript.read(cx).source_rows().len(), 30);
            view.open("b".into(), cx);
            let b = view.transcript.entity_id();
            assert_ne!(a, b);
            assert!(view.transcript.read(cx).source_rows().is_empty());
            let serial = view.serial;
            view.open("b".into(), cx);
            assert_eq!(view.serial, serial, "double click issues no second command");
            assert_eq!(view.transcript.entity_id(), b);
            view.receive(snapshot("a", json!(entries)), cx);
            assert!(view.transcript.read(cx).source_rows().is_empty(), "late A snapshot cannot patch B");
            view.receive(snapshot("b", json!([
                {"item":{"type":"user_message","text":"B question"}},
                {"item":{"type":"assistant_message","text":"B answer"}}
            ])), cx);
            assert_eq!(view.transcript.read(cx).source_rows().iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["B question", "B answer"]);
            // The same agent's shorter authoritative page must remove its old tail too.
            view.receive(snapshot("b", json!([])), cx);
            assert!(view.transcript.read(cx).source_rows().is_empty());
            view.open("a".into(), cx);
            assert_eq!(view.transcript.entity_id(), a);
            assert!(view.transcript.read(cx).source_rows().iter().all(|r| r.text.starts_with("A-only-")));
            assert_eq!(view.transcript.read(cx).source_rows().len(), 30);
            view.close_tab("b", cx);
            assert!(!view.transcripts.contains_key("b"));
            view.close_tab("a", cx);
            assert!(view.transcripts.is_empty());
            assert!(view.transcript.read(cx).source_rows().is_empty());
            assert!(view.agent.is_null());
        });
    }

    #[gpui::test]
    fn successful_open_clears_non_permission_attention_only(cx: &mut gpui::TestAppContext) {
        for reason in ["finished", "error", "permission"] {
            let view = cx.new(Prototype::new);
            view.update(cx, |view, cx| {
                view.server = "s".into();
                view.open("a".into(), cx);
                let open = view.serial;
                view.receive(json!({"event":"snapshot","serverId":"s","agentId":"a","agent":{"id":"a","status":"idle","requiresAttention":true,"attentionReason":reason},"entries":[]}), cx);
                assert!(!view.pending.values().any(|op| op == "clearAttention"));
                view.receive(json!({"id":open,"ok":true,"value":{"agentId":"a"}}), cx);
                assert_eq!(view.pending.values().filter(|op| *op == "clearAttention").count(), usize::from(reason != "permission"));
            });
        }
    }
    fn fixture(cx: &mut Context<Prototype>) -> Prototype {
        let mut view = Prototype::new(cx);
        view.status = "connected".into();
        view.server = "fixture-server".into();
        view.version = "fixture 0.13".into();
        view.error.clear();
        let now = monocode_core::time::now_ms() as i64;
        let updated = |minutes: i64| {
            chrono::DateTime::from_timestamp_millis(now - minutes * 60_000)
                .unwrap()
                .to_rfc3339()
        };
        view.directory = vec![
            json!({"agent":{"id":"fixture-a","workspaceId":"daily-a","title":"继续同一个伙伴的对话","provider":"hermes","model":"openai-codex:gpt-6.1-sol","status":"idle","updatedAt":updated(1),"activeTurn":null,"pendingPermissions":[],"cwd":"/fixture/monocode"},"project":{"projectKey":"daily","projectName":"Workspace","workspaceName":"接着刚才继续","checkout":{"cwd":"/fixture/daily","currentBranch":null}}}),
            json!({"agent":{"id":"fixture-b","workspaceId":"daily-b","title":"原生界面与 Paseo 对照","provider":"claude-code","model":"custom:sub2tok:claude-opus-5-5","status":"idle","updatedAt":updated(4),"activeTurn":null,"pendingPermissions":[],"cwd":"/fixture/daily"},"project":{"projectKey":"daily","projectName":"Workspace","workspaceName":"原生界面对照","checkout":{"cwd":"/fixture/daily","currentBranch":null}}}),
            json!({"agent":{"id":"fixture-c","workspaceId":"daily-shared","title":"计划步骤","provider":"omp","model":"custom:cpa-gemini:gemini-3.8-flash-high","status":"running","updatedAt":updated(6),"activeTurn":{"turnId":"fixture-plan"},"pendingPermissions":[],"cwd":"/fixture/daily"},"project":{"projectKey":"daily","projectName":"Workspace","workspaceName":"共同计划","checkout":{"cwd":"/fixture/daily","currentBranch":"plan"}}}),
            json!({"agent":{"id":"fixture-d","workspaceId":"daily-shared","title":"补充资料","provider":"codex","model":"gpt-6.1-sol","status":"closed","updatedAt":updated(9),"pendingPermissions":[],"cwd":"/fixture/daily"},"project":{"projectKey":"daily","projectName":"Workspace","workspaceName":"共同计划","checkout":{"cwd":"/fixture/daily","currentBranch":"plan"}}}),
            json!({"agent":{"id":"fixture-e","workspaceId":"bot-research","title":"资料助手的长标题","provider":"example-hermes-research","model":"custom:cpa-gemini:gemini-3.8-flash-high","status":"idle","requiresAttention":true,"attentionReason":"finished","updatedAt":updated(7),"pendingPermissions":[],"cwd":"/fixture/bots"},"project":{"projectKey":"bots","projectName":"Helpers","workspaceName":"资料助手","checkout":{"cwd":"/fixture/bots","currentBranch":null}}}),
            json!({"agent":{"id":"fixture-f","workspaceId":"bot-storage","title":"存储助手的长标题","provider":"example-hermes-storage","model":"openai-codex:gpt-6.1-sol","status":"running","activeTurn":{"turnId":"fixture-storage"},"updatedAt":updated(8),"pendingPermissions":[],"cwd":"/fixture/bots"},"project":{"projectKey":"bots","projectName":"Helpers","workspaceName":"存储助手","checkout":{"cwd":"/fixture/bots","currentBranch":null}}}),
        ];
        for entry in &mut view.directory {
            entry["workspace"] = json!({"id":entry["agent"]["workspaceId"],"name":entry["project"]["workspaceName"],"title":null,"pinnedAt":null,"canArchive":true});
        }
        view.directory[0]["workspace"]["pinnedAt"] = json!("2026-10-25T00:00:00Z");
        view.open("fixture-b".into(), cx);
        view.open("fixture-a".into(), cx);
        let agent = view.directory[0]["agent"].clone();
        view.receive(json!({"event":"snapshot","serverId":"fixture-server","agentId":"fixture-a","agent":agent,"epoch":"fixture-epoch","hasOlder":true,"entries":completed_turn()["entries"]}),cx);
        view.input.update(cx, |input, cx| {
            input.set_text("接着刚才的上下文继续…".into(), cx)
        });
        view
    }
    pub(super) fn configure_test(cx: &mut App) {
        let prefs = crate::settings::Preferences {
            paced_reveal: false,
            ..Default::default()
        };
        crate::theme::configure(&prefs, false);
        cx.set_global(prefs);
        input::bind(cx);
        bind_keys(cx);
        cx.set_reduce_motion(true);
    }
    #[gpui::test]
    fn workspace_layout_and_real_shortcuts_keep_focus_and_remote_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(configure_test);
        let window = cx.update(|cx| {
            let options = native_shell::window_options("Prototype", None, cx);
            assert_eq!(options.app_owns_titlebar_drag, cfg!(target_os = "macos"));
            assert_eq!(
                options.titlebar.as_ref().unwrap().appears_transparent,
                cfg!(target_os = "macos")
            );
            if cfg!(target_os = "macos") {
                assert_eq!(
                    options.titlebar.as_ref().unwrap().traffic_light_position,
                    Some(gpui::point(px(12.), px(13.)))
                );
            }
            cx.open_window(options, |_, cx| cx.new(fixture)).unwrap()
        });
        cx.refresh().unwrap();
        cx.run_until_parked();
        let cx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        assert_eq!(cx.debug_bounds("title-tab-strip").unwrap().top(), px(0.));
        assert_eq!(
            cx.debug_bounds("title-tab-strip").unwrap().size.height,
            px(TITLE_BAR_HEIGHT)
        );
        assert_eq!(
            cx.debug_bounds("sidebar-frame").unwrap().size.width,
            px(260.)
        );
        assert_eq!(
            cx.debug_bounds("main-pane-frame").unwrap().top(),
            px(TITLE_BAR_HEIGHT)
        );
        assert!(cx.debug_bounds("composer-box").unwrap().size.width <= px(COLUMN));
        let blank = cx.debug_bounds("paseo-tab-drag-blank").unwrap();
        assert!(blank.size.width >= px(24.));
        assert!(blank.left() >= cx.debug_bounds("paseo-tab-1").unwrap().right());
        cx.simulate_keystrokes("cmd-k");
        cx.run_until_parked();
        window
            .update(cx, |view, window, cx| {
                assert!(view.search.focus_handle(cx).is_focused(window))
            })
            .unwrap();
        cx.simulate_keystrokes("cmd-b");
        cx.run_until_parked();
        assert!(cx.debug_bounds("sidebar-frame").is_none());
        window
            .update(cx, |view, window, cx| {
                assert!(view.input.focus_handle(cx).is_focused(window))
            })
            .unwrap();
        cx.simulate_keystrokes("ctrl-tab");
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| {
                assert_eq!(view.agent["id"], "fixture-b");
                assert_eq!(view.drafts["fixture-a"], "接着刚才的上下文继续…");
                view.agent = view.directory[1]["agent"].clone();
                view.input
                    .update(cx, |input, cx| input.set_text("Do not send".into(), cx));
                cx.notify();
            })
            .unwrap();
        cx.simulate_keystrokes("shift-enter");
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| {
                assert!(view.sending.is_none());
                assert!(view.input.read(cx).content.contains('\n'));
            })
            .unwrap();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        window
            .update(cx, |view, _, cx| {
                assert!(view.sending.is_some());
                assert_eq!(
                    view.pending
                        .values()
                        .filter(|op| op.as_str() == "send")
                        .count(),
                    1
                );
                view.sending = None;
                view.close_tab("fixture-b", cx);
                assert_eq!(view.agent["id"], "fixture-a");
                assert!(
                    !view
                        .pending
                        .values()
                        .any(|op| matches!(op.as_str(), "cancel" | "archive" | "delete"))
                );
            })
            .unwrap();
    }
    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "Requires a Metal device; uses actual Prototype rendering, no OS windows or daemon"]
    fn render_native_workspace_fixture() {
        use std::sync::Arc;
        let mut cx = gpui::HeadlessAppContext::with_platform(
            Arc::new(gpui_apple::AppleTextSystem::new()),
            Arc::new(crate::assets::Assets),
            || {
                eprintln!("native fixture: initializing Metal renderer (no OS window)");
                Ok(Some(Box::new(
                    gpui_apple::metal_renderer::MetalHeadlessRenderer::new(),
                )))
            },
        );
        cx.update(configure_test);
        let window = cx
            .open_window(gpui::size(px(1180.), px(800.)), |_, cx| cx.new(fixture))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
        let image = cx.capture_screenshot(window.into()).unwrap();
        let expected = cx
            .update_window(window.into(), |_, window, _| {
                window.bounds().size.to_device_pixels(window.scale_factor())
            })
            .unwrap();
        assert_eq!(
            (image.width(), image.height()),
            (expected.width.0 as u32, expected.height.0 as u32)
        );
        let out = std::env::var_os("PASEO_R2_RENDER_DIR")
            .map(PathBuf::from)
            .expect("Set PASEO_R2_RENDER_DIR to an isolated output directory");
        std::fs::create_dir_all(&out).unwrap();
        image.save(out.join("r2-native-fixture.png")).unwrap();
        window
            .update(&mut cx, |view, _, cx| {
                view.info_visible = true;
                view.sidebar_visible = false;
                cx.notify();
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.capture_screenshot(window.into())
            .unwrap()
            .save(out.join("r2-native-info-fixture.png"))
            .unwrap();
    }
}
