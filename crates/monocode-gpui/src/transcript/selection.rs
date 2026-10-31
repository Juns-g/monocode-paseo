//! Select the rendered text, retaining byte anchors instead of shaped history.
use super::*;
use gpui::{
    AnyElement, App, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId,
    MouseButton, Point, TextLayout, WeakEntity,
};
use std::{cell::RefCell, ops::Range, rc::Rc};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Position {
    row: (usize, usize),
    byte: usize,
}
#[derive(Clone, Copy)]
enum Granularity {
    Character,
    Word,
    Paragraph,
}
struct Drag {
    origin: Point<Pixels>,
    anchor: Range<Position>,
    granularity: Granularity,
    moved: bool,
}
#[derive(Default)]
struct State {
    anchor: Option<Position>,
    head: Option<Position>,
    response: usize,
    quote: bool,
    drag: Option<Drag>,
    menu: Option<Point<Pixels>>,
    copied: Option<std::time::Instant>,
    layouts: HashMap<((usize, usize), usize, usize), Fragment>,
}
struct Fragment {
    row: (usize, usize),
    response: usize,
    offset: usize,
    text: SharedString,
    row_text: SharedString,
    layout: TextLayout,
}
#[derive(Clone, Default)]
pub(super) struct Selection(Rc<RefCell<State>>);
#[derive(Clone)]
pub(crate) struct TextSelection {
    state: Selection,
    view: WeakEntity<TranscriptView>,
    row: (usize, usize),
    response: usize,
    offset: usize,
    quote: bool,
    row_text: Option<SharedString>,
    cursor: gpui::CursorStyle,
}
impl Selection {
    pub(super) fn hide_menu(&self) {
        let mut state = self.0.borrow_mut();
        state.menu = None;
        state.drag = None;
    }
    pub(super) fn clear_if_not_dragging(&self) {
        let dragging = self.0.borrow().drag.is_some();
        if !dragging {
            self.clear();
        }
    }
    pub(super) fn active(&self) -> bool {
        let state = self.0.borrow();
        state.drag.is_some() || state.menu.is_some() || state.anchor != state.head
    }
    pub(super) fn clear(&self) {
        let mut state = self.0.borrow_mut();
        state.anchor = None;
        state.head = None;
        state.drag = None;
        state.menu = None;
        state.copied = None;
        state.layouts.clear();
    }
    pub(super) fn begin_frame(&self) {
        self.0.borrow_mut().layouts.clear();
    }
    fn range(
        &self,
        row: (usize, usize),
        response: usize,
        offset: usize,
        len: usize,
    ) -> Range<usize> {
        let s = self.0.borrow();
        let (Some(a), Some(b)) = (s.anchor, s.head) else {
            return 0..0;
        };
        if response != s.response {
            return 0..0;
        }
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if row < a.row || row > b.row {
            return 0..0;
        }
        let start = if row == a.row {
            a.byte.saturating_sub(offset).min(len)
        } else {
            0
        };
        let end = if row == b.row {
            b.byte.saturating_sub(offset).min(len)
        } else {
            len
        };
        start.min(end)..end
    }
}
const MENU_DELAY: std::time::Duration = std::time::Duration::from_millis(300);
const COPIED_DELAY: std::time::Duration = std::time::Duration::from_millis(700);
/// The top centre of the selection's first line, as React anchors to the
/// range's first client rect rather than the pointer.
fn menu_anchor(state: &State) -> Option<Point<Pixels>> {
    let (a, b) = (state.anchor?, state.head?);
    let (start, end) = if a < b { (a, b) } else { (b, a) };
    let fragment = state.layouts.values().find(|f| {
        f.response == state.response
            && f.row == start.row
            && f.offset <= start.byte
            && start.byte < f.offset + f.text.len()
    })?;
    let top = fragment
        .layout
        .position_for_index(start.byte - fragment.offset)?;
    let right = (end.row == start.row && end.byte <= fragment.offset + fragment.text.len())
        .then(|| {
            fragment
                .layout
                .position_for_index(end.byte - fragment.offset)
        })
        .flatten()
        .filter(|p| p.y == top.y)
        .map_or(fragment.layout.bounds().right(), |p| p.x);
    Some(gpui::point((top.x + right) / 2., top.y))
}
fn snap(text: &str, byte: usize) -> usize {
    if byte >= text.len() {
        return text.len();
    }
    text.grapheme_indices(true)
        .map(|(ix, _)| ix)
        .take_while(|ix| *ix <= byte)
        .last()
        .unwrap_or(0)
}
fn word(text: &str, byte: usize) -> Range<usize> {
    let at = if byte >= text.len() {
        text.grapheme_indices(true)
            .next_back()
            .map_or(0, |(ix, _)| ix)
    } else {
        snap(text, byte)
    };
    text.split_word_bound_indices()
        .find(|(ix, s)| *ix <= at && at < *ix + s.len())
        .map(|(ix, s)| ix..ix + s.len())
        .unwrap_or(at..at)
}
fn unit(text: &str, byte: usize, granularity: Granularity) -> Range<usize> {
    let byte = snap(text, byte);
    match granularity {
        Granularity::Character => byte..byte,
        Granularity::Word => word(text, byte),
        Granularity::Paragraph => {
            let start = text[..byte].rfind(['\n', '\t']).map_or(0, |ix| ix + 1);
            let end = text[byte..]
                .find(['\n', '\t'])
                .map_or(text.len(), |ix| byte + ix);
            start..end
        }
    }
}
impl TextSelection {
    pub(crate) fn with_text(mut self, text: SharedString) -> Self {
        self.row_text = Some(text);
        self
    }
    pub(crate) fn with_cursor(mut self, cursor: gpui::CursorStyle) -> Self {
        self.cursor = cursor;
        self
    }
    pub(crate) fn offset(&self, offset: usize) -> Self {
        let mut next = self.clone();
        next.offset += offset;
        next
    }
    pub(crate) fn has_selection(&self) -> bool {
        let state = self.state.0.borrow();
        state.anchor != state.head || state.drag.as_ref().is_some_and(|drag| drag.moved)
    }
    pub(crate) fn wrap(
        &self,
        text: SharedString,
        layout: TextLayout,
        child: AnyElement,
    ) -> AnyElement {
        let selection = self.clone();
        let click_layout = layout.clone();
        let click_text = text.clone();
        let row_text = self.row_text.clone().unwrap_or_else(|| text.clone());
        div()
            .relative()
            .cursor(self.cursor)
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                let byte = click_layout
                    .index_for_position(event.position)
                    .unwrap_or_else(|ix| ix);
                let byte = snap(&click_text, byte);
                let granularity = if event.click_count >= 3 {
                    Granularity::Paragraph
                } else if event.click_count == 2 {
                    Granularity::Word
                } else {
                    Granularity::Character
                };
                let range = unit(&row_text, selection.offset + byte, granularity);
                let _ = selection.view.update(cx, |view, cx| {
                    let mut state = selection.state.0.borrow_mut();
                    let start = Position {
                        row: selection.row,
                        byte: range.start,
                    };
                    let end = Position {
                        row: selection.row,
                        byte: range.end,
                    };
                    let extend = event.modifiers.shift
                        && state.anchor.is_some()
                        && state.response == selection.response;
                    let anchor = if extend {
                        let anchor = state.anchor.unwrap();
                        anchor..anchor
                    } else {
                        start..end
                    };
                    state.anchor = Some(anchor.start);
                    state.head = Some(if start < anchor.start { start } else { end });
                    state.response = selection.response;
                    state.quote = selection.quote;
                    state.drag = Some(Drag {
                        origin: event.position,
                        anchor,
                        granularity,
                        moved: false,
                    });
                    state.menu = None;
                    state.copied = None;
                    drop(state);
                    if let Some(focus) = &view.selection_focus {
                        window.focus(focus, cx)
                    }
                    cx.notify();
                });
            })
            .child(SelectedText {
                context: self.clone(),
                text,
                layout,
                child,
            })
            .into_any_element()
    }
}
struct SelectedText {
    context: TextSelection,
    text: SharedString,
    layout: TextLayout,
    child: AnyElement,
}
impl IntoElement for SelectedText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for SelectedText {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
        self.context.state.0.borrow_mut().layouts.insert(
            (self.context.row, self.context.response, self.context.offset),
            Fragment {
                row: self.context.row,
                response: self.context.response,
                offset: self.context.offset,
                text: self.text.clone(),
                row_text: self
                    .context
                    .row_text
                    .clone()
                    .unwrap_or_else(|| self.text.clone()),
                layout: self.layout.clone(),
            },
        );
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let selected = self.context.state.range(
            self.context.row,
            self.context.response,
            self.context.offset,
            self.text.len(),
        );
        if !selected.is_empty() {
            let bounds = self.layout.bounds();
            let height = self.layout.line_height();
            if let (Some(a), Some(b)) = (
                self.layout.position_for_index(selected.start),
                self.layout.position_for_index(selected.end),
            ) {
                let mut y = a.y;
                while y <= b.y {
                    let left = if y == a.y { a.x } else { bounds.left() };
                    let right = if y == b.y { b.x } else { bounds.right() };
                    if right > left {
                        window.paint_quad(gpui::fill(
                            Bounds::new(gpui::point(left, y), size(right - left, height)),
                            crate::theme::text_selection(),
                        ))
                    }
                    y += height;
                }
            }
        }
        self.child.paint(window, cx);
    }
}
impl TranscriptView {
    pub(super) fn selection_turn(&self) -> Option<usize> {
        let state = self.selection.0.borrow();
        state.menu?;
        self.messages
            .iter()
            .find(|m| m.start == state.response)
            .map(|m| m.turn)
    }
    pub(super) fn text_selection(
        &self,
        row: (usize, usize),
        response: usize,
        quote: bool,
        cx: &Context<Self>,
    ) -> Option<TextSelection> {
        (!self.document).then(|| TextSelection {
            state: self.selection.clone(),
            view: cx.weak_entity(),
            row,
            response,
            offset: 0,
            quote,
            row_text: None,
            cursor: gpui::CursorStyle::IBeam,
        })
    }
    pub(super) fn move_selection(
        &mut self,
        event: &gpui::MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut state = self.selection.0.borrow_mut();
        let Some(drag) = state.drag.as_mut() else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            state.drag = None;
            return;
        }
        if !drag.moved {
            let delta = event.position - drag.origin;
            if delta.x.abs().max(delta.y.abs()) < px(3.) {
                return;
            }
            drag.moved = true;
        }
        let anchor = drag.anchor.clone();
        let granularity = drag.granularity;
        let response = state.response;
        let hit = state
            .layouts
            .values()
            .filter(|f| f.response == response)
            .min_by(|a, b| {
                let distance = |f: &Fragment| {
                    let bounds = f.layout.bounds();
                    let dx = (bounds.left() - event.position.x)
                        .max(px(0.))
                        .max(event.position.x - bounds.right());
                    let dy = (bounds.top() - event.position.y)
                        .max(px(0.))
                        .max(event.position.y - bounds.bottom());
                    f32::from(dy) * 10000. + f32::from(dx)
                };
                distance(a).total_cmp(&distance(b))
            });
        if let Some(fragment) = hit {
            let byte = fragment
                .layout
                .index_for_position(event.position)
                .unwrap_or_else(|ix| ix);
            let range = unit(&fragment.row_text, fragment.offset + byte, granularity);
            let start = Position {
                row: fragment.row,
                byte: range.start,
            };
            let end = Position {
                row: fragment.row,
                byte: range.end,
            };
            let (anchor, head) = if start < anchor.start {
                (anchor.end, start)
            } else {
                (anchor.start, end)
            };
            if state.anchor != Some(anchor) || state.head != Some(head) {
                state.anchor = Some(anchor);
                state.head = Some(head);
                drop(state);
                cx.notify()
            }
        }
    }
    pub(super) fn finish_selection(
        &mut self,
        event: &gpui::MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut state = self.selection.0.borrow_mut();
        if state.drag.take().is_none() {
            return;
        }
        state.menu = None;
        let (anchor, head) = (state.anchor, state.head);
        drop(state);
        cx.notify();
        if anchor == head {
            return;
        }
        // Wait out a quick select-and-go before offering the menu, and drop
        // it if the selection moved on in the meantime.
        let fallback = event.position;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MENU_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                let mut state = this.selection.0.borrow_mut();
                if state.drag.is_some() || state.anchor != anchor || state.head != head {
                    return;
                }
                state.menu = Some(menu_anchor(&state).unwrap_or(fallback));
                drop(state);
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn selected_text(&self) -> Option<String> {
        let state = self.selection.0.borrow();
        let (a, b) = (state.anchor?, state.head?);
        if a == b {
            return None;
        }
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        let mut parts = Vec::new();
        for row in self.rows.iter().filter(|row| {
            row.key >= a.row
                && row.key <= b.row
                && self.messages[row.message].start == state.response
        }) {
            let text = match &row.body {
                Body::Markdown(md) => crate::markdown::selection_text(md),
                Body::Plain(text) => text.to_string(),
                Body::Interjection(note) => note.text.clone(),
                _ => continue,
            };
            let start = if row.key == a.row {
                snap(&text, a.byte)
            } else {
                0
            };
            let end = if row.key == b.row {
                snap(&text, b.byte)
            } else {
                text.len()
            };
            if start <= end {
                parts.push(text[start..end].to_owned())
            }
        }
        let text = parts.join("\n").trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
    pub(super) fn copy_selection(
        &mut self,
        _: &crate::input::Copy,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            cx.stop_propagation()
        }
    }
    /// Copy from the menu, show "Copied" briefly, then close it.
    fn copy_menu_selection(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let born = std::time::Instant::now();
        self.selection.0.borrow_mut().copied = Some(born);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.selection.0.borrow().copied == Some(born) {
                    this.selection.clear();
                    cx.notify();
                }
            });
        })
        .detach();
    }
    pub(super) fn selection_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.selection.0.borrow();
        let position = state.menu?;
        let quote = state.quote;
        let copied = state.copied.is_some();
        let response = state.response;
        drop(state);
        let text = self.selected_text()?;
        let copy = text.clone();
        let note = text.clone();
        let turn = self
            .messages
            .iter()
            .find(|m| m.start == response)
            .map_or(response, |m| m.turn);
        let button = |id: &'static str, label: &'static str, glyph: &'static str| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(label)
                .flex()
                .items_center()
                .gap_2()
                .h(px(32.))
                .px_2p5()
                .rounded(px(8.))
                .text_size(px(13.))
                .hover(|s| s.bg(content(0.05)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(icon(glyph, 14.))
                .child(label)
        };
        let panel = div()
            .id("transcript-selection-actions")
            .role(Role::Toolbar)
            .aria_label("Selected text actions")
            .p_1()
            .min_w(px(144.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                button(
                    "selection-copy",
                    if copied { "Copied" } else { "Copy" },
                    if copied { "check" } else { "copy" },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.copy_menu_selection(copy.clone(), cx)
                })),
            )
            .when(quote, |d| {
                d.child(
                    button("selection-chat", "Add to chat", "comment-add").on_click(cx.listener(
                        move |this, _, window, cx| {
                            if this.mono {
                                cx.emit(MonoResponseAction::Quote {
                                    text: text.clone(),
                                    window: window.window_handle(),
                                })
                            } else {
                                cx.emit(ResponseRequest::QuoteSelection { text: text.clone() })
                            }
                            this.selection.clear();
                            cx.notify();
                        },
                    )),
                )
            })
            .when(quote, |d| {
                d.child(
                    button("selection-note", "Add to notes", "response-note").on_click(
                        cx.listener(move |this, _, _, cx| {
                            if this.note_pending.insert(turn) {
                                cx.emit(ResponseRequest::SaveNote {
                                    turn,
                                    text: note.clone(),
                                    selection: true,
                                });
                                cx.notify()
                            }
                        }),
                    ),
                )
            })
            .when_some(
                self.note_feedback
                    .get(&turn)
                    .and_then(|result| result.as_ref().err()),
                |d, error| {
                    d.child(
                        div()
                            .id("selection-note-error")
                            .role(Role::Alert)
                            .max_w(px(280.))
                            .px_2p5()
                            .py_1()
                            .text_size(px(11.))
                            .text_color(rgb(0xf87171))
                            .child(format!("Could not save note: {error}")),
                    )
                },
            );
        let weak = cx.weak_entity();
        Some(
            popover("transcript-selection", Anchor::Point(position))
                .allow_outside_press()
                .side(Side::Top)
                .align(crate::popover::Align::Center)
                .child(panel)
                .on_dismiss(move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.selection.0.borrow_mut().menu = None;
                        cx.notify()
                    });
                })
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    #[gpui::test]
    fn rendered_selection_crosses_markdown_and_code_without_chrome_or_other_responses(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(|_| TranscriptView::new());
        view.update(cx,|view,cx|{
            view.patch(0,vec![Row{role:"assistant".into(),text:"Hello **café**.\n\n```rs\nlet x = 1;\n```\n\nTail".into(),first:true,sent_at:None},Row{role:"user".into(),text:"Next".into(),first:true,sent_at:None},Row{role:"assistant".into(),text:"Other response".into(),first:true,sent_at:None}],cx);
            let first=view.rows.iter().find(|row|matches!(&row.body,Body::Markdown(md) if crate::markdown::selection_text(md)=="Hello café.")).unwrap().key;
            let code=view.rows.iter().find(|row|matches!(&row.body,Body::Markdown(md) if matches!(md.part,Part::Code{..}))).unwrap().key;
            {let mut selection=view.selection.0.borrow_mut();selection.response=0;selection.quote=true;selection.anchor=Some(Position{row:first,byte:6});selection.head=Some(Position{row:code,byte:10});}
            assert_eq!(view.selected_text().as_deref(),Some("café.\nlet x = 1;"));
            {let mut selection=view.selection.0.borrow_mut();let anchor=selection.anchor;selection.anchor=selection.head;selection.head=anchor;}
            assert_eq!(view.selected_text().as_deref(),Some("café.\nlet x = 1;"));
            view.selection.clear();assert!(view.selected_text().is_none());
        });
    }
    fn select_hello(view: &mut TranscriptView, cx: &mut Context<TranscriptView>) -> ((usize, usize), String) {
        view.patch(0,vec![Row{role:"assistant".into(),text:"Hello **café**.".into(),first:true,sent_at:None}],cx);
        let key=view.rows.iter().find(|row|matches!(&row.body,Body::Markdown(_))).unwrap().key;
        {let mut selection=view.selection.0.borrow_mut();selection.response=0;selection.quote=true;selection.anchor=Some(Position{row:key,byte:0});selection.head=Some(Position{row:key,byte:5});selection.menu=Some(gpui::point(px(10.),px(10.)));}
        (key,view.selected_text().unwrap())
    }
    #[gpui::test]
    fn menu_copy_shows_copied_then_closes_the_menu(cx: &mut TestAppContext) {
        let view = cx.new(|_| TranscriptView::new());
        view.update(cx, |view, cx| {
            let (_, text) = select_hello(view, cx);
            assert_eq!(text, "Hello");
            view.copy_menu_selection(text, cx);
            assert!(view.selection.0.borrow().copied.is_some());
            assert!(view.selection.0.borrow().menu.is_some());
            assert!(view.selection_menu(cx).is_some());
        });
        assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some("Hello"));
        cx.executor().advance_clock(COPIED_DELAY / 2);
        cx.run_until_parked();
        view.update(cx, |view, _| {
            assert!(view.selection.0.borrow().copied.is_some());
            assert!(view.selection.0.borrow().menu.is_some());
        });
        cx.executor().advance_clock(COPIED_DELAY);
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            let state = view.selection.0.borrow();
            assert!(state.copied.is_none() && state.menu.is_none() && state.anchor.is_none());
            drop(state);
            assert!(view.selected_text().is_none());
            assert!(view.selection_menu(cx).is_none());
        });
    }
    #[gpui::test]
    fn menu_copy_timer_leaves_a_newer_selection_alone(cx: &mut TestAppContext) {
        let view = cx.new(|_| TranscriptView::new());
        view.update(cx, |view, cx| {
            let (key, text) = select_hello(view, cx);
            view.copy_menu_selection(text, cx);
            // A new press resets the feedback and selects another range.
            let mut state = view.selection.0.borrow_mut();
            state.copied = None;
            state.menu = None;
            state.anchor = Some(Position { row: key, byte: 6 });
            state.head = Some(Position { row: key, byte: 10 });
        });
        cx.executor().advance_clock(COPIED_DELAY * 2);
        cx.run_until_parked();
        view.update(cx, |view, _| assert!(view.selection.0.borrow().anchor.is_some()));
    }
    fn link_window(cx: &mut TestAppContext, text: &str) -> gpui::WindowHandle<TranscriptView> {
        let window = cx.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(0.), px(0.)),
                        size(px(900.), px(600.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| TranscriptView::new()),
            )
            .unwrap()
        });
        window
            .update(cx, |view, _, cx| {
                view.smooth_reveal = false;
                view.patch(0, vec![Row { role: "assistant".into(), text: text.into(), first: true, sent_at: None }], cx)
            })
            .unwrap();
        window.update(cx, |_, _, cx| cx.notify()).unwrap();
        cx.refresh().unwrap();
        cx.run_until_parked();
        window
    }
    #[gpui::test]
    fn clicking_a_markdown_link_in_an_answer_opens_it(cx: &mut TestAppContext) {
        let window = link_window(cx, "[docs](https://example.com/x) tail");
        let bounds = window
            .update(cx, |view, _, _| view.selection.0.borrow().layouts.values().next().unwrap().layout.bounds())
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let at = gpui::point(bounds.left() + px(8.), bounds.center().y);
        visual.simulate_click(at, Default::default());
        assert_eq!(visual.opened_url().as_deref(), Some("https://example.com/x"));
    }
    #[gpui::test]
    fn clicking_links_in_every_markdown_layout_opens_them(cx: &mut TestAppContext) {
        for (mono, hover_first, text) in [
            (false, true, "[docs](https://example.com/x) tail"),
            (true, true, "[docs](https://example.com/x) tail"),
            (false, true, "- one\n- [docs](https://example.com/x)"),
            (false, true, "# [docs](https://example.com/x)"),
            (false, true, "> [docs](https://example.com/x)"),
            (false, true, "| a | b |\n|---|---|\n| [docs](https://example.com/x) | c |"),
            (false, true, "see `code` and [docs](https://example.com/x) tail"),
            (false, true, "first\n\nsecond [docs](https://example.com/x)"),
        ] {
            let window = link_window(cx, text);
            if mono {
                window.update(cx, |view, _, _| view.mono_mode("cat", 0xe86b5f, "MonoCat")).unwrap();
                window.update(cx, |_, _, cx| cx.notify()).unwrap();
                cx.refresh().unwrap();
                cx.run_until_parked();
            }
            let at = window
                .update(cx, |view, _, _| {
                    let state = view.selection.0.borrow();
                    let fragment = state.layouts.values().find(|f| f.text.contains("docs")).unwrap();
                    let ix = fragment.text.find("docs").unwrap() + 1;
                    fragment.layout.position_for_index(ix).unwrap() + gpui::point(px(3.), px(12.))
                })
                .unwrap();
            let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
            if hover_first {
                visual.simulate_mouse_move(at, None, Default::default());
            }
            visual.simulate_click(at, Default::default());
            assert_eq!(visual.opened_url().as_deref(), Some("https://example.com/x"), "{text:?} mono={mono}");
        }
    }
    #[gpui::test]
    fn clicking_a_link_in_a_rendered_document_opens_it(cx: &mut TestAppContext) {
        let window = cx.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(0.), px(0.)),
                        size(px(900.), px(600.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| TranscriptView::document("# T\n\n[docs](https://example.com/x) tail".into(), cx)),
            )
            .unwrap()
        });
        window.update(cx, |_, _, cx| cx.notify()).unwrap();
        cx.refresh().unwrap();
        cx.run_until_parked();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let at = window
            .update(visual, |view, _, _| {
                view.rows.iter().find(|r| matches!(&r.body, Body::Markdown(md) if crate::markdown::selection_text(md).contains("docs"))).unwrap().key
            })
            .unwrap();
        let _ = at;
        let found = (20..200).step_by(4).find_map(|y| {
            (0..400).step_by(6).find_map(|x| {
                let p = gpui::point(px(x as f32), px(y as f32));
                visual.simulate_click(p, Default::default());
                visual.opened_url().map(|_| p)
            })
        });
        assert!(found.is_some(), "no click in the document opened the link");
    }
    #[gpui::test]
    fn clicking_a_link_in_a_whole_conversation_opens_it(cx: &mut TestAppContext) {
        let store = monocode_core::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let mut summary = store.create("codex", "/work/monocode").unwrap();
        summary.has_messages = true;
        let store = std::sync::Arc::new(std::sync::Mutex::new(store));
        let window = cx.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(0.), px(0.)),
                        size(px(900.), px(700.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| crate::conversation::Conversation::new(summary, store, cx)),
            )
            .unwrap()
        });
        let transcript = window.update(cx, |c, _, _| c.transcript.clone()).unwrap();
        transcript.update(cx, |view, cx| {
            view.smooth_reveal = false;
            view.patch(0, vec![Row { role: "assistant".into(), text: format!("{}[docs](https://example.com/x) tail", "A long earlier paragraph of prose.\n\n".repeat(40)), first: true, sent_at: None }], cx)
        });
        window.update(cx, |_, _, cx| cx.notify()).unwrap();
        cx.refresh().unwrap();
        cx.run_until_parked();
        let at = transcript.update(cx, |view, _| {
            let state = view.selection.0.borrow();
            let fragment = state.layouts.values().find(|f| f.text.contains("docs")).unwrap();
            fragment.layout.position_for_index(1).unwrap() + gpui::point(px(3.), px(12.))
        });
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_mouse_move(at, None, Default::default());
        visual.simulate_click(at, Default::default());
        assert_eq!(visual.opened_url().as_deref(), Some("https://example.com/x"));
    }
    #[gpui::test]
    fn clicking_a_link_in_the_shell_opens_it(cx: &mut TestAppContext) {
        shell_link(cx, "[docs](https://example.com/x) tail", |visual| visual.opened_url().is_some());
    }
    #[gpui::test]
    fn clicking_a_file_link_in_the_shell_opens_the_file(cx: &mut TestAppContext) {
        let folder = std::env::temp_dir().join(format!("monocode-link-click-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("README.md");
        std::fs::write(&path, "# Heading\n").unwrap();
        shell_link(cx, &format!("[docs]({}) tail", path.display()), |visual| visual.debug_bounds("review-tab-strip").is_some());
        std::fs::remove_dir_all(folder).unwrap();
    }
    fn shell_link(cx: &mut TestAppContext, text: &str, done: impl Fn(&mut gpui::VisualTestContext) -> bool) {
        let mut store = monocode_core::store::Store::open(std::path::Path::new(":memory:")).unwrap();
        let mut a = store.create("codex", "/work/monocode").unwrap();
        store
            .write_rows(a.id, 0, &[Row { role: "assistant".into(), text: text.into(), first: true, sent_at: None }])
            .unwrap();
        a.has_messages = true;
        let mut state = crate::workspace::Workspace::default();
        state.add_project("/work/monocode");
        state.open(a.id);
        let saved = serde_json::to_string(&state).unwrap();
        let store = std::sync::Arc::new(std::sync::Mutex::new(store));
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| crate::shell::Shell::new(store, vec![a], "/work/monocode".into(), Some(saved), cx))
            })
            .unwrap()
        });
        cx.refresh().unwrap();
        cx.run_until_parked();
        cx.refresh().unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let found = (60..900).step_by(10).find_map(|y| {
            (480..760).step_by(4).find_map(|x| {
                let p = gpui::point(px(x as f32), px(y as f32));
                visual.simulate_mouse_move(p, None, Default::default());
                visual.simulate_click(p, Default::default());
                done(visual).then_some(p)
            })
        });
        assert!(found.is_some(), "no click in the shell opened the link");
    }
    #[gpui::test]
    fn clicking_a_file_link_in_a_remote_answer_asks_to_open_the_file(cx: &mut TestAppContext) {
        let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = opened.clone();
        cx.update(|cx| {
            cx.on_action(move |action: &crate::file_editor::OpenPath, _| {
                seen.borrow_mut().push(action.0.clone())
            });
        });
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|_| {
                    let mut view = TranscriptView::new();
                    // A Paseo conversation's cwd: links resolve against the host project.
                    view.file_link_base = Some("remote://fixture/work/p".into());
                    view
                })
            })
            .unwrap()
        });
        window
            .update(cx, |view, _, cx| {
                view.smooth_reveal = false;
                view.patch(0, vec![Row { role: "assistant".into(), text: "[main](src/main.rs:12) tail".into(), first: true, sent_at: None }], cx)
            })
            .unwrap();
        cx.refresh().unwrap();
        cx.run_until_parked();
        let bounds = window
            .update(cx, |view, _, _| view.selection.0.borrow().layouts.values().next().unwrap().layout.bounds())
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_click(gpui::point(bounds.left() + px(8.), bounds.center().y), Default::default());
        assert_eq!(visual.opened_url(), None, "a project file is not a URL for the system");
        assert_eq!(*opened.borrow(), ["remote://fixture/work/p/src/main.rs:12:1"]);
    }
    #[gpui::test]
    fn clicking_a_link_with_another_scheme_hands_it_to_the_system(cx: &mut TestAppContext) {
        let window = link_window(cx, "[docs](custom-app://open/x) tail");
        let bounds = window
            .update(cx, |view, _, _| view.selection.0.borrow().layouts.values().next().unwrap().layout.bounds())
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_click(gpui::point(bounds.left() + px(8.), bounds.center().y), Default::default());
        assert_eq!(visual.opened_url().as_deref(), Some("custom-app://open/x"));
    }
    #[gpui::test]
    fn clicking_an_in_page_anchor_opens_nothing(cx: &mut TestAppContext) {
        let window = link_window(cx, "[docs](#top) tail");
        let bounds = window
            .update(cx, |view, _, _| view.selection.0.borrow().layouts.values().next().unwrap().layout.bounds())
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_click(gpui::point(bounds.left() + px(8.), bounds.center().y), Default::default());
        assert_eq!(visual.opened_url(), None);
    }
    #[gpui::test]
    fn clicking_a_markdown_link_with_a_frame_between_press_and_release_opens_it(
        cx: &mut TestAppContext,
    ) {
        let window = link_window(cx, "[docs](https://example.com/x) tail");
        let bounds = window
            .update(cx, |view, _, _| view.selection.0.borrow().layouts.values().next().unwrap().layout.bounds())
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let at = gpui::point(bounds.left() + px(8.), bounds.center().y);
        visual.simulate_mouse_down(at, MouseButton::Left, Default::default());
        visual.refresh().unwrap();
        visual.simulate_mouse_up(at, MouseButton::Left, Default::default());
        assert_eq!(visual.opened_url().as_deref(), Some("https://example.com/x"));
    }
    #[test]
    fn selection_ranges_are_direction_independent_and_response_scoped() {
        let selection = Selection::default();
        {
            let mut state = selection.0.borrow_mut();
            state.response = 5;
            state.anchor = Some(Position {
                row: (5, 2),
                byte: 8,
            });
            state.head = Some(Position {
                row: (5, 0),
                byte: 3,
            });
        }
        assert_eq!(selection.range((5, 0), 5, 0, 10), 3..10);
        assert_eq!(selection.range((5, 1), 5, 0, 20), 0..20);
        assert_eq!(selection.range((5, 2), 5, 4, 20), 0..4);
        assert_eq!(selection.range((5, 1), 6, 0, 20), 0..0);
    }
    #[test]
    fn unicode_anchors_do_not_split_composed_graphemes() {
        assert_eq!(snap("e\u{301}🙂", 2), 0);
        assert_eq!(snap("e\u{301}🙂", 4), 3);
        assert_eq!(word("hello café", 7), 6..11);
    }
}
