//! The window itself: the view, its sizes, and the one-time setup.

pub(crate) use super::*;

pub(crate) fn view(app: &BiliGuga, cx: &mut Cx<BiliGuga>) -> El {
    // Reading the stage's size re-runs the view only when that size changes, not when the
    // element merely moves: exactly what telling mpv where to decode for needs.
    let stage_size = cx.layout_size("player-stage").map(|[width, height]| {
        (
            width.round().max(1.0) as usize,
            height.round().max(1.0) as usize,
        )
    });
    app.stage_size.set(stage_size);
    // The browse pane's width decides how many cards fit. `layout_with` re-runs this view only
    // when that count changes, not on every point the window is dragged wider.
    if let Some(columns) = cx.layout_with("feed-scroll", column_count)
        && let Some([_, _, pane_width, _]) = cx.layout("feed-scroll")
    {
        app.card_columns.set(columns);
        app.card_width.set(slot_width(pane_width, columns));
    }
    // The browse pane's height, which paging measures its runway against.
    if let Some([_, _, _, pane_height]) = cx.layout("feed-scroll") {
        app.pane_height.set(pane_height);
    }
    let mut root = row().key("window").w_full().h_full().fill(theme::BG);
    if app.player_fullscreen {
        return match &app.playing_video {
            Some(video) => root.child(render_player_fullscreen(app, video)),
            None => root,
        };
    }
    // The two panes never share the window: the feed is what the window shows when nothing is
    // open, and a video takes the whole of it. There is no rail down the side either — the tabs
    // are a row across the feed's own top, and the player has no tabs at all.
    match &app.playing_video {
        Some(video) => root = root.child(render_player(app, video)),
        None => root = root.child(render_feed(app)),
    }
    root
}

/// `layout_size_with`'s comparison for the browse pane: the view only has to run again when the
/// number of columns changes, not on every point the window is dragged wider.
pub(crate) fn column_count([_, _, width, _]: [f32; 4]) -> usize {
    column_count_of(width)
}

pub(crate) fn column_count_of(width: f32) -> usize {
    let usable = width - 2.0 * PANE_PAD;
    (((usable + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)).floor() as i32).clamp(1, 5) as usize
}

/// One card's share of the pane: the padded width, less the gaps, split evenly.
pub(crate) fn slot_width(pane_width: f32, columns: usize) -> f32 {
    let usable = pane_width - 2.0 * PANE_PAD;
    let columns = columns.max(1) as f32;
    ((usable - (columns - 1.0) * CARD_GAP) / columns)
        .floor()
        .max(80.0)
}

// ---- Runtime hooks ---------------------------------------------------------------------------

/// Called once, before the first frame, with the handle other threads use to wake the window.
pub(crate) fn setup(app: &mut BiliGuga, waker: Waker) {
    app.images = Some(ImageCache::new(waker.clone()));
    if let Some(worker) = app.worker.as_mut() {
        worker.waker = waker;
    }
    // A local file plays instead of the feed, for measuring the real window without a network
    // account: `BILIGUGA_BENCH_CLIP=/path/to/clip.mp4`. Not a feature, a harness.
    if let Some(clip) = std::env::var_os("BILIGUGA_BENCH_CLIP") {
        let clip = clip.to_string_lossy().into_owned();
        app.playing_video = Some(Arc::new(Video {
            bvid: "BVbench".into(),
            aid: 0,
            cid: 0,
            title: "本地压测片段".into(),
            uploader: "bench".into(),
            uploader_mid: 0,
            duration: "--:--".into(),
            cover: String::new(),
            accent: 0x3e4654,
        }));
        app.playback = PlaybackState::Buffering;
        app.player.load(clip, None, 0., 1.);
        return;
    }
    // Everything slow starts here, after the first frame: the window opens at once.
    let cookie = app.cookie();
    app.load_home_page(false);
    let _ = cookie;
    if app.session.is_some() {
        app.refresh_session();
    }
}
