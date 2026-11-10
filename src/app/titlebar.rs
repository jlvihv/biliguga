//! The titlebar the app draws itself, in place of the window's native one.
//!
//! Left to right: the way back to the feed, the title of what is playing, and the window's own
//! controls. It exists only while a video is open — the feed keeps its own navigation row at
//! the top of the pane, and a titlebar with nothing in it would be a band of empty pixels
//! above the window — so entering and leaving the player changes the window's titlebar height,
//! which is what `Cx::titlebar_height` is for.
//!
//! The band is where the window's own controls live: AppKit's traffic lights at the leading
//! edge on macOS, the caption buttons at the trailing edge on Windows. Both are drawn *over*
//! the view, so the app keeps clear of `Cx::chrome_insets` — see [`leading_inset`].

pub(crate) use super::*;

/// How tall the drawn titlebar is. Rune draws the window's caption buttons at this height, and
/// the view lays its own controls out in a band the same size.
pub(crate) const TITLEBAR_HEIGHT: f32 = 44.0;

/// What the window's own controls take of the band, so the app's do not sit under them.
///
/// On macOS the traffic lights are at the leading edge, so the back button starts after them;
/// on Windows the caption buttons are at the trailing edge, so the right-hand controls end
/// before them. Everywhere else — Linux among them, where Rune keeps the native frame — both
/// are zero and the band is the app's alone.
fn insets(cx: &Cx<BiliGuga>) -> (f32, f32) {
    let chrome = cx.chrome_insets();
    (chrome.leading, chrome.trailing)
}

/// The bar across the top of the window.
///
/// While a video is open it carries the way back and the clip's title; over the feed it carries
/// the app's name. In both cases it is the only place the window can be dragged from and the
/// only place its own controls live, because on Linux there is no frame left to provide them.
pub(crate) fn render_titlebar(app: &BiliGuga, cx: &Cx<BiliGuga>) -> El {
    let watching = app.playing_video.is_some();
    let (leading, trailing) = insets(cx);
    let mut bar = row()
        .key("titlebar")
        .w_full()
        .h(TITLEBAR_HEIGHT)
        .items_center()
        .gap(8.0)
        .pl(leading.max(PANE_PAD))
        .pr(trailing.max(PANE_PAD))
        .fill(theme::PANEL)
        // Dragging the band moves the window; the buttons inside it keep their presses, which
        // Rune guarantees for anything with a press handler.
        .window_drag()
        .child(
            el().size(30.0, 30.0)
                .center()
                .radius(9.0)
                .child(glyph(&icons::LOGO, theme::ACCENT, 21.0)),
        );
    // What the band carries between the brand and the window's controls: the way out and the
    // clip's title while a video is open, the tabs and the account while it is not. The feed's
    // row used to hold those itself, which would be two navigation rows on one screen.
    bar = match watching {
        true => bar
            .child(
                square_button("titlebar-back", &icons::BACK, "返回首页")
                    .on_press(|app: &mut BiliGuga| app.close_player()),
            )
            .child(
                text(app.playing_video.as_ref().map_or("", |video| video.title.as_str()))
                    .font(TextStyle::new(13.0, 550.0))
                    .color(theme::TEXT)
                    .single_line()
                    .truncate()
                    // The title takes whatever the buttons leave, and is what gives way.
                    .grow(1.0),
            ),
        false => bar
            .child(el().w(6.0))
            .child(render_nav(app))
            .child(el().grow(1.0))
            .child(account_button(app)),
    };
    // On Windows the caption buttons are drawn over the band's trailing edge, so the last
    // control has to stop short of them; `trailing` is their width.
    bar = bar.child(window_controls(app));
    bar
}

/// The controls at the right of the titlebar.
///
/// These are the window's, drawn by the app because on Linux a borderless window has no frame
/// to draw them: `WindowOptions::borderless` is what takes the system titlebar away here, since
/// Rune's own `Titlebar` only replaces the frame on macOS and Windows.
///
/// No minimise button. `Tx::minimize` refuses a borderless window outright — winit would need
/// the frame's button — so a button for it would be a control that does nothing. Maximise and
/// close are the two that work, and they are the two the frame offered that still can.
fn window_controls(app: &BiliGuga) -> El {
    let maximized = app.window_maximized;
    let watching = app.playing_video.is_some();
    row()
        .shrink()
        .items_center()
        .gap(2.0)
        // Full screen is about the video, so it only exists on the screen that has one.
        .child(match watching {
            true => square_button(
                "titlebar-fullscreen",
                if app.player_fullscreen {
                    &icons::EXIT_FULLSCREEN
                } else {
                    &icons::FULLSCREEN
                },
                "全屏",
            )
            .on_press_tx(|app: &mut BiliGuga, tx: &mut Tx| {
                app.player_fullscreen = !app.player_fullscreen;
                tx.toggle_full_screen();
            }),
            false => el(),
        })
        .child(
            square_button(
                "titlebar-maximize",
                if maximized {
                    &icons::RESTORE
                } else {
                    &icons::MAXIMIZE
                },
                if maximized { "还原" } else { "最大化" },
            )
            .on_press_tx(|app: &mut BiliGuga, tx: &mut Tx| {
                app.window_maximized = !app.window_maximized;
                tx.toggle_maximized();
            }),
        )
        .child(
            square_button("titlebar-close", &icons::CLOSE, "关闭窗口")
                .on_press_tx(|app: &mut BiliGuga, tx: &mut Tx| {
                    let _ = app;
                    tx.close_window();
                }),
        )
}
