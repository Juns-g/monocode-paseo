//! Native context menus for local tabs and exact daemon-owned workspace cards.
use super::*;
use crate::popover::{Anchor, Menu, MenuAction, MenuEvent, MenuItem, MenuStyle, menu_popover};
use gpui::{AnyElement, ClipboardItem, Pixels, Point};

gpui::actions!(paseo_menus, [RenameSelected, CancelRename]);
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("f2", RenameSelected, Some("Workspace && !PaseoRename")),
        gpui::KeyBinding::new("escape", CancelRename, Some("PaseoRename")),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn actions(items: Vec<MenuItem>) -> Vec<MenuAction> {
        items
            .into_iter()
            .filter_map(|i| {
                if let MenuItem::Action(a) = i {
                    Some(*a)
                } else {
                    None
                }
            })
            .collect()
    }
    #[test]
    fn native_menu_labels_shortcuts_and_disabled_boundaries() {
        let tabs = actions(tab_items(0, 1, false));
        assert_eq!(
            tabs.iter().map(|a| a.label.as_ref()).collect::<Vec<&str>>(),
            [
                "Close Tab",
                "Close Other Tabs",
                "Close Tabs to the Right",
                "Close Tabs to the Left",
                "Close All Tabs"
            ]
        );
        assert_eq!(
            tabs.iter().map(|a| a.disabled).collect::<Vec<_>>(),
            [false, true, true, true, false]
        );
        assert!(tabs[0].shortcut.is_some() && tabs[4].shortcut.is_some());
        assert!(actions(tab_items(1, 3, true)).iter().all(|a| a.disabled));
        let entry = json!({"agent":{"id":"a","workspaceId":"w","cwd":"/fixture"},"workspace":{"id":"w","pinnedAt":null,"canArchive":false}});
        let items = card_items(&entry, false);
        assert!(
            items
                .iter()
                .any(|i| matches!(i,MenuItem::Note(n) if n.as_ref()=="请在 Paseo 中归档"))
        );
        let cards = actions(items);
        assert!(
            cards
                .iter()
                .find(|a| a.id.as_ref() == "archive")
                .unwrap()
                .disabled
        );
        assert_eq!(
            cards
                .iter()
                .find(|a| a.id.as_ref() == "pin")
                .unwrap()
                .label
                .as_ref(),
            "Pin"
        );
        let mut pinned = entry.clone();
        pinned["workspace"]["pinnedAt"] = json!("2026-10-25T00:00:00Z");
        pinned["workspace"]["canArchive"] = json!(true);
        let cards = actions(card_items(&pinned, false));
        assert_eq!(
            cards
                .iter()
                .find(|a| a.id.as_ref() == "pin")
                .unwrap()
                .label
                .as_ref(),
            "Unpin"
        );
        assert!(
            !cards
                .iter()
                .find(|a| a.id.as_ref() == "archive")
                .unwrap()
                .disabled
        );
        assert!(!cards.iter().any(|a| a.id.as_ref() == "delete"));
    }
    #[gpui::test]
    fn real_native_menu_handlers_close_tabs_and_submit_workspace_rename(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(super::super::tests::configure_test);
        let window = cx.add_window(|_, cx| Prototype::new(cx));
        window.update(cx,|view,window,cx|{
            view.server="s".into();
            for id in ["a","b","c"] {view.open(id.into(),cx);}
            view.open_context_menu(Target::Tab("b".into()),gpui::point(px(20.),px(20.)),window,cx);
            assert!(view.menu.is_some());assert!(!view.focus_pending);view.menu=None;
            view.run_context_action(Target::Tab("b".into()),"right",window,cx);
            assert_eq!(view.tabs.len(),2);
            view.run_context_action(Target::Tab("b".into()),"left",window,cx);
            assert_eq!(view.tabs[0]["id"],"b");
            view.open("a".into(),cx);
            view.run_context_action(Target::Tab("b".into()),"others",window,cx);
            assert_eq!(view.tabs.len(),1);assert_eq!(view.agent["id"],"b");
            let entry=json!({"agent":{"id":"b","workspaceId":"w"},"project":{"projectKey":"p"},"workspace":{"id":"w","name":"Synthetic name","title":null,"pinnedAt":null,"canArchive":true}});
            view.focus_pending=true;
            view.start_rename(entry.clone(),window,cx);
            assert!(!view.focus_pending);
            view.renaming.as_ref().unwrap().input.update(cx,|i,cx|i.set_text(" 新标题 ".into(),cx));
            view.finish_rename(true,window,cx);
            assert!(view.pending.values().any(|op|op=="renameWorkspace"));
            view.focus_pending=true;
            view.start_rename(entry.clone(),window,cx);
            assert!(!view.focus_pending);
            view.renaming.as_ref().unwrap().input.update(cx,|i,cx|i.set_text(" ".into(),cx));
            let serial=view.serial;view.finish_rename(true,window,cx);assert_eq!(view.serial,serial);
            // A sub-conversation card renames its agent, prefilled from the agent title.
            let child=json!({"agent":{"id":"k","workspaceId":"w","title":"Child title","labels":{"paseo.parent-agent-id":"b"}},"project":{"projectKey":"p"},"workspace":{"id":"w","name":"Synthetic name","title":"Workspace title","pinnedAt":null,"canArchive":true}});
            view.start_rename(child.clone(),window,cx);
            assert_eq!(view.renaming.as_ref().unwrap().input.read(cx).content.as_ref(),"Child title");
            view.renaming.as_ref().unwrap().input.update(cx,|i,cx|i.set_text(" 子标题 ".into(),cx));
            let serial=view.serial;view.finish_rename(true,window,cx);
            assert_eq!(view.serial,serial+1);
            assert_eq!(view.pending.get(&view.serial).map(String::as_str),Some("renameAgent"));
            // An empty parent label is not a child; the workspace anchor keeps renameWorkspace.
            let anchor=json!({"agent":{"id":"b","workspaceId":"w","title":"Agent title","labels":{"paseo.parent-agent-id":""}},"project":{"projectKey":"p"},"workspace":{"id":"w","name":"Synthetic name","title":"Workspace title","pinnedAt":null,"canArchive":true}});
            view.start_rename(anchor,window,cx);
            assert_eq!(view.renaming.as_ref().unwrap().input.read(cx).content.as_ref(),"Workspace title");
            view.finish_rename(false,window,cx);
            // The reply refreshes the directory like the other workspace ops.
            let rename=view.serial;
            view.receive(json!({"id":rename,"ok":true,"value":{"serverId":"s","agentId":"k"}}),cx);
            assert!(view.pending.values().any(|op|op=="list"),"renameAgent reply must refresh the directory");
            view.run_context_action(Target::Card(entry.clone()),"pin",window,cx);
            assert!(view.pending.values().any(|op|op=="pinWorkspace"));
            view.run_context_action(Target::Tab("b".into()),"all",window,cx);
            assert!(view.tabs.is_empty());assert!(view.transcripts.is_empty());
            assert!(!view.pending.values().any(|op|matches!(op.as_str(),"send"|"cancel"|"delete"|"archiveWorkspace")));
            view.open("b".into(),cx);
            view.tabs[0]["workspaceId"]=json!("w");
            view.agent["workspaceId"]=json!("w");
            view.directory=vec![entry.clone()];
            view.run_context_action(Target::Card(entry),"archive",window,cx);
            let archive=view.serial;
            view.receive(json!({"id":archive,"ok":true,"value":{"serverId":"s","workspaceId":"w"}}),cx);
            assert!(view.tabs.is_empty());assert!(view.transcripts.is_empty());assert!(view.directory.is_empty());

        }).unwrap();
    }
}
#[derive(Clone)]
pub(super) enum Target {
    Tab(String),
    Card(Value),
}
pub(super) struct OpenMenu {
    menu: Entity<Menu>,
    anchor: Anchor,
    target: Target,
    server: String,
    _subscription: Subscription,
}
pub(super) struct Renaming {
    entry: Value,
    server: String,
    input: Entity<input::Input>,
    _subscriptions: Vec<Subscription>,
}
fn tab_items(index: usize, count: usize, locked: bool) -> Vec<MenuItem> {
    vec![
        MenuAction::new("close", "Close Tab")
            .shortcut(if cfg!(target_os = "macos") {
                "⌘W"
            } else {
                "Ctrl+W"
            })
            .disabled(locked)
            .into(),
        MenuItem::Separator,
        MenuAction::new("others", "Close Other Tabs")
            .disabled(locked || count < 2)
            .into(),
        MenuAction::new("right", "Close Tabs to the Right")
            .disabled(locked || index + 1 >= count)
            .into(),
        MenuAction::new("left", "Close Tabs to the Left")
            .disabled(locked || index == 0)
            .into(),
        MenuAction::new("all", "Close All Tabs")
            .shortcut(if cfg!(target_os = "macos") {
                "⌘⇧W"
            } else {
                "Ctrl+Shift+W"
            })
            .disabled(locked || count == 0)
            .into(),
    ]
}
fn path(entry: &Value) -> &str {
    entry["agent"]["cwd"]
        .as_str()
        .filter(|p| !p.is_empty())
        .or_else(|| entry["project"]["checkout"]["cwd"].as_str())
        .unwrap_or("")
}
fn card_items(entry: &Value, locked: bool) -> Vec<MenuItem> {
    let workspace = &entry["workspace"];
    let known = workspace["id"] == entry["agent"]["workspaceId"] && workspace["id"].is_string();
    let pin_known = known && workspace.get("pinnedAt").is_some();
    let archive = known && workspace["canArchive"] == true && !locked;
    let mut items = vec![
        MenuAction::new("open", "Open").disabled(locked).into(),
        MenuAction::new("copy-id", "Copy conversation ID").into(),
        MenuAction::new("copy-path", "Copy path")
            .disabled(path(entry).is_empty())
            .into(),
        MenuItem::Separator,
        MenuAction::new("rename", "Rename…")
            .shortcut("F2")
            .disabled(!known || locked)
            .into(),
        MenuAction::new(
            "pin",
            if workspace["pinnedAt"].is_string() {
                "Unpin"
            } else {
                "Pin"
            },
        )
        .disabled(!pin_known || locked)
        .into(),
        MenuItem::Separator,
        MenuAction::new("archive", "Archive")
            .disabled(!archive)
            .into(),
    ];
    if !archive {
        items.push(MenuItem::Note("请在 Paseo 中归档".into()));
    }
    items
}
impl Prototype {
    pub(super) fn open_context_menu(
        &mut self,
        target: Target,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        self.focus_pending = false;
        let items = match &target {
            Target::Tab(id) => tab_items(
                self.tabs.iter().position(|a| a["id"] == *id).unwrap_or(0),
                self.tabs.len(),
                self.sending.is_some(),
            ),
            Target::Card(entry) => card_items(entry, self.sending.is_some()),
        };
        let menu = cx.new(|cx| {
            Menu::new("Conversation actions", items, window, cx)
                .width(250.)
                .style(MenuStyle::Explorer)
        });
        let subscription =
            cx.subscribe_in(&menu, window, |this, _, event: &MenuEvent, window, cx| {
                let Some(open) = this.menu.take() else { return };
                if let MenuEvent::Picked(action, _) = event
                    && open.server == this.server
                {
                    this.run_context_action(open.target, action, window, cx);
                }
                cx.notify();
            });
        self.menu = Some(OpenMenu {
            menu,
            anchor: Anchor::Point(at),
            target,
            server: self.server.clone(),
            _subscription: subscription,
        });
        cx.notify();
    }
    pub(super) fn context_menu(&self, cx: &App) -> Option<crate::popover::Popover> {
        self.menu
            .as_ref()
            .map(|m| menu_popover(&m.menu, m.anchor.clone(), cx).gap(0.))
    }
    fn run_context_action(
        &mut self,
        target: Target,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            Target::Tab(id) => {
                if self.sending.is_some() {
                    return;
                }
                let Some(index) = self.tabs.iter().position(|a| a["id"] == id) else {
                    return;
                };
                let ids = self
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| match action {
                        "close" => *i == index,
                        "others" => *i != index,
                        "right" => *i > index,
                        "left" => *i < index,
                        "all" => true,
                        _ => false,
                    })
                    .filter_map(|(_, a)| a["id"].as_str().map(str::to_owned))
                    .collect::<Vec<_>>();
                for id in ids {
                    self.close_tab(&id, cx);
                }
            }
            Target::Card(entry) => match action {
                "open" => {
                    if let Some(id) = entry["agent"]["id"].as_str() {
                        self.open(id.into(), cx);
                    }
                }
                "copy-id" => cx.write_to_clipboard(ClipboardItem::new_string(
                    entry["agent"]["id"].as_str().unwrap_or("").into(),
                )),
                "copy-path" => {
                    cx.write_to_clipboard(ClipboardItem::new_string(path(&entry).into()))
                }
                "rename" => self.start_rename(entry, window, cx),
                "pin" => {
                    self.workspace_command(
                        "pinWorkspace",
                        &entry,
                        json!({"pinned":!entry["workspace"]["pinnedAt"].is_string()}),
                    );
                }
                "archive" if entry["workspace"]["canArchive"] == true => {
                    self.workspace_command("archiveWorkspace", &entry, json!({}));
                }
                _ => {}
            },
        }
    }
    fn workspace_command(&mut self, op: &str, entry: &Value, mut args: Value) {
        args["agentId"] = entry["agent"]["id"].clone();
        args["workspaceId"] = if entry["workspace"]["id"].is_string() {
            entry["workspace"]["id"].clone()
        } else {
            entry["agent"]["workspaceId"].clone()
        };
        self.error.clear();
        self.command(op, args);
    }
    pub(super) fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self
            .directory
            .iter()
            .find(|e| e["agent"]["id"] == self.agent["id"])
            .cloned()
        {
            self.start_rename(entry, window, cx);
        }
    }
    /// A sub-conversation card shows its own agent title; renaming it renames the agent.
    fn is_child_agent(entry: &Value) -> bool {
        entry["agent"]["labels"]["paseo.parent-agent-id"]
            .as_str()
            .is_some_and(|parent| !parent.is_empty())
    }
    fn start_rename(&mut self, entry: Value, window: &mut Window, cx: &mut Context<Self>) {
        let child = Self::is_child_agent(&entry);
        if !(if child {
            entry["agent"]["id"].is_string()
        } else {
            entry["workspace"]["id"].is_string()
        }) || self.sending.is_some()
        {
            return;
        }
        self.focus_pending = false;
        let input = cx.new(|cx| {
            let mut i = input::Input::field(
                "Rename. Press Enter to confirm or Escape to cancel.",
                "",
                content(0.35),
                cx,
            );
            i.set_font_size(14.);
            i
        });
        let title = if child {
            entry["agent"]["title"].as_str()
        } else {
            entry["workspace"]["title"]
                .as_str()
                .or_else(|| entry["workspace"]["name"].as_str())
        }
        .unwrap_or("")
        .to_owned();
        input.update(cx, |i, cx| {
            i.set_text(title.clone(), cx);
            i.select_range(0..title.len(), cx);
        });
        let focus = input.read(cx).focus_handle(cx);
        let subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |this, _, _: &input::Submitted, window, cx| this.finish_rename(true, window, cx),
            ),
            cx.on_blur(&focus, window, |this, window, cx| {
                this.finish_rename(true, window, cx)
            }),
        ];
        self.renaming = Some(Renaming {
            entry,
            server: self.server.clone(),
            input,
            _subscriptions: subscriptions,
        });
        self.sidebar_visible = true;
        if let Some(rename) = &self.renaming {
            let project = &rename.entry["project"]["projectKey"];
            self.collapsed
                .remove(&format!("project:{}", json!([self.server, project])));
            let workspace = format!(
                "workspace:{}",
                rename.entry["agent"]["workspaceId"].as_str().unwrap_or("")
            );
            self.collapsed.remove(&format!(
                "workspace:{}",
                json!([self.server, project, workspace])
            ));
        }
        window.focus(&focus, cx);
        cx.notify();
    }
    fn finish_rename(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.renaming.take() else {
            return;
        };
        let text = rename.input.read(cx).content.trim().to_owned();
        if commit && !text.is_empty() && rename.server == self.server {
            let op = if Self::is_child_agent(&rename.entry) {
                "renameAgent"
            } else {
                "renameWorkspace"
            };
            self.workspace_command(op, &rename.entry, json!({"title":text}));
        }
        self.focus_composer(window, cx);
        cx.notify();
    }
    pub(super) fn rename_row(&self, entry: &Value, cx: &mut Context<Self>) -> Option<AnyElement> {
        let rename = self
            .renaming
            .as_ref()
            .filter(|r| r.entry["agent"]["id"] == entry["agent"]["id"])?;
        Some(
            div()
                .key_context("PaseoRename")
                .h(px(32.))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .on_action(cx.listener(|this, _: &CancelRename, window, cx| {
                    this.finish_rename(false, window, cx)
                }))
                .child(div().size(px(16.)).flex_shrink_0())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(22.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded_sm()
                        .bg(content(0.1))
                        .border_1()
                        .border_color(gpui::rgba(0x4da3f566))
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(rename.input.clone()),
                )
                .into_any_element(),
        )
    }
}
