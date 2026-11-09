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
    // The player column's size decides the stage: 16:9 across the column, or the height it has
    // left under the controls, whichever is smaller. Reading the column (not the stage) keeps
    // the stage's own size out of the measurement, so no read can feed its own output.
    if let Some([_, _, width, height]) = cx.layout("player") {
        // The clip's own shape, once mpv reports one: a 4:3 upload keeps its proportions
        // instead of floating small inside a 16:9 letterbox.
        let aspect = app
            .player
            .source_size()
            .filter(|(_, height)| *height > 0)
            .map_or(16.0 / 9.0, |(width, height)| width as f32 / height as f32);
        app.stage_box.set(stage_box(width, height, aspect));
    }
    // The browse pane's height, which paging measures its runway against.
    if let Some([_, _, _, pane_height]) = cx.layout("feed-scroll") {
        app.pane_height.set(pane_height);
    }
    // The window's own width, which the feed's share beside the player is worked out from.
    // Reading the root is safe: its size is the window's, not something the view decides.
    if let Some([_, _, window_width, _]) = cx.layout("window") {
        app.feed_pane_width
            .set((window_width * FEED_PANE_SHARE).clamp(FEED_PANE_MIN, FEED_PANE_MAX));
    }
    let mut root = row().key("window").w_full().h_full().fill(theme::BG);
    if app.player_fullscreen {
        return match &app.playing_video {
            Some(video) => root.child(render_player_fullscreen(app, video)),
            None => root,
        };
    }
    root = root.child(render_rail(app));
    // What the browse pane is depends on whether something is open: with a video playing the
    // feed becomes a list beside it, so the two never fight over the window's width.
    let watching = app.playing_video.is_some();
    if watching && app.theater {
        if let Some(video) = &app.playing_video {
            root = root.child(render_player(app, video));
        }
    } else if watching {
        let feed = col()
            .key("feed-pane")
            .w(app.feed_pane_width.get())
            .h_full()
            .shrink()
            .child(render_feed(app));
        root = root.child(feed);
        if let Some(video) = &app.playing_video {
            root = root.child(render_player(app, video));
        }
    } else {
        root = root.child(render_feed(app));
    }
    root
}

/// The rectangle the video gets inside a player column `width` × `height` points.
///
/// The controls under the stage are a fixed strip (seek bar, transport, actions), so the stage
/// takes what is left, and its frame is fitted to 16:9 inside that: a wide window fills the
/// stage, a short one letterboxes it instead of pushing the controls off the bottom.
pub(crate) fn stage_box(column_width: f32, column_height: f32, aspect: f32) -> (f32, f32) {
    let width = (column_width - 2.0 * PANE_PAD).max(160.0);
    // Everything under the stage, measured rather than guessed: the block's own padding, the
    // gap, the controls card (10 + 18 + 8 + 34 + 10), the gap, the actions row, and a little
    // slack so a rounding point never clips the last row.
    let reserved = 2.0 * PANE_PAD + 10.0 + 80.0 + 10.0 + 32.0 + 10.0;
    let room = (column_height - reserved).max(90.0);
    let height = ((width / aspect.max(0.2)).min(room)).max(90.0);
    (width, height)
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
        app.playing_video = Some(Video {
            bvid: "BVbench".into(),
            aid: 0,
            cid: 0,
            title: "本地压测片段".into(),
            uploader: "bench".into(),
            uploader_mid: 0,
            duration: "--:--".into(),
            cover: String::new(),
            accent: 0x3e4654,
        });
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
