//! Startup: the demo rows `--snap` shows, the named scenes and the entry into Rune.

pub(crate) use super::*;

pub(crate) fn demo_video(index: usize) -> Video {
    Video {
        bvid: format!("BV1demo{index:04}"),
        aid: 1000 + index as i64,
        cid: 2000 + index as i64,
        title: DEMO_TITLES[index % DEMO_TITLES.len()].to_string(),
        uploader: DEMO_UPLOADERS[index % DEMO_UPLOADERS.len()].to_string(),
        uploader_mid: 100 + index as i64,
        duration: format!("{:02}:{:02}", 3 + index % 40, index * 7 % 60),
        cover: String::new(),
        accent: DEMO_ACCENTS[index % DEMO_ACCENTS.len()],
    }
}

pub(crate) fn demo_videos(count: usize) -> Vec<Video> {
    (0..count).map(demo_video).collect()
}

/// Named states for `--snap`: each seeds the app so a headless run shows a real screen.
///
/// A `const` table, not a lazily built `Vec`: Rune asks for `&'static [..]` once at startup, so
/// a `OnceLock` around it bought an allocation and an indirection and nothing else.
pub(crate) const SCENES: &[Scene] = &[
    ("feed", seed_feed),
    ("player", seed_player),
    ("search", seed_search),
    ("empty", seed_empty),
    ("login", seed_login),
    ("history", seed_history),
    ("signed-out", seed_signed_out),
    ("fullscreen", seed_fullscreen),
];

pub(crate) type Scene = (&'static str, fn(&mut BiliGuga));

pub(crate) fn seed_loaded(app: &mut BiliGuga) {
    app.videos = demo_videos(12);
    app.loading = false;
    app.home_loading = false;
}

pub(crate) fn seed_feed(app: &mut BiliGuga) {
    seed_loaded(app);
}

pub(crate) fn seed_player(app: &mut BiliGuga) {
    seed_loaded(app);
    app.playing_video = Some(demo_video(0));
    app.playback = PlaybackState::Buffering;
    app.show_controls();
    app.stats = Some(VideoStats {
        view_count: "128.4万".into(),
        danmaku_count: "1.2万".into(),
        like_count: "9.8万".into(),
        published_at: "2026-11-02 18:20".into(),
    });
    app.comments = (0..6)
        .map(|index| Comment {
            rpid: index as i64,
            username: format!("用户{}", index + 1),
            message: "这个镜头调色太舒服了，请问用的是什么预设？".into(),
            like: 128 - index as i64,
            time: "3 小时前".into(),
        })
        .collect();
    app.comments_total = 6;
}

/// The first frame a fresh install shows: nothing loaded, nothing playing.
pub(crate) fn seed_empty(app: &mut BiliGuga) {
    app.loading = false;
    app.home_loading = false;
}

/// A feed that needs a session, on a machine that has none: the panel offers the door.
pub(crate) fn seed_signed_out(app: &mut BiliGuga) {
    seed_empty(app);
    // The session is loaded from disk at launch, so a machine that has one would otherwise
    // never show this screen.
    app.session = None;
    app.account_avatar = None;
    app.active_tab = AppTab::Dynamic;
    app.dynamic_loading = false;
}

/// The OS-level full screen, with its floating control strip.
pub(crate) fn seed_fullscreen(app: &mut BiliGuga) {
    seed_player(app);
    app.player_fullscreen = true;
}

/// The login card, before the QR code has arrived.
pub(crate) fn seed_login(app: &mut BiliGuga) {
    seed_empty(app);
    app.active_tab = AppTab::Login;
    app.login_status = "正在获取登录二维码…".into();
}

/// A signed-in feed, so the history list can be checked without an account.
pub(crate) fn seed_history(app: &mut BiliGuga) {
    seed_loaded(app);
    app.active_tab = AppTab::History;
    app.history = demo_videos(10);
}

pub(crate) fn seed_search(app: &mut BiliGuga) {
    seed_loaded(app);
    app.active_tab = AppTab::Search;
    app.search_results = demo_videos(8);
    app.search_edit = TextEdit::new("旅行短片");
}

pub(crate) fn launch() {
    rune::run(AppSpec {
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        scenes: SCENES,
        min_size: Some((820.0, 560.0)),
        ..AppSpec::new("哔哩咕嘎", (1360.0, 820.0), BiliGuga::new, view)
    });
}
