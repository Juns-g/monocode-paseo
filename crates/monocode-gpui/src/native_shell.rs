//! Native presentation shared by the local Shell and the daemon-owned Paseo client.
use crate::theme::*;
use gpui::{
    AnyElement, Bounds, Context, Div, Stateful, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, div, point, prelude::*, px, size,
};

pub(crate) fn window_options(
    title: &'static str,
    app_id: Option<&'static str>,
    cx: &gpui::App,
) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1180.), px(800.)),
            cx,
        ))),
        window_min_size: Some(size(px(820.), px(520.))),
        window_background: if cfg!(target_os = "macos") {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        },
        app_owns_titlebar_drag: cfg!(target_os = "macos"),
        app_id: app_id.map(Into::into),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: cfg!(target_os = "macos"),
            traffic_light_position: if cfg!(target_os = "macos") {
                Some(point(px(12.), px((TITLE_BAR_HEIGHT - 14.) / 2.)))
            } else {
                None
            },
        }),
        ..Default::default()
    }
}
pub(crate) fn pin_titlebar<T: 'static>(window: &mut Window, cx: &mut Context<T>) {
    #[cfg(target_os = "macos")]
    {
        crate::traffic_lights::pin(window, TITLE_BAR_HEIGHT);
        cx.observe_window_activation(window, |_, window, _| {
            crate::traffic_lights::pin(window, TITLE_BAR_HEIGHT)
        })
        .detach();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (window, cx);
}

const CARD_RADIUS: f32 = 10.;

// GPUI overflow clips rectangles. Capture outside the bordered frame so the
// final mask includes its background, border and children at the outer radius.
// A mask inside the frame only covers its inset content rectangle.
fn card_corner_mask(begin: bool) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let r = px(CARD_RADIUS)
                .min(bounds.size.width / 2.)
                .min(bounds.size.height / 2.);
            #[cfg(target_os = "macos")]
            let corners = [
                bounds.origin,
                gpui::point(bounds.right() - r, bounds.top()),
                gpui::point(bounds.right() - r, bounds.bottom() - r),
                gpui::point(bounds.left(), bounds.bottom() - r),
            ];
            #[cfg(target_os = "macos")]
            for (corner, origin) in corners.into_iter().enumerate() {
                window.paint_quad(
                    gpui::fill(gpui::Bounds::new(origin, gpui::size(r, r)), gpui::black())
                        .border_color(gpui_apple::metal_renderer::corner_clip_marker(
                            begin,
                            corner as u8,
                        )),
                );
            }
            if !begin {
                // The areas outside the curve belong to the surrounding
                // chrome, just like the gutters and footer. Restoring the
                // window's unwashed glass here leaves dark square corners.
                #[cfg(target_os = "macos")]
                let outside = inset_chrome_wash();
                #[cfg(not(target_os = "macos"))]
                let outside = shell_fill().blend(inset_chrome_wash());
                let vertices = [
                    (
                        bounds.origin,
                        gpui::point(bounds.left() + r, bounds.top()),
                        gpui::point(bounds.left(), bounds.top() + r),
                    ),
                    (
                        gpui::point(bounds.right(), bounds.top()),
                        gpui::point(bounds.right(), bounds.top() + r),
                        gpui::point(bounds.right() - r, bounds.top()),
                    ),
                    (
                        gpui::point(bounds.right(), bounds.bottom()),
                        gpui::point(bounds.right() - r, bounds.bottom()),
                        gpui::point(bounds.right(), bounds.bottom() - r),
                    ),
                    (
                        gpui::point(bounds.left(), bounds.bottom()),
                        gpui::point(bounds.left(), bounds.bottom() - r),
                        gpui::point(bounds.left() + r, bounds.bottom()),
                    ),
                ];
                for (corner, start, end) in vertices {
                    let mut path = gpui::PathBuilder::fill();
                    path.move_to(corner);
                    path.line_to(start);
                    path.arc_to(gpui::point(r, r), px(0.), false, false, end);
                    path.close();
                    if let Ok(path) = path.build() {
                        window.paint_path(path, outside);
                    }
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

pub(crate) fn inset_layout(
    titlebar: AnyElement,
    sidebar: AnyElement,
    card: AnyElement,
    footer: AnyElement,
    sidebar_full_width: f32,
    sidebar_fraction: f32,
    overlays: Vec<AnyElement>,
) -> Vec<AnyElement> {
    let sidebar_width = sidebar_full_width * sidebar_fraction;
    let card = div()
        .id("rounded-main-pane")
        .debug_selector(|| "rounded-main-pane".into())
        .relative()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .overflow_hidden()
        .child(card_corner_mask(true))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .overflow_hidden()
                .bg(inset_pane_fill())
                .rounded(px(CARD_RADIUS))
                .border_1()
                .border_color(content(0.08))
                .child(card),
        )
        .child(card_corner_mask(false));
    vec![
        div()
            .h(px(TITLE_BAR_HEIGHT))
            .flex_shrink_0()
            .bg(inset_chrome_wash())
            .child(titlebar)
            .into_any_element(),
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .relative()
            .when(sidebar_width > 0., |d| {
                d.child(
                    // Keep the sidebar still beneath the moving card. Clip
                    // its covered part so text cannot bleed through the
                    // translucent workspace or receive covered clicks.
                    div()
                        .id("sidebar-frame")
                        .debug_selector(|| "sidebar-frame".into())
                        .absolute()
                        .left_0()
                        .top_0()
                        .w(px(sidebar_width))
                        .h_full()
                        .overflow_hidden()
                        .bg(inset_chrome_wash())
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .w(px(sidebar_full_width))
                                .h_full()
                                .opacity(sidebar_fraction)
                                .child(sidebar),
                        ),
                )
            })
            .child(div().w(px(sidebar_width)).h_full().flex_shrink_0())
            .child(
                div()
                    .id("main-pane-frame")
                    .debug_selector(|| "main-pane-frame".into())
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .pr_1p5()
                    .pl(px(6. * (1. - sidebar_fraction)))
                    .child(
                        div()
                            .absolute()
                            .right_0()
                            .top_0()
                            .bottom_0()
                            .w(px(6.))
                            .bg(inset_chrome_wash()),
                    )
                    .when(sidebar_fraction < 1., |d| {
                        d.child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .bottom_0()
                                .w(px(6. * (1. - sidebar_fraction)))
                                .bg(inset_chrome_wash()),
                        )
                    })
                    .child(card)
                    .child(footer),
            )
            .children(overlays)
            .into_any_element(),
    ]
}

/// Original Conversation composer frame, including native backdrop frosting.
pub(crate) fn composer_surface(focused: bool) -> Stateful<Div> {
    let radius = 12.;
    let blur = composer_blur();
    let fill = composer_fill();
    div()
        .id("composer-box")
        .debug_selector(|| "composer-box".into())
        .relative()
        .rounded(px(radius))
        .border_1()
        .border_color(composer_border(focused))
        .flex()
        .flex_col()
        .child(
            gpui::canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let surface = gpui::fill(bounds, fill).corner_radii(px(radius - 1.));
                    #[cfg(target_os = "macos")]
                    let surface = if blur > 0. {
                        surface.border_color(gpui_apple::metal_renderer::backdrop_blur_marker(
                            blur * window.scale_factor(),
                        ))
                    } else {
                        surface
                    };
                    #[cfg(not(target_os = "macos"))]
                    let _ = blur;
                    window.paint_quad(surface);
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
}
pub(crate) fn composer_input() -> Div {
    div().px_3().pt(px(14.)).pb(px(14.))
}
pub(crate) fn composer_toolbar() -> Div {
    div()
        .debug_selector(|| "composer-toolbar".into())
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .pb(px(10.))
}
pub(crate) fn composer_action(busy: bool, enabled: bool) -> Stateful<Div> {
    div()
        .id("send-or-stop")
        .role(gpui::Role::Button)
        .aria_label(if busy { "Stop" } else { "Send" })
        .cursor(gpui::CursorStyle::Arrow)
        .flex_shrink_0()
        .size(px(26.))
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .bg(white(if enabled { 1. } else { 0.3 }))
        .when(enabled, |d| d.hover(|s| s.bg(white(0.9))))
        .map(|d| {
            if busy {
                d.child(div().size(px(10.)).rounded(px(2.)).bg(black(1.)))
            } else {
                d.child(icon("send", 14.).text_color(black(if enabled { 1. } else { 0.4 })))
            }
        })
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// Sidebar card geometry shared with Shell::session_row_with.
pub(crate) fn session_row_surface(compact: bool, nested: bool, active: bool) -> Div {
    div()
        .cursor(gpui::CursorStyle::Arrow)
        .h(px(if compact {
            if nested { 28. } else { 32. }
        } else if nested {
            56.
        } else {
            60.
        }))
        .px(px(if nested { 7. } else { 9. }))
        .rounded_md()
        .flex()
        .items_center()
        .when(active, |d| d.bg(content(0.1)))
        .hover(move |s| s.bg(content(if active { 0.1 } else { 0.05 })))
}
