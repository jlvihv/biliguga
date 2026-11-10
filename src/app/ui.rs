//! The pieces every pane draws with: pills, buttons, glyphs and poster art.

pub(crate) use super::*;

/// A 16-point glyph in `color`, laid out inline.
pub(crate) fn glyph(icon: &'static rune::Icon, color: Color, size: f32) -> El {
    rune::icon(icon, size).color(color)
}

/// One line of text that is allowed to run out of room rather than wrap.
pub(crate) fn label(content: impl Into<String>, size: f32, weight: f32, color: Color) -> El {
    text(content)
        .font(TextStyle::new(size, weight))
        .color(color)
        .single_line()
        .truncate()
}

/// The "this is the current one" wash. A tint and a bar down the leading edge rather than a
/// ring: a border on a poster card reads as a focus ring, which is not what this is.
pub(crate) fn selection_wash(el: El) -> El {
    el.fill(theme::ACCENT_DIM.alpha(0.7))
        .hover(theme::ACCENT_DIM)
        .press(theme::ACCENT_DIM)
        .border_sides([0.0, 0.0, 0.0, 2.0], theme::ACCENT)
}

/// A pill button with an icon and a label, the shape used for every action in the app.
pub(crate) fn pill(
    key: &'static str,
    icon: Option<&'static rune::Icon>,
    caption: &str,
    tone: Tone,
) -> El {
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
pub(crate) enum Tone {
    /// No fill: secondary actions that sit in a row of their own.
    Quiet,
    /// A card-coloured chip with a border: the default for a real button.
    Raised,
    /// The accent, filled: the one primary action on a screen.
    Accent,
}

/// A flat square button for the player's control strip.
pub(crate) fn icon_button(key: &'static str, icon: &'static rune::Icon, caption: &str) -> El {
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
            .single_line()
            // Truncates rather than overflows, for the callers that bound it: the episode chip's
            // caption is a collection title, and a title with no room left has to end in an
            // ellipsis inside the chip instead of running over the controls beside it.
            .truncate()
            .grow(1.0),
    );
    body
}

/// A 34-point square icon-only button (window chrome, quiet round controls).
pub(crate) fn square_button(
    key: &'static str,
    icon: &'static rune::Icon,
    help: &'static str,
) -> El {
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
pub(crate) fn poster(
    app: &BiliGuga,
    video: &Video,
    width: f32,
    height: f32,
    radius: f32,
    badge: bool,
) -> El {
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

/// A one-point divider across the pane, the hairline that separates a header from its content.
/// The feed, the login panel and the player's own top bar each drew this as the same three
/// calls.
pub(crate) fn hairline() -> El {
    el().w_full().h(1.0).shrink().fill(theme::HAIRLINE)
}

/// How tall one grid row is for a card `width` points wide: the 16:9 cover plus the caption
/// under it. The virtual list's estimate and the paging arithmetic both read this, so the two
/// cannot disagree about where a row ends.
pub(crate) fn grid_row_height(width: f32) -> f32 {
    (width * 9.0 / 16.0 + CARD_CAPTION_HEIGHT).round()
}

/// A count the way the detail strip shows it: "1.2万", "128.4万", "9.8亿". A plain number with
/// no unit until it is worth one, so a new upload does not read as "0.0万".
pub(crate) fn count_label(value: i64) -> String {
    if value >= 100_000_000 {
        format!("{:.1}亿", value as f64 / 100_000_000.0)
    } else if value >= 10_000 {
        format!("{:.1}万", value as f64 / 10_000.0)
    } else {
        value.to_string()
    }
}
