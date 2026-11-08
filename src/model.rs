use std::sync::Arc;

/// A decoded image as straight-alpha sRGB pixels, rows top first, four bytes each in RGBA
/// order: what `rune::ImageCache::rgba` and `rune::Pixels::set` both take. Keeping it free of
/// any UI type is what lets the network and player layers stay framework-agnostic.
///
/// The pixels are shared, not owned: a cover used to be copied twice on its way to the
/// renderer (once out of the video row, once into the image cache), and a 160x90 thumbnail is
/// 57 KB, so the copies were pure waste. `Arc<[u8]>` also lets `ImageCache::rgba` take the
/// buffer without cloning it.
#[derive(Clone, Debug)]
pub(crate) struct Rgba8 {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Arc<[u8]>,
}

impl Rgba8 {
    pub(crate) fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        debug_assert_eq!(pixels.len(), width as usize * height as usize * 4);
        Self {
            width,
            height,
            pixels: pixels.into(),
        }
    }
}

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
