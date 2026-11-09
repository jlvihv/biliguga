//! The small types the window is built from: tabs, playback, feeds, events, the worker.

pub(crate) use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppTab {
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
    pub(crate) fn signed_in(self) -> Option<SignedInFeed> {
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
pub(crate) enum PlaybackState {
    Idle,
    Buffering,
    Playing,
    Paused,
}

/// Which popup the player has open. Only one kind exists: speed cycles in place and the
/// collection is reached with previous/next, so neither ever needed a menu.
/// Whose uploads the Author tab is showing.
pub(crate) struct Author {
    pub(crate) mid: i64,
    pub(crate) name: String,
}

/// The numbers the detail call brings back. They always arrive together, so they are one value
/// rather than four fields that a caller has to remember to set in step.
pub(crate) struct VideoStats {
    pub(crate) view_count: String,
    pub(crate) danmaku_count: String,
    pub(crate) like_count: String,
    pub(crate) published_at: String,
}

/// The feeds that need a session. Each one used to own a `load_..` method, a `.._loading` flag
/// and an `Event` arm holding the same three lines; the enum is the one place they differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignedInFeed {
    Dynamic,
    History,
    WatchLater,
    Favorites,
}

impl SignedInFeed {
    pub(crate) fn videos_mut(self, app: &mut BiliGuga) -> &mut Vec<Video> {
        match self {
            SignedInFeed::Dynamic => &mut app.dynamic_videos,
            SignedInFeed::History => &mut app.history,
            SignedInFeed::WatchLater => &mut app.watch_later,
            SignedInFeed::Favorites => &mut app.favorites,
        }
    }

    pub(crate) fn loading(self, app: &BiliGuga) -> bool {
        match self {
            SignedInFeed::Dynamic => app.dynamic_loading,
            SignedInFeed::History => app.history_loading,
            SignedInFeed::WatchLater => app.watch_later_loading,
            SignedInFeed::Favorites => app.favorites_loading,
        }
    }

    pub(crate) fn set_loading(self, app: &mut BiliGuga, value: bool) {
        match self {
            SignedInFeed::Dynamic => app.dynamic_loading = value,
            SignedInFeed::History => app.history_loading = value,
            SignedInFeed::WatchLater => app.watch_later_loading = value,
            SignedInFeed::Favorites => app.favorites_loading = value,
        }
    }

    /// The event that lands this feed's answer. `Event` has no payload for the feed itself, so
    /// the arm is picked here rather than at the call site.
    pub(crate) fn event(self) -> impl FnOnce(Result<Vec<Video>, String>) -> Event + Copy + 'static {
        match self {
            SignedInFeed::Dynamic => Event::Dynamic,
            SignedInFeed::History => Event::History,
            SignedInFeed::WatchLater => Event::WatchLater,
            SignedInFeed::Favorites => Event::Favorites,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlayerMenu {
    Quality,
}

/// One answer from background work, drained by `poll` at the top of a frame.
pub(crate) enum Event {
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
pub(crate) struct Worker {
    pub(crate) events: std::sync::mpsc::Sender<Event>,
    pub(crate) waker: Waker,
}

impl Worker {
    /// Runs `future` on the shared tokio runtime and turns its output into an `Event`.
    pub(crate) fn spawn<F>(&self, future: F, wrap: impl FnOnce(F::Output) -> Event + Send + 'static)
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
