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

/// The seek bar: presses and drags anywhere on the track jump the video there. The hit area is
/// taller than the drawn line, so it is easy to grab.
pub(crate) fn seek_bar(position: f64, duration: f64) -> El {
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

pub(crate) fn volume_bar(volume: f64, width: f32) -> El {
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
pub(crate) fn collection_marker(app: &BiliGuga) -> El {
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
pub(crate) fn render_stage(app: &BiliGuga, size: Option<(f32, f32)>) -> El {
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
/// One row for both layouts: the windowed player owns the window beside the rail, and the
/// full-screen strip floats over the picture — same width to spend either way.
pub(crate) fn render_transport(app: &BiliGuga) -> El {
    let playing = app.playback == PlaybackState::Playing;
    let quality = icon_button("quality", &icons::QUALITY, "画质").on_press(|app: &mut BiliGuga| {
        app.open_menu = if app.open_menu == Some(PlayerMenu::Quality) {
            None
        } else {
            Some(PlayerMenu::Quality)
        };
    });
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
        .child(collection_marker(app))
        .child(
            quality.child(
                text(crate::api::quality_label(app.quality))
                    .font(TextStyle::new(11.0, 500.0))
                    .color(theme::TEXT_FAINT)
                    .single_line(),
            ),
        )
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
            pill("speed", None, &format!("{:.2}x", app.speed), Tone::Quiet)
                .h(28.0)
                .px(8.0)
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
pub(crate) fn render_actions(app: &BiliGuga) -> El {
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

/// The player column while a video is open: the stage in its 16:9 block, then one card holding
/// everything that acts on it, then the comments. It scrolls as a whole, because a player
/// column that pinned its controls would leave the comments in a 150-point slot.
pub(crate) fn render_player(app: &BiliGuga, video: &Video) -> El {
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
    // The windowed layout always shows its controls: there is nothing underneath to reveal
    // them over, and hiding them would only hide the way to act on the video.
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
                .child(render_transport(app)),
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

    // No side border: the rail already draws the hairline the player sits against.
    let column = col()
        .key("player")
        .grow(1.0)
        .h_full()
        .min_w(0.0)
        .min_h(0.0)
        .fill(theme::PANEL);
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
pub(crate) fn render_player_fullscreen(app: &BiliGuga, video: &Video) -> El {
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
                .child(render_transport(app)),
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
