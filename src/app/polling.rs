//! The frame loop: draining events, keeping the player honest, paging the feed.

pub(crate) use super::*;

/// Runs at the start of every frame. Returns true only when something visibly changed, so an
/// idle window draws nothing and arms no timer.
pub(crate) fn poll(app: &mut BiliGuga, _seconds: f64, tx: &mut Tx) -> bool {
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
    //
    // Asking is not a visible change and must not report one: `poll` returning true commits the
    // view, and a commit in the middle of a landing re-runs the layout the landing is measuring.
    // The page's own arrival reports the change instead, a round trip later.
    app.page_home_if_near_end();

    while let Ok(event) = app.events.try_recv() {
        // Only an event that changes what is on screen is a redraw: `Silent` and
        // `HistoryReported` exist precisely because the work behind them is not something the
        // viewer can see, and reporting them would commit a view for a frame that looks
        // exactly like the last one.
        changed |= app.apply(event);
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
        // The one seek a stream gets: apply it as soon as the stream knows its own duration,
        // which is what says whether the position is somewhere inside it.
        if let Resume::To(position) = app.resume
            && status.duration.is_finite()
            && status.duration > 0.
        {
            if position < status.duration - 3. {
                app.player.seek_seconds(position);
            }
            app.resume = Resume::Nothing;
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
    /// Takes one answer from background work and folds it into the state, saying whether what
    /// is on screen changed.
    ///
    /// This was the body of `poll`'s drain loop, which made every case reachable only
    /// through a frame. It is a plain method so a test can drive one event. The boolean is what
    /// keeps a frame that only carried bookkeeping from committing a view: the drain loop used
    /// to treat every event as a change, including the two that are explicitly not.
    pub(crate) fn apply(&mut self, event: Event) -> bool {
        let app = self;
        match event {
            Event::Home(result, generation) => {
                if generation != app.home_generation || app.active_tab != AppTab::Home {
                    return false;
                }
                app.home_loading = false;
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
                        app.rows_changed();
                        app.request_missing_covers();
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
                app.search_loading = false;
                match result {
                    Ok(videos) => {
                        app.search_results = videos;
                        app.selected = None;
                        app.rows_changed();
                        app.request_missing_covers();
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
                    return false;
                }
                app.author_loading = false;
                match result {
                    Ok(page) => {
                        app.author_page = page.page;
                        app.author_has_more = page.has_more;
                        app.author_videos.extend(page.videos);
                        app.rows_changed();
                        app.request_missing_covers();
                    }
                    Err(error) => app.feed_error = Some(error),
                }
            }
            Event::VideoContext(context, generation) => {
                if generation != app.playback_generation {
                    return false;
                }
                app.stats = Some(VideoStats {
                    views: context.views,
                    danmaku: context.danmaku,
                    likes: context.likes,
                    published: context.pubdate,
                });
                app.collection = context.collection;
                if let Some(video) = app.playing_video.as_mut() {
                    let video = Arc::make_mut(video);
                    if video.aid == 0 {
                        video.aid = context.aid;
                    }
                    if video.cid == 0 {
                        video.cid = context.cid;
                    }
                    // A collection episode carries the collection's owner, which is not always
                    // the uploader of this part; the detail endpoint knows the right one.
                    if !context.uploader.is_empty() {
                        video.uploader = context.uploader;
                        video.uploader_mid = context.uploader_mid;
                    }
                }
            }
            Event::PlayUrl(result, generation) => {
                if generation != app.playback_generation {
                    return false;
                }
                match result {
                    Ok(play) => app.finish_play(play),
                    Err(error) => {
                        app.playback = PlaybackState::Idle;
                        app.playback_error = Some(error);
                    }
                }
            }
            Event::Resume(progress, generation) => {
                if generation != app.playback_generation {
                    return false;
                }
                // Answered after the viewer already switched quality: their own playhead wins.
                app.resume = match (app.resume, progress) {
                    (Resume::To(position), _) => Resume::To(position),
                    (_, Some(position)) if position > 3. => Resume::To(position),
                    _ => Resume::Nothing,
                };
            }
            Event::Comments(result, generation) => {
                if generation != app.comments_generation {
                    return false;
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
                    return false;
                }
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
                    return false;
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
                return false;
            }
            Event::HistoryReported => {
                app.history_report_in_flight = false;
                return false;
            }
            Event::Silent => return false,
            Event::Cover(bvid, pixels) => app.attach_cover(bvid, pixels),
        }
        true
    }
}

