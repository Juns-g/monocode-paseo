//! In-conversation multi-host Paseo descendant tree (children -> grandchildren -> ...)
//! with live status, latest progress/conclusion summary, ancestor back-navigation,
//! and one-click cross-host session jump.
use super::*;
use gpui::rgb;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

const ACTIVE_POLL_INTERVAL: Duration = Duration::from_secs(6);
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenPaseoAgent {
    pub(crate) server_id: String,
    pub(crate) host_label: String,
    pub(crate) agent_id: String,
    pub(crate) cwd: String,
    pub(crate) title: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct DescendantRef {
    pub(super) server_id: String,
    pub(super) host_label: String,
    pub(super) is_remote: bool,
    pub(super) agent_id: String,
    pub(super) title: String,
    pub(super) cwd: String,
    pub(super) repo_name: String,
    pub(super) status: String,
    pub(super) archived: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct DescendantNode {
    pub(super) server_id: String,
    pub(super) host_label: String,
    pub(super) is_remote: bool,
    pub(super) agent_id: String,
    pub(super) title: String,
    pub(super) cwd: String,
    pub(super) repo_name: String,
    pub(super) branch: Option<String>,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) status: String,
    pub(super) needs_input: bool,
    pub(super) archived: bool,
    pub(super) created_at: u64,
    pub(super) updated_at: u64,
    pub(super) summary: Option<String>,
    pub(super) summary_kind: Option<String>,
    pub(super) children: Vec<DescendantNode>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct DescendantCounts {
    pub(super) total: usize,
    pub(super) active: usize,
    pub(super) running: usize,
    pub(super) attention: usize,
    pub(super) error: usize,
    pub(super) done: usize,
    pub(super) archived: usize,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct UnreachableHost {
    pub(super) server_id: Option<String>,
    pub(super) label: String,
    pub(super) error: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct DescendantRoot {
    pub(super) server_id: String,
    pub(super) agent_id: String,
    pub(super) host_label: String,
    pub(super) title: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct DescendantTree {
    pub(super) root: DescendantRoot,
    pub(super) parent: Option<DescendantRef>,
    pub(super) ancestors: Vec<DescendantRef>,
    pub(super) children: Vec<DescendantNode>,
    pub(super) counts: DescendantCounts,
    pub(super) unreachable_hosts: Vec<UnreachableHost>,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) tree: Option<DescendantTree>,
    pub(super) loading: bool,
    pub(super) last_fetched: Option<Instant>,
    pub(super) collapsed: bool,
    pub(super) show_archived: bool,
    pub(super) collapsed_branches: HashSet<String>,
}

impl State {
    fn due_for_refresh(&self, parent_busy: bool) -> bool {
        let Some(last) = self.last_fetched else {
            return true;
        };
        let has_live_children = self
            .tree
            .as_ref()
            .is_some_and(|t| t.counts.running > 0 || t.counts.attention > 0);
        let interval = if parent_busy || has_live_children {
            ACTIVE_POLL_INTERVAL
        } else {
            IDLE_POLL_INTERVAL
        };
        last.elapsed() >= interval
    }
}

struct FlatRow<'a> {
    node: &'a DescendantNode,
    depth: usize,
    branch_key: String,
    visible_children: usize,
    branch_collapsed: bool,
}

fn flatten_visible<'a>(
    nodes: &'a [DescendantNode],
    depth: usize,
    include_archived: bool,
    collapsed_branches: &HashSet<String>,
    out: &mut Vec<FlatRow<'a>>,
) {
    for node in nodes {
        if node.archived && !include_archived {
            continue;
        }
        let branch_key = format!("{}:{}", node.server_id, node.agent_id);
        let visible_children = node
            .children
            .iter()
            .filter(|c| include_archived || !c.archived)
            .count();
        let branch_collapsed = collapsed_branches.contains(&branch_key);
        out.push(FlatRow {
            node,
            depth,
            branch_key: branch_key.clone(),
            visible_children,
            branch_collapsed,
        });
        if visible_children > 0 && !branch_collapsed {
            flatten_visible(
                &node.children,
                depth + 1,
                include_archived,
                collapsed_branches,
                out,
            );
        }
    }
}

fn status_color(status: &str, archived: bool) -> gpui::Hsla {
    if archived {
        return content(0.32);
    }
    match status {
        "running" => rgb(0x38bdf8).into(),
        "attention" => rgb(0xf59e0b).into(),
        "error" => rgb(0xf43f5e).into(),
        "done" => rgb(0x34d399).into(),
        _ => content(0.45),
    }
}

fn status_label(status: &str, archived: bool) -> &'static str {
    if archived {
        return "已归档";
    }
    match status {
        "running" => "运行中",
        "attention" => "待处理",
        "error" => "报错",
        "done" => "已完成",
        _ => "空闲",
    }
}

fn format_relative_age(ms: u64) -> String {
    if ms == 0 {
        return String::new();
    }
    let now = monocode_core::time::now_ms();
    let diff_secs = now.saturating_sub(ms) / 1000;
    if diff_secs < 60 {
        "刚刚".into()
    } else if diff_secs < 3600 {
        format!("{}m", diff_secs / 60)
    } else if diff_secs < 86_400 {
        format!("{}h", diff_secs / 3600)
    } else {
        format!("{}d", diff_secs / 86_400)
    }
}

fn short_host_label(label: &str) -> String {
    let trimmed = label.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 16 {
        trimmed.to_owned()
    } else {
        format!("{}…", chars[..15].iter().collect::<String>())
    }
}

pub(super) fn parse_tree(value: Value) -> Result<DescendantTree, String> {
    serde_json::from_value(value).map_err(|e| format!("Invalid paseo.descendants payload: {e}"))
}

impl Conversation {
    pub(super) fn refresh_paseo_descendants(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.is_remote()
            || !self.remote_state.capabilities.contains("paseo.descendants")
            || self.remote_state.descendants.loading
        {
            return;
        }
        if !force && !self.remote_state.descendants.due_for_refresh(self.busy) {
            return;
        }
        let Some(client) = self.remote_state.client.clone() else {
            return;
        };
        let store = self.store.clone();
        let id = self.summary.id;
        self.remote_state.descendants.loading = true;
        if force {
            cx.notify();
        }
        let work = cx.background_executor().spawn(async move {
            let remote = locked(&store)?
                .remote_session(id)?
                .ok_or_else(|| "Missing remote session".to_string())?;
            let Some(agent_id) = remote.session_id.clone() else {
                return Ok::<Option<DescendantTree>, String>(None);
            };
            let machine = remote.machine(&client)?;
            let raw = client.request(
                &machine.id,
                "paseo.descendants",
                json!({
                    "agentId": agent_id,
                    "serverId": remote.project.environment_id,
                    "includeSummaries": true,
                }),
            )?;
            Ok(Some(parse_tree(raw)?))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.remote_state.descendants.loading = false;
                this.remote_state.descendants.last_fetched = Some(Instant::now());
                if let Ok(Some(tree)) = result {
                    let changed = this.remote_state.descendants.tree.as_ref() != Some(&tree);
                    this.remote_state.descendants.tree = Some(tree);
                    if changed {
                        cx.notify();
                    }
                } else if force {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn paseo_descendants_shelf(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tree = self.remote_state.descendants.tree.as_ref()?;
        let has_children = !tree.children.is_empty();
        let has_ancestors = !tree.ancestors.is_empty();
        if !has_children && !has_ancestors {
            return None;
        }

        let root_server = tree.root.server_id.clone();
        let mut card = div()
            .id("paseo-descendants-shelf")
            .debug_selector(|| "paseo-descendants-shelf".into())
            .mb(px(6.))
            .w_full()
            .rounded(px(10.))
            .border_1()
            .border_color(content(0.1))
            .bg(content(0.03))
            .overflow_hidden()
            .flex()
            .flex_col();

        // Top ancestor breadcrumb (e.g. `← 返回父对话 · 频道全栈需求总控 › 队长3`).
        if has_ancestors {
            // `ancestors` arrives nearest-first (`[parent, grandparent, ...]`); display root -> parent.
            let chain: Vec<&DescendantRef> = tree.ancestors.iter().rev().collect();
            let mut trail = div()
                .px(px(10.))
                .py(px(6.))
                .when(has_children, |d| {
                    d.border_b_1().border_color(content(0.07))
                })
                .flex()
                .items_center()
                .flex_wrap()
                .gap_1()
                .text_size(px(11.))
                .text_color(content(0.55))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(content(0.45))
                        .child("← 父对话"),
                );

            for (ix, anc) in chain.iter().enumerate() {
                if ix > 0 {
                    trail = trail.child(
                        div()
                            .flex_shrink_0()
                            .text_color(content(0.3))
                            .child("›"),
                    );
                }
                let target = OpenPaseoAgent {
                    server_id: anc.server_id.clone(),
                    host_label: anc.host_label.clone(),
                    agent_id: anc.agent_id.clone(),
                    cwd: anc.cwd.clone(),
                    title: anc.title.clone(),
                };
                let show_host = !anc.host_label.is_empty() && anc.server_id != root_server;
                let host_badge = short_host_label(&anc.host_label);
                let title = anc.title.clone();
                let dot_color = status_color(&anc.status, anc.archived);
                trail = trail.child(
                    div()
                        .id(("paseo-ancestor", ix))
                        .role(Role::Button)
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(5.))
                        .bg(content(0.05))
                        .hover(|s| s.bg(content(0.11)))
                        .cursor(CursorStyle::PointingHand)
                        .flex()
                        .items_center()
                        .gap_1()
                        .max_w(px(260.))
                        .child(div().size(px(6.)).rounded_full().flex_shrink_0().bg(dot_color))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(content(0.82))
                                .child(title),
                        )
                        .when(show_host, |d| {
                            d.child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(10.))
                                    .text_color(content(0.45))
                                    .child(host_badge),
                            )
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(target.clone());
                        })),
                );
            }
            card = card.child(trail);
        }

        if !has_children {
            return Some(card.into_any_element());
        }

        let collapsed = self.remote_state.descendants.collapsed;
        let loading = self.remote_state.descendants.loading;
        let include_archived =
            self.remote_state.descendants.show_archived || tree.counts.active == 0;
        let counts = &tree.counts;

        // Card header: summary counts + archived toggle + refresh button.
        let mut stats = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(11.))
            .text_color(content(0.55));
        stats = stats.child(format!("{} 个", counts.total));
        if counts.running > 0 {
            stats = stats.child(
                div()
                    .text_color(rgb(0x38bdf8))
                    .child(format!("{} 运行中", counts.running)),
            );
        }
        if counts.attention > 0 {
            stats = stats.child(
                div()
                    .text_color(rgb(0xf59e0b))
                    .child(format!("{} 待处理", counts.attention)),
            );
        }
        if counts.error > 0 {
            stats = stats.child(
                div()
                    .text_color(rgb(0xf43f5e))
                    .child(format!("{} 报错", counts.error)),
            );
        }
        if counts.done > 0 {
            stats = stats.child(
                div()
                    .text_color(rgb(0x34d399))
                    .child(format!("{} 已完成", counts.done)),
            );
        }

        let header = div()
            .id("paseo-descendants-header")
            .debug_selector(|| "paseo-descendants-header".into())
            .role(Role::Button)
            .aria_expanded(!collapsed)
            .px(px(10.))
            .py(px(7.))
            .cursor(CursorStyle::PointingHand)
            .hover(|s| s.bg(content(0.03)))
            .flex()
            .items_center()
            .gap_2()
            .on_click(cx.listener(|this, _, _, cx| {
                this.remote_state.descendants.collapsed = !this.remote_state.descendants.collapsed;
                cx.notify();
            }))
            .child(
                icon(
                    if collapsed {
                        "chevron-right"
                    } else {
                        "chevron-down"
                    },
                    12.,
                )
                .text_color(content(0.5)),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(content(0.86))
                    .child("子对话"),
            )
            .child(stats)
            .child(div().flex_1())
            .when(!tree.unreachable_hosts.is_empty(), |d| {
                d.child(
                    div()
                        .px(px(6.))
                        .py(px(1.))
                        .rounded(px(4.))
                        .bg(tint(AMBER_400, 0.1))
                        .text_size(px(10.))
                        .text_color(rgb(0xf59e0b))
                        .child(format!("{} 台离线", tree.unreachable_hosts.len())),
                )
            })
            .when(counts.archived > 0 && counts.active > 0, |d| {
                let showing = self.remote_state.descendants.show_archived;
                d.child(
                    div()
                        .id("paseo-descendants-archived-toggle")
                        .role(Role::Button)
                        .px(px(6.))
                        .py(px(1.))
                        .rounded(px(4.))
                        .bg(if showing {
                            content(0.12)
                        } else {
                            content(0.05)
                        })
                        .hover(|s| s.bg(content(0.12)))
                        .text_size(px(10.))
                        .text_color(content(0.6))
                        .cursor(CursorStyle::PointingHand)
                        .child(format!("已归档 {}", counts.archived))
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.remote_state.descendants.show_archived =
                                !this.remote_state.descendants.show_archived;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .id("paseo-descendants-refresh")
                    .role(Role::Button)
                    .aria_label("刷新子对话进度")
                    .size(px(20.))
                    .rounded(px(5.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(content(0.08)))
                    .cursor(CursorStyle::PointingHand)
                    .child(
                        icon("refresh-cw", 11.)
                            .text_color(if loading { accent_color() } else { content(0.5) }),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.refresh_paseo_descendants(true, cx);
                    })),
            );

        card = card.child(header);

        if collapsed {
            return Some(card.into_any_element());
        }

        let mut rows = Vec::new();
        flatten_visible(
            &tree.children,
            0,
            include_archived,
            &self.remote_state.descendants.collapsed_branches,
            &mut rows,
        );

        let body = div()
            .id("paseo-descendants-list")
            .max_h(px(248.))
            .overflow_y_scroll()
            .border_t_1()
            .border_color(content(0.07))
            .py(px(4.))
            .flex()
            .flex_col()
            .children(rows.into_iter().enumerate().map(|(ix, flat)| {
                let node = flat.node;
                let depth = flat.depth;
                let branch_key = flat.branch_key;
                let target = OpenPaseoAgent {
                    server_id: node.server_id.clone(),
                    host_label: node.host_label.clone(),
                    agent_id: node.agent_id.clone(),
                    cwd: node.cwd.clone(),
                    title: node.title.clone(),
                };
                let dot = status_color(&node.status, node.archived);
                let state_text = status_label(&node.status, node.archived);
                let age = format_relative_age(node.updated_at);
                let show_host = !node.host_label.is_empty()
                    && (node.server_id != root_server || node.is_remote);
                let host_text = short_host_label(&node.host_label);
                let repo_text = node.repo_name.clone();
                let title_text = node.title.clone();
                let summary_text = node.summary.clone();
                let summary_color = match node.summary_kind.as_deref() {
                    Some("permission") => gpui::Hsla::from(rgb(0xf59e0b)),
                    Some("error") => gpui::Hsla::from(rgb(0xf43f5e)),
                    Some("tool") => gpui::Hsla::from(rgb(0x7dd3fc)),
                    _ => content(0.52),
                };
                let title_color = if node.archived {
                    content(0.45)
                } else {
                    content(0.9)
                };

                let mut row_content = div()
                    .w_full()
                    .min_w_0()
                    .when(depth > 0, |d| {
                        d.pl(px(10.)).border_l_1().border_color(content(0.12))
                    })
                    .flex()
                    .flex_col()
                    .gap(px(2.));

                let mut top = div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.));

                if flat.visible_children > 0 {
                    let toggle_key = branch_key.clone();
                    top = top.child(
                        div()
                            .id(("paseo-desc-branch", ix))
                            .role(Role::Button)
                            .size(px(16.))
                            .flex_shrink_0()
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(content(0.1)))
                            .cursor(CursorStyle::PointingHand)
                            .child(
                                icon(
                                    if flat.branch_collapsed {
                                        "chevron-right"
                                    } else {
                                        "chevron-down"
                                    },
                                    10.,
                                )
                                .text_color(content(0.55)),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                let set = &mut this.remote_state.descendants.collapsed_branches;
                                if !set.remove(&toggle_key) {
                                    set.insert(toggle_key.clone());
                                }
                                cx.notify();
                            })),
                    );
                }

                top = top
                    .child(div().size(px(7.)).rounded_full().flex_shrink_0().bg(dot))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(12.))
                            .font_weight(if node.archived {
                                gpui::FontWeight::NORMAL
                            } else {
                                gpui::FontWeight::MEDIUM
                            })
                            .text_color(title_color)
                            .child(title_text),
                    )
                    .when(flat.visible_children > 0, |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .px(px(5.))
                                .py(px(1.))
                                .rounded(px(4.))
                                .bg(content(0.06))
                                .text_size(px(10.))
                                .text_color(content(0.55))
                                .child(format!("{} 子", flat.visible_children)),
                        )
                    })
                    .when(show_host, |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .px(px(5.))
                                .py(px(1.))
                                .rounded(px(4.))
                                .bg(content(0.07))
                                .text_size(px(10.))
                                .text_color(content(0.62))
                                .child(host_text),
                        )
                    })
                    .when(!repo_text.is_empty(), |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .px(px(5.))
                                .py(px(1.))
                                .rounded(px(4.))
                                .bg(content(0.04))
                                .text_size(px(10.))
                                .text_color(content(0.48))
                                .child(repo_text),
                        )
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(10.))
                            .text_color(dot)
                            .child(state_text),
                    )
                    .when(!age.is_empty(), |d| {
                        d.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(10.))
                                .text_color(content(0.38))
                                .child(age),
                        )
                    });

                row_content = row_content.child(top);

                if let Some(summary) = summary_text.filter(|s| !s.trim().is_empty()) {
                    row_content = row_content.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .pl(px(if flat.visible_children > 0 { 29. } else { 13. }))
                            .truncate()
                            .text_size(px(11.))
                            .line_height(px(16.))
                            .text_color(summary_color)
                            .child(summary),
                    );
                }

                div()
                    .id(("paseo-desc-row", ix))
                    .debug_selector(move || format!("paseo-desc-row-{ix}"))
                    .role(Role::Button)
                    .flex_shrink_0()
                    .w_full()
                    .pl(px(10. + depth as f32 * 16.))
                    .pr(px(10.))
                    .py(px(5.))
                    .cursor(CursorStyle::PointingHand)
                    .hover(|s| s.bg(content(0.06)))
                    .child(row_content)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(target.clone());
                    }))
            }));

        Some(card.child(body).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_descendant_tree_and_flattens_with_collapse_and_archive_filters() {
        let payload = json!({
            "root": { "serverId": "mac", "agentId": "leader", "hostLabel": "Local Mac" },
            "parent": null,
            "ancestors": [],
            "children": [
                {
                    "serverId": "boe",
                    "hostLabel": "Devbox BOE",
                    "isRemote": true,
                    "agentId": "c3",
                    "title": "[队长3] mall_channel",
                    "cwd": "/work/mall_channel",
                    "repoName": "mall_channel",
                    "status": "running",
                    "archived": false,
                    "summary": "已拉起 2 个孙子",
                    "summaryKind": "assistant",
                    "children": [
                        {
                            "serverId": "boe",
                            "hostLabel": "Devbox BOE",
                            "isRemote": true,
                            "agentId": "g3a",
                            "title": "[孙3A]",
                            "status": "done",
                            "archived": false,
                            "children": []
                        },
                        {
                            "serverId": "boe",
                            "hostLabel": "Devbox BOE",
                            "isRemote": true,
                            "agentId": "g3-arch",
                            "title": "[已归档孙]",
                            "status": "archived",
                            "archived": true,
                            "children": []
                        }
                    ]
                }
            ],
            "counts": { "total": 3, "active": 2, "running": 1, "done": 1, "archived": 1 },
            "unreachableHosts": []
        });
        let tree = parse_tree(payload).unwrap();
        assert_eq!(tree.counts.total, 3);
        let mut rows = Vec::new();
        flatten_visible(&tree.children, 0, false, &HashSet::new(), &mut rows);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].node.agent_id.as_str(), rows[0].depth, rows[0].visible_children), ("c3", 0, 1));
        assert_eq!((rows[1].node.agent_id.as_str(), rows[1].depth), ("g3a", 1));

        let mut collapsed = HashSet::new();
        collapsed.insert("boe:c3".to_owned());
        rows.clear();
        flatten_visible(&tree.children, 0, true, &collapsed, &mut rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].visible_children, 2);
    }
}
