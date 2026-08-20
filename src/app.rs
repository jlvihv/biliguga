use crate::{
    api::{
        PlayQuality, add_to_favorites, add_to_watch_later, coin_video, download_avatar,
        fetch_author_videos, fetch_comments, fetch_dynamic_feed, fetch_favorites, fetch_history,
        fetch_last_play_progress, fetch_recommendations, fetch_search_results, fetch_video_context,
        fetch_watch_later, format_time, like_video, quality_label, queue_cover_download,
        report_video_heartbeat, report_video_progress, resolve_play_url,
    },
    login::{self, PollResult, UserSession},
    model::{Comment, LOADING_VIDEO, Video, VideoCollection},
    mpv, network,
    search_input::{SearchInput, bind_search_keys},
};
use futures::{
    FutureExt,
    future::BoxFuture,
    stream::{FuturesUnordered, StreamExt},
};
use gpui::{
    App, Application, AssetSource, Bounds, ClickEvent, Context, DispatchPhase, Entity, FocusHandle,
    FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    RenderImage, ScrollWheelEvent, SharedString, Timer, UniformListScrollHandle, Window,
    WindowBounds, WindowOptions, canvas, div, img, point, prelude::*, px, relative, rgb, size, svg,
    uniform_list,
};
use std::{
    fs,
    ops::Range,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const HOME_MAX_ITEMS: usize = 120;
const HOME_PREFETCH_ITEMS: usize = 4;
const VIDEO_ROW_HEIGHT: f32 = 76.;
const SPEED_OPTIONS: [f64; 5] = [0.5, 1., 1.25, 1.5, 2.];
const WINDOW_FULLSCREEN_ICON: &str = "icons/window-fullscreen.svg";
const WINDOW_FULLSCREEN_EXIT_ICON: &str = "icons/window-fullscreen-exit.svg";
const SCREEN_FULLSCREEN_ICON: &str = "icons/screen-fullscreen.svg";
const SCREEN_FULLSCREEN_EXIT_ICON: &str = "icons/screen-fullscreen-exit.svg";
const DEFAULT_VOLUME: f64 = 100.;
const DEFAULT_SPEED: f64 = 1.;

struct AppAssets;

fn player_settings_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let config_dir = std::env::var_os("APPDATA").map(PathBuf::from);

    #[cfg(not(target_os = "windows"))]
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    config_dir.map(|dir| dir.join("biliguga").join("settings.json"))
}

fn load_player_settings() -> (f64, f64) {
    let defaults = (DEFAULT_VOLUME, DEFAULT_SPEED);
    let Some(path) = player_settings_path() else {
        return defaults;
    };
    let Ok(content) = fs::read_to_string(path) else {
        return defaults;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return defaults;
    };
    let volume = value
        .get("volume")
        .and_then(serde_json::Value::as_f64)
        .filter(|volume| volume.is_finite())
        .map(|volume| volume.clamp(0., 100.))
        .unwrap_or(DEFAULT_VOLUME);
    let speed = value
        .get("speed")
        .and_then(serde_json::Value::as_f64)
        .filter(|speed| speed.is_finite() && *speed >= 0.1)
        .unwrap_or(DEFAULT_SPEED);
    (volume, speed)
}

fn save_player_settings(volume: f64, speed: f64) {
    let Some(path) = player_settings_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let value = serde_json::json!({
        "volume": volume.clamp(0., 100.),
        "speed": speed.max(0.1),
    });
    let _ = fs::write(path, value.to_string());
}

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        let asset = match path {
            WINDOW_FULLSCREEN_ICON => {
                include_bytes!("../assets/icons/window-fullscreen.svg").as_slice()
            }
            WINDOW_FULLSCREEN_EXIT_ICON => {
                include_bytes!("../assets/icons/window-fullscreen-exit.svg").as_slice()
            }
            SCREEN_FULLSCREEN_ICON => {
                include_bytes!("../assets/icons/screen-fullscreen.svg").as_slice()
            }
            SCREEN_FULLSCREEN_EXIT_ICON => {
                include_bytes!("../assets/icons/screen-fullscreen-exit.svg").as_slice()
            }
            _ => return Ok(None),
        };
        Ok(Some(std::borrow::Cow::Borrowed(asset)))
    }

    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

struct BiliGuga {
    root_focus: FocusHandle,
    search_input: Entity<SearchInput>,
    search_query: String,
    active_tab: AppTab,
    videos: Vec<Video>,
    home_page: usize,
    home_loading: bool,
    home_has_more: bool,
    home_generation: u64,
    home_scroll_handle: UniformListScrollHandle,
    home_scroll_requested: bool,
    home_last_scroll_offset: Option<Pixels>,
    home_feed_mid: Option<i64>,
    search_results: Vec<Video>,
    history: Vec<Video>,
    watch_later: Vec<Video>,
    favorites: Vec<Video>,
    dynamic_videos: Vec<Video>,
    author_videos: Vec<Video>,
    author_mid: i64,
    author_name: String,
    author_page: usize,
    author_has_more: bool,
    author_loading: bool,
    author_generation: u64,
    selected: usize,
    loading: bool,
    history_loading: bool,
    watch_later_loading: bool,
    favorites_loading: bool,
    dynamic_loading: bool,
    cover_loading: bool,
    cover_generation: u64,
    cover_cancelled: Arc<AtomicBool>,
    feed_generation: u64,
    session: Option<UserSession>,
    account_avatar: Option<Arc<RenderImage>>,
    login_image: Option<Arc<RenderImage>>,
    login_key: Option<String>,
    login_loading: bool,
    login_status: SharedString,
    login_generation: u64,
    playback: PlaybackState,
    playback_request: u64,
    volume: f64,
    speed: f64,
    quality: u32,
    quality_options: Vec<PlayQuality>,
    speed_menu_open: bool,
    quality_menu_open: bool,
    volume_dragging: bool,
    controls_visible: bool,
    controls_generation: u64,
    player_fullscreen: bool,
    screen_fullscreen: bool,
    playing_video: Option<Video>,
    cloud_resume_progress: Option<f64>,
    cloud_resume_applied: bool,
    pending_seek: Option<f64>,
    collection: Option<VideoCollection>,
    collection_loading: bool,
    collection_expanded: bool,
    collection_cover_loading: bool,
    collection_cover_generation: u64,
    collection_cover_cancelled: Arc<AtomicBool>,
    history_report_at: Instant,
    history_report_in_flight: bool,
    pending_cover_drops: Vec<Arc<RenderImage>>,
    comments: Vec<Comment>,
    comments_for: String,
    comments_page: u32,
    comments_total: i64,
    comments_has_more: bool,
    comments_loading: bool,
    comments_generation: u64,
    comments_error: Option<SharedString>,
    message: SharedString,
    player: mpv::MpvPlayer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppTab {
    Home,
    Search,
    Dynamic,
    WatchLater,
    Favorites,
    History,
    Author,
    Login,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaybackState {
    Idle,
    Buffering,
    Playing,
    Paused,
}

impl BiliGuga {
    fn new(cx: &mut Context<Self>) -> Self {
        let session = login::load_session();
        let (volume, speed) = load_player_settings();
        let login_status = session
            .as_ref()
            .map(|session| format!("已登录：{}", session.username))
            .unwrap_or_else(|| "使用哔哩哔哩 App 扫码登录".into());
        Self {
            root_focus: cx.focus_handle(),
            search_input: cx.new(SearchInput::new),
            search_query: String::new(),
            active_tab: AppTab::Home,
            videos: Vec::new(),
            home_page: 0,
            home_loading: true,
            home_has_more: true,
            home_generation: 0,
            home_scroll_handle: UniformListScrollHandle::new(),
            home_scroll_requested: false,
            home_last_scroll_offset: None,
            home_feed_mid: None,
            search_results: Vec::new(),
            history: Vec::new(),
            watch_later: Vec::new(),
            favorites: Vec::new(),
            dynamic_videos: Vec::new(),
            author_videos: Vec::new(),
            author_mid: 0,
            author_name: String::new(),
            author_page: 0,
            author_has_more: false,
            author_loading: false,
            author_generation: 0,
            selected: 0,
            loading: true,
            history_loading: false,
            watch_later_loading: false,
            favorites_loading: false,
            dynamic_loading: false,
            cover_loading: false,
            cover_generation: 0,
            cover_cancelled: Arc::new(AtomicBool::new(false)),
            feed_generation: 0,
            session,
            account_avatar: None,
            login_image: None,
            login_key: None,
            login_loading: false,
            login_status: SharedString::from(login_status),
            login_generation: 0,
            playback: PlaybackState::Idle,
            playback_request: 0,
            volume,
            speed,
            quality: 64,
            quality_options: Vec::new(),
            speed_menu_open: false,
            quality_menu_open: false,
            volume_dragging: false,
            controls_visible: false,
            controls_generation: 0,
            player_fullscreen: false,
            screen_fullscreen: false,
            playing_video: None,
            cloud_resume_progress: None,
            cloud_resume_applied: false,
            pending_seek: None,
            collection: None,
            collection_loading: false,
            collection_expanded: true,
            collection_cover_loading: false,
            collection_cover_generation: 0,
            collection_cover_cancelled: Arc::new(AtomicBool::new(false)),
            history_report_at: Instant::now(),
            history_report_in_flight: false,
            pending_cover_drops: Vec::new(),
            comments: Vec::new(),
            comments_for: String::new(),
            comments_page: 0,
            comments_total: 0,
            comments_has_more: false,
            comments_loading: false,
            comments_generation: 0,
            comments_error: None,
            message: SharedString::from("正在从 B 站加载推荐视频…"),
            player: mpv::MpvPlayer::new(),
        }
    }

    fn current_videos(&self) -> &Vec<Video> {
        match self.active_tab {
            AppTab::Home => &self.videos,
            AppTab::Search => &self.search_results,
            AppTab::Dynamic => &self.dynamic_videos,
            AppTab::WatchLater => &self.watch_later,
            AppTab::Favorites => &self.favorites,
            AppTab::History => &self.history,
            AppTab::Author => &self.author_videos,
            AppTab::Login => &self.videos,
        }
    }

    fn current_videos_mut(&mut self) -> &mut Vec<Video> {
        match self.active_tab {
            AppTab::Home => &mut self.videos,
            AppTab::Search => &mut self.search_results,
            AppTab::Dynamic => &mut self.dynamic_videos,
            AppTab::WatchLater => &mut self.watch_later,
            AppTab::Favorites => &mut self.favorites,
            AppTab::History => &mut self.history,
            AppTab::Author => &mut self.author_videos,
            AppTab::Login => &mut self.videos,
        }
    }

    fn selected_video_ref(&self) -> Option<&Video> {
        self.playing_video
            .as_ref()
            .or_else(|| self.current_videos().get(self.selected))
    }

    fn pin_playing_video(&mut self, video: &Video) {
        let mut video = video.clone();
        video.cover_image = None;
        self.playing_video = Some(video);
    }

    fn trim_home_items(&mut self) {
        if self.videos.len() <= HOME_MAX_ITEMS {
            return;
        }
        let excess = self.videos.len() - HOME_MAX_ITEMS;
        let remove_count = excess.min(self.videos.len());
        let removed: Vec<_> = self.videos.drain(..remove_count).collect();
        for mut video in removed {
            if let Some(image) = video.cover_image.take() {
                self.pending_cover_drops.push(image);
            }
        }
        if self.selected >= remove_count {
            self.selected -= remove_count;
        } else {
            self.selected = 0;
        }

        let handle = self.home_scroll_handle.0.borrow().base_handle.clone();
        let offset = handle.offset();
        handle.set_offset(point(
            offset.x,
            offset.y + px(remove_count as f32 * VIDEO_ROW_HEIGHT),
        ));
        self.home_last_scroll_offset = Some(offset.y + px(remove_count as f32 * VIDEO_ROW_HEIGHT));
    }

    fn reset_home_scroll(&mut self) {
        let handle = self.home_scroll_handle.0.borrow().base_handle.clone();
        handle.set_offset(point(px(0.), px(0.)));
        self.home_last_scroll_offset = Some(px(0.));
    }

    fn load_home_page(&mut self, cx: &mut Context<Self>, reset: bool) {
        if (!reset && self.home_loading) || (!reset && !self.home_has_more) {
            return;
        }
        if reset {
            self.home_generation = self.home_generation.wrapping_add(1);
            self.home_page = 0;
            self.home_has_more = true;
            self.loading = true;
            self.videos.clear();
            self.selected = 0;
            self.playing_video = None;
            self.home_scroll_requested = false;
            self.reset_home_scroll();
        }
        let page = self.home_page + 1;
        let generation = self.home_generation;
        let cookie = self.session.as_ref().map(|session| session.cookie.clone());
        self.home_loading = true;
        self.home_scroll_requested = false;
        if reset {
            self.message = SharedString::from("正在从 B 站加载推荐视频…");
        } else {
            self.message = SharedString::from("正在加载更多推荐视频…");
        }
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(
                        async move { fetch_recommendations(page, cookie.as_deref()).await },
                    )
                    .await
                })
                .await;
            view.update(cx, |app, cx| {
                if app.home_generation != generation || app.active_tab != AppTab::Home {
                    return;
                }
                app.home_loading = false;
                app.loading = false;
                app.home_scroll_requested = false;
                match result {
                    Ok(videos) => {
                        let has_more = videos.has_more;
                        let videos = videos.videos;
                        let old_len = app.videos.len();
                        if reset {
                            app.videos = videos;
                        } else {
                            for video in videos {
                                if !app.videos.iter().any(|current| current.bvid == video.bvid) {
                                    app.videos.push(video);
                                }
                            }
                        }
                        app.home_page = page;
                        app.home_feed_mid = app.session.as_ref().map(|session| session.mid);
                        let added = app.videos.len().saturating_sub(old_len);
                        app.home_has_more = has_more && (reset || added > 0);
                        app.trim_home_items();
                        app.message = SharedString::from(format!(
                            "为你推荐 · 已加载 {} 个视频",
                            app.videos.len()
                        ));
                        app.start_cover_loading(cx);
                    }
                    Err(error) => {
                        app.home_has_more = false;
                        app.message = SharedString::from(format!("加载推荐失败：{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn maybe_load_home_page(&mut self, cx: &mut Context<Self>) {
        if self.active_tab != AppTab::Home
            || self.loading
            || self.home_loading
            || !self.home_has_more
            || self.videos.is_empty()
        {
            return;
        }

        let current_scroll_offset = {
            let state = self.home_scroll_handle.0.borrow();
            state.base_handle.offset().y
        };
        if self
            .home_last_scroll_offset
            .is_some_and(|last| last != current_scroll_offset)
        {
            self.home_scroll_requested = true;
        }
        self.home_last_scroll_offset = Some(current_scroll_offset);

        if !self.home_scroll_requested {
            return;
        }
        let near_bottom = {
            let state = self.home_scroll_handle.0.borrow();
            let offset = state.base_handle.offset();
            let max_offset = state.base_handle.max_offset();
            let remaining = max_offset.height + offset.y;
            max_offset.height > px(0.)
                && remaining <= px(VIDEO_ROW_HEIGHT * HOME_PREFETCH_ITEMS as f32)
        };
        if near_bottom {
            self.load_home_page(cx, false);
        }
    }

    fn queue_history_report(&mut self, cx: &mut Context<Self>, play_type: i64) {
        if self.history_report_in_flight || self.session.is_none() {
            return;
        }
        let Some(video) = self.selected_video_ref().cloned() else {
            return;
        };
        let status = self.player.status();
        if !status.time_pos.is_finite() || video.aid <= 0 || video.cid <= 0 {
            return;
        }
        let Some(cookie) = self.session.as_ref().map(|session| session.cookie.clone()) else {
            return;
        };
        self.history_report_in_flight = true;
        self.history_report_at = Instant::now();
        let progress = status.time_pos.max(0.);
        let aid = video.aid;
        let cid = video.cid;
        let heartbeat_video = video.clone();
        cx.spawn(async move |view, cx| {
            let (heartbeat_result, progress_result) = cx
                .background_spawn(async move {
                    let heartbeat_cookie = cookie.clone();
                    let progress_cookie = cookie;
                    futures::join!(
                        network::run(async move {
                            report_video_heartbeat(
                                &heartbeat_cookie,
                                &heartbeat_video,
                                progress,
                                play_type,
                            )
                            .await
                        }),
                        network::run(async move {
                            report_video_progress(&progress_cookie, aid, cid, progress).await
                        })
                    )
                })
                .await;
            view.update(cx, |app, _| {
                app.history_report_in_flight = false;
                if let Err(error) = heartbeat_result {
                    if std::env::var_os("BILIGUGA_API_DEBUG").is_some() {
                        eprintln!("[biliguga-history] {error}");
                    }
                }
                if let Err(error) = progress_result {
                    if std::env::var_os("BILIGUGA_API_DEBUG").is_some() {
                        eprintln!("[biliguga-history] {error}");
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn stop_current_playback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.queue_history_report(cx, 2);
        let frames = self.player.stop_playback();
        self.drop_player_frames(frames, window);
        self.selected = 0;
        self.playback = PlaybackState::Idle;
        self.playing_video = None;
        self.cloud_resume_progress = None;
        self.cloud_resume_applied = false;
        self.pending_seek = None;
        self.quality_options.clear();
        self.clear_collection(window);
        self.collection_loading = false;
        self.playback_request = self.playback_request.wrapping_add(1);
        self.controls_visible = false;
        self.controls_generation = self.controls_generation.wrapping_add(1);
    }

    fn note_home_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab == AppTab::Home && event.delta.pixel_delta(px(20.)).y < px(0.) {
            self.home_scroll_requested = true;
            cx.notify();
        }
    }

    fn drop_player_frames(&mut self, frames: Vec<Arc<RenderImage>>, window: &mut Window) {
        for frame in frames {
            let _ = window.drop_image(frame.clone());
            self.player.recycle_frame(frame);
        }
    }

    fn reset_comments(&mut self) {
        self.comments_generation = self.comments_generation.wrapping_add(1);
        self.comments.clear();
        self.comments_for.clear();
        self.comments_page = 0;
        self.comments_total = 0;
        self.comments_has_more = false;
        self.comments_loading = false;
        self.comments_error = None;
    }

    fn load_comments_for_current(&mut self, cx: &mut Context<Self>) {
        let Some(video) = self.selected_video_ref().cloned() else {
            self.reset_comments();
            return;
        };
        if self.comments_for != video.bvid {
            self.comments_generation = self.comments_generation.wrapping_add(1);
            self.comments.clear();
            self.comments_for = video.bvid.clone();
            self.comments_page = 0;
            self.comments_total = 0;
            self.comments_has_more = true;
            self.comments_loading = false;
            self.comments_error = None;
        }
        if self.comments_page == 0 && !self.comments_loading {
            self.load_comments_page(cx, true);
        }
    }

    fn load_comments_page(&mut self, cx: &mut Context<Self>, reset: bool) {
        let Some(video) = self.selected_video_ref().cloned() else {
            return;
        };
        if self.comments_loading || (!reset && !self.comments_has_more) {
            return;
        }
        if reset {
            self.comments_generation = self.comments_generation.wrapping_add(1);
            self.comments.clear();
            self.comments_page = 0;
            self.comments_total = 0;
            self.comments_has_more = true;
            self.comments_error = None;
        }
        let page = self.comments_page + 1;
        self.comments_loading = true;
        let generation = self.comments_generation;
        let bvid = video.bvid.clone();
        let cookie = self.session.as_ref().map(|session| session.cookie.clone());
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(
                        async move { fetch_comments(&video, cookie.as_deref(), page).await },
                    )
                    .await
                })
                .await;
            view.update(cx, |app, cx| {
                if app.comments_generation != generation
                    || app
                        .selected_video_ref()
                        .map(|current| current.bvid != bvid)
                        .unwrap_or(true)
                {
                    return;
                }
                app.comments_loading = false;
                match result {
                    Ok(comment_page) => {
                        app.comments.extend(comment_page.comments);
                        app.comments_page = page;
                        app.comments_total = comment_page.total;
                        app.comments_has_more = comment_page.has_more;
                        app.comments_error = None;
                    }
                    Err(error) => {
                        app.comments_has_more = false;
                        app.comments_error = Some(SharedString::from(error));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn retry_comments(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.load_comments_page(cx, true);
    }

    fn load_more_comments(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.load_comments_page(cx, false);
    }

    fn leave_current_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.debug_memory("before-tab-leave");
        if self.active_tab == AppTab::Home {
            self.home_generation = self.home_generation.wrapping_add(1);
            self.home_loading = false;
        }
        if self.active_tab == AppTab::Author {
            self.author_generation = self.author_generation.wrapping_add(1);
            self.author_loading = false;
        }
        self.stop_current_playback(window, cx);
        self.reset_cover_loading();
        self.reset_comments();
        self.debug_memory("after-tab-leave");
    }

    fn debug_memory_enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var_os("BILIGUGA_MEM_DEBUG").is_some())
    }

    fn debug_memory(&self, label: &str) {
        if !Self::debug_memory_enabled() {
            return;
        }

        let memory = fs::read_to_string("/proc/self/status")
            .ok()
            .map(|status| {
                let value = |name: &str| {
                    status
                        .lines()
                        .find_map(|line| line.strip_prefix(name))
                        .and_then(|value| value.split_whitespace().next())
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(0)
                };
                (
                    value("VmRSS:"),
                    value("RssAnon:"),
                    value("RssFile:"),
                    value("Threads:"),
                )
            })
            .unwrap_or_default();
        let covers = |videos: &[Video]| {
            videos
                .iter()
                .filter(|video| video.cover_image.is_some())
                .count()
        };
        let (current_frame, retired_frames) = self.player.debug_frame_counts();
        eprintln!(
            concat!(
                "[biliguga-mem] {} rss={}KB anon={}KB file={}KB threads={} tab={:?} ",
                "videos={{home:{}/{},search:{}/{},dynamic:{}/{},watch:{}/{},",
                "favorites:{}/{},history:{}/{}}} ",
                "player={{current:{},retired:{}}} covers={{home:{},search:{},dynamic:{},watch:{},favorites:{},history:{}}}"
            ),
            label,
            memory.0,
            memory.1,
            memory.2,
            memory.3,
            self.active_tab,
            self.videos.len(),
            covers(&self.videos),
            self.search_results.len(),
            covers(&self.search_results),
            self.dynamic_videos.len(),
            covers(&self.dynamic_videos),
            self.watch_later.len(),
            covers(&self.watch_later),
            self.favorites.len(),
            covers(&self.favorites),
            self.history.len(),
            covers(&self.history),
            current_frame,
            retired_frames,
            covers(&self.videos),
            covers(&self.search_results),
            covers(&self.dynamic_videos),
            covers(&self.watch_later),
            covers(&self.favorites),
            covers(&self.history),
        );
    }

    fn debug_image(label: &str, image: &Arc<RenderImage>, key: &str) {
        if std::env::var_os("BILIGUGA_IMAGE_DEBUG").is_some() {
            eprintln!(
                "[biliguga-image] {} key={} id={} strong_count={}",
                label,
                key,
                image.id.0,
                Arc::strong_count(image),
            );
        }
    }

    fn release_cover_images(videos: &mut Vec<Video>, window: &mut Window) {
        for video in videos {
            if let Some(image) = video.cover_image.take() {
                Self::debug_image("release", &image, &video.bvid);
                let _ = window.drop_image(image);
            }
        }
    }

    fn release_image(image: &mut Option<Arc<RenderImage>>, window: &mut Window) {
        if let Some(image) = image.take() {
            Self::debug_image("release-special", &image, "special");
            let _ = window.drop_image(image);
        }
    }

    fn reset_cover_loading(&mut self) {
        self.cover_cancelled.store(true, Ordering::Release);
        self.cover_cancelled = Arc::new(AtomicBool::new(false));
        self.cover_generation = self.cover_generation.wrapping_add(1);
        self.cover_loading = false;
    }

    fn reset_collection_cover_loading(&mut self) {
        self.collection_cover_cancelled
            .store(true, Ordering::Release);
        self.collection_cover_cancelled = Arc::new(AtomicBool::new(false));
        self.collection_cover_generation = self.collection_cover_generation.wrapping_add(1);
        self.collection_cover_loading = false;
    }

    fn clear_collection(&mut self, window: &mut Window) {
        self.reset_collection_cover_loading();
        if let Some(mut collection) = self.collection.take() {
            for episode in &mut collection.episodes {
                Self::release_image(&mut episode.cover_image, window);
            }
        }
        self.collection_expanded = true;
    }

    fn release_active_cover_images(&mut self, window: &mut Window) {
        match self.active_tab {
            AppTab::Home => Self::release_cover_images(&mut self.videos, window),
            AppTab::Search => Self::release_cover_images(&mut self.search_results, window),
            AppTab::Dynamic => Self::release_cover_images(&mut self.dynamic_videos, window),
            AppTab::WatchLater => Self::release_cover_images(&mut self.watch_later, window),
            AppTab::Favorites => Self::release_cover_images(&mut self.favorites, window),
            AppTab::History => Self::release_cover_images(&mut self.history, window),
            AppTab::Author => Self::release_cover_images(&mut self.author_videos, window),
            AppTab::Login => {}
        }
    }

    fn show_home(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab != AppTab::Home {
            self.leave_current_tab(window, cx);
            self.active_tab = AppTab::Home;
            self.message = SharedString::from("首页推荐");
            let current_mid = self.session.as_ref().map(|session| session.mid);
            if (!self.home_loading && self.home_feed_mid != current_mid)
                || (self.videos.is_empty() && !self.home_loading)
            {
                Self::release_cover_images(&mut self.videos, window);
                self.reset_cover_loading();
                self.load_home_page(cx, true);
            } else {
                self.start_cover_loading(cx);
            }
            cx.notify();
        }
    }

    fn show_search(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab != AppTab::Search {
            self.leave_current_tab(window, cx);
            self.active_tab = AppTab::Search;
            self.message = if self.search_results.is_empty() {
                SharedString::from("输入关键词搜索视频")
            } else {
                SharedString::from(format!("找到 {} 个视频", self.search_results.len()))
            };
            self.start_cover_loading(cx);
            cx.notify();
        }
    }

    fn show_dynamic(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_current_tab(window, cx);
        self.active_tab = AppTab::Dynamic;
        if self.session.is_none() {
            self.message = SharedString::from("请先登录后查看动态");
            cx.notify();
            return;
        }
        if self.dynamic_videos.is_empty() && !self.dynamic_loading {
            self.load_dynamic(cx, true);
        } else {
            self.message =
                SharedString::from(format!("动态 · {} 个视频", self.dynamic_videos.len()));
            self.start_cover_loading(cx);
            cx.notify();
        }
    }

    fn load_dynamic(&mut self, cx: &mut Context<Self>, reset: bool) {
        let Some(session) = &self.session else {
            self.message = SharedString::from("请先登录后查看动态");
            cx.notify();
            return;
        };
        let cookie = session.cookie.clone();
        if self.dynamic_loading {
            return;
        }
        if reset {
            self.dynamic_videos.clear();
            self.reset_cover_loading();
            self.selected = 0;
        }
        self.dynamic_loading = true;
        self.message = if reset {
            SharedString::from("正在加载动态…")
        } else {
            SharedString::from("正在加载更多动态…")
        };
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(async move { fetch_dynamic_feed(&cookie).await }).await
                })
                .await;
            view.update(cx, |app, cx| {
                app.dynamic_loading = false;
                match result {
                    Ok(videos) => {
                        app.dynamic_videos = videos;
                        app.message = SharedString::from(format!(
                            "动态 · {} 个视频",
                            app.dynamic_videos.len()
                        ));
                        if app.active_tab == AppTab::Dynamic {
                            app.start_cover_loading(cx);
                        }
                    }
                    Err(error) => {
                        app.message = SharedString::from(format!("加载动态失败：{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_dynamic(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.release_active_cover_images(window);
        self.load_dynamic(cx, true);
    }

    fn show_history(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_current_tab(window, cx);
        self.active_tab = AppTab::History;
        if self.session.is_none() {
            self.message = SharedString::from("请先登录后查看观看历史");
            cx.notify();
            return;
        }
        if self.history.is_empty() && !self.history_loading {
            self.load_history(cx);
        } else {
            self.message = SharedString::from(format!("观看历史 · {} 个视频", self.history.len()));
            self.start_cover_loading(cx);
            cx.notify();
        }
    }

    fn load_history(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            self.message = SharedString::from("请先登录后查看观看历史");
            cx.notify();
            return;
        };
        let cookie = session.cookie.clone();
        self.history_loading = true;
        self.history.clear();
        self.reset_cover_loading();
        self.selected = 0;
        self.message = SharedString::from("正在加载观看历史…");
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(async move { fetch_history(&cookie).await }).await
                })
                .await;
            view.update(cx, |app, cx| {
                app.history_loading = false;
                match result {
                    Ok(videos) => {
                        let count = videos.len();
                        app.history = videos;
                        app.message = SharedString::from(format!("观看历史 · {count} 个视频"));
                        if app.active_tab == AppTab::History {
                            app.start_cover_loading(cx);
                        }
                    }
                    Err(error) => {
                        app.message = SharedString::from(format!("加载观看历史失败：{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_history(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.history_loading {
            self.release_active_cover_images(window);
            self.load_history(cx);
        }
    }

    fn show_watch_later(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_current_tab(window, cx);
        self.active_tab = AppTab::WatchLater;
        if self.session.is_none() {
            self.message = SharedString::from("请先登录后查看稍后再看");
            cx.notify();
            return;
        }
        if self.watch_later.is_empty() && !self.watch_later_loading {
            self.load_watch_later(cx);
        } else {
            self.message =
                SharedString::from(format!("稍后再看 · {} 个视频", self.watch_later.len()));
            self.start_cover_loading(cx);
            cx.notify();
        }
    }

    fn load_watch_later(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            self.message = SharedString::from("请先登录后查看稍后再看");
            cx.notify();
            return;
        };
        let cookie = session.cookie.clone();
        self.watch_later_loading = true;
        self.watch_later.clear();
        self.reset_cover_loading();
        self.selected = 0;
        self.message = SharedString::from("正在加载稍后再看…");
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(async move { fetch_watch_later(&cookie).await }).await
                })
                .await;
            view.update(cx, |app, cx| {
                app.watch_later_loading = false;
                match result {
                    Ok(videos) => {
                        let count = videos.len();
                        app.watch_later = videos;
                        app.message = SharedString::from(format!("稍后再看 · {count} 个视频"));
                        if app.active_tab == AppTab::WatchLater {
                            app.start_cover_loading(cx);
                        }
                    }
                    Err(error) => {
                        app.message = SharedString::from(format!("加载稍后再看失败：{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_watch_later(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.watch_later_loading {
            self.release_active_cover_images(window);
            self.load_watch_later(cx);
        }
    }

    fn show_favorites(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_current_tab(window, cx);
        self.active_tab = AppTab::Favorites;
        if self.session.is_none() {
            self.message = SharedString::from("请先登录后查看收藏夹");
            cx.notify();
            return;
        }
        if self.favorites.is_empty() && !self.favorites_loading {
            self.load_favorites(cx);
        } else {
            self.message = SharedString::from(format!("收藏夹 · {} 个视频", self.favorites.len()));
            self.start_cover_loading(cx);
            cx.notify();
        }
    }

    fn load_favorites(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            self.message = SharedString::from("请先登录后查看收藏夹");
            cx.notify();
            return;
        };
        let cookie = session.cookie.clone();
        let mid = session.mid;
        self.favorites_loading = true;
        self.favorites.clear();
        self.reset_cover_loading();
        self.selected = 0;
        self.message = SharedString::from("正在加载收藏夹…");
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(async move { fetch_favorites(&cookie, mid).await }).await
                })
                .await;
            view.update(cx, |app, cx| {
                app.favorites_loading = false;
                match result {
                    Ok(videos) => {
                        let count = videos.len();
                        app.favorites = videos;
                        app.message = SharedString::from(format!("收藏夹 · {count} 个视频"));
                        if app.active_tab == AppTab::Favorites {
                            app.start_cover_loading(cx);
                        }
                    }
                    Err(error) => {
                        app.message = SharedString::from(format!("加载收藏夹失败：{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_favorites(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.favorites_loading {
            self.release_active_cover_images(window);
            self.load_favorites(cx);
        }
    }

    fn open_author_videos(
        &mut self,
        mid: i64,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if mid <= 0 {
            self.message = SharedString::from("该视频没有返回作者 UID");
            cx.notify();
            return;
        }
        Self::release_cover_images(&mut self.author_videos, window);
        self.reset_cover_loading();
        self.active_tab = AppTab::Author;
        self.author_mid = mid;
        self.author_name = name;
        self.author_page = 0;
        self.author_has_more = true;
        self.author_generation = self.author_generation.wrapping_add(1);
        self.author_videos.clear();
        self.selected = 0;
        self.load_author_page(cx, true);
    }

    fn load_author_page(&mut self, cx: &mut Context<Self>, reset: bool) {
        if self.author_mid <= 0 || self.author_loading || (!reset && !self.author_has_more) {
            return;
        }
        if reset {
            self.author_page = 0;
            self.author_has_more = true;
            self.author_videos.clear();
        }
        let page = self.author_page + 1;
        let generation = self.author_generation;
        let mid = self.author_mid;
        let cookie = self.session.as_ref().map(|session| session.cookie.clone());
        self.author_loading = true;
        self.message = SharedString::from(if reset {
            format!("正在加载 {} 的投稿…", self.author_name)
        } else {
            format!("正在加载 {} 的更多投稿…", self.author_name)
        });
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(
                        async move { fetch_author_videos(mid, page, cookie.as_deref()).await },
                    )
                    .await
                })
                .await;
            view.update(cx, |app, cx| {
                if app.author_generation != generation || app.active_tab != AppTab::Author {
                    return;
                }
                app.author_loading = false;
                match result {
                    Ok(result) => {
                        let mut seen = app
                            .author_videos
                            .iter()
                            .map(|video| video.bvid.clone())
                            .collect::<std::collections::HashSet<_>>();
                        for video in result.videos {
                            if seen.insert(video.bvid.clone()) {
                                app.author_videos.push(video);
                            }
                        }
                        app.author_page = result.page;
                        app.author_has_more = result.has_more;
                        app.message = SharedString::from(format!(
                            "{} · 已加载 {} 个视频{}",
                            app.author_name,
                            app.author_videos.len(),
                            if app.author_has_more {
                                " · 可继续加载"
                            } else {
                                ""
                            }
                        ));
                        app.start_cover_loading(cx);
                    }
                    Err(error) => {
                        app.message = SharedString::from(format!("加载作者视频失败：{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_author_videos(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.author_loading {
            return;
        }
        Self::release_cover_images(&mut self.author_videos, window);
        self.reset_cover_loading();
        self.author_generation = self.author_generation.wrapping_add(1);
        self.load_author_page(cx, true);
    }

    fn load_more_author_videos(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.load_author_page(cx, false);
    }

    fn show_login(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab != AppTab::Login {
            self.leave_current_tab(window, cx);
            self.active_tab = AppTab::Login;
            self.message = SharedString::from("登录账号后可使用更多 B 站功能");
            cx.notify();
        }
        if self.session.is_none() && self.login_image.is_none() && !self.login_loading {
            self.start_login(window, cx);
        }
        if self.session.is_some() && self.account_avatar.is_none() {
            self.start_avatar_loading(cx);
        }
    }

    fn start_avatar_loading(&self, cx: &mut Context<Self>) {
        let Some(face_url) = self
            .session
            .as_ref()
            .map(|session| session.face.clone())
            .filter(|face| !face.is_empty())
        else {
            return;
        };
        cx.spawn(async move |view, cx| {
            let image = cx
                .background_spawn(async move {
                    network::run(async move { download_avatar(&face_url).await }).await
                })
                .await;
            if let Some(image) = image {
                view.update(cx, |app, cx| {
                    app.account_avatar = Some(image);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn start_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.login_loading {
            return;
        }
        self.login_generation = self.login_generation.wrapping_add(1);
        let generation = self.login_generation;
        self.login_loading = true;
        Self::release_image(&mut self.login_image, window);
        self.login_key = None;
        self.login_status = SharedString::from("正在获取登录二维码…");
        cx.notify();

        cx.spawn(async move |view, cx| {
            let qr = cx
                .background_spawn(async { network::run(login::fetch_qr_code()).await })
                .await;
            let qr = match qr {
                Ok(qr) => qr,
                Err(error) => {
                    view.update(cx, |app, cx| {
                        if app.login_generation == generation {
                            app.login_loading = false;
                            app.login_status = SharedString::from(format!("登录失败：{error}"));
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            let key = qr.key.clone();
            let active = view
                .update(cx, |app, cx| {
                    if app.login_generation != generation {
                        return false;
                    }
                    app.login_image = Some(qr.image);
                    app.login_key = Some(key.clone());
                    app.login_status = SharedString::from("请使用哔哩哔哩 App 扫码");
                    cx.notify();
                    true
                })
                .ok()
                .unwrap_or(false);
            if !active {
                return;
            }

            loop {
                Timer::after(Duration::from_secs(2)).await;
                let active = view
                    .update(cx, |app, _| app.login_generation == generation)
                    .ok()
                    .unwrap_or(false);
                if !active {
                    return;
                }
                let poll_key = key.clone();
                let result = cx
                    .background_spawn(async move {
                        network::run(async move { login::poll_qr_code(&poll_key).await }).await
                    })
                    .await;
                match result {
                    Ok(PollResult::Waiting) => {}
                    Ok(PollResult::Scanned) => {
                        view.update(cx, |app, cx| {
                            if app.login_generation == generation {
                                app.login_status = SharedString::from("已扫码，请在手机上确认登录");
                                cx.notify();
                            }
                        })
                        .ok();
                    }
                    Ok(PollResult::Expired) => {
                        view.update(cx, |app, cx| {
                            if app.login_generation == generation {
                                app.login_loading = false;
                                app.login_status = SharedString::from("二维码已过期，请点击刷新");
                                cx.notify();
                            }
                        })
                        .ok();
                        return;
                    }
                    Ok(PollResult::LoggedIn(session)) => {
                        let save_error = login::save_session(&session).err();
                        view.update(cx, |app, cx| {
                            if app.login_generation == generation {
                                app.session = Some(session.clone());
                                app.login_loading = false;
                                app.login_image = None;
                                app.login_key = None;
                                app.login_status =
                                    SharedString::from(if let Some(error) = save_error {
                                        format!("登录成功，但保存登录状态失败：{error}")
                                    } else {
                                        format!("登录成功：{}", session.username)
                                    });
                                cx.notify();
                            }
                        })
                        .ok();
                        return;
                    }
                    Err(error) => {
                        view.update(cx, |app, cx| {
                            if app.login_generation == generation {
                                app.login_status =
                                    SharedString::from(format!("等待登录确认（{error}）"));
                                cx.notify();
                            }
                        })
                        .ok();
                    }
                }
            }
        })
        .detach();
    }

    fn refresh_login(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.login_loading {
            return;
        }
        self.start_login(window, cx);
    }

    fn logout(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        login::clear_session();
        self.release_active_cover_images(window);
        Self::release_cover_images(&mut self.videos, window);
        Self::release_cover_images(&mut self.search_results, window);
        Self::release_cover_images(&mut self.dynamic_videos, window);
        Self::release_cover_images(&mut self.watch_later, window);
        Self::release_cover_images(&mut self.favorites, window);
        Self::release_cover_images(&mut self.history, window);
        Self::release_cover_images(&mut self.author_videos, window);
        Self::release_image(&mut self.account_avatar, window);
        Self::release_image(&mut self.login_image, window);
        self.session = None;
        self.home_feed_mid = None;
        self.reset_cover_loading();
        self.history.clear();
        self.watch_later.clear();
        self.favorites.clear();
        self.dynamic_videos.clear();
        self.author_videos.clear();
        self.author_mid = 0;
        self.author_name.clear();
        self.login_image = None;
        self.login_key = None;
        self.login_status = SharedString::from("已退出登录");
        cx.notify();
    }

    fn add_video_to_watch_later(
        &mut self,
        index: usize,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = &self.session else {
            self.message = SharedString::from("请先登录后使用稍后再看");
            cx.notify();
            return;
        };
        let Some(video) = self.current_videos().get(index).cloned() else {
            return;
        };
        if video.aid <= 0 {
            self.message = SharedString::from("这个视频没有可用的 AV 号");
            cx.notify();
            return;
        }
        let cookie = session.cookie.clone();
        let title = video.title.clone();
        self.message = SharedString::from(format!("正在添加：{title}"));
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(async move { add_to_watch_later(&cookie, video.aid).await }).await
                })
                .await;
            view.update(cx, |app, cx| {
                app.message = SharedString::from(match result {
                    Ok(()) => "已添加到稍后再看".to_string(),
                    Err(error) => format!("添加稍后再看失败：{error}"),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn add_video_to_favorites(
        &mut self,
        index: usize,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = &self.session else {
            self.message = SharedString::from("请先登录后使用收藏功能");
            cx.notify();
            return;
        };
        let Some(video) = self.current_videos().get(index).cloned() else {
            return;
        };
        if video.aid <= 0 {
            self.message = SharedString::from("这个视频没有可用的 AV 号");
            cx.notify();
            return;
        }
        let cookie = session.cookie.clone();
        let mid = session.mid;
        let title = video.title.clone();
        self.message = SharedString::from(format!("正在收藏：{title}"));
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(async move { add_to_favorites(&cookie, mid, video.aid).await })
                        .await
                })
                .await;
            view.update(cx, |app, cx| {
                app.message = SharedString::from(match result {
                    Ok(()) => "已收藏到默认收藏夹".to_string(),
                    Err(error) => format!("收藏失败：{error}"),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn like_current_video(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            self.message = SharedString::from("请先登录后点赞");
            cx.notify();
            return;
        };
        let Some(video) = self.selected_video_ref().cloned() else {
            return;
        };
        if video.aid <= 0 {
            self.message = SharedString::from("这个视频没有可用的 AV 号");
            cx.notify();
            return;
        }
        let cookie = session.cookie.clone();
        self.message = SharedString::from("正在点赞…");
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(async move { like_video(&cookie, video.aid).await }).await
                })
                .await;
            view.update(cx, |app, cx| {
                app.message = SharedString::from(match result {
                    Ok(()) => "已点赞".to_string(),
                    Err(error) => format!("点赞失败：{error}"),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn coin_current_video(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            self.message = SharedString::from("请先登录后投币");
            cx.notify();
            return;
        };
        let Some(video) = self.selected_video_ref().cloned() else {
            return;
        };
        if video.aid <= 0 {
            self.message = SharedString::from("这个视频没有可用的 AV 号");
            cx.notify();
            return;
        }
        let cookie = session.cookie.clone();
        self.message = SharedString::from("正在投币…");
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(async move { coin_video(&cookie, video.aid).await }).await
                })
                .await;
            view.update(cx, |app, cx| {
                app.message = SharedString::from(match result {
                    Ok(()) => "已投 1 枚硬币".to_string(),
                    Err(error) => format!("投币失败：{error}"),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save_current_to_watch_later(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = self.selected;
        self.add_video_to_watch_later(index, event, window, cx);
    }

    fn save_current_to_favorites(
        &mut self,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = self.selected;
        self.add_video_to_favorites(index, event, window, cx);
    }

    fn start_collection_cover_loading(&mut self, cx: &mut Context<Self>) {
        const MAX_IN_FLIGHT: usize = 8;

        if self.collection_cover_loading {
            return;
        }
        let Some(collection) = &self.collection else {
            return;
        };
        let collection_id = collection.id;
        let covers = collection
            .episodes
            .iter()
            .enumerate()
            .filter(|(_, episode)| episode.cover_image.is_none())
            .map(|(index, episode)| (index, episode.bvid.clone(), episode.cover.clone()))
            .collect::<Vec<_>>();
        if covers.is_empty() {
            return;
        }

        self.collection_cover_loading = true;
        let generation = self.collection_cover_generation;
        let cancelled = self.collection_cover_cancelled.clone();
        cx.spawn(async move |view, cx| {
            type CoverFuture = BoxFuture<'static, (usize, String, Option<Arc<RenderImage>>)>;

            let make_future = |index: usize, bvid: String, cover_url: String| -> CoverFuture {
                let cancelled = cancelled.clone();
                async move {
                    let image = match queue_cover_download(cover_url, cancelled) {
                        Some(receiver) => receiver.await.ok().flatten(),
                        None => None,
                    };
                    (index, bvid, image)
                }
                .boxed()
            };

            let mut covers = covers.into_iter();
            let mut pending = FuturesUnordered::<CoverFuture>::new();
            for _ in 0..MAX_IN_FLIGHT {
                if let Some((index, bvid, cover_url)) = covers.next() {
                    pending.push(make_future(index, bvid, cover_url));
                }
            }

            while let Some((index, bvid, image)) = pending.next().await {
                let keep_loading = view
                    .update(cx, |app, cx| {
                        if app.collection_cover_generation != generation {
                            return false;
                        }
                        let is_same_collection = app
                            .collection
                            .as_ref()
                            .filter(|collection| collection.id == collection_id)
                            .and_then(|collection| collection.episodes.get(index))
                            .map(|episode| episode.bvid == bvid)
                            .unwrap_or(false);
                        if is_same_collection {
                            if let Some(image) = image {
                                Self::debug_image("store-collection", &image, &bvid);
                                if let Some(episode) = app
                                    .collection
                                    .as_mut()
                                    .and_then(|collection| collection.episodes.get_mut(index))
                                {
                                    episode.cover_image = Some(image);
                                    cx.notify();
                                }
                            }
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_loading {
                    cancelled.store(true, Ordering::Release);
                    return;
                }
                if let Some((index, bvid, cover_url)) = covers.next() {
                    pending.push(make_future(index, bvid, cover_url));
                }
            }

            view.update(cx, |app, _| {
                if app.collection_cover_generation == generation {
                    app.collection_cover_loading = false;
                }
            })
            .ok();
        })
        .detach();
    }

    fn start_cover_loading(&mut self, cx: &mut Context<Self>) {
        const MAX_IN_FLIGHT: usize = 8;

        if self.cover_loading {
            return;
        }
        let covers = self
            .current_videos()
            .iter()
            .enumerate()
            .filter(|(_, video)| video.cover_image.is_none())
            .map(|(index, video)| (index, video.bvid.clone(), video.cover.clone()))
            .collect::<Vec<_>>();
        if covers.is_empty() {
            return;
        }

        self.cover_loading = true;
        let generation = self.cover_generation;
        let cancelled = self.cover_cancelled.clone();
        cx.spawn(async move |view, cx| {
            type CoverFuture = BoxFuture<'static, (usize, String, Option<Arc<RenderImage>>)>;

            let make_future = |index: usize, bvid: String, cover_url: String| -> CoverFuture {
                let cancelled = cancelled.clone();
                async move {
                    let image = match queue_cover_download(cover_url, cancelled) {
                        Some(receiver) => receiver.await.ok().flatten(),
                        None => None,
                    };
                    (index, bvid, image)
                }
                .boxed()
            };

            let mut covers = covers.into_iter();
            let mut pending = FuturesUnordered::<CoverFuture>::new();
            for _ in 0..MAX_IN_FLIGHT {
                if let Some((index, bvid, cover_url)) = covers.next() {
                    pending.push(make_future(index, bvid, cover_url));
                }
            }

            while let Some((index, bvid, image)) = pending.next().await {
                let keep_loading = view
                    .update(cx, |app, cx| {
                        if app.cover_generation != generation {
                            return false;
                        }
                        if let Some(image) = image {
                            let is_same_video = app
                                .current_videos()
                                .get(index)
                                .map(|video| video.bvid == bvid)
                                .unwrap_or(false);
                            if is_same_video {
                                Self::debug_image("store", &image, &bvid);
                                if let Some(video) = app.current_videos_mut().get_mut(index) {
                                    video.cover_image = Some(image);
                                    cx.notify();
                                }
                            }
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_loading {
                    cancelled.store(true, Ordering::Release);
                    return;
                }
                if let Some((index, bvid, cover_url)) = covers.next() {
                    pending.push(make_future(index, bvid, cover_url));
                }
            }

            view.update(cx, |app, cx| {
                if app.cover_generation == generation {
                    app.cover_loading = false;
                    let has_pending_covers = app
                        .current_videos()
                        .iter()
                        .any(|video| video.cover_image.is_none());
                    if has_pending_covers {
                        app.start_cover_loading(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn search_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key.eq_ignore_ascii_case("enter") {
            self.start_search(window, cx);
        }
    }

    fn submit_search(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.start_search(window, cx);
    }

    fn start_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keyword = self.search_input.read(cx).content.trim().to_string();
        if keyword.is_empty() {
            self.message = SharedString::from("请输入搜索关键词");
            cx.notify();
            return;
        }
        self.active_tab = AppTab::Search;
        self.search_query = keyword.clone();
        self.feed_generation = self.feed_generation.wrapping_add(1);
        let feed_generation = self.feed_generation;
        Self::release_cover_images(&mut self.search_results, window);
        self.loading = true;
        self.search_results.clear();
        self.reset_cover_loading();
        self.selected = 0;
        self.queue_history_report(cx, 2);
        let frames = self.player.stop_playback();
        self.drop_player_frames(frames, window);
        self.playback = PlaybackState::Idle;
        self.playing_video = None;
        self.cloud_resume_progress = None;
        self.cloud_resume_applied = false;
        self.pending_seek = None;
        self.quality_options.clear();
        self.playback_request = self.playback_request.wrapping_add(1);
        self.controls_visible = false;
        self.controls_generation = self.controls_generation.wrapping_add(1);
        self.message = SharedString::from(format!("正在搜索：{keyword}"));
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(async move { fetch_search_results(&keyword).await }).await
                })
                .await;
            view.update(cx, |app, cx| {
                if app.feed_generation != feed_generation {
                    return;
                }
                app.loading = false;
                match result {
                    Ok(videos) => {
                        app.message = SharedString::from(format!("找到 {} 个视频", videos.len()));
                        app.search_results = videos;
                        if app.active_tab == AppTab::Search {
                            app.start_cover_loading(cx);
                        }
                    }
                    Err(error) => {
                        app.message = SharedString::from(format!("搜索失败：{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab = AppTab::Home;
        Self::release_cover_images(&mut self.videos, window);
        self.search_query.clear();
        self.search_input.update(cx, |input, cx| input.reset(cx));
        self.reset_cover_loading();
        self.queue_history_report(cx, 2);
        let frames = self.player.stop_playback();
        self.drop_player_frames(frames, window);
        self.playback = PlaybackState::Idle;
        self.playing_video = None;
        self.cloud_resume_progress = None;
        self.cloud_resume_applied = false;
        self.pending_seek = None;
        self.quality_options.clear();
        self.playback_request = self.playback_request.wrapping_add(1);
        self.controls_visible = false;
        self.controls_generation = self.controls_generation.wrapping_add(1);
        self.load_home_page(cx, true);
    }

    fn select_video(
        &mut self,
        index: usize,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(video) = self.current_videos().get(index).cloned() else {
            return;
        };
        let same_video = self
            .selected_video_ref()
            .map(|current| {
                current.bvid == video.bvid
                    && (video.cid <= 0 || current.cid <= 0 || current.cid == video.cid)
            })
            .unwrap_or(false);
        if same_video {
            if self.playback == PlaybackState::Paused {
                self.player.set_pause(false);
                self.playback = PlaybackState::Playing;
                self.message = SharedString::from("继续播放");
                cx.notify();
            } else if self.playback == PlaybackState::Idle {
                self.begin_play_selected(cx);
            }
            return;
        }
        self.debug_memory("before-video-switch");
        self.queue_history_report(cx, 2);
        self.selected = index;
        self.pin_playing_video(&video);
        let frames = self.player.stop_playback();
        self.drop_player_frames(frames, window);
        self.debug_memory("after-video-stop");
        self.playback = PlaybackState::Idle;
        self.pending_seek = None;
        self.quality_options.clear();
        self.clear_collection(window);
        self.collection_loading = true;
        self.playback_request = self.playback_request.wrapping_add(1);
        self.controls_visible = false;
        self.controls_generation = self.controls_generation.wrapping_add(1);
        self.speed_menu_open = false;
        self.quality_menu_open = false;
        self.begin_play_selected(cx);
    }

    fn select_collection_episode(
        &mut self,
        index: usize,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(video) = self
            .collection
            .as_ref()
            .and_then(|collection| collection.episodes.get(index))
            .cloned()
        else {
            return;
        };
        let same_video = self
            .selected_video_ref()
            .map(|current| {
                current.bvid == video.bvid
                    && (video.cid <= 0 || current.cid <= 0 || current.cid == video.cid)
            })
            .unwrap_or(false);
        if same_video {
            if self.playback == PlaybackState::Paused {
                self.player.set_pause(false);
                self.playback = PlaybackState::Playing;
                self.message = SharedString::from("继续播放");
                cx.notify();
            } else if self.playback == PlaybackState::Idle {
                self.begin_play_selected(cx);
            }
            return;
        }
        self.queue_history_report(cx, 2);
        self.pin_playing_video(&video);
        let frames = self.player.stop_playback();
        self.drop_player_frames(frames, window);
        self.playback = PlaybackState::Idle;
        self.pending_seek = None;
        self.quality_options.clear();
        self.clear_collection(window);
        self.collection_loading = true;
        self.playback_request = self.playback_request.wrapping_add(1);
        self.controls_visible = false;
        self.controls_generation = self.controls_generation.wrapping_add(1);
        self.speed_menu_open = false;
        self.quality_menu_open = false;
        self.begin_play_selected(cx);
    }

    fn play_selected(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.playback == PlaybackState::Paused {
            self.player.set_pause(false);
            self.playback = PlaybackState::Playing;
            self.message = SharedString::from("继续播放");
            cx.notify();
            return;
        }
        self.begin_play_selected(cx);
    }

    fn begin_play_selected(&mut self, cx: &mut Context<Self>) {
        if self.playback == PlaybackState::Buffering {
            return;
        }
        let Some(video) = self.selected_video_ref().cloned() else {
            self.message = SharedString::from("还没有可播放的视频");
            cx.notify();
            return;
        };
        self.pin_playing_video(&video);
        self.load_comments_for_current(cx);
        self.cloud_resume_progress = None;
        self.cloud_resume_applied = false;
        self.collection_loading = true;
        self.history_report_at = Instant::now();
        self.playback = PlaybackState::Buffering;
        self.message = SharedString::from("正在向 B 站获取播放地址…");
        cx.notify();
        let video_bvid = video.bvid.clone();
        let cookie = self.session.as_ref().map(|session| session.cookie.clone());
        self.playback_request = self.playback_request.wrapping_add(1);
        let playback_request = self.playback_request;
        let quality = self.quality;
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    network::run(async move {
                        let context = fetch_video_context(&video, cookie.as_deref()).await.ok();
                        let mut resolved_video = video.clone();
                        if let Some(context) = &context {
                            resolved_video.aid = context.aid;
                            resolved_video.cid = context.cid;
                            resolved_video.uploader = context.uploader.clone();
                            resolved_video.uploader_mid = context.uploader_mid;
                        }
                        let result =
                            resolve_play_url(&resolved_video, cookie.as_deref(), quality).await?;
                        let mut resolved_video = video.clone();
                        resolved_video.cid = result.cid;
                        resolved_video.aid = result.aid;
                        let progress =
                            fetch_last_play_progress(&resolved_video, cookie.as_deref()).await;
                        Ok::<_, String>((result, progress, context))
                    })
                    .await
                })
                .await;
            let message = match result {
                Ok((play_url, progress, context)) => {
                    view.update(cx, |app, cx| {
                        if app.playback_request != playback_request
                            || app
                                .selected_video_ref()
                                .map(|current| current.bvid != video_bvid)
                                .unwrap_or(true)
                        {
                            return;
                        }
                        let explicit_quality = !app.quality_options.is_empty();
                        if explicit_quality
                            && play_url.actual_quality > 0
                            && play_url.actual_quality != quality
                        {
                            app.playback = PlaybackState::Idle;
                            app.message = SharedString::from(format!(
                                "B 站没有返回 {}，实际返回 {}",
                                quality_label(quality),
                                quality_label(play_url.actual_quality),
                            ));
                            cx.notify();
                            return;
                        }
                        if let Some(playing_video) = app.playing_video.as_mut() {
                            playing_video.cid = play_url.cid;
                            playing_video.aid = play_url.aid;
                            if let Some(context) = &context {
                                playing_video.uploader = context.uploader.clone();
                                playing_video.uploader_mid = context.uploader_mid;
                            }
                        }
                        app.reset_collection_cover_loading();
                        app.collection = context.and_then(|context| context.collection);
                        app.collection_expanded = true;
                        app.start_collection_cover_loading(cx);
                        app.collection_loading = false;
                        app.quality_options = play_url.qualities.clone();
                        if play_url.actual_quality > 0 {
                            app.quality = play_url.actual_quality;
                        }
                        if let Some(progress) = progress {
                            app.cloud_resume_progress = Some(progress as f64);
                        }
                        app.debug_memory("before-video-load");
                        app.player.load(play_url.url, app.volume, app.speed);
                        app.cloud_resume_applied = false;
                        app.playback = PlaybackState::Buffering;
                        app.message = SharedString::from("已获取播放地址，libmpv 正在缓冲");
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Err(error) => format!("播放失败：{error}"),
            };
            view.update(cx, |app, cx| {
                if app.playback_request != playback_request {
                    return;
                }
                app.playback = PlaybackState::Idle;
                app.collection_loading = false;
                app.message = SharedString::from(message);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_pause_state(&mut self, cx: &mut Context<Self>) {
        match self.playback {
            PlaybackState::Playing => {
                self.queue_history_report(cx, 2);
                self.player.set_pause(true);
                self.playback = PlaybackState::Paused;
                self.message = SharedString::from("已暂停");
                cx.notify();
            }
            PlaybackState::Paused => {
                self.player.set_pause(false);
                self.playback = PlaybackState::Playing;
                self.message = SharedString::from("继续播放");
                cx.notify();
            }
            PlaybackState::Buffering => {
                self.message = SharedString::from("视频正在缓冲");
                cx.notify();
            }
            PlaybackState::Idle => self.begin_play_selected(cx),
        }
    }

    fn toggle_pause(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_pause_state(cx);
    }

    fn set_volume_from_position(
        &mut self,
        position: gpui::Point<Pixels>,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if bounds.size.width <= px(0.) {
            return;
        }
        let volume =
            (((position.x - bounds.origin.x) / bounds.size.width).clamp(0., 1.) * 100.) as f64;
        self.volume = volume.clamp(0., 100.);
        self.player.set_volume(self.volume);
        cx.notify();
    }

    fn persist_player_settings(&self) {
        save_player_settings(self.volume, self.speed);
    }

    fn toggle_speed_menu(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.speed_menu_open = !self.speed_menu_open;
        self.quality_menu_open = false;
        cx.notify();
    }

    fn select_speed(&mut self, speed: f64, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.speed = speed;
        self.player.set_speed(self.speed);
        self.persist_player_settings();
        self.speed_menu_open = false;
        cx.notify();
    }

    fn toggle_quality_menu(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.quality_menu_open = !self.quality_menu_open;
        self.speed_menu_open = false;
        cx.notify();
    }

    fn select_quality(
        &mut self,
        quality: u32,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.quality_menu_open = false;
        if self.quality == quality {
            cx.notify();
            return;
        }
        self.quality = quality;
        let status = self.player.status();
        if self.playback != PlaybackState::Idle {
            if status.time_pos.is_finite() {
                self.pending_seek = Some(status.time_pos.max(0.));
            }
            self.queue_history_report(cx, 2);
            let frames = self.player.stop_playback();
            self.drop_player_frames(frames, window);
            self.playback = PlaybackState::Idle;
            self.playback_request = self.playback_request.wrapping_add(1);
            self.begin_play_selected(cx);
        }
        cx.notify();
    }

    fn toggle_player_fullscreen(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.screen_fullscreen {
            self.screen_fullscreen = false;
            window.toggle_fullscreen();
            cx.notify();
            return;
        }
        self.player_fullscreen = !self.player_fullscreen;
        self.controls_visible = true;
        cx.notify();
    }

    fn exit_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen_fullscreen {
            self.screen_fullscreen = false;
            window.toggle_fullscreen();
        }
        self.player_fullscreen = false;
        self.controls_visible = true;
        self.controls_generation = self.controls_generation.wrapping_add(1);
        cx.notify();
    }

    fn handle_global_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.is_held {
            return;
        }
        let key = event.keystroke.key.as_str();
        let search_focused = self.search_input.read(cx).is_focused(window);
        if key.eq_ignore_ascii_case("escape") || key.eq_ignore_ascii_case("esc") {
            if self.player_fullscreen || self.screen_fullscreen {
                self.exit_fullscreen(window, cx);
            }
        } else if key.eq_ignore_ascii_case("f")
            && !event.keystroke.modifiers.modified()
            && !search_focused
        {
            self.toggle_screen_fullscreen_state(window, cx);
        } else if (key.eq_ignore_ascii_case("space") || key == " ")
            && !event.keystroke.modifiers.modified()
            && !search_focused
        {
            self.toggle_pause_state(cx);
        }
    }

    fn toggle_screen_fullscreen(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_screen_fullscreen_state(window, cx);
    }

    fn toggle_screen_fullscreen_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.screen_fullscreen = !self.screen_fullscreen;
        self.player_fullscreen = self.screen_fullscreen;
        window.toggle_fullscreen();
        self.controls_visible = true;
        self.controls_generation = self.controls_generation.wrapping_add(1);
        cx.notify();
    }

    fn seek_percent(&mut self, percent: f64, cx: &mut Context<Self>) {
        self.player.seek_percent(percent);
        cx.notify();
    }

    fn show_player_controls(&mut self, _: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.controls_visible = true;
        self.controls_generation = self.controls_generation.wrapping_add(1);
        let generation = self.controls_generation;
        cx.notify();
        cx.spawn(async move |view, cx| {
            Timer::after(Duration::from_millis(2400)).await;
            view.update(cx, |app, cx| {
                if app.controls_generation == generation {
                    app.controls_visible = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(64.))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .bg(rgb(0x2f343e))
            .child(clickable_sidebar_item(
                "⌂",
                "首页",
                self.active_tab == AppTab::Home,
                cx.listener(Self::show_home),
            ))
            .child(clickable_sidebar_item(
                "▣",
                "动态",
                self.active_tab == AppTab::Dynamic,
                cx.listener(Self::show_dynamic),
            ))
            .child(clickable_sidebar_item(
                "♡",
                "收藏",
                self.active_tab == AppTab::Favorites,
                cx.listener(Self::show_favorites),
            ))
            .child(clickable_sidebar_item(
                "＋",
                "稍后",
                self.active_tab == AppTab::WatchLater,
                cx.listener(Self::show_watch_later),
            ))
            .child(clickable_sidebar_item(
                "◷",
                "历史",
                self.active_tab == AppTab::History,
                cx.listener(Self::show_history),
            ))
            .child(clickable_sidebar_item(
                "⌕",
                "搜索",
                self.active_tab == AppTab::Search,
                cx.listener(Self::show_search),
            ))
            .child(clickable_sidebar_item(
                "◉",
                if self.session.is_some() {
                    "账号"
                } else {
                    "登录"
                },
                self.active_tab == AppTab::Login,
                cx.listener(Self::show_login),
            ))
    }

    fn render_video_card(
        index: usize,
        video: Video,
        selected: bool,
        entity: Entity<BiliGuga>,
    ) -> impl IntoElement + use<> {
        let title = video.title.clone();
        let click_entity = entity.clone();
        let later_entity = entity.clone();
        let favorite_entity = entity.clone();
        div()
            .id(SharedString::from(format!("video-card-{index}")))
            .w_full()
            .h(px(76.))
            .flex()
            .flex_none()
            .gap_2()
            .cursor_pointer()
            .when(selected, |this| this.bg(rgb(0x454a56)))
            .when(!selected, |this| {
                this.hover(|style| style.bg(rgb(0x363c46)))
            })
            .child(thumbnail(&video, 126., 76.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .py_1()
                    .child(
                        div()
                            .text_sm()
                            .line_height(px(20.))
                            .text_color(rgb(0xdce0e5))
                            .text_ellipsis()
                            .child(title),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0xa9afbc))
                                    .text_ellipsis()
                                    .child(format!("{}  ·  {}", video.uploader, video.duration)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(rgb(0x878a98))
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("later-{index}")))
                                            .cursor_pointer()
                                            .hover(|style| style.text_color(rgb(0x74ade8)))
                                            .child("稍后")
                                            .on_click(move |event, window, cx| {
                                                later_entity.update(cx, |this, cx| {
                                                    this.add_video_to_watch_later(
                                                        index, event, window, cx,
                                                    );
                                                });
                                            }),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("favorite-{index}")))
                                            .cursor_pointer()
                                            .hover(|style| style.text_color(rgb(0x74ade8)))
                                            .child("收藏")
                                            .on_click(move |event, window, cx| {
                                                favorite_entity.update(cx, |this, cx| {
                                                    this.add_video_to_favorites(
                                                        index, event, window, cx,
                                                    );
                                                });
                                            }),
                                    ),
                            ),
                    ),
            )
            .on_click(move |event, window, cx| {
                click_entity.update(cx, |this, cx| {
                    this.select_video(index, event, window, cx);
                });
            })
    }

    fn render_login(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut panel = div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .text_center();
        if let Some(session) = &self.session {
            if let Some(image) = &self.account_avatar {
                panel = panel.child(img(image.clone()).w(px(72.)).h(px(72.)));
            } else {
                panel = panel.child(
                    div()
                        .w(px(72.))
                        .h(px(72.))
                        .bg(rgb(0x454a56))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_2xl()
                        .text_color(rgb(0xdce0e5))
                        .child(session.username.chars().next().unwrap_or('?').to_string()),
                );
            }
            panel = panel
                .child(
                    div()
                        .text_lg()
                        .text_color(rgb(0xdce0e5))
                        .child(session.username.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x878a98))
                        .child(format!("UID {} · 已登录", session.mid)),
                );
        } else {
            if let Some(image) = &self.login_image {
                panel = panel.child(img(image.clone()).w(px(240.)).h(px(240.)));
            } else {
                panel = panel.child(div().text_sm().text_color(rgb(0xa9afbc)).child(
                    if self.login_loading {
                        "正在准备登录…"
                    } else {
                        "点击刷新获取二维码"
                    },
                ));
            }
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(rgb(0xa9afbc))
                    .child(self.login_status.clone()),
            );
        }

        if self.session.is_some() {
            panel = panel.child(
                div()
                    .id("logout")
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(0xd07277))
                    .bg(rgb(0x363c46))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x454a56)))
                    .child("退出登录")
                    .on_click(cx.listener(Self::logout)),
            );
        } else if !self.login_loading {
            panel = panel.child(
                div()
                    .id("refresh-login")
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(0x74ade8))
                    .bg(rgb(0x363c46))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x454a56)))
                    .child("刷新二维码")
                    .on_click(cx.listener(Self::refresh_login)),
            );
        }

        div()
            .w(px(414.))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x2f343e))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(0xdce0e5))
                    .child(if self.session.is_some() {
                        "账号"
                    } else {
                        "登录"
                    }),
            )
            .child(panel)
    }

    fn render_feed(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        if self.active_tab == AppTab::Login {
            return self.render_login(cx);
        }
        let is_home = self.active_tab == AppTab::Home;
        let is_search = self.active_tab == AppTab::Search;
        let is_dynamic = self.active_tab == AppTab::Dynamic;
        let is_watch_later = self.active_tab == AppTab::WatchLater;
        let is_favorites = self.active_tab == AppTab::Favorites;
        let is_history = self.active_tab == AppTab::History;
        let is_author = self.active_tab == AppTab::Author;
        if is_home {
            self.maybe_load_home_page(cx);
        }
        let is_loading = if is_home {
            self.loading && self.videos.is_empty()
        } else if is_dynamic {
            self.dynamic_loading
        } else if is_watch_later {
            self.watch_later_loading
        } else if is_favorites {
            self.favorites_loading
        } else if is_history {
            self.history_loading
        } else if is_author {
            self.author_loading
        } else {
            self.loading
        };
        let search_input = self.search_input.clone();
        let entity = cx.entity();
        let mut feed = div()
            .id("feed-scroll")
            .w_full()
            .flex_1()
            .overflow_x_hidden();
        if is_loading {
            feed = feed.child(
                div()
                    .w_full()
                    .py_8()
                    .text_center()
                    .text_sm()
                    .text_color(rgb(0xa9afbc))
                    .child(if is_search {
                        "正在搜索 B 站视频…"
                    } else if is_dynamic {
                        "正在加载动态视频…"
                    } else if is_watch_later {
                        "正在加载稍后再看…"
                    } else if is_favorites {
                        "正在加载收藏夹…"
                    } else if is_history {
                        "正在加载观看历史…"
                    } else if is_author {
                        "正在加载作者视频…"
                    } else {
                        "正在加载 B 站推荐…"
                    }),
            );
        } else if self.current_videos().is_empty() {
            feed = feed.child(
                div()
                    .w_full()
                    .py_8()
                    .text_center()
                    .text_sm()
                    .text_color(rgb(0xd07277))
                    .child(if is_search {
                        "没有找到视频，请换个关键词试试"
                    } else if is_dynamic {
                        if self.session.is_some() {
                            "暂时没有关注中的视频动态"
                        } else {
                            "请先登录后查看动态"
                        }
                    } else if is_watch_later {
                        if self.session.is_some() {
                            "还没有稍后再看"
                        } else {
                            "请先登录后查看稍后再看"
                        }
                    } else if is_favorites {
                        if self.session.is_some() {
                            "还没有收藏视频"
                        } else {
                            "请先登录后查看收藏夹"
                        }
                    } else if is_history {
                        if self.session.is_some() {
                            "还没有观看历史"
                        } else {
                            "请先登录后查看观看历史"
                        }
                    } else if is_author {
                        "该作者暂时没有公开视频"
                    } else {
                        "没有加载到视频，请点击刷新重试"
                    }),
            );
        } else {
            let list = uniform_list(
                "video-list",
                self.current_videos().len(),
                cx.processor(move |this, range: Range<usize>, _window, _cx| {
                    range
                        .filter_map(|index| {
                            let video = this.current_videos().get(index)?.clone();
                            let selected = this
                                .selected_video_ref()
                                .map(|current| current.bvid == video.bvid)
                                .unwrap_or(index == this.selected);
                            Some(BiliGuga::render_video_card(
                                index,
                                video,
                                selected,
                                entity.clone(),
                            ))
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .with_horizontal_sizing_behavior(gpui::ListHorizontalSizingBehavior::FitList)
            .size_full();
            let list = if is_home {
                list.track_scroll(self.home_scroll_handle.clone())
                    .on_scroll_wheel(cx.listener(Self::note_home_scroll))
            } else {
                list
            };
            feed = feed.child(list);
        }
        let feed_title = if is_search {
            SharedString::from("搜索")
        } else if is_dynamic {
            SharedString::from("动态")
        } else if is_watch_later {
            SharedString::from("稍后再看")
        } else if is_favorites {
            SharedString::from("收藏夹")
        } else if is_history {
            SharedString::from("观看历史")
        } else if is_author {
            SharedString::from(self.author_name.clone())
        } else {
            SharedString::from("为你推荐")
        };
        let mut header = div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(0xdce0e5))
                    .child(feed_title),
            );
        if !is_search {
            header = header.child(
                div()
                    .id("refresh-feed")
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(0x74ade8))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x363c46)))
                    .child("刷新")
                    .on_click(cx.listener(move |app, event, window, cx| {
                        if is_author {
                            app.refresh_author_videos(event, window, cx);
                        } else if is_dynamic {
                            app.refresh_dynamic(event, window, cx);
                        } else if is_watch_later {
                            app.refresh_watch_later(event, window, cx);
                        } else if is_favorites {
                            app.refresh_favorites(event, window, cx);
                        } else if is_history {
                            app.refresh_history(event, window, cx);
                        } else {
                            app.refresh(event, window, cx);
                        }
                    })),
            );
            if is_author && self.author_has_more {
                header = header.child(
                    div()
                        .id("author-load-more")
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(rgb(0x74ade8))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(0x363c46)))
                        .child("加载更多")
                        .on_click(cx.listener(Self::load_more_author_videos)),
                );
            }
        }
        let search_bar = div()
            .flex()
            .items_center()
            .child(
                div()
                    .id("search-box")
                    .flex_1()
                    .h(px(30.))
                    .on_key_down(cx.listener(Self::search_key_down))
                    .child(search_input),
            )
            .child(
                div()
                    .id("search-button")
                    .h(px(30.))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(rgb(0x74ade8))
                    .bg(rgb(0x363c46))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x454a56)))
                    .child("搜索")
                    .on_click(cx.listener(Self::submit_search)),
            );
        let mut root = div()
            .w(px(414.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(0x2f343e))
            .child(header);
        if is_search {
            root = root.child(search_bar);
        }
        root.child(feed)
    }

    fn render_comments(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut section = div().w_full().px_5().pb_6().flex().flex_col().gap_2();
        let count = if self.comments_total > 0 {
            self.comments_total
        } else {
            self.comments.len() as i64
        };
        section = section.child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(0xdce0e5))
                        .child(format!("评论 {}", count)),
                )
                .child(div().text_xs().text_color(rgb(0x878a98)).child(
                    if self.comments_page > 0 {
                        "按热度"
                    } else {
                        "正在准备"
                    },
                )),
        );

        if self.comments.is_empty() && self.comments_loading {
            return section.child(
                div()
                    .py_5()
                    .text_sm()
                    .text_color(rgb(0x878a98))
                    .child("正在加载评论…"),
            );
        }
        if let Some(error) = &self.comments_error {
            return section.child(
                div()
                    .py_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(rgb(0xd07277))
                    .child(error.clone())
                    .child(
                        div()
                            .id("comments-retry")
                            .cursor_pointer()
                            .text_color(rgb(0x74ade8))
                            .child("重试")
                            .on_click(cx.listener(Self::retry_comments)),
                    ),
            );
        }
        if self.comments.is_empty() {
            return section.child(
                div()
                    .py_5()
                    .text_sm()
                    .text_color(rgb(0x878a98))
                    .child("还没有评论"),
            );
        }

        for comment in &self.comments {
            let mut item = div()
                .id(SharedString::from(format!("comment-{}", comment.rpid)))
                .w_full()
                .py_2()
                .flex()
                .flex_col()
                .gap_1();
            item = item
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .child(div().text_color(rgb(0x74ade8)).child(
                            if comment.username.is_empty() {
                                "用户".to_string()
                            } else {
                                comment.username.clone()
                            },
                        ))
                        .child(
                            div()
                                .text_color(rgb(0x878a98))
                                .child(format!("{}  ·  {}赞", comment.time, comment.like)),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .line_height(px(22.))
                        .text_color(rgb(0xdce0e5))
                        .child(comment.message.clone()),
                );
            section = section.child(item);
        }

        if self.comments_loading {
            section = section.child(
                div()
                    .py_3()
                    .text_center()
                    .text_xs()
                    .text_color(rgb(0x878a98))
                    .child("正在加载更多…"),
            );
        } else if self.comments_has_more {
            section = section.child(
                div()
                    .id("comments-load-more")
                    .w_full()
                    .py_2()
                    .text_center()
                    .text_xs()
                    .text_color(rgb(0x74ade8))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x363c46)))
                    .child("加载更多评论")
                    .on_click(cx.listener(Self::load_more_comments)),
            );
        }
        section
    }

    fn toggle_collection(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.collection_expanded = !self.collection_expanded;
        cx.notify();
    }

    fn render_collection(&self, cx: &mut Context<Self>) -> gpui::Div {
        let Some(collection) = &self.collection else {
            return div();
        };
        let current = self.selected_video_ref();
        let current_bvid = current.map(|video| video.bvid.as_str()).unwrap_or("");
        let current_cid = current.map(|video| video.cid).unwrap_or(0);
        let entity = cx.entity();
        let mut section = div()
            .w_full()
            .py_1()
            .bg(rgb(0x343a45))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id(SharedString::from(format!("collection-{}", collection.id)))
                    .w_full()
                    .h(px(28.))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x3d4350)))
                    .child(div().flex().items_center().gap_1().child(
                        div().text_xs().text_color(rgb(0x74ade8)).child(
                            if self.collection_expanded {
                                "▾"
                            } else {
                                "▸"
                            },
                        ),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(0xdce0e5))
                            .text_ellipsis()
                            .child(format!("合集 · {}", collection.title)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x878a98))
                            .child(format!("共 {} 集", collection.episodes.len())),
                    )
                    .on_click(cx.listener(Self::toggle_collection)),
            );
        if !self.collection_expanded {
            return section;
        }
        if collection.episodes.is_empty() {
            return section.child(
                div()
                    .px_4()
                    .text_xs()
                    .text_color(rgb(0x878a98))
                    .child("合集暂时没有可播放的视频"),
            );
        }
        for (index, episode) in collection.episodes.iter().enumerate() {
            let is_current = episode.bvid == current_bvid
                && (episode.cid <= 0 || current_cid <= 0 || episode.cid == current_cid);
            let click_entity = entity.clone();
            let mut row =
                div()
                    .id(SharedString::from(format!("collection-episode-{index}")))
                    .w_full()
                    .h(px(46.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .cursor_pointer()
                    .when(is_current, |this| this.bg(rgb(0x454a56)))
                    .when(!is_current, |this| {
                        this.hover(|style| style.bg(rgb(0x3d4350)))
                    })
                    .child(thumbnail(episode, 72., 42.))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .gap_0()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if is_current {
                                        rgb(0x74ade8)
                                    } else {
                                        rgb(0xdce0e5)
                                    })
                                    .text_ellipsis()
                                    .child(format!("{}. {}", index + 1, episode.title)),
                            )
                            .child(div().text_xs().text_color(rgb(0x878a98)).child(
                                if is_current {
                                    format!("{} · 正在播放", episode.duration)
                                } else {
                                    episode.duration.clone()
                                },
                            )),
                    )
                    .on_click(move |event, window, cx| {
                        click_entity.update(cx, |app, cx| {
                            app.select_collection_episode(index, event, window, cx);
                        });
                    });
            if is_current {
                row = row.text_color(rgb(0x74ade8));
            }
            section = section.child(row);
        }
        section
    }

    fn render_player(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let video = self.selected_video_ref().unwrap_or(&LOADING_VIDEO);
        let status = self.player.status();
        let playing = self.playback == PlaybackState::Playing;
        let paused = self.playback == PlaybackState::Paused;
        let buffering = self.playback == PlaybackState::Buffering;
        let frame = self.player.frame();
        let duration = if status.duration.is_finite() && status.duration > 0. {
            status.duration
        } else {
            0.
        };
        let position = if status.time_pos.is_finite() {
            status.time_pos.clamp(0., duration.max(0.))
        } else {
            0.
        };
        let progress = if duration > 0. {
            (position / duration).clamp(0., 1.) as f32
        } else {
            0.
        };
        let entity = cx.entity();
        let author_mid = video.uploader_mid;
        let author_name = video.uploader.clone();
        let author_entity = entity.clone();
        let mut stage = div()
            .w_full()
            .overflow_hidden()
            .bg(rgb(0x000000))
            .flex()
            .items_center()
            .justify_center()
            .relative()
            .on_mouse_move(cx.listener(Self::show_player_controls));
        if self.player_fullscreen {
            stage = stage.h_full();
        } else {
            // Keep the normal player at the video's native 16:9 ratio instead of
            // imposing a height that only happens to fit one window size.
            stage.style().aspect_ratio = Some(16. / 9.);
        }

        if let Some(frame) = frame {
            stage = stage.child(
                img(frame)
                    .w_full()
                    .h_full()
                    .object_fit(gpui::ObjectFit::Contain),
            );
        } else {
            stage = stage.child(
                div()
                    .text_sm()
                    .text_color(rgb(0x878a98))
                    .child(if buffering {
                        "正在缓冲视频…"
                    } else {
                        "点击播放开始观看"
                    }),
            );
        }

        if self.controls_visible {
            let seek_entity = entity.clone();
            let progress_bar = div()
                .relative()
                .flex_1()
                .h(px(16.))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(6.))
                        .h(px(4.))
                        .bg(rgb(0x464b57)),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(6.))
                        .h(px(4.))
                        .w(relative(progress))
                        .bg(rgb(0x74ade8)),
                )
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let seek_entity = seek_entity.clone();
                            window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                                if phase != DispatchPhase::Bubble
                                    || event.button != MouseButton::Left
                                    || !bounds.contains(&event.position)
                                    || bounds.size.width <= px(0.)
                                {
                                    return;
                                }
                                let percent = ((event.position.x - bounds.origin.x)
                                    / bounds.size.width)
                                    .clamp(0., 1.);
                                seek_entity.update(cx, |app, cx| {
                                    app.seek_percent(percent as f64, cx);
                                });
                            });
                        },
                    )
                    .size_full(),
                );
            let play_label = if buffering {
                "…"
            } else if playing {
                "Ⅱ"
            } else {
                "▶"
            };
            let play_handler = if playing || paused {
                Self::toggle_pause
            } else {
                Self::play_selected
            };
            let volume_entity = entity.clone();
            let volume_slider = div()
                .relative()
                .w(px(82.))
                .h(px(16.))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(6.))
                        .h(px(4.))
                        .bg(rgb(0x464b57)),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(6.))
                        .h(px(4.))
                        .w(relative((self.volume / 100.).clamp(0., 1.) as f32))
                        .bg(rgb(0x74ade8)),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(4.))
                        .left(relative((self.volume / 100.).clamp(0., 1.) as f32))
                        .w(px(7.))
                        .h(px(8.))
                        .bg(rgb(0xdce0e5)),
                )
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let entity = volume_entity.clone();
                            window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble
                                    && event.button == MouseButton::Left
                                    && bounds.contains(&event.position)
                                {
                                    entity.update(cx, |app, cx| {
                                        app.volume_dragging = true;
                                        app.set_volume_from_position(event.position, bounds, cx);
                                    });
                                }
                            });
                            let entity = volume_entity.clone();
                            window.on_mouse_event(move |event: &MouseMoveEvent, _, _, cx| {
                                entity.update(cx, |app, cx| {
                                    if app.volume_dragging
                                        && event.pressed_button == Some(MouseButton::Left)
                                    {
                                        app.set_volume_from_position(event.position, bounds, cx);
                                    }
                                });
                            });
                            let entity = volume_entity.clone();
                            window.on_mouse_event(move |event: &MouseUpEvent, _, _, cx| {
                                if event.button == MouseButton::Left {
                                    entity.update(cx, |app, _| {
                                        app.volume_dragging = false;
                                        app.persist_player_settings();
                                    });
                                }
                            });
                        },
                    )
                    .size_full(),
                );

            let mut speed_popup = div()
                .absolute()
                .bottom(px(34.))
                .right_0()
                .w(px(72.))
                .p_1()
                .bg(rgb(0x252a33))
                .flex()
                .flex_col();
            for speed in SPEED_OPTIONS {
                let selected_speed = (speed - self.speed).abs() < 0.01;
                speed_popup = speed_popup.child(
                    div()
                        .id(SharedString::from(format!("speed-option-{speed}")))
                        .w_full()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(if selected_speed {
                            rgb(0x74ade8)
                        } else {
                            rgb(0xdce0e5)
                        })
                        .when(!selected_speed, |this| {
                            this.hover(|style| style.bg(rgb(0x363c46)))
                        })
                        .cursor_pointer()
                        .child(format!("{speed:.2}x"))
                        .on_click(cx.listener(move |app, event, window, cx| {
                            app.select_speed(speed, event, window, cx);
                        })),
                );
            }

            let quality_text = self
                .quality_options
                .iter()
                .find(|option| option.qn == self.quality)
                .map(|option| option.label.clone())
                .unwrap_or_else(|| quality_label(self.quality));
            let mut quality_popup = div()
                .absolute()
                .bottom(px(34.))
                .right_0()
                .w(px(84.))
                .p_1()
                .bg(rgb(0x252a33))
                .flex()
                .flex_col();
            for option in self.quality_options.iter().cloned() {
                let quality = option.qn;
                let selected_quality = quality == self.quality;
                quality_popup = quality_popup.child(
                    div()
                        .id(SharedString::from(format!("quality-option-{quality}")))
                        .w_full()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(if selected_quality {
                            rgb(0x74ade8)
                        } else {
                            rgb(0xdce0e5)
                        })
                        .when(!selected_quality, |this| {
                            this.hover(|style| style.bg(rgb(0x363c46)))
                        })
                        .cursor_pointer()
                        .child(option.label)
                        .on_click(cx.listener(move |app, event, window, cx| {
                            app.select_quality(quality, event, window, cx);
                        })),
                );
            }

            let controls = div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .px_4()
                .pt_2()
                .pb_3()
                .bg(rgb(0x000000))
                .text_color(rgb(0xdce0e5))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .child(format_time(position))
                        .child(progress_bar)
                        .child(format_time(duration)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .id("player-play-overlay")
                                .w(px(30.))
                                .h(px(30.))
                                .bg(rgb(0x74ade8))
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(play_label)
                                .on_click(cx.listener(play_handler)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(0xa9afbc))
                                .child(if buffering {
                                    "缓冲中"
                                } else if playing {
                                    "播放中"
                                } else if paused {
                                    "已暂停"
                                } else {
                                    "待播放"
                                }),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .child("音量")
                                .child(volume_slider),
                        )
                        .child(
                            div()
                                .relative()
                                .child(
                                    div()
                                        .id("player-speed")
                                        .px_2()
                                        .py_1()
                                        .cursor_pointer()
                                        .text_xs()
                                        .hover(|style| style.bg(rgb(0x363c46)))
                                        .child(format!("倍速 {:.2}x", self.speed))
                                        .on_click(cx.listener(Self::toggle_speed_menu)),
                                )
                                .when(self.speed_menu_open, |this| this.child(speed_popup)),
                        )
                        .child(
                            div()
                                .relative()
                                .child(
                                    div()
                                        .id("player-quality")
                                        .px_2()
                                        .py_1()
                                        .cursor_pointer()
                                        .text_xs()
                                        .hover(|style| style.bg(rgb(0x363c46)))
                                        .child(quality_text.clone())
                                        .on_click(cx.listener(Self::toggle_quality_menu)),
                                )
                                .when(self.quality_menu_open, |this| this.child(quality_popup)),
                        )
                        .child(
                            div()
                                .id("player-window-fullscreen")
                                .w(px(24.))
                                .h(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|style| style.bg(rgb(0x363c46)))
                                .child(
                                    svg()
                                        .path(if self.player_fullscreen {
                                            WINDOW_FULLSCREEN_EXIT_ICON
                                        } else {
                                            WINDOW_FULLSCREEN_ICON
                                        })
                                        .size(px(16.))
                                        .text_color(rgb(0xdce0e5)),
                                )
                                .on_click(cx.listener(Self::toggle_player_fullscreen)),
                        )
                        .child(
                            div()
                                .id("player-screen-fullscreen")
                                .w(px(24.))
                                .h(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|style| style.bg(rgb(0x363c46)))
                                .child(
                                    svg()
                                        .path(if self.screen_fullscreen {
                                            SCREEN_FULLSCREEN_EXIT_ICON
                                        } else {
                                            SCREEN_FULLSCREEN_ICON
                                        })
                                        .size(px(16.))
                                        .text_color(rgb(0xdce0e5)),
                                )
                                .on_click(cx.listener(Self::toggle_screen_fullscreen)),
                        ),
                );
            stage = stage.child(controls);
        }

        div()
            .id("player-scroll")
            .flex_1()
            .when(self.player_fullscreen, |this| this.w_full())
            .h_full()
            .when(self.player_fullscreen, |this| this.overflow_hidden())
            .when(!self.player_fullscreen, |this| this.overflow_y_scroll())
            .overflow_x_hidden()
            .bg(rgb(0x3b414d))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .when(self.player_fullscreen, |this| this.h_full())
                    .child(stage)
                    .when(!self.player_fullscreen, |this| {
                        this.child(
                            div()
                                .w_full()
                                .px_5()
                                .py_4()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .w_full()
                                        .text_xl()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(rgb(0xdce0e5))
                                        .text_ellipsis()
                                        .child(video.title.clone()),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .text_sm()
                                        .text_color(rgb(0xa9afbc))
                                        .text_ellipsis()
                                        .child(
                                            div()
                                                .id("player-author")
                                                .cursor_pointer()
                                                .hover(|style| style.text_color(rgb(0x74ade8)))
                                                .child(author_name.clone())
                                                .on_click(move |_, window, cx| {
                                                    author_entity.update(cx, |app, cx| {
                                                        app.open_author_videos(
                                                            author_mid,
                                                            author_name.clone(),
                                                            window,
                                                            cx,
                                                        );
                                                    });
                                                }),
                                        )
                                        .child("  ·  ")
                                        .child(video.stats.clone())
                                        .child("  ·  ")
                                        .child(video.category.clone()),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .text_sm()
                                        .text_color(rgb(0xa9afbc))
                                        .child(
                                            div()
                                                .id("player-like")
                                                .cursor_pointer()
                                                .hover(|style| style.text_color(rgb(0x74ade8)))
                                                .child("♡ 点赞")
                                                .on_click(cx.listener(Self::like_current_video)),
                                        )
                                        .child(
                                            div()
                                                .id("player-coin")
                                                .cursor_pointer()
                                                .hover(|style| style.text_color(rgb(0x74ade8)))
                                                .child("◇ 投币")
                                                .on_click(cx.listener(Self::coin_current_video)),
                                        )
                                        .child(
                                            div()
                                                .id("player-favorite")
                                                .cursor_pointer()
                                                .hover(|style| style.text_color(rgb(0x74ade8)))
                                                .child("收藏")
                                                .on_click(
                                                    cx.listener(Self::save_current_to_favorites),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id("player-watch-later")
                                                .cursor_pointer()
                                                .hover(|style| style.text_color(rgb(0x74ade8)))
                                                .child("稍后")
                                                .on_click(
                                                    cx.listener(Self::save_current_to_watch_later),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .text_xs()
                                        .text_color(rgb(0x878a98))
                                        .text_ellipsis()
                                        .child(self.message.clone()),
                                ),
                        )
                        .child(self.render_collection(cx))
                        .child(self.render_comments(cx))
                    }),
            )
    }
}

impl Render for BiliGuga {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .size_full()
            .flex()
            .bg(rgb(0x3b414d))
            .track_focus(&self.root_focus)
            .capture_key_down(cx.listener(Self::handle_global_key));
        if self.player_fullscreen {
            root = root.child(self.render_player(cx));
        } else {
            root = root.child(
                div()
                    .size_full()
                    .flex()
                    .child(self.render_sidebar(cx))
                    .child(self.render_feed(cx))
                    .child(self.render_player(cx)),
            );
        }
        root
    }
}

fn clickable_sidebar_item<F>(
    icon: &'static str,
    label: &'static str,
    active: bool,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    sidebar_item(icon, label, active)
        .id(SharedString::from(format!("sidebar-{label}")))
        .on_click(on_click)
}

fn sidebar_item(icon: &'static str, label: &'static str, active: bool) -> gpui::Div {
    div()
        .w(px(64.))
        .h(px(52.))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .when(active, |this| {
            this.bg(rgb(0x454a56)).text_color(rgb(0xdce0e5))
        })
        .when(!active, |this| this.text_color(rgb(0xa9afbc)))
        .child(div().text_lg().child(icon))
        .child(div().text_xs().child(label))
}

fn thumbnail(video: &Video, width: f32, height: f32) -> impl IntoElement {
    let mut thumbnail = div()
        .w(px(width))
        .h(px(height))
        .flex_none()
        .bg(rgb(video.accent));
    if let Some(image) = &video.cover_image {
        thumbnail = thumbnail.child(
            img(image.clone())
                .w_full()
                .h_full()
                .object_fit(gpui::ObjectFit::Cover),
        );
    }
    thumbnail
}

pub(crate) fn launch() {
    Application::new()
        .with_assets(AppAssets)
        .run(|cx: &mut App| {
            bind_search_keys(cx);
            let bounds = Bounds::centered(None, size(px(1360.), px(820.)), cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        ..Default::default()
                    },
                    |_, cx| cx.new(BiliGuga::new),
                )
                .expect("failed to open biliguga window");
            let view = window.update(cx, |_, _, cx| cx.entity()).unwrap();
            window
                .update(cx, |app, window, _| window.focus(&app.root_focus))
                .unwrap();
            let window_handle = window
                .update(cx, |_, window, _| window.window_handle())
                .unwrap();
            let frame_view = view.clone();
            cx.spawn(async move |cx| {
                let mut last_memory_log = Instant::now();
                loop {
                    Timer::after(Duration::from_millis(50)).await;
                    if cx
                        .update_window(window_handle.clone(), |_, window, cx| {
                            let expired_frames = frame_view.update(cx, |app, cx| {
                                let was_active = app.playback != PlaybackState::Idle;
                                if !was_active {
                                    app.player.discard_pending_frames();
                                } else {
                                    app.player.poll_frame();
                                }
                                let status = app.player.status();
                                if app.playback != PlaybackState::Idle
                                    && status.time_pos.is_finite()
                                {
                                    if !app.cloud_resume_applied {
                                        let duration_ready = app.cloud_resume_progress.is_none()
                                            || (status.duration.is_finite()
                                                && status.duration > 0.);
                                        if duration_ready {
                                            if let Some(progress) = app
                                                .pending_seek
                                                .take()
                                                .or(app.cloud_resume_progress)
                                            {
                                                if progress > 3. && progress < status.duration - 3.
                                                {
                                                    app.player.seek_seconds(progress);
                                                }
                                            }
                                            app.cloud_resume_applied = true;
                                        }
                                    }
                                    app.playback = if status.paused {
                                        PlaybackState::Paused
                                    } else {
                                        PlaybackState::Playing
                                    };
                                    if status.volume.is_finite() {
                                        app.volume = status.volume.clamp(0., 100.);
                                    }
                                    if status.speed.is_finite() {
                                        app.speed = status.speed.max(0.1);
                                    }
                                    if app.playback == PlaybackState::Playing
                                        && app.history_report_at.elapsed()
                                            >= Duration::from_secs(15)
                                    {
                                        app.queue_history_report(cx, 0);
                                    }
                                }
                                let expired_frames = app.player.take_expired_frames();
                                if was_active || !expired_frames.is_empty() {
                                    cx.notify();
                                }
                                if last_memory_log.elapsed() >= Duration::from_secs(2) {
                                    app.debug_memory("tick");
                                    last_memory_log = Instant::now();
                                }
                                app.maybe_load_home_page(cx);
                                expired_frames
                            });
                            for frame in &expired_frames {
                                let _ = window.drop_image(frame.clone());
                            }
                            let pending_cover_drops = frame_view
                                .update(cx, |app, _| std::mem::take(&mut app.pending_cover_drops));
                            for image in pending_cover_drops {
                                let _ = window.drop_image(image);
                            }
                            frame_view.update(cx, |app, _| {
                                for frame in expired_frames {
                                    app.player.recycle_frame(frame);
                                }
                            });
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            let initial_cookie = view
                .read(cx)
                .session
                .as_ref()
                .map(|session| session.cookie.clone());
            cx.spawn(async move |cx| {
                let result = cx
                    .background_spawn(async move {
                        network::run(async move {
                            fetch_recommendations(1, initial_cookie.as_deref()).await
                        })
                        .await
                    })
                    .await;
                view.update(cx, |app, cx| {
                    if app.home_generation != 0 || app.active_tab != AppTab::Home {
                        return;
                    }
                    app.home_loading = false;
                    app.loading = false;
                    match result {
                        Ok(videos) => {
                            app.message = SharedString::from(format!(
                                "已加载 {} 个真实视频",
                                videos.videos.len()
                            ));
                            app.home_page = 1;
                            app.home_has_more = videos.has_more;
                            app.home_feed_mid = app.session.as_ref().map(|session| session.mid);
                            app.videos = videos.videos;
                            app.start_cover_loading(cx);
                        }
                        Err(error) => {
                            app.message = SharedString::from(format!("加载失败：{error}"));
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
            cx.activate(true);
        });
}
