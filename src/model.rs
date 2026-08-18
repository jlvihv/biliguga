use gpui::RenderImage;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct Video {
    pub(crate) bvid: String,
    pub(crate) aid: i64,
    pub(crate) cid: i64,
    pub(crate) progress: i64,
    pub(crate) title: String,
    pub(crate) uploader: String,
    pub(crate) stats: String,
    pub(crate) duration: String,
    pub(crate) cover: String,
    pub(crate) cover_image: Option<Arc<RenderImage>>,
    pub(crate) accent: u32,
    pub(crate) category: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Comment {
    pub(crate) rpid: i64,
    pub(crate) username: String,
    pub(crate) message: String,
    pub(crate) like: i64,
    pub(crate) time: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CommentPage {
    pub(crate) comments: Vec<Comment>,
    pub(crate) total: i64,
    pub(crate) has_more: bool,
}

pub(crate) static LOADING_VIDEO: Video = Video {
    bvid: String::new(),
    aid: 0,
    cid: 0,
    progress: 0,
    title: String::new(),
    uploader: String::new(),
    stats: String::new(),
    duration: String::new(),
    cover: String::new(),
    cover_image: None,
    accent: 0x74ade8,
    category: String::new(),
};
