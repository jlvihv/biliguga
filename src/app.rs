//! The window: state, the async work behind it, and the Rune view that draws it.
//!
//! The shape follows Rune's model rather than GPUI's. There are three pieces:
//!
//! * [`BiliGuga`] is plain state. Handlers get `&mut BiliGuga` and change it directly; nothing
//!   is leased out of a store, so a handler can never panic on re-entering the view.
//! * [`Worker`] owns every async request. Each call spawns onto the shared tokio runtime,
//!   funnels its answer into one `mpsc` channel as an [`Event`], and wakes the window through
//!   the `Waker` Rune hands us in `setup`. [`poll`] drains that channel once per frame.
//! * [`view`] is a pure function of `&BiliGuga`: it builds the whole `El` tree on the frames
//!   that need one, and the runtime diffs it against the last commit by key.
//!
//! Player frames are the one thing that never goes through [`poll`]: mpv's worker thread
//! writes straight into a `rune::Pixels`, which the renderer uploads without re-running the
//! view. That is why a 1080p video does not rebuild the feed 30 times a second.
//!
//! The module is split by what a change touches, not by type:
//!
//! * [`state`] — the fields the window keeps between frames, and the feed logic over them.
//! * [`types`] — tabs, playback, feeds, events, the worker.
//! * [`consts`] — the tuning numbers and the pane sizes.
//! * [`theme`] — the palette.
//! * [`settings`] — the player's volume and speed, on disk.
//! * [`icons`] — the glyphs.
//! * [`ui`] — pills, buttons, poster art: what every pane draws with.
//! * [`browse`] — the navigation row, the feed column and its cards.
//! * [`actions`] — like, coin, favourite: what the viewer does to a video.
//! * [`player`] — the stage, transport, actions, detail and comments.
//! * [`titlebar`] — the bar the app draws in place of the window's native one.
//! * [`shell`] — [`view`], the window's sizes, and the one-time `setup`.
//! * [`polling`] — the frame loop: the player's clock, paging, and folding each event in.
//! * [`keys`] — keyboard handling.
//! * [`login_view`] — the QR-code panel and the signed-in account card.
//! * [`startup`] — the scenes `--snap` renders, and `main`'s entry into Rune.

mod actions;
mod browse;
mod consts;
mod icons;
mod keys;
mod login_view;
mod player;
mod polling;
mod settings;
mod shell;
mod startup;
mod titlebar;
mod state;
mod theme;
mod types;
mod ui;

use crate::{
    api::{
        self, Clock, PlayQuality, add_to_favorites, add_to_watch_later, coin_video,
        download_avatar, fetch_author_videos, fetch_comments, fetch_dynamic_feed, fetch_favorites,
        fetch_history, fetch_last_play_progress, fetch_recommendations, fetch_search_results,
        fetch_video_context, fetch_watch_later, like_video,
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

// What every submodule opens with as `use super::*`. Gathering it here is what lets a module
// reach for a sibling's item without knowing which file it lives in, and keeps the `use` lists
// above from being repeated in thirteen places.
pub(crate) use browse::*;
pub(crate) use consts::*;
pub(crate) use keys::*;
pub(crate) use login_view::*;
pub(crate) use player::*;
pub(crate) use settings::{load_player_settings, save_player_settings};
#[allow(unused_imports)] // used by startup itself, through `super::*`
pub(crate) use startup::{SCENES, Scene, demo_video, demo_videos};
pub(crate) use state::{BiliGuga, Resume};
pub(crate) use titlebar::*;
pub(crate) use types::*;
pub(crate) use ui::*;

// The three entry points `main` and Rune call, by the names they already use.
pub(crate) use polling::poll;
pub(crate) use shell::*;
pub(crate) use startup::launch;
