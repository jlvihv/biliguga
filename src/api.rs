use crate::{
    model::{Comment, CommentPage, Rgba8, Video, VideoCollection},
    network,
};
use futures::{
    StreamExt,
    channel::{mpsc as async_mpsc, oneshot},
};
use md5::{Digest, Md5};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::{Client, RequestBuilder};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const WBI_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

const MIXIN_KEY_TABLE: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

const RECOMMENDATION_PAGE_SIZE: usize = 20;

pub(crate) struct RecommendationPage {
    pub(crate) videos: Vec<Video>,
    pub(crate) has_more: bool,
}

pub(crate) fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string().trim_matches('"').to_string(),
        None => String::new(),
    }
}

/// The text of a JSON string field, borrowed. `None` for anything that is not a string.
///
/// `text()` returns an owned `String` and is what most of the parsers want, but the ones that
/// only pass the value on to another function (a cleaner, a URL fixer) would allocate a
/// `String` just to borrow it for one call. This is that borrow.
fn str_of(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn with_cookie(request: RequestBuilder, cookie: Option<&str>) -> RequestBuilder {
    if let Some(cookie) = cookie.filter(|cookie| !cookie.is_empty()) {
        request.header("Cookie", cookie)
    } else {
        request
    }
}

fn generated_buvid3() -> &'static str {
    static BUVID3: OnceLock<String> = OnceLock::new();
    BUVID3.get_or_init(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut digest = Md5::new();
        digest.update(now.to_le_bytes());
        format!("{:x}infoc", digest.finalize())
    })
}

fn with_action_cookie(request: RequestBuilder, cookie: &str) -> RequestBuilder {
    if cookie.is_empty() {
        return request;
    }
    let has_buvid3 = cookie.split(';').any(|part| {
        part.trim()
            .split_once('=')
            .map(|(name, _)| name == "buvid3")
            .unwrap_or(false)
    });
    if has_buvid3 {
        request.header("Cookie", cookie)
    } else {
        request.header("Cookie", format!("{cookie}; buvid3={}", generated_buvid3()))
    }
}

pub(crate) fn number(value: Option<&Value>) -> i64 {
    value
        .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
        .unwrap_or_default()
}

/// A clip's length as `mm:ss`, from a JSON seconds count.
///
/// Same shape as [`Clock`], and it now goes through it: this used to be a second copy of the
/// same `format!("{:02}:{:02}")`, which is where the two could drift apart.
fn duration(value: Option<&Value>) -> String {
    let seconds = number(value);
    if seconds < 0 {
        return "--:--".into();
    }
    // Zero is a real length (an empty upload, a clip that has not been probed yet) and reads as
    // `00:00`; only a negative one is unknown. It goes through the clock for the same reason the
    // player does: a poster and a control bar should never disagree about how to write a length.
    let seconds = seconds as f64;
    Clock::new(seconds, Clock::needs_hours(seconds))
        .as_str()
        .to_string()
}

/// A clock reading, written into a fixed buffer.
///
/// This runs twice per render of the player's control bar, which is several times a second
/// while a video plays. As a `String` that was two heap allocations per call; a reading never
/// needs more than eight bytes, so the caller supplies them.
///
/// The shape is chosen by the clip, not by the reading: [`Clock::for_duration`] decides once
/// whether the hours are worth a column, and the playhead and the length are then both written
/// that way. That is what keeps the two labels the same width and the seek bar between them
/// still — a clock that grew an hour field when the video crossed 59:59 would move it.
pub(crate) struct Clock {
    bytes: [u8; 8],
    len: u8,
}

impl Clock {
    /// A reading in the shape `with_hours` picks: `mm:ss` (five characters) or `hh:mm:ss`
    /// (eight). A clip over an hour gets the second, and everything reading it — the length on
    /// the poster, the total in the controls — asks [`Clock::needs_hours`] the same question.
    pub(crate) fn new(seconds: f64, with_hours: bool) -> Self {
        let mut clock = Clock {
            bytes: [0; 8],
            len: 0,
        };
        if !seconds.is_finite() || seconds < 0. {
            clock.push_bytes(if with_hours { "--:--:--" } else { "--:--" });
            return clock;
        }
        let total = seconds as u64;
        if with_hours {
            let (hours, minutes, secs) = (total / 3_600, total % 3_600 / 60, total % 60);
            clock.push_two(hours.min(99) as u8);
            clock.push_u8(b':');
            clock.push_two(minutes as u8);
            clock.push_u8(b':');
            clock.push_two(secs as u8);
        } else {
            // Minutes are capped at 99 rather than wrapped: only a caller that did not ask
            // `needs_hours` first reaches this with an hour on the clock.
            let (minutes, secs) = ((total / 60).min(99), total % 60);
            clock.push_two(minutes as u8);
            clock.push_u8(b':');
            clock.push_two(secs as u8);
        }
        clock
    }

    /// Whether a reading of `seconds` needs the hour field, which is the same thing as asking
    /// whether the whole clip does: the question is asked once per clip so its two labels agree.
    pub(crate) fn needs_hours(seconds: f64) -> bool {
        seconds.is_finite() && seconds >= 3_600.
    }

    fn push_two(&mut self, value: u8) {
        let value = value.min(99);
        self.push_u8(b'0' + value / 10);
        self.push_u8(b'0' + value % 10);
    }

    fn push_u8(&mut self, byte: u8) {
        if let Some(slot) = self.bytes.get_mut(self.len as usize) {
            *slot = byte;
            self.len += 1;
        }
    }

    fn push_bytes(&mut self, text: &str) {
        for byte in text.bytes() {
            self.push_u8(byte);
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        // Every byte written above is ASCII by construction.
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("--:--")
    }
}

pub(crate) fn format_publish_date(timestamp: i64) -> String {
    if timestamp <= 0 {
        return "发布时间未知".into();
    }

    // Bilibili timestamps are Unix seconds; display them in China Standard Time.
    let days = (timestamp + 8 * 60 * 60).div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era.div_euclid(1_460) + day_of_era.div_euclid(36_524)
        - day_of_era.div_euclid(146_096))
    .div_euclid(365);
    let year = year_of_era + era * 400;
    let day_of_year =
        day_of_era - (365 * year_of_era + year_of_era.div_euclid(4) - year_of_era.div_euclid(100));
    let month_part = (5 * day_of_year + 2).div_euclid(153);
    let day = day_of_year - (153 * month_part + 2).div_euclid(5) + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    let seconds_of_day = (timestamp + 8 * 60 * 60).rem_euclid(86_400);
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

fn accent_for(index: usize) -> u32 {
    [0x3e4654, 0x39455a, 0x4b4240, 0x4c4255, 0x354c4a, 0x46503d][index % 6]
}

fn https_url(value: String) -> String {
    if value.starts_with("//") {
        format!("https:{value}")
    } else if value.starts_with("http://") {
        value.replacen("http://", "https://", 1)
    } else {
        value
    }
}

fn thumbnail_url(url: &str, width: u32, height: u32) -> String {
    let normalized = https_url(url.to_string());
    if !(normalized.contains(".hdslb.com/") || normalized.contains(".biliimg.com/")) {
        return normalized;
    }

    let base = normalized
        .split('?')
        .next()
        .unwrap_or(&normalized)
        .split('@')
        .next()
        .unwrap_or(&normalized);
    format!("{base}@{width}w_{height}h_1c.webp")
}

fn decode_image(bytes: &[u8], width: u32, height: u32) -> Option<Arc<Rgba8>> {
    let compressed_len = bytes.len();
    let source = image::load_from_memory(bytes).ok()?.into_rgba8();
    if std::env::var_os("BILIGUGA_IMAGE_DEBUG").is_some() {
        eprintln!(
            "[biliguga-image] decode bytes={} source={}x{} target={}x{}",
            compressed_len,
            source.width(),
            source.height(),
            width,
            height,
        );
    }
    // `image` already hands back RGBA in the order `rune::Pixels` and `ImageCache` want, so no
    // channel swap is needed here (GPUI's BGRA order used to need one).
    let rgba = image::imageops::thumbnail(&source, width, height);
    drop(source);
    Some(Arc::new(Rgba8::new(
        rgba.width(),
        rgba.height(),
        rgba.into_raw(),
    )))
}

/// The HTTP clients this module uses, built once.
///
/// Every request used to build its own `reqwest::Client`, which meant a fresh connection pool
/// and a fresh TLS session per call: no keep-alive, no connection reuse, and a new TLS handshake
/// for every cover, every page and every comment page. There are two because a few write
/// endpoints want a browser user agent, and both are created on first use inside whatever tokio
/// worker calls them.
pub(crate) fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("Mozilla/5.0 biliguga/0.1")
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build the biliguga HTTP client")
    })
}

/// The client for endpoints that want to look like a browser (like, coin): same pool settings,
/// a different user agent.
fn browser_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            )
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build the biliguga browser HTTP client")
    })
}

async fn download_cover_async(
    client: &Client,
    url: &str,
    cancelled: &AtomicBool,
) -> Option<Arc<Rgba8>> {
    if cancelled.load(Ordering::Acquire) || url.is_empty() {
        return None;
    }
    let request_url = thumbnail_url(url, 160, 90);
    let response = client
        .get(&request_url)
        .header("Referer", "https://www.bilibili.com/")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() || cancelled.load(Ordering::Acquire) {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    if cancelled.load(Ordering::Acquire) {
        return None;
    }
    decode_image(&bytes, 160, 90)
}

struct CoverRequest {
    url: String,
    cancelled: Arc<AtomicBool>,
    reply: oneshot::Sender<Option<Arc<Rgba8>>>,
}

/// How many covers are fetched at once. The point of the limit is to keep a feed of twenty
/// thumbnails from opening twenty TLS handshakes at once; spreading them over several channels
/// (which is what this did) bought nothing, since the concurrency was already bounded here.
const COVER_CONCURRENCY: usize = 8;

/// The queue every cover goes through, and the one task that drains it.
fn cover_queue() -> Option<&'static async_mpsc::UnboundedSender<CoverRequest>> {
    static QUEUE: OnceLock<Option<async_mpsc::UnboundedSender<CoverRequest>>> = OnceLock::new();

    QUEUE
        .get_or_init(|| {
            let (sender, receiver) = async_mpsc::unbounded::<CoverRequest>();
            network::detach(async move {
                let mut results = receiver
                    .map(|request| async move {
                        // The shared client keeps its connection pool warm across covers, so a
                        // feed of twenty thumbnails reuses one TLS session per CDN host instead
                        // of shaking hands twenty times.
                        let image = download_cover_async(
                            client(),
                            &request.url,
                            &request.cancelled,
                        )
                        .await;
                        (request.reply, image)
                    })
                    .buffer_unordered(COVER_CONCURRENCY);
                while let Some((reply, image)) = results.next().await {
                    let _ = reply.send(image);
                }
            });
            Some(sender)
        })
        .as_ref()
}

pub(crate) fn queue_cover_download(
    url: String,
    cancelled: Arc<AtomicBool>,
) -> Option<oneshot::Receiver<Option<Arc<Rgba8>>>> {
    let queue = cover_queue()?;
    let (reply, receiver) = oneshot::channel();
    queue
        .unbounded_send(CoverRequest {
            url,
            cancelled,
            reply,
        })
        .ok()?;
    Some(receiver)
}

pub(crate) async fn download_avatar(url: &str) -> Option<Arc<Rgba8>> {
    if url.is_empty() {
        return None;
    }
    let client = client();
    let request_url = thumbnail_url(url, 160, 160);
    let response = client
        .get(&request_url)
        .header("Referer", "https://www.bilibili.com/")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    decode_image(&bytes, 160, 160)
}

/// A BV id, or `None` for a row that has none.
///
/// Eight parsers wrote the same "read it, bail if it is empty" pair; the id is also the only
/// field every one of them requires, since it is what a row is keyed and played by.
fn bvid_of(value: Option<&Value>) -> Option<String> {
    let bvid = text(value);
    (!bvid.is_empty()).then_some(bvid)
}

fn parse_video(item: &Value, index: usize) -> Option<Video> {
    let bvid = bvid_of(item.get("bvid"))?;
    // 推荐接口使用 `id` 表示 AV 号，详情/其它接口通常使用 `aid`。
    // 两者都兼容，否则推荐流中的视频无法上报观看记录和续播进度。
    let aid = number(item.get("aid"));
    let aid = if aid > 0 { aid } else { number(item.get("id")) };
    Some(Video {
        bvid,
        aid,
        cid: number(item.get("cid")),
        title: text(item.get("title")),
        uploader: text(item.get("owner").and_then(|owner| owner.get("name"))),
        uploader_mid: number(item.get("owner").and_then(|owner| owner.get("mid"))),
        duration: duration(item.get("duration")),
        cover: https_url(text(item.get("pic"))),
        accent: accent_for(index),
    })
}

/// Strips the `<em class="keyword">` markup Bilibili wraps search hits in, and unescapes the
/// entities it leaves behind.
///
/// Bypassing the tags and decoding the entity happen in one pass into one buffer. The old
/// version built the stripped text and then ran `str::replace` five times, and `replace`
/// allocates a fresh `String` each call: seven allocations for a title, twice per search hit,
/// which is 280 allocations for one page of twenty results.
fn clean_search_text(value: &str) -> String {
    let mut plain = String::with_capacity(value.len());
    let mut in_tag = false;
    let mut rest = value;
    while let Some(ch) = rest.chars().next() {
        rest = &rest[ch.len_utf8()..];
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            '&' => {
                // The five entities Bilibili uses in search text. Anything else is taken
                // literally, including a bare `&` that starts no known entity.
                let entity_len = ["&amp;", "&quot;", "&#39;", "&lt;", "&gt;"]
                    .iter()
                    .find(|entity| rest.starts_with(&entity[1..]))
                    .map(|entity| entity.len() - 1);
                match entity_len {
                    Some(len) => {
                        // `rest` starts after the `&`, so these are the tails of the entities.
                        plain.push(match &rest[..len] {
                            "amp;" => '&',
                            "quot;" => '"',
                            "#39;" => '\'',
                            "lt;" => '<',
                            _ => '>',
                        });
                        rest = &rest[len..];
                    }
                    None => plain.push('&'),
                }
            }
            _ => plain.push(ch),
        }
    }
    plain
}

fn parse_search_video(item: &Value, index: usize) -> Option<Video> {
    // Search results often carry the id only inside the page URL.
    let bvid = bvid_of(item.get("bvid")).or_else(|| {
        let from_url = text(item.get("arcurl"));
        let id = from_url.split('/').next_back().unwrap_or("");
        (!id.is_empty()).then(|| id.to_string())
    })?;
    let title = clean_search_text(str_of(item.get("title")));
    let duration = clean_search_text(str_of(item.get("duration")));
    Some(Video {
        bvid,
        aid: number(item.get("aid")),
        cid: 0,
        title,
        uploader: clean_search_text(str_of(item.get("author"))),
        uploader_mid: number(item.get("mid")),
        duration: if duration.is_empty() {
            "--:--".into()
        } else {
            duration
        },
        cover: https_url(text(item.get("pic"))),
        accent: accent_for(index),
    })
}

fn encode_wbi_value(value: &str) -> String {
    utf8_percent_encode(value, WBI_ENCODE_SET).to_string()
}

fn wbi_sign(
    params: &BTreeMap<String, String>,
    img_key: &str,
    sub_key: &str,
) -> BTreeMap<String, String> {
    let mut mixin_source = String::with_capacity(img_key.len() + sub_key.len());
    mixin_source.push_str(img_key);
    mixin_source.push_str(sub_key);
    let mixin: String = MIXIN_KEY_TABLE
        .iter()
        .filter_map(|index| mixin_source.as_bytes().get(*index).copied())
        .map(char::from)
        .take(32)
        .collect();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    let mut signed = params.clone();
    signed.insert("wts".into(), now);
    let query = signed
        .iter()
        .map(|(key, value)| {
            let filtered = value
                .chars()
                .filter(|ch| !"!'()*".contains(*ch))
                .collect::<String>();
            format!("{key}={}", encode_wbi_value(&filtered))
        })
        .collect::<Vec<_>>()
        .join("&");
    let mut digest = Md5::new();
    digest.update(format!("{query}{mixin}"));
    signed.insert("w_rid".into(), format!("{:x}", digest.finalize()));
    signed
}

/// The WBI signing keys, cached for a while.
///
/// They come from the `nav` endpoint and rotate roughly daily; every page of the feed, every
/// search and every history call used to fetch them again, so a scroll through three pages
/// spent three extra round trips on keys that had not changed. One entry, refreshed when it is
/// older than an hour or when a signed request comes back rejected.
fn wbi_cache() -> &'static std::sync::Mutex<Option<(String, String, std::time::Instant)>> {
    static CACHE: OnceLock<std::sync::Mutex<Option<(String, String, std::time::Instant)>>> =
        OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

fn cached_wbi_keys() -> Option<(String, String)> {
    let cache = wbi_cache().lock().ok()?;
    let (img, sub, at) = cache.as_ref()?;
    (at.elapsed() < std::time::Duration::from_secs(3600)).then(|| (img.clone(), sub.clone()))
}

fn store_wbi_keys(img_key: &str, sub_key: &str) {
    if let Ok(mut cache) = wbi_cache().lock() {
        *cache = Some((
            img_key.to_string(),
            sub_key.to_string(),
            std::time::Instant::now(),
        ));
    }
}

/// Forgets the cached keys, so the next request fetches fresh ones after a rejection.
fn invalidate_wbi_keys() {
    if let Ok(mut cache) = wbi_cache().lock() {
        *cache = None;
    }
}

async fn fetch_wbi_keys(client: &Client, cookie: Option<&str>) -> Result<(String, String), String> {
    if let Some(keys) = cached_wbi_keys() {
        return Ok(keys);
    }
    let nav: Value = with_cookie(
        client.get("https://api.bilibili.com/x/web-interface/nav"),
        cookie,
    )
    .send()
    .await
    .map_err(|error| format!("获取 WBI 密钥失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析 WBI 密钥失败：{error}"))?;
    let wbi_img = nav
        .get("data")
        .and_then(|data| data.get("wbi_img"))
        .ok_or_else(|| "B 站没有返回 WBI 密钥".to_string())?;
    let img_key = media_key(wbi_img.get("img_url"));
    let sub_key = media_key(wbi_img.get("sub_url"));
    if img_key.is_empty() || sub_key.is_empty() {
        return Err("B 站 WBI 密钥为空".into());
    }
    store_wbi_keys(&img_key, &sub_key);
    Ok((img_key, sub_key))
}

/// The key part of a WBI media URL: its file name without the extension.
///
/// This extraction was copy-pasted into five places (the feed, search, the dynamic feed, the
/// history report and the player detail call), each one eleven lines.
fn media_key(url: Option<&Value>) -> String {
    text(url)
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string()
}

#[derive(Clone, Debug)]
pub(crate) struct VideoContext {
    pub(crate) aid: i64,
    pub(crate) cid: i64,
    pub(crate) views: i64,
    pub(crate) danmaku: i64,
    pub(crate) likes: i64,
    pub(crate) pubdate: i64,
    pub(crate) uploader: String,
    pub(crate) uploader_mid: i64,
    pub(crate) collection: Option<VideoCollection>,
}

fn parse_collection(season: &Value, owner_mid: i64, owner_name: &str) -> Option<VideoCollection> {
    let id = number(season.get("id"))
        .max(number(season.get("season_id")))
        .max(
            season
                .get("sections")
                .and_then(Value::as_array)
                .and_then(|sections| {
                    sections
                        .iter()
                        .map(|section| number(section.get("season_id")))
                        .max()
                })
                .unwrap_or(0),
        );
    let title = {
        let title = text(season.get("title"));
        if title.is_empty() {
            text(season.get("name"))
        } else {
            title
        }
    };
    let mut episodes = Vec::new();
    if let Some(sections) = season.get("sections").and_then(Value::as_array) {
        for section in sections {
            let section_episodes = section
                .get("episodes")
                .or_else(|| section.get("archives"))
                .and_then(Value::as_array);
            let Some(section_episodes) = section_episodes else {
                continue;
            };
            for (index, episode) in section_episodes.iter().enumerate() {
                let arc = episode.get("arc").unwrap_or(&Value::Null);
                let Some(bvid) = bvid_of(episode.get("bvid")).or_else(|| bvid_of(arc.get("bvid")))
                else {
                    continue;
                };
                let aid = number(episode.get("aid")).max(number(arc.get("aid")));
                let cid = number(episode.get("cid"));
                let episode_title = {
                    let title = clean_search_text(str_of(episode.get("title")));
                    if title.is_empty() {
                        clean_search_text(str_of(arc.get("title")))
                    } else {
                        title
                    }
                };
                episodes.push(Video {
                    bvid,
                    aid,
                    cid,
                    title: episode_title,
                    uploader: owner_name.to_string(),
                    uploader_mid: owner_mid,
                    duration: {
                        let duration_text = text(arc.get("duration"));
                        if duration_text.is_empty() {
                            duration(arc.get("duration"))
                        } else {
                            duration_text
                        }
                    },
                    cover: https_url(text(arc.get("pic"))),
                    accent: accent_for(index),
                });
            }
        }
    }
    if id <= 0 && episodes.is_empty() {
        return None;
    }
    Some(VideoCollection {
        id,
        title,
        episodes,
    })
}

pub(crate) async fn fetch_video_context(
    video: &Video,
    cookie: Option<&str>,
) -> Result<VideoContext, String> {
    let client = client();
    let query = if video.bvid.is_empty() {
        vec![("aid", video.aid.to_string())]
    } else {
        vec![("bvid", video.bvid.clone())]
    };
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/web-interface/view")
            .header(
                "Referer",
                format!("https://www.bilibili.com/video/{}", video.bvid),
            )
            .query(&query),
        cookie,
    )
    .send()
    .await
    .map_err(|error| format!("获取视频详情失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析视频详情失败：{error}"))?;
    if number(response.get("code")) != 0 {
        return Err(api_error(&response, "获取视频详情"));
    }
    let data = response
        .get("data")
        .ok_or_else(|| "视频详情没有返回数据".to_string())?;
    let owner = data.get("owner").unwrap_or(&Value::Null);
    let owner_mid = number(owner.get("mid")).max(video.uploader_mid);
    let owner_name = {
        let name = text(owner.get("name"));
        if name.is_empty() {
            video.uploader.clone()
        } else {
            name
        }
    };
    let stat = data.get("stat").unwrap_or(&Value::Null);
    Ok(VideoContext {
        aid: number(data.get("aid")).max(video.aid),
        cid: number(data.get("cid")).max(video.cid),
        views: number(stat.get("view")),
        danmaku: number(stat.get("danmaku")),
        likes: number(stat.get("like")),
        pubdate: number(data.get("pubdate")),
        uploader: owner_name.clone(),
        uploader_mid: owner_mid,
        collection: data
            .get("ugc_season")
            .and_then(|season| parse_collection(season, owner_mid, &owner_name)),
    })
}

pub(crate) struct AuthorVideoPage {
    pub(crate) videos: Vec<Video>,
    pub(crate) page: usize,
    pub(crate) has_more: bool,
}

fn parse_author_video(item: &Value, index: usize, mid: i64) -> Option<Video> {
    let bvid = bvid_of(item.get("bvid"))?;
    let title = clean_search_text(str_of(item.get("title")));
    let duration_text = text(item.get("length"));
    Some(Video {
        bvid,
        aid: number(item.get("aid")),
        cid: 0,
        title,
        uploader: text(item.get("author")),
        uploader_mid: mid,
        duration: if duration_text.is_empty() {
            duration(item.get("duration"))
        } else {
            duration_text
        },
        cover: https_url(text(item.get("pic"))),
        accent: accent_for(index),
    })
}

pub(crate) async fn fetch_author_videos(
    mid: i64,
    page: usize,
    cookie: Option<&str>,
) -> Result<AuthorVideoPage, String> {
    if mid <= 0 {
        return Err("作者 UID 无效".into());
    }
    const PAGE_SIZE: usize = 30;
    let client = client();
    let (img_key, sub_key) = fetch_wbi_keys(client, cookie).await?;
    let mut params = BTreeMap::new();
    params.insert("mid".into(), mid.to_string());
    params.insert("ps".into(), PAGE_SIZE.to_string());
    params.insert("pn".into(), page.max(1).to_string());
    params.insert("tid".into(), "0".into());
    params.insert("keyword".into(), String::new());
    params.insert("order".into(), "pubdate".into());
    params.insert("platform".into(), "web".into());
    params.insert("web_location".into(), "1550101".into());
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/space/wbi/arc/search")
            .header("Referer", format!("https://space.bilibili.com/{mid}/video"))
            .query(&wbi_sign(&params, &img_key, &sub_key)),
        cookie,
    )
    .send()
    .await
    .map_err(|error| format!("请求作者视频失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析作者视频失败：{error}"))?;
    if number(response.get("code")) != 0 {
        return Err(api_error(&response, "作者视频"));
    }
    let data = response.get("data").unwrap_or(&Value::Null);
    let list = data
        .get("list")
        .and_then(|list| list.get("vlist"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let videos = list
        .iter()
        .enumerate()
        .filter_map(|(index, item)| parse_author_video(item, index, mid))
        .collect::<Vec<_>>();
    let total = number(data.get("page").and_then(|page| page.get("count")));
    let page = page.max(1);
    Ok(AuthorVideoPage {
        has_more: if total > 0 {
            page as i64 * (PAGE_SIZE as i64) < total
        } else {
            list.len() >= PAGE_SIZE
        },
        videos,
        page,
    })
}

pub(crate) async fn fetch_recommendations(
    page: usize,
    cookie: Option<&str>,
) -> Result<RecommendationPage, String> {
    let client = client();
    let (img_key, sub_key) = fetch_wbi_keys(client, cookie).await?;

    let mut params = BTreeMap::new();
    params.insert("version".into(), "1".into());
    params.insert("feed_version".into(), "V8".into());
    params.insert("fresh_idx".into(), page.max(1).to_string());
    params.insert("brush".into(), page.max(1).to_string());
    params.insert("fresh_type".into(), "4".into());
    params.insert("homepage_ver".into(), "1".into());
    params.insert("ps".into(), RECOMMENDATION_PAGE_SIZE.to_string());
    let signed = wbi_sign(&params, &img_key, &sub_key);
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/web-interface/wbi/index/top/feed/rcmd")
            .header("Referer", "https://www.bilibili.com/"),
        cookie,
    )
    .query(&signed)
    .send()
    .await
    .map_err(|error| format!("请求推荐流失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析推荐流失败：{error}"))?;
    if number(response.get("code")) != 0 {
        return Err(api_error(&response, "推荐"));
    }
    let items = response
        .get("data")
        .and_then(|data| data.get("item"))
        .and_then(Value::as_array)
        .ok_or_else(|| "推荐接口没有返回视频列表".to_string())?;
    let videos = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| parse_video(item, index))
        .collect::<Vec<_>>();
    if videos.is_empty() {
        Err("推荐接口返回了空列表".into())
    } else {
        Ok(RecommendationPage {
            videos,
            has_more: items.len() >= RECOMMENDATION_PAGE_SIZE,
        })
    }
}

fn parse_search_result_list(response: &Value) -> Vec<Video> {
    let direct = response
        .get("data")
        .and_then(|data| data.get("result"))
        .and_then(Value::as_array);
    if let Some(items) = direct {
        let videos = items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| parse_search_video(item, index))
            .collect::<Vec<_>>();
        if !videos.is_empty() {
            return videos;
        }
        if let Some(items) = items.iter().find_map(|item| {
            (text(item.get("result_type")) == "video")
                .then(|| item.get("data"))
                .flatten()
                .and_then(Value::as_array)
        }) {
            return items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| parse_search_video(item, index))
                .collect();
        }
    }
    Vec::new()
}

pub(crate) async fn fetch_search_results(keyword: &str) -> Result<Vec<Video>, String> {
    let client = client();
    let (img_key, sub_key) = fetch_wbi_keys(client, None).await?;

    let mut params = BTreeMap::new();
    params.insert("keyword".into(), keyword.into());
    params.insert("search_type".into(), "video".into());
    params.insert("order".into(), "totalrank".into());
    params.insert("duration".into(), "0".into());
    params.insert("tids".into(), "0".into());
    params.insert("page".into(), "1".into());
    params.insert("page_size".into(), "20".into());
    params.insert("platform".into(), "pc".into());
    params.insert("web_location".into(), "1430654".into());
    let signed = wbi_sign(&params, &img_key, &sub_key);
    let response: Value = client
        .get("https://api.bilibili.com/x/web-interface/wbi/search/type")
        .header("Origin", "https://search.bilibili.com")
        .header("Referer", "https://search.bilibili.com/")
        .query(&signed)
        .send()
        .await
        .map_err(|error| format!("请求 B 站搜索失败：{error}"))?
        .json()
        .await
        .map_err(|error| format!("解析 B 站搜索结果失败：{error}"))?;
    let code = number(response.get("code"));
    let videos = if code == 0 {
        parse_search_result_list(&response)
    } else {
        Vec::new()
    };
    if !videos.is_empty() {
        return Ok(videos);
    }

    let mut fallback_params = BTreeMap::new();
    fallback_params.insert("keyword".into(), keyword.into());
    fallback_params.insert("page".into(), "1".into());
    fallback_params.insert("page_size".into(), "20".into());
    fallback_params.insert("platform".into(), "pc".into());
    fallback_params.insert("web_location".into(), "1430654".into());
    let fallback_response: Value = client
        .get("https://api.bilibili.com/x/web-interface/wbi/search/all/v2")
        .header("Origin", "https://search.bilibili.com")
        .header("Referer", "https://search.bilibili.com/")
        .query(&wbi_sign(&fallback_params, &img_key, &sub_key))
        .send()
        .await
        .map_err(|error| format!("请求 B 站综合搜索失败：{error}"))?
        .json()
        .await
        .map_err(|error| format!("解析 B 站综合搜索结果失败：{error}"))?;
    if number(fallback_response.get("code")) != 0 {
        // `fallback_response`, not `response`: the first one succeeded, which is why this
        // fallback ran at all, so reporting its code described the wrong answer.
        return Err(api_error(&fallback_response, "综合搜索"));
    }
    let videos = parse_search_result_list(&fallback_response);
    if videos.is_empty() {
        Err("没有找到相关视频".into())
    } else {
        Ok(videos)
    }
}

fn parse_history_video(item: &Value, index: usize) -> Option<Video> {
    let history = item.get("history")?;
    let bvid = bvid_of(history.get("bvid"))?;
    let cover = {
        let cover = text(item.get("cover"));
        if cover.is_empty() {
            text(item.get("pic"))
        } else {
            cover
        }
    };
    Some(Video {
        bvid,
        // 历史接口通常把 AV 号放在 history.oid，而不是 aid。
        aid: number(history.get("aid"))
            .max(number(history.get("oid")))
            .max(number(item.get("aid"))),
        cid: number(history.get("cid")),
        title: clean_search_text(str_of(item.get("title"))),
        uploader: text(item.get("author_name")),
        uploader_mid: number(item.get("author_mid")),
        duration: duration(item.get("duration")),
        cover: https_url(cover),
        accent: accent_for(index),
    })
}

pub(crate) async fn fetch_history(cookie: &str) -> Result<Vec<Video>, String> {
    let client = client();
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/web-interface/history/cursor")
            .header("Referer", "https://www.bilibili.com/")
            .query(&[("ps", "30"), ("max", "0"), ("view_at", "0")]),
        Some(cookie),
    )
    .send()
    .await
    .map_err(|error| format!("请求观看历史失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析观看历史失败：{error}"))?;
    if number(response.get("code")) != 0 {
        return Err(api_error(&response, "观看历史"));
    }
    let items = response
        .get("data")
        .and_then(|data| data.get("list"))
        .and_then(Value::as_array)
        .ok_or_else(|| "观看历史接口没有返回列表".to_string())?;
    Ok(items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| parse_history_video(item, index))
        .collect())
}

pub(crate) async fn report_video_progress(
    cookie: &str,
    aid: i64,
    cid: i64,
    progress: f64,
) -> Result<(), String> {
    if aid <= 0 || cid <= 0 {
        return Err("视频缺少有效的 AV 号或 CID".into());
    }
    let csrf = csrf_from_cookie(cookie).ok_or_else(|| "登录状态缺少 CSRF 凭证".to_string())?;
    let client = client();
    let progress = progress.max(0.).floor() as i64;
    let response: Value = with_cookie(
        client
            .post("https://api.bilibili.com/x/v2/history/report")
            .header("Referer", "https://www.bilibili.com/"),
        Some(cookie),
    )
    .form(&[
        ("aid", aid.to_string()),
        ("cid", cid.to_string()),
        ("progress", progress.to_string()),
        ("platform", "web".to_string()),
        ("csrf", csrf),
    ])
    .send()
    .await
    .map_err(|error| format!("上报观看进度失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析观看进度响应失败：{error}"))?;
    if number(response.get("code")) == 0 {
        Ok(())
    } else {
        Err(api_error(&response, "观看进度上报"))
    }
}

pub(crate) async fn report_video_heartbeat(
    cookie: &str,
    video: &Video,
    progress: f64,
    play_type: i64,
) -> Result<(), String> {
    if video.aid <= 0 || video.cid <= 0 {
        return Err("视频缺少有效的 AV 号或 CID".into());
    }
    let csrf = csrf_from_cookie(cookie).ok_or_else(|| "登录状态缺少 CSRF 凭证".to_string())?;
    let client = client();
    let progress = progress.max(0.).floor() as i64;
    let start_ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(progress as u64);
    let response: Value = with_cookie(
        client
            .post("https://api.bilibili.com/x/click-interface/web/heartbeat")
            .header(
                "Referer",
                format!("https://www.bilibili.com/video/{}", video.bvid),
            ),
        Some(cookie),
    )
    .form(&[
        ("aid", video.aid.to_string()),
        ("bvid", video.bvid.clone()),
        ("cid", video.cid.to_string()),
        ("played_time", progress.to_string()),
        ("realtime", progress.to_string()),
        ("start_ts", start_ts.to_string()),
        ("type", "3".to_string()),
        ("dt", "2".to_string()),
        ("play_type", play_type.to_string()),
        ("csrf", csrf),
    ])
    .send()
    .await
    .map_err(|error| format!("上报播放心跳失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析播放心跳响应失败：{error}"))?;
    if number(response.get("code")) == 0 {
        Ok(())
    } else {
        Err(api_error(&response, "播放心跳上报"))
    }
}

pub(crate) async fn fetch_last_play_progress(video: &Video, cookie: Option<&str>) -> Option<i64> {
    if video.aid <= 0 || video.cid <= 0 || cookie.is_none() {
        return None;
    }
    let client = client();
    let (img_key, sub_key) = fetch_wbi_keys(client, cookie).await.ok()?;
    let mut params = BTreeMap::new();
    params.insert("aid".into(), video.aid.to_string());
    params.insert("bvid".into(), video.bvid.clone());
    params.insert("cid".into(), video.cid.to_string());
    params.insert("web_location".into(), "1315873".into());
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/player/wbi/v2")
            .header(
                "Referer",
                format!("https://www.bilibili.com/video/{}", video.bvid),
            ),
        cookie,
    )
    .query(&wbi_sign(&params, &img_key, &sub_key))
    .send()
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    (number(response.get("code")) == 0)
        .then(|| {
            // B 站 player 接口返回毫秒，播放器和历史上报使用秒。
            number(
                response
                    .get("data")
                    .and_then(|data| data.get("last_play_time")),
            ) / 1000
        })
        .filter(|progress| *progress > 0)
}

fn parse_dynamic_archive(archive: &Value) -> Option<Video> {
    let bvid = bvid_of(archive.get("bvid"))?;
    Some(Video {
        bvid,
        aid: text(archive.get("aid")).parse().unwrap_or_default(),
        cid: 0,
        title: clean_search_text(str_of(archive.get("title"))),
        uploader: String::new(),
        uploader_mid: 0,
        duration: {
            let value = text(archive.get("duration_text"));
            if value.is_empty() {
                "--:--".into()
            } else {
                value
            }
        },
        cover: https_url(text(archive.get("cover"))),
        accent: 0x3e4654,
    })
}

fn parse_dynamic_video(item: &Value, index: usize) -> Option<Video> {
    if item.get("visible").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let modules = item.get("modules").unwrap_or(&Value::Null);
    let author_module = modules.get("module_author").unwrap_or(&Value::Null);
    let content_module = modules.get("module_dynamic").unwrap_or(&Value::Null);
    let author = text(author_module.get("name"));
    let author_mid = number(author_module.get("mid"));
    let major = content_module.get("major").unwrap_or(&Value::Null);
    let archive = major
        .get("archive")
        .or_else(|| major.get("pgc"))
        .or_else(|| {
            major
                .get("ugc_season")
                .and_then(|season| season.get("archive"))
        });
    let mut video = archive.and_then(parse_dynamic_archive);
    if video.is_none() && text(item.get("type")) == "DYNAMIC_TYPE_FORWARD" {
        video = item
            .get("orig")
            .and_then(|original| parse_dynamic_video(original, index));
    }
    let mut video = video?;
    if video.uploader.is_empty() {
        video.uploader = author;
    }
    if video.uploader_mid == 0 {
        video.uploader_mid = author_mid;
    }
    video.accent = accent_for(index);
    Some(video)
}

pub(crate) async fn fetch_dynamic_feed(cookie: &str) -> Result<Vec<Video>, String> {
    let client = client();
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/polymer/web-dynamic/v1/feed/all")
            .header("Referer", "https://t.bilibili.com/")
            .query(&[
                ("type", "all"),
                ("offset", ""),
                ("update_baseline", ""),
                ("page", "1"),
                ("features", "itemOpusStyle,listOnlyfans"),
                ("timezone_offset", "-480"),
                ("platform", "web"),
                ("web_location", "333.1365"),
            ]),
        Some(cookie),
    )
    .send()
    .await
    .map_err(|error| format!("请求动态失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析动态失败：{error}"))?;
    if number(response.get("code")) != 0 {
        return Err(api_error(&response, "动态"));
    }
    let items = response
        .get("data")
        .and_then(|data| data.get("items"))
        .and_then(Value::as_array)
        .ok_or_else(|| "动态接口没有返回列表".to_string())?
        .iter()
        .enumerate()
        .filter_map(|(index, item)| parse_dynamic_video(item, index))
        .collect();
    Ok(items)
}

fn api_error(response: &Value, operation: &str) -> String {
    let code = number(response.get("code"));
    // -403 is Bilibili's "bad signature": the WBI keys changed under us. Dropping the cache is
    // what makes the next attempt fetch new ones instead of failing the same way forever.
    if code == -403 {
        invalidate_wbi_keys();
    }
    format!(
        "{operation}接口返回错误 {code}：{}",
        text(response.get("message"))
    )
}

fn csrf_from_cookie(cookie: &str) -> Option<String> {
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == "bili_jct").then(|| value.to_string())
    })
}

fn parse_watch_later_video(item: &Value, index: usize) -> Option<Video> {
    let bvid = bvid_of(item.get("bvid"))?;
    Some(Video {
        bvid,
        aid: number(item.get("aid")),
        cid: number(item.get("cid")),
        title: clean_search_text(str_of(item.get("title"))),
        uploader: text(item.get("owner").and_then(|owner| owner.get("name"))),
        uploader_mid: number(item.get("owner").and_then(|owner| owner.get("mid"))),
        duration: duration(item.get("duration")),
        cover: https_url(text(item.get("pic"))),
        accent: accent_for(index),
    })
}

pub(crate) async fn fetch_watch_later(cookie: &str) -> Result<Vec<Video>, String> {
    let client = client();
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/v2/history/toview")
            .header("Referer", "https://www.bilibili.com/watchlater/"),
        Some(cookie),
    )
    .send()
    .await
    .map_err(|error| format!("请求稍后再看失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析稍后再看失败：{error}"))?;
    if number(response.get("code")) != 0 {
        return Err(api_error(&response, "稍后再看"));
    }
    let items = response
        .get("data")
        .and_then(|data| data.get("list").or(Some(data)))
        .and_then(Value::as_array)
        .ok_or_else(|| "稍后再看接口没有返回列表".to_string())?;
    Ok(items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| parse_watch_later_video(item, index))
        .collect())
}

fn parse_favorite_video(item: &Value, index: usize) -> Option<Video> {
    let bvid = bvid_of(item.get("bvid")).or_else(|| bvid_of(item.get("bv_id")))?;
    let ugc = item.get("ugc");
    let upper = item.get("upper");
    Some(Video {
        bvid,
        aid: number(item.get("id")),
        cid: number(ugc.and_then(|ugc| ugc.get("first_cid"))),
        title: clean_search_text(str_of(item.get("title"))),
        uploader: text(upper.and_then(|upper| upper.get("name"))),
        uploader_mid: number(upper.and_then(|upper| upper.get("mid"))),
        duration: duration(item.get("duration")),
        cover: https_url(text(item.get("cover"))),
        accent: accent_for(index),
    })
}

pub(crate) async fn fetch_favorites(cookie: &str, mid: i64) -> Result<Vec<Video>, String> {
    let client = client();
    let folders: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/v3/fav/folder/created/list-all")
            .header(
                "Referer",
                format!("https://space.bilibili.com/{mid}/favlist"),
            )
            .query(&[
                ("up_mid", mid.to_string()),
                ("web_location", "333.1387".into()),
            ]),
        Some(cookie),
    )
    .send()
    .await
    .map_err(|error| format!("请求收藏夹失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析收藏夹失败：{error}"))?;
    if number(folders.get("code")) != 0 {
        return Err(api_error(&folders, "收藏夹"));
    }
    let folder_id = folders
        .get("data")
        .and_then(|data| data.get("list"))
        .and_then(Value::as_array)
        .and_then(|folders| {
            folders.iter().find_map(|folder| {
                let id = number(folder.get("id")).max(number(folder.get("fid")));
                (id > 0).then_some(id)
            })
        })
        .ok_or_else(|| "没有找到可用的收藏夹".to_string())?;
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/v3/fav/resource/list")
            .header(
                "Referer",
                format!("https://www.bilibili.com/medialist/detail/ml{folder_id}"),
            )
            .query(&[
                ("media_id", folder_id.to_string()),
                ("pn", "1".into()),
                ("ps", "30".into()),
                ("type", "0".into()),
                ("tid", "0".into()),
                ("platform", "web".into()),
            ]),
        Some(cookie),
    )
    .send()
    .await
    .map_err(|error| format!("请求收藏视频失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析收藏视频失败：{error}"))?;
    if number(response.get("code")) != 0 {
        return Err(api_error(&response, "收藏视频"));
    }
    let items = response
        .get("data")
        .and_then(|data| data.get("medias"))
        .and_then(Value::as_array)
        .ok_or_else(|| "收藏夹没有返回视频列表".to_string())?;
    Ok(items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| parse_favorite_video(item, index))
        .collect())
}

pub(crate) async fn add_to_watch_later(cookie: &str, aid: i64) -> Result<(), String> {
    let csrf = csrf_from_cookie(cookie).ok_or_else(|| "登录状态缺少 CSRF 凭证".to_string())?;
    let client = client();
    let response: Value = with_cookie(
        client
            .post("https://api.bilibili.com/x/v2/history/toview/add")
            .header("Referer", "https://www.bilibili.com/"),
        Some(cookie),
    )
    .form(&[("aid", aid.to_string()), ("csrf", csrf)])
    .send()
    .await
    .map_err(|error| format!("添加稍后再看失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析稍后再看响应失败：{error}"))?;
    if number(response.get("code")) == 0 {
        Ok(())
    } else {
        Err(api_error(&response, "添加稍后再看"))
    }
}

pub(crate) async fn add_to_favorites(cookie: &str, mid: i64, aid: i64) -> Result<(), String> {
    let csrf = csrf_from_cookie(cookie).ok_or_else(|| "登录状态缺少 CSRF 凭证".to_string())?;
    let client = client();
    let folders: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/v3/fav/folder/created/list-all")
            .header(
                "Referer",
                format!("https://space.bilibili.com/{mid}/favlist"),
            )
            .query(&[
                ("up_mid", mid.to_string()),
                ("web_location", "333.1387".into()),
            ]),
        Some(cookie),
    )
    .send()
    .await
    .map_err(|error| format!("请求默认收藏夹失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析默认收藏夹失败：{error}"))?;
    if number(folders.get("code")) != 0 {
        return Err(api_error(&folders, "默认收藏夹"));
    }
    let folder_id = folders
        .get("data")
        .and_then(|data| data.get("list"))
        .and_then(Value::as_array)
        .and_then(|folders| {
            folders.iter().find_map(|folder| {
                let id = number(folder.get("id")).max(number(folder.get("fid")));
                (id > 0).then_some(id)
            })
        })
        .ok_or_else(|| "没有找到可用的收藏夹".to_string())?;
    let response: Value = with_cookie(
        client
            .post("https://api.bilibili.com/x/v3/fav/resource/deal")
            .header("Referer", "https://www.bilibili.com/"),
        Some(cookie),
    )
    .form(&[
        ("rid", aid.to_string()),
        ("type", "2".into()),
        ("add_media_ids", folder_id.to_string()),
        ("del_media_ids", String::new()),
        ("csrf", csrf),
    ])
    .send()
    .await
    .map_err(|error| format!("收藏视频失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析收藏响应失败：{error}"))?;
    if number(response.get("code")) == 0 {
        Ok(())
    } else {
        Err(api_error(&response, "收藏视频"))
    }
}

pub(crate) async fn like_video(cookie: &str, aid: i64) -> Result<(), String> {
    let csrf = csrf_from_cookie(cookie).ok_or_else(|| "登录状态缺少 CSRF 凭证".to_string())?;
    let client = browser_client();
    let response: Value = with_action_cookie(
        client
            .post("https://api.bilibili.com/x/web-interface/archive/like")
            .header("Origin", "https://www.bilibili.com")
            .header("Referer", "https://www.bilibili.com/"),
        cookie,
    )
    .form(&[
        ("aid", aid.to_string()),
        ("like", "1".into()),
        ("csrf", csrf),
    ])
    .send()
    .await
    .map_err(|error| format!("点赞失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析点赞响应失败：{error}"))?;
    if number(response.get("code")) == 0 {
        Ok(())
    } else {
        Err(api_error(&response, "点赞"))
    }
}

pub(crate) async fn coin_video(cookie: &str, aid: i64) -> Result<(), String> {
    let csrf = csrf_from_cookie(cookie).ok_or_else(|| "登录状态缺少 CSRF 凭证".to_string())?;
    let client = browser_client();
    let response: Value = with_action_cookie(
        client
            .post("https://api.bilibili.com/x/web-interface/coin/add")
            .header("Origin", "https://www.bilibili.com")
            .header("Referer", "https://www.bilibili.com/"),
        cookie,
    )
    .form(&[
        ("aid", aid.to_string()),
        ("multiply", "1".into()),
        ("select_like", "0".into()),
        ("csrf", csrf),
    ])
    .send()
    .await
    .map_err(|error| format!("投币失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析投币响应失败：{error}"))?;
    if number(response.get("code")) == 0 {
        Ok(())
    } else if number(response.get("code")) == -401
        && response
            .get("data")
            .and_then(|data| data.get("ga_data"))
            .and_then(|data| data.get("decisions"))
            .and_then(Value::as_array)
            .is_some_and(|decisions| {
                decisions
                    .iter()
                    .any(|decision| text(Some(decision)) == "verify_captcha_level3")
            })
    {
        Err("投币被 B 站风控拦截，请先在浏览器打开该视频并完成验证码后再重试".into())
    } else {
        Err(api_error(&response, "投币"))
    }
}

fn format_comment_time(timestamp: i64) -> String {
    if timestamp <= 0 {
        return "未知时间".into();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let elapsed = now.saturating_sub(timestamp as u64);
    if elapsed < 60 {
        "刚刚".into()
    } else if elapsed < 3600 {
        format!("{}分钟前", elapsed / 60)
    } else if elapsed < 86_400 {
        format!("{}小时前", elapsed / 3600)
    } else if elapsed < 2_592_000 {
        format!("{}天前", elapsed / 86_400)
    } else {
        format!(
            "{}年{}月",
            (timestamp / 31_536_000) + 1970,
            (timestamp / 2_592_000) % 12 + 1
        )
    }
}

fn parse_comment(item: &Value) -> Option<Comment> {
    let rpid = number(item.get("rpid"));
    if rpid <= 0 {
        return None;
    }
    let message = clean_search_text(str_of(
        item.get("content")
            .and_then(|content| content.get("message")),
    ));
    if message.trim().is_empty() {
        return None;
    }
    Some(Comment {
        rpid,
        username: text(item.get("member").and_then(|member| member.get("uname"))),
        message,
        like: number(item.get("like")),
        time: format_comment_time(number(item.get("ctime"))),
    })
}

pub(crate) async fn fetch_comments(
    video: &Video,
    cookie: Option<&str>,
    page: u32,
) -> Result<CommentPage, String> {
    let client = client();
    let mut aid = video.aid;
    if aid <= 0 {
        if video.bvid.is_empty() {
            return Err("视频缺少 BV 号，无法获取评论".into());
        }
        let detail: Value = with_cookie(
            client
                .get("https://api.bilibili.com/x/web-interface/view")
                .header(
                    "Referer",
                    format!("https://www.bilibili.com/video/{}", video.bvid),
                )
                .query(&[("bvid", video.bvid.as_str())]),
            cookie,
        )
        .send()
        .await
        .map_err(|error| format!("获取视频详情失败：{error}"))?
        .json()
        .await
        .map_err(|error| format!("解析视频详情失败：{error}"))?;
        if number(detail.get("code")) != 0 {
            return Err(api_error(&detail, "获取视频详情"));
        }
        aid = number(detail.get("data").and_then(|data| data.get("aid")));
        if aid <= 0 {
            return Err("视频详情没有返回 AV 号，无法获取评论".into());
        }
    }
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/v2/reply")
            .header(
                "Referer",
                format!("https://www.bilibili.com/video/{}", video.bvid),
            )
            .query(&[
                ("oid", aid.to_string()),
                ("type", "1".into()),
                ("pn", page.max(1).to_string()),
                ("ps", "20".into()),
                ("sort", "1".into()),
                ("web_location", "1315875".into()),
            ]),
        cookie,
    )
    .send()
    .await
    .map_err(|error| format!("获取评论失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析评论失败：{error}"))?;
    if number(response.get("code")) != 0 {
        return Err(api_error(&response, "获取评论"));
    }

    let data = response.get("data").unwrap_or(&Value::Null);
    let mut comments = Vec::new();
    let mut seen = HashSet::new();
    for key in ["hots", "replies"] {
        if let Some(items) = data.get(key).and_then(Value::as_array) {
            for item in items {
                if let Some(comment) = parse_comment(item)
                    && seen.insert(comment.rpid)
                {
                    comments.push(comment);
                }
            }
        }
    }

    let page_info = data.get("page").unwrap_or(&Value::Null);
    let total = number(page_info.get("acount")).max(number(page_info.get("count")));
    let page_size = number(page_info.get("size")).max(20);
    let loaded = page as i64 * page_size;
    let has_more = if total > 0 {
        loaded < total
    } else {
        comments.len() >= 20
    };
    Ok(CommentPage {
        comments,
        total,
        has_more,
    })
}

pub(crate) async fn resolve_play_url(
    video: &Video,
    cookie: Option<&str>,
    quality: u32,
) -> Result<PlayUrlResult, String> {
    let client = client();

    let mut cid = video.cid;
    let mut aid = video.aid;
    if cid == 0 || aid == 0 {
        let detail: Value = with_cookie(
            client
                .get("https://api.bilibili.com/x/web-interface/view")
                .header(
                    "Referer",
                    format!("https://www.bilibili.com/video/{}", video.bvid),
                )
                .query(&[("bvid", video.bvid.as_str())]),
            cookie,
        )
        .send()
        .await
        .map_err(|error| format!("获取视频详情失败：{error}"))?
        .json()
        .await
        .map_err(|error| format!("解析视频详情失败：{error}"))?;
        if number(detail.get("code")) != 0 {
            return Err(api_error(&detail, "视频详情"));
        }
        let data = detail.get("data");
        if cid == 0 {
            cid = number(data.and_then(|data| data.get("cid")));
        }
        aid = aid
            .max(number(data.and_then(|data| data.get("aid"))))
            .max(number(data.and_then(|data| data.get("id"))));
    }
    if cid == 0 {
        return Err("视频没有可用的 CID".into());
    }

    let mut params = BTreeMap::new();
    params.insert("bvid".into(), video.bvid.clone());
    params.insert("cid".into(), cid.to_string());
    params.insert("qn".into(), quality.to_string());
    // `fnval` is a bitfield of capabilities to ask for, and one of its bits asks for a P2P
    // CDN: with it, playurl answers with `mcdn.bilivideo.cn:8082` URLs that only resolve
    // through Bilibili's peer network and time out elsewhere. With plain DASH (1024) the same
    // video comes back on an ordinary edge host (`edge.*`). Measured: 4048 -> mcdn (18 s
    // timeout), 1024 -> edge (fetches). `fourk` below still asks for 4K, so this is not
    // giving up quality.
    params.insert("fnval".into(), "1024".into());
    params.insert("fnver".into(), "0".into());
    params.insert("fourk".into(), "1".into());
    params.insert("voice_balance".into(), "1".into());
    params.insert("gaia_source".into(), "pre-load".into());
    params.insert("isGaiaAvoided".into(), "true".into());
    params.insert("web_location".into(), "1315873".into());
    let referer = format!("https://www.bilibili.com/video/{}", video.bvid);
    let mut errors = Vec::new();

    // WBI 是当前接口，但接口策略会变化，所以失败时继续尝试旧接口。
    //
    // This block used to carry the fifth hand-copied WBI key extraction, and the only one that
    // bypassed the cache: every play fetched from `nav` keys it had already been given.
    match fetch_wbi_keys(client, cookie).await {
        Ok((img_key, sub_key)) => {
            let play_result = match with_cookie(
                client
                    .get("https://api.bilibili.com/x/player/wbi/playurl")
                    .header("Referer", &referer)
                    .query(&wbi_sign(&params, &img_key, &sub_key)),
                cookie,
            )
            .send()
            .await
            {
                Ok(response) => response
                    .json::<Value>()
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            match play_result {
                Ok(response) if number(response.get("code")) == 0 => {
                    if let Some(result) = parse_play_url(&response, cid, aid, quality) {
                        return Ok(result);
                    }
                    errors.push("WBI 接口没有返回 MP4 地址".to_string());
                }
                Ok(response) => errors.push(api_error(&response, "WBI 播放")),
                Err(error) => errors.push(format!("WBI 播放请求失败：{error}")),
            }
        }
        Err(error) => errors.push(format!("获取播放 WBI 密钥失败：{error}")),
    }

    // 部分视频或未登录请求会拒绝 WBI 播放接口，旧接口仍可返回 MP4。
    let legacy_result = match with_cookie(
        client
            .get("https://api.bilibili.com/x/player/playurl")
            .header("Referer", &referer)
            .query(&params),
        cookie,
    )
    .send()
    .await
    {
        Ok(response) => response
            .json::<Value>()
            .await
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    match legacy_result {
        Ok(response) if number(response.get("code")) == 0 => {
            if let Some(result) = parse_play_url(&response, cid, aid, quality) {
                return Ok(result);
            }
            errors.push("旧播放接口没有返回 MP4 地址".to_string());
        }
        Ok(response) => errors.push(format!(
            "旧播放接口返回错误 {}：{}",
            number(response.get("code")),
            text(response.get("message"))
        )),
        Err(error) => errors.push(format!("旧播放请求失败：{error}")),
    }

    Err(format!("无法获取播放地址：{}", errors.join("；")))
}

#[derive(Clone, Debug)]
pub(crate) struct PlayQuality {
    pub(crate) qn: u32,
    pub(crate) label: String,
    pub(crate) switchable: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PlayUrlResult {
    pub(crate) url: String,
    pub(crate) audio_url: Option<String>,
    pub(crate) cid: i64,
    pub(crate) aid: i64,
    pub(crate) actual_quality: u32,
    pub(crate) qualities: Vec<PlayQuality>,
}

fn parse_play_url(
    response: &Value,
    cid: i64,
    aid: i64,
    requested_qn: u32,
) -> Option<PlayUrlResult> {
    let data = response.get("data")?;
    let mut actual_quality = number(data.get("quality")).max(0) as u32;

    let mut video_url = String::new();
    let mut audio_url = None;

    if let Some(dash) = data.get("dash") {
        if let Some(videos) = dash.get("video").and_then(Value::as_array) {
            let mut candidates: Vec<(u32, u64, String)> = Vec::new();
            for v in videos {
                let id = number(v.get("id")).max(0) as u32;
                let bandwidth = number(v.get("bandwidth")).max(0) as u64;
                let url = text(v.get("baseUrl"));
                let url = if !url.is_empty() {
                    url
                } else {
                    let url = text(v.get("base_url"));
                    if !url.is_empty() {
                        url
                    } else {
                        v.get("backupUrl")
                            .or_else(|| v.get("backup_url"))
                            .and_then(Value::as_array)
                            .and_then(|arr| arr.first())
                            .map(|s| text(Some(s)))
                            .filter(|s| !s.is_empty())
                            .unwrap_or_default()
                    }
                };
                if !url.is_empty() {
                    candidates.push((id, bandwidth, url));
                }
            }

            if let Some((id, _, url)) = candidates.iter().find(|(id, _, _)| *id == requested_qn) {
                video_url = url.clone();
                actual_quality = *id;
            } else if let Some((id, _, url)) =
                candidates.iter().max_by_key(|(id, bw, _)| (*id, *bw))
            {
                video_url = url.clone();
                actual_quality = *id;
            }
        }

        let mut audio_candidates: Vec<(u64, String)> = Vec::new();
        let mut collect_audios = |audios_val: Option<&Value>| {
            if let Some(audios) = audios_val.and_then(Value::as_array) {
                for a in audios {
                    let bandwidth = number(a.get("bandwidth")).max(0) as u64;
                    let url = text(a.get("baseUrl"));
                    let url = if !url.is_empty() {
                        url
                    } else {
                        let url = text(a.get("base_url"));
                        if !url.is_empty() {
                            url
                        } else {
                            a.get("backupUrl")
                                .or_else(|| a.get("backup_url"))
                                .and_then(Value::as_array)
                                .and_then(|arr| arr.first())
                                .map(|s| text(Some(s)))
                                .filter(|s| !s.is_empty())
                                .unwrap_or_default()
                        }
                    };
                    if !url.is_empty() {
                        audio_candidates.push((bandwidth, url));
                    }
                }
            }
        };

        collect_audios(dash.get("audio"));
        collect_audios(dash.get("dolby").and_then(|d| d.get("audio")));
        collect_audios(dash.get("flac").and_then(|f| f.get("audio")));

        if let Some((_, url)) = audio_candidates.iter().max_by_key(|(bw, _)| *bw) {
            audio_url = Some(url.clone());
        }
    }

    if video_url.is_empty()
        && let Some(durl) = data
            .get("durl")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
    {
        video_url = text(durl.get("url"));
    }

    if video_url.is_empty() {
        return None;
    }

    let mut qualities = Vec::new();
    let dash_video_qualities = data
        .get("dash")
        .and_then(|dash| dash.get("video"))
        .and_then(Value::as_array)
        .map(|videos| {
            videos
                .iter()
                .map(|video| number(video.get("id")).max(0) as u32)
                .filter(|qn| *qn > 0)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(accepted) = data.get("accept_quality").and_then(Value::as_array) {
        let descriptions = data
            .get("accept_description")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (index, value) in accepted.iter().enumerate() {
            let qn = number(Some(value)).max(0) as u32;
            if qn == 0 || qualities.iter().any(|q: &PlayQuality| q.qn == qn) {
                continue;
            }
            let label = descriptions
                .get(index)
                .map(|description| text(Some(description)))
                .filter(|description| !description.is_empty())
                .unwrap_or_else(|| quality_label(qn));
            qualities.push(PlayQuality {
                qn,
                label,
                switchable: dash_video_qualities.contains(&qn)
                    || (dash_video_qualities.is_empty() && qn == actual_quality),
            });
        }
    }

    if let Some(dash) = data.get("dash")
        && let Some(videos) = dash.get("video").and_then(Value::as_array)
    {
        for v in videos {
            let qn = number(v.get("id")).max(0) as u32;
            if qn > 0 && !qualities.iter().any(|q| q.qn == qn) {
                qualities.push(PlayQuality {
                    qn,
                    label: quality_label(qn),
                    switchable: true,
                });
            }
        }
    }

    qualities.sort_by_key(|quality| std::cmp::Reverse(quality.qn));

    if qualities.is_empty() && actual_quality > 0 {
        qualities.push(PlayQuality {
            qn: actual_quality,
            label: quality_label(actual_quality),
            switchable: true,
        });
    }

    Some(PlayUrlResult {
        url: video_url,
        audio_url,
        cid,
        aid,
        actual_quality,
        qualities,
    })
}

pub(crate) fn quality_label(qn: u32) -> String {
    match qn {
        127 => "8K".into(),
        126 => "杜比视界".into(),
        125 => "HDR 真彩".into(),
        120 => "4K 超清".into(),
        116 => "1080P 60帧".into(),
        112 => "1080P 高码率".into(),
        80 => "1080P 高清".into(),
        74 => "720P 60帧".into(),
        64 => "720P 高清".into(),
        32 => "480P 清晰".into(),
        16 => "360P 流畅".into(),
        6 => "240P 极速".into(),
        _ => format!("{qn}P"),
    }
}

#[cfg(test)]
mod clock_tests {
    use super::Clock;

    fn read(seconds: f64, with_hours: bool) -> String {
        Clock::new(seconds, with_hours).as_str().to_string()
    }

    /// The shape is the caller's decision, and both readings of one clip use it.
    #[test]
    fn the_shape_is_the_clips_not_the_readings() {
        assert_eq!(read(0.0, false), "00:00");
        assert_eq!(read(59.0, false), "00:59");
        assert_eq!(read(3_599.0, false), "59:59");
        assert_eq!(read(3_599.0, true), "00:59:59");
        assert_eq!(read(3_600.0, true), "01:00:00");
        assert_eq!(read(7_384.0, true), "02:03:04");
    }

    /// A clip of an hour or more asks for hours, and one under it does not.
    #[test]
    fn hours_are_asked_for_at_an_hour() {
        assert!(!Clock::needs_hours(0.0));
        assert!(!Clock::needs_hours(3_599.9));
        assert!(Clock::needs_hours(3_600.0));
        assert!(Clock::needs_hours(7_384.0));
        assert!(!Clock::needs_hours(f64::NAN));
    }

    /// Every reading of a given shape is the same number of characters, which is what keeps the
    /// two ends of the seek bar the same width as the numbers change.
    #[test]
    fn a_shape_has_one_length() {
        for seconds in [0.0, 1.0, 9.0, 10.0, 59.0, 60.0, 599.0, 3_599.0] {
            assert_eq!(read(seconds, false).len(), 5, "mm:ss at {seconds}");
        }
        for seconds in [3_600.0, 3_661.0, 86_399.0] {
            assert_eq!(read(seconds, true).len(), 8, "hh:mm:ss at {seconds}");
        }
        assert_eq!(read(f64::NAN, false).len(), 5);
        assert_eq!(read(f64::NAN, true).len(), 8);
    }

    /// A very long clip no longer wraps to `00:00`: past 99 hours the hours field caps, so the
    /// reading is wrong at the far end rather than silently restarting.
    #[test]
    fn a_very_long_clip_does_not_wrap() {
        let hours = read(100.0 * 3_600.0, true);
        assert_eq!(hours, "99:00:00", "capped, not wrapped");
        assert_ne!(hours, "00:00:00");
    }


}
