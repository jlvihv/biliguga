//! The player: the stage, transport, actions, detail and comments.

pub(crate) use super::*;

/// A thin track with a filled portion, used by the seek bar and the volume slider.
pub(crate) fn track(fraction: f32, height: f32, fill: Color) -> El {
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

/// The playhead and the length, cleaned up for the bar: a stream that has not reported either
/// yet gives `0.0`, and the position never reads past the end. Both layouts asked for this the
/// same way, in two copies of six lines.
pub(crate) fn clock_of(status: &mpv::MpvStatus) -> (f64, f64) {
    let duration = if status.duration.is_finite() && status.duration > 0.0 {
        status.duration
    } else {
        0.0
    };
    let position = if status.time_pos.is_finite() {
        status.time_pos.clamp(0.0, duration)
    } else {
        0.0
    };
    (position, duration)
}

/// Where in the track a drag is, as a fraction: `drag.x` is the pointer in the element's own
/// points, so a press at 30% is 30%. The two sliders computed this the same way.
pub(crate) fn drag_fraction(drag: &Drag) -> f32 {
    if drag.w > 0.0 {
        (drag.x / drag.w).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The seek bar: presses and drags anywhere on the track jump the video there. The hit area is
/// taller than the drawn line, so it is easy to grab.
/// A clock, in tabular figures and a width of its own.
///
/// Both matter while it is ticking. `'tnum' 1` gives every digit the same advance, so `11:11`
/// is the same width as `00:00` and the digits do not shuffle as the seconds count; the width
/// means the label does not resize with its glyphs at all, and the text is right-aligned inside
/// it so the digits stay put.
///
/// The width follows the shape: `mm:ss` is five characters, and once the clip is an hour long
/// so is `hh:mm:ss`.
pub(crate) fn clock_label(seconds: f64, with_hours: bool, color: Color) -> El {
    let clock = Clock::new(seconds, with_hours);
    let width = if with_hours {
        CLOCK_WIDTH_HOURS
    } else {
        CLOCK_WIDTH
    };
    row().w(width).shrink().justify_end().child(
        text(clock.as_str())
            .font(TextStyle::new(11.5, 500.0).features("'tnum' 1"))
            .color(color)
            .single_line(),
    )
}

pub(crate) fn seek_bar(position: f64, duration: f64) -> El {
    // One answer for both labels: the playhead's shape has to be the length's, or the two ends
    // of the bar would disagree about their width the moment the video crossed an hour.
    let with_hours = Clock::needs_hours(duration);
    row()
        .key("seek-row")
        .w_full()
        .gap(10.0)
        .items_center()
        .child(clock_label(position, with_hours, theme::TEXT_DIM))
        .child(seek_track(position, duration))
        .child(clock_label(duration, with_hours, theme::TEXT_FAINT))
}

/// The track itself: presses and drags anywhere on it jump the video there. The hit area is
/// taller than the drawn line, so it is easy to grab.
pub(crate) fn seek_track(position: f64, duration: f64) -> El {
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
            app.seek_percent(drag_fraction(drag) as f64);
            app.seek_dragging = drag.phase != Phase::Up;
            app.show_controls();
        })
        .child(track(fraction, 4.0, theme::ACCENT))
}

pub(crate) fn volume_bar(volume: f64, width: f32) -> El {
    col()
        .key("volume-bar")
        .w(width)
        .h(18.0)
        .justify_center()
        .cursor(rune::Cursor::Pointer)
        .help("音量")
        .on_drag(move |app: &mut BiliGuga, drag: &mut Drag, _: &mut Tx| {
            app.volume = (drag_fraction(drag) as f64 * 100.0).clamp(0.0, 100.0);
            app.player.set_volume(app.volume);
            app.volume_dragging = drag.phase != Phase::Up;
            if !app.volume_dragging {
                app.persist_player_settings();
            }
            app.show_controls();
        })
        .child(track((volume / 100.0) as f32, 4.0, theme::TEXT_DIM))
}

/// Which episode of which collection is playing, as a button that opens the episode list.
///
/// It only exists when the clip has siblings: a single video has no list to open, and a chip
/// that did nothing would be a control that lies about what it does.
pub(crate) fn collection_marker(app: &BiliGuga) -> El {
    let Some(collection) = &app.collection else {
        return el();
    };
    let position = current_episode_index(app);
    let text = match position {
        Some(index) => format!(
            "{} · {}/{}",
            collection.title,
            index + 1,
            collection.episodes.len()
        ),
        None => collection.title.clone(),
    };
    // Keyed by the collection, so switching episodes keeps this label's identity (and its
    // place in the transport row) instead of looking like a new element every time.
    let open = app.episode_menu_open;
    icon_button("episodes", &icons::TV, &text)
        .key(format!("collection-{}", collection.id))
        // A fixed width, and the title inside it truncated: this label is the one thing in the
        // transport row whose text comes from the clip, so without a bound a long collection
        // name would push the quality, volume and speed controls off the end — and the row
        // would resize itself again when the episode number went from `9/12` to `10/12`.
        .w(EPISODE_CHIP_WIDTH)
        .max_w(EPISODE_CHIP_WIDTH)
        .fill(if open {
            theme::CARD_HOVER
        } else {
            Color::TRANSPARENT
        })
        .help("选集")
        .on_press(|app: &mut BiliGuga| {
            app.episode_menu_open = !app.episode_menu_open;
            app.quality_menu_open = false;
            // Opening the list is using the controls: keep them up while it is open.
            app.show_controls();
            // The list opens on the episode that is playing, not on the first one: at episode
            // 50 of 120 the viewer is looking for their place in it. The scroll itself happens
            // on the list's first layout — see `episode_scroll_pending`.
            app.episode_scroll_pending = app.episode_menu_open;
        })
}

/// The index of the episode playing, within its collection.
pub(crate) fn current_episode_index(app: &BiliGuga) -> Option<usize> {
    let collection = app.collection.as_ref()?;
    let playing = app.playing_video.as_ref()?;
    collection
        .episodes
        .iter()
        .position(|episode| episode.bvid == playing.bvid)
}

/// How far the episode list has to be scrolled to bring the playing episode into view.
///
/// Computed rather than measured: each row is a constant [`EPISODE_ROW_HEIGHT`] plus the gap,
/// so the offset is arithmetic and needs no layout pass — a scroll asked for during the press
/// would otherwise be clamped against a viewport that does not exist yet. The list is centred
/// on the playing row where it can be, so the episodes before and after it are both visible.
pub(crate) fn episode_scroll_offset(app: &BiliGuga) -> f32 {
    let Some(index) = current_episode_index(app) else {
        return 0.0;
    };
    let row = EPISODE_ROW_HEIGHT + EPISODE_ROW_GAP;
    let playing_centre = index as f32 * row + EPISODE_ROW_HEIGHT / 2.0;
    let viewport = EPISODE_MENU_HEIGHT - EPISODE_HEADER_HEIGHT;
    (playing_centre - viewport / 2.0).max(0.0)
}

/// The episode list, scrolled to the playing one and marking it. Same shape as the quality
/// menu: a card of rows, the current one in the accent, the rest plain.
pub(crate) fn render_episode_menu(app: &BiliGuga) -> El {
    let Some(collection) = &app.collection else {
        return el();
    };
    let playing = current_episode_index(app);
    // The header stays put; the rows scroll under it, so the list can hold a series of any
    // length inside a 320-point menu.
    let rows = col()
        .key("episode-rows")
        .w_full()
        .gap(EPISODE_ROW_GAP)
        .children(collection.episodes.iter().enumerate().map(|(index, episode)| {
            let is_current = playing == Some(index);
            // The playing one is washed and its number in the accent: the list opens on the
            // episode the viewer is on, and this is what says which that is.
            row()
                .key(format!("episode-{}-{}", collection.id, episode.bvid))
                .w_full()
                .h(EPISODE_ROW_HEIGHT)
                .px(8.0)
                .gap(8.0)
                .items_center()
                .radius(7.0)
                .cursor(rune::Cursor::Pointer)
                .hover(theme::CARD_HOVER)
                .fill(if is_current {
                    theme::ACCENT_DIM
                } else {
                    Color::TRANSPARENT
                })
                .on_press(move |app: &mut BiliGuga| {
                    if !is_current {
                        app.play_episode_at(index);
                    }
                })
                .child(
                    text((index + 1).to_string())
                        .font(TextStyle::new(11.5, 600.0))
                        .color(if is_current {
                            theme::ACCENT_SOFT
                        } else {
                            theme::TEXT_FAINT
                        })
                        .w(22.0)
                        .single_line(),
                )
                .child(
                    text(&episode.title)
                        .font(TextStyle::new(12.0, if is_current { 600.0 } else { 400.0 }))
                        .color(if is_current {
                            theme::ACCENT_SOFT
                        } else {
                            theme::TEXT
                        })
                        .single_line()
                        .truncate()
                        .grow(1.0),
                )
                .child(if is_current {
                    glyph(&icons::PLAY, theme::ACCENT, 11.0)
                } else {
                    el()
                })
        }));
    // A fixed height rather than `max_h`: the row list is the only thing that may scroll, and a
    // `grow` inside a box whose height is decided by its content reserves no room to scroll in.
    // Twelve episodes fit; a longer series scrolls.
    col()
        .key("episode-menu")
        .w(320.0)
        .h(EPISODE_MENU_HEIGHT)
        .shrink()
        .padding(6.0)
        .gap(4.0)
        .radius(12.0)
        .fill(theme::CARD)
        .border(1.0, theme::BORDER)
        .shadow(10.0, 32.0, Color::BLACK.alpha(0.45))
        .child(
            row()
                .w_full()
                .px(8.0)
                .gap(6.0)
                .items_center()
                .child(
                    text(&collection.title)
                        .font(TextStyle::new(12.0, 600.0))
                        .color(theme::TEXT_DIM)
                        .single_line()
                        .truncate()
                        .grow(1.0),
                )
                .child(
                    text(format!("{} 集", collection.episodes.len()))
                        .font(TextStyle::new(10.5, 500.0))
                        .color(theme::TEXT_FAINT),
                ),
        )
        .child(
            col()
                .key("episode-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(rows),
        )
}

/// The video stage: the frame letterboxed on black, or a message until one arrives.
///
/// The picture runs edge to edge on all four sides — no margin, no rounding, no border — and the
/// controls are pinned to the bottom *inside* it, over the black of the frame's letterbox or the
/// picture itself, with no fill of their own.
pub(crate) fn render_stage(app: &BiliGuga, fullscreen: bool) -> El {
    let mut stage = col()
        .key("player-stage")
        .items_center()
        .justify_center()
        .fill(Color::BLACK)
        .on_pointer_move(|app: &mut BiliGuga, _| app.show_controls())
        // Where the controls' visibility comes from: they rise while the pointer is over the
        // picture and go once it leaves, in both layouts. `on_hover_change` counts descendants,
        // so moving from the picture onto the seek bar is not a leave, and a press on a slider
        // keeps the pointer inside until it is released.
        .on_hover_change(|app: &mut BiliGuga, inside, _| app.pointer_over_stage(inside));
    stage = if fullscreen {
        // Everything, edge to edge.
        stage.grow(1.0).w_full().h_full().min_h(0.0)
    } else {
        // The window's width, and the clip's own shape for the height. The layout stretches the
        // width and the aspect ratio derives the height, so a resize is right on the frame it
        // happens; nothing else on the page has to know the picture's size.
        stage.w_full().aspect_ratio(app.video_aspect())
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
    // Both layouts auto-hide: the controls sit over the picture now, so a still pointer leaves
    // them nothing to do but cover the video. A press, the quality menu or a paused picture
    // holds them — see `controls_visible`.
    if app.controls_visible() {
        stage = stage.child(controls_overlay(app));
    }
    stage
}

/// The episode button, with its list hanging above it when the list is open.
///
/// The popup is a child of the button's own box, pinned to the box's top-right, so it opens
/// where the button is — a menu at the far corner of the stage would be a menu the viewer has
/// to hunt for. `bottom: 34` is the button's own height, so the list sits just clear of it.
fn episode_anchor(app: &BiliGuga) -> El {
    let mut anchor = row().key("episode-anchor").shrink().items_center();
    anchor = anchor.child(collection_marker(app));
    if app.episode_menu_open && app.collection.is_some() {
        anchor = anchor.child(
            el().pin(None, None, Some(0.0), Some(36.0))
                .shrink()
                .child(render_episode_menu(app)),
        );
    }
    anchor
}

/// The quality button, with its menu in the same place relative to it.
fn quality_anchor(app: &BiliGuga) -> El {
    let mut anchor = row().key("quality-anchor").shrink().items_center();
    anchor = anchor.child(
        icon_button("quality", &icons::QUALITY, "画质").on_press(|app: &mut BiliGuga| {
            app.quality_menu_open = !app.quality_menu_open;
            // One popup at a time: both hang over the picture.
            app.episode_menu_open = false;
            app.show_controls();
        })
            .child(
                // The labels run from `8K` to `1080P 高码率`, so this is the widest of them
                // plus a little: a row that re-flows every time the quality changes would move
                // the volume slider under the pointer.
                text(crate::api::quality_label(app.quality))
                    .font(TextStyle::new(11.0, 500.0))
                    .color(theme::TEXT_FAINT)
                    .single_line()
                    .w(QUALITY_WIDTH)
                    .truncate(),
            ),
    );
    if app.quality_menu_open && !app.quality_options.is_empty() {
        anchor = anchor.child(
            el().pin(None, None, Some(0.0), Some(36.0))
                .shrink()
                .child(render_quality_menu(app)),
        );
    }
    anchor
}

/// The player's own controls, laid over the bottom of the picture: the seek bar and the
/// transport row. No fill behind them, in either layout — the video is the background.
///
/// They are pinned inside the stage rather than placed under it, so the picture owns the whole
/// window and the controls sit on its lower edge the way a full-screen player's do. The windowed
/// player used to put them in a card below the stage, which is what pushed the video down and
/// left a panel-coloured band between the two.
fn controls_overlay(app: &BiliGuga) -> El {
    let (position, duration) = clock_of(&app.player.status());
    col()
        .key("player-controls")
        .pin(Some(0.0), None, Some(0.0), Some(0.0))
        .w_full()
        .gap(8.0)
        .px(PANE_PAD)
        .pb(10.0)
        .child(seek_bar(position, duration))
        .child(render_transport(app))
}

/// The transport bar: the play button, then the switches that change how it plays.
///
/// One row for both layouts: the windowed player owns the window beside the rail, and the
/// full-screen strip floats over the picture — same width to spend either way.
pub(crate) fn render_transport(app: &BiliGuga) -> El {
    let playing = app.playback == PlaybackState::Playing;
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
        .child(episode_anchor(app))
        .child(quality_anchor(app))
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
        .child(volume_bar(app.volume, 96.0))
        .child(
            // A fixed width for the same reason as the clock: `0.50x` through `2.00x` are all
            // five characters, and a width that follows the glyphs lets the row shift when the
            // rate changes.
            pill("speed", None, &format!("{:.2}x", app.speed), Tone::Quiet)
                .h(28.0)
                .px(8.0)
                .w(SPEED_WIDTH)
                .justify_center()
                .help("倍速")
                .on_press(|app: &mut BiliGuga| app.cycle_speed()),
        )
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
pub(crate) fn render_actions() -> El {
    row()
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
        )
}

/// The quality popup, drawn under the transport bar while it is open.
pub(crate) fn render_quality_menu(app: &BiliGuga) -> El {
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
/// title, then who made it, then the counts. The player has the whole window beside the rail
/// now, but the title is the one thing here that grows, so it gets a line of its own.
pub(crate) fn render_detail(app: &BiliGuga, video: &Video) -> El {
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
pub(crate) fn render_detail_stats(app: &BiliGuga) -> El {
    let Some(stats) = &app.stats else {
        return el();
    };
    row().gap(12.0).items_center().shrink().children(
        [
            ("播放", count_label(stats.views)),
            ("弹幕", count_label(stats.danmaku)),
            ("点赞", count_label(stats.likes)),
            ("发布", api::format_publish_date(stats.published)),
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

pub(crate) fn render_comments(app: &BiliGuga) -> El {
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

/// The player column while a video is open: the picture edge to edge with its controls inside
/// it, then what acts on it, then the comments. It scrolls as a whole, because a player column
/// that pinned its controls would leave the comments in a 150-point slot.
pub(crate) fn render_player(app: &BiliGuga, video: &Video) -> El {
    // The picture is the first thing in the column and has no margin: it starts at the pane's
    // top-left corner. Everything below it is inset by `PANE_PAD`.
    let stage = col()
        .key("stage-block")
        .w_full()
        .child(render_stage(app, false));
    let below = col()
        .w_full()
        .px(PANE_PAD)
        .pt(10.0)
        .gap(10.0)
        .child(
            row()
                .w_full()
                .gap(6.0)
                .items_center()
                .child(render_actions())
                .child(el().grow(1.0))
                .child(
                    pill("close-player", Some(&icons::CLOSE), "关闭", Tone::Quiet)
                        .on_press(|app: &mut BiliGuga| app.close_player()),
                ),
        )
        .child(render_detail(app, video))
        .child(render_comments(app));

    col()
        .key("player")
        .grow(1.0)
        .h_full()
        .min_w(0.0)
        .min_h(0.0)
        .fill(theme::PANEL)
        .child(
            col()
                .key("player-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(col().w_full().child(stage).child(below)),
        )
}

/// The player column while the video is in full screen: the picture, and the title it shows
/// while the controls are away.
pub(crate) fn render_player_fullscreen(app: &BiliGuga, video: &Video) -> El {
    // The picture keeps the whole window and its controls ride inside its bottom edge (see
    // `render_stage`), so showing them never resizes the video or re-times mpv's decode.
    let mut root = col()
        .key("player")
        .w_full()
        .h_full()
        .fill(Color::BLACK)
        .child(render_stage(app, true));
    if !app.controls_visible() {
        // With the controls gone this is what still says what is playing. No fill behind it
        // either: the picture is the background everywhere on this screen.
        root = root.child(
            row()
                .pin(Some(0.0), Some(0.0), Some(0.0), None)
                .h(44.0)
                .px(20.0)
                .items_center()
                .child(
                    text(&video.title)
                        .font(TextStyle::new(12.5, 500.0))
                        .color(Color::WHITE.alpha(0.78))
                        .truncate(),
                ),
        );
    }
    root
}
