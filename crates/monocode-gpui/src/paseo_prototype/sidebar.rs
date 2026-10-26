//! Page-local presentation of daemon-owned agents; no local workspace/session creation.
use super::*;
use monocode_core::project_appearance::ProjectAppearance;
use std::cmp::Reverse;

struct Section<'a> {
    key: &'a str,
    name: &'a str,
    newest: u64,
    workspaces: Vec<Workspace<'a>>,
}
struct Workspace<'a> {
    key: String,
    name: Option<&'a str>,
    newest: u64,
    pinned: bool,
    entries: Vec<&'a Value>,
}
impl Workspace<'_> {
    fn is_group(&self) -> bool {
        self.entries.len() > 1
    }
}
fn label(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.trim().is_empty())
}
fn updated_at(entry: &Value) -> u64 {
    entry["agent"]["updatedAt"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or(0, |date| date.timestamp_millis().max(0) as u64)
}
fn pinned(entry: &Value) -> bool {
    entry["workspace"]["pinnedAt"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
}
fn sections(entries: &[Value]) -> Vec<Section<'_>> {
    let mut sections: Vec<Section<'_>> = vec![];
    for entry in entries {
        let project = &entry["project"];
        let key = project["projectKey"].as_str().unwrap_or("");
        let at = match sections.iter().position(|section| section.key == key) {
            Some(at) => at,
            None => {
                sections.push(Section {
                    key,
                    name: label(&project["projectName"]).unwrap_or("Project"),
                    newest: 0,
                    workspaces: vec![],
                });
                sections.len() - 1
            }
        };
        let section = &mut sections[at];
        let updated = updated_at(entry);
        section.newest = section.newest.max(updated);
        // Older daemon agents may lack workspaceId. Never combine those unrelated agents.
        let workspace_key = label(&entry["agent"]["workspaceId"])
            .map(|id| format!("workspace:{id}"))
            .unwrap_or_else(|| format!("agent:{}", entry["agent"]["id"].as_str().unwrap_or("")));
        if let Some(workspace) = section
            .workspaces
            .iter_mut()
            .find(|w| w.key == workspace_key)
        {
            workspace.newest = workspace.newest.max(updated);
            workspace.pinned |= pinned(entry);
            workspace.entries.push(entry);
        } else {
            section.workspaces.push(Workspace {
                key: workspace_key,
                name: label(&project["workspaceName"]),
                newest: updated,
                pinned: pinned(entry),
                entries: vec![entry],
            });
        }
    }
    for section in &mut sections {
        section
            .workspaces
            .sort_by_key(|w| (!w.pinned, Reverse(w.newest)));
        for workspace in &mut section.workspaces {
            workspace
                .entries
                .sort_by_key(|entry| Reverse(updated_at(entry)));
        }
    }
    sections.sort_by_key(|section| Reverse(section.newest));
    sections
}
fn card_title<'a>(workspace: &Workspace<'a>, entry: &'a Value) -> &'a str {
    let title = label(&entry["agent"]["title"]).unwrap_or("Untitled conversation");
    if !workspace.is_group() {
        workspace.name.unwrap_or(title)
    } else {
        title
    }
}
fn provider_mark_id(provider: &str) -> Option<&'static str> {
    match provider {
        "hermes" => Some("hermes"),
        "omp" => Some("omp"),
        "claude" | "claude-code" => Some("claude"),
        "codex" => Some("codex"),
        id if id.contains("-hermes-") => Some("hermes"),
        _ => None,
    }
}
fn provider_mark(provider: &str, side: f32) -> gpui::Div {
    div()
        .relative()
        .size(px(side))
        .flex_shrink_0()
        .child(match provider_mark_id(provider) {
            Some(id) => provider_icon(id, side).into_any_element(),
            None => icon("chat", side).into_any_element(),
        })
        .when(provider.contains("-hermes-"), |d| {
            d.child(
                div()
                    .absolute()
                    .right(px(-1.))
                    .bottom(px(-1.))
                    .size(px(4.))
                    .rounded_full()
                    .bg(gpui::rgb(ProjectAppearance::default().color(provider))),
            )
        })
}
#[derive(Debug, PartialEq, Eq)]
enum Suffix {
    Approval,
    Failed,
    Working,
    Done,
    Time,
}
fn suffix_kind(entry: &Value) -> Suffix {
    let agent = &entry["agent"];
    if agent["pendingPermissions"]
        .as_array()
        .is_some_and(|p| !p.is_empty())
        || agent["attentionReason"] == "permission"
    {
        Suffix::Approval
    } else if agent["status"] == "error" || agent["attentionReason"] == "error" {
        Suffix::Failed
    } else if agent["status"] == "running" || !agent["activeTurn"].is_null() {
        Suffix::Working
    } else if agent["requiresAttention"] == true && agent["attentionReason"] == "finished" {
        Suffix::Done
    } else {
        Suffix::Time
    }
}
fn suffix(entry: &Value) -> gpui::Div {
    div()
        .flex_shrink_0()
        .text_size(px(11.))
        .line_height(px(16.))
        .font_weight(gpui::FontWeight::NORMAL)
        .text_color(content(0.4))
        .child(match suffix_kind(entry) {
            Suffix::Approval => div()
                .flex()
                .items_center()
                .gap_1()
                .text_color(gpui::rgb(0xfbbf24))
                .child(icon("circle-alert", 14.))
                .child("Needs approval")
                .into_any_element(),
            // Match the original task rail's failed/blocked alert mark.
            Suffix::Failed => div()
                .flex()
                .items_center()
                .gap_1()
                .text_color(gpui::rgb(0xfbbf24))
                .child(icon("circle-alert", 14.))
                .child("Failed")
                .into_any_element(),
            Suffix::Working => div()
                .text_color(content(0.5))
                .child("Working…")
                .into_any_element(),
            Suffix::Done => div()
                .flex()
                .items_center()
                .gap_1()
                .text_color(gpui::rgb(0x2dd4bf))
                .child(icon("check-circle-2", 14.).text_color(gpui::rgb(0x2dd4bf)))
                .child("Done")
                .into_any_element(),
            Suffix::Time => div()
                .child(monocode_core::time::relative(
                    updated_at(entry),
                    monocode_core::time::now_ms(),
                ))
                .into_any_element(),
        })
}
impl Prototype {
    fn sidebar_card(
        &self,
        entry: &Value,
        title: &str,
        nested: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if let Some(row) = self.rename_row(entry, cx) {
            return row;
        }
        let agent = &entry["agent"];
        let id = agent["id"].as_str().unwrap_or("").to_owned();
        let menu_entry = entry.clone();
        let provider = agent["provider"].as_str().unwrap_or("");
        let checkout = &entry["project"]["checkout"];
        let branch = label(&checkout["currentBranch"]);
        let cwd = label(&checkout["cwd"])
            .or_else(|| label(&agent["cwd"]))
            .unwrap_or("");
        let directory = std::path::Path::new(cwd)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(cwd);
        let model = label(&agent["model"])
            .unwrap_or("")
            .rsplit(':')
            .next()
            .unwrap_or("");
        native_shell::session_row_surface(false, nested, self.agent["id"] == id)
            .id(gpui::SharedString::from(format!("paseo-session:{id}")))
            .group("session-row")
            .role(gpui::Role::Button)
            .aria_label(title.to_owned())
            .aria_description(format!("{provider} conversation"))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_context_menu(
                        menus::Target::Card(menu_entry.clone()),
                        event.position,
                        window,
                        cx,
                    );
                }),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(provider_mark(provider, 14.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(20.))
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(title.to_owned()),
                            )
                            .when(pinned(entry), |d| {
                                d.child(icon("pin", 12.).text_color(content(0.45)))
                            })
                            .child(suffix(entry)),
                    )
                    .child(
                        div()
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_size(px(11.))
                                    .text_color(content(0.45))
                                    .child(icon(
                                        if branch.is_some() {
                                            "git-branch"
                                        } else {
                                            "folder"
                                        },
                                        12.,
                                    ))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .child(branch.unwrap_or(directory).to_owned()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .min_w_0()
                                    .max_w(gpui::relative(0.5))
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_size(px(11.))
                                    .text_color(content(0.5))
                                    .child(div().min_w_0().truncate().child(model.to_owned()))
                                    .child(provider_mark(provider, 14.)),
                            ),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open(id.clone(), cx)))
            .into_any_element()
    }
    pub(super) fn sidebar(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let searching = !self.search.read(cx).content.trim().is_empty();
        let rows = sections(&self.directory)
            .into_iter()
            .map(|section| {
                let key = format!("section:{}", json!([self.server, section.key]));
                let collapsed = !searching && self.collapsed.contains(&key);
                let project_key = key.clone();
                let appearance = ProjectAppearance::default();
                div()
                    .mb_2()
                    .child(
                        div()
                            .id(gpui::SharedString::from(key))
                            .h(px(28.))
                            .px_2()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(content(0.5))
                            .cursor_pointer()
                            .role(gpui::Role::Button)
                            .aria_label(section.name.to_owned())
                            .aria_expanded(!collapsed)
                            .child(crate::project_settings::project_icon(
                                &appearance,
                                section.key,
                                14.,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(section.name.to_owned()),
                            )
                            .child(icon(
                                if collapsed {
                                    "chevron-right"
                                } else {
                                    "chevron-down"
                                },
                                12.,
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.collapsed.remove(&project_key) {
                                    this.collapsed.insert(project_key.clone());
                                }
                                cx.notify();
                            })),
                    )
                    .when(!collapsed, |d| {
                        d.children(section.workspaces.into_iter().map(|workspace| {
                            if !workspace.is_group() {
                                return div()
                                    .pb_px()
                                    .child(self.sidebar_card(
                                        workspace.entries[0],
                                        card_title(&workspace, workspace.entries[0]),
                                        false,
                                        cx,
                                    ))
                                    .into_any_element();
                            }
                            let key = format!(
                                "workspace:{}",
                                json!([self.server, section.key, workspace.key])
                            );
                            let collapsed = !searching && self.collapsed.contains(&key);
                            let group_key = key.clone();
                            let count = workspace.entries.len();
                            let group_pinned = workspace.pinned;
                            div()
                                    .child(
                                        div()
                                            .id(gpui::SharedString::from(key))
                                            .group("paseo-workspace")
                                            .h(px(32.))
                                            .px_2()
                                            .rounded_md()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .text_color(content(0.5))
                                            .when(!collapsed, |d| {
                                                d.bg(content(0.05)).text_color(content(0.8))
                                            })
                                            .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
                                            .cursor_pointer()
                                            .role(gpui::Role::Button)
                                            .aria_expanded(!collapsed)
                                            .when(group_pinned, |d| {
                                                d.child(icon("pin", 12.).text_color(content(0.45)))
                                            })
                                            .child(
                                                div()
                                                    .relative()
                                                    .size(px(16.))
                                                    .flex_shrink_0()
                                                    .opacity(0.7)
                                                    .child(
                                                        icon("folder", 16.)
                                                            .when(!collapsed, |i| i.invisible())
                                                            .group_hover("paseo-workspace", |s| {
                                                                s.invisible()
                                                            }),
                                                    )
                                                    .child(
                                                        icon(
                                                            if collapsed {
                                                                "chevron-right"
                                                            } else {
                                                                "chevron-down"
                                                            },
                                                            16.,
                                                        )
                                                        .absolute()
                                                        .top_0()
                                                        .left_0()
                                                        .when(collapsed, |i| i.invisible())
                                                        .group_hover("paseo-workspace", |s| {
                                                            s.visible()
                                                        }),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_size(px(14.))
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                    .child(
                                                        workspace
                                                            .name
                                                            .unwrap_or("Workspace")
                                                            .to_owned(),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(content(0.4))
                                                    .child(count.to_string()),
                                            )
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if !this.collapsed.remove(&group_key) {
                                                    this.collapsed.insert(group_key.clone());
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .when(!collapsed, |d| {
                                        d.children(workspace.entries.iter().enumerate().map(
                                            |(ix, entry)| {
                                                crate::shell::sidebar_step(
                                                    ix + 1 == count,
                                                    self.sidebar_card(
                                                        entry,
                                                        card_title(&workspace, entry),
                                                        true,
                                                        cx,
                                                    ),
                                                )
                                            },
                                        ))
                                    })
                                    .into_any_element()
                        }))
                    })
            })
            .collect::<Vec<_>>();
        div()
            .size_full()
            .flex()
            .flex_col()
            .px_2()
            .child(
                div()
                    .h(px(36.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(13.))
                    .child(icon("chat", 14.))
                    .child("Conversations")
                    .child(div().flex_1())
                    .child(
                        icon_button("paseo-refresh", "Refresh conversations", "refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.list(false, cx))),
                    ),
            )
            .when(self.search_visible, |d| {
                d.child(div().p_2().child(self.search.clone()))
            })
            .child(
                div()
                    .id("paseo-directory")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows)
                    .when(self.directory.is_empty(), |d| {
                        d.child(
                            div()
                                .p_3()
                                .text_size(px(12.))
                                .text_color(content(0.5))
                                .child(if self.status == "connected" {
                                    "No matching conversations"
                                } else {
                                    "Connecting to Paseo…"
                                }),
                        )
                    }),
            )
            .when(self.next.is_some(), |d| {
                d.child(
                    icon_button("paseo-more", "Next page of conversations", "chevron-right")
                        .on_click(cx.listener(|this, _, _, cx| this.list(true, cx))),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn daemon_workspace_pins_sort_before_newer_unpinned_cards() {
        let entries = vec![
            json!({"agent":{"id":"new","workspaceId":"w-new","updatedAt":"2026-10-25T12:00:00Z"},"project":{"projectKey":"p"},"workspace":{"pinnedAt":null}}),
            json!({"agent":{"id":"old","workspaceId":"w-pin","updatedAt":"2026-10-25T01:00:00Z"},"project":{"projectKey":"p"},"workspace":{"pinnedAt":"2026-10-25T00:00:00Z"}}),
        ];
        let groups = sections(&entries);
        assert_eq!(groups[0].workspaces[0].entries[0]["agent"]["id"], "old");
        assert!(groups[0].workspaces[0].pinned);
        assert!(!groups[0].workspaces[1].pinned);
    }
    #[test]
    fn attention_reason_distinguishes_approval_failure_and_finished() {
        for (agent, expected) in [
            (
                json!({"pendingPermissions":[{"id":"request"}],"attentionReason":"finished"}),
                Suffix::Approval,
            ),
            (
                json!({"attentionReason":"permission","pendingPermissions":[]}),
                Suffix::Approval,
            ),
            (json!({"status":"error"}), Suffix::Failed),
            (
                json!({"attentionReason":"error","status":"running"}),
                Suffix::Failed,
            ),
            (
                json!({"requiresAttention":true,"attentionReason":"finished","pendingPermissions":[],"status":"idle"}),
                Suffix::Done,
            ),
            (
                json!({"status":"running","requiresAttention":true,"attentionReason":"finished"}),
                Suffix::Working,
            ),
            (
                json!({"status":"idle","activeTurn":{"turnId":"t"}}),
                Suffix::Working,
            ),
            (
                json!({"status":"closed","requiresAttention":false}),
                Suffix::Time,
            ),
            (
                json!({"status":"idle","requiresAttention":true}),
                Suffix::Time,
            ),
        ] {
            assert_eq!(suffix_kind(&json!({"agent":agent})), expected);
        }
    }
    #[test]
    fn single_workspaces_are_cards_and_shared_ids_group_by_latest_activity() {
        let entry = |project: &str, workspace: &str, id: &str, hour: u8, provider: &str| {
            json!({
                "project":{"projectKey":project,"projectName":project,"workspaceName":"Same short name"},
                "agent":{"id":id,"workspaceId":workspace,"title":format!("Long title {id}"),"provider":provider,"status":"closed","updatedAt":format!("2026-10-25T{hour:02}:00:00Z")}
            })
        };
        let mut entries = vec![
            entry("Helpers", "bot-a", "bot", 8, "example-hermes-x"),
            entry("Daily", "shared", "old", 9, "hermes"),
            entry("Daily", "single-b", "b", 10, "omp"),
            entry("Daily", "shared", "new", 12, "codex"),
            entry("Daily", "single-a", "a", 13, "hermes"),
        ];
        entries[0]["agent"]["status"] = json!("running");
        entries[2]["agent"]["status"] = json!("running");
        entries[3]["agent"]["status"] = json!("idle");
        let grouped = sections(&entries);
        assert_eq!(
            grouped.iter().map(|s| s.key).collect::<Vec<_>>(),
            ["Daily", "Helpers"]
        );
        let workspaces = &grouped[0].workspaces;
        assert_eq!(
            workspaces
                .iter()
                .map(Workspace::is_group)
                .collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(
            workspaces
                .iter()
                .map(|w| w.entries.len())
                .collect::<Vec<_>>(),
            [1, 2, 1]
        );
        assert_eq!(workspaces[0].entries[0]["agent"]["id"], "a");
        assert_eq!(
            card_title(&workspaces[0], workspaces[0].entries[0]),
            "Same short name"
        );
        assert_eq!(
            workspaces[1]
                .entries
                .iter()
                .map(|e| e["agent"]["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["new", "old"]
        );
        assert_eq!(
            card_title(&workspaces[1], workspaces[1].entries[0]),
            "Long title new"
        );
        assert_eq!(provider_mark_id("example-hermes-x"), Some("hermes"));
        assert_eq!(provider_mark_id("claude-code"), Some("claude"));
        assert_eq!(provider_mark_id("unknown-provider"), None);
    }
}
