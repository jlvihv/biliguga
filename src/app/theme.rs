//! The app's colours, in Rune's linear-sRGB `Color`.

/// The app's colours, in Rune's linear-sRGB `Color`.
///
/// These used to be functions calling `oklch(..)` on every access, so the OKLCH to linear-sRGB
/// conversion ran thousands of times a frame (each `theme::HOVER` in each row) and produced the
/// same numbers every time. They are the same colours, precomputed; the comment on each is the
/// sRGB hex it came from, so a designer can still edit one and rerun the conversion.
///
/// The palette is one hue family (a cool near-black) with a single warm accent, so the eye lands
/// on the poster art and the one thing that is clickable rather than on the chrome.
use rune::Color;

const fn c(r: f32, g: f32, b: f32) -> Color {
    Color { r, g, b, a: 1.0 }
}

/// #0A0B0F — the page behind everything.
pub(crate) const BG: Color = c(0.003_035, 0.003_347, 0.004_777);
/// #111319 — the player pane and control bars.
pub(crate) const PANEL: Color = c(0.005_605, 0.006_512, 0.009_721);
/// #16181F — anything raised: cards, fields, chips.
pub(crate) const CARD: Color = c(0.008_023, 0.009_134, 0.013_702);
/// #1B1E27 — an input well, a shade above a card so a field reads as editable.
pub(crate) const FIELD: Color = c(0.010_960, 0.012_983, 0.019_382);
/// #1E212B — a card under the pointer.
pub(crate) const CARD_HOVER: Color = c(0.012_983, 0.015_209, 0.024_158);
/// #23262F — a visible edge, for fields and the split between panes.
pub(crate) const BORDER: Color = c(0.016_807, 0.019_382, 0.028_426);
/// #191C24 — a divider that only needs to be sensed.
pub(crate) const HAIRLINE: Color = c(0.009_721, 0.011_612, 0.017_642);
/// #F1F3F7
pub(crate) const TEXT: Color = c(0.879_622, 0.896_269, 0.930_111);
/// #A3ABBA — secondary: uploaders, counts, metadata.
pub(crate) const TEXT_DIM: Color = c(0.366_253, 0.407_240, 0.491_021);
/// #6E7686 — tertiary: labels, hints, placeholders.
pub(crate) const TEXT_FAINT: Color = c(0.155_926, 0.181_164, 0.238_398);
/// #FB7299 — the one accent: the play button, the active tab, the seek fill.
pub(crate) const ACCENT: Color = c(0.964_686, 0.168_269, 0.318_547);
/// #FF9CBE — the accent as text, which has to be lighter to stay readable.
pub(crate) const ACCENT_SOFT: Color = c(1.0, 0.332_452, 0.514_918);
/// #3A1B25 — the accent, nearly black, for a selected row's wash.
pub(crate) const ACCENT_DIM: Color = c(0.042_311, 0.010_960, 0.018_500);
/// #FF6B6B
pub(crate) const DANGER: Color = c(1.0, 0.147_027, 0.147_027);
/// #2A2E39 — the unfilled half of a slider.
pub(crate) const TRACK: Color = c(0.023_153, 0.027_321, 0.040_915);
/// The pointer resting on something that can be pressed.
pub(crate) const HOVER: Color = c(0.008_023, 0.009_134, 0.014_000);
/// The pointer holding it down.
pub(crate) const PRESS: Color = c(0.013_000, 0.015_209, 0.024_158);

/// The placeholder a cover shows before its pixels arrive: the row's own accent, which
/// `api` assigns per position so a feed of half-loaded covers still looks intentional.
pub(crate) fn cover_placeholder(accent: u32) -> Color {
    rune::color::hex(&format!("#{accent:06x}")).unwrap_or(Color::BLACK)
}
