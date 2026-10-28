use super::*;
use gpui::{TestAppContext, WindowBounds, WindowOptions, point};

fn row(role: &str, text: &str) -> Row {
    Row {
        role: role.into(),
        text: text.into(),
        first: true,
        sent_at: None,
    }
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    Chat,
    Mono,
}

fn open(cx: &mut TestAppContext, layout: Layout) -> gpui::WindowHandle<TranscriptView> {
    cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(900.), px(600.)),
                ))),
                ..Default::default()
            },
            move |_, cx| {
                cx.new(|_| {
                    let mut view = TranscriptView::new();
                    match layout {
                        Layout::Chat => {}
                        Layout::Mono => view.mono_mode("cat", 0xe86b5f, "MonoCat"),
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

/// The display index of the turn's WorkLine, keyed where its first row lands.
fn work_line(view: &TranscriptView, turn: usize) -> Option<usize> {
    view.rows.iter().position(|row| {
        row.key == (turn, 0) && matches!(row.body, Body::WorkLine { turn: t, .. } if t == turn)
    })
}

fn earlier() -> Vec<Row> {
    vec![row("user", "Earlier"), row("assistant", "Earlier answer")]
}

/// AgentTranscript's fold line is live from before there is any work: a
/// started turn whose host has only acknowledged the prompt shows it too.
#[gpui::test]
fn a_started_turn_shows_its_live_line_before_the_first_response_row(cx: &mut TestAppContext) {
    for layout in [Layout::Chat, Layout::Mono] {
        let window = open(cx, layout);
        window
            .update(cx, |view, _, cx| {
                view.identity("codex", "Live Model");
                view.patch(0, earlier(), cx);
                view.streaming(true, cx);
                // The host receipt: the prompt, and no response row yet.
                view.patch(2, vec![row("user", "Next")], cx);
                let at = work_line(view, 3)
                    .unwrap_or_else(|| panic!("{layout:?}: no live WorkLine under the prompt"));
                assert!(
                    view.rows[..at].iter().any(|r| r.key.0 == 2),
                    "{layout:?}: the line sits under the prompt"
                );
                assert!(view.work_line_live(3), "{layout:?}: the line is live");
            })
            .unwrap();
        render(window, cx);
        if matches!(layout, Layout::Mono) {
            let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
            assert!(
                visual.debug_bounds("mono-turn-ticker").is_some(),
                "a Mono's header says it is thinking"
            );
        }
        window
            .update(cx, |view, _, cx| {
                let before = work_line(view, 3).unwrap();
                view.patch(3, vec![row("reasoning", "Looking around")], cx);
                assert_eq!(
                    work_line(view, 3),
                    Some(before),
                    "{layout:?}: the first row keeps the same line in place"
                );
                assert!(view.work_line_live(3));
            })
            .unwrap();
    }
}

/// Reopening a running host turn attaches after its prompt is already stored.
#[gpui::test]
fn attaching_to_a_running_turn_shows_its_live_line(cx: &mut TestAppContext) {
    for layout in [Layout::Chat, Layout::Mono] {
        let window = open(cx, layout);
        window
            .update(cx, |view, _, cx| {
                let mut rows = earlier();
                rows.push(row("user", "Still running"));
                view.patch(0, rows, cx);
                assert_eq!(work_line(view, 3), None, "a settled prompt has no line");
                view.streaming(true, cx);
                assert!(
                    work_line(view, 3).is_some(),
                    "{layout:?}: attach shows the line"
                );
                assert!(view.work_line_live(3));
            })
            .unwrap();
        render(window, cx);
        if matches!(layout, Layout::Mono) {
            let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
            assert!(visual.debug_bounds("mono-turn-ticker").is_some());
        }
    }
}

/// The placeholder belongs to the live turn only. A turn that ends without any
/// response row, and the previous turn while a new prompt is in transit, keep
/// their settled layout.
#[gpui::test]
fn the_live_line_leaves_with_the_turn_and_never_reopens_the_previous_one(cx: &mut TestAppContext) {
    for layout in [Layout::Chat, Layout::Mono] {
        let window = open(cx, layout);
        window
            .update(cx, |view, _, cx| {
                view.patch(0, earlier(), cx);
                let settled: Vec<_> = view.rows.iter().map(|r| r.key).collect();
                view.streaming(true, cx);
                assert_eq!(
                    view.rows.iter().map(|r| r.key).collect::<Vec<_>>(),
                    settled,
                    "{layout:?}: no line before the prompt arrives"
                );
                assert!(!view.work_line_live(1));
                view.patch(2, vec![row("user", "Next")], cx);
                assert!(work_line(view, 3).is_some());
                view.streaming(false, cx);
                assert_eq!(
                    work_line(view, 3),
                    None,
                    "{layout:?}: the line ends with the turn"
                );
                assert!(view.rows.iter().all(|r| r.key.0 < 3));
            })
            .unwrap();
        render(window, cx);
    }
}

/// The optimistic send path already has a response row; it keeps one line.
#[gpui::test]
fn an_optimistic_prompt_keeps_a_single_live_line(cx: &mut TestAppContext) {
    for layout in [Layout::Chat, Layout::Mono] {
        let window = open(cx, layout);
        window
            .update(cx, |view, _, cx| {
                view.patch(0, earlier(), cx);
                view.streaming(true, cx);
                view.submit_prompt("Next", &Default::default(), cx);
                let lines = || {
                    view.rows
                        .iter()
                        .filter(|r| matches!(r.body, Body::WorkLine { turn: 3, .. }))
                        .count()
                };
                assert_eq!(lines(), 1, "{layout:?}");
                assert!(view.work_line_live(3));
            })
            .unwrap();
        render(window, cx);
    }
}
