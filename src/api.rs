use crate::model::{Comment, CommentPage, Video};
use futures::channel::oneshot;
use gpui::RenderImage;
use image::Frame;
use md5::{Digest, Md5};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::blocking::{Client, RequestBuilder};
use serde_json::Value;
use smallvec::SmallVec;
use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
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

fn image_client() -> Option<&'static Client> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .user_agent("Mozilla/5.0 biliguga/0.1")
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(15))
                .build()
                .ok()
        })
        .as_ref()
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

fn download_image(url: &str, width: u32, height: u32) -> Option<Arc<RenderImage>> {
    if url.is_empty() {
        return None;
    }
    let request_url = thumbnail_url(url, width, height);
    let response = image_client()?
        .get(&request_url)
        .header("Referer", "https://www.bilibili.com/")
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = response.bytes().ok()?;
    let compressed_len = bytes.len();
    let source = image::load_from_memory(&bytes).ok()?.into_rgba8();
    drop(bytes);
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

pub(crate) fn download_cover(url: &str) -> Option<Arc<RenderImage>> {
    download_image(url, 160, 90)
}

struct CoverRequest {
    url: String,
    cancelled: Arc<AtomicBool>,
    reply: oneshot::Sender<Option<Arc<RenderImage>>>,
}

struct CoverWorkers {
    senders: Vec<mpsc::Sender<CoverRequest>>,
    next: AtomicUsize,
}

fn cover_workers() -> Option<&'static CoverWorkers> {
    const WORKER_COUNT: usize = 4;
    static WORKERS: OnceLock<Option<CoverWorkers>> = OnceLock::new();

    WORKERS
        .get_or_init(|| {
            let mut senders = Vec::with_capacity(WORKER_COUNT);
            for index in 0..WORKER_COUNT {
                let (sender, receiver) = mpsc::channel::<CoverRequest>();
                thread::Builder::new()
                    .name(format!("biliguga-cover-{index}"))
                    .spawn(move || {
                        while let Ok(request) = receiver.recv() {
                            let image = if request.cancelled.load(Ordering::Acquire) {
                                None
                            } else {
                                download_cover(&request.url)
                            };
                            let _ = request.reply.send(image);
                        }
                    })
                    .ok()?;
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
        .send(CoverRequest {
            url,
            cancelled,
            reply,
        })
        .ok()?;
    Some(receiver)
}

pub(crate) fn download_avatar(url: &str) -> Option<Arc<RenderImage>> {
    download_image(url, 160, 160)
}

fn parse_video(item: &Value, index: usize) -> Option<Video> {
    let bvid = text(item.get("bvid"));
    if bvid.is_empty() {
        return None;
    }
    let view = number(item.get("stat").and_then(|stat| stat.get("view")));
    let danmaku = number(item.get("stat").and_then(|stat| stat.get("danmaku")));
    Some(Video {
        bvid,
        aid: number(item.get("aid")),
        cid: number(item.get("cid")),
        title: text(item.get("title")),
        uploader: text(item.get("owner").and_then(|owner| owner.get("name"))),
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

pub(crate) fn fetch_recommendations() -> Result<Vec<Video>, String> {
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let nav: Value = client
        .get("https://api.bilibili.com/x/web-interface/nav")
        .send()
        .map_err(|error| format!("获取 WBI 密钥失败：{error}"))?
        .json()
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
    params.insert("feed_version".into(), "V8".into());
    params.insert("fresh_idx".into(), "1".into());
    params.insert("fresh_type".into(), "4".into());
    params.insert("homepage_ver".into(), "1".into());
    params.insert("ps".into(), "12".into());
    let signed = wbi_sign(&params, &img_key, &sub_key);
    let response: Value = client
        .get("https://api.bilibili.com/x/web-interface/wbi/index/top/feed/rcmd")
        .header("Referer", "https://www.bilibili.com/")
        .query(&signed)
        .send()
        .map_err(|error| format!("请求推荐流失败：{error}"))?
        .json()
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
        Ok(videos)
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

pub(crate) fn fetch_search_results(keyword: &str) -> Result<Vec<Video>, String> {
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let nav: Value = client
        .get("https://api.bilibili.com/x/web-interface/nav")
        .send()
        .map_err(|error| format!("获取搜索 WBI 密钥失败：{error}"))?
        .json()
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
        .map_err(|error| format!("请求 B 站搜索失败：{error}"))?
        .json()
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
        .map_err(|error| format!("请求 B 站综合搜索失败：{error}"))?
        .json()
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
        aid: number(history.get("aid")).max(number(item.get("aid"))),
        cid: number(history.get("cid")),
        title: clean_search_text(text(item.get("title"))),
        uploader: text(item.get("author_name")),
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

pub(crate) fn fetch_history(cookie: &str) -> Result<Vec<Video>, String> {
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
    .map_err(|error| format!("请求观看历史失败：{error}"))?
    .json()
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
    video.accent = accent_for(index);
    Some(video)
}

pub(crate) fn fetch_dynamic_feed(cookie: &str) -> Result<Vec<Video>, String> {
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
    .map_err(|error| format!("请求动态失败：{error}"))?
    .json()
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

pub(crate) fn fetch_watch_later(cookie: &str) -> Result<Vec<Video>, String> {
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
    .map_err(|error| format!("请求稍后再看失败：{error}"))?
    .json()
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

pub(crate) fn fetch_favorites(cookie: &str, mid: i64) -> Result<Vec<Video>, String> {
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
    .map_err(|error| format!("请求收藏夹失败：{error}"))?
    .json()
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
    .map_err(|error| format!("请求收藏视频失败：{error}"))?
    .json()
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

pub(crate) fn add_to_watch_later(cookie: &str, aid: i64) -> Result<(), String> {
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
    .map_err(|error| format!("添加稍后再看失败：{error}"))?
    .json()
    .map_err(|error| format!("解析稍后再看响应失败：{error}"))?;
    if number(response.get("code")) == 0 {
        Ok(())
    } else {
        Err(api_error(&response, "添加稍后再看"))
    }
}

pub(crate) fn add_to_favorites(cookie: &str, mid: i64, aid: i64) -> Result<(), String> {
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
    .map_err(|error| format!("请求默认收藏夹失败：{error}"))?
    .json()
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
    .map_err(|error| format!("收藏视频失败：{error}"))?
    .json()
    .map_err(|error| format!("解析收藏响应失败：{error}"))?;
    if number(response.get("code")) == 0 {
        Ok(())
    } else {
        Err(api_error(&response, "收藏视频"))
    }
}

pub(crate) fn like_video(cookie: &str, aid: i64) -> Result<(), String> {
    let csrf = csrf_from_cookie(cookie).ok_or_else(|| "登录状态缺少 CSRF 凭证".to_string())?;
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let response: Value = with_cookie(
        client
            .post("https://api.bilibili.com/x/web-interface/archive/like")
            .header("Referer", "https://www.bilibili.com/"),
        Some(cookie),
    )
    .form(&[
        ("aid", aid.to_string()),
        ("like", "1".into()),
        ("csrf", csrf),
    ])
    .send()
    .map_err(|error| format!("点赞失败：{error}"))?
    .json()
    .map_err(|error| format!("解析点赞响应失败：{error}"))?;
    if number(response.get("code")) == 0 {
        Ok(())
    } else {
        Err(api_error(&response, "点赞"))
    }
}

pub(crate) fn coin_video(cookie: &str, aid: i64) -> Result<(), String> {
    let csrf = csrf_from_cookie(cookie).ok_or_else(|| "登录状态缺少 CSRF 凭证".to_string())?;
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let response: Value = with_cookie(
        client
            .post("https://api.bilibili.com/x/web-interface/coin/add")
            .header("Referer", "https://www.bilibili.com/"),
        Some(cookie),
    )
    .form(&[
        ("aid", aid.to_string()),
        ("multiply", "1".into()),
        ("select_like", "0".into()),
        ("csrf", csrf),
    ])
    .send()
    .map_err(|error| format!("投币失败：{error}"))?
    .json()
    .map_err(|error| format!("解析投币响应失败：{error}"))?;
    if number(response.get("code")) == 0 {
        Ok(())
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

pub(crate) fn fetch_comments(
    video: &Video,
    cookie: Option<&str>,
    page: u32,
) -> Result<CommentPage, String> {
    if video.aid <= 0 {
        return Err("视频缺少 AV 号，无法获取评论".into());
    }
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())?;
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/v2/reply")
            .header(
                "Referer",
                format!("https://www.bilibili.com/video/{}", video.bvid),
            )
            .query(&[
                ("oid", video.aid.to_string()),
                ("type", "1".into()),
                ("pn", page.max(1).to_string()),
                ("ps", "20".into()),
                ("sort", "1".into()),
                ("web_location", "1315875".into()),
            ]),
        cookie,
    )
    .send()
    .map_err(|error| format!("获取评论失败：{error}"))?
    .json()
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
    use super::thumbnail_url;

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
}

pub(crate) fn resolve_play_url(video: &Video, cookie: Option<&str>) -> Result<String, String> {
    let client = Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| error.to_string())?;

    let mut cid = video.cid;
    if cid == 0 {
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
        .map_err(|error| format!("获取视频详情失败：{error}"))?
        .json()
        .map_err(|error| format!("解析视频详情失败：{error}"))?;
        if number(detail.get("code")) != 0 {
            return Err(format!(
                "视频详情接口返回错误 {}：{}",
                number(detail.get("code")),
                text(detail.get("message"))
            ));
        }
        cid = number(detail.get("data").and_then(|data| data.get("cid")));
    }
    if cid == 0 {
        return Err("视频没有可用的 CID".into());
    }

    let mut params = BTreeMap::new();
    params.insert("bvid".into(), video.bvid.clone());
    params.insert("cid".into(), cid.to_string());
    params.insert("qn".into(), "64".into());
    params.insert("fnval".into(), "1".into());
    params.insert("fnver".into(), "0".into());
    params.insert("fourk".into(), "1".into());
    params.insert("platform".into(), "html5".into());
    params.insert("high_quality".into(), "1".into());
    params.insert("web_location".into(), "1315873".into());

    let referer = format!("https://www.bilibili.com/video/{}", video.bvid);
    let mut errors = Vec::new();

    // WBI 是当前接口，但接口策略会变化，所以失败时继续尝试旧接口。
    match with_cookie(
        client.get("https://api.bilibili.com/x/web-interface/nav"),
        cookie,
    )
    .send()
    .and_then(|response| response.json::<Value>())
    {
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
                    match with_cookie(
                        client
                            .get("https://api.bilibili.com/x/player/wbi/playurl")
                            .header("Referer", &referer)
                            .query(&wbi_sign(&params, &img_key, &sub_key)),
                        cookie,
                    )
                    .send()
                    .and_then(|response| response.json::<Value>())
                    {
                        Ok(response) if number(response.get("code")) == 0 => {
                            if let Some(url) = first_durl(&response) {
                                return Ok(url);
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
    match with_cookie(
        client
            .get("https://api.bilibili.com/x/player/playurl")
            .header("Referer", &referer)
            .query(&params),
        cookie,
    )
    .send()
    .and_then(|response| response.json::<Value>())
    {
        Ok(response) if number(response.get("code")) == 0 => {
            if let Some(url) = first_durl(&response) {
                return Ok(url);
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

fn first_durl(response: &Value) -> Option<String> {
    response
        .get("data")
        .and_then(|data| data.get("durl"))
        .and_then(Value::as_array)
        .and_then(|durl| durl.first())
        .map(|durl| text(durl.get("url")))
        .filter(|url| !url.is_empty())
}
