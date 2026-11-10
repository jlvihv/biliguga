//! The titlebar the app draws itself, in place of the window's native one.
//!
//! One flat band across the top. Over a video it carries the way back and the clip's title;
//! over the feed, the tabs. The window's own controls sit at the trailing edge, and on Linux
//! `WindowOptions::borderless` is what removed the frame these replace.
//!
//! The band is the only place the window can be dragged from and the only place its controls
//! live, so it is always drawn — a screen without it would be a window that cannot be moved or
//! closed.

pub(crate) use super::*;

/// How tall the band is. Slim: it is a title strip, not a toolbar.
pub(crate) const TITLEBAR_HEIGHT: f32 = 36.0;

/// How wide one of the square controls at either end is. Full height, so it reads as part of
/// the band rather than as a button sitting in it.
pub(crate) const CAPTION_WIDTH: f32 = 44.0;

/// What the platform's own controls take of the band, so the app's keep clear of them: AppKit's
/// traffic lights at the leading edge on macOS, the caption buttons at the trailing edge on
/// Windows. Both zero where there is nothing drawn over the view.
fn insets(cx: &Cx<BiliGuga>) -> (f32, f32) {
    let chrome = cx.chrome_insets();
    (chrome.leading, chrome.trailing)
}

/// One of the band's square controls: full height, square corners, sitting flush against the
/// edge it belongs to. The hover wash is the only decoration.
fn caption(
    key: &'static str,
    icon: &'static rune::Icon,
    help: &'static str,
    ink: Color,
    hover: Color,
) -> El {
    // Full height, stated: `h_full` in a flex row resolves against the line's content, which
    // would leave the wash and the hover area only as tall as the glyph.
    el().key(key)
        .w(CAPTION_WIDTH)
        .h(TITLEBAR_HEIGHT)
        .center()
        .hover(hover)
        .press(theme::PRESS)
        .cursor(rune::Cursor::Pointer)
        .help(help)
        .child(glyph(icon, ink, 15.0))
}

/// The bar across the top of the window.
pub(crate) fn render_titlebar(app: &BiliGuga, cx: &Cx<BiliGuga>) -> El {
    let (leading, trailing) = insets(cx);
    let mut bar = row()
        .key("titlebar")
        .w_full()
        .h(TITLEBAR_HEIGHT)
        .items_center()
        .pl(leading)
        .pr(trailing)
        .fill(theme::PANEL)
        // Dragging the band moves the window; anything with its own press handler inside it
        // keeps its presses.
        .window_drag();
    bar = match app.playing_video.as_ref() {
        Some(video) => bar
            // The way out of the player, flush against the leading edge: the first thing on the
            // band, with nothing beside it to click by mistake.
            .child(
                // In the accent: it is the one control on the band that goes somewhere, and
                // the only way off this screen.
                caption("titlebar-back", &icons::BACK, "返回首页", theme::ACCENT, theme::HOVER)
                    .on_press(|app: &mut BiliGuga| app.close_player()),
            )
            .child(
                text(&video.title)
                    .font(TextStyle::new(12.5, 500.0))
                    .color(theme::TEXT_DIM)
                    .single_line()
                    .truncate()
                    // The title takes what the buttons leave, and is what gives way when there
                    // is not enough of it.
                    .grow(1.0)
                    .pl(4.0),
            ),
        None => bar
            .child(el().w(8.0))
            .child(render_nav(app))
            .child(el().grow(1.0))
            .child(account_button(app))
            .child(el().w(10.0)),
    };
    bar.child(window_controls(app))
}

/// The window's controls at the trailing edge, flush and square.
///
/// No minimise button. `Tx::minimize` refuses a borderless window outright — winit needs the
/// frame's button — so a button for it would be a control that does nothing.
fn window_controls(app: &BiliGuga) -> El {
    let maximized = app.window_maximized;
    let mut controls = row().key("titlebar-controls").shrink().items_center();
    if app.playing_video.is_some() {
        controls = controls.child(
            caption(
                "titlebar-fullscreen",
                if app.player_fullscreen {
                    &icons::EXIT_FULLSCREEN
                } else {
                    &icons::FULLSCREEN
                },
                "全屏",
                theme::TEXT_DIM,
                theme::HOVER,
            )
            .on_press_tx(|app: &mut BiliGuga, tx: &mut Tx| {
                app.player_fullscreen = !app.player_fullscreen;
                tx.toggle_full_screen();
            }),
        );
    }
    controls
        .child(
            caption(
                "titlebar-maximize",
                if maximized {
                    &icons::RESTORE
                } else {
                    &icons::MAXIMIZE
                },
                if maximized { "还原" } else { "最大化" },
                theme::TEXT_DIM,
                theme::HOVER,
            )
            .on_press_tx(|app: &mut BiliGuga, tx: &mut Tx| {
                app.window_maximized = !app.window_maximized;
                tx.toggle_maximized();
            }),
        )
        .child(
            // Close turns red under the pointer, the way every window's does.
            caption("titlebar-close", &icons::CLOSE, "关闭窗口", theme::TEXT_DIM, theme::DANGER)
                .on_press_tx(|_: &mut BiliGuga, tx: &mut Tx| tx.close_window()),
        )
}
