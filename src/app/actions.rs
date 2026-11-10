//! What the viewer can do *to* a video: like, coin, favourite, add to watch later, step through
//! a collection. These lived in `keys` next to the keyboard handler, which made a module about
//! input also the module about requests; the keys only call them.

pub(crate) use super::*;

impl BiliGuga {
    /// The ids the action endpoints need, if a video is open and its detail call resolved them.
    ///
    /// Borrowed, not cloned: the three actions below only read `aid`, and copying a whole
    /// `Video` (six `String`s) to read one integer was the shape this replaced.
    fn action_target(&self) -> Option<(String, i64)> {
        let video = self.playing_video.as_ref()?;
        if video.aid <= 0 {
            return None;
        }
        Some((self.cookie()?, video.aid))
    }

    pub(crate) fn like_current(&mut self) {
        let Some((cookie, aid)) = self.action_target() else {
            return;
        };
        self.action_note = Some("正在点赞…".into());
        self.worker().spawn(
            async move { like_video(&cookie, aid).await },
            |result| Event::ActionDone("已点赞", result),
        );
    }

    pub(crate) fn coin_current(&mut self) {
        let Some((cookie, aid)) = self.action_target() else {
            return;
        };
        self.action_note = Some("正在投币…".into());
        self.worker().spawn(
            async move { coin_video(&cookie, aid).await },
            |result| Event::ActionDone("已投币", result),
        );
    }

    pub(crate) fn save_to_watch_later(&mut self) {
        let Some((cookie, aid)) = self.action_target() else {
            return;
        };
        self.worker().spawn(
            async move { add_to_watch_later(&cookie, aid).await },
            |result| Event::ActionDone("已加入稍后再看", result),
        );
    }

    pub(crate) fn save_to_favorites(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let (cookie, mid) = (session.cookie.clone(), session.mid);
        let Some(aid) = self.playing_video.as_ref().map(|video| video.aid).filter(|aid| *aid > 0)
        else {
            return;
        };
        self.worker().spawn(
            async move { add_to_favorites(&cookie, mid, aid).await },
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
