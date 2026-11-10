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

/// The bar across the top of the window while a video is open.
pub(crate) fn render_titlebar(app: &BiliGuga, cx: &Cx<BiliGuga>) -> El {
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
            square_button("titlebar-back", &icons::BACK, "返回首页")
                .on_press(|app: &mut BiliGuga| app.close_player()),
        )
        .child(
            text(match &app.playing_video {
                Some(video) => video.title.as_str(),
                None => "",
            })
            .font(TextStyle::new(13.0, 550.0))
            .color(theme::TEXT)
            .single_line()
            .truncate()
            // The title takes whatever the buttons leave, and is the thing that gives way when
            // there is not enough of it.
            .grow(1.0),
        );
    // On Windows the caption buttons are drawn over the band's trailing edge, so the last
    // control has to stop short of them; `trailing` is their width.
    bar = bar.child(titlebar_controls(app));
    bar
}

/// The controls at the right of the titlebar: the app's own, not the window's.
///
/// Minimise, maximise and close are the window manager's, and Rune draws them over the band's
/// trailing edge on the platforms where it replaces the native frame — `insets` is what keeps
/// this row clear of them. Drawing a second close button next to theirs would be two controls
/// doing the same thing in the same place.
fn titlebar_controls(app: &BiliGuga) -> El {
    let mut controls = row().shrink().items_center().gap(4.0);
    controls = controls.child(
        square_button(
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
    );
    controls
}
