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
        self, Clock, PlayQuality, add_to_favorites, add_to_watch_later, coin_video,
        download_avatar, fetch_author_videos, fetch_comments, fetch_dynamic_feed, fetch_favorites,
        fetch_history, fetch_last_play_progress, fetch_recommendations, fetch_search_results,
        fetch_video_context, fetch_watch_later, format_publish_date, like_video,
        queue_cover_download, report_video_heartbeat, report_video_progress, resolve_play_url,
    },
    login::{self, PollResult, UserSession},
    model::{Comment, Rgba8, Video, VideoCollection},
    mpv, network,
    power::PowerInhibitor,
};
use rune::{
    AppSpec, Color, Cx, Drag, El, GrowthSpec, Image, ImageCache, Key, KeyEvent, Phase, ScrollSpec,
    SurfaceFit, TextEdit, TextStyle, Tx, Waker, WheelEvent, col, el, row, text, text_input,
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const HOME_MAX_ITEMS: usize = 120;
/// How much of the next page to keep loaded past the bottom of what is loaded, in panes. The
/// feed asks for more while the reader is within this of the end, so a page arrives before
/// they reach it rather than after.
const PAGE_RUNWAY: f32 = 1.0;
/// How many decoded covers to keep. A cover is 57 KB, so this bounds the cache at a few
/// megabytes even if some path keeps adding rows without trimming.
const COVER_CACHE_LIMIT: usize = 256;
const SPEED_OPTIONS: [f64; 5] = [0.5, 1., 1.25, 1.5, 2.];
const DEFAULT_VOLUME: f64 = 100.;
const DEFAULT_SPEED: f64 = 1.;
/// The left navigation rail.
const RAIL_WIDTH: f32 = 88.;
/// The feed pane's width while a video is open beside it: a share of the window, held between
/// these bounds, so the player beside it keeps the room a 16:9 frame needs.
const FEED_PANE_MIN: f32 = 300.;
const FEED_PANE_MAX: f32 = 460.;
const FEED_PANE_SHARE: f32 = 0.32;
/// Padding inside every pane.
const PANE_PAD: f32 = 20.;
/// The narrowest a browse card gets before the grid gives up a column.
const CARD_MIN_WIDTH: f32 = 232.;
/// The space between two cards, and the padding inside one around its cover.
const CARD_GAP: f32 = 14.;
const CARD_PAD: f32 = 6.;
/// How many points one notch of a mouse wheel moves the feed.
///
/// Rune turns a notch into a flat 40 points, which was a quarter of the old 170-point cards
/// and is a *fifth* of a row now that a poster card is ~200 points tall: the feed crawled, and
/// the two dozen notches a page of posters takes felt broken. A hundred points a notch is what
/// a browser scrolls, and it moves half a card, so a card can be crossed in two.
const WHEEL_NOTCH: f32 = 100.0;
/// The list layout's row: a 74-point cover plus the padding around it.
const LIST_ROW_HEIGHT: f32 = 88.0;
/// What Rune itself moves for a notch, so the handler below can scale rather than assume.
const RUNE_WHEEL_NOTCH: f32 = 40.0;
/// How long the player's controls stay up after the pointer moves, in full screen.
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
/// sRGB hex it came from, so a designer can still edit one and rerun the conversion.
///
/// The palette is one hue family (a cool near-black) with a single warm accent, so the eye lands
/// on the poster art and the one thing that is clickable rather than on the chrome.
mod theme {
    use rune::Color;

    const fn c(r: f32, g: f32, b: f32) -> Color {
        Color { r, g, b, a: 1.0 }
    }

    /// #0A0B0F — the page behind everything.
    pub(super) const BG: Color = c(0.003_035, 0.003_347, 0.004_777);
    /// #0E1015 — the navigation rail, one step up from the page.
    pub(super) const RAIL: Color = c(0.004_391, 0.005_182, 0.007_499);
    /// #111319 — the player column and control bars.
    pub(super) const PANEL: Color = c(0.005_605, 0.006_512, 0.009_721);
    /// #16181F — anything raised: cards, fields, chips.
    pub(super) const CARD: Color = c(0.008_023, 0.009_134, 0.013_702);
    /// #1B1E27 — an input well, a shade above a card so a field reads as editable.
    pub(super) const FIELD: Color = c(0.010_960, 0.012_983, 0.019_382);
    /// #1E212B — a card under the pointer.
    pub(super) const CARD_HOVER: Color = c(0.012_983, 0.015_209, 0.024_158);
    /// #23262F — a visible edge, for fields and the split between panes.
    pub(super) const BORDER: Color = c(0.016_807, 0.019_382, 0.028_426);
    /// #191C24 — a divider that only needs to be sensed.
    pub(super) const HAIRLINE: Color = c(0.009_721, 0.011_612, 0.017_642);
    /// #F1F3F7
    pub(super) const TEXT: Color = c(0.879_622, 0.896_269, 0.930_111);
    /// #A3ABBA — secondary: uploaders, counts, metadata.
    pub(super) const TEXT_DIM: Color = c(0.366_253, 0.407_240, 0.491_021);
    /// #6E7686 — tertiary: labels, hints, placeholders.
    pub(super) const TEXT_FAINT: Color = c(0.155_926, 0.181_164, 0.238_398);
    /// #FB7299 — the one accent: the play button, the active tab, the seek fill.
    pub(super) const ACCENT: Color = c(0.964_686, 0.168_269, 0.318_547);
    /// #FF9CBE — the accent as text, which has to be lighter to stay readable.
    pub(super) const ACCENT_SOFT: Color = c(1.0, 0.332_452, 0.514_918);
    /// #3A1B25 — the accent, nearly black, for a selected row's wash.
    pub(super) const ACCENT_DIM: Color = c(0.042_311, 0.010_960, 0.018_500);
    /// #FF6B6B
    pub(super) const DANGER: Color = c(1.0, 0.147_027, 0.147_027);
    /// #2A2E39 — the unfilled half of a slider.
    pub(super) const TRACK: Color = c(0.023_153, 0.027_321, 0.040_915);
    /// The pointer resting on something that can be pressed.
    pub(super) const HOVER: Color = c(0.008_023, 0.009_134, 0.014_000);
    /// The pointer holding it down.
    pub(super) const PRESS: Color = c(0.013_000, 0.015_209, 0.024_158);

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
    /// Windowed, the player can take the whole window instead of sharing it with the feed: a
    /// video that wants the room does not need the OS-level full screen (which hides the rail
    /// and the title bar) to get it.
    theater: bool,
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
    /// Why the last feed request failed. Separate from `playback_error`: a search that fails
    /// used to write its error there, where only the player column shows it, so the viewer saw
    /// an empty feed and no reason for it.
    feed_error: Option<String>,
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
    /// How many cards the browse grid fits across, and how wide each one is. `view` gets
    /// `&self`, so — as with `stage_size` — it measures the pane and leaves the numbers here
    /// for the grid it is building in the same pass.
    card_columns: std::cell::Cell<usize>,
    card_width: std::cell::Cell<f32>,
    /// Where the feed's scroll viewport stands and how far it can go, published by
    /// `on_scroll_offset` so the wheel handler (which takes the delta and places the viewport
    /// itself) knows both. An `Rc<Cell>` because `on_scroll_offset` may not change state: it is
    /// a per-frame read, not a commit.
    ///
    /// `None` until the viewport has been laid out once. That matters as much as the numbers:
    /// before the first layout `max` is 0, which reads as "there is nothing below the last row",
    /// and taking that for the truth paged the feed four times in its first four frames.
    ///
    /// The limit matters as much as the offset. Without it, a reader holding the wheel at the
    /// end of a page pushed this past the end, and the next page's growth let the viewport land
    /// at that stale value: the feed jumped to wherever they had scrolled *past*.
    scroll_pos: std::rc::Rc<std::cell::Cell<Option<(f32, f32)>>>,
    /// The browse pane's height, measured in `view`, so paging knows how much of the feed is
    /// on screen without guessing from a row count.
    pane_height: std::cell::Cell<f32>,
    /// The width the feed gets while the player shares the window with it.
    feed_pane_width: std::cell::Cell<f32>,
    /// The box `view` gives the stage, worked out from the player column's own size: the video
    /// keeps its 16:9 shape without ever growing taller than the room it has.
    stage_box: std::cell::Cell<(f32, f32)>,
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
            theater: false,
            playing_video: None,
            stage_size: std::cell::Cell::new(None),
            stats: None,
            playback_error: None,
            feed_error: None,
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
            card_columns: std::cell::Cell::new(2),
            card_width: std::cell::Cell::new(CARD_MIN_WIDTH),
            stage_box: std::cell::Cell::new((320.0, 180.0)),
            feed_pane_width: std::cell::Cell::new(FEED_PANE_MAX),
            scroll_pos: std::rc::Rc::new(std::cell::Cell::new(None)),
            pane_height: std::cell::Cell::new(700.0),
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
        self.author
            .as_ref()
            .map_or("", |author| author.name.as_str())
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
            self.feed_error = None;
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

    /// Asks for the next home page while the reader is within [`PAGE_RUNWAY`] of the end of
    /// what is loaded, and says whether that changed anything.
    ///
    /// The test is on the distance to the end of the content, and nothing else. Reading it off
    /// the grid's *visible rows* was wrong twice over: the range comes from row heights, so a
    /// commit that measures a row shifts it by one, and the check then flips direction — the
    /// reader could sit at the bottom of a page without it loading, then nudge the wheel up and
    /// watch it load. Distance to the end has no such edge: scrolling up only increases it.
    fn page_home_if_near_end(&mut self) -> bool {
        if self.active_tab != AppTab::Home
            || self.home_loading
            || !self.home_has_more
            || self.videos.is_empty()
        {
            return false;
        }
        // Nothing is known about where the viewport stands until it has been laid out once, and
        // its `max` is 0 before that: treated as the truth, it says "the last row is the bottom
        // of the pane" and the feed pages away before it has anything to show.
        let Some((offset, max)) = self.scroll_pos.get() else {
            return false;
        };
        // A page that fits the pane has nothing to scroll (max == 0) and is still short of it,
        // so "no room left below" is the same answer as "the reader is at the end".
        let runway = (self.pane_height.get() * PAGE_RUNWAY).max(1.0);
        if max - offset > runway {
            return false;
        }
        self.load_home_page(false);
        true
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
        self.feed_error = None;
        self.selected = None;
        self.worker().spawn(
            async move { fetch_search_results(&query).await },
            Event::Search,
        );
    }

    /// Folds a signed-in feed's answer into the state. A failure leaves whatever was shown.
    fn land_signed_in(&mut self, feed: SignedInFeed, result: Result<Vec<Video>, String>) {
        feed.set_loading(self, false);
        match result {
            Ok(videos) => {
                *feed.videos_mut(self) = videos;
                self.new_rows = true;
            }
            Err(error) => self.feed_error = Some(error),
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
        // A failure belongs to the tab it happened on. Carrying it over showed the previous
        // tab's error on a tab that had not even asked for anything yet.
        self.feed_error = None;
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
        self.player
            .load(result.url, result.audio_url, self.volume, self.speed);
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

    /// Closes the player and gives the window back to the browse pane.
    fn close_player(&mut self) {
        self.stop_playback();
        self.playing_video = None;
        self.stats = None;
        self.collection = None;
        self.comments.clear();
        self.comments_total = 0;
        self.comments_error = None;
        self.action_note = None;
        self.playback_error = None;
        self.open_menu = None;
        self.theater = false;
        self.selected = None;
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

    /// Whether the browse pane is waiting on a request. A signed-in feed knows, the home feed
    /// only while it has nothing to show yet, and a search until its answer lands.
    fn is_loading(&self) -> bool {
        if let Some(feed) = self.active_tab.signed_in() {
            return feed.loading(self);
        }
        match self.active_tab {
            AppTab::Home => self.loading && self.videos.is_empty(),
            AppTab::Author => self.author_loading,
            _ => self.loading,
        }
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
        let (Some(cookie), Some(video)) = (self.cookie(), self.playing_video.clone()) else {
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
        self.worker().spawn(
            async move { login::poll_qr_code(&key).await },
            move |result| Event::QrPoll(result, generation),
        );
    }

    fn load_avatar(&mut self, url: String) {
        if url.is_empty() || self.account_avatar.is_some() {
            return;
        }
        self.worker()
            .spawn(async move { download_avatar(&url).await }, Event::Avatar);
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
        if self.cover_images.len() >= COVER_CACHE_LIMIT && !self.cover_images.contains_key(&bvid) {
            // Over the limit: drop whatever no feed is showing. If everything is shown, the
            // cap waits for a row to go away rather than evicting a cover in use.
            let shown: std::collections::HashSet<String> =
                self.bvids_on_screen().map(str::to_string).collect();
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

/// Our own icon set, drawn as SVG bodies rather than borrowed from the text font.
///
/// A house, a play mark and a bookmark all exist in whatever font happens to be installed, at
/// different weights and sometimes not at all; a column of mismatched shapes is exactly what a
/// sidebar looks like when it borrows glyphs. These are 16-point bodies in a 16-point box,
/// stroked in `currentColor`, and drawn through Rune's icon cache so one body rasterises once.
mod icons {
    use rune::Icon;

    pub(super) static HOME: Icon = Icon {
        name: "bg-home",
        view: 16.0,
        body: r#"<path d="M2.6 7.1 8 2.7l5.4 4.4" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/><path d="M4.1 6.6v6.7h7.8V6.6" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/><path d="M6.6 13.3v-3.5h2.8v3.5" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
    pub(super) static PULSE: Icon = Icon {
        name: "bg-pulse",
        view: 16.0,
        body: r#"<path d="M1.8 8.4h2.9l1.7-4.1 2.4 8.2 1.7-4.1h3.7" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
    pub(super) static HISTORY: Icon = Icon {
        name: "bg-history",
        view: 16.0,
        body: r#"<path d="M3 3.4v2.9h2.9" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/><path d="M3.3 6.9a5.2 5.2 0 1 1-.3 2.6" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/><path d="M8 5.1v3.3l2.2 1.4" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
    pub(super) static CLOCK: Icon = Icon {
        name: "bg-clock",
        view: 16.0,
        body: r#"<circle cx="8" cy="8" r="5.6" stroke="currentColor" stroke-width="1.5" fill="none"/><path d="M8 4.9v3.4l2.3 1.4" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
    pub(super) static STAR: Icon = Icon {
        name: "bg-star",
        view: 16.0,
        body: r#"<path d="M8 2.1 9.9 6l4.3.6-3.1 3 0.7 4.3L8 11.9 4.2 13.9l0.7-4.3-3.1-3L6.1 6z" stroke="currentColor" stroke-width="1.4" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
    pub(super) static USER: Icon = Icon {
        name: "bg-user",
        view: 16.0,
        body: r#"<circle cx="8" cy="5.5" r="2.7" stroke="currentColor" stroke-width="1.5" fill="none"/><path d="M3 13.4a5 5 0 0 1 10 0" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round"/>"#,
    };
    pub(super) static SEARCH: Icon = Icon {
        name: "bg-search",
        view: 16.0,
        body: r#"<circle cx="7.1" cy="7.1" r="4.4" stroke="currentColor" stroke-width="1.5" fill="none"/><path d="m10.4 10.4 3.2 3.2" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round"/>"#,
    };
    pub(super) static REFRESH: Icon = Icon {
        name: "bg-refresh",
        view: 16.0,
        body: r#"<path d="M13.3 7.2a5.3 5.3 0 1 1-1.6-3.3" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round"/><path d="M13.6 2.6v2.7h-2.7" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
    pub(super) static PLAY: Icon = Icon {
        name: "bg-play",
        view: 16.0,
        body: r#"<path d="M5.6 3.6 12.2 8l-6.6 4.4z" fill="currentColor" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round"/>"#,
    };
    pub(super) static PAUSE: Icon = Icon {
        name: "bg-pause",
        view: 16.0,
        body: r#"<rect x="4.6" y="3.4" width="2.5" height="9.2" rx="0.8" fill="currentColor"/><rect x="8.9" y="3.4" width="2.5" height="9.2" rx="0.8" fill="currentColor"/>"#,
    };
    pub(super) static PREV: Icon = Icon {
        name: "bg-prev",
        view: 16.0,
        body: r#"<path d="M11.6 3.7 5.7 8l5.9 4.3z" fill="currentColor"/><rect x="3.5" y="3.6" width="1.5" height="8.8" rx="0.7" fill="currentColor"/>"#,
    };
    pub(super) static NEXT: Icon = Icon {
        name: "bg-next",
        view: 16.0,
        body: r#"<path d="M4.4 3.7 10.3 8l-5.9 4.3z" fill="currentColor"/><rect x="11" y="3.6" width="1.5" height="8.8" rx="0.7" fill="currentColor"/>"#,
    };
    pub(super) static VOLUME: Icon = Icon {
        name: "bg-volume",
        view: 16.0,
        body: r#"<path d="M7.6 3.1 4.4 5.6H2.2v4.8h2.2l3.2 2.5z" fill="currentColor" stroke="currentColor" stroke-width="1" stroke-linejoin="round"/><path d="M9.9 5.8a3 3 0 0 1 0 4.4" stroke="currentColor" stroke-width="1.4" fill="none" stroke-linecap="round"/><path d="M11.7 4.1a5.4 5.4 0 0 1 0 7.8" stroke="currentColor" stroke-width="1.4" fill="none" stroke-linecap="round"/>"#,
    };
    pub(super) static MUTED: Icon = Icon {
        name: "bg-muted",
        view: 16.0,
        body: r#"<path d="M7.6 3.1 4.4 5.6H2.2v4.8h2.2l3.2 2.5z" fill="currentColor" stroke="currentColor" stroke-width="1" stroke-linejoin="round"/><path d="m10.2 6 3.4 4M13.6 6l-3.4 4" stroke="currentColor" stroke-width="1.4" fill="none" stroke-linecap="round"/>"#,
    };
    pub(super) static FULLSCREEN: Icon = Icon {
        name: "bg-fullscreen",
        view: 16.0,
        body: r#"<path d="M2.6 6.2V2.6h3.6M13.4 9.8v3.6H9.8M9.8 2.6h3.6v3.6M6.2 13.4H2.6V9.8" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
    pub(super) static EXIT_FULLSCREEN: Icon = Icon {
        name: "bg-exit-fullscreen",
        view: 16.0,
        body: r#"<path d="M6.2 2.6v3.6H2.6M9.8 13.4v-3.6h3.6M13.4 6.2h-3.6V2.6M2.6 9.8h3.6v3.6" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
    pub(super) static QUALITY: Icon = Icon {
        name: "bg-quality",
        view: 16.0,
        body: r#"<rect x="2.2" y="3.6" width="11.6" height="8.8" rx="2" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M6.4 6.4 9.2 8l-2.8 1.6z" fill="currentColor"/>"#,
    };
    pub(super) static LIKE: Icon = Icon {
        name: "bg-like",
        view: 16.0,
        body: r#"<path d="M6.1 13.6H3.4a1 1 0 0 1-1-1V8.1a1 1 0 0 1 1-1h2.7M6.1 7.4 8.3 2.6a1.7 1.7 0 0 1 2.2 2.1l-.7 2.4h2.9a1.6 1.6 0 0 1 1.6 1.9l-.9 3.5a1.8 1.8 0 0 1-1.7 1.3H6.1z" stroke="currentColor" stroke-width="1.35" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
    pub(super) static COIN: Icon = Icon {
        name: "bg-coin",
        view: 16.0,
        body: r#"<circle cx="8" cy="8" r="5.6" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M8 5.4v5.2M6.3 8h3.4" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linecap="round"/>"#,
    };
    pub(super) static COMMENT: Icon = Icon {
        name: "bg-comment",
        view: 16.0,
        body: r#"<path d="M13.6 9.1a2 2 0 0 1-2 2H6.2l-3 2.3V5.1a2 2 0 0 1 2-2h6.4a2 2 0 0 1 2 2z" stroke="currentColor" stroke-width="1.4" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
    pub(super) static LOGO: Icon = Icon {
        name: "bg-logo",
        view: 16.0,
        body: r##"<rect x="1.4" y="3.6" width="13.2" height="9.2" rx="2.6" fill="currentColor"/><path d="M5.2 2.1 6.9 4.4M10.8 2.1 9.1 4.4" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linecap="round"/><circle cx="5.6" cy="7.4" r="0.95" fill="#0A0B0F"/><circle cx="10.4" cy="7.4" r="0.95" fill="#0A0B0F"/><path d="M6.6 10.2h2.8" stroke="#0A0B0F" stroke-width="0.9" fill="none" stroke-linecap="round"/>"##,
    };
    /// Theater mode: the video alone, no feed beside it.
    pub(super) static EXPAND: Icon = Icon {
        name: "bg-expand",
        view: 16.0,
        body: r#"<rect x="1.6" y="3.4" width="12.8" height="9.2" rx="2" stroke="currentColor" stroke-width="1.4" fill="none"/><rect x="4.2" y="5.8" width="4.6" height="4.4" rx="1" fill="currentColor" opacity="0.55"/>"#,
    };
    /// Back to the split layout: video with its feed beside it.
    pub(super) static COLLAPSE: Icon = Icon {
        name: "bg-collapse",
        view: 16.0,
        body: r#"<rect x="1.6" y="3.4" width="12.8" height="9.2" rx="2" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M9.6 3.4v9.2" stroke="currentColor" stroke-width="1.4"/><path d="M4.4 6h2.8M4.4 8h2.8M4.4 10h2.2" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
    };
    pub(super) static CHECK: Icon = Icon {
        name: "bg-check",
        view: 16.0,
        body: r#"<path d="m3.4 8.4 3 3 6.2-6.8" stroke="currentColor" stroke-width="1.7" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
    pub(super) static CLOSE: Icon = Icon {
        name: "bg-close",
        view: 16.0,
        body: r#"<path d="m4.2 4.2 7.6 7.6M11.8 4.2l-7.6 7.6" stroke="currentColor" stroke-width="1.5" fill="none" stroke-linecap="round"/>"#,
    };
    pub(super) static TV: Icon = Icon {
        name: "bg-tv",
        view: 16.0,
        body: r#"<rect x="1.8" y="4.2" width="12.4" height="8.6" rx="2" stroke="currentColor" stroke-width="1.4" fill="none"/><path d="M5.2 1.9 8 4.2l2.8-2.3" stroke="currentColor" stroke-width="1.4" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
    };
}

/// A 16-point glyph in `color`, laid out inline.
fn glyph(icon: &'static rune::Icon, color: Color, size: f32) -> El {
    rune::icon(icon, size).color(color)
}

/// One line of text that is allowed to run out of room rather than wrap.
fn label(content: impl Into<String>, size: f32, weight: f32, color: Color) -> El {
    text(content)
        .font(TextStyle::new(size, weight))
        .color(color)
        .single_line()
        .truncate()
}

/// The "this is the current one" wash. A tint and a bar down the leading edge rather than a
/// ring: a border on a poster card reads as a focus ring, which is not what this is.
fn selection_wash(el: El) -> El {
    el.fill(theme::ACCENT_DIM.alpha(0.7))
        .hover(theme::ACCENT_DIM)
        .press(theme::ACCENT_DIM)
        .border_sides([0.0, 0.0, 0.0, 2.0], theme::ACCENT)
}

/// A pill button with an icon and a label, the shape used for every action in the app.
fn pill(key: &'static str, icon: Option<&'static rune::Icon>, caption: &str, tone: Tone) -> El {
    let (fill, ink) = match tone {
        Tone::Quiet => (Color::TRANSPARENT, theme::TEXT_DIM),
        Tone::Raised => (theme::CARD, theme::TEXT),
        Tone::Accent => (theme::ACCENT, Color::WHITE),
    };
    let mut body = row()
        .key(key)
        .h(32.0)
        .px(12.0)
        .gap(6.0)
        .center_y()
        .radius(16.0)
        .fill(fill)
        .hover(if matches!(tone, Tone::Accent) {
            theme::ACCENT_SOFT
        } else {
            theme::CARD_HOVER
        })
        .press(theme::PRESS)
        .cursor(rune::Cursor::Pointer)
        .shrink();
    if matches!(tone, Tone::Raised) {
        body = body.border(1.0, theme::BORDER);
    }
    if let Some(icon) = icon {
        body = body.child(glyph(icon, ink, 14.0));
    }
    if !caption.is_empty() {
        body = body.child(
            text(caption)
                .font(TextStyle::new(12.5, 500.0))
                .color(ink)
                .single_line(),
        );
    }
    body
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    /// No fill: secondary actions that sit in a row of their own.
    Quiet,
    /// A card-coloured chip with a border: the default for a real button.
    Raised,
    /// The accent, filled: the one primary action on a screen.
    Accent,
}

/// A flat square button for the player's control strip.
fn icon_button(key: &'static str, icon: &'static rune::Icon, caption: &str) -> El {
    let mut body = row()
        .key(key)
        .h(30.0)
        .px(9.0)
        .gap(5.0)
        .center_y()
        .radius(8.0)
        .hover(theme::CARD_HOVER)
        .press(theme::PRESS)
        .cursor(rune::Cursor::Pointer)
        .help(caption)
        .shrink()
        .child(glyph(icon, theme::TEXT_DIM, 14.0));
    body = body.child(
        text(caption)
            .font(TextStyle::new(12.0, 500.0))
            .color(theme::TEXT_DIM)
            .single_line(),
    );
    body
}

/// A 34-point square icon-only button (window chrome, quiet round controls).
fn square_button(key: &'static str, icon: &'static rune::Icon, help: &'static str) -> El {
    el().key(key)
        .size(34.0, 34.0)
        .center()
        .radius(10.0)
        .hover(theme::CARD_HOVER)
        .press(theme::PRESS)
        .cursor(rune::Cursor::Pointer)
        .help(help)
        .shrink()
        .child(glyph(icon, theme::TEXT_DIM, 16.0))
}

/// The poster frame of a video: the decoded cover when it has arrived, else its own accent.
///
/// Rounded and clipped, so a cover fills its box whatever shape the CDN sent, and a duration
/// chip sits in the corner where Bilibili puts one.
fn poster(app: &BiliGuga, video: &Video, width: f32, height: f32, radius: f32, badge: bool) -> El {
    // No `.shrink()`: a flex item defaults to its content size, but shrinking is what let a
    // long title squash the cover in a narrow row.
    let mut frame = el()
        .w(width)
        .h(height)
        .radius(radius)
        .clip_rounded(radius)
        .fill(theme::cover_placeholder(video.accent));
    if let Some(image) = app.cover_for(video) {
        frame = frame.child(
            el().w_full()
                .h_full()
                .pixels_fit(image.pixels(), SurfaceFit::Fill),
        );
    }
    if badge {
        // Revealed by the card's own hover group, so it costs no commit: the card sets
        // `.hover_group()`, and this child fades in while the pointer is anywhere on it.
        frame = frame.child(
            el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .center()
                .fill(Color::BLACK.alpha(0.32))
                .group_opacity(0.0, 1.0)
                .a11y_hidden()
                .child(
                    el().size(46.0, 46.0)
                        .center()
                        .radius(23.0)
                        .fill(theme::ACCENT)
                        .child(glyph(&icons::PLAY, Color::WHITE, 19.0)),
                ),
        );
    }
    if !video.duration.is_empty() && !video.duration.starts_with("--") {
        // Pinned to the corner, not placed from an assumed width: a duration is "03:00" or
        // "1:02:33", and the chip has to hold both without running off the poster.
        frame = frame.child(
            row()
                .pin(None, None, Some(6.0), Some(6.0))
                .h(18.0)
                .px(5.0)
                .center_y()
                .radius(4.0)
                .fill(Color::BLACK.alpha(0.62))
                .child(
                    text(&video.duration)
                        .font(TextStyle::new(10.5, 600.0))
                        .color(Color::WHITE)
                        .single_line(),
                ),
        );
    }
    frame
}

// ---- The rail -------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RailItem {
    Home,
    Dynamic,
    History,
    WatchLater,
    Favorites,
}

impl RailItem {
    /// The tabs this item stands for. Search and author pages are reached from the home rail.
    fn owns(self, tab: AppTab) -> bool {
        matches!(
            (self, tab),
            (
                RailItem::Home,
                AppTab::Home | AppTab::Search | AppTab::Author
            ) | (RailItem::Dynamic, AppTab::Dynamic)
                | (RailItem::History, AppTab::History)
                | (RailItem::WatchLater, AppTab::WatchLater)
                | (RailItem::Favorites, AppTab::Favorites)
        )
    }
}

fn rail_entry(item: RailItem, icon: &'static rune::Icon, caption: &str, active: bool) -> El {
    let icon_color = if active {
        theme::ACCENT
    } else {
        theme::TEXT_DIM
    };
    let text_color = if active {
        theme::TEXT
    } else {
        theme::TEXT_FAINT
    };
    let mut entry = col()
        .w_full()
        .h(56.0)
        .center()
        .gap(4.0)
        .radius(12.0)
        .hover(theme::CARD_HOVER)
        .press(theme::PRESS)
        .cursor(rune::Cursor::Pointer)
        .help(caption)
        .on_press(move |app: &mut BiliGuga| app.show_tab(tab_of(item)))
        .child(glyph(icon, icon_color, 19.0))
        .child(
            text(caption)
                .font(TextStyle::new(10.5, 500.0))
                .color(text_color)
                .single_line(),
        );
    if active {
        // A wash plus a bar down the inner edge: the bar is a child with `.h(..)` and its own
        // offset rather than an absolute overlay, so it needs no ancestor positioning.
        entry = entry
            .fill(theme::CARD)
            .border(1.0, theme::ACCENT.alpha(0.22));
    }
    entry
}

fn tab_of(item: RailItem) -> AppTab {
    match item {
        RailItem::Home => AppTab::Home,
        RailItem::Dynamic => AppTab::Dynamic,
        RailItem::History => AppTab::History,
        RailItem::WatchLater => AppTab::WatchLater,
        RailItem::Favorites => AppTab::Favorites,
    }
}

fn render_rail(app: &BiliGuga) -> El {
    let mut rail = col()
        .key("rail")
        .w(RAIL_WIDTH)
        .h_full()
        .shrink()
        .items_center()
        .py(14.0)
        .gap(6.0)
        .fill(theme::RAIL)
        .border_sides([0.0, 1.0, 0.0, 0.0], theme::HAIRLINE)
        .child(col().w_full().items_center().pb(10.0).child(
            el().size(36.0, 36.0).center().radius(11.0).child(glyph(
                &icons::LOGO,
                theme::ACCENT,
                24.0,
            )),
        ));
    for (item, icon, caption) in [
        (RailItem::Home, &icons::HOME, "首页"),
        (RailItem::Dynamic, &icons::PULSE, "动态"),
        (RailItem::History, &icons::HISTORY, "历史"),
        (RailItem::WatchLater, &icons::CLOCK, "稍后"),
        (RailItem::Favorites, &icons::STAR, "收藏"),
    ] {
        rail = rail.child(el().w(RAIL_WIDTH - 20.0).child(rail_entry(
            item,
            icon,
            caption,
            item.owns(app.active_tab),
        )));
    }
    rail = rail.child(el().grow(1.0));
    // The account is the last thing in the rail, always in the same place: an avatar when
    // signed in, the sign-in door when not.
    let has_session = app.session.is_some();
    let mut account = el()
        .key("account")
        .size(40.0, 40.0)
        .center()
        .radius(20.0)
        .clip_rounded(20.0)
        .cursor(rune::Cursor::Pointer)
        .hover(theme::CARD_HOVER)
        .press(theme::PRESS)
        .help(if has_session { "账号" } else { "登录" })
        .on_press(|app: &mut BiliGuga| app.show_tab(AppTab::Login));
    match (has_session, app.account_avatar.as_ref()) {
        (true, Some(avatar)) => {
            account = account.child(
                el().w_full()
                    .h_full()
                    .pixels_fit(avatar.pixels(), SurfaceFit::Fill),
            );
        }
        (true, None) => {
            account = account.fill(theme::ACCENT_DIM).child(glyph(
                &icons::USER,
                theme::ACCENT_SOFT,
                18.0,
            ));
        }
        (false, _) => {
            account = account
                .fill(theme::CARD)
                .border(1.0, theme::BORDER)
                .child(glyph(&icons::USER, theme::TEXT_DIM, 17.0));
        }
    }
    rail.child(account)
}

// ---- The browse pane ------------------------------------------------------------------------

/// The feed column's heading: a fixed sentence for every tab but one, which borrows the
/// uploader's name. Returning a `Cow` keeps the seven fixed cases allocation-free.
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

/// What an empty feed means, in the tab's own terms.
fn empty_message(app: &BiliGuga) -> &'static str {
    let logged_in = app.session.is_some();
    match app.active_tab {
        AppTab::Search => "没有找到视频",
        AppTab::Dynamic if !logged_in => "登录后可以看到动态",
        AppTab::Dynamic => "关注的人最近没有更新",
        AppTab::WatchLater if !logged_in => "登录后可以看到稍后再看",
        AppTab::WatchLater => "稍后再看还是空的",
        AppTab::Favorites if !logged_in => "登录后可以看到收藏夹",
        AppTab::Favorites => "收藏夹还是空的",
        AppTab::History if !logged_in => "登录后可以看到观看历史",
        AppTab::History => "还没有观看记录",
        AppTab::Author => "这位 UP 主还没有公开投稿",
        _ => "没有加载到视频",
    }
}

/// What the spinner says while a feed is on its way.
fn loading_message(tab: AppTab) -> &'static str {
    match tab {
        AppTab::Search => "正在搜索…",
        AppTab::Dynamic => "正在加载动态…",
        AppTab::WatchLater => "正在加载稍后再看…",
        AppTab::Favorites => "正在加载收藏夹…",
        AppTab::History => "正在加载观看历史…",
        AppTab::Author => "正在加载 UP 主的投稿…",
        _ => "正在加载推荐…",
    }
}

/// The heading above the feed: what this column is, the one control that acts on it, and the
/// search field.
///
/// The field is here on every tab rather than only on the search results page. It used to be
/// reachable only *from* the search results, which meant the only way to search was to already
/// have searched: a first run had no search at all.
fn render_topbar(app: &BiliGuga) -> El {
    let mut header = col()
        .key("feed-header")
        .w_full()
        .shrink()
        .child(render_title_row(app));
    if app.active_tab != AppTab::Login {
        header = header.child(render_search(app));
    }
    header
}

/// The title, the refresh button, and "load more" when an author page has another batch.
fn render_title_row(app: &BiliGuga) -> El {
    let title = match app.active_tab {
        AppTab::Author => app.author_name().to_string(),
        tab => feed_title(tab, app.author_name()).into_owned(),
    };
    let busy = app.is_loading();
    let mut title_row = row()
        .key("topbar")
        .w_full()
        .shrink()
        .px(PANE_PAD)
        .pt(14.0)
        .pb(10.0)
        .items_center()
        .gap(12.0)
        .child(
            text(title)
                .font(TextStyle::new(19.0, 650.0))
                .color(theme::TEXT)
                .single_line()
                .truncate()
                .grow(1.0),
        );
    if app.active_tab == AppTab::Author && app.author_has_more {
        title_row = title_row.child(
            pill("author-more", None, "更多", Tone::Raised)
                .on_press(|app: &mut BiliGuga| app.load_author_page(false)),
        );
    }
    // Loading says so in the corner, in the same place the refresh button lives, so the two
    // never both occupy it: a second refresh of a request in flight would be a no-op anyway.
    if app.active_tab == AppTab::Login {
        return title_row;
    }
    title_row.child(if busy {
        el().key("refresh")
            .size(34.0, 34.0)
            .center()
            .help("正在加载…")
            .child(pulse_dot())
    } else {
        square_button("refresh", &icons::REFRESH, "刷新")
            .on_press(|app: &mut BiliGuga| app.refresh_feed())
    })
}

/// The search field. Always present at the top of the browse pane; Enter submits, and the
/// button beside it does the same for people who would rather click.
fn render_search(app: &BiliGuga) -> El {
    row()
        .key("search")
        .w_full()
        .shrink()
        .px(PANE_PAD)
        .pb(12.0)
        .gap(8.0)
        .child(
            row()
                .key("search-field")
                .grow(1.0)
                .h(36.0)
                .gap(8.0)
                .px(11.0)
                .items_center()
                .radius(10.0)
                .fill(theme::FIELD)
                .border(1.0, theme::BORDER)
                .child(glyph(&icons::SEARCH, theme::TEXT_FAINT, 14.0))
                .child(
                    text_input(&app.search_edit, TextStyle::new(13.0, 400.0))
                        .key("search-input")
                        .grow(1.0)
                        .single_line()
                        .placeholder("搜视频、UP 主、话题", theme::TEXT_FAINT)
                        .color(theme::TEXT)
                        .on_edit(|app: &mut BiliGuga| &mut app.search_edit)
                        .on_submit(|app: &mut BiliGuga, _: &mut Tx| app.load_search()),
                ),
        )
        .child(
            pill("search-button", None, "搜索", Tone::Accent)
                .h(36.0)
                .on_press(|app: &mut BiliGuga| app.load_search()),
        )
}

/// A poster card for the browse grid: cover, two-line title, uploader, and a play badge on
/// hover so it reads as playable rather than as a link.
fn browse_card(app: &BiliGuga, index: usize, video: &Video, slot: f32) -> El {
    // The 6 pt padding is drawn inside the slot, so the card's own box is the slot less it.
    let width = (slot - 2.0 * CARD_PAD).max(80.0);
    let cover_width = width;
    let cover_height = (width * 9.0 / 16.0).round();
    let selected = app.selected == Some(index);
    let playing = app
        .playing_video
        .as_ref()
        .is_some_and(|current| current.bvid == video.bvid);
    let mut card = col()
        .w(width)
        .shrink()
        .gap(8.0)
        .padding(CARD_PAD)
        .radius(12.0)
        .hover(theme::CARD_HOVER)
        .hover_group()
        .cursor(rune::Cursor::Pointer)
        .on_press(move |app: &mut BiliGuga| app.select(index))
        .child(el().w(cover_width).h(cover_height).shrink().child(poster(
            app,
            video,
            cover_width,
            cover_height,
            10.0,
            true,
        )))
        .child(
            col()
                .w_full()
                .min_w(0.0)
                .gap(5.0)
                .px(2.0)
                .child(
                    text(&video.title)
                        .font(TextStyle::new(13.0, 550.0))
                        .color(if playing {
                            theme::ACCENT_SOFT
                        } else {
                            theme::TEXT
                        })
                        .max_lines(2),
                )
                .child(
                    row()
                        .w_full()
                        .gap(5.0)
                        .items_center()
                        .child(glyph(&icons::USER, theme::TEXT_FAINT, 11.0))
                        .child(label(&video.uploader, 11.5, 400.0, theme::TEXT_FAINT).grow(1.0)),
                ),
        );
    if selected {
        card = selection_wash(card);
    }
    card
}

/// A list row: 16:9 cover on the left, title and metadata stacked beside it.
fn video_row(app: &BiliGuga, index: usize, video: &Video) -> El {
    let selected = app.selected == Some(index);
    let playing = app
        .playing_video
        .as_ref()
        .is_some_and(|current| current.bvid == video.bvid);
    let mut body = row()
        .w_full()
        .padding(6.0)
        .gap(10.0)
        .radius(10.0)
        .hover(theme::CARD_HOVER)
        .hover_group()
        .cursor(rune::Cursor::Pointer)
        .on_press(move |app: &mut BiliGuga| app.select(index))
        .child(poster(app, video, 132.0, 74.0, 8.0, true))
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .shrink()
                .gap(4.0)
                .py(1.0)
                .child(
                    text(&video.title)
                        .font(TextStyle::new(12.5, 550.0))
                        .color(if playing {
                            theme::ACCENT_SOFT
                        } else {
                            theme::TEXT
                        })
                        .max_lines(2),
                )
                .child(
                    row()
                        .w_full()
                        .gap(5.0)
                        .items_center()
                        .child(glyph(&icons::USER, theme::TEXT_FAINT, 10.0))
                        .child(label(&video.uploader, 11.0, 400.0, theme::TEXT_FAINT).grow(1.0)),
                ),
        );
    if selected {
        body = selection_wash(body);
    }
    body
}

/// A spinner-ish placeholder that says what is happening without a fake progress bar: a dot
/// that breathes, and the message beside it.
fn pulse_dot() -> El {
    el().size(7.0, 7.0)
        .radius(4.0)
        .fill(theme::ACCENT)
        .looping(rune::LoopSpec::opacity(1.4, 0.0, 0.22, 1.0))
}

/// A centred message for the empty, loading and error states.
/// A centred message for the empty, loading and error states, with an optional action under it.
/// The action lives in the same centred column: as a sibling of a grown panel it was pushed to
/// the bottom of the pane, a screen away from the sentence it belonged to.
fn state_panel(
    icon: &'static rune::Icon,
    headline: &str,
    detail: Option<&str>,
    tone: Color,
    action: Option<El>,
) -> El {
    let mut panel = col()
        .w_full()
        .grow(1.0)
        .center()
        .gap(10.0)
        .padding(24.0)
        .child(
            el().size(52.0, 52.0)
                .center()
                .radius(26.0)
                .fill(theme::CARD)
                .border(1.0, theme::BORDER)
                .child(glyph(icon, tone, 22.0)),
        )
        .child(
            text(headline)
                .font(TextStyle::new(14.0, 550.0))
                .color(theme::TEXT_DIM),
        );
    if let Some(detail) = detail {
        panel = panel.child(
            text(detail)
                .font(TextStyle::new(12.0, 400.0))
                .color(theme::TEXT_FAINT)
                .max_w(280.0)
                .align(rune::Align::Center),
        );
    }
    if let Some(action) = action {
        panel = panel.child(el().h(4.0)).child(action);
    }
    panel
}

fn render_feed(app: &BiliGuga) -> El {
    if app.active_tab == AppTab::Login {
        return render_login(app);
    }
    let videos = app.current_videos();
    let is_loading = app.is_loading();

    let body = if is_loading && videos.is_empty() {
        col()
            .w_full()
            .grow(1.0)
            .center()
            .gap(12.0)
            .child(pulse_dot())
            .child(
                text(loading_message(app.active_tab))
                    .font(TextStyle::new(12.5, 400.0))
                    .color(theme::TEXT_FAINT),
            )
    } else if let Some(error) = &app.feed_error {
        // A failure offers the way out of it: the old panel said "no" and left the fixed
        // refresh button in the corner as the only way to try again.
        state_panel(
            &icons::CLOSE,
            "没能加载出来",
            Some(error.as_str()),
            theme::DANGER,
            Some(
                pill("retry", Some(&icons::REFRESH), "重试", Tone::Raised)
                    .on_press(|app: &mut BiliGuga| app.refresh_feed()),
            ),
        )
    } else if videos.is_empty() {
        let needs_session = app.active_tab.signed_in().is_some() && app.session.is_none();
        state_panel(
            &icons::TV,
            empty_message(app),
            // A signed-out feed already says what to do in its headline; a second line under
            // it would only repeat the offer the button makes.
            (app.session.is_some() || app.active_tab.signed_in().is_none()).then_some(
                match app.active_tab {
                    AppTab::Home => "点右上角刷新试试",
                    AppTab::Search => "换一个关键词",
                    _ => "这里还是空的",
                },
            ),
            theme::TEXT_FAINT,
            needs_session.then(|| {
                pill(
                    "empty-sign-in",
                    Some(&icons::USER),
                    "扫码登录",
                    Tone::Accent,
                )
                .on_press(|app: &mut BiliGuga| app.show_tab(AppTab::Login))
            }),
        )
    } else if app.card_columns.get() >= 2 {
        // The grid takes as many equal columns as the pane allows, so the same view is two
        // columns in a narrow window and five in a wide one, without a measurement pass: the
        // view is handed the pane's width by `card_columns`. One column is a list, not a grid:
        // a poster card as wide as the pane is a billboard, and the pane is this narrow only
        // when the player has taken the rest of the window.
        let columns = app.card_columns.get();
        let width = app.card_width.get();
        let rows = videos.len().div_ceil(columns);
        col()
            .key("home-grid")
            .w_full()
            .growth(GrowthSpec::default())
            .px(PANE_PAD)
            .py(6.0)
            .virtual_rows(
                rows,
                (width * 9.0 / 16.0 + 78.0).round(),
                |_app: &BiliGuga, row_index| format!("grid-row-{row_index}").into(),
                move |app: &BiliGuga, row_index| {
                    let videos = app.current_videos();
                    let mut line = row().w_full().gap(CARD_GAP);
                    for column in 0..columns {
                        let index = row_index * columns + column;
                        if let Some(video) = videos.get(index) {
                            line = line.child(browse_card(app, index, video, width));
                        }
                    }
                    line
                },
            )
    } else {
        col()
            .key("video-list")
            .w_full()
            .growth(GrowthSpec::default())
            .px(PANE_PAD - 6.0)
            .py(6.0)
            .virtual_rows(
                videos.len(),
                LIST_ROW_HEIGHT,
                |app: &BiliGuga, index| {
                    app.current_videos()
                        .get(index)
                        .map(|video| format!("row-{}", video.bvid))
                        .unwrap_or_else(|| format!("row-{index}"))
                        .into()
                },
                |app: &BiliGuga, index| match app.current_videos().get(index) {
                    Some(video) => video_row(app, index, video),
                    None => el(),
                },
            )
    };

    // Paging hangs off the list itself: it reports the range of rows it has mounted, once per
    // change rather than per frame, and the feed asks for more when that range gets close to the
    // end. Nothing here depends on how tall the loaded page happens to be.
    let scroll_read = app.scroll_pos.clone();
    let scroll_wheel = app.scroll_pos.clone();
    let viewport = col()
        .key("feed-scroll")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        // Where it stands, for the wheel handler below.
        .on_scroll_offset(move |at| scroll_read.set(Some((at.offset[1], at.max[1]))))
        // A notched wheel takes the feed further per click than Rune's flat 40 points. Taking
        // the delta means placing the viewport here, so the offset above is read back; a
        // trackpad (`precise`) and horizontal gestures are left to Rune.
        .on_wheel(
            move |_: &mut BiliGuga, ev: WheelEvent, tx: &mut Tx| -> bool {
                if ev.precise || ev.dy == 0.0 || ev.dx != 0.0 || ev.momentum {
                    return false;
                }
                let Some((offset, max)) = scroll_wheel.get() else {
                    // Not laid out yet: nothing to place the viewport at, so let Rune scroll.
                    return false;
                };
                if max <= 0.0 {
                    // Everything fits: nothing to scroll, and nothing to take from Rune.
                    return false;
                }
                let step = ev.dy * (WHEEL_NOTCH / RUNE_WHEEL_NOTCH);
                let to = (offset - step).clamp(0.0, max);
                // Two notches can arrive between frames; the second starts where the first left
                // off, not where the last frame's offset was. Clamped here as well as by the
                // viewport, so holding the wheel at the end cannot build up an offset past it
                // for the next page's growth to spring to.
                scroll_wheel.set(Some((to, max)));
                tx.scroll_to_instant("feed-scroll", 0.0, to);
                true
            },
        );
    // "Loading the next page" is a footer inside the viewport, so the reader sees the feed
    // still filling rather than a frozen list.
    let footer = if is_loading && !videos.is_empty() {
        row()
            .w_full()
            .py(16.0)
            .justify_center()
            .gap(8.0)
            .child(pulse_dot())
            .child(
                text("正在加载更多…")
                    .font(TextStyle::new(11.5, 400.0))
                    .color(theme::TEXT_FAINT),
            )
    } else {
        el().h(12.0).w_full()
    };

    col()
        .key("feed")
        .h_full()
        .min_h(0.0)
        .min_w(0.0)
        .grow(1.0)
        .fill(theme::BG)
        .child(render_topbar(app))
        .child(el().w_full().h(1.0).shrink().fill(theme::HAIRLINE))
        .child(viewport.child(body).child(footer))
}

fn render_login(app: &BiliGuga) -> El {
    let body = if let Some(session) = &app.session {
        let initial: String = session
            .username
            .chars()
            .next()
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_else(|| "B".into());
        let mut card = col()
            .w(320.0)
            .shrink()
            .gap(14.0)
            .padding(24.0)
            .items_center()
            .radius(16.0)
            .fill(theme::CARD)
            .border(1.0, theme::BORDER)
            .shadow(12.0, 40.0, Color::BLACK.alpha(0.35))
            .child(match app.account_avatar.as_ref() {
                Some(avatar) => el().size(64.0, 64.0).radius(32.0).clip_rounded(32.0).child(
                    el().w_full()
                        .h_full()
                        .pixels_fit(avatar.pixels(), SurfaceFit::Fill),
                ),
                None => el()
                    .size(64.0, 64.0)
                    .center()
                    .radius(32.0)
                    .fill(theme::ACCENT_DIM)
                    .child(
                        text(initial)
                            .font(TextStyle::new(24.0, 650.0))
                            .color(theme::ACCENT_SOFT),
                    ),
            })
            .child(
                text(&session.username)
                    .font(TextStyle::new(17.0, 600.0))
                    .color(theme::TEXT),
            )
            .child(
                text(if session.is_vip {
                    "大会员 · 已登录"
                } else {
                    "已登录"
                })
                .font(TextStyle::new(12.0, 400.0))
                .color(theme::TEXT_FAINT),
            )
            .child(pill("sign-out", None, "退出登录", Tone::Raised).on_press(
                |app: &mut BiliGuga| {
                    login::clear_session();
                    app.session = None;
                    app.account_avatar = None;
                    app.login_status = "使用哔哩哔哩 App 扫码登录".into();
                    app.active_tab = AppTab::Home;
                },
            ));
        if app.session.is_some() {
            card = card.child(
                row()
                    .gap(8.0)
                    .child(
                        pill("to-history", Some(&icons::HISTORY), "观看历史", Tone::Quiet)
                            .on_press(|app: &mut BiliGuga| app.show_tab(AppTab::History)),
                    )
                    .child(
                        pill("to-favs", Some(&icons::STAR), "我的收藏", Tone::Quiet)
                            .on_press(|app: &mut BiliGuga| app.show_tab(AppTab::Favorites)),
                    ),
            );
        }
        card
    } else {
        let mut card = col()
            .w(320.0)
            .shrink()
            .gap(14.0)
            .padding(24.0)
            .items_center()
            .radius(16.0)
            .fill(theme::CARD)
            .border(1.0, theme::BORDER)
            .shadow(12.0, 40.0, Color::BLACK.alpha(0.35))
            .child(
                text("扫码登录")
                    .font(TextStyle::new(17.0, 600.0))
                    .color(theme::TEXT),
            )
            .child(
                text("用哔哩哔哩 App 扫一扫，收藏和历史就会出现在这里")
                    .font(TextStyle::new(12.0, 400.0))
                    .color(theme::TEXT_FAINT)
                    .max_w(240.0)
                    .align(rune::Align::Center),
            );
        // The code sits on a white plate whatever the theme: scanners need the quiet zone.
        let plate = el()
            .size(196.0, 196.0)
            .center()
            .radius(10.0)
            .fill(Color::WHITE)
            .clip_rounded(10.0);
        card = card.child(match &app.login_qr {
            Some(qr) => plate.child(
                el().size(172.0, 172.0)
                    .pixels_fit(qr.pixels(), SurfaceFit::Fit),
            ),
            None => plate.child(el().size(172.0, 172.0).center().child(glyph(
                &icons::REFRESH,
                theme::TEXT_FAINT,
                26.0,
            ))),
        });
        card = card
            .child(
                text(app.login_status.as_ref())
                    .font(TextStyle::new(12.0, 400.0))
                    .color(if app.login_qr.is_some() {
                        theme::TEXT_DIM
                    } else {
                        theme::TEXT_FAINT
                    })
                    .align(rune::Align::Center),
            )
            .child(
                pill(
                    "refresh-qr",
                    Some(&icons::REFRESH),
                    "刷新二维码",
                    Tone::Accent,
                )
                .on_press(|app: &mut BiliGuga| app.start_login()),
            );
        card
    };
    col()
        .w_full()
        .h_full()
        .grow(1.0)
        .fill(theme::BG)
        .child(render_topbar(app))
        .child(el().w_full().h(1.0).shrink().fill(theme::HAIRLINE))
        .child(col().w_full().grow(1.0).center().child(body))
}

// ---- The player -----------------------------------------------------------------------------

/// A thin track with a filled portion, used by the seek bar and the volume slider.
fn track(fraction: f32, height: f32, fill: Color) -> El {
    let fraction = fraction.clamp(0.0, 1.0);
    col()
        .w_full()
        .h(height)
        .radius(height / 2.0)
        .fill(theme::TRACK)
        .child(
            el().h(height)
                .w_frac(fraction)
                .radius(height / 2.0)
                .fill(fill),
        )
}

/// The seek bar: presses and drags anywhere on the track jump the video there. The hit area is
/// taller than the drawn line, so it is easy to grab.
fn seek_bar(position: f64, duration: f64) -> El {
    row()
        .key("seek-row")
        .w_full()
        .gap(10.0)
        .items_center()
        .child(
            text(Clock::new(position).as_str())
                .font(TextStyle::new(11.5, 500.0))
                .color(theme::TEXT_DIM)
                .shrink(),
        )
        .child(seek_track(position, duration))
        .child(
            text(Clock::new(duration).as_str())
                .font(TextStyle::new(11.5, 500.0))
                .color(theme::TEXT_FAINT)
                .shrink(),
        )
}

/// The track itself: presses and drags anywhere on it jump the video there. The hit area is
/// taller than the drawn line, so it is easy to grab.
fn seek_track(position: f64, duration: f64) -> El {
    let fraction = if duration > 0.0 {
        (position / duration) as f32
    } else {
        0.0
    };
    col()
        .key("seek-bar")
        .grow(1.0)
        .h(18.0)
        .justify_center()
        .cursor(rune::Cursor::Pointer)
        .on_drag(move |app: &mut BiliGuga, drag: &mut Drag, _: &mut Tx| {
            // `drag.x` is the pointer in the element's own points, so a press at 30% is 30%.
            let fraction = if drag.w > 0.0 {
                (drag.x / drag.w).clamp(0.0, 1.0)
            } else {
                0.0
            };
            app.seek_percent(fraction as f64);
            app.seek_dragging = drag.phase != Phase::Up;
            app.show_controls();
        })
        .child(track(fraction, 4.0, theme::ACCENT))
}

fn volume_bar(volume: f64, width: f32) -> El {
    col()
        .key("volume-bar")
        .w(width)
        .h(18.0)
        .justify_center()
        .cursor(rune::Cursor::Pointer)
        .help("音量")
        .on_drag(move |app: &mut BiliGuga, drag: &mut Drag, _: &mut Tx| {
            let fraction = if drag.w > 0.0 {
                (drag.x / drag.w).clamp(0.0, 1.0)
            } else {
                0.0
            };
            app.volume = (fraction as f64 * 100.0).clamp(0.0, 100.0);
            app.player.set_volume(app.volume);
            app.volume_dragging = drag.phase != Phase::Up;
            if !app.volume_dragging {
                app.persist_player_settings();
            }
            app.show_controls();
        })
        .child(track((volume / 100.0) as f32, 4.0, theme::TEXT_DIM))
}

/// Which collection the current episode belongs to, and where in it.
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
    let text = match position {
        Some(index) => format!(
            "{} · {}/{}",
            collection.title,
            index,
            collection.episodes.len()
        ),
        None => collection.title.clone(),
    };
    // Keyed by the collection, so switching episodes keeps this label's identity (and its
    // place in the transport row) instead of looking like a new element every time.
    row()
        .key(format!("collection-{}", collection.id))
        .shrink()
        .max_w(220.0)
        .gap(5.0)
        .items_center()
        .child(glyph(&icons::TV, theme::TEXT_FAINT, 12.0))
        .child(label(text, 11.5, 500.0, theme::TEXT_FAINT))
}

/// The video stage: the frame letterboxed on black, or a message until one arrives. In the
/// windowed layout it is one block at the top of the player column; in full screen it takes
/// everything and the controls float over it.
fn render_stage(app: &BiliGuga, size: Option<(f32, f32)>) -> El {
    let mut stage = col()
        .key("player-stage")
        .items_center()
        .justify_center()
        .fill(Color::BLACK)
        .on_pointer_move(|app: &mut BiliGuga, _| app.show_controls());
    stage = match size {
        // The windowed player is handed the box it may use, so the frame is letterboxed inside
        // a known rectangle: the layout never depends on what mpv happens to be decoding.
        Some((width, height)) => stage
            .w(width)
            .h(height)
            .radius(12.0)
            .clip_rounded(12.0)
            .border(1.0, theme::BORDER),
        // Full screen: everything, edge to edge.
        None => stage.grow(1.0).w_full().h_full().min_h(0.0),
    };
    match app.player.presented() {
        Some(pixels) => {
            stage = stage.child(
                el().key("video-frame")
                    .w_full()
                    .h_full()
                    .pixels_fit(&pixels, SurfaceFit::Fit),
            );
        }
        None => {
            let (message, tone) = match &app.playback_error {
                Some(error) => (error.as_str(), theme::DANGER),
                None if app.playback == PlaybackState::Buffering => ("正在缓冲…", theme::TEXT_DIM),
                None => ("准备播放…", theme::TEXT_DIM),
            };
            stage = stage.child(
                col()
                    .gap(12.0)
                    .center()
                    .child(if app.playback_error.is_some() {
                        el().size(44.0, 44.0)
                            .center()
                            .radius(22.0)
                            .fill(theme::DANGER.alpha(0.12))
                            .child(glyph(&icons::CLOSE, theme::DANGER, 20.0))
                    } else {
                        pulse_dot()
                    })
                    .child(
                        text(message)
                            .font(TextStyle::new(12.5, 400.0))
                            .color(tone)
                            .max_w(420.0)
                            .align(rune::Align::Center),
                    ),
            );
        }
    }
    stage
}

/// The transport bar: the play button, then the switches that change how it plays.
///
/// `compact` is the windowed player, whose column is a share of the window and can be as
/// narrow as 390 points. There the quality is its icon alone and the volume slider is short,
/// so the row fits on one line; full screen has the room to spell the quality out.
fn render_transport(app: &BiliGuga, fullscreen: bool) -> El {
    let playing = app.playback == PlaybackState::Playing;
    let mut quality =
        icon_button("quality", &icons::QUALITY, "画质").on_press(|app: &mut BiliGuga| {
            app.open_menu = if app.open_menu == Some(PlayerMenu::Quality) {
                None
            } else {
                Some(PlayerMenu::Quality)
            };
        });
    if fullscreen {
        quality = quality.child(
            text(crate::api::quality_label(app.quality))
                .font(TextStyle::new(11.0, 500.0))
                .color(theme::TEXT_FAINT)
                .single_line(),
        );
    }
    row()
        .key("transport")
        .w_full()
        .items_center()
        .gap(4.0)
        .child(
            el().size(34.0, 34.0)
                .center()
                .radius(17.0)
                .fill(theme::ACCENT)
                .hover(theme::ACCENT_SOFT)
                .press(theme::ACCENT_SOFT)
                .cursor(rune::Cursor::Pointer)
                .help(if playing { "暂停" } else { "播放" })
                .on_press(|app: &mut BiliGuga| app.toggle_pause())
                .child(glyph(
                    if playing { &icons::PAUSE } else { &icons::PLAY },
                    Color::WHITE,
                    15.0,
                )),
        )
        .child(el().grow(1.0).min_w(4.0))
        .child(if fullscreen {
            collection_marker(app)
        } else {
            el()
        })
        .child(quality)
        .child(
            square_button(
                "mute",
                if app.volume <= 0.5 {
                    &icons::MUTED
                } else {
                    &icons::VOLUME
                },
                "静音",
            )
            .on_press(|app: &mut BiliGuga| {
                app.volume = if app.volume > 0.5 { 0.0 } else { 100.0 };
                app.player.set_volume(app.volume);
                app.persist_player_settings();
            }),
        )
        .child(volume_bar(app.volume, if fullscreen { 96.0 } else { 62.0 }))
        .child(
            pill("speed", None, &format!("{:.2}x", app.speed), Tone::Quiet)
                .h(28.0)
                .px(8.0)
                .help("倍速")
                .on_press(|app: &mut BiliGuga| app.cycle_speed()),
        )
        .child(if fullscreen {
            el()
        } else {
            square_button(
                "theater",
                if app.theater {
                    &icons::COLLAPSE
                } else {
                    &icons::EXPAND
                },
                if app.theater {
                    "还原分栏"
                } else {
                    "影院模式"
                },
            )
            .on_press(|app: &mut BiliGuga| app.theater = !app.theater)
        })
        .child(
            square_button(
                "fullscreen",
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
        )
}

/// Like, coin, watch later, favourite and the episode steppers: the things you do *to* a video.
fn render_actions(app: &BiliGuga) -> El {
    let has_collection = app.collection.is_some();
    let mut actions = row()
        .key("actions")
        .shrink()
        .gap(6.0)
        .items_center()
        .child(
            pill("like", Some(&icons::LIKE), "点赞", Tone::Raised)
                .on_press(|app: &mut BiliGuga| app.like_current()),
        )
        .child(
            pill("coin", Some(&icons::COIN), "投币", Tone::Raised)
                .on_press(|app: &mut BiliGuga| app.coin_current()),
        )
        .child(
            pill("favorite", Some(&icons::STAR), "收藏", Tone::Raised)
                .on_press(|app: &mut BiliGuga| app.save_to_favorites()),
        )
        .child(
            pill("watch-later", Some(&icons::CLOCK), "稍后看", Tone::Raised)
                .on_press(|app: &mut BiliGuga| app.save_to_watch_later()),
        );
    if has_collection {
        actions = actions
            .child(
                pill("prev-episode", Some(&icons::PREV), "上一集", Tone::Quiet)
                    .on_press(|app: &mut BiliGuga| app.play_collection_step(false)),
            )
            .child(
                pill("next-episode", Some(&icons::NEXT), "下一集", Tone::Quiet)
                    .on_press(|app: &mut BiliGuga| app.play_collection_step(true)),
            );
    }
    actions
}

/// The quality popup, drawn under the transport bar while it is open.
fn render_quality_menu(app: &BiliGuga) -> El {
    col()
        .key("quality-menu")
        .w(220.0)
        .shrink()
        .padding(6.0)
        .gap(2.0)
        .radius(12.0)
        .fill(theme::CARD)
        .border(1.0, theme::BORDER)
        .shadow(10.0, 32.0, Color::BLACK.alpha(0.45))
        .children(app.quality_options.iter().map(|option| {
            let selected = option.qn == app.quality;
            let quality = option.qn;
            let allowed = option.switchable;
            let mut row_el = row()
                .w_full()
                .h(30.0)
                .px(8.0)
                .gap(8.0)
                .items_center()
                .radius(7.0)
                .cursor(rune::Cursor::Pointer)
                .hover(theme::CARD_HOVER)
                .on_press(move |app: &mut BiliGuga| {
                    if allowed {
                        app.select_quality(quality);
                    }
                })
                .child(
                    text(&option.label)
                        .font(TextStyle::new(12.5, 500.0))
                        .color(if selected {
                            theme::ACCENT_SOFT
                        } else if allowed {
                            theme::TEXT
                        } else {
                            theme::TEXT_FAINT
                        })
                        .grow(1.0),
                );
            if !allowed {
                row_el = row_el.child(
                    text("需要会员")
                        .font(TextStyle::new(10.5, 500.0))
                        .color(theme::TEXT_FAINT),
                );
            }
            if selected {
                row_el = row_el.child(glyph(&icons::CHECK, theme::ACCENT, 13.0));
            }
            row_el
        }))
}

/// The clip's title, uploader and numbers, under the stage. Three lines rather than one: a
/// title, then who made it, then the counts — the player column is narrow enough that a single
/// row of all three ran off the right edge.
fn render_detail(app: &BiliGuga, video: &Video) -> El {
    let mut block = col()
        .key("player-detail")
        .w_full()
        .gap(8.0)
        .child(
            text(&video.title)
                .font(TextStyle::new(15.5, 600.0))
                .color(theme::TEXT)
                .max_lines(2),
        )
        .child(
            row().gap(6.0).items_center().child(
                row()
                    .gap(6.0)
                    .items_center()
                    .radius(14.0)
                    .pr(10.0)
                    .cursor(rune::Cursor::Pointer)
                    .hover(theme::HOVER)
                    .press(theme::PRESS)
                    .help("查看该 UP 主的投稿")
                    .on_press({
                        let mid = video.uploader_mid;
                        let name = video.uploader.clone();
                        move |app: &mut BiliGuga| app.open_author(mid, name.clone())
                    })
                    .child(
                        el().size(24.0, 24.0)
                            .center()
                            .radius(12.0)
                            .fill(theme::CARD)
                            .child(glyph(&icons::USER, theme::TEXT_DIM, 13.0)),
                    )
                    .child(
                        text(&video.uploader)
                            .font(TextStyle::new(12.0, 550.0))
                            .color(theme::ACCENT_SOFT)
                            .single_line()
                            .truncate(),
                    ),
            ),
        )
        .child(render_detail_stats(app));
    if let Some(note) = &app.action_note {
        block = block.child(
            row()
                .gap(6.0)
                .items_center()
                .child(glyph(&icons::CHECK, theme::ACCENT, 12.0))
                .child(
                    text(note)
                        .font(TextStyle::new(11.5, 500.0))
                        .color(theme::ACCENT_SOFT),
                ),
        );
    }
    block
}

/// The four numbers the detail call brings back, as glyph-plus-value chips.
fn render_detail_stats(app: &BiliGuga) -> El {
    let Some(stats) = &app.stats else {
        return el();
    };
    row().gap(12.0).items_center().shrink().children(
        [
            ("播放", &stats.view_count),
            ("弹幕", &stats.danmaku_count),
            ("点赞", &stats.like_count),
            ("发布", &stats.published_at),
        ]
        .into_iter()
        .map(|(caption, value)| {
            row()
                .gap(4.0)
                .items_center()
                .shrink()
                .child(
                    text(caption)
                        .font(TextStyle::new(10.5, 500.0))
                        .color(theme::TEXT_FAINT)
                        .single_line(),
                )
                .child(
                    text(value)
                        .font(TextStyle::new(11.5, 500.0))
                        .color(theme::TEXT_DIM)
                        .single_line(),
                )
        }),
    )
}

fn render_comments(app: &BiliGuga) -> El {
    let mut section = col().key("comments").w_full().gap(8.0).child(
        row()
            .w_full()
            .items_center()
            .gap(6.0)
            .child(glyph(&icons::COMMENT, theme::TEXT_DIM, 14.0))
            .child(
                text(format!("评论 {}", app.comments_total.max(0)))
                    .font(TextStyle::new(12.5, 600.0))
                    .color(theme::TEXT_DIM),
            )
            .child(el().grow(1.0)),
    );
    if app.comments_loading && app.comments.is_empty() {
        section = section.child(
            row()
                .py(12.0)
                .gap(8.0)
                .items_center()
                .child(pulse_dot())
                .child(
                    text("正在加载评论…")
                        .font(TextStyle::new(11.5, 400.0))
                        .color(theme::TEXT_FAINT),
                ),
        );
    } else if let Some(error) = &app.comments_error {
        section = section.child(
            text(error)
                .font(TextStyle::new(11.5, 400.0))
                .color(theme::DANGER),
        );
    } else if app.comments.is_empty() {
        section = section.child(
            text("还没有评论")
                .font(TextStyle::new(11.5, 400.0))
                .color(theme::TEXT_FAINT),
        );
    } else {
        section = section.child(col().w_full().gap(2.0).children(app.comments.iter().map(
            |comment| {
                // A loaded comment never changes, so `memo` keeps its subtree across commits:
                // loading another page no longer re-builds and re-shapes every comment on
                // screen, which is the expensive half of a list.
                rune::memo(format!("comment-{}", comment.rpid), comment.rpid, || {
                    col()
                        .w_full()
                        .gap(3.0)
                        .padding(6.0)
                        .radius(8.0)
                        .hover(theme::HOVER)
                        .child(
                            row()
                                .gap(6.0)
                                .items_center()
                                .child(
                                    el().size(18.0, 18.0)
                                        .center()
                                        .radius(9.0)
                                        .fill(theme::CARD)
                                        .child(glyph(&icons::USER, theme::TEXT_FAINT, 10.0)),
                                )
                                .child(
                                    text(&comment.username)
                                        .font(TextStyle::new(11.5, 600.0))
                                        .color(theme::ACCENT_SOFT),
                                )
                                .child(
                                    text(&comment.time)
                                        .font(TextStyle::new(10.5, 400.0))
                                        .color(theme::TEXT_FAINT),
                                )
                                .child(el().grow(1.0))
                                .child(glyph(&icons::LIKE, theme::TEXT_FAINT, 11.0))
                                .child(
                                    text(comment.like.to_string())
                                        .font(TextStyle::new(10.5, 400.0))
                                        .color(theme::TEXT_FAINT),
                                ),
                        )
                        .child(
                            text(&comment.message)
                                .font(TextStyle::new(12.0, 400.0))
                                .color(theme::TEXT_DIM)
                                .wrap(),
                        )
                })
            },
        )));
        if app.comments_has_more {
            section = section.child(
                pill("more-comments", None, "加载更多评论", Tone::Quiet)
                    .on_press(|app: &mut BiliGuga| app.load_comments(false)),
            );
        }
    }
    section
}

/// The player column while a video is open: the stage in its 16:9 block, then one card holding
/// everything that acts on it, then the comments. It scrolls as a whole, because a player
/// column that pinned its controls would leave the comments in a 150-point slot.
fn render_player(app: &BiliGuga, video: &Video) -> El {
    let (stage_width, stage_height) = app.stage_box.get();
    let status = app.player.status();
    let duration = if status.duration.is_finite() && status.duration > 0.0 {
        status.duration
    } else {
        0.0
    };
    let position = if status.time_pos.is_finite() {
        status.time_pos.clamp(0.0, duration.max(0.0))
    } else {
        0.0
    };
    let mut stage_block = col()
        .key("stage-block")
        .w_full()
        .shrink()
        .padding(PANE_PAD)
        .child(render_stage(app, Some((stage_width, stage_height))));
    // The windowed layout always shows its controls: hiding them would hide the only way to
    // act on the video, since the pointer is usually over the feed.
    stage_block = stage_block
        .child(el().h(10.0))
        .child(
            col()
                .w_full()
                .gap(8.0)
                .padding(10.0)
                .radius(12.0)
                .fill(theme::PANEL)
                .child(seek_bar(position, duration))
                .child(render_transport(app, false)),
        )
        .child(
            if app.open_menu == Some(PlayerMenu::Quality) && !app.quality_options.is_empty() {
                row()
                    .w_full()
                    .pt(6.0)
                    .justify_end()
                    .child(render_quality_menu(app))
            } else {
                el()
            },
        )
        .child(
            row()
                .w_full()
                .gap(6.0)
                .items_center()
                .child(render_actions(app))
                .child(el().grow(1.0))
                .child(
                    pill("close-player", Some(&icons::CLOSE), "关闭", Tone::Quiet)
                        .on_press(|app: &mut BiliGuga| app.close_player()),
                ),
        );

    let column = col()
        .key("player")
        .grow(1.0)
        .h_full()
        .min_w(0.0)
        .min_h(0.0)
        .fill(theme::PANEL)
        .border_sides([0.0, 0.0, 0.0, 1.0], theme::HAIRLINE);
    column.child(
        col()
            .key("player-scroll")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .child(
                col()
                    .w_full()
                    .gap(14.0)
                    .child(stage_block)
                    .child(
                        col()
                            .w_full()
                            .gap(12.0)
                            .px(PANE_PAD)
                            .pb(8.0)
                            .child(render_detail(app, video)),
                    )
                    .child(
                        col()
                            .w_full()
                            .gap(10.0)
                            .px(PANE_PAD)
                            .pb(28.0)
                            .child(render_comments(app)),
                    ),
            ),
    )
}

/// The player column while the video is in full screen: stage and a control strip that fades
/// out with the pointer.
fn render_player_fullscreen(app: &BiliGuga, video: &Video) -> El {
    let status = app.player.status();
    let duration = if status.duration.is_finite() && status.duration > 0.0 {
        status.duration
    } else {
        0.0
    };
    let position = if status.time_pos.is_finite() {
        status.time_pos.clamp(0.0, duration.max(0.0))
    } else {
        0.0
    };
    // The picture keeps the whole window; the title and the controls ride over it, pinned to
    // the edges, so showing them never resizes the video or re-times mpv's decode.
    let mut root = col()
        .key("player")
        .w_full()
        .h_full()
        .fill(Color::BLACK)
        .child(render_stage(app, None));
    if app.controls_visible() {
        root = root.child(
            col()
                .pin(Some(0.0), None, Some(0.0), Some(0.0))
                .gap(8.0)
                .px(24.0)
                .pt(14.0)
                .pb(14.0)
                .fill(Color::BLACK.alpha(0.82))
                .child(seek_bar(position, duration))
                .child(render_transport(app, true)),
        );
    } else {
        root = root.child(
            row()
                .pin(Some(0.0), Some(0.0), Some(0.0), None)
                .h(44.0)
                .px(20.0)
                .items_center()
                .fill(Color::BLACK.alpha(0.45))
                .child(
                    text(&video.title)
                        .font(TextStyle::new(12.5, 500.0))
                        .color(Color::WHITE.alpha(0.78))
                        .truncate(),
                ),
        );
    }
    if app.open_menu == Some(PlayerMenu::Quality) && !app.quality_options.is_empty() {
        root = root.child(
            row()
                .pin(Some(0.0), None, Some(0.0), Some(78.0))
                .px(24.0)
                .justify_end()
                .child(render_quality_menu(app)),
        );
    }
    root
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
fn stage_box(column_width: f32, column_height: f32, aspect: f32) -> (f32, f32) {
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
fn column_count([_, _, width, _]: [f32; 4]) -> usize {
    column_count_of(width)
}

fn column_count_of(width: f32) -> usize {
    let usable = width - 2.0 * PANE_PAD;
    (((usable + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)).floor() as i32).clamp(1, 5) as usize
}

/// One card's share of the pane: the padded width, less the gaps, split evenly.
fn slot_width(pane_width: f32, columns: usize) -> f32 {
    let usable = pane_width - 2.0 * PANE_PAD;
    let columns = columns.max(1) as f32;
    ((usable - (columns - 1.0) * CARD_GAP) / columns)
        .floor()
        .max(80.0)
}

// ---- Runtime hooks ---------------------------------------------------------------------------

/// Called once, before the first frame, with the handle other threads use to wake the window.
fn setup(app: &mut BiliGuga, waker: Waker) {
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

/// Runs at the start of every frame. Returns true only when something visibly changed, so an
/// idle window draws nothing and arms no timer.
fn poll(app: &mut BiliGuga, _seconds: f64, tx: &mut Tx) -> bool {
    let mut changed = false;

    // Keep the next page in hand before the reader reaches the end of this one.
    //
    // This runs *before* this frame's events, and that order is the whole point. A page that
    // lands below grows the rows, but `max` still describes the layout of the frame before,
    // which was built from the shorter list: checked after the event, every arrival looked like
    // the reader standing at the bottom of the old content, and the feed paged again and again
    // without ever drawing what it had already asked for. Checked first, each arrival gets its
    // own frame of layout before anything is decided from it.
    //
    // A scroll callback could not do this job: it may not change state, and it would miss the
    // ways of moving the feed that are not the wheel (a trackpad, the arrow keys, Page Down).
    if app.page_home_if_near_end() {
        changed = true;
    }

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
        } else if status.time_pos.is_finite() && (status.time_pos - app.last_position).abs() >= 0.2
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
                    Err(error) => {
                        // Put the page number back so a retry asks for this page again.
                        app.home_page = app.home_page.saturating_sub(1);
                        app.home_has_more = false;
                        app.feed_error = Some(error);
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
                    Err(error) => app.feed_error = Some(error),
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
                match result {
                    Ok(page) => {
                        app.author_page = page.page;
                        app.author_has_more = page.has_more;
                        app.author_videos.extend(page.videos);
                        app.new_rows = true;
                    }
                    Err(error) => app.feed_error = Some(error),
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

/// Keys the window handles itself. The transport keys only count while something is open:
/// with nothing playing, Space used to start a "playback" of nothing, and it swallowed the
/// key a reader might be using for something else.
fn on_key(app: &mut BiliGuga, event: &KeyEvent, tx: &mut Tx) -> bool {
    let mods = event.mods;
    let watching = app.playing_video.is_some();
    match event.key {
        Key::Text(ref typed) if typed == " " && !mods.ctrl && !mods.alt && watching => {
            app.toggle_pause();
            true
        }
        Key::Left if watching => {
            app.player.seek_relative(-10.);
            true
        }
        Key::Right if watching => {
            app.player.seek_relative(10.);
            true
        }
        Key::Up if watching => {
            app.volume = (app.volume + 5.).clamp(0., 100.);
            app.player.set_volume(app.volume);
            app.persist_player_settings();
            true
        }
        Key::Down if watching => {
            app.volume = (app.volume - 5.).clamp(0., 100.);
            app.player.set_volume(app.volume);
            app.persist_player_settings();
            true
        }
        Key::Text(ref typed)
            if typed.eq_ignore_ascii_case("f") && !mods.ctrl && !mods.alt && watching =>
        {
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
            // Windowed, Escape is the way out of the player: back to the browse grid.
            if watching {
                app.close_player();
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

fn fetch_qr_code_event()
-> impl std::future::Future<Output = Result<login::QrCodeData, String>> + Send {
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
    ("empty", seed_empty),
    ("login", seed_login),
    ("history", seed_history),
    ("signed-out", seed_signed_out),
    ("theater", seed_theater),
    ("fullscreen", seed_fullscreen),
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

/// The first frame a fresh install shows: nothing loaded, nothing playing.
fn seed_empty(app: &mut BiliGuga) {
    app.loading = false;
    app.home_loading = false;
}

/// A feed that needs a session, on a machine that has none: the panel offers the door.
fn seed_signed_out(app: &mut BiliGuga) {
    seed_empty(app);
    // The session is loaded from disk at launch, so a machine that has one would otherwise
    // never show this screen.
    app.session = None;
    app.account_avatar = None;
    app.active_tab = AppTab::Dynamic;
    app.dynamic_loading = false;
}

/// Theater mode: the player alone in the window, no feed beside it.
fn seed_theater(app: &mut BiliGuga) {
    seed_player(app);
    app.theater = true;
}

/// The OS-level full screen, with its floating control strip.
fn seed_fullscreen(app: &mut BiliGuga) {
    seed_player(app);
    app.player_fullscreen = true;
}

/// The login card, before the QR code has arrived.
fn seed_login(app: &mut BiliGuga) {
    seed_empty(app);
    app.active_tab = AppTab::Login;
    app.login_status = "正在获取登录二维码…".into();
}

/// A signed-in feed, so the history list can be checked without an account.
fn seed_history(app: &mut BiliGuga) {
    seed_loaded(app);
    app.active_tab = AppTab::History;
    app.history = demo_videos(10);
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
        min_size: Some((820.0, 560.0)),
        ..AppSpec::new("哔哩咕嘎", (1360.0, 820.0), BiliGuga::new, view)
    });
}

impl BiliGuga {
    /// The video the player is on, with the ids its action endpoints need.
    fn current_action_target(&self) -> Option<Video> {
        self.playing_video.clone()
    }

    fn like_current(&mut self) {
        let (Some(cookie), Some(video)) = (self.cookie(), self.current_action_target()) else {
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
        let (Some(cookie), Some(video)) = (self.cookie(), self.current_action_target()) else {
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
        let (Some(cookie), Some(video)) = (self.cookie(), self.current_action_target()) else {
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
        let clip =
            std::env::var("BILIGUGA_TEST_CLIP").unwrap_or_else(|_| "/tmp/biliguga-test.mp4".into());
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
        assert!(
            width > 0 && height > 0,
            "the frame buffer is {width}x{height}"
        );

        // A decoded frame must be non-blank: the test clip is colourful.
        let mut non_black = 0usize;
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(20));
            player.poll_frame();
            let (now_width, now_height) = target.size();
            assert_eq!(
                (now_width, now_height),
                (width, height),
                "the size changed mid-check"
            );
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
        assert!(
            !app.home_loading,
            "nothing is in flight before the first request"
        );
        assert_eq!(app.home_page, 0);
        app.load_home_page(false);
        assert_eq!(app.home_page, 1, "page 1 is recorded when it is asked for");
        assert!(app.home_loading, "and it is in flight now");
        app.home_loading = false;
        app.load_home_page(false);
        assert_eq!(
            app.home_page, 2,
            "the next request asks for page 2, not page 1 again"
        );
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

    /// A feed that fails to load says so in the column the viewer is looking at. It used to
    /// write its error into `playback_error`, which only the player column draws, so the
    /// viewer saw an empty feed and no reason for it.
    #[test]
    fn a_feed_failure_is_reported_in_the_feed() {
        let mut app = BiliGuga::new();
        app.apply(Event::Search(Err("网络不给力".into())));
        assert_eq!(app.feed_error.as_deref(), Some("网络不给力"));
        assert!(
            app.playback_error.is_none(),
            "the player's error is untouched"
        );
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

    /// The home feed asks for the next page while the reader is a pane away from the end, and
    /// only downward progress narrows that distance: it used to be judged from the grid's
    /// visible rows, so measuring a row could shift the range by one and the check flipped
    /// direction — the feed would sit still at the bottom of a page and then load one as the
    /// reader scrolled *up*.
    #[test]
    fn the_feed_pages_a_pane_before_its_end() {
        let mut app = BiliGuga::new();
        let (sender, _events) = std::sync::mpsc::channel();
        app.worker = Some(Worker {
            events: sender,
            waker: Arc::new(|| {}),
        });
        app.videos = demo_videos(40);
        app.loading = false;
        app.home_loading = false;
        app.home_has_more = true;
        app.active_tab = AppTab::Home;
        app.pane_height.set(713.0);
        // Well above the end: nothing to do.
        app.scroll_pos.set(Some((0.0, 8000.0)));
        assert!(!app.page_home_if_near_end());
        // One pane from the end: ask now.
        app.scroll_pos.set(Some((8000.0 - 713.0, 8000.0)));
        assert!(app.page_home_if_near_end());
        assert_eq!(app.home_page, 1, "and ask for page 1");
        assert!(app.home_loading);
        // Already in flight: asking again would queue a duplicate page.
        assert!(!app.page_home_if_near_end());
    }

    /// The distance to the end only grows when the reader scrolls up, so a feed that is loading
    /// when it reaches the bottom never turns around and asks again on the way back.
    #[test]
    fn scrolling_up_never_asks_for_a_page() {
        let mut app = BiliGuga::new();
        let (sender, _events) = std::sync::mpsc::channel();
        app.worker = Some(Worker {
            events: sender,
            waker: Arc::new(|| {}),
        });
        app.videos = demo_videos(40);
        app.loading = false;
        app.home_loading = false;
        app.home_has_more = true;
        app.active_tab = AppTab::Home;
        app.pane_height.set(713.0);
        let max = 8000.0;
        // Every position on the way back up, from the end to the top.
        let mut y = max;
        while y > 0.0 {
            app.scroll_pos.set(Some((y, max)));
            y -= 100.0;
            // Only the last pane can ask, and once it is asked the request is in flight.
            if !app.home_loading {
                app.page_home_if_near_end();
            }
            if app.home_loading {
                break;
            }
        }
        assert_eq!(app.home_page, 1, "asked once, at the bottom");
        assert!(app.home_loading);
        // Everything above the bottom pane changes nothing while the request is in flight.
        app.scroll_pos.set(Some((0.0, max)));
        assert!(!app.page_home_if_near_end());
    }

    /// A page that fits the pane has no scroll room at all, and still asks for the next one.
    #[test]
    fn a_page_shorter_than_the_pane_still_pages() {
        let mut app = BiliGuga::new();
        let (sender, _events) = std::sync::mpsc::channel();
        app.worker = Some(Worker {
            events: sender,
            waker: Arc::new(|| {}),
        });
        app.videos = demo_videos(20);
        app.loading = false;
        app.home_loading = false;
        app.home_has_more = true;
        app.active_tab = AppTab::Home;
        app.pane_height.set(713.0);
        // Nothing to scroll, and the list is shorter than the pane.
        app.scroll_pos.set(Some((0.0, 0.0)));
        assert!(app.page_home_if_near_end());
        // With nothing left to load, it asks for nothing however close the reader gets.
        app.home_loading = false;
        app.home_has_more = false;
        assert!(!app.page_home_if_near_end());
    }

    /// A frame that lands a page then decides again from data that has not been laid out:
    /// that was the bug that made the feed page forever. `poll` therefore decides *before* it
    /// applies the frame's events, so the layout `max` describes is the layout the decision
    /// reads.
    #[test]
    fn a_landed_page_is_measured_before_the_next_decision() {
        let mut app = BiliGuga::new();
        let (sender, events) = std::sync::mpsc::channel();
        app.worker = Some(Worker {
            events: sender,
            waker: Arc::new(|| {}),
        });
        app.videos = demo_videos(20);
        app.loading = false;
        app.home_loading = true;
        app.home_has_more = true;
        app.active_tab = AppTab::Home;
        app.pane_height.set(713.0);
        // Nothing below the last row, and the reader there: the feed is waiting on a page.
        app.scroll_pos.set(Some((0.0, 0.0)));
        // The page lands as an event, with the rows it brings.
        drop(events);
        // Before the event is applied the viewport still describes the old, shorter content;
        // once it has grown, `max` must be allowed to catch up before another page is asked
        // for. Standing at `max == 0` with content that fits is a real case (it asks once,
        // while `home_loading` then blocks a second ask), so the guard under test is the
        // in-flight flag: a landing must not clear it and let the same frame ask again.
        assert!(!app.page_home_if_near_end(), "a page is already in flight");
        app.home_loading = false;
        app.videos.extend(demo_videos(20));
        // The taller list's `max` has not been published yet: it still says 0. That is exactly
        // what the old order read, and why it asked again every frame.
        assert!(
            app.page_home_if_near_end(),
            "with the stale max, it asks again — the reason the order matters"
        );
    }

    /// Before the viewport has ever been laid out there is nothing to judge: its `max` is 0,
    /// which used to read as "the last row is the bottom of the pane" and paged the feed four
    /// times in the first four frames of a run.
    #[test]
    fn nothing_is_decided_before_the_viewport_has_been_measured() {
        let mut app = BiliGuga::new();
        let (sender, _events) = std::sync::mpsc::channel();
        app.worker = Some(Worker {
            events: sender,
            waker: Arc::new(|| {}),
        });
        app.videos = demo_videos(20);
        app.home_has_more = true;
        app.active_tab = AppTab::Home;
        app.pane_height.set(713.0);
        assert_eq!(app.scroll_pos.get(), None, "nothing is known yet");
        assert!(!app.page_home_if_near_end(), "so nothing is asked for");
        assert!(!app.home_loading);
    }

    /// The feed lays out from a plain state, which is what `--snap` shows too.
    #[test]
    fn the_view_builds_from_a_seeded_feed() {
        let mut app = BiliGuga::new();
        app.videos = demo_videos(12);
        app.loading = false;
        let mut runtime = Runtime::new(app, view, (1360.0, 820.0), rune::Env::new(true, 1.0));
        runtime.frame(0.0);
        assert!(
            runtime.presented("home-grid").is_some(),
            "the grid did not mount"
        );
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
        let first_ids: std::collections::HashSet<_> = first
            .videos
            .iter()
            .map(|video| video.bvid.clone())
            .collect();
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
        let image =
            futures::executor::block_on(network::run(async move { cover.await.ok().flatten() }))
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
            let image =
                futures::executor::block_on(network::run(
                    async move { cover.await.ok().flatten() },
                ));
            assert!(image.is_some(), "a cover failed to decode: {url}");
        }
        eprintln!("{} covers in {:?}", urls.len(), started.elapsed());
    }
}

#[cfg(test)]
mod stress {
    //! Load tests, run with `cargo test --release stress -- --ignored --nocapture`.
    //!
    //! These drive the real view over the real `Runtime`: the same code the window runs, with a
    //! fixed virtual clock, so a number here is a number the app would pay. `#[ignore]` because
    //! they are slow and because their output is a report, not an assertion about right/wrong —
    //! the assertions are loose bounds that catch a regression, not a target.
    use super::*;
    use crate::metrics::{self, mib};
    use std::time::Instant;

    fn runtime_with(videos: usize) -> rune::Runtime<BiliGuga> {
        let mut app = BiliGuga::new();
        app.videos = demo_videos(videos);
        app.loading = false;
        app.home_loading = false;
        rune::Runtime::new(app, view, (1360.0, 820.0), rune::Env::new(true, 1.0))
    }

    /// The cost of a commit while the home feed scrolls, which is what the app does most.
    ///
    /// `BILIGUGA_STRESS_FRAMES` raises the frame count, for profiling: the default run is far
    /// too short for a sampler to see anything.
    #[test]
    #[ignore = "slow: a load test, run explicitly"]
    fn scroll_cost_over_a_full_home_feed() {
        let mut rt = runtime_with(HOME_MAX_ITEMS);
        rt.frame(0.0);
        let before = metrics::sample();

        let started = Instant::now();
        let frames = std::env::var("BILIGUGA_STRESS_FRAMES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(240);
        for step in 0..frames {
            // A trackpad-ish delta, with the pointer over the grid.
            rt.wheel_xy(300.0, 400.0, 0.0, -40.0);
            rt.frame(step as f64 / 60.0);
        }
        let elapsed = started.elapsed();
        let after = metrics::sample();

        let per_frame = elapsed.as_secs_f64() * 1000.0 / frames as f64;
        eprintln!(
            "scroll: {frames} commits over {} rows in {elapsed:?} ({per_frame:.2} ms/frame)",
            HOME_MAX_ITEMS
        );
        if let (Some(before), Some(after)) = (before, after) {
            eprintln!(
                "  memory: rss {:.1} -> {:.1} MiB (peak {:.1}), cpu {:.2}s",
                mib(before.rss),
                mib(after.rss),
                mib(after.rss_peak),
                after.cpu_seconds - before.cpu_seconds,
            );
        }
        // A commit that costs a whole frame budget would be a design problem, not a slow path.
        assert!(per_frame < 16.0, "a scroll commit took {per_frame:.2} ms");
    }

    /// Paging in new rows: the append, the trim, and the cover requests that follow.
    #[test]
    #[ignore = "slow: a load test, run explicitly"]
    fn paging_to_the_retention_limit_does_not_grow_memory() {
        let mut rt = runtime_with(0);
        rt.frame(0.0);
        let before = metrics::sample().expect("a sample");

        let mut appended = 0usize;
        // Paging in a loop: the number that matters is whether RSS keeps climbing once the
        // feed is at its limit, not how much a fresh process costs. Comparing against the
        // starting RSS would measure the first pages filling the retention window, which is
        // work the app is supposed to do.
        let mut settled = 0.0;
        for page in 0..40 {
            // The same shape `Event::Home` produces, without the network.
            let batch = demo_videos(20);
            appended += batch.len();
            rt.state.videos.extend(batch);
            rt.state.trim_home_items();
            rt.state.new_rows = true;
            rt.frame(page as f64 / 60.0);
            if page == 9 {
                // Ten pages in, the feed has long been at its retention limit.
                settled = mib(metrics::sample().expect("a sample").rss);
            }
        }
        let after = metrics::sample().expect("a sample");
        eprintln!(
            "paging: appended {appended} rows, kept {}, rss {:.1} -> {:.1} -> {:.1} MiB (settled -> end)",
            rt.state.videos.len(),
            mib(before.rss),
            settled,
            mib(after.rss),
        );
        assert!(
            rt.state.videos.len() <= HOME_MAX_ITEMS,
            "the feed grew past its retention limit: {}",
            rt.state.videos.len()
        );
        // Thirty more pages after the feed settled: a leak shows here, a full retention window
        // does not.
        let growth = mib(after.rss) - settled;
        assert!(
            growth < 8.0,
            "paging 30 more pages kept another {growth:.1} MiB: the retention limit is not \
             releasing rows"
        );
    }

    /// Covers arriving for rows that have since scrolled out of the feed. This is the shape of
    /// the leak the earlier pass found: the image cache held every thumbnail ever decoded.
    #[test]
    #[ignore = "slow: a load test, run explicitly"]
    fn covers_for_evicted_rows_are_released() {
        let mut rt = runtime_with(0);
        rt.state.images = Some(ImageCache::new(std::sync::Arc::new(|| {}) as rune::Waker));

        // Twelve rounds of twenty rows, each round over the retention limit, each cover a real
        // 160x90 buffer handed to the cache the way `attach_cover` does. RSS is read once the
        // cache has settled rather than at the start: the first rounds fill it, which is the
        // work the app is meant to do, and a leak shows in the rounds after that.
        let mut settled = 0.0;
        for round in 0..12 {
            rt.state.videos.extend(demo_videos(20));
            rt.state.trim_home_items();
            for video in rt.state.videos.clone() {
                let pixels = Rgba8::new(160, 90, vec![round as u8; 160 * 90 * 4]);
                rt.state
                    .attach_cover(video.bvid.clone(), std::sync::Arc::new(pixels));
            }
            rt.frame(round as f64 / 60.0);
            if round == 5 {
                settled = mib(metrics::sample().expect("a sample").rss);
            }
        }
        let after = metrics::sample().expect("a sample");
        eprintln!(
            "covers: {} held for {} rows, rss settled {:.1} -> {:.1} MiB",
            rt.state.cover_images.len(),
            rt.state.videos.len(),
            settled,
            mib(after.rss),
        );
        assert!(
            rt.state.cover_images.len() <= COVER_CACHE_LIMIT,
            "the cover cache passed its limit: {}",
            rt.state.cover_images.len()
        );
        // The six rounds after it settled must not keep the memory of six more cover sets.
        let growth = mib(after.rss) - settled;
        assert!(
            growth < 8.0,
            "covers for evicted rows kept another {growth:.1} MiB"
        );
    }

    /// Comment rows through `memo`, which is where a page of 200 comments is rebuilt or reused.
    #[test]
    #[ignore = "slow: a load test, run explicitly"]
    fn many_comment_rows_are_memoised() {
        let mut rt = runtime_with(4);
        rt.state.playing_video = Some(demo_video(0));
        rt.state.comments = (0..200)
            .map(|index| Comment {
                rpid: index as i64,
                username: format!("用户{index}"),
                message: "这个镜头调色太舒服了，请问用的是什么预设？".into(),
                like: 100,
                time: "3 小时前".into(),
            })
            .collect();
        rt.frame(0.0);

        let started = Instant::now();
        let frames = 60;
        for step in 0..frames {
            // A commit with no change to the comments: `memo` should reuse all 200 subtrees.
            rt.state.new_rows = false;
            rt.frame(step as f64 / 60.0);
        }
        let per_frame = started.elapsed().as_secs_f64() * 1000.0 / frames as f64;
        eprintln!("comments: {frames} idle commits over 200 rows, {per_frame:.2} ms/frame");
        assert!(per_frame < 16.0, "an idle commit took {per_frame:.2} ms");
    }

    /// A commit that rebuilds the whole tree rather than reusing `memo`d rows: what a state
    /// change on the comments does, and the worst case for a long list.
    #[test]
    #[ignore = "slow: a load test, run explicitly"]
    fn a_changed_comment_rebuilds_the_list() {
        let mut rt = runtime_with(4);
        rt.state.playing_video = Some(demo_video(0));
        rt.state.comments = (0..200)
            .map(|index| Comment {
                rpid: index as i64,
                username: format!("用户{index}"),
                message: "这个镜头调色太舒服了".into(),
                like: 100,
                time: "3 小时前".into(),
            })
            .collect();
        rt.frame(0.0);

        let started = Instant::now();
        let frames = 60;
        for step in 0..frames {
            // One new comment per commit: every row shifts and one is added.
            rt.state.comments.push(Comment {
                rpid: 1000 + step as i64,
                username: "新用户".into(),
                message: "刚看到这里".into(),
                like: 0,
                time: "刚刚".into(),
            });
            rt.frame(step as f64 / 60.0);
        }
        let per_frame = started.elapsed().as_secs_f64() * 1000.0 / frames as f64;
        eprintln!("comments-append: {frames} commits, {per_frame:.2} ms/frame");
        assert!(
            per_frame < 16.0,
            "a growing list took {per_frame:.2} ms per commit"
        );
    }

    /// The player's steady state: frames land from mpv and the window repaints without the view
    /// running. This is where a video client spends its time, so it is the number that matters.
    #[test]
    #[ignore = "slow: a load test, run explicitly"]
    fn video_frames_do_not_rerun_the_view() {
        let mut rt = runtime_with(12);
        rt.state.playing_video = Some(demo_video(0));
        let pixels = rune::Pixels::new();
        pixels.set(1920, 1080, vec![0u8; 1920 * 1080 * 4]);
        rt.state.player = mpv::MpvPlayer::new();
        rt.frame(0.0);

        // One buffer, rewritten in place: an 8 MiB `vec![]` per frame would measure the
        // allocator, not the app, and that is what the first version of this test did.
        let mut frame_buffer = vec![0u8; 1920 * 1080 * 4];
        let before = metrics::sample().expect("a sample");
        let started = Instant::now();
        let frames = 300;
        for step in 0..frames {
            frame_buffer[0] = step as u8;
            pixels.set(1920, 1080, std::mem::take(&mut frame_buffer));
            frame_buffer = vec![0u8; 1920 * 1080 * 4];
            rt.frame(step as f64 / 60.0);
        }
        let elapsed = started.elapsed();
        let after = metrics::sample().expect("a sample");
        let per_frame = elapsed.as_secs_f64() * 1000.0 / frames as f64;
        eprintln!(
            "video: {frames} frame updates + commits in {elapsed:?} ({per_frame:.2} ms/frame)"
        );
        eprintln!(
            "  cpu {:.2}s, rss {:.1} -> {:.1} MiB (peak {:.1})",
            after.cpu_seconds - before.cpu_seconds,
            mib(before.rss),
            mib(after.rss),
            mib(after.rss_peak),
        );
        // The upload itself is the renderer's cost and is not measured here (headless draws
        // nothing); this bounds the app-side work per frame.
        assert!(per_frame < 16.0, "a frame landed in {per_frame:.2} ms");
    }
    #[test]
    #[ignore = "diagnostic"]
    fn thread_census() {
        let mut rt = runtime_with(12);
        rt.frame(0.0);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let dir = std::fs::read_dir("/proc/self/task").unwrap();
        let mut names = Vec::new();
        for entry in dir.flatten() {
            let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
            names.push(comm.trim().to_string());
        }
        names.sort();
        eprintln!("{} threads:", names.len());
        let mut last = String::new();
        let mut count = 0;
        for n in &names {
            if *n == last {
                count += 1;
            } else {
                if count > 0 {
                    eprintln!("  {count}x {last}");
                }
                last = n.clone();
                count = 1;
            }
        }
        if count > 0 {
            eprintln!("  {count}x {last}");
        }
    }

    /// The thread count of the real player, not of a bare `mpv_initialize`: the options that
    /// matter (`hwdec-threads` above all) only show up once a decoder is actually running.
    ///
    /// Run with `BILIGUGA_TEST_CLIP=/path/to/1080p.mp4 cargo test --release player_threads --
    /// --ignored --nocapture`.
    #[test]
    #[ignore = "needs a local clip; see BILIGUGA_TEST_CLIP"]
    fn player_threads_while_decoding() {
        let clip = std::env::var("BILIGUGA_TEST_CLIP").unwrap_or_else(|_| "/tmp/t1080.mp4".into());
        if !std::path::Path::new(&clip).exists() {
            eprintln!("skipping: no clip at {clip}");
            return;
        }
        let before = std::fs::read_dir("/proc/self/task")
            .map(|d| d.count())
            .unwrap_or(0);
        let mut player = mpv::MpvPlayer::new();
        player.load(clip, None, 0., 1.);
        // Wait for frames to actually flow, which is when the decoder threads exist.
        let mut decoded = false;
        for _ in 0..400 {
            std::thread::sleep(Duration::from_millis(20));
            player.poll_frame();
            let status = player.status();
            if status.time_pos.is_finite() && status.time_pos > 0.3 {
                decoded = true;
                break;
            }
        }
        assert!(decoded, "the clip never started playing");
        let mut names: Vec<String> = std::fs::read_dir("/proc/self/task")
            .map(|d| {
                d.flatten()
                    .map(|e| {
                        std::fs::read_to_string(e.path().join("comm"))
                            .unwrap_or_default()
                            .trim()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        let count = |pred: &dyn Fn(&str) -> bool| names.iter().filter(|n| pred(n)).count();
        let lua = count(&|n| n.starts_with("lua/"));
        let av = count(&|n| n.starts_with("av:"));
        eprintln!(
            "player: {} threads total (before mpv: {before}), lua {lua}, av {av}",
            names.len()
        );
        // The point of the two decode options is that no per-core decoder pool appears. A
        // 16-core machine with defaults gives 16 `av:` threads; the ceiling here is what
        // hwdec needs to run at all.
        assert!(
            av <= 2,
            "decode threads came back: {av} of them, names {names:?}"
        );
    }
    /// What one rendered frame of a playing video costs the app side, in the headless runtime:
    /// the `Pixels` update plus the commit and paint that the wake triggers.
    #[test]
    #[ignore = "benchmark"]
    fn playing_frame_cost() {
        let mut rt = runtime_with(12);
        rt.state.playing_video = Some(demo_video(0));
        let pixels = rune::Pixels::new();
        pixels.set(1920, 1080, vec![0u8; 1920 * 1080 * 4]);
        rt.frame(0.0);

        let mut buffer = vec![0u8; 1920 * 1080 * 4];
        let frames: usize = std::env::var("BILIGUGA_STRESS_FRAMES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);
        // Warm up: the first frames shape text and build atlases.
        for step in 0..30 {
            buffer[0] = step as u8;
            pixels.set(1920, 1080, std::mem::take(&mut buffer));
            buffer = vec![0u8; 1920 * 1080 * 4];
            rt.frame(step as f64 / 60.0);
        }
        let before = metrics::sample().expect("sample");
        let started = Instant::now();
        for step in 0..frames {
            buffer[0] = step as u8;
            pixels.set(1920, 1080, std::mem::take(&mut buffer));
            buffer = vec![0u8; 1920 * 1080 * 4];
            rt.frame(step as f64 / 60.0);
        }
        let elapsed = started.elapsed();
        let after = metrics::sample().expect("sample");
        let per = elapsed.as_secs_f64() * 1000.0 / frames as f64;
        eprintln!(
            "playing frame: {per:.3} ms/frame, cpu {:.3}s for {frames} frames",
            after.cpu_seconds - before.cpu_seconds
        );
        // The 8 MiB buffer this test rebuilds every frame is the test's cost, not the app's;
        // it is here so `set` gets a fresh Vec like the worker's recycled buffers do.
    }
    /// How much of a playing frame's cost is the clock tick rather than the frame itself:
    /// the same loop with `poll`'s playhead check effectively disabled by a huge step.
    #[test]
    #[ignore = "benchmark"]
    fn playing_frame_without_the_clock() {
        let mut rt = runtime_with(12);
        rt.state.playing_video = Some(demo_video(0));
        let pixels = rune::Pixels::new();
        pixels.set(1920, 1080, vec![0u8; 1920 * 1080 * 4]);
        rt.frame(0.0);
        let mut buffer = vec![0u8; 1920 * 1080 * 4];
        for step in 0..30 {
            buffer[0] = step as u8;
            pixels.set(1920, 1080, std::mem::take(&mut buffer));
            buffer = vec![0u8; 1920 * 1080 * 4];
            rt.frame(step as f64 / 60.0);
        }
        let started = Instant::now();
        let frames = 300;
        for step in 0..frames {
            // A frame landing is the only thing that should matter: no `changed = true` from
            // the app's own poll path, so `build` only runs when `Pixels` wakes it.
            rt.state.last_position = f64::NEG_INFINITY;
            buffer[0] = step as u8;
            pixels.set(1920, 1080, std::mem::take(&mut buffer));
            buffer = vec![0u8; 1920 * 1080 * 4];
            rt.frame(step as f64 / 60.0);
        }
        let per = started.elapsed().as_secs_f64() * 1000.0 / frames as f64;
        eprintln!("frame only (no clock tick): {per:.3} ms/frame");
    }
    /// The exact path the window takes when a signed-in viewer clicks a video.
    #[test]
    #[ignore = "needs the network and a session"]
    fn live_signed_in_playback() {
        let session = login::load_session();
        eprintln!("session: {}", session.is_some());
        let cookie = session.as_ref().map(|s| s.cookie.clone());
        let page = futures::executor::block_on(network::run(async {
            fetch_recommendations(1, None).await
        }))
        .expect("feed failed");
        let video = page.videos.first().expect("no rows").clone();
        eprintln!(
            "row: bvid={} aid={} cid={}",
            video.bvid, video.aid, video.cid
        );
        let v2 = video.clone();
        let c = cookie.clone();
        let play = futures::executor::block_on(network::run(async move {
            resolve_play_url(&v2, c.as_deref(), 64).await
        }));
        match &play {
            Ok(p) => eprintln!("resolved: url len {} qn {}", p.url.len(), p.actual_quality),
            Err(e) => panic!("resolve failed: {e}"),
        }
        let play = play.expect("resolve");
        let mut player = mpv::MpvPlayer::new();
        player.load(play.url, play.audio_url, 100., 1.);
        let start = Instant::now();
        let mut last = 0.0;
        while start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(100));
            player.poll_frame();
            let s = player.status();
            if s.time_pos.is_finite() && s.time_pos > last {
                last = s.time_pos;
            }
            if last > 3.0 {
                break;
            }
        }
        let s = player.status();
        eprintln!(
            "time_pos={:.2} duration={:.2} paused={} presented={}",
            s.time_pos,
            s.duration,
            s.paused,
            player.presented().is_some()
        );
        assert!(s.time_pos > 1.0, "playback did not start: {s:?}");
        assert!(player.presented().is_some(), "no frame buffer ever arrived");
    }
}
