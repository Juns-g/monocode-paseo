//! A Paseo window that a sync reinstalls must not move what the reader sees.
use super::*;
use gpui::{TestAppContext, WindowBounds, WindowOptions, point};
use monocode_core::store::HistoryPage;
use std::sync::atomic::{AtomicUsize, Ordering};

fn row(role: &str, text: &str) -> Row {
    Row {
        role: role.into(),
        text: text.into(),
        first: true,
        sent_at: None,
    }
}

fn tool(ix: usize) -> Row {
    row(
        "tool",
        &serde_json::to_string(&ToolCall {
            id: ix.to_string(),
            name: "Bash".into(),
            input: format!(r#"{{"command":"rg pattern-{ix} src"}}"#),
            output: "match\n".repeat(6),
            state: ToolState::Completed,
            panel: None,
            display: None,
        })
        .unwrap(),
    )
}

/// One host turn: a prompt, a tool call and a long reply.
fn turn(ix: usize) -> Vec<Row> {
    vec![
        row("user", &format!("Question {ix}")),
        tool(ix),
        row(
            "assistant",
            &format!(
                "Answer {ix}.\n\n{}",
                "A long paragraph of prose. ".repeat(40)
            ),
        ),
    ]
}

fn window(turns: std::ops::Range<usize>) -> Vec<Row> {
    turns.flat_map(turn).collect()
}

fn paseo(cx: &mut TestAppContext, paseo: bool) -> gpui::WindowHandle<TranscriptView> {
    cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(900.), px(600.)),
                ))),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| {
                    let mut view = TranscriptView::new();
                    if paseo {
                        view.paseo_mode();
                    }
                    view
                })
            },
        )
        .unwrap()
    })
}

fn render(window: gpui::WindowHandle<TranscriptView>, cx: &mut TestAppContext) {
    window.update(cx, |_, _, cx| cx.notify()).unwrap();
    cx.refresh().unwrap();
    cx.run_until_parked();
}

/// One host block per row, identified by what it says.
fn layout(rows: &[Row]) -> WindowLayout {
    rows.iter()
        .map(|row| {
            (
                format!(
                    "{}/{}",
                    row.role,
                    row.text.chars().take(40).collect::<String>()
                ),
                1,
            )
        })
        .collect()
}

fn install(
    view: &mut TranscriptView,
    rows: Vec<Row>,
    before: Option<i64>,
    cx: &mut Context<TranscriptView>,
) {
    let layout = layout(&rows);
    view.install_remote_window(PreparedHistory::new(rows, false), layout, before, cx);
}

/// A host whose cursors are row numbers of `full`, a few turns per page.
fn pager(full: Vec<Row>, calls: Arc<AtomicUsize>) -> HistoryPager {
    Arc::new(move |before, all| {
        assert!(!all, "a Paseo pager cannot read every page");
        calls.fetch_add(1, Ordering::SeqCst);
        let before = before as usize;
        let start = before.saturating_sub(6);
        Ok(HistoryPage {
            rows: full[start..before].to_vec(),
            turns: HashMap::new(),
            before: (start > 0).then_some(start as i64),
        })
    })
}

/// What the reader sees at the top: the row's content, its part and offset.
fn reading(view: &TranscriptView) -> (String, usize, Pixels) {
    let top = view.list.logical_scroll_top();
    let key = view.rows[top.item_ix].key;
    (view.source[key.0].text.clone(), key.1, top.offset_in_item)
}

#[gpui::test]
fn paseo_window_syncs_keep_the_reading_position(cx: &mut TestAppContext) {
    // Each sync is what the adapter's 1000-entry window really produces.
    let live = || {
        let mut rows = window(0..11);
        rows.extend([row("user", "Question 11"), tool(11)]);
        rows
    };
    // The shrunk window also arrives through the plain installer.
    let cases: [(&str, Vec<Row>, Vec<Row>, bool); 6] = [
        ("identical", window(0..12), window(0..12), false),
        (
            "new tail row",
            window(0..12),
            {
                let mut rows = window(0..12);
                rows.push(row("user", "Question 12"));
                rows
            },
            false,
        ),
        ("shrunk", window(0..12), window(0..12)[..35].to_vec(), false),
        (
            "shrunk, plain install",
            window(0..12),
            window(0..12)[..35].to_vec(),
            true,
        ),
        ("slid by one turn", window(0..12), window(1..13), false),
        ("turn end folds its work", live(), window(0..12), false),
    ];
    let mut failures = Vec::new();
    for (name, first, next, plain) in cases {
        let anchor = 15;
        let window_handle = paseo(cx, true);
        window_handle
            .update(cx, |view, _, cx| {
                view.visible = false;
                install(view, first, None, cx);
                view.visible = true;
            })
            .unwrap();
        render(window_handle, cx);
        let before = window_handle
            .update(cx, |view, _, _| {
                let at = view.rows.iter().position(|r| r.key.0 == anchor).unwrap();
                view.list.scroll_to(ListOffset {
                    item_ix: at,
                    offset_in_item: px(13.),
                });
                reading(view)
            })
            .unwrap();
        render(window_handle, cx);
        window_handle
            .update(cx, |view, _, cx| {
                if plain {
                    view.install_prepared_history(PreparedHistory::new(next, false), cx)
                } else {
                    install(view, next, None, cx)
                }
            })
            .unwrap();
        render(window_handle, cx);
        let after = window_handle
            .update(cx, |view, _, _| reading(view))
            .unwrap();
        if after != before {
            failures.push(format!("{name}: {before:?} -> {after:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "reading position moved:\n{}",
        failures.join("\n")
    );
}

/// Open a window over `full[from..]` at the top of which `calls` pages load.
fn paged(
    cx: &mut TestAppContext,
    full: &[Row],
    from: usize,
    calls: &Arc<AtomicUsize>,
) -> gpui::WindowHandle<TranscriptView> {
    let handle = paseo(cx, true);
    let full = full.to_vec();
    let calls = calls.clone();
    handle
        .update(cx, |view, _, cx| {
            view.visible = false;
            install(view, full[from..].to_vec(), Some(from as i64), cx);
            view.set_remote_pager(Some(pager(full, calls)));
            view.visible = true;
        })
        .unwrap();
    render(handle, cx);
    handle
}

/// Scroll to the row at source index `key`, 13px in, and read it back.
fn read_at(
    handle: gpui::WindowHandle<TranscriptView>,
    cx: &mut TestAppContext,
    key: usize,
) -> (String, usize, Pixels) {
    let reading = handle
        .update(cx, |view, _, _| {
            let at = view.rows.iter().position(|r| r.key.0 == key).unwrap();
            view.list.pause_following_tail();
            view.list.scroll_to(ListOffset {
                item_ix: at,
                offset_in_item: px(13.),
            });
            reading(view)
        })
        .unwrap();
    render(handle, cx);
    reading
}

fn texts(view: &TranscriptView) -> Vec<String> {
    view.history_rows()
        .iter()
        .map(|row| row.text.clone())
        .collect()
}

#[gpui::test]
fn a_sync_after_an_older_page_keeps_every_row_once_and_the_anchor(cx: &mut TestAppContext) {
    let full = window(0..17);
    let calls = Arc::new(AtomicUsize::new(0));
    // The host window is turns 5.., so the cursor is row 15.
    let handle = paged(cx, &full, 15, &calls);
    let anchor = read_at(handle, cx, 15 + 5);
    handle
        .update(cx, |view, _, cx| view.load_older_history(cx))
        .unwrap();
    render(handle, cx);
    handle
        .update(cx, |view, _, _| {
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(view.base, 6);
            assert_eq!(view.history_rows(), &full[9..]);
            assert_eq!(view.history_before, Some(9));
            assert_eq!(reading(view), anchor, "the page keeps the anchor");
        })
        .unwrap();

    let mut next = full[15..].to_vec();
    next.push(row("user", "Question 17"));
    handle
        .update(cx, |view, _, cx| install(view, next.clone(), Some(15), cx))
        .unwrap();
    render(handle, cx);
    handle
        .update(cx, |view, _, _| {
            let mut expected: Vec<_> = full[9..].iter().map(|row| row.text.clone()).collect();
            expected.push("Question 17".into());
            assert_eq!(texts(view), expected, "every row once");
            assert_eq!(view.source_rows(), &next[..]);
            assert_eq!(view.base, 6);
            assert_eq!(
                view.history_before,
                Some(9),
                "the cursor stays above the page"
            );
            assert_eq!(view.window_before, Some(15));
            assert_eq!(reading(view), anchor);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        })
        .unwrap();
}

#[gpui::test]
fn a_slide_after_an_older_page_keeps_page_slid_rows_and_window_contiguous(cx: &mut TestAppContext) {
    let full = window(0..18);
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = paged(cx, &full[..], 12, &calls);
    let anchor = read_at(handle, cx, 12 + 15);
    handle
        .update(cx, |view, _, cx| view.load_older_history(cx))
        .unwrap();
    render(handle, cx);
    // The host window moves on by two turns (six rows).
    let next = full[18..].to_vec();
    handle
        .update(cx, |view, _, cx| {
            assert_eq!(view.base, 6);
            install(view, next.clone(), Some(18), cx)
        })
        .unwrap();
    render(handle, cx);
    handle
        .update(cx, |view, _, _| {
            assert_eq!(view.base, 12, "six rows slid above the window");
            assert_eq!(
                view.history_rows(),
                &full[6..],
                "page, slid rows and window"
            );
            assert_eq!(view.source_rows(), &next[..]);
            assert_eq!(
                view.history_before,
                Some(6),
                "the cursor stays above the page"
            );
            assert_eq!(reading(view), anchor);
        })
        .unwrap();

    // A window that shares nothing with the old one is a new epoch.
    let other: Vec<_> = (0..7)
        .map(|ix| row("assistant", &format!("Other {ix}")))
        .collect();
    handle
        .update(cx, |view, _, cx| view.load_older_history(cx))
        .unwrap();
    handle
        .update(cx, |view, _, cx| {
            assert!(view.history_loading);
            // The same cursor, as a fresh epoch can reuse one.
            install(view, other.clone(), Some(6), cx)
        })
        .unwrap();
    // Only the page in flight is read here; a frame may ask for the next.
    cx.run_until_parked();
    handle
        .update(cx, |view, _, _| {
            assert_eq!(view.base, 0);
            assert_eq!(view.history_before, Some(6));
            assert_eq!(view.history_rows(), &other[..]);
            assert!(!view.history_loading, "the page in flight is discarded");
        })
        .unwrap();
}

#[gpui::test]
fn a_live_slide_aligns_the_window_before_its_patch(cx: &mut TestAppContext) {
    let handle = paseo(cx, true);
    handle
        .update(cx, |view, _, cx| {
            view.visible = false;
            install(view, window(0..12), Some(40), cx);
            view.visible = true;
        })
        .unwrap();
    render(handle, cx);
    let anchor = read_at(handle, cx, 15);
    let next = window(1..13);
    handle
        .update(cx, |view, _, cx| {
            view.align_remote_window(layout(&next));
            assert_eq!(view.base, 3);
            assert_eq!(view.history_before, Some(40), "the cursor stays above");
            view.patch(0, next.clone(), cx);
        })
        .unwrap();
    render(handle, cx);
    handle
        .update(cx, |view, _, _| {
            assert_eq!(view.source_rows(), &next[..]);
            assert_eq!(view.history_rows().len(), 39);
            assert_eq!(reading(view), anchor);
        })
        .unwrap();
}

/// Switching back to an evicted remote conversation reinstalls its window
/// the way the remote reload does; the reader returns where they left it.
#[gpui::test]
fn a_reloaded_remote_window_restores_the_suspended_reading_position(cx: &mut TestAppContext) {
    let mut failures = Vec::new();
    for (path, paseo_mode) in [("ordinary", false), ("paseo", true), ("paged", true)] {
        for following in [false, true] {
            for grown in [false, true] {
                let name = format!("{path} following={following} grown={grown}");
                let handle = paseo(cx, paseo_mode);
                let reinstall = |view: &mut TranscriptView,
                                 rows: Vec<Row>,
                                 cx: &mut Context<TranscriptView>| {
                    if path == "paged" {
                        install(view, rows, None, cx);
                    } else {
                        view.install_prepared_history(PreparedHistory::new(rows, false), cx);
                    }
                };
                handle
                    .update(cx, |view, _, cx| {
                        view.visible = false;
                        reinstall(view, window(0..12), cx);
                        view.visible = true;
                    })
                    .unwrap();
                render(handle, cx);
                if !following {
                    handle
                        .update(cx, |view, _, _| {
                            let at = view.rows.iter().position(|r| r.key.0 == 15).unwrap();
                            view.list.scroll_to(ListOffset {
                                item_ix: at,
                                offset_in_item: px(13.),
                            });
                        })
                        .unwrap();
                    render(handle, cx);
                }
                let before = handle
                    .update(cx, |view, _, _| {
                        (reading(view), view.list.is_following_tail())
                    })
                    .unwrap();
                handle
                    .update(cx, |view, _, cx| {
                        view.visible = false;
                        assert!(view.suspend_history(cx), "{name}: evictable");
                        view.visible = true;
                        let mut rows = window(0..12);
                        if grown {
                            rows.push(row("user", "Question 12"));
                        }
                        reinstall(view, rows, cx);
                    })
                    .unwrap();
                render(handle, cx);
                let (after, pending) = handle
                    .update(cx, |view, _, _| {
                        (
                            (reading(view), view.list.is_following_tail()),
                            view.history_position.is_some(),
                        )
                    })
                    .unwrap();
                // A followed tail follows the grown tail; a read row stays put.
                let moved = if following { !after.1 } else { after != before };
                if moved || pending {
                    failures.push(format!(
                        "{name}: {before:?} -> {after:?}, position pending={pending}"
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "reload moved the reader:\n{}",
        failures.join("\n")
    );
}
