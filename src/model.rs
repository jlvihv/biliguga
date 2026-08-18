use gpui::RenderImage;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) struct Video {
    pub(crate) bvid: String,
    pub(crate) cid: i64,
    pub(crate) title: String,
    pub(crate) uploader: String,
    pub(crate) stats: String,
    pub(crate) duration: String,
    pub(crate) cover: String,
    pub(crate) cover_image: Option<Arc<RenderImage>>,
    pub(crate) accent: u32,
    pub(crate) category: String,
}

pub(crate) static LOADING_VIDEO: Video = Video {
    bvid: String::new(),
    cid: 0,
    title: String::new(),
    uploader: String::new(),
    stats: String::new(),
    duration: String::new(),
    cover: String::new(),
    cover_image: None,
    accent: 0x74ade8,
    category: String::new(),
};
