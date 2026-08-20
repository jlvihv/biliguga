use crate::{
    model::{Comment, CommentPage, Video, VideoCollection},
    network,
};
use futures::{
    StreamExt,
    channel::{mpsc as async_mpsc, oneshot},
};
use gpui::RenderImage;
use image::Frame;
use md5::{Digest, Md5};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::{Client, RequestBuilder};
use serde_json::Value;
use smallvec::SmallVec;
use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
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

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string().trim_matches('"').to_string(),
        None => String::new(),
    }
}

fn with_cookie(request: RequestBuilder, cookie: Option<&str>) -> RequestBuilder {
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

fn number(value: Option<&Value>) -> i64 {
    value
        .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
        .unwrap_or_default()
}

fn compact_number(value: i64) -> String {
    if value >= 100_000_000 {
        format!("{:.1}亿", value as f64 / 100_000_000.0)
    } else if value >= 10_000 {
        format!("{:.1}万", value as f64 / 10_000.0)
    } else {
        value.to_string()
    }
}

fn duration(value: Option<&Value>) -> String {
    let seconds = number(value);
    if seconds <= 0 {
        return "--:--".into();
    }
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

pub(crate) fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0. {
        return "--:--".into();
    }
    let seconds = seconds as u64;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
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

fn decode_image(bytes: &[u8], width: u32, height: u32) -> Option<Arc<RenderImage>> {
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
    let mut rgba = image::imageops::thumbnail(&source, width, height);
    drop(source);
    for pixel in rgba.pixels_mut() {
        let [red, green, blue, alpha] = pixel.0;
        pixel.0 = [blue, green, red, alpha];
    }
    Some(Arc::new(RenderImage::new(SmallVec::from_elem(
        Frame::new(rgba),
        1,
    ))))
}

fn cover_image_client() -> Option<Client> {
    Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .ok()
}

async fn download_cover_async(
    client: &Client,
    url: &str,
    cancelled: &AtomicBool,
) -> Option<Arc<RenderImage>> {
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
    reply: oneshot::Sender<Option<Arc<RenderImage>>>,
}

struct CoverWorkers {
    senders: Vec<async_mpsc::UnboundedSender<CoverRequest>>,
    next: AtomicUsize,
}

const COVER_WORKER_COUNT: usize = 2;
const COVER_REQUESTS_PER_WORKER: usize = 4;

fn cover_workers() -> Option<&'static CoverWorkers> {
    static WORKERS: OnceLock<Option<CoverWorkers>> = OnceLock::new();

    WORKERS
        .get_or_init(|| {
            let mut senders = Vec::with_capacity(COVER_WORKER_COUNT);
            for _ in 0..COVER_WORKER_COUNT {
                let (sender, receiver) = async_mpsc::unbounded::<CoverRequest>();
                network::detach(async move {
                    let Some(client) = cover_image_client() else {
                        return;
                    };
                    let mut results = receiver
                        .map(|request| {
                            let client = client.clone();
                            async move {
                                let image =
                                    download_cover_async(&client, &request.url, &request.cancelled)
                                        .await;
                                (request.reply, image)
                            }
                        })
                        .buffer_unordered(COVER_REQUESTS_PER_WORKER);
                    while let Some((reply, image)) = results.next().await {
                        let _ = reply.send(image);
                    }
                });
                senders.push(sender);
            }
            Some(CoverWorkers {
                senders,
                next: AtomicUsize::new(0),
            })
        })
        .as_ref()
}

pub(crate) fn queue_cover_download(
    url: String,
    cancelled: Arc<AtomicBool>,
) -> Option<oneshot::Receiver<Option<Arc<RenderImage>>>> {
    let workers = cover_workers()?;
    let (reply, receiver) = oneshot::channel();
    let index = workers.next.fetch_add(1, Ordering::Relaxed) % workers.senders.len();
    workers.senders[index]
        .unbounded_send(CoverRequest {
            url,
            cancelled,
            reply,
        })
        .ok()?;
    Some(receiver)
}

pub(crate) async fn download_avatar(url: &str) -> Option<Arc<RenderImage>> {
    if url.is_empty() {
        return None;
    }
    let client = cover_image_client()?;
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

fn parse_video(item: &Value, index: usize) -> Option<Video> {
    let bvid = text(item.get("bvid"));
    if bvid.is_empty() {
        return None;
    }
    // 推荐接口使用 `id` 表示 AV 号，详情/其它接口通常使用 `aid`。
    // 两者都兼容，否则推荐流中的视频无法上报观看记录和续播进度。
    let aid = number(item.get("aid"));
    let aid = if aid > 0 { aid } else { number(item.get("id")) };
    let view = number(item.get("stat").and_then(|stat| stat.get("view")));
    let danmaku = number(item.get("stat").and_then(|stat| stat.get("danmaku")));
    Some(Video {
        bvid,
        aid,
        cid: number(item.get("cid")),
        title: text(item.get("title")),
        uploader: text(item.get("owner").and_then(|owner| owner.get("name"))),
        uploader_mid: number(item.get("owner").and_then(|owner| owner.get("mid"))),
        stats: format!(
            "{}播放  ·  {}弹幕",
            compact_number(view),
            compact_number(danmaku)
        ),
        duration: duration(item.get("duration")),
        cover: https_url(text(item.get("pic"))),
        cover_image: None,
        accent: accent_for(index),
        category: text(item.get("tname")),
    })
}

fn clean_search_text(value: String) -> String {
    let mut plain = String::with_capacity(value.len());
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => plain.push(ch),
            _ => {}
        }
    }
    plain
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn parse_search_video(item: &Value, index: usize) -> Option<Video> {
    let mut bvid = text(item.get("bvid"));
    if bvid.is_empty() {
        bvid = text(item.get("arcurl"))
            .split('/')
            .next_back()
            .unwrap_or("")
            .to_string();
    }
    if bvid.is_empty() {
        return None;
    }
    let title = clean_search_text(text(item.get("title")));
    let play = clean_search_text(text(item.get("play")));
    let danmaku = clean_search_text(text(item.get("video_review")));
    let duration = clean_search_text(text(item.get("duration")));
    Some(Video {
        bvid,
        aid: number(item.get("aid")),
        cid: 0,
        title,
        uploader: clean_search_text(text(item.get("author"))),
        uploader_mid: number(item.get("mid")),
        stats: format!(
            "{}播放  ·  {}弹幕",
            if play.is_empty() { "0" } else { &play },
            if danmaku.is_empty() { "0" } else { &danmaku }
        ),
        duration: if duration.is_empty() {
            "--:--".into()
        } else {
            duration
        },
        cover: https_url(text(item.get("pic"))),
        cover_image: None,
        accent: accent_for(index),
        category: clean_search_text(text(item.get("typename"))),
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

async fn fetch_wbi_keys(client: &Client, cookie: Option<&str>) -> Result<(String, String), String> {
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
    let img_key = text(wbi_img.get("img_url"))
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string();
    let sub_key = text(wbi_img.get("sub_url"))
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string();
    if img_key.is_empty() || sub_key.is_empty() {
        Err("B 站 WBI 密钥为空".into())
    } else {
        Ok((img_key, sub_key))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct VideoContext {
    pub(crate) aid: i64,
    pub(crate) cid: i64,
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
                let bvid = text(episode.get("bvid"));
                let bvid = if bvid.is_empty() {
                    text(arc.get("bvid"))
                } else {
                    bvid
                };
                if bvid.is_empty() {
                    continue;
                }
                let aid = number(episode.get("aid")).max(number(arc.get("aid")));
                let cid = number(episode.get("cid"));
                let episode_title = {
                    let title = clean_search_text(text(episode.get("title")));
                    if title.is_empty() {
                        clean_search_text(text(arc.get("title")))
                    } else {
                        title
                    }
                };
                let stat = arc.get("stat");
                episodes.push(Video {
                    bvid,
                    aid,
                    cid,
                    title: episode_title,
                    uploader: owner_name.to_string(),
                    uploader_mid: owner_mid,
                    stats: format!(
                        "{}播放  ·  {}弹幕",
                        compact_number(number(stat.and_then(|stat| stat.get("view")))),
                        compact_number(number(stat.and_then(|stat| stat.get("danmaku"))))
                    ),
                    duration: {
                        let duration_text = text(arc.get("duration"));
                        if duration_text.is_empty() {
                            duration(arc.get("duration"))
                        } else {
                            duration_text
                        }
                    },
                    cover: https_url(text(arc.get("pic"))),
                    cover_image: None,
                    accent: accent_for(index),
                    category: "合集".into(),
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
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
    Ok(VideoContext {
        aid: number(data.get("aid")).max(video.aid),
        cid: number(data.get("cid")).max(video.cid),
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
    let bvid = text(item.get("bvid"));
    if bvid.is_empty() {
        return None;
    }
    let title = clean_search_text(text(item.get("title")));
    let duration_text = text(item.get("length"));
    Some(Video {
        bvid,
        aid: number(item.get("aid")),
        cid: 0,
        title,
        uploader: text(item.get("author")),
        uploader_mid: mid,
        stats: format!(
            "{}播放  ·  {}评论",
            compact_number(number(item.get("play"))),
            compact_number(number(item.get("comment")))
        ),
        duration: if duration_text.is_empty() {
            duration(item.get("duration"))
        } else {
            duration_text
        },
        cover: https_url(text(item.get("pic"))),
        cover_image: None,
        accent: accent_for(index),
        category: text(item.get("typename")),
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let (img_key, sub_key) = fetch_wbi_keys(&client, cookie).await?;
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
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
    let img_key = text(wbi_img.get("img_url"))
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string();
    let sub_key = text(wbi_img.get("sub_url"))
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string();
    if img_key.is_empty() || sub_key.is_empty() {
        return Err("B 站 WBI 密钥为空".into());
    }

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
    let code = number(response.get("code"));
    if code != 0 {
        return Err(format!(
            "推荐接口返回错误 {code}：{}",
            text(response.get("message"))
        ));
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let nav: Value = client
        .get("https://api.bilibili.com/x/web-interface/nav")
        .send()
        .await
        .map_err(|error| format!("获取搜索 WBI 密钥失败：{error}"))?
        .json()
        .await
        .map_err(|error| format!("解析搜索 WBI 密钥失败：{error}"))?;
    let wbi_img = nav
        .get("data")
        .and_then(|data| data.get("wbi_img"))
        .ok_or_else(|| "B 站没有返回搜索 WBI 密钥".to_string())?;
    let img_key = text(wbi_img.get("img_url"))
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string();
    let sub_key = text(wbi_img.get("sub_url"))
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string();
    if img_key.is_empty() || sub_key.is_empty() {
        return Err("B 站搜索 WBI 密钥为空".into());
    }

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
        return Err(format!(
            "搜索接口返回错误 {code}：{}",
            text(response.get("message"))
        ));
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
    let bvid = text(history.get("bvid"));
    if bvid.is_empty() {
        return None;
    }
    let stat = item.get("stat");
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
        title: clean_search_text(text(item.get("title"))),
        uploader: text(item.get("author_name")),
        uploader_mid: number(item.get("author_mid")),
        stats: format!(
            "{}播放  ·  {}弹幕",
            compact_number(number(stat.and_then(|stat| stat.get("view")))),
            compact_number(number(stat.and_then(|stat| stat.get("danmaku"))))
        ),
        duration: duration(item.get("duration")),
        cover: https_url(cover),
        cover_image: None,
        accent: accent_for(index),
        category: "观看历史".into(),
    })
}

pub(crate) async fn fetch_history(cookie: &str) -> Result<Vec<Video>, String> {
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
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
    let code = number(response.get("code"));
    if code != 0 {
        return Err(format!(
            "观看历史接口返回错误 {code}：{}",
            text(response.get("message"))
        ));
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let nav: Value = with_cookie(
        client.get("https://api.bilibili.com/x/web-interface/nav"),
        cookie,
    )
    .send()
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    let wbi_img = nav.get("data")?.get("wbi_img")?;
    let img_key = text(wbi_img.get("img_url"))
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string();
    let sub_key = text(wbi_img.get("sub_url"))
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string();
    if img_key.is_empty() || sub_key.is_empty() {
        return None;
    }
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
    let bvid = text(archive.get("bvid"));
    if bvid.is_empty() {
        return None;
    }
    let stat = archive.get("stat");
    Some(Video {
        bvid,
        aid: text(archive.get("aid")).parse().unwrap_or_default(),
        cid: 0,
        title: clean_search_text(text(archive.get("title"))),
        uploader: String::new(),
        uploader_mid: 0,
        stats: format!(
            "{}播放  ·  {}弹幕",
            text(stat.and_then(|stat| stat.get("play"))),
            text(stat.and_then(|stat| stat.get("danmaku")))
        ),
        duration: {
            let value = text(archive.get("duration_text"));
            if value.is_empty() {
                "--:--".into()
            } else {
                value
            }
        },
        cover: https_url(text(archive.get("cover"))),
        cover_image: None,
        accent: 0x3e4654,
        category: "动态".into(),
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
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
    format!(
        "{operation}接口返回错误 {}：{}",
        number(response.get("code")),
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
    let bvid = text(item.get("bvid"));
    if bvid.is_empty() {
        return None;
    }
    let stat = item.get("stat");
    Some(Video {
        bvid,
        aid: number(item.get("aid")),
        cid: number(item.get("cid")),
        title: clean_search_text(text(item.get("title"))),
        uploader: text(item.get("owner").and_then(|owner| owner.get("name"))),
        uploader_mid: number(item.get("owner").and_then(|owner| owner.get("mid"))),
        stats: format!(
            "{}播放  ·  {}弹幕",
            compact_number(number(stat.and_then(|stat| stat.get("view")))),
            compact_number(number(stat.and_then(|stat| stat.get("danmaku"))))
        ),
        duration: duration(item.get("duration")),
        cover: https_url(text(item.get("pic"))),
        cover_image: None,
        accent: accent_for(index),
        category: "稍后再看".into(),
    })
}

pub(crate) async fn fetch_watch_later(cookie: &str) -> Result<Vec<Video>, String> {
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
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
    let bvid = {
        let bvid = text(item.get("bvid"));
        if bvid.is_empty() {
            text(item.get("bv_id"))
        } else {
            bvid
        }
    };
    if bvid.is_empty() {
        return None;
    }
    let ugc = item.get("ugc");
    let upper = item.get("upper");
    let stat = item.get("cnt_info");
    Some(Video {
        bvid,
        aid: number(item.get("id")),
        cid: number(ugc.and_then(|ugc| ugc.get("first_cid"))),
        title: clean_search_text(text(item.get("title"))),
        uploader: text(upper.and_then(|upper| upper.get("name"))),
        uploader_mid: number(upper.and_then(|upper| upper.get("mid"))),
        stats: format!(
            "{}播放  ·  {}弹幕",
            compact_number(number(stat.and_then(|stat| stat.get("play")))),
            compact_number(number(stat.and_then(|stat| stat.get("danmaku"))))
        ),
        duration: duration(item.get("duration")),
        cover: https_url(text(item.get("cover"))),
        cover_image: None,
        accent: accent_for(index),
        category: "收藏夹".into(),
    })
}

pub(crate) async fn fetch_favorites(cookie: &str, mid: i64) -> Result<Vec<Video>, String> {
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
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
    let client = Client::builder()
        .user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        )
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
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
    let client = Client::builder()
        .user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        )
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
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
    let message = clean_search_text(text(
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
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::{
        parse_author_video, parse_collection, parse_history_video, parse_play_url, parse_video,
        thumbnail_url,
    };
    use serde_json::json;

    #[test]
    fn recommendation_id_is_used_as_video_aid() {
        let video = parse_video(
            &json!({
                "id": 123456,
                "bvid": "BV1xx411c7mD",
                "cid": 789,
                "title": "test",
                "pic": "//example.com/cover.jpg",
                "duration": 60,
                "owner": {"name": "tester"},
                "stat": {"view": 1, "danmaku": 2}
            }),
            0,
        )
        .expect("recommendation item should parse");

        assert_eq!(video.aid, 123456);
    }

    #[test]
    fn history_oid_is_used_as_video_aid() {
        let video = parse_history_video(
            &json!({
                "title": "test",
                "duration": 60,
                "history": {
                    "oid": 654321,
                    "bvid": "BV1xx411c7mD",
                    "cid": 789
                }
            }),
            0,
        )
        .expect("history item should parse");

        assert_eq!(video.aid, 654321);
    }

    #[test]
    fn bilibili_thumbnail_url_requests_small_cdn_image() {
        assert_eq!(
            thumbnail_url(
                "http://i0.hdslb.com/bfs/archive/cover.jpg@672w_378h.webp?legacy=1",
                160,
                90,
            ),
            "https://i0.hdslb.com/bfs/archive/cover.jpg@160w_90h_1c.webp",
        );
    }

    #[test]
    fn non_bilibili_thumbnail_url_is_unchanged() {
        assert_eq!(
            thumbnail_url("https://example.com/cover.jpg", 160, 90),
            "https://example.com/cover.jpg",
        );
    }

    #[test]
    fn play_url_uses_bilibili_quality_metadata() {
        let result = parse_play_url(
            &json!({
                "data": {
                    "quality": 64,
                    "durl": [{"url": "https://cdn.example/video.mp4"}],
                    "accept_quality": [16, 32, 64],
                    "accept_description": ["360P", "480P", "720P"]
                }
            }),
            789,
            123456,
        )
        .expect("play url should parse");

        assert_eq!(result.actual_quality, 64);
        assert_eq!(result.qualities.len(), 3);
        assert_eq!(result.qualities[1].qn, 32);
        assert_eq!(result.qualities[1].label, "480P");
    }

    #[test]
    fn collection_parses_sections_and_uses_section_season_id() {
        let collection = parse_collection(
            &json!({
                "name": "测试合集",
                "sections": [{
                    "season_id": 9001,
                    "episodes": [{
                        "aid": 123,
                        "bvid": "BV1collection",
                        "cid": 456,
                        "title": "第一集",
                        "arc": {
                            "pic": "//example.com/cover.jpg",
                            "duration": 61,
                            "stat": {"view": 10, "danmaku": 2}
                        }
                    }]
                }]
            }),
            7788,
            "作者",
        )
        .expect("collection should parse");

        assert_eq!(collection.id, 9001);
        assert_eq!(collection.title, "测试合集");
        assert_eq!(collection.episodes[0].cid, 456);
        assert_eq!(collection.episodes[0].uploader_mid, 7788);
    }

    #[test]
    fn author_video_parser_maps_author_uid_and_stats() {
        let video = parse_author_video(
            &json!({
                "aid": 123,
                "bvid": "BV1author",
                "title": "作者投稿",
                "pic": "//example.com/cover.jpg",
                "author": "作者",
                "play": 10000,
                "comment": 20,
                "length": "01:02",
                "typename": "知识"
            }),
            0,
            7788,
        )
        .expect("author video should parse");

        assert_eq!(video.uploader_mid, 7788);
        assert_eq!(video.duration, "01:02");
        assert!(video.stats.contains("1.0万播放"));
    }
}

pub(crate) async fn resolve_play_url(
    video: &Video,
    cookie: Option<&str>,
    quality: u32,
) -> Result<PlayUrlResult, String> {
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| error.to_string())?;

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
            return Err(format!(
                "视频详情接口返回错误 {}：{}",
                number(detail.get("code")),
                text(detail.get("message"))
            ));
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
    params.insert("fnval".into(), "1".into());
    params.insert("fnver".into(), "0".into());
    params.insert("fourk".into(), "1".into());
    params.insert("platform".into(), "html5".into());
    params.insert("high_quality".into(), "1".into());
    params.insert("web_location".into(), "1315873".into());

    let referer = format!("https://www.bilibili.com/video/{}", video.bvid);
    let mut errors = Vec::new();

    // WBI 是当前接口，但接口策略会变化，所以失败时继续尝试旧接口。
    let nav_result = match with_cookie(
        client.get("https://api.bilibili.com/x/web-interface/nav"),
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
    match nav_result {
        Ok(nav) => {
            if let Some(wbi_img) = nav.get("data").and_then(|data| data.get("wbi_img")) {
                let img_key = text(wbi_img.get("img_url"))
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .to_string();
                let sub_key = text(wbi_img.get("sub_url"))
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !img_key.is_empty() && !sub_key.is_empty() {
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
                            if let Some(result) = parse_play_url(&response, cid, aid) {
                                return Ok(result);
                            }
                            errors.push("WBI 接口没有返回 MP4 地址".to_string());
                        }
                        Ok(response) => errors.push(format!(
                            "WBI 播放接口返回错误 {}：{}",
                            number(response.get("code")),
                            text(response.get("message"))
                        )),
                        Err(error) => errors.push(format!("WBI 播放请求失败：{error}")),
                    }
                } else {
                    errors.push("播放 WBI 密钥为空".to_string());
                }
            } else {
                errors.push("B 站没有返回播放 WBI 密钥".to_string());
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
            if let Some(result) = parse_play_url(&response, cid, aid) {
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
}

#[derive(Clone, Debug)]
pub(crate) struct PlayUrlResult {
    pub(crate) url: String,
    pub(crate) cid: i64,
    pub(crate) aid: i64,
    pub(crate) actual_quality: u32,
    pub(crate) qualities: Vec<PlayQuality>,
}

fn parse_play_url(response: &Value, cid: i64, aid: i64) -> Option<PlayUrlResult> {
    let data = response.get("data")?;
    let url = data
        .get("durl")
        .and_then(Value::as_array)
        .and_then(|durl| durl.first())
        .map(|durl| text(durl.get("url")))
        .filter(|url| !url.is_empty())?;
    let actual_quality = number(data.get("quality")).max(0) as u32;
    let mut qualities = Vec::new();
    if let Some(accepted) = data.get("accept_quality").and_then(Value::as_array) {
        let descriptions = data
            .get("accept_description")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (index, value) in accepted.iter().enumerate() {
            let qn = number(Some(value)).max(0) as u32;
            if qn == 0
                || qualities
                    .iter()
                    .any(|quality: &PlayQuality| quality.qn == qn)
            {
                continue;
            }
            let label = descriptions
                .get(index)
                .map(|description| text(Some(description)))
                .filter(|description| !description.is_empty())
                .unwrap_or_else(|| quality_label(qn));
            qualities.push(PlayQuality { qn, label });
        }
    }
    if qualities.is_empty() && actual_quality > 0 {
        qualities.push(PlayQuality {
            qn: actual_quality,
            label: quality_label(actual_quality),
        });
    }
    Some(PlayUrlResult {
        url,
        cid,
        aid,
        actual_quality,
        qualities,
    })
}

pub(crate) fn quality_label(qn: u32) -> String {
    match qn {
        6 => "240P".into(),
        16 => "360P".into(),
        32 => "480P".into(),
        64 => "720P".into(),
        80 => "1080P".into(),
        112 => "1080P+".into(),
        116 => "1080P60".into(),
        120 => "4K".into(),
        125 => "HDR".into(),
        126 => "杜比视界".into(),
        127 => "8K".into(),
        _ => format!("QN {qn}"),
    }
}
