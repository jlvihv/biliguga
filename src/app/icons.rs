//! The glyphs the rail and the cards are drawn with.

/// Our own icon set, drawn as SVG bodies rather than borrowed from the text font.
///
/// A house, a play mark and a bookmark all exist in whatever font happens to be installed, at
/// different weights and sometimes not at all; a column of mismatched shapes is exactly what a
/// sidebar looks like when it borrows glyphs. These are 16-point bodies in a 16-point box,
/// stroked in `currentColor`, and drawn through Rune's icon cache so one body rasterises once.
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
