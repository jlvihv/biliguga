//! The app state: everything the window keeps between frames.

pub(crate) use super::*;

/// The first seek of a new stream: back to where the viewer left it.
///
/// Three flags and two `Option<f64>` used to express this, and their combinations were the real
/// state machine: "asked for nothing yet", "waiting for the cloud answer", "have a cloud
/// position", "have a position from a quality switch", "already seeked". Written out here,
/// `poll` reads it instead of reconstructing it from four fields every frame.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Resume {
    /// Nothing to seek: this stream plays from wherever mpv starts it.
    Nothing,
    /// A new stream is loading and the cloud position has not answered yet.
    Waiting,
    /// Seek here once the stream reports a duration: the cloud's position, or the playhead the
    /// viewer was at when they changed quality.
    To(f64),
}

pub(crate) struct BiliGuga {
    // Identity and navigation.
    pub(crate) search_edit: TextEdit,
    pub(crate) active_tab: AppTab,
    // Feeds. Only one is shown at a time; `selected` indexes into whichever that is.
    pub(crate) videos: Vec<Video>,
    pub(crate) search_results: Vec<Video>,
    pub(crate) history: Vec<Video>,
    pub(crate) watch_later: Vec<Video>,
    pub(crate) favorites: Vec<Video>,
    pub(crate) dynamic_videos: Vec<Video>,
    pub(crate) author_videos: Vec<Video>,
    /// Whose page the feed column is showing, while the Author tab is active. `mid` is only
    /// needed to ask for a page, `name` to label the column, and both are set together: they
    /// were two fields with a `0` / `""` pair standing for "nobody".
    pub(crate) author: Option<Author>,
    pub(crate) author_page: usize,
    pub(crate) author_has_more: bool,
    pub(crate) author_loading: bool,
    pub(crate) author_generation: u64,
    /// The row the keyboard and the highlight are on, `None` until the user picks one: an
    /// unplayed feed should not claim its first row is chosen.
    pub(crate) selected: Option<usize>,
    /// Bumped whenever any feed's rows change — a page landed, a tab was reloaded, the trim
    /// dropped rows. The virtual list's key function is only called when this changes, so a
    /// frame that merely scrolls or ticks the clock no longer builds a `String` per visible row
    /// just to find out that the row is the one it was.
    pub(crate) feed_revision: u64,
    /// A search is in flight. Every other feed has its own flag (`home_loading`,
    /// `author_loading`, and the four inside [`SignedInFeed`]); this one field used to be set
    /// by the home feed too, where it duplicated `home_loading` and [`Self::is_loading`] had to
    /// guess which of the two meanings it was being read with.
    pub(crate) search_loading: bool,
    pub(crate) history_loading: bool,
    pub(crate) watch_later_loading: bool,
    pub(crate) favorites_loading: bool,
    pub(crate) dynamic_loading: bool,
    // Paging.
    pub(crate) home_page: usize,
    pub(crate) home_loading: bool,
    pub(crate) home_has_more: bool,
    pub(crate) home_generation: u64,
    // Account and login.
    pub(crate) session: Option<UserSession>,
    pub(crate) account_avatar: Option<Image>,
    pub(crate) login_qr: Option<Image>,
    pub(crate) login_key: Option<String>,
    /// What the login panel tells the viewer. Almost every value is a fixed sentence, so this
    /// is a `Cow`: the seven static messages cost no allocation, and only an error from the
    /// API is a `String`.
    pub(crate) login_status: std::borrow::Cow<'static, str>,
    pub(crate) login_generation: u64,
    // Player.
    pub(crate) playback: PlaybackState,
    pub(crate) volume: f64,
    pub(crate) speed: f64,
    pub(crate) quality: u32,
    pub(crate) quality_options: Vec<PlayQuality>,
    /// The quality menu is open. It was `Option<PlayerMenu>` with a one-variant enum: a menu
    /// that can only be one menu is a `bool` wearing a type's clothes.
    pub(crate) quality_menu_open: bool,
    /// The episode list is open, when the clip belongs to a collection.
    pub(crate) episode_menu_open: bool,
    pub(crate) seek_dragging: bool,
    pub(crate) volume_dragging: bool,
    pub(crate) controls_until: Option<Instant>,
    /// Whether the pointer is over the picture. The controls stay up while it is and fade when
    /// it leaves, so moving the mouse off the video hides them without waiting out the linger.
    pub(crate) pointer_on_stage: bool,
    pub(crate) player_fullscreen: bool,
    /// The video on the stage. Shared rather than copied: a play, a quality change and a
    /// detail call all need it at once, and each `clone` was a fresh handful of `String`s.
    pub(crate) playing_video: Option<Arc<Video>>,
    /// Bumped for every stream the player starts loading — a play, or a quality switch on the
    /// same video. Every answer about that stream comes back tagged with the number it was
    /// asked under, and a mismatch is dropped: see [`Event`].
    pub(crate) playback_generation: u64,
    /// Where the first seek of a new stream stands. One value instead of the three fields it
    /// replaces (`pending_seek`, `cloud_resume_progress`, `cloud_resume_applied`), whose
    /// combinations were a state machine nobody had written down.
    pub(crate) resume: Resume,
    /// The stage size the view last drew at.
    ///
    /// `view` gets `&self`, so it publishes the measurement here and `poll` — which owns the
    /// command channel to mpv — is what acts on it. mpv remembers the size it is already
    /// decoding for, so nothing has to mirror it here.
    pub(crate) stage_size: std::cell::Cell<Option<(usize, usize)>>,
    /// The clip's own numbers, once the detail call lands. `None` means "not loaded yet".
    pub(crate) stats: Option<VideoStats>,
    pub(crate) playback_error: Option<String>,
    /// Why the last feed request failed. Separate from `playback_error`: a search that fails
    /// used to write its error there, where only the player column shows it, so the viewer saw
    /// an empty feed and no reason for it.
    pub(crate) feed_error: Option<String>,
    pub(crate) collection: Option<VideoCollection>,
    pub(crate) history_report_at: Instant,
    pub(crate) history_report_in_flight: bool,
    // Comments.
    pub(crate) comments: Vec<Comment>,
    pub(crate) comments_page: u32,
    pub(crate) comments_total: i64,
    pub(crate) comments_has_more: bool,
    pub(crate) comments_loading: bool,
    pub(crate) comments_generation: u64,
    pub(crate) comments_error: Option<String>,
    // Runtime plumbing.
    pub(crate) power_inhibitor: PowerInhibitor,
    pub(crate) player: mpv::MpvPlayer,
    /// The cancel flag this app's covers share. Nothing flips it: a cover that arrives late for
    /// a card that scrolled away is stored and reused, so cancelling would only throw the work
    /// away. It stays a parameter of [`queue_cover_download`] because that function is written
    /// to be cancellable; the app hands it a flag it never sets.
    pub(crate) covers_cancelled: Arc<AtomicBool>,
    /// Covers asked for and not yet arrived, by bvid. Without this, every page load asked for
    /// every missing cover again: a 20-row feed could fire 20 identical downloads per page.
    pub(crate) covers_in_flight: std::collections::HashSet<String>,
    pub(crate) images: Option<ImageCache>,
    pub(crate) worker: Option<Worker>,
    pub(crate) events: std::sync::mpsc::Receiver<Event>,
    /// Covers already decoded, by bvid. Rune's `Image` is a cheap shared handle, so one lookup
    /// per card per frame costs nothing and no bytes are hashed again.
    pub(crate) cover_images: std::collections::HashMap<String, Image>,
    /// The playhead the clock last showed, so `poll` reports a change only when the bar moves.
    pub(crate) last_position: f64,
    /// The last thing an action (like, coin, favourite) told the user.
    pub(crate) action_note: Option<String>,
    /// How many cards the browse grid fits across, and how wide each one is. `view` gets
    /// `&self`, so — as with `stage_size` — it measures the pane and leaves the numbers here
    /// for the grid it is building in the same pass.
    pub(crate) card_columns: std::cell::Cell<usize>,
    pub(crate) card_width: std::cell::Cell<f32>,
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
    pub(crate) scroll_pos: std::rc::Rc<std::cell::Cell<Option<(f32, f32)>>>,
    /// Set when the feed moves the reader past a row, and cleared when the feed asks for a page.
    ///
    /// This is the fact that separates "somebody is scrolling" from "the layout settled", and
    /// distance to the end cannot tell them apart: a landing adds rows below, `trim_home_items`
    /// takes rows off the top, and when the two cancel out the end sits exactly where it was, so
    /// a reader parked at the bottom looks identical, frame after frame, to one who is still
    /// travelling. `poll` runs the check on every frame, so that reading asked for a page on
    /// every frame — the feed loaded forever and could never show what it had brought, because
    /// what it brought never made the feed any taller.
    ///
    /// It is fed by the viewport's own `on_scroll_offset`, and counts *rows*, not points: that
    /// handler is the only thing every way of scrolling the feed goes through (the wheel below,
    /// a trackpad, a drag, the arrow keys, Page Down), and a row is what paging is about. A
    /// ramp that lands under a reader who has not touched anything does not move the offset
    /// (the trim keeps their row in place), so it does not set this either.
    pub(crate) reader_row: std::rc::Rc<std::cell::Cell<Option<usize>>>,
    /// The reader's place when the feed last asked for a page, counted in rows of the whole feed
    /// rather than of the list currently loaded: `trim_home_items` drops rows off the top, which
    /// shortens the list under the reader, and a bare index into the current list would then sit
    /// permanently ahead of them — the feed would ask once and never again, however far they
    /// scrolled. Measured this way, "the reader has moved on" survives a trim, and a landing that
    /// the trim immediately cancels out leaves the number where it was, which is what keeps a
    /// parked reader from paging forever.
    pub(crate) asked_row: Option<usize>,
    /// Rows the trim has dropped from the top of the feed since it was loaded, so a row index in
    /// the current list can be turned into one of the whole feed.
    pub(crate) trimmed_rows: usize,

    /// The browse pane's height, measured in `view`, so paging knows how much of the feed is
    /// on screen without guessing from a row count.
    pub(crate) pane_height: std::cell::Cell<f32>,
}

impl BiliGuga {
    pub(crate) fn new() -> Self {
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
            feed_revision: 0,
            search_loading: false,
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
            login_status,
            login_generation: 0,
            playback: PlaybackState::Idle,
            volume,
            speed,
            quality: 64,
            quality_options: Vec::new(),
            quality_menu_open: false,
            episode_menu_open: false,
            seek_dragging: false,
            volume_dragging: false,
            controls_until: None,
            pointer_on_stage: false,
            player_fullscreen: false,
            playing_video: None,
            playback_generation: 0,
            resume: Resume::Nothing,
            stage_size: std::cell::Cell::new(None),
            stats: None,
            playback_error: None,
            feed_error: None,
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
            card_columns: std::cell::Cell::new(2),
            card_width: std::cell::Cell::new(CARD_MIN_WIDTH),
            scroll_pos: std::rc::Rc::new(std::cell::Cell::new(None)),
            // `None` until the viewport has been laid out once: the first frame is what turns
            // the offset into a row.
            reader_row: std::rc::Rc::new(std::cell::Cell::new(None)),
            asked_row: None,
            trimmed_rows: 0,
            pane_height: std::cell::Cell::new(700.0),
        }
    }
}

impl BiliGuga {
    pub(crate) fn worker(&self) -> &Worker {
        self.worker.as_ref().expect("worker is installed in setup")
    }

    /// The clip's shape, width over height, for the stage to take its height from. 16:9 until mpv
    /// reports what it is actually showing, so the stage has a sensible size on the first frame.
    pub(crate) fn video_aspect(&self) -> f32 {
        self.player
            .source_size()
            .filter(|(width, height)| *width > 0 && *height > 0)
            .map_or(16.0 / 9.0, |(width, height)| width as f32 / height as f32)
    }

    /// The session cookie for a request that can run logged out.
    ///
    /// Seventeen call sites wrote `as_ref().map(|s| s.cookie.clone())` by hand; one accessor
    /// says the same thing and cannot drift.
    pub(crate) fn cookie(&self) -> Option<String> {
        self.session.as_ref().map(|session| session.cookie.clone())
    }

    /// The uploader's name for the feed heading, empty when no author page is open.
    pub(crate) fn author_name(&self) -> &str {
        self.author
            .as_ref()
            .map_or("", |author| author.name.as_str())
    }

    /// The rows of the feed the feed column is showing. A slice, not a `&Vec`: no caller needs
    /// to know it is a `Vec`, and this way none of them can push to it by accident.
    pub(crate) fn current_videos(&self) -> &[Video] {
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

    /// Says the feed's rows changed, so the virtual lists rebuild their keys.
    pub(crate) fn rows_changed(&mut self) {
        self.feed_revision = self.feed_revision.wrapping_add(1);
    }

    /// Drops the oldest rows once the home feed grows past [`HOME_KEEP_ROWS`].
    ///
    /// Nothing the reader can reach goes: the cap is far more rows than anyone scrolls back
    /// through, and it is there so a session left open for days cannot grow the list without
    /// end. Trimming from the front at a tight limit was the shape this used to have, and it
    /// both ended the feed behind the reader — scroll up and there is nothing there — and lay
    /// behind the paging faults: a page landing below and a trim above cancel out, so the
    /// content never got taller, the end never moved away from the reader, and what was on
    /// screen was renumbered and replaced every time a page landed.
    ///
    /// The dropped rows' covers are released with them.
    pub(crate) fn trim_home_items(&mut self) {
        if self.videos.len() <= HOME_KEEP_ROWS {
            return;
        }
        // In whole grid rows. A grid row is keyed by its first video's bvid, so dropping a
        // number of videos that is not a multiple of the column count renumbers every row: what
        // is on screen reads as an entirely new set of rows. One column (a list) is the same
        // rule with a row being a single video.
        let per_row = self.card_columns.get().max(1);
        let remove_count = ((self.videos.len() - HOME_KEEP_ROWS) / per_row) * per_row;
        if remove_count == 0 {
            return;
        }
        let dropped: Vec<String> = self.videos.drain(..remove_count).map(|v| v.bvid).collect();
        for bvid in dropped {
            self.forget_cover(&bvid);
        }
        self.selected = self
            .selected
            .map(|selected| selected.saturating_sub(remove_count));
        // Rows dropped off the top, so a row index in the loaded list can still be turned into a
        // place in the whole feed. Rune's virtual list anchors the commit to the row the reader
        // is on and moves the viewport itself, so nothing else has to be corrected here.
        self.trimmed_rows += remove_count / per_row;
        self.rows_changed();
    }

    /// One row of the browse grid or of the list, as the last frame laid it out. Turns a scroll
    /// offset into a count of rows, which is what paging is about: a reader who has scrolled past
    /// three more rows has moved on, whatever the pixel height of a row happens to be.
    pub(crate) fn feed_row_height(&self) -> f32 {
        if self.card_columns.get() >= 2 {
            grid_row_height(self.card_width.get())
        } else {
            LIST_ROW_HEIGHT
        }
    }

    /// The seven lists a feed column can be showing. Every one of these is loaded, kept and
    /// walked for covers, and they used to be listed out three times by hand — once here, once
    /// in [`Self::bvids_on_screen`] and once in [`Self::request_missing_covers`] — so adding a
    /// feed meant remembering all three.
    pub(crate) fn feed_columns(&self) -> impl Iterator<Item = &Vec<Video>> {
        [
            &self.videos,
            &self.search_results,
            &self.dynamic_videos,
            &self.watch_later,
            &self.favorites,
            &self.history,
            &self.author_videos,
        ]
        .into_iter()
    }

    /// Every bvid any feed column, the player or the open collection can still show. A cover
    /// for one of these is in use; anything else is a candidate for eviction.
    pub(crate) fn bvids_on_screen(&self) -> impl Iterator<Item = &str> {
        self.feed_columns()
            .flatten()
            .map(|video| video.bvid.as_str())
            .chain(self.playing_video.as_deref().map(|video| video.bvid.as_str()))
            .chain(
                self.collection
                    .as_ref()
                    .into_iter()
                    .flat_map(|collection| &collection.episodes)
                    .map(|video| video.bvid.as_str()),
            )
    }

    /// Releases a cover nothing is showing any more.
    pub(crate) fn forget_cover(&mut self, bvid: &str) {
        if !self.bvids_on_screen().any(|shown| shown == bvid) {
            self.cover_images.remove(bvid);
        }
    }

    // ---- Feeds ---------------------------------------------------------------------------

    pub(crate) fn load_home_page(&mut self, reset: bool) {
        if !reset && (self.home_loading || !self.home_has_more) {
            return;
        }
        if reset {
            self.home_generation = self.home_generation.wrapping_add(1);
            self.home_page = 0;
            self.home_has_more = true;
            // The feed is about to be emptied, so nothing was asked for the list that is going
            // away: the first row of the new one is a new approach to its end.
            self.asked_row = None;
            self.trimmed_rows = 0;
            self.videos.clear();
            self.rows_changed();
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
    pub(crate) fn page_home_if_near_end(&mut self) -> bool {
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
        // Near the end, and the reader has moved on since the last page was asked for. See
        // `reader_row`: this is the case that a page per frame used to look like without it.
        // The reader's place in the whole feed: rows the trim has taken off the top, plus their
        // row in the list as it stands.
        let row = self.reader_row.get().map(|row| row + self.trimmed_rows);
        if row.is_some() && row <= self.asked_row {
            return false;
        }
        self.asked_row = row;
        self.load_home_page(false);
        true
    }

    /// Puts the feed where a reader who has scrolled to `offset` leaves it: the scroll position
    /// `poll` reads, and the row it counts as their place. Tests say both because a bare offset
    /// is a viewport no reader ever stood at — the row comes from the view, which is not running.
    pub(crate) fn load_search(&mut self) {
        // Read from the field itself: `search_query` was a second copy kept in step by hand at
        // four call sites, and three of those synced it immediately before this function
        // overwrote its only reader anyway.
        let query = self.search_edit.text.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.active_tab = AppTab::Search;
        self.search_loading = true;
        self.search_results.clear();
        self.feed_error = None;
        self.selected = None;
        self.worker().spawn(
            async move { fetch_search_results(&query).await },
            Event::Search,
        );
    }

    /// Folds a signed-in feed's answer into the state. A failure leaves whatever was shown.
    pub(crate) fn land_signed_in(
        &mut self,
        feed: SignedInFeed,
        result: Result<Vec<Video>, String>,
    ) {
        feed.set_loading(self, false);
        match result {
            Ok(videos) => {
                *feed.videos_mut(self) = videos;
                self.rows_changed();
                self.request_missing_covers();
            }
            Err(error) => self.feed_error = Some(error),
        }
    }

    /// Starts the request for a signed-in feed, or clears it when there is no session.
    ///
    /// The four feeds below were four copies of the same six lines with a different field name
    /// in each: copying one and forgetting to rename a field is exactly the kind of mistake the
    /// duplication was inviting.
    pub(crate) fn load_signed_in(&mut self, feed: SignedInFeed) {
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

    pub(crate) fn load_author_page(&mut self, reset: bool) {
        if !reset && (self.author_loading || !self.author_has_more) {
            return;
        }
        if reset {
            self.author_generation = self.author_generation.wrapping_add(1);
            self.author_page = 0;
            self.author_has_more = true;
            self.author_videos.clear();
            self.rows_changed();
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
    pub(crate) fn open_author(&mut self, mid: i64, name: String) {
        if mid <= 0 {
            return;
        }
        self.author = Some(Author { mid, name });
        self.active_tab = AppTab::Author;
        self.selected = None;
        self.load_author_page(true);
    }

    pub(crate) fn show_tab(&mut self, tab: AppTab) {
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

    /// Opens `video` on the stage and asks for everything it needs to play.
    ///
    /// The collection is dropped and re-fetched: another clip is another clip's collection,
    /// which the detail call will bring back if it has one.
    pub(crate) fn play_video(&mut self, video: Video) {
        self.play_video_keeping_collection(video, false)
    }

    /// Opens another episode of the collection already on screen.
    ///
    /// The collection survives the switch: it is the list the viewer is picking from, and
    /// dropping it to re-fetch would empty the menu they are pointing at — and make the episode
    /// buttons flip out of the transport row for a round trip.
    pub(crate) fn play_episode(&mut self, video: Video) {
        self.play_video_keeping_collection(video, true)
    }

    fn play_video_keeping_collection(&mut self, video: Video, keep_collection: bool) {
        // Release the previous stream first: its decoder, its audio track and its frame buffer
        // all belong to the video being replaced. Without this a quality change left the old
        // session decoding until the new one started.
        self.stop_playback();
        let video = Arc::new(video);
        self.playing_video = Some(video.clone());
        self.playback = PlaybackState::Buffering;
        self.playback_error = None;
        self.comments.clear();
        self.comments_page = 0;
        if !keep_collection {
            self.collection = None;
        }
        self.episode_menu_open = false;
        self.stats = None;
        // A fresh stream has its own generation, and its own first seek: the cloud position,
        // asked for below.
        self.resume = Resume::Waiting;
        // Show the controls once, so opening a video says how to pause it. With the pointer
        // elsewhere they would otherwise never appear: nothing has put it over the picture yet.
        self.show_controls();
        let generation = self.next_generation();
        self.request_stream(video, self.quality, generation);
        self.load_comments(true);
    }

    /// Asks the two calls a stream needs, tagged with the generation they answer under.
    fn request_stream(&mut self, video: Arc<Video>, quality: u32, generation: u64) {
        let cookie = self.cookie();
        let for_context = video.clone();
        let context_generation = generation;
        self.worker().spawn(
            async move { fetch_video_context(&for_context, cookie.as_deref()).await },
            move |result| match result {
                Ok(context) => Event::VideoContext(Box::new(context), context_generation),
                Err(_) => Event::Silent,
            },
        );
        let play_cookie = self.cookie();
        self.worker().spawn(
            async move { resolve_play_url(&video, play_cookie.as_deref(), quality).await },
            move |result| Event::PlayUrl(result, generation),
        );
    }

    /// The generation the next stream is asked under, and the one [`Event`] answers are
    /// checked against.
    fn next_generation(&mut self) -> u64 {
        self.playback_generation = self.playback_generation.wrapping_add(1);
        self.playback_generation
    }

    pub(crate) fn finish_play(&mut self, result: api::PlayUrlResult) {
        // The play URL call resolved the ids it needed; keep them on the video so the history
        // report and the detail call stop resolving them again.
        if let Some(video) = self.playing_video.as_mut() {
            let video = Arc::make_mut(video);
            if video.aid <= 0 {
                video.aid = result.aid;
            }
            if video.cid <= 0 {
                video.cid = result.cid;
            }
        }
        self.quality_options = result.qualities;
        self.quality = result.actual_quality;
        self.player
            .load(result.url, result.audio_url, self.volume, self.speed);
        self.playback = PlaybackState::Buffering;
        let Some(video) = self.playing_video.clone() else {
            return;
        };
        let generation = self.playback_generation;
        let play_cookie = self.cookie();
        self.worker().spawn(
            async move {
                fetch_last_play_progress(&video, play_cookie.as_deref())
                    .await
                    .map(|progress| progress as f64)
            },
            move |progress| Event::Resume(progress, generation),
        );
    }

    pub(crate) fn toggle_pause(&mut self) {
        // Pausing while the stream is still buffering would claim a state mpv has not reached:
        // the seek bar would show a pause button for a video with no picture yet.
        let paused = match self.playback {
            PlaybackState::Playing => true,
            PlaybackState::Paused => false,
            PlaybackState::Idle | PlaybackState::Buffering => return,
        };
        self.player.set_pause(paused);
        self.playback = if paused {
            PlaybackState::Paused
        } else {
            PlaybackState::Playing
        };
    }

    pub(crate) fn stop_playback(&mut self) {
        self.player.stop_playback();
        self.playback = PlaybackState::Idle;
        self.resume = Resume::Nothing;
        // Answers still in flight for the stream just stopped are now stale.
        self.playback_generation = self.playback_generation.wrapping_add(1);
        self.power_inhibitor.set_active(false);
    }

    /// Closes the player and gives the window back to the browse pane.
    pub(crate) fn close_player(&mut self) {
        self.stop_playback();
        self.playing_video = None;
        self.stats = None;
        self.collection = None;
        self.comments.clear();
        self.comments_total = 0;
        self.comments_error = None;
        self.action_note = None;
        self.playback_error = None;
        self.quality_menu_open = false;
        self.episode_menu_open = false;
        self.selected = None;
    }

    pub(crate) fn seek_percent(&mut self, percent: f64) {
        self.player.seek_percent(percent.clamp(0., 1.));
    }

    pub(crate) fn persist_player_settings(&self) {
        save_player_settings(self.volume, self.speed);
    }

    /// Reloads the current video at another quality, keeping the playhead.
    pub(crate) fn select_quality(&mut self, quality: u32) {
        let Some(video) = self.playing_video.clone() else {
            return;
        };
        let position = self.player.status().time_pos;
        self.quality = quality;
        self.quality_menu_open = false;
        self.playback = PlaybackState::Buffering;
        // Resume where the viewer was, once the new stream reports its own duration. This
        // overrides the cloud position: the viewer is further along than it.
        self.resume = if position.is_finite() && position > 1. {
            Resume::To(position)
        } else {
            Resume::Nothing
        };
        let generation = self.next_generation();
        self.request_stream(video, quality, generation);
    }

    pub(crate) fn select(&mut self, index: usize) {
        self.selected = Some(index);
        if let Some(video) = self.current_videos().get(index).cloned() {
            self.play_video(video);
        }
    }

    pub(crate) fn show_controls(&mut self) {
        self.controls_until = Some(Instant::now() + CONTROLS_LINGER);
    }

    /// Called with whether the pointer is over the picture.
    pub(crate) fn pointer_over_stage(&mut self, inside: bool) {
        self.pointer_on_stage = inside;
        if inside {
            self.show_controls();
        } else {
            // Off the picture: gone at once rather than after the linger. Waiting would leave
            // the controls sitting over a video nobody is pointing at.
            self.controls_until = None;
        }
    }

    /// Whether the browse pane is waiting on a request. A signed-in feed knows, the home feed
    /// only while it has nothing to show yet, and a search until its answer lands.
    pub(crate) fn is_loading(&self) -> bool {
        if let Some(feed) = self.active_tab.signed_in() {
            return feed.loading(self);
        }
        match self.active_tab {
            AppTab::Home => self.home_loading,
            AppTab::Author => self.author_loading,
            AppTab::Search => self.search_loading,
            // Login has its own status line, and the other tabs are signed-in feeds, handled
            // above.
            _ => false,
        }
    }

    /// Whether the controls are on screen.
    ///
    /// They follow the pointer the way a video player's do: up while the pointer moves over the
    /// picture or rests on it, and gone once it leaves and the linger runs out. What they never
    /// do is disappear while the viewer is using them — a press on a slider holds them, the
    /// quality menu holds them, and a paused picture holds them, because a paused player with no
    /// visible controls has no obvious way to resume.
    pub(crate) fn controls_visible(&self) -> bool {
        self.pointer_on_stage
            || self.controls_until
                .is_some_and(|until| Instant::now() < until)
            || self.quality_menu_open
            || self.episode_menu_open
            || self.seek_dragging
            || self.volume_dragging
            || self.playback == PlaybackState::Paused
    }

    pub(crate) fn queue_history_report(&mut self) {
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

    pub(crate) fn load_comments(&mut self, reset: bool) {
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

    pub(crate) fn start_login(&mut self) {
        self.login_generation = self.login_generation.wrapping_add(1);
        let generation = self.login_generation;
        self.login_status = "正在获取登录二维码…".into();
        self.login_qr = None;
        self.login_key = None;
        self.active_tab = AppTab::Login;
        self.worker().spawn(login::fetch_qr_code(), move |result| {
            Event::QrCode(result, generation)
        });
    }

    pub(crate) fn poll_login(&mut self) {
        let Some(key) = self.login_key.clone() else {
            return;
        };
        let generation = self.login_generation;
        self.worker().spawn(
            async move { login::poll_qr_code(&key).await },
            move |result| Event::QrPoll(result, generation),
        );
    }

    pub(crate) fn load_avatar(&mut self, url: String) {
        if url.is_empty() || self.account_avatar.is_some() {
            return;
        }
        self.worker()
            .spawn(async move { download_avatar(&url).await }, Event::Avatar);
    }

    pub(crate) fn refresh_session(&mut self) {
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
    pub(crate) fn request_cover(&mut self, bvid: String, url: String) {
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

    pub(crate) fn attach_cover(&mut self, bvid: String, pixels: Arc<Rgba8>) {
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

    pub(crate) fn cover_for(&self, video: &Video) -> Option<&Image> {
        self.cover_images.get(&video.bvid)
    }

    pub(crate) fn refresh_feed(&mut self) {
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

    pub(crate) fn cycle_speed(&mut self) {
        let index = SPEED_OPTIONS
            .iter()
            .position(|speed| (*speed - self.speed).abs() < 0.01)
            .unwrap_or(1);
        self.speed = SPEED_OPTIONS[(index + 1) % SPEED_OPTIONS.len()];
        self.player.set_speed(self.speed);
        self.persist_player_settings();
    }

    pub(crate) fn sync_power(&mut self) {
        self.power_inhibitor
            .set_active(self.playback == PlaybackState::Playing);
    }

    /// Asks for the covers the current feed is missing, skipping ones already downloaded or in
    /// flight for that bvid.
    pub(crate) fn request_missing_covers(&mut self) {
        let mut missing: Vec<(String, String)> = Vec::new();
        for videos in self.feed_columns() {
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

// ---- The view -------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn app_watching(playback: PlaybackState) -> BiliGuga {
        let mut app = BiliGuga::new();
        app.playing_video = Some(Arc::new(super::demo_video(0)));
        app.playback = playback;
        app.controls_until = None;
        app
    }

    /// A playing picture with the pointer off it shows no controls: that is the whole of
    /// "auto-hide", and the case the windowed player used to get wrong by always showing them.
    #[test]
    fn a_playing_picture_hides_its_controls() {
        let app = app_watching(PlaybackState::Playing);
        assert!(!app.controls_visible());
    }

    /// Moving the pointer onto the picture brings them up, and taking it off drops them at once
    /// rather than after the linger.
    #[test]
    fn the_pointer_shows_and_hides_them() {
        let mut app = app_watching(PlaybackState::Playing);
        app.pointer_over_stage(true);
        assert!(app.controls_visible());
        app.pointer_over_stage(false);
        assert!(!app.controls_visible());
    }

    /// Opening a video shows the controls once, so a viewer who is not already pointing at the
    /// picture can see how to pause it. The linger is what takes them away again.
    #[test]
    fn opening_a_video_shows_them_once() {
        let mut app = app_watching(PlaybackState::Playing);
        app.play_video(super::demo_video(1));
        assert!(app.controls_visible(), "opening a video shows its controls");
        // Expire the linger the way `poll` does when it comes due.
        app.controls_until = None;
        assert!(!app.controls_visible(), "and they go when it runs out");
    }

    /// The episode list holds the controls up: it is a thing the viewer is using, and it is
    /// drawn by the same branch.
    #[test]
    fn an_open_episode_list_holds_the_controls() {
        let mut app = app_watching(PlaybackState::Playing);
        app.episode_menu_open = true;
        assert!(app.controls_visible());
    }

    /// Switching episode keeps the collection, so the list the viewer is picking from does not
    /// empty itself, and picks up on the new episode.
    #[test]
    fn an_episode_switch_keeps_the_collection() {
        let mut app = app_watching(PlaybackState::Playing);
        let episodes = super::demo_videos(4);
        app.collection = Some(VideoCollection {
            id: 7,
            title: "合集".into(),
            episodes: episodes.clone(),
        });
        app.playing_video = Some(Arc::new(episodes[0].clone()));
        app.episode_menu_open = true;

        app.play_episode_at(2);
        assert!(app.collection.is_some(), "the list survives the switch");
        assert_eq!(current_episode_index(&app), Some(2), "and follows the new episode");
        assert!(!app.episode_menu_open, "picking one closes the list");
    }

    /// A clip with no siblings has no list: opening one is not offered.
    #[test]
    fn a_standalone_clip_has_no_episode_index() {
        let app = app_watching(PlaybackState::Playing);
        assert_eq!(current_episode_index(&app), None);
    }

    /// What holds them up: a paused picture (no other way to resume), a press on a slider, and
    /// the quality menu. None of these may be taken away by the pointer leaving.
    #[test]
    fn use_holds_them_up() {
        let mut paused = app_watching(PlaybackState::Paused);
        paused.pointer_over_stage(false);
        assert!(paused.controls_visible(), "a paused picture keeps its controls");

        let mut dragging = app_watching(PlaybackState::Playing);
        dragging.seek_dragging = true;
        assert!(dragging.controls_visible());

        let mut menu = app_watching(PlaybackState::Playing);
        menu.quality_menu_open = true;
        assert!(menu.controls_visible());
    }
}
