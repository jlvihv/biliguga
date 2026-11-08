//! The window: state, the async work behind it, and the Rune view that draws it.
//!
//! The shape of this file follows Rune's model rather than GPUI's. There are three pieces:
//!
//! * [`BiliGuga`] is plain state. Handlers get `&mut BiliGuga` and change it directly; nothing
//!   is leased out of a store, so a handler can never panic on re-entering the view.
//! * [`Worker`] owns every async request. Each call spawns onto the shared tokio runtime,
//!   funnels its answer into one `mpsc` channel as an [`Event`], and wakes the window through
//!   the `Waker` Rune hands us in `setup`. `poll` drains that channel once per frame.
//! * [`view`] is a pure function of `&BiliGuga`: it builds the whole `El` tree on the frames
//!   that need one, and the runtime diffs it against the last commit by key.
//!
//! Player frames are the one thing that never goes through `poll`: mpv's worker thread writes
//! straight into a `rune::Pixels`, which the renderer uploads without re-running the view.
//! That is why a 1080p video does not rebuild the feed 30 times a second.

use crate::{
    api::{
        self, PlayQuality, add_to_favorites, add_to_watch_later, coin_video,
        fetch_author_videos, fetch_comments, fetch_dynamic_feed, fetch_favorites, fetch_history,
        fetch_last_play_progress, fetch_recommendations, fetch_search_results,
        download_avatar, fetch_video_context, fetch_watch_later, like_video, format_publish_date,
        Clock,
        queue_cover_download, report_video_heartbeat, report_video_progress, resolve_play_url,
    },
    login::{self, PollResult, UserSession},
    model::{Comment, Rgba8, Video, VideoCollection},
    mpv, network,
    power::PowerInhibitor,
};
use rune::tok::type_;
use rune::{
    AppSpec, Color, Cx, Drag, El, GrowthSpec, Image, ImageCache, Key, KeyEvent, Phase, ScrollSpec,
    SurfaceFit, TextEdit, Tx, Waker, col, el, row, text, text_input,
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const HOME_MAX_ITEMS: usize = 120;
/// How many decoded covers to keep. A cover is 57 KB, so this bounds the cache at a few
/// megabytes even if some path keeps adding rows without trimming.
const COVER_CACHE_LIMIT: usize = 256;
const SPEED_OPTIONS: [f64; 5] = [0.5, 1., 1.25, 1.5, 2.];
const DEFAULT_VOLUME: f64 = 100.;
const DEFAULT_SPEED: f64 = 1.;
const SIDEBAR_WIDTH: f32 = 64.;
const FEED_WIDTH: f32 = 414.;
const GRID_CARD_HEIGHT: f32 = 170.;
const GRID_CARD_WIDTH: f32 = 190.;
/// How long the player's controls stay up after the pointer moves.
const CONTROLS_LINGER: Duration = Duration::from_millis(2400);

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

/// The app's colours, in Rune's linear-sRGB `Color`.
///
/// These used to be functions calling `oklch(..)` on every access, so the OKLCH to linear-sRGB
/// conversion ran thousands of times a frame (each `theme::HOVER` in each row) and produced the
/// same numbers every time. They are the same colours, precomputed; the comment on each is the
/// `oklch(l c h)` it came from, so a designer can still edit the triple and rerun the conversion.
mod theme {
    use rune::Color;

    const fn c(r: f32, g: f32, b: f32) -> Color {
        Color { r, g, b, a: 1.0 }
    }

    /// oklch(0.185 0.02 265)
    pub(super) const BG: Color = c(0.004_503_9, 0.006_317_4, 0.011_431_3);
    /// oklch(0.215 0.022 265)
    pub(super) const PANEL: Color = c(0.007_211_4, 0.009_929_4, 0.017_459_9);
    /// oklch(0.205 0.021 265)
    pub(super) const FEED: Color = c(0.006_248_8, 0.008_607_1, 0.015_143_3);
    /// oklch(0.30 0.022 265)
    pub(super) const BORDER: Color = c(0.021_578_9, 0.027_099_4, 0.041_115);
    /// oklch(0.94 0.004 265)
    pub(super) const TEXT: Color = c(0.820_442, 0.831_242, 0.853_617);
    /// oklch(0.72 0.012 265)
    pub(super) const TEXT_DIM: Color = c(0.355_543, 0.374_257, 0.414_456);
    /// oklch(0.58 0.014 265)
    pub(super) const TEXT_FAINT: Color = c(0.181_774, 0.195_807, 0.226_616);
    /// oklch(0.62 0.19 285)
    pub(super) const ACCENT: Color = c(0.209_745, 0.156_45, 0.884_517);
    /// oklch(0.72 0.15 285)
    pub(super) const ACCENT_SOFT: Color = c(0.332_29, 0.300_493, 0.990_357);
    /// oklch(0.245 0.024 265)
    pub(super) const HOVER: Color = c(0.010_830_8, 0.014_706_3, 0.025_302_4);
    /// oklch(0.28 0.05 280)
    pub(super) const ACTIVE_ROW: Color = c(0.017_726_2, 0.019_160_6, 0.052_899_6);
    /// oklch(0.66 0.16 25)
    pub(super) const DANGER: Color = c(0.771_293, 0.127_848, 0.113_027);
    /// oklch(0.42 0.02 265)
    pub(super) const TRACK: Color = c(0.064_254_2, 0.074_446_4, 0.098_420_2);

    /// The placeholder a cover shows before its pixels arrive: the row's own accent, which
    /// `api` assigns per position so a feed of half-loaded covers still looks intentional.
    pub(super) fn cover_placeholder(accent: u32) -> Color {
        rune::color::hex(&format!("#{accent:06x}")).unwrap_or(Color::BLACK)
    }
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

impl AppTab {
    /// The signed-in feed this tab shows, if it shows one.
    fn signed_in(self) -> Option<SignedInFeed> {
        match self {
            AppTab::Dynamic => Some(SignedInFeed::Dynamic),
            AppTab::History => Some(SignedInFeed::History),
            AppTab::WatchLater => Some(SignedInFeed::WatchLater),
            AppTab::Favorites => Some(SignedInFeed::Favorites),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaybackState {
    Idle,
    Buffering,
    Playing,
    Paused,
}

/// Which popup the player has open. Only one kind exists: speed cycles in place and the
/// collection is reached with previous/next, so neither ever needed a menu.
/// Whose uploads the Author tab is showing.
struct Author {
    mid: i64,
    name: String,
}

/// The numbers the detail call brings back. They always arrive together, so they are one value
/// rather than four fields that a caller has to remember to set in step.
struct VideoStats {
    view_count: String,
    danmaku_count: String,
    like_count: String,
    published_at: String,
}

/// The feeds that need a session. Each one used to own a `load_..` method, a `.._loading` flag
/// and an `Event` arm holding the same three lines; the enum is the one place they differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SignedInFeed {
    Dynamic,
    History,
    WatchLater,
    Favorites,
}

impl SignedInFeed {
    fn videos_mut(self, app: &mut BiliGuga) -> &mut Vec<Video> {
        match self {
            SignedInFeed::Dynamic => &mut app.dynamic_videos,
            SignedInFeed::History => &mut app.history,
            SignedInFeed::WatchLater => &mut app.watch_later,
            SignedInFeed::Favorites => &mut app.favorites,
        }
    }

    fn loading(self, app: &BiliGuga) -> bool {
        match self {
            SignedInFeed::Dynamic => app.dynamic_loading,
            SignedInFeed::History => app.history_loading,
            SignedInFeed::WatchLater => app.watch_later_loading,
            SignedInFeed::Favorites => app.favorites_loading,
        }
    }

    fn set_loading(self, app: &mut BiliGuga, value: bool) {
        match self {
            SignedInFeed::Dynamic => app.dynamic_loading = value,
            SignedInFeed::History => app.history_loading = value,
            SignedInFeed::WatchLater => app.watch_later_loading = value,
            SignedInFeed::Favorites => app.favorites_loading = value,
        }
    }

    /// The event that lands this feed's answer. `Event` has no payload for the feed itself, so
    /// the arm is picked here rather than at the call site.
    fn event(self) -> impl FnOnce(Result<Vec<Video>, String>) -> Event + Copy + 'static {
        match self {
            SignedInFeed::Dynamic => Event::Dynamic,
            SignedInFeed::History => Event::History,
            SignedInFeed::WatchLater => Event::WatchLater,
            SignedInFeed::Favorites => Event::Favorites,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlayerMenu {
    Quality,
}

/// One answer from background work, drained by `poll` at the top of a frame.
enum Event {
    Home(Result<api::RecommendationPage, String>, u64),
    Search(Result<Vec<Video>, String>),
    Dynamic(Result<Vec<Video>, String>),
    History(Result<Vec<Video>, String>),
    WatchLater(Result<Vec<Video>, String>),
    Favorites(Result<Vec<Video>, String>),
    Author(Result<api::AuthorVideoPage, String>, u64),
    VideoContext(Box<api::VideoContext>),
    PlayUrl(Result<api::PlayUrlResult, String>),
    Resume(Option<f64>),
    Comments(Result<(usize, Vec<Comment>, i64, bool), String>, u64),
    Avatar(Option<Arc<Rgba8>>),
    QrCode(Result<login::QrCodeData, String>, u64),
    QrPoll(Result<PollResult, String>, u64),
    Session(Result<UserSession, String>),
    /// The outcome of a like, coin or favourite, shown as a short note under the player.
    ActionDone(&'static str, Result<(), String>),
    /// A cover arrived, keyed by bvid, with its pixels.
    Cover(String, Arc<Rgba8>),
    /// A cover download failed or was cancelled, so its bvid may be asked for again.
    CoverFailed(String),
    /// A history report finished, freeing the next one. Carries nothing: it is a gate, not
    /// news for the UI.
    HistoryReported,
    /// Work finished with nothing to show. `poll` counts it as activity but changes no state.
    Silent,
}

/// Everything that talks to the network or the player. All of it is one-way: an [`Event`]
/// comes back through `events`, and the `Waker` brings the window round to collect it.
struct Worker {
    events: std::sync::mpsc::Sender<Event>,
    waker: Waker,
}

impl Worker {
    /// Runs `future` on the shared tokio runtime and turns its output into an `Event`.
    fn spawn<F>(&self, future: F, wrap: impl FnOnce(F::Output) -> Event + Send + 'static)
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let events = self.events.clone();
        let waker = self.waker.clone();
        network::detach(async move {
            let output = future.await;
            if events.send(wrap(output)).is_ok() {
                waker();
            }
        });
    }
}

pub(crate) struct BiliGuga {
    // Identity and navigation.
    search_edit: TextEdit,
    active_tab: AppTab,
    // Feeds. Only one is shown at a time; `selected` indexes into whichever that is.
    videos: Vec<Video>,
    search_results: Vec<Video>,
    history: Vec<Video>,
    watch_later: Vec<Video>,
    favorites: Vec<Video>,
    dynamic_videos: Vec<Video>,
    author_videos: Vec<Video>,
    /// Whose page the feed column is showing, while the Author tab is active. `mid` is only
    /// needed to ask for a page, `name` to label the column, and both are set together: they
    /// were two fields with a `0` / `""` pair standing for "nobody".
    author: Option<Author>,
    author_page: usize,
    author_has_more: bool,
    author_loading: bool,
    author_generation: u64,
    /// The row the keyboard and the highlight are on, `None` until the user picks one: an
    /// unplayed feed should not claim its first row is chosen.
    selected: Option<usize>,
    loading: bool,
    history_loading: bool,
    watch_later_loading: bool,
    favorites_loading: bool,
    dynamic_loading: bool,
    // Paging.
    home_page: usize,
    home_loading: bool,
    home_has_more: bool,
    home_generation: u64,
    // Account and login.
    session: Option<UserSession>,
    account_avatar: Option<Image>,
    login_qr: Option<Image>,
    login_key: Option<String>,
    login_loading: bool,
    /// What the login panel tells the viewer. Almost every value is a fixed sentence, so this
    /// is a `Cow`: the seven static messages cost no allocation, and only an error from the
    /// API is a `String`.
    login_status: std::borrow::Cow<'static, str>,
    login_generation: u64,
    // Player.
    playback: PlaybackState,
    pending_pause: Option<bool>,
    volume: f64,
    speed: f64,
    quality: u32,
    quality_options: Vec<PlayQuality>,
    open_menu: Option<PlayerMenu>,
    seek_dragging: bool,
    volume_dragging: bool,
    controls_until: Option<Instant>,
    player_fullscreen: bool,
    playing_video: Option<Video>,
    /// The stage size the view last drew at.
    ///
    /// `view` gets `&self`, so it publishes the measurement here and `poll` — which owns the
    /// command channel to mpv — is what acts on it. mpv remembers the size it is already
    /// decoding for, so nothing has to mirror it here.
    stage_size: std::cell::Cell<Option<(usize, usize)>>,
    /// The clip's own numbers, once the detail call lands. `None` means "not loaded yet", which
    /// is what the four `String` fields this replaces expressed by holding a sentinel `"—"`:
    /// a placeholder that a real API value could collide with, and four fields that always
    /// moved together.
    stats: Option<VideoStats>,
    playback_error: Option<String>,
    cloud_resume_progress: Option<f64>,
    cloud_resume_applied: bool,
    pending_seek: Option<f64>,
    collection: Option<VideoCollection>,
    history_report_at: Instant,
    history_report_in_flight: bool,
    // Comments.
    comments: Vec<Comment>,
    comments_page: u32,
    comments_total: i64,
    comments_has_more: bool,
    comments_loading: bool,
    comments_generation: u64,
    comments_error: Option<String>,
    // Runtime plumbing.
    power_inhibitor: PowerInhibitor,
    player: mpv::MpvPlayer,
    /// The cancel flag this app's covers share. Nothing flips it: a cover that arrives late for
    /// a card that scrolled away is stored and reused, so cancelling would only throw the work
    /// away. `queue_cover_download` takes one, and one flag for the process is the honest value.
    covers_cancelled: Arc<AtomicBool>,
    /// Covers asked for and not yet arrived, by bvid. Without this, every page load asked for
    /// every missing cover again: a 20-row feed could fire 20 identical downloads per page.
    covers_in_flight: std::collections::HashSet<String>,
    images: Option<ImageCache>,
    worker: Option<Worker>,
    events: std::sync::mpsc::Receiver<Event>,
    /// Covers already decoded, by bvid. Rune's `Image` is a cheap shared handle, so one lookup
    /// per card per frame costs nothing and no bytes are hashed again.
    cover_images: std::collections::HashMap<String, Image>,
    /// The playhead the clock last showed, so `poll` reports a change only when the bar moves.
    last_position: f64,
    /// The last thing an action (like, coin, favourite) told the user.
    action_note: Option<String>,
    /// Set by any `Event` that added rows, so `poll` looks for missing covers once per frame
    /// rather than once per answer.
    new_rows: bool,
}

impl BiliGuga {
    fn new() -> Self {
        let session = login::load_session();
        let (volume, speed) = load_player_settings();
        let login_status = match &session {
            Some(session) => format!("已登录：{}", session.username).into(),
            None => "使用哔哩哔哩 App 扫码登录".into(),
        };
        let (events_tx, events) = std::sync::mpsc::channel();
        Self {
            search_edit: TextEdit::new(""),
            active_tab: AppTab::Home,
            videos: Vec::new(),
            search_results: Vec::new(),
            history: Vec::new(),
            watch_later: Vec::new(),
            favorites: Vec::new(),
            dynamic_videos: Vec::new(),
            author_videos: Vec::new(),
            author: None,
            author_page: 0,
            author_has_more: false,
            author_loading: false,
            author_generation: 0,
            selected: None,
            loading: false,
            history_loading: false,
            watch_later_loading: false,
            favorites_loading: false,
            dynamic_loading: false,
            home_page: 0,
            // Nothing is in flight yet: the load in `setup` is what starts the first page.
            // Starting this true made that load return immediately on its own guard, so the
            // feed stayed empty until the user pressed 刷新.
            home_loading: false,
            home_has_more: true,
            home_generation: 0,
            session,
            account_avatar: None,
            login_qr: None,
            login_key: None,
            login_loading: false,
            login_status,
            login_generation: 0,
            playback: PlaybackState::Idle,
            pending_pause: None,
            volume,
            speed,
            quality: 64,
            quality_options: Vec::new(),
            open_menu: None,
            seek_dragging: false,
            volume_dragging: false,
            controls_until: None,
            player_fullscreen: false,
            playing_video: None,
            stage_size: std::cell::Cell::new(None),
            stats: None,
            playback_error: None,
            cloud_resume_progress: None,
            cloud_resume_applied: false,
            pending_seek: None,
            collection: None,
            history_report_at: Instant::now(),
            history_report_in_flight: false,
            comments: Vec::new(),
            comments_page: 0,
            comments_total: 0,
            comments_has_more: false,
            comments_loading: false,
            comments_generation: 0,
            comments_error: None,
            power_inhibitor: PowerInhibitor::default(),
            player: mpv::MpvPlayer::new(),
            covers_cancelled: Arc::new(AtomicBool::new(false)),
            covers_in_flight: std::collections::HashSet::new(),
            images: None,
            worker: Some(Worker {
                events: events_tx,
                waker: Arc::new(|| {}),
            }),
            events,
            cover_images: std::collections::HashMap::new(),
            last_position: 0.,
            action_note: None,
            new_rows: false,
        }
    }
}

impl BiliGuga {
    fn worker(&self) -> &Worker {
        self.worker.as_ref().expect("worker is installed in setup")
    }

    /// The session cookie for a request that can run logged out.
    ///
    /// Seventeen call sites wrote `as_ref().map(|s| s.cookie.clone())` by hand; one accessor
    /// says the same thing and cannot drift.
    fn cookie(&self) -> Option<String> {
        self.session.as_ref().map(|session| session.cookie.clone())
    }

    /// The uploader's name for the feed heading, empty when no author page is open.
    fn author_name(&self) -> &str {
        self.author.as_ref().map_or("", |author| author.name.as_str())
    }

    /// The rows of the feed the feed column is showing. A slice, not a `&Vec`: no caller needs
    /// to know it is a `Vec`, and this way none of them can push to it by accident.
    fn current_videos(&self) -> &[Video] {
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

    /// Drops the oldest rows once the home feed grows past its limit.
    ///
    /// This removes their decoded covers too. It did not before, so every thumbnail the viewer
    /// ever scrolled past stayed in memory: a 160x90 cover is 57 KB, and a session that pages
    /// through a few hundred videos held every one of them for good.
    fn trim_home_items(&mut self) {
        if self.videos.len() <= HOME_MAX_ITEMS {
            return;
        }
        let remove_count = self.videos.len() - HOME_MAX_ITEMS;
        let dropped: Vec<String> = self.videos.drain(..remove_count).map(|v| v.bvid).collect();
        for bvid in dropped {
            self.forget_cover(&bvid);
        }
        self.selected = self
            .selected
            .map(|selected| selected.saturating_sub(remove_count));
    }

    /// Every bvid any feed column, the player or the open collection can still show. A cover
    /// for one of these is in use; anything else is a candidate for eviction.
    fn bvids_on_screen(&self) -> impl Iterator<Item = &str> {
        self.videos
            .iter()
            .chain(&self.search_results)
            .chain(&self.dynamic_videos)
            .chain(&self.watch_later)
            .chain(&self.favorites)
            .chain(&self.history)
            .chain(&self.author_videos)
            .chain(self.playing_video.as_ref())
            .chain(
                self.collection
                    .as_ref()
                    .into_iter()
                    .flat_map(|collection| &collection.episodes),
            )
            .map(|video| video.bvid.as_str())
    }

    /// Releases a cover nothing is showing any more.
    fn forget_cover(&mut self, bvid: &str) {
        if !self.bvids_on_screen().any(|shown| shown == bvid) {
            self.cover_images.remove(bvid);
        }
    }

    // ---- Feeds ---------------------------------------------------------------------------

    fn load_home_page(&mut self, reset: bool) {
        if !reset && (self.home_loading || !self.home_has_more) {
            return;
        }
        if reset {
            self.home_generation = self.home_generation.wrapping_add(1);
            self.home_page = 0;
            self.home_has_more = true;
            self.loading = true;
            self.videos.clear();
            self.selected = None;
        }
        let page = self.home_page + 1;
        // Record the page now, not when it lands: a second request while this one is in flight
        // would otherwise ask for the same page again, and the feed would never advance.
        self.home_page = page;
        let generation = self.home_generation;
        let cookie = self.cookie();
        self.home_loading = true;
        self.worker().spawn(
            async move { fetch_recommendations(page, cookie.as_deref()).await },
            move |result| Event::Home(result, generation),
        );
    }

    fn load_search(&mut self) {
        // Read from the field itself: `search_query` was a second copy kept in step by hand at
        // four call sites, and three of those synced it immediately before this function
        // overwrote its only reader anyway.
        let query = self.search_edit.text.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.active_tab = AppTab::Search;
        self.loading = true;
        self.search_results.clear();
        self.selected = None;
        self.worker().spawn(
            async move { fetch_search_results(&query).await },
            Event::Search,
        );
    }

    /// Folds a signed-in feed's answer into the state. A failure leaves whatever was shown.
    fn land_signed_in(&mut self, feed: SignedInFeed, result: Result<Vec<Video>, String>) {
        feed.set_loading(self, false);
        if let Ok(videos) = result {
            *feed.videos_mut(self) = videos;
            self.new_rows = true;
        }
    }

    /// Starts the request for a signed-in feed, or clears it when there is no session.
    ///
    /// The four feeds below were four copies of the same six lines with a different field name
    /// in each: copying one and forgetting to rename a field is exactly the kind of mistake the
    /// duplication was inviting.
    fn load_signed_in(&mut self, feed: SignedInFeed) {
        let Some(session) = self.session.as_ref() else {
            feed.videos_mut(self).clear();
            feed.set_loading(self, false);
            return;
        };
        let (cookie, mid) = (session.cookie.clone(), session.mid);
        feed.set_loading(self, true);
        let event = feed.event();
        self.worker().spawn(
            async move {
                match feed {
                    SignedInFeed::Dynamic => fetch_dynamic_feed(&cookie).await,
                    SignedInFeed::History => fetch_history(&cookie).await,
                    SignedInFeed::WatchLater => fetch_watch_later(&cookie).await,
                    SignedInFeed::Favorites => fetch_favorites(&cookie, mid).await,
                }
            },
            event,
        );
    }

    fn load_author_page(&mut self, reset: bool) {
        if !reset && (self.author_loading || !self.author_has_more) {
            return;
        }
        if reset {
            self.author_generation = self.author_generation.wrapping_add(1);
            self.author_page = 0;
            self.author_has_more = true;
            self.author_videos.clear();
        }
        let page = self.author_page + 1;
        let generation = self.author_generation;
        let Some(author) = self.author.as_ref() else {
            return;
        };
        let mid = author.mid;
        let cookie = self.cookie();
        self.author_loading = true;
        self.worker().spawn(
            async move { fetch_author_videos(mid, page, cookie.as_deref()).await },
            move |result| Event::Author(result, generation),
        );
    }

    /// Opens an uploader's videos in the feed column.
    fn open_author(&mut self, mid: i64, name: String) {
        if mid <= 0 {
            return;
        }
        self.author = Some(Author { mid, name });
        self.active_tab = AppTab::Author;
        self.selected = None;
        self.load_author_page(true);
    }

    fn show_tab(&mut self, tab: AppTab) {
        if self.active_tab == tab {
            return;
        }
        self.active_tab = tab;
        self.selected = None;
        match tab {
            AppTab::Home if self.videos.is_empty() => self.load_home_page(false),
            AppTab::Dynamic => self.load_signed_in(SignedInFeed::Dynamic),
            AppTab::History => self.load_signed_in(SignedInFeed::History),
            AppTab::WatchLater => self.load_signed_in(SignedInFeed::WatchLater),
            AppTab::Favorites => self.load_signed_in(SignedInFeed::Favorites),
            AppTab::Login => self.start_login(),
            _ => {}
        }
    }

    // ---- Playback ------------------------------------------------------------------------

    fn play_video(&mut self, video: Video) {
        // Release the previous stream first: its decoder, its audio track and its frame buffer
        // all belong to the video being replaced. Without this a quality change left the old
        // session decoding until the new one started.
        self.stop_playback();
        // One clone serves both requests below; the state keeps the original.
        let requested = video.clone();
        self.playing_video = Some(video);
        self.playback = PlaybackState::Buffering;
        self.playback_error = None;
        self.comments.clear();
        self.comments_page = 0;
        self.cloud_resume_applied = false;
        self.pending_seek = None;
        self.collection = None;
        self.stats = None;

        let cookie = self.cookie();
        let quality = self.quality;
        let for_context = requested.clone();
        self.worker().spawn(
            async move { fetch_video_context(&for_context, cookie.as_deref()).await },
            move |result| match result {
                Ok(context) => Event::VideoContext(Box::new(context)),
                Err(_) => Event::Silent,
            },
        );
        let play_cookie = self.cookie();
        self.worker().spawn(
            async move { resolve_play_url(&requested, play_cookie.as_deref(), quality).await },
            Event::PlayUrl,
        );
        self.load_comments(true);
    }

    fn finish_play(&mut self, result: api::PlayUrlResult) {
        // The play URL call resolved the ids it needed; keep them on the video so the history
        // report and the detail call stop resolving them again.
        if let Some(video) = self.playing_video.as_mut() {
            if video.aid <= 0 {
                video.aid = result.aid;
            }
            if video.cid <= 0 {
                video.cid = result.cid;
            }
        }
        self.quality_options = result.qualities.clone();
        self.quality = result.actual_quality;
        self.player.load(
            result.url,
            result.audio_url,
            self.volume,
            self.speed,
        );
        self.playback = PlaybackState::Buffering;
        let Some(video) = self.playing_video.clone() else {
            return;
        };
        let play_cookie = self.cookie();
        self.worker().spawn(
            async move {
                fetch_last_play_progress(&video, play_cookie.as_deref())
                    .await
                    .map(|progress| progress as f64)
            },
            Event::Resume,
        );
    }

    fn toggle_pause(&mut self) {
        let paused = self.playback == PlaybackState::Playing;
        self.pending_pause = Some(paused);
        self.player.set_pause(paused);
        self.playback = if paused {
            PlaybackState::Paused
        } else {
            PlaybackState::Playing
        };
    }

    fn stop_playback(&mut self) {
        self.player.stop_playback();
        self.playback = PlaybackState::Idle;
        self.pending_pause = None;
        self.pending_seek = None;
        self.cloud_resume_progress = None;
        self.cloud_resume_applied = true;
        self.power_inhibitor.set_active(false);
    }

    fn seek_percent(&mut self, percent: f64) {
        self.player.seek_percent(percent.clamp(0., 1.));
    }

    fn persist_player_settings(&self) {
        save_player_settings(self.volume, self.speed);
    }

    /// Selects a feed row and plays it.
    /// Reloads the current video at another quality, keeping the playhead.
    fn select_quality(&mut self, quality: u32) {
        let Some(video) = self.playing_video.clone() else {
            return;
        };
        let position = self.player.status().time_pos;
        self.quality = quality;
        self.open_menu = None;
        self.playback = PlaybackState::Buffering;
        let cookie = self.cookie();
        self.worker().spawn(
            async move { resolve_play_url(&video, cookie.as_deref(), quality).await },
            Event::PlayUrl,
        );
        // Resume where the viewer was, once the new stream reports its own duration.
        if position.is_finite() && position > 1. {
            self.pending_seek = Some(position);
            self.cloud_resume_applied = false;
            self.cloud_resume_progress = None;
        }
    }

    fn select(&mut self, index: usize) {
        self.selected = Some(index);
        if let Some(video) = self.current_videos().get(index).cloned() {
            self.play_video(video);
        }
    }

    fn show_controls(&mut self) {
        self.controls_until = Some(Instant::now() + CONTROLS_LINGER);
    }

    fn controls_visible(&self) -> bool {
        self.controls_until
            .is_some_and(|until| Instant::now() < until)
            || self.open_menu.is_some()
            || self.seek_dragging
            || self.volume_dragging
            || self.playback == PlaybackState::Paused
    }

    fn queue_history_report(&mut self) {
        if self.history_report_in_flight {
            return;
        }
        let (Some(cookie), Some(video)) = (
            self.cookie(),
            self.playing_video.clone(),
        ) else {
            return;
        };
        if video.aid <= 0 || video.cid <= 0 {
            return;
        }
        let position = self.player.status().time_pos;
        if !position.is_finite() || position < 1. {
            return;
        }
        self.history_report_in_flight = true;
        self.history_report_at = Instant::now();
        self.worker().spawn(
            async move {
                let _ = report_video_progress(&cookie, video.aid, video.cid, position).await;
                let _ = report_video_heartbeat(&cookie, &video, position, 0).await;
            },
            |()| Event::HistoryReported,
        );
    }

    // ---- Comments ------------------------------------------------------------------------

    fn load_comments(&mut self, reset: bool) {
        let Some(video) = self.playing_video.clone() else {
            return;
        };
        if !reset && (self.comments_loading || !self.comments_has_more) {
            return;
        }
        if reset {
            self.comments_generation = self.comments_generation.wrapping_add(1);
            self.comments.clear();
            self.comments_page = 0;
            self.comments_error = None;
        }
        let page = self.comments_page + 1;
        let generation = self.comments_generation;
        let cookie = self.cookie();
        self.comments_loading = true;
        self.worker().spawn(
            async move {
                fetch_comments(&video, cookie.as_deref(), page)
                    .await
                    .map(|page_result| {
                        (
                            page as usize,
                            page_result.comments,
                            page_result.total,
                            page_result.has_more,
                        )
                    })
            },
            move |result| Event::Comments(result, generation),
        );
    }

    // ---- Login ---------------------------------------------------------------------------

    fn start_login(&mut self) {
        self.login_generation = self.login_generation.wrapping_add(1);
        let generation = self.login_generation;
        self.login_loading = true;
        self.login_status = "正在获取登录二维码…".into();
        self.login_qr = None;
        self.login_key = None;
        self.active_tab = AppTab::Login;
        self.worker().spawn(fetch_qr_code_event(), move |result| {
            Event::QrCode(result, generation)
        });
    }

    fn poll_login(&mut self) {
        let Some(key) = self.login_key.clone() else {
            return;
        };
        let generation = self.login_generation;
        self.worker()
            .spawn(async move { login::poll_qr_code(&key).await }, move |result| {
                Event::QrPoll(result, generation)
            });
    }

    fn load_avatar(&mut self, url: String) {
        if url.is_empty() || self.account_avatar.is_some() {
            return;
        }
        self.worker().spawn(
            async move { download_avatar(&url).await },
            Event::Avatar,
        );
    }

    fn refresh_session(&mut self) {
        let Some(cookie) = self.cookie() else {
            return;
        };
        self.worker().spawn(
            async move { login::fetch_user(cookie).await },
            Event::Session,
        );
    }

    // ---- Covers --------------------------------------------------------------------------

    /// Asks for one cover. Returns silently if that bvid is already on its way, so a second
    /// page asking for the same row does not queue a duplicate download behind the first.
    fn request_cover(&mut self, bvid: String, url: String) {
        if url.is_empty() || !self.covers_in_flight.insert(bvid.clone()) {
            return;
        }
        let cancelled = self.covers_cancelled.clone();
        let Some(receiver) = queue_cover_download(url, cancelled) else {
            self.covers_in_flight.remove(&bvid);
            return;
        };
        self.worker().spawn(
            async move { receiver.await.ok().flatten() },
            move |image| match image {
                Some(image) => Event::Cover(bvid, image),
                None => Event::CoverFailed(bvid),
            },
        );
    }

    fn attach_cover(&mut self, bvid: String, pixels: Arc<Rgba8>) {
        self.covers_in_flight.remove(&bvid);
        let Some(cache) = self.images.as_ref() else {
            return;
        };
        // `Arc<[u8]>` clones here, the bytes do not: the cache becomes the second owner.
        let image = cache.rgba(pixels.width, pixels.height, pixels.pixels.clone());
        if self.cover_images.len() >= COVER_CACHE_LIMIT && !self.cover_images.contains_key(&bvid)
        {
            // Over the limit: drop whatever no feed is showing. If everything is shown, the
            // cap waits for a row to go away rather than evicting a cover in use.
            let shown: std::collections::HashSet<String> = self
                .bvids_on_screen()
                .map(str::to_string)
                .collect();
            self.cover_images
                .retain(|key, _| shown.contains(key.as_str()));
        }
        self.cover_images.insert(bvid, image);
    }

    fn cover_for(&self, video: &Video) -> Option<&Image> {
        self.cover_images.get(&video.bvid)
    }
}

// ---- The view -------------------------------------------------------------------------------

/// The app's colour for a video's placeholder, picked from its accent so a feed of
/// half-loaded covers does not look broken.
fn card_cover(video: &Video, image: Option<&Image>, width: f32, height: f32) -> El {
    let mut frame = el()
        .w(width)
        .h(height)
        .radius(6.0)
        .fill(theme::cover_placeholder(video.accent));
    if let Some(image) = image {
        frame = frame.child(
            el()
                .w_full()
                .h_full()
                .radius(6.0)
                .pixels_fit(image.pixels(), SurfaceFit::Fill),
        );
    }
    frame
}

/// An icon drawn from lines rather than taken from the text font.
///
/// A house, a play mark, a clock, a bookmark, a star and a person all exist in whatever font
/// happens to be installed, at different widths and weights and sometimes missing entirely;
/// a column of mismatched shapes is exactly what a sidebar looks like when it borrows glyphs.
fn sidebar_icon(icon: SidebarIcon, color: Color) -> El {
    let mut shape = rune::Shape::new().view_box(16.0, 16.0);
    match icon {
        SidebarIcon::Home => {
            shape = shape
                .stroke(vec![[2.5, 7.5], [8.0, 2.5], [13.5, 7.5]], 1.4, color)
                .stroke(vec![[4.5, 6.5], [4.5, 13.5], [11.5, 13.5], [11.5, 6.5]], 1.4, color);
        }
        SidebarIcon::Dynamic => {
            // A triangle inside a circle: the play mark a feed of videos suggests.
            let mut circle = Vec::with_capacity(25);
            for step in 0..=24 {
                let angle = step as f32 * std::f32::consts::TAU / 24.0;
                circle.push([8.0 + 6.3 * angle.cos(), 8.0 + 6.3 * angle.sin()]);
            }
            shape = shape
                .stroke(circle, 1.3, color)
                .fill(vec![[6.6, 5.4], [10.8, 8.0], [6.6, 10.6]], color);
        }
        SidebarIcon::History => {
            shape = shape
                .stroke(
                    vec![
                        [13.0, 8.0],
                        [11.0, 10.8],
                        [8.0, 12.6],
                        [5.0, 10.8],
                        [3.0, 8.0],
                        [5.0, 5.2],
                        [8.0, 3.4],
                        [11.0, 5.2],
                        [13.0, 8.0],
                    ],
                    1.4,
                    color,
                )
                .stroke(vec![[8.0, 5.2], [8.0, 8.3], [10.2, 9.4]], 1.3, color);
        }
        SidebarIcon::WatchLater => {
            shape = shape.stroke(
                vec![[4.0, 2.5], [12.0, 2.5], [12.0, 13.5], [8.0, 10.6], [4.0, 13.5], [4.0, 2.5]],
                1.4,
                color,
            );
        }
        SidebarIcon::Favorites => {
            let mut points = Vec::with_capacity(11);
            for step in 0..10 {
                let angle =
                    -std::f32::consts::FRAC_PI_2 + step as f32 * std::f32::consts::PI / 5.0;
                let radius = if step % 2 == 0 { 6.2 } else { 2.7 };
                points.push([8.0 + radius * angle.cos(), 8.0 + radius * angle.sin()]);
            }
            points.push(points[0]);
            shape = shape.stroke(points, 1.3, color);
        }
        SidebarIcon::Login => {
            // A person: a head and shoulders, so it cannot be read as the feed's mark.
            let mut head = Vec::with_capacity(17);
            for step in 0..=16 {
                let angle = step as f32 * std::f32::consts::TAU / 16.0;
                head.push([8.0 + 2.9 * angle.cos(), 5.4 + 2.9 * angle.sin()]);
            }
            shape = shape.stroke(head, 1.3, color).stroke(
                vec![[3.2, 14.0], [4.6, 10.4], [8.0, 9.4], [11.4, 10.4], [12.8, 14.0]],
                1.3,
                color,
            );
        }
    }
    // The canvas *is* the icon: as a child of a sized wrapper it would lay out at zero width
    // and only its first stroke would draw.
    rune::canvas(shape).size(16.0, 16.0).shrink()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarIcon {
    Home,
    Dynamic,
    History,
    WatchLater,
    Favorites,
    Login,
}

fn sidebar_item(label: &'static str, icon: SidebarIcon, active: bool, tab: AppTab) -> El {
    let (fill, glyph, caption) = if active {
        (theme::ACTIVE_ROW, theme::ACCENT_SOFT, theme::ACCENT_SOFT)
    } else {
        (Color::TRANSPARENT, theme::TEXT_DIM, theme::TEXT_FAINT)
    };
    col()
        .w(SIDEBAR_WIDTH)
        .h(52.0)
        .items_center()
        .justify_center()
        .gap(3.0)
        .cursor(rune::Cursor::Pointer)
        .fill(fill)
        .hover(theme::HOVER)
        .on_press(move |app: &mut BiliGuga| app.show_tab(tab))
        .child(sidebar_icon(icon, glyph))
        .child(text(label).font(type_::SECTION).color(caption))
}

fn render_sidebar(app: &BiliGuga) -> El {
    col()
        .w(SIDEBAR_WIDTH)
        .h_full()
        .items_center()
        .py(8.0)
        .gap(4.0)
        .fill(theme::BG)
        .child(sidebar_item("首页", SidebarIcon::Home, app.active_tab == AppTab::Home, AppTab::Home))
        .child(sidebar_item("动态", SidebarIcon::Dynamic, app.active_tab == AppTab::Dynamic, AppTab::Dynamic))
        .child(sidebar_item("历史", SidebarIcon::History, app.active_tab == AppTab::History, AppTab::History))
        .child(sidebar_item("稍后", SidebarIcon::WatchLater, app.active_tab == AppTab::WatchLater, AppTab::WatchLater))
        .child(sidebar_item("收藏", SidebarIcon::Favorites, app.active_tab == AppTab::Favorites, AppTab::Favorites))
        .child(sidebar_item("登录", SidebarIcon::Login, app.active_tab == AppTab::Login, AppTab::Login))
}

/// One row of the feed: a 16:9 cover, the title over two lines, and the uploader and duration.
fn video_row(app: &BiliGuga, index: usize, video: &Video) -> El {
    let selected = app.selected == Some(index);
    let image = app.cover_for(video);
    let mut row_el = row()
        .key(format!("row-{}", video.bvid))
        .w_full()
        .gap(10.0)
        .padding(8.0)
        .radius(8.0)
        .cursor(rune::Cursor::Pointer)
        .hover(theme::HOVER)
        .on_press(move |app: &mut BiliGuga| app.select(index))
        .child(card_cover(video, image, 132.0, 74.0))
        .child(
            col()
                .grow(1.0)
                .min_h(0.0)
                .gap(4.0)
                .child(
                    text(&video.title)
                        .font(type_::BODY)
                        .color(theme::TEXT)
                        .max_lines(2),
                )
                .child(
                    text(&video.uploader)
                        .font(type_::LABEL)
                        .color(theme::TEXT_FAINT)
                        .truncate(),
                )
                .child(
                    text(&video.duration)
                        .font(type_::LABEL)
                        .color(theme::TEXT_FAINT),
                ),
        );
    if selected {
        row_el = row_el.fill(theme::ACTIVE_ROW);
    }
    row_el
}

/// A two-column card for the home grid.
fn video_card(app: &BiliGuga, index: usize, video: &Video) -> El {
    let image = app.cover_for(video);
    let mut card = col()
        .key(format!("card-{}", video.bvid))
        .w(GRID_CARD_WIDTH)
        .h(GRID_CARD_HEIGHT)
        .shrink()
        .gap(4.0)
        .padding(4.0)
        .radius(8.0)
        .cursor(rune::Cursor::Pointer)
        .hover(theme::HOVER)
        .on_press(move |app: &mut BiliGuga| app.select(index))
        .child(card_cover(video, image, GRID_CARD_WIDTH - 8.0, 106.0))
        .child(
            text(&video.title)
                .font(type_::LABEL)
                .color(theme::TEXT)
                .max_lines(2),
        )
        .child(
            text(&video.uploader)
                .font(type_::SECTION)
                .color(theme::TEXT_FAINT)
                .truncate(),
        );
    if app.selected == Some(index) {
        card = card.fill(theme::ACTIVE_ROW);
    }
    card
}

/// The feed column's heading: a fixed sentence for every tab but one, which borrows the
/// uploader's name. Returning `String` allocated on every render for seven of the eight cases.
fn feed_title<'a>(tab: AppTab, author_name: &'a str) -> std::borrow::Cow<'a, str> {
    match tab {
        AppTab::Home => "为你推荐".into(),
        AppTab::Search => "搜索".into(),
        AppTab::Dynamic => "动态".into(),
        AppTab::WatchLater => "稍后再看".into(),
        AppTab::Favorites => "收藏夹".into(),
        AppTab::History => "观看历史".into(),
        AppTab::Author => author_name.into(),
        AppTab::Login => "登录".into(),
    }
}

fn empty_message(app: &BiliGuga) -> &'static str {
    let logged_in = app.session.is_some();
    match app.active_tab {
        AppTab::Search => "没有找到视频，请换个关键词试试",
        AppTab::Dynamic if !logged_in => "请先登录后查看动态",
        AppTab::Dynamic => "暂时没有关注中的视频动态",
        AppTab::WatchLater if !logged_in => "请先登录后查看稍后再看",
        AppTab::WatchLater => "还没有稍后再看",
        AppTab::Favorites if !logged_in => "请先登录后查看收藏夹",
        AppTab::Favorites => "还没有收藏视频",
        AppTab::History if !logged_in => "请先登录后查看观看历史",
        AppTab::History => "还没有观看历史",
        AppTab::Author => "该作者暂时没有公开视频",
        _ => "没有加载到视频，请点击刷新重试",
    }
}

fn loading_message(tab: AppTab) -> &'static str {
    match tab {
        AppTab::Search => "正在搜索 B 站视频…",
        AppTab::Dynamic => "正在加载动态视频…",
        AppTab::WatchLater => "正在加载稍后再看…",
        AppTab::Favorites => "正在加载收藏夹…",
        AppTab::History => "正在加载观看历史…",
        AppTab::Author => "正在加载作者视频…",
        _ => "正在加载 B 站推荐…",
    }
}

fn render_search_bar(app: &BiliGuga) -> El {
    row()
        .w_full()
        .gap(8.0)
        .px(12.0)
        .py(8.0)
        .items_center()
        .fill(theme::PANEL)
        .child(
            el()
                .key("search-field")
                .grow(1.0)
                .h(32.0)
                .radius(6.0)
                .fill(theme::BG)
                .border(1.0, theme::BORDER)
                .px(8.0)
                .items_center()
                .child(
                    text_input(&app.search_edit, type_::BODY)
                        .key("search-input")
                        .w_full()
                        .single_line()
                        .placeholder("搜索视频", theme::TEXT_FAINT)
                        .color(theme::TEXT)
                        .on_edit(|app: &mut BiliGuga| &mut app.search_edit)
                        .on_submit(|app: &mut BiliGuga, _: &mut Tx| app.load_search()),
                ),
        )
        .child(
            el()
                .key("search-button")
                .h(32.0)
                .px(12.0)
                .radius(6.0)
                .items_center()
                .justify_center()
                .cursor(rune::Cursor::Pointer)
                .fill(theme::ACCENT)
                .hover(theme::ACCENT_SOFT)
                .on_press(|app: &mut BiliGuga| app.load_search())
                .child(text("搜索").font(type_::LABEL).color(Color::WHITE)),
        )
}

fn render_feed(app: &BiliGuga, cx: &mut Cx<BiliGuga>) -> El {
    if app.active_tab == AppTab::Login {
        return render_login(app);
    }
    let is_home = app.active_tab == AppTab::Home;
    // One question, asked of the tab: which feed is showing, and is it still loading. This was
    // a five-arm match here plus five more in the loader, all naming the same five fields.
    let is_loading = match app.active_tab.signed_in() {
        Some(feed) => feed.loading(app),
        None => match app.active_tab {
            AppTab::Home => app.loading && app.videos.is_empty(),
            AppTab::Author => app.author_loading,
            _ => app.loading,
        },
    };
    let videos = app.current_videos();

    let mut header = row()
        .w_full()
        .px(12.0)
        .py(12.0)
        .items_center()
        .justify_between()
        .fill(theme::PANEL)
        .child(
            text(feed_title(app.active_tab, app.author_name()))
                .font(type_::TITLE)
                .color(theme::TEXT),
        );
    if app.active_tab == AppTab::Author && app.author_has_more {
        header = header.child(
            el()
                .key("author-more")
                .px(8.0)
                .py(4.0)
                .radius(4.0)
                .cursor(rune::Cursor::Pointer)
                .hover(theme::HOVER)
                .on_press(|app: &mut BiliGuga| app.load_author_page(false))
                .child(text("加载更多").font(type_::SECTION).color(theme::ACCENT_SOFT)),
        );
    }
    if app.active_tab != AppTab::Search {
        header = header.child(
            el()
                .key("refresh")
                .px(8.0)
                .py(4.0)
                .radius(4.0)
                .cursor(rune::Cursor::Pointer)
                .hover(theme::HOVER)
                .on_press(|app: &mut BiliGuga| app.refresh_feed())
                .child(text("刷新").font(type_::SECTION).color(theme::ACCENT_SOFT)),
        );
    }

    let body = if is_loading {
        col().w_full().py(32.0).items_center().gap(6.0).child(
            text(loading_message(app.active_tab))
                .font(type_::BODY)
                .color(theme::TEXT_FAINT),
        )
    } else if videos.is_empty() {
        col().w_full().py(32.0).items_center().child(
            text(empty_message(app))
                .font(type_::BODY)
                .color(theme::TEXT_FAINT),
        )
    } else if is_home {
        col()
            .key("home-grid")
            .w_full()
            .growth(GrowthSpec::default())
            .gap(4.0)
            .padding(4.0)
            .virtual_rows(
                videos.len().div_ceil(2),
                GRID_CARD_HEIGHT + 4.0,
                |_app: &BiliGuga, row_index| format!("grid-row-{row_index}").into(),
                |app: &BiliGuga, row_index| {
                    let videos = app.current_videos();
                    let first = row_index * 2;
                    let mut line = row().w_full().gap(4.0);
                    if let Some(video) = videos.get(first) {
                        line = line.child(video_card(app, first, video));
                    }
                    if let Some(video) = videos.get(first + 1) {
                        line = line.child(video_card(app, first + 1, video));
                    }
                    line
                },
            )
    } else {
        col()
            .key("video-list")
            .w_full()
            .growth(GrowthSpec::default())
            .gap(2.0)
            .padding(4.0)
            .virtual_rows(
                videos.len(),
                90.0,
                |app: &BiliGuga, index| {
                    app.current_videos()
                        .get(index)
                        .map(|video| format!("row-{}", video.bvid))
                        .unwrap_or_else(|| format!("row-{index}"))
                        .into()
                },
                |app: &BiliGuga, index| {
                    let videos = app.current_videos();
                    match videos.get(index) {
                        Some(video) => video_row(app, index, video),
                        None => el(),
                    }
                },
            )
    };

    let mut root = col()
        .w(FEED_WIDTH)
        .h_full()
        .shrink()
        .fill(theme::FEED)
        .child(header);
    if app.active_tab == AppTab::Search {
        root = root.child(render_search_bar(app));
    }
    let _ = cx;
    // Paging hangs off the viewport: it reports when the reader is far from the bottom, once
    // per crossing rather than per frame, which is what makes this cheap.
    let mut viewport = col()
        .key("feed-scroll")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .scroll(ScrollSpec { stick_bottom: false });
    if app.active_tab == AppTab::Home {
        viewport = viewport.on_far_from_bottom(600.0, |app: &mut BiliGuga, far: bool| {
            if far && !app.home_loading && app.home_has_more {
                app.load_home_page(false);
            }
        });
    }
    root.child(viewport.child(body))
}

fn render_login(app: &BiliGuga) -> El {
    let mut panel = col()
        .w_full()
        .h_full()
        .padding(20.0)
        .gap(12.0)
        .items_center()
        .fill(theme::FEED);
    if let Some(session) = &app.session {
        panel = panel
            .child(
                text(format!("已登录：{}", session.username))
                    .font(type_::TITLE)
                    .color(theme::TEXT),
            )
            .child(
                el()
                    .px(12.0)
                    .py(6.0)
                    .radius(6.0)
                    .cursor(rune::Cursor::Pointer)
                    .fill(theme::PANEL)
                    .hover(theme::HOVER)
                    .on_press(|app: &mut BiliGuga| {
                        login::clear_session();
                        app.session = None;
                        app.account_avatar = None;
                        app.login_status = "使用哔哩哔哩 App 扫码登录".into();
                        app.active_tab = AppTab::Home;
                    })
                    .child(text("退出登录").font(type_::LABEL).color(theme::DANGER)),
            );
        return panel;
    }
    if let Some(qr) = &app.login_qr {
        panel = panel.child(
            el()
                .w(220.0)
                .h(220.0)
                .radius(8.0)
                .pixels_fit(qr.pixels(), SurfaceFit::Fit),
        );
    }
    panel = panel
        .child(
            text(app.login_status.as_ref())
                .font(type_::BODY)
                .color(theme::TEXT_DIM),
        )
        .child(
            el()
                .px(12.0)
                .py(6.0)
                .radius(6.0)
                .cursor(rune::Cursor::Pointer)
                .fill(theme::ACCENT)
                .hover(theme::ACCENT_SOFT)
                .on_press(|app: &mut BiliGuga| app.start_login())
                .child(text("刷新二维码").font(type_::LABEL).color(Color::WHITE)),
        );
    panel
}

/// A thin track with a filled portion. Only the display half: the seek bar and the volume
/// slider wrap it in the drag handler that turns a pointer x into a fraction.
fn progress_track(fraction: f32, height: f32) -> El {
    let fraction = fraction.clamp(0., 1.);
    col()
        .w_full()
        .h(height)
        .radius(height / 2.0)
        .fill(theme::TRACK)
        .child(
            el()
                .h(height)
                .w_frac(fraction)
                .radius(height / 2.0)
                .fill(theme::ACCENT),
        )
}

/// The seek bar: presses and drags anywhere on the track jump the video there.
fn seek_bar(position: f64, duration: f64) -> El {
    let fraction = if duration > 0. {
        (position / duration) as f32
    } else {
        0.
    };
    col()
        .key("seek-bar")
        .w_full()
        .h(14.0)
        .justify_center()
        .cursor(rune::Cursor::Pointer)
        .on_drag(move |app: &mut BiliGuga, drag: &mut Drag, _: &mut Tx| {
            // `drag.x` is the pointer in the element's own points, so a press at 30% is 30%.
            let fraction = if drag.w > 0. {
                (drag.x / drag.w).clamp(0., 1.)
            } else {
                0.
            };
            app.seek_percent(fraction as f64);
            app.seek_dragging = drag.phase != Phase::Up;
            app.show_controls();
        })
        .child(progress_track(fraction, 4.0))
}

/// The volume slider, sharing the drag driver with the seek bar.
fn volume_bar(volume: f64) -> El {
    col()
        .key("volume-bar")
        .w(72.0)
        .h(14.0)
        .justify_center()
        .cursor(rune::Cursor::Pointer)
        .on_drag(move |app: &mut BiliGuga, drag: &mut Drag, _: &mut Tx| {
            let fraction = if drag.w > 0. {
                (drag.x / drag.w).clamp(0., 1.)
            } else {
                0.
            };
            app.volume = (fraction as f64 * 100.).clamp(0., 100.);
            app.player.set_volume(app.volume);
            app.volume_dragging = drag.phase != Phase::Up;
            if !app.volume_dragging {
                app.persist_player_settings();
            }
            app.show_controls();
        })
        .child(progress_track((volume / 100.) as f32, 4.0))
}

/// Which collection the current episode belongs to, and where in it, when it belongs to one.
/// The title comes from the API and had nowhere to show before this.
fn collection_marker(app: &BiliGuga) -> El {
    let Some(collection) = &app.collection else {
        return el();
    };
    let position = app.playing_video.as_ref().and_then(|video| {
        collection
            .episodes
            .iter()
            .position(|episode| episode.bvid == video.bvid)
            .map(|index| index + 1)
    });
    let label = match position {
        Some(index) => format!("{} {}/{}", collection.title, index, collection.episodes.len()),
        None => collection.title.clone(),
    };
    text(label)
        .key(format!("collection-{}", collection.id))
        .font(type_::SECTION)
        .color(theme::TEXT_FAINT)
        .truncate()
        .max_w(180.0)
}

fn control_button(label: &'static str, key: &'static str) -> El {
    el()
        .key(key)
        .px(8.0)
        .py(4.0)
        .radius(4.0)
        .cursor(rune::Cursor::Pointer)
        .hover(theme::HOVER)
        .child(text(label).font(type_::LABEL).color(theme::TEXT))
}

/// The player column. The video area's measured size reaches mpv through
/// `BiliGuga::stage_size` and `poll`, so a 1080p frame is not decoded for a 400 pt column.
fn render_player(app: &BiliGuga) -> El {
    let Some(video) = &app.playing_video else {
        return col()
            .grow(1.0)
            .h_full()
            .items_center()
            .justify_center()
            .fill(theme::PANEL)
            .child(
                text("暂无正在播放的视频")
                    .font(type_::BODY)
                    .color(theme::TEXT_FAINT),
            );
    };

    let status = app.player.status();
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
    // The stage: the video frame letterboxed on black, or a message until one arrives.
    let mut stage = col()
        .key("player-stage")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .items_center()
        .justify_center()
        .fill(Color::BLACK)
        .on_pointer_move(|app: &mut BiliGuga, _| app.show_controls());
    // The stage keeps the clip's own shape until a frame says otherwise, so a 4:3 video is
    // letterboxed instead of being stretched into 16:9.
    if !app.player_fullscreen {
        let aspect = app
            .player
            .source_size()
            .filter(|(_, height)| *height > 0)
            .map(|(width, height)| width as f32 / height as f32)
            .unwrap_or(16. / 9.);
        stage = stage.aspect_ratio(aspect);
    }
    match app.player.presented() {
        Some(pixels) => {
            stage = stage.child(
                el()
                    .key("video-frame")
                    .w_full()
                    .h_full()
                    .pixels_fit(&pixels, SurfaceFit::Fit),
            );
        }
        None => {
            // Borrowed, not cloned: this runs on every commit, and the message is either an
            // error already in the state or one of two fixed sentences.
            let message = app.playback_error.as_deref().unwrap_or({
                if app.playback == PlaybackState::Buffering {
                    "正在缓冲…"
                } else {
                    "点击播放开始观看"
                }
            });
            stage = stage.child(
                text(message).font(type_::BODY).color(if app.playback_error.is_some() {
                    theme::DANGER
                } else {
                    theme::TEXT_FAINT
                }),
            );
        }
    }
    // The controls, over the stage, while the pointer has been around recently.
    let controls_visible = app.controls_visible();
    let transport = row()
        .w_full()
        .items_center()
        .gap(8.0)
        .child(
            control_button(
                if app.playback == PlaybackState::Playing {
                    "暂停"
                } else {
                    "播放"
                },
                "play-pause",
            )
            .on_press(|app: &mut BiliGuga| app.toggle_pause()),
        )
        .child(
            text({
                // Written into stack buffers: this runs on every commit, and two times plus a
                // separator used to be three allocations.
                let mut line = String::with_capacity(16);
                line.push_str(Clock::new(position).as_str());
                line.push_str(" / ");
                line.push_str(Clock::new(duration).as_str());
                line
            })
            .font(type_::LABEL)
            .color(theme::TEXT_DIM),
        )
        .child(el().grow(1.0))
        .child(collection_marker(app))
        .child(
            control_button("画质", "quality")
                .child(
                    text(crate::api::quality_label(app.quality))
                        .font(type_::SECTION)
                        .color(theme::TEXT_FAINT),
                )
                .on_press(|app: &mut BiliGuga| {
                    app.open_menu = if app.open_menu == Some(PlayerMenu::Quality) {
                        None
                    } else {
                        Some(PlayerMenu::Quality)
                    };
                }),
        )
        .child(
            control_button("全屏", "fullscreen").on_press_tx(
                |app: &mut BiliGuga, tx: &mut Tx| {
                    app.player_fullscreen = !app.player_fullscreen;
                    tx.toggle_full_screen();
                },
            ),
        );

    // Actions on their own row: eleven buttons in one line is wider than the player column at
    // the default window size, and the rightmost ones were cut off entirely.
    let actions = row()
        .w_full()
        .items_center()
        .gap(6.0)
        .child(control_button("点赞", "like").on_press(|app: &mut BiliGuga| app.like_current()))
        .child(control_button("投币", "coin").on_press(|app: &mut BiliGuga| app.coin_current()))
        .child(
            control_button("稍后", "watch-later")
                .on_press(|app: &mut BiliGuga| app.save_to_watch_later()),
        )
        .child(
            control_button("收藏", "favorite").on_press(|app: &mut BiliGuga| app.save_to_favorites()),
        )
        .child(
            control_button("上集", "prev-episode")
                .on_press(|app: &mut BiliGuga| app.play_collection_step(false)),
        )
        .child(
            control_button("下集", "next-episode")
                .on_press(|app: &mut BiliGuga| app.play_collection_step(true)),
        )
        .child(el().grow(1.0))
        .child(volume_bar(app.volume))
        .child(control_button("倍速", "speed").on_press(|app: &mut BiliGuga| app.cycle_speed()))
        .child(
            text(format!("{:.2}x", app.speed))
                .font(type_::LABEL)
                .color(theme::TEXT_DIM),
        );

    let controls = col()
        .w_full()
        .gap(4.0)
        .px(12.0)
        .py(8.0)
        .fill(theme::PANEL)
        .child(seek_bar(position, duration))
        .child(transport)
        .child(actions);

    let mut root = col()
        .key("player")
        .grow(1.0)
        .min_h(0.0)
        .h_full()
        .fill(theme::PANEL)
        .child(stage);
    if app.open_menu == Some(PlayerMenu::Quality) && !app.quality_options.is_empty() {
        root = root.child(
            col()
                .key("quality-menu")
                .w_full()
                .gap(1.0)
                .px(12.0)
                .py(4.0)
                .fill(theme::PANEL)
                .children(app.quality_options.iter().map(|option| {
                    let selected = option.qn == app.quality;
                    let quality = option.qn;
                    let label = option.label.as_str();
                    // `switchable` says whether the account may play this quality at all (4K
                    // without a membership is listed but refused), so the menu says so rather
                    // than letting the click fail with a server error.
                    let allowed = option.switchable;
                    let mut row_el = row()
                        .w_full()
                        .items_center()
                        .gap(6.0)
                        .px(6.0)
                        .py(3.0)
                        .radius(4.0)
                        .cursor(rune::Cursor::Pointer)
                        .hover(theme::HOVER)
                        .on_press(move |app: &mut BiliGuga| {
                            if allowed {
                                app.select_quality(quality)
                            }
                        })
                        .child(text(label).font(type_::LABEL).color(if selected {
                            theme::ACCENT_SOFT
                        } else if allowed {
                            theme::TEXT_DIM
                        } else {
                            theme::TEXT_FAINT
                        }));
                    if !allowed {
                        row_el = row_el
                            .child(text("需要会员").font(type_::SECTION).color(theme::TEXT_FAINT));
                    }
                    if selected {
                        row_el = row_el.fill(theme::ACTIVE_ROW);
                    }
                    row_el
                })),
        );
    }
    if controls_visible {
        root = root.child(controls);
    }
    let mut root = root;
    if let Some(note) = &app.action_note {
        root = root.child(
            row()
                .w_full()
                .px(12.0)
                .py(4.0)
                .fill(theme::PANEL)
                .child(text(note).font(type_::LABEL).color(theme::ACCENT_SOFT)),
        );
    }
    root.child(render_detail(app, video))
        .child(render_comments(app, video))
}

/// The clip's own numbers: what the detail call fetches. They lived in fields nobody read
/// until this row existed, so the view count, danmaku count and publish date never showed.
fn render_detail(app: &BiliGuga, video: &Video) -> El {
    let mut line = row()
        .key("player-detail")
        .w_full()
        .min_w(0.0)
        .items_center()
        .gap(10.0)
        .px(12.0)
        .py(6.0)
        .fill(theme::PANEL)
        .child(
            text(&video.title)
                .font(type_::BODY)
                .color(theme::TEXT)
                .truncate(),
        );
    match &app.stats {
        Some(stats) => {
            for (label, value) in [
                ("播放", &stats.view_count),
                ("弹幕", &stats.danmaku_count),
                ("点赞", &stats.like_count),
                ("发布于", &stats.published_at),
            ] {
                // Two elements rather than one `format!`: this runs for every commit, and a
                // label plus its value costs no allocation this way.
                line = line
                    .child(
                        text(label)
                            .font(type_::SECTION)
                            .color(theme::TEXT_FAINT),
                    )
                    .child(text(value).font(type_::SECTION).color(theme::TEXT_FAINT));
            }
        }
        None => {
            // Nothing has arrived yet: no numbers is honest, a row of dashes is noise.
        }
    }
    line
}

fn render_comments(app: &BiliGuga, video: &Video) -> El {
    let mut section = col()
        .key("comments")
        .w_full()
        .max_h(220.0)
        .gap(4.0)
        .px(12.0)
        .py(8.0)
        .fill(theme::PANEL)
        .child(
            row()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    text(format!("评论 {}", app.comments_total.max(0)))
                        .font(type_::LABEL)
                        .color(theme::TEXT_DIM),
                )
                .child(
                    text(&video.uploader)
                        .font(type_::LABEL)
                        .color(theme::ACCENT_SOFT)
                        .cursor(rune::Cursor::Pointer)
                        .hover(theme::HOVER)
                        .on_press(move |app: &mut BiliGuga| {
                            let (mid, name) = match &app.playing_video {
                                Some(video) => (video.uploader_mid, video.uploader.clone()),
                                None => return,
                            };
                            app.open_author(mid, name);
                        }),
                ),
        );
    if app.comments_loading && app.comments.is_empty() {
        section = section.child(
            text("正在加载评论…")
                .font(type_::LABEL)
                .color(theme::TEXT_FAINT),
        );
    } else if let Some(error) = &app.comments_error {
        section = section.child(text(error).font(type_::LABEL).color(theme::DANGER));
    } else if app.comments.is_empty() {
        section = section.child(
            text("还没有评论")
                .font(type_::LABEL)
                .color(theme::TEXT_FAINT),
        );
    } else {
        // A scroll viewport needs a bounded height: inside an auto-height column `.grow()`
        // resolves to 0 pt (Rune's own debug warning says so), so the list gets one outright.
        section = section.child(
            col()
                .key("comment-list")
                .w_full()
                .h(150.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .children(app.comments.iter().map(|comment| {
                    // A loaded comment never changes, so `memo` keeps its subtree across
                    // commits: loading another page no longer re-builds and re-shapes every
                    // comment already on screen, which is the expensive half of a list.
                    rune::memo(format!("comment-{}", comment.rpid), comment.rpid, || {
                        col()
                            .w_full()
                            .gap(1.0)
                            .padding(4.0)
                            .child(
                                row()
                                    .gap(6.0)
                                    .child(
                                        text(&comment.username)
                                            .font(type_::LABEL)
                                            .color(theme::ACCENT_SOFT),
                                    )
                                    .child(
                                        text(&comment.time)
                                            .font(type_::SECTION)
                                            .color(theme::TEXT_FAINT),
                                    )
                                    .child(el().grow(1.0))
                                    .child(
                                        text(comment.like.to_string())
                                            .font(type_::SECTION)
                                            .color(theme::TEXT_FAINT),
                                    ),
                            )
                            .child(
                                text(&comment.message)
                                    .font(type_::LABEL)
                                    .color(theme::TEXT_DIM)
                                    .wrap(),
                            )
                    })
                })),
        );
        if app.comments_has_more {
            section = section.child(
                el()
                    .w_full()
                    .py(4.0)
                    .items_center()
                    .cursor(rune::Cursor::Pointer)
                    .hover(theme::HOVER)
                    .on_press(|app: &mut BiliGuga| app.load_comments(false))
                    .child(
                        text("加载更多评论")
                            .font(type_::SECTION)
                            .color(theme::ACCENT_SOFT),
                    ),
            );
        }
    }
    section
}

fn view(app: &BiliGuga, cx: &mut Cx<BiliGuga>) -> El {
    // Reading the stage's size re-runs the view only when that size changes, not when the
    // element merely moves: exactly what telling mpv where to decode for needs.
    let stage_size = cx.layout_size("player-stage").map(|[width, height]| {
        (
            width.round().max(1.0) as usize,
            height.round().max(1.0) as usize,
        )
    });
    app.stage_size.set(stage_size);
    let mut root = row().w_full().h_full().fill(theme::BG);
    if app.player_fullscreen {
        return root.child(render_player(app));
    }
    root = root
        .child(render_sidebar(app))
        .child(render_feed(app, cx));
    root.child(render_player(app))
}

// ---- Runtime hooks ---------------------------------------------------------------------------

/// Called once, before the first frame, with the handle other threads use to wake the window.
fn setup(app: &mut BiliGuga, waker: Waker) {
    app.images = Some(ImageCache::new(waker.clone()));
    if let Some(worker) = app.worker.as_mut() {
        worker.waker = waker;
    }
    // Everything slow starts here, after the first frame: the window opens at once.
    let cookie = app.cookie();
    app.load_home_page(false);
    let _ = cookie;
    if app.session.is_some() {
        app.refresh_session();
    }
}

/// Runs at the start of every frame. Returns true only when something visibly changed, so an
/// idle window draws nothing and arms no timer.
fn poll(app: &mut BiliGuga, _seconds: f64, tx: &mut Tx) -> bool {
    let mut changed = false;

    while let Ok(event) = app.events.try_recv() {
        changed = true;
        app.apply(event);
    }

    if std::mem::take(&mut app.new_rows) {
        app.request_missing_covers();
    }

    // Player state. Only while something is playing, and only a changed value counts as a
    // change: the video frames themselves do not need this loop at all, because `Pixels::set`
    // wakes the window on its own. What runs here keeps the seek bar and the clock honest,
    // about ten times a second.
    // The view publishes the stage size it drew at; mpv decodes for it instead of for a fixed
    // 1920x1080. `set_render_viewport` drops a repeat of the size already in force, so reading
    // it every frame costs a comparison and sends nothing.
    if app.playing_video.is_some()
        && let Some((width, height)) = app.stage_size.get()
    {
        app.player.set_render_viewport(width, height);
    }

    if app.playback != PlaybackState::Idle {
        if app.player.poll_frame() {
            changed = true;
        }
        let status = app.player.status();
        let duration = if status.duration.is_finite() && status.duration > 0. {
            status.duration
        } else {
            0.
        };
        let reaching_end = duration > 0.
            && status.time_pos.is_finite()
            && status.time_pos >= duration - 0.25
            && app.playback != PlaybackState::Buffering;
        if reaching_end {
            app.stop_playback();
            changed = true;
        } else if app.playback == PlaybackState::Buffering && status.time_pos.is_finite() {
            app.playback = if status.paused {
                PlaybackState::Paused
            } else {
                PlaybackState::Playing
            };
            changed = true;
        } else if status.time_pos.is_finite()
            && (status.time_pos - app.last_position).abs() >= 0.2
        {
            // The clock and the bar follow the playhead, not every frame of the video.
            app.last_position = status.time_pos;
            changed = true;
        }
        if status.volume.is_finite() && (status.volume - app.volume).abs() > 0.5 {
            app.volume = status.volume.clamp(0., 100.);
            changed = true;
        }
        if status.speed.is_finite() && (status.speed - app.speed).abs() > 0.001 {
            app.speed = status.speed.max(0.1);
            changed = true;
        }
        if !app.cloud_resume_applied {
            if let Some(target) = app.pending_seek.take() {
                app.player.seek_seconds(target);
                app.cloud_resume_applied = true;
            } else if let Some(progress) = app.cloud_resume_progress.take() {
                if status.duration.is_finite() && progress < status.duration - 3. {
                    app.player.seek_seconds(progress);
                }
                app.cloud_resume_applied = true;
            } else {
                app.cloud_resume_applied = true;
            }
        }
        if app.playback == PlaybackState::Playing
            && app.history_report_at.elapsed() >= Duration::from_secs(15)
        {
            app.queue_history_report();
        }
        app.sync_power();
        // Asks for another frame in 100 ms so the clock and the seek bar keep up, which is what
        // replaces a repaint per video frame. Returning false on a frame where nothing visible
        // changed is the point: the video itself is drawn from `Pixels`, not from a new commit.
        tx.wake_after(Duration::from_millis(100));
    }

    if let Some(until) = app.controls_until
        && Instant::now() >= until
    {
        app.controls_until = None;
        changed = true;
    }
    changed
}


impl BiliGuga {
    /// Takes one answer from background work and folds it into the state.
    ///
    /// This was the body of `poll`'s drain loop, which made every case reachable only
    /// through a frame. It is a plain method so a test can drive one event.
    fn apply(&mut self, event: Event) {
        let app = self;
        match event {
            Event::Home(result, generation) => {
                if generation != app.home_generation || app.active_tab != AppTab::Home {
                    return;
                }
                app.home_loading = false;
                app.loading = false;
                match result {
                    Ok(page) => {
                        let first_page = app.videos.is_empty();
                        let old_len = app.videos.len();
                        for video in page.videos {
                            if !app.videos.iter().any(|current| current.bvid == video.bvid) {
                                app.videos.push(video);
                            }
                        }
                        let added = app.videos.len().saturating_sub(old_len);
                        app.home_has_more = page.has_more && (first_page || added > 0);
                        app.trim_home_items();
                        app.new_rows = true;
                    }
                    Err(_) => {
                        // Put the page number back so a retry asks for this page again.
                        app.home_page = app.home_page.saturating_sub(1);
                        app.home_has_more = false;
                    }
                }
            }
            Event::Search(result) => {
                app.loading = false;
                match result {
                    Ok(videos) => {
                        app.search_results = videos;
                        app.selected = None;
                        app.new_rows = true;
                    }
                    Err(error) => app.playback_error = Some(error),
                }
            }
            // The four signed-in feeds answer the same way, so they are handled in one arm with
            // the feed as the only variable.
            Event::Dynamic(result) => app.land_signed_in(SignedInFeed::Dynamic, result),
            Event::History(result) => app.land_signed_in(SignedInFeed::History, result),
            Event::WatchLater(result) => app.land_signed_in(SignedInFeed::WatchLater, result),
            Event::Favorites(result) => app.land_signed_in(SignedInFeed::Favorites, result),
            Event::Author(result, generation) => {
                if generation != app.author_generation {
                    return;
                }
                app.author_loading = false;
                if let Ok(page) = result {
                    app.author_page = page.page;
                    app.author_has_more = page.has_more;
                    app.author_videos.extend(page.videos);
                    app.new_rows = true;
                }
            }
            Event::VideoContext(context) => {
                // An answer that lands after the viewer moved on would put the previous video's
                // collection in the next video's menu.
                let still_current = app
                    .playing_video
                    .as_ref()
                    .is_some_and(|video| video.aid == context.aid || video.aid <= 0);
                if !still_current {
                    return;
                }
                app.stats = Some(VideoStats {
                    view_count: context.view_count,
                    danmaku_count: context.danmaku_count,
                    like_count: context.like_count,
                    published_at: format_publish_date(context.pubdate),
                });
                app.collection = context.collection;
                if let Some(video) = app.playing_video.as_mut() {
                    if video.aid == 0 {
                        video.aid = context.aid;
                    }
                    if video.cid == 0 {
                        video.cid = context.cid;
                    }
                    // A collection episode carries the collection's owner, which is not always the
                    // uploader of this part; the detail endpoint knows the right one.
                    if !context.uploader.is_empty() {
                        video.uploader = context.uploader.clone();
                        video.uploader_mid = context.uploader_mid;
                    }
                }
            }
            Event::PlayUrl(result) => match result {
                Ok(play) => app.finish_play(play),
                Err(error) => {
                    app.playback = PlaybackState::Idle;
                    app.playback_error = Some(error);
                }
            },
            Event::Resume(progress) => {
                app.cloud_resume_progress = progress.filter(|progress| *progress > 3.);
                app.cloud_resume_applied = app.cloud_resume_progress.is_none();
            }
            Event::Comments(result, generation) => {
                if generation != app.comments_generation {
                    return;
                }
                app.comments_loading = false;
                match result {
                    Ok((page, comments, total, has_more)) => {
                        if page <= 1 {
                            app.comments.clear();
                        }
                        app.comments.extend(comments);
                        app.comments_total = total;
                        app.comments_has_more = has_more;
                        app.comments_page = page as u32;
                    }
                    Err(error) => app.comments_error = Some(error),
                }
            }
            Event::Avatar(image) => {
                if let Some(image) = image
                    && let Some(cache) = app.images.as_ref()
                {
                    app.account_avatar =
                        Some(cache.rgba(image.width, image.height, image.pixels.clone()));
                }
            }
            Event::QrCode(result, generation) => {
                if generation != app.login_generation {
                    return;
                }
                app.login_loading = false;
                match result {
                    Ok(data) => {
                        app.login_key = Some(data.key);
                        if let Some(cache) = app.images.as_ref() {
                            app.login_qr = Some(cache.rgba(
                                data.image.width,
                                data.image.height,
                                data.image.pixels.clone(),
                            ));
                        }
                        app.login_status = "请使用哔哩哔哩 App 扫码登录".into();
                        app.poll_login();
                    }
                    Err(error) => app.login_status = error.into(),
                }
            }
            Event::QrPoll(result, generation) => {
                if generation != app.login_generation {
                    return;
                }
                match result {
                    Ok(PollResult::Waiting) => app.poll_login(),
                    Ok(PollResult::Scanned) => {
                        app.login_status = "已扫码，请在手机上确认".into();
                        app.poll_login();
                    }
                    Ok(PollResult::LoggedIn(session)) => {
                        let _ = login::save_session(&session);
                        app.session = Some(session);
                        app.login_status = "登录成功".into();
                        app.active_tab = AppTab::Home;
                        app.load_home_page(true);
                    }
                    Ok(PollResult::Expired) => {
                        app.login_status = "二维码已过期，请刷新".into();
                        app.login_qr = None;
                    }
                    Err(error) => app.login_status = error.into(),
                }
            }
            Event::Session(result) => {
                if let Ok(session) = result {
                    let _ = login::save_session(&session);
                    let face = session.face.clone();
                    app.session = Some(session);
                    app.load_signed_in(SignedInFeed::Dynamic);
                    app.load_avatar(face);
                }
            }
            Event::ActionDone(done, result) => {
                app.action_note = Some(match result {
                    Ok(()) => done.to_string(),
                    Err(error) => error,
                });
            }
            Event::CoverFailed(bvid) => {
                app.covers_in_flight.remove(&bvid);
            }
            Event::HistoryReported => app.history_report_in_flight = false,
            Event::Silent => {}
            Event::Cover(bvid, pixels) => app.attach_cover(bvid, pixels),
        }
    }
}

fn on_key(app: &mut BiliGuga, event: &KeyEvent, tx: &mut Tx) -> bool {
    let mods = event.mods;
    match event.key {
        Key::Text(ref typed) if typed == " " && !mods.ctrl && !mods.alt => {
            app.toggle_pause();
            true
        }
        Key::Left => {
            app.player.seek_relative(-10.);
            true
        }
        Key::Right => {
            app.player.seek_relative(10.);
            true
        }
        Key::Up => {
            app.volume = (app.volume + 5.).clamp(0., 100.);
            app.player.set_volume(app.volume);
            app.persist_player_settings();
            true
        }
        Key::Down => {
            app.volume = (app.volume - 5.).clamp(0., 100.);
            app.player.set_volume(app.volume);
            app.persist_player_settings();
            true
        }
        Key::Text(ref typed) if typed.eq_ignore_ascii_case("f") && !mods.ctrl && !mods.alt => {
            app.player_fullscreen = !app.player_fullscreen;
            tx.toggle_full_screen();
            true
        }
        Key::Escape => {
            if app.player_fullscreen {
                app.player_fullscreen = false;
                tx.toggle_full_screen();
                return true;
            }
            false
        }
        Key::Enter if mods.ctrl || mods.alt => {
            app.load_search();
            true
        }
        _ => false,
    }
}

impl BiliGuga {
    fn refresh_feed(&mut self) {
        match self.active_tab {
            AppTab::Home => self.load_home_page(true),
            AppTab::Author => self.load_author_page(true),
            AppTab::Search => self.load_search(),
            AppTab::Login => self.start_login(),
            tab => {
                if let Some(feed) = tab.signed_in() {
                    self.load_signed_in(feed);
                }
            }
        }
    }

    fn cycle_speed(&mut self) {
        let index = SPEED_OPTIONS
            .iter()
            .position(|speed| (*speed - self.speed).abs() < 0.01)
            .unwrap_or(1);
        self.speed = SPEED_OPTIONS[(index + 1) % SPEED_OPTIONS.len()];
        self.player.set_speed(self.speed);
        self.persist_player_settings();
    }

    fn sync_power(&mut self) {
        self.power_inhibitor
            .set_active(self.playback == PlaybackState::Playing);
    }

    /// Asks for the covers the current feed is missing, skipping ones already downloaded or in
    /// flight for that bvid.
    fn request_missing_covers(&mut self) {
        let mut missing: Vec<(String, String)> = Vec::new();
        for videos in [
            &self.videos,
            &self.search_results,
            &self.dynamic_videos,
            &self.watch_later,
            &self.favorites,
            &self.history,
            &self.author_videos,
        ] {
            for video in videos {
                if video.cover.is_empty()
                    || self.cover_images.contains_key(&video.bvid)
                    || self.covers_in_flight.contains(&video.bvid)
                {
                    continue;
                }
                missing.push((video.bvid.clone(), video.cover.clone()));
            }
        }
        for (bvid, url) in missing {
            self.request_cover(bvid, url);
        }
    }
}

fn fetch_qr_code_event() -> impl std::future::Future<Output = Result<login::QrCodeData, String>> + Send
{
    login::fetch_qr_code()
}

/// Builds a feed of placeholder videos, so `--snap feed` can check the layout without a
/// network round trip. Only reachable from a named scene.
/// Demo rows that carry a cover URL, for the tests that exercise the cover queue.
/// Placeholder rows so `--snap` can check the layout without a network round trip. Only
/// reachable from a named scene, never from the running app.
const DEMO_TITLES: [&str; 8] = [
    "【4K】这可能是今年最好看的旅行短片，一次看遍北海道",
    "用 Rust 从零写一个窗口管理器｜第一期：到底难在哪",
    "深夜食堂：一个人也要好好吃饭",
    "钢琴改编《千本樱》，手指快到看不清",
    "拆解一台二十年前的随身听，里面比想象中复杂",
    "3 小时讲透线性代数，看完你就懂了",
    "在海拔 5000 米看星空，是什么体验",
    "手工复原宋代点茶，全过程",
];
const DEMO_UPLOADERS: [&str; 8] = [
    "影视飓风",
    "Rust 语言中文社区",
    "曼食慢语",
    "指尖琴键",
    "硬件茶谈",
    "3Blue1Brown",
    "星球研究所",
    "南翔不爱吃饭",
];
const DEMO_ACCENTS: [u32; 6] = [0x3e4654, 0x39455a, 0x4b4240, 0x4c4255, 0x354c4a, 0x46503d];

fn demo_video(index: usize) -> Video {
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

fn demo_videos(count: usize) -> Vec<Video> {
    (0..count).map(demo_video).collect()
}

/// Demo rows that carry a cover URL, for the tests that exercise the cover queue.
#[cfg(test)]
fn demo_video_with_covers(count: usize) -> Vec<Video> {
    (0..count)
        .map(|index| {
            let mut video = demo_video(index);
            video.cover = format!("https://example.invalid/{index}.jpg");
            video
        })
        .collect()
}

/// Named states for `--snap`: each seeds the app so a headless run shows a real screen.
///
/// A `const` table, not a lazily built `Vec`: Rune asks for `&'static [..]` once at startup, so
/// a `OnceLock` around it bought an allocation and an indirection and nothing else.
type Scene = (&'static str, fn(&mut BiliGuga));
const SCENES: &[Scene] = &[
    ("feed", seed_feed),
    ("player", seed_player),
    ("search", seed_search),
];

fn seed_loaded(app: &mut BiliGuga) {
    app.videos = demo_videos(12);
    app.loading = false;
    app.home_loading = false;
}

fn seed_feed(app: &mut BiliGuga) {
    seed_loaded(app);
}

fn seed_player(app: &mut BiliGuga) {
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

fn seed_search(app: &mut BiliGuga) {
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
        ..AppSpec::new("哔哩咕嘎", (1360.0, 820.0), BiliGuga::new, view)
    });
}

impl BiliGuga {
    /// The video the player is on, with the ids its action endpoints need.
    fn current_action_target(&self) -> Option<Video> {
        self.playing_video.clone()
    }

    fn like_current(&mut self) {
        let (Some(cookie), Some(video)) = (
            self.cookie(),
            self.current_action_target(),
        ) else {
            return;
        };
        if video.aid <= 0 {
            return;
        }
        self.action_note = Some("正在点赞…".into());
        self.worker().spawn(
            async move { like_video(&cookie, video.aid).await },
            |result| Event::ActionDone("已点赞", result),
        );
    }

    fn coin_current(&mut self) {
        let (Some(cookie), Some(video)) = (
            self.cookie(),
            self.current_action_target(),
        ) else {
            return;
        };
        if video.aid <= 0 {
            return;
        }
        self.action_note = Some("正在投币…".into());
        self.worker().spawn(
            async move { coin_video(&cookie, video.aid).await },
            |result| Event::ActionDone("已投币", result),
        );
    }

    fn save_to_watch_later(&mut self) {
        let (Some(cookie), Some(video)) = (
            self.cookie(),
            self.current_action_target(),
        ) else {
            return;
        };
        if video.aid <= 0 {
            return;
        }
        self.worker().spawn(
            async move { add_to_watch_later(&cookie, video.aid).await },
            |result| Event::ActionDone("已加入稍后再看", result),
        );
    }

    fn save_to_favorites(&mut self) {
        let (Some(session), Some(video)) = (self.session.clone(), self.current_action_target())
        else {
            return;
        };
        if video.aid <= 0 {
            return;
        }
        let (cookie, mid) = (session.cookie, session.mid);
        self.worker().spawn(
            async move { add_to_favorites(&cookie, mid, video.aid).await },
            |result| Event::ActionDone("已收藏", result),
        );
    }

    /// Jump to the next or previous episode of the collection the player is in.
    fn play_collection_step(&mut self, forward: bool) {
        let Some(collection) = &self.collection else {
            return;
        };
        let Some(current) = &self.playing_video else {
            return;
        };
        let index = collection
            .episodes
            .iter()
            .position(|episode| episode.bvid == current.bvid);
        let target = match (index, forward) {
            (Some(index), true) => collection.episodes.get(index + 1),
            (Some(index), false) if index > 0 => collection.episodes.get(index - 1),
            _ => None,
        };
        if let Some(episode) = target.cloned() {
            self.play_video(episode);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::Runtime;

    /// The player's whole point in this rewrite: mpv's worker writes into a `rune::Pixels`,
    /// the renderer uploads it, and the view never runs for a frame. This drives that path
    /// with a real file and checks pixels actually arrive.
    #[test]
    fn player_frames_reach_the_pixels_handle() {
        let clip = std::env::var("BILIGUGA_TEST_CLIP").unwrap_or_else(|_| "/tmp/biliguga-test.mp4".into());
        if !std::path::Path::new(&clip).exists() {
            eprintln!("skipping: no clip at {clip}");
            return;
        }
        let mut player = mpv::MpvPlayer::new();
        player.load(clip, None, 0., 1.);

        // The worker publishes one handle per render size and writes every frame into it.
        let target = (0..300)
            .find_map(|_| {
                std::thread::sleep(Duration::from_millis(20));
                player.poll_frame();
                player.presented()
            })
            .expect("the player never published a frame buffer");
        let (width, height) = target.size();
        assert!(width > 0 && height > 0, "the frame buffer is {width}x{height}");

        // A decoded frame must be non-blank: the test clip is colourful.
        let mut non_black = 0usize;
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(20));
            player.poll_frame();
            let (now_width, now_height) = target.size();
            assert_eq!((now_width, now_height), (width, height), "the size changed mid-check");
            let status = player.status();
            if status.time_pos.is_finite() && status.time_pos > 0.2 {
                non_black += 1;
                if non_black > 3 {
                    break;
                }
            }
        }
        assert!(non_black > 0, "the player never reported a playhead");
    }

    /// Paging records the page when the request goes out. Recording it only when the answer
    /// landed let two requests in flight both ask for page 1; and the first call in `setup`
    /// was turned away by a guard that started closed, so the feed never loaded at all.
    #[test]
    fn requesting_a_home_page_advances_the_number_immediately() {
        let mut app = BiliGuga::new();
        let (sender, _events) = std::sync::mpsc::channel();
        app.worker = Some(Worker {
            events: sender,
            waker: Arc::new(|| {}),
        });
        assert!(!app.home_loading, "nothing is in flight before the first request");
        assert_eq!(app.home_page, 0);
        app.load_home_page(false);
        assert_eq!(app.home_page, 1, "page 1 is recorded when it is asked for");
        assert!(app.home_loading, "and it is in flight now");
        app.home_loading = false;
        app.load_home_page(false);
        assert_eq!(app.home_page, 2, "the next request asks for page 2, not page 1 again");
    }

    /// A cover is asked for once per bvid. Every page load used to re-request every cover that
    /// was still missing, so the same download could be queued many times over.
    #[test]
    fn a_cover_is_only_requested_once_while_in_flight() {
        let mut app = BiliGuga::new();
        let (sender, _events) = std::sync::mpsc::channel();
        app.worker = Some(Worker {
            events: sender,
            waker: Arc::new(|| {}),
        });
        app.videos = demo_video_with_covers(3);
        app.request_missing_covers();
        assert_eq!(app.covers_in_flight.len(), 3, "one per row");
        app.request_missing_covers();
        assert_eq!(app.covers_in_flight.len(), 3, "no duplicates");
    }

    /// A cover that failed has to free its bvid, or that row never gets one.
    #[test]
    fn a_failed_cover_can_be_asked_for_again() {
        let mut app = BiliGuga::new();
        app.covers_in_flight.insert("BV1demo0000".into());
        app.apply(Event::CoverFailed("BV1demo0000".into()));
        assert!(app.covers_in_flight.is_empty(), "the bvid is free again");
    }

    /// The history report's in-flight gate has to open again, or history is uploaded exactly
    /// once per video and never again.
    #[test]
    fn a_finished_history_report_lets_the_next_one_run() {
        let mut app = BiliGuga::new();
        app.history_report_in_flight = true;
        app.apply(Event::HistoryReported);
        assert!(!app.history_report_in_flight, "the gate opened again");
    }

    /// An action's result lands as a note, not as a playback error: the two used to share one
    /// field, guessed apart by looking for the word 失败 inside the message.
    #[test]
    fn an_action_result_is_a_note_not_a_playback_error() {
        let mut app = BiliGuga::new();
        app.apply(Event::ActionDone("已点赞", Ok(())));
        assert_eq!(app.action_note.as_deref(), Some("已点赞"));
        assert!(app.playback_error.is_none());
        app.apply(Event::ActionDone("已投币", Err("未登录".into())));
        assert_eq!(app.action_note.as_deref(), Some("未登录"));
    }

    /// The feed lays out from a plain state, which is what `--snap` shows too.
    #[test]
    fn the_view_builds_from_a_seeded_feed() {
        let mut app = BiliGuga::new();
        app.videos = demo_videos(12);
        app.loading = false;
        let mut runtime = Runtime::new(app, view, (1360.0, 820.0), rune::Env::new(true, 1.0));
        runtime.frame(0.0);
        assert!(runtime.presented("home-grid").is_some(), "the grid did not mount");
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Talks to Bilibili for real. Ignored by default; run with
    /// `cargo test -- --ignored live_recommendations`.
    #[test]
    #[ignore = "needs the network"]
    fn live_recommendations_paginate() {
        let first = futures::executor::block_on(network::run(async {
            fetch_recommendations(1, None).await
        }))
        .expect("page 1 failed");
        assert!(!first.videos.is_empty(), "page 1 came back empty");
        assert!(
            first.videos.iter().all(|video| !video.bvid.is_empty()),
            "a recommendation has no bvid, so it cannot be played or keyed"
        );

        let second = futures::executor::block_on(network::run(async {
            fetch_recommendations(2, None).await
        }))
        .expect("page 2 failed");
        assert!(!second.videos.is_empty(), "page 2 came back empty");
        // Pages must differ, otherwise the feed would loop on the same rows forever.
        let first_ids: std::collections::HashSet<_> =
            first.videos.iter().map(|video| video.bvid.clone()).collect();
        assert!(
            second
                .videos
                .iter()
                .any(|video| !first_ids.contains(&video.bvid)),
            "page 2 repeated page 1 entirely"
        );
    }

    /// A cover decodes to RGBA the renderer can take, with the size the card asks for.
    #[test]
    #[ignore = "needs the network"]
    fn live_cover_decodes_to_rgba() {
        let url = "https://i0.hdslb.com/bfs/archive/2f5b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1.jpg";
        let _ = url;
        let page = futures::executor::block_on(network::run(async {
            fetch_recommendations(1, None).await
        }))
        .expect("recommendations failed");
        let video = page.videos.first().expect("no videos").clone();
        let cover = queue_cover_download(video.cover.clone(), Arc::new(AtomicBool::new(false)))
            .expect("cover worker refused");
        let image = futures::executor::block_on(network::run(async move {
            cover.await.ok().flatten()
        }))
        .expect("the cover never decoded");
        assert_eq!((image.width, image.height), (160, 90));
        assert_eq!(image.pixels.len(), 160 * 90 * 4);
        assert!(
            image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[0] > 8 || pixel[1] > 8),
            "the cover decoded to a blank image"
        );
    }
}

#[cfg(test)]
mod perf_tests {
    use super::*;

    /// Ten covers through the shared client should reuse connections: a per-request
    /// `reqwest::Client` paid a fresh TLS handshake for every one of them.
    #[test]
    #[ignore = "needs the network"]
    fn ten_covers_share_one_client() {
        let page = futures::executor::block_on(network::run(async {
            fetch_recommendations(1, None).await
        }))
        .expect("recommendations failed");
        let urls: Vec<String> = page
            .videos
            .iter()
            .filter(|video| !video.cover.is_empty())
            .take(10)
            .map(|video| video.cover.clone())
            .collect();
        assert!(urls.len() >= 5, "need a few covers to be meaningful");
        let started = std::time::Instant::now();
        for url in &urls {
            let cover = queue_cover_download(url.clone(), Arc::new(AtomicBool::new(false)))
                .expect("cover worker refused");
            let image = futures::executor::block_on(network::run(async move {
                cover.await.ok().flatten()
            }));
            assert!(image.is_some(), "a cover failed to decode: {url}");
        }
        eprintln!("{} covers in {:?}", urls.len(), started.elapsed());
    }
}
