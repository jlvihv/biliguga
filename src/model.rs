use gpui::RenderImage;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct Video {
    pub(crate) bvid: String,
    pub(crate) aid: i64,
    pub(crate) cid: i64,
    pub(crate) title: String,
    pub(crate) uploader: String,
    pub(crate) uploader_mid: i64,
    pub(crate) duration: String,
    pub(crate) cover: String,
    pub(crate) cover_image: Option<Arc<RenderImage>>,
    pub(crate) accent: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct VideoCollection {
    pub(crate) id: i64,
    pub(crate) title: String,
    pub(crate) episodes: Vec<Video>,
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
