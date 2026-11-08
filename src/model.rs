/// A decoded image as straight-alpha sRGB pixels, rows top first, four bytes each in RGBA
/// order: what `rune::ImageCache::rgba` and `rune::Pixels::set` both take. Keeping it free of
/// any UI type is what lets the network and player layers stay framework-agnostic.
#[derive(Clone, Debug)]
pub(crate) struct Rgba8 {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<u8>,
}

impl Rgba8 {
    pub(crate) fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        debug_assert_eq!(pixels.len(), width as usize * height as usize * 4);
        Self {
            width,
            height,
            pixels,
        }
    }

    /// A blank image, drawn as a flat colour until its real pixels arrive.
    pub(crate) fn filled(width: u32, height: u32, [red, green, blue, alpha]: [u8; 4]) -> Self {
        let mut pixels = vec![0; width as usize * height as usize * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[red, green, blue, alpha]);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
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
