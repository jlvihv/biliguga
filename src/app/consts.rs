//! The tuning numbers and the sizes the panes are laid out at. All in one place, so a
//! layout tweak is one edit rather than a hunt through the views.

pub(crate) use super::*;

/// How many rows the home feed keeps once it has scrolled well past them.
///
/// The feed keeps its rows: scrolling back up through a recommendation feed is a thing a reader
/// does, and a wall where the list used to be is a wall. What grows without a bound is the list
/// itself, so there is a cap — but a generous one, because a row is cheap. `Video` is a handful
/// of strings and numbers; a thousand of them is a few hundred kilobytes, while one decoded
/// cover is 57 KB and the cover cache alone is allowed [`COVER_CACHE_LIMIT`] of those. The cap
/// therefore exists to catch a runaway rather than to shape the ordinary case: at the size of a
/// pane this is dozens of screens of history, far past where anyone scrolls back to.
pub(crate) const HOME_KEEP_ROWS: usize = 2000;

/// How much of the next page to keep loaded past the bottom of what is loaded, in panes. The
/// feed asks for more while the reader is within this of the end, so a page arrives before
/// they reach it rather than after.
pub(crate) const PAGE_RUNWAY: f32 = 1.0;

/// How many decoded covers to keep. A cover is 57 KB, so this bounds the cache at a few
/// megabytes even if some path keeps adding rows without trimming.
pub(crate) const COVER_CACHE_LIMIT: usize = 256;

pub(crate) const SPEED_OPTIONS: [f64; 5] = [0.5, 1., 1.25, 1.5, 2.];

pub(crate) const DEFAULT_VOLUME: f64 = 100.;

pub(crate) const DEFAULT_SPEED: f64 = 1.;

/// Padding inside every pane.
pub(crate) const PANE_PAD: f32 = 20.;

/// The narrowest a browse card gets before the grid gives up a column.
pub(crate) const CARD_MIN_WIDTH: f32 = 232.;

/// The space between two cards, and the padding inside one around its cover.
pub(crate) const CARD_GAP: f32 = 14.;

pub(crate) const CARD_PAD: f32 = 6.;

/// How many points one notch of a mouse wheel moves the feed.
///
/// Rune turns a notch into a flat 40 points, which was a quarter of the old 170-point cards
/// and is a *fifth* of a row now that a poster card is ~200 points tall: the feed crawled, and
/// the two dozen notches a page of posters takes felt broken. A hundred points a notch is what
/// a browser scrolls, and it moves half a card, so a card can be crossed in two.
pub(crate) const WHEEL_NOTCH: f32 = 100.0;

/// The list layout's row: a 74-point cover plus the padding around it.
pub(crate) const LIST_ROW_HEIGHT: f32 = 88.0;

/// The parts of a grid card that sit under its cover: the gaps, the two title lines and the
/// uploader row. A grid row's height used to be spelled out as `* 9.0 / 16.0 + 78.0` in two
/// places — the virtual list's estimate and the paging arithmetic's row count — which had to
/// agree or paging would ask for a page at the wrong moment.
pub(crate) const CARD_CAPTION_HEIGHT: f32 = 78.0;

/// What Rune itself moves for a notch, so the handler below can scale rather than assume.
pub(crate) const RUNE_WHEEL_NOTCH: f32 = 40.0;

/// The narrowest the window goes. Below this the 2-column grid stops making sense and the
/// player's transport row runs out of room.
pub(crate) const MIN_WINDOW_WIDTH: f32 = 820.0;

/// The episode list: the height of one row, the gap between rows, and the box they scroll in.
/// The scroll offset that brings the playing episode into view is arithmetic over these, so the
/// list opens where the viewer is instead of at episode one.
pub(crate) const EPISODE_ROW_HEIGHT: f32 = 34.0;
pub(crate) const EPISODE_ROW_GAP: f32 = 2.0;
pub(crate) const EPISODE_MENU_HEIGHT: f32 = 320.0;
/// The title row above the list plus the menu's own padding.
pub(crate) const EPISODE_HEADER_HEIGHT: f32 = 26.0;

/// How long the player's controls stay up after the pointer moves, in full screen.
pub(crate) const CONTROLS_LINGER: Duration = Duration::from_millis(2400);

/// Placeholder rows so `--snap` can check the layout without a network round trip. Only
/// reachable from a named scene, never from the running app.
pub(crate) const DEMO_TITLES: [&str; 8] = [
    "【4K】这可能是今年最好看的旅行短片，一次看遍北海道",
    "用 Rust 从零写一个窗口管理器｜第一期：到底难在哪",
    "深夜食堂：一个人也要好好吃饭",
    "钢琴改编《千本樱》，手指快到看不清",
    "拆解一台二十年前的随身听，里面比想象中复杂",
    "3 小时讲透线性代数，看完你就懂了",
    "在海拔 5000 米看星空，是什么体验",
    "手工复原宋代点茶，全过程",
];

pub(crate) const DEMO_UPLOADERS: [&str; 8] = [
    "影视飓风",
    "Rust 语言中文社区",
    "曼食慢语",
    "指尖琴键",
    "硬件茶谈",
    "3Blue1Brown",
    "星球研究所",
    "南翔不爱吃饭",
];

pub(crate) const DEMO_ACCENTS: [u32; 6] =
    [0x3e4654, 0x39455a, 0x4b4240, 0x4c4255, 0x354c4a, 0x46503d];
