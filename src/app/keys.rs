//! The keys the window handles itself.

pub(crate) use super::*;

/// Keys the window handles itself. The transport keys only count while something is open:
/// with nothing playing, Space used to start a "playback" of nothing, and it swallowed the
/// key a reader might be using for something else.
pub(crate) fn on_key(app: &mut BiliGuga, event: &KeyEvent, tx: &mut Tx) -> bool {
    let mods = event.mods;
    let watching = app.playing_video.is_some();
    match event.key {
        Key::Text(ref typed) if typed == " " && !mods.ctrl && !mods.alt && watching => {
            app.toggle_pause();
            true
        }
        Key::Left if watching => {
            app.player.seek_relative(-10.);
            true
        }
        Key::Right if watching => {
            app.player.seek_relative(10.);
            true
        }
        Key::Up if watching => {
            app.volume = (app.volume + 5.).clamp(0., 100.);
            app.player.set_volume(app.volume);
            app.persist_player_settings();
            true
        }
        Key::Down if watching => {
            app.volume = (app.volume - 5.).clamp(0., 100.);
            app.player.set_volume(app.volume);
            app.persist_player_settings();
            true
        }
        Key::Text(ref typed)
            if typed.eq_ignore_ascii_case("f") && !mods.ctrl && !mods.alt && watching =>
        {
            app.player_fullscreen = !app.player_fullscreen;
            tx.toggle_full_screen();
            true
        }
        Key::Escape => {
            if app.player_fullscreen {
                app.player_fullscreen = false;
                tx.toggle_full_screen();
                return true;
            }
            // Windowed, Escape is the way out of the player: back to the browse grid.
            if watching {
                app.close_player();
                return true;
            }
            false
        }
        Key::Enter if mods.ctrl || mods.alt => {
            app.load_search();
            true
        }
        _ => false,
    }
}

impl BiliGuga {
    /// The video the player is on, with the ids its action endpoints need.
    pub(crate) fn current_action_target(&self) -> Option<Video> {
        self.playing_video.clone()
    }

    pub(crate) fn like_current(&mut self) {
        let (Some(cookie), Some(video)) = (self.cookie(), self.current_action_target()) else {
            return;
        };
        if video.aid <= 0 {
            return;
        }
        self.action_note = Some("正在点赞…".into());
        self.worker().spawn(
            async move { like_video(&cookie, video.aid).await },
            |result| Event::ActionDone("已点赞", result),
        );
    }

    pub(crate) fn coin_current(&mut self) {
        let (Some(cookie), Some(video)) = (self.cookie(), self.current_action_target()) else {
            return;
        };
        if video.aid <= 0 {
            return;
        }
        self.action_note = Some("正在投币…".into());
        self.worker().spawn(
            async move { coin_video(&cookie, video.aid).await },
            |result| Event::ActionDone("已投币", result),
        );
    }

    pub(crate) fn save_to_watch_later(&mut self) {
        let (Some(cookie), Some(video)) = (self.cookie(), self.current_action_target()) else {
            return;
        };
        if video.aid <= 0 {
            return;
        }
        self.worker().spawn(
            async move { add_to_watch_later(&cookie, video.aid).await },
            |result| Event::ActionDone("已加入稍后再看", result),
        );
    }

    pub(crate) fn save_to_favorites(&mut self) {
        let (Some(session), Some(video)) = (self.session.clone(), self.current_action_target())
        else {
            return;
        };
        if video.aid <= 0 {
            return;
        }
        let (cookie, mid) = (session.cookie, session.mid);
        self.worker().spawn(
            async move { add_to_favorites(&cookie, mid, video.aid).await },
            |result| Event::ActionDone("已收藏", result),
        );
    }

    /// Jump to the next or previous episode of the collection the player is in.
    pub(crate) fn play_collection_step(&mut self, forward: bool) {
        let Some(collection) = &self.collection else {
            return;
        };
        let Some(current) = &self.playing_video else {
            return;
        };
        let index = collection
            .episodes
            .iter()
            .position(|episode| episode.bvid == current.bvid);
        let target = match (index, forward) {
            (Some(index), true) => collection.episodes.get(index + 1),
            (Some(index), false) if index > 0 => collection.episodes.get(index - 1),
            _ => None,
        };
        if let Some(episode) = target.cloned() {
            self.play_video(episode);
        }
    }
}
