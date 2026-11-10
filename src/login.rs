use image::Luma;
use qrcode::QrCode;
use reqwest::header::SET_COOKIE;
use serde_json::Value;
use std::{fs, path::PathBuf, sync::Arc};
use url::Url;

use crate::paths;

pub(crate) struct QrCodeData {
    pub(crate) key: String,
    pub(crate) image: Arc<crate::model::Rgba8>,
}

#[derive(Clone, Debug)]
pub(crate) struct UserSession {
    pub(crate) cookie: String,
    pub(crate) mid: i64,
    pub(crate) username: String,
    pub(crate) face: String,
    pub(crate) is_vip: bool,
}

pub(crate) enum PollResult {
    Waiting,
    Scanned,
    Expired,
    LoggedIn(UserSession),
}

// The HTTP client and the JSON helpers live in `api`: this module used to carry byte-identical
// copies of all four, which meant two connection pools with two sets of timeouts.
use crate::api::{client, number, text, with_cookie};
pub(crate) async fn fetch_qr_code() -> Result<QrCodeData, String> {
    let client = client();
    let response: Value = client
        .get("https://passport.bilibili.com/x/passport-login/web/qrcode/generate")
        .header("Referer", "https://www.bilibili.com/")
        .send()
        .await
        .map_err(|error| format!("获取登录二维码失败：{error}"))?
        .json()
        .await
        .map_err(|error| format!("解析登录二维码失败：{error}"))?;
    let code = number(response.get("code"));
    if code != 0 {
        return Err(format!(
            "登录二维码接口返回错误 {code}：{}",
            text(response.get("message"))
        ));
    }
    let data = response
        .get("data")
        .ok_or_else(|| "登录二维码数据为空".to_string())?;
    let url = text(data.get("url"));
    let key = text(data.get("qrcode_key"));
    if url.is_empty() || key.is_empty() {
        return Err("登录二维码缺少 URL 或 key".into());
    }
    let qr = QrCode::new(url.as_bytes()).map_err(|error| format!("生成登录二维码失败：{error}"))?;
    let image = qr
        .render::<Luma<u8>>()
        .min_dimensions(280, 280)
        .quiet_zone(true)
        .build();
    let rgba = image::DynamicImage::ImageLuma8(image).into_rgba8();
    Ok(QrCodeData {
        key,
        image: Arc::new(crate::model::Rgba8::new(
            rgba.width(),
            rgba.height(),
            rgba.into_raw(),
        )),
    })
}

pub(crate) async fn poll_qr_code(key: &str) -> Result<PollResult, String> {
    let client = client();
    let response = client
        .get("https://passport.bilibili.com/x/passport-login/web/qrcode/poll")
        .header("Referer", "https://www.bilibili.com/")
        .query(&[("qrcode_key", key)])
        .send()
        .await
        .map_err(|error| format!("轮询登录状态失败：{error}"))?;
    let header_cookie = response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("解析登录状态失败：{error}"))?;
    let data = body.get("data").unwrap_or(&Value::Null);
    match number(data.get("code")) {
        0 => {
            let cookie = merge_cookies(&header_cookie, &text(data.get("url")));
            if cookie.is_empty() {
                return Err("登录成功但没有获取到 Cookie".into());
            }
            let cookie_mid = cookie_value(&cookie, "DedeUserID")
                .and_then(|value| value.parse().ok())
                .unwrap_or_default();
            let user = fetch_user(cookie.clone()).await.ok();
            Ok(PollResult::LoggedIn(UserSession {
                cookie,
                mid: user.as_ref().map(|user| user.mid).unwrap_or(cookie_mid),
                username: user
                    .as_ref()
                    .map(|user| user.username.clone())
                    .unwrap_or_else(|| "已登录用户".into()),
                face: user
                    .as_ref()
                    .map(|user| user.face.clone())
                    .unwrap_or_default(),
                is_vip: user.as_ref().map(|user| user.is_vip).unwrap_or(false),
            }))
        }
        86090 => Ok(PollResult::Scanned),
        86038 => Ok(PollResult::Expired),
        86101 | 86039 => Ok(PollResult::Waiting),
        code => Err(format!(
            "登录状态返回错误 {code}：{}",
            text(body.get("message"))
        )),
    }
}

fn merge_cookies(header_cookie: &str, login_url: &str) -> String {
    let mut cookies = header_cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect::<Vec<_>>();
    if let Ok(url) = Url::parse(login_url) {
        for (name, value) in url.query_pairs() {
            if matches!(
                name.as_ref(),
                "DedeUserID" | "SESSDATA" | "bili_jct" | "buvid3"
            ) {
                if let Some(cookie) = cookies.iter_mut().find(|(key, _)| key == name.as_ref()) {
                    cookie.1 = value.into_owned();
                } else {
                    cookies.push((name.into_owned(), value.into_owned()));
                }
            }
        }
    }
    cookies
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    cookie.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

pub(crate) async fn fetch_user(cookie: String) -> Result<UserSession, String> {
    let client = client();
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/web-interface/nav")
            .header("Referer", "https://www.bilibili.com/"),
        Some(&cookie),
    )
    .send()
    .await
    .map_err(|error| format!("获取用户信息失败：{error}"))?
    .json()
    .await
    .map_err(|error| format!("解析用户信息失败：{error}"))?;
    if number(response.get("code")) != 0 {
        return Err(text(response.get("message")));
    }
    let data = response.get("data").unwrap_or(&Value::Null);
    Ok(UserSession {
        cookie: cookie.to_string(),
        mid: number(data.get("mid")),
        username: text(data.get("uname")),
        face: text(data.get("face")),
        is_vip: data
            .get("vip")
            .and_then(|vip| vip.get("status"))
            .map(|status| number(Some(status)) == 1)
            .unwrap_or(false),
    })
}

fn session_path() -> PathBuf {
    paths::app_dir().join("session")
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn legacy_session_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("biliguga")
        .join("session")
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn legacy_session_path() -> PathBuf {
    session_path()
}

/// Reads a session file.
///
/// JSON, written by `save_session` below. The file used to be five newline-separated lines,
/// which broke for any value containing a newline: a username that did leaked its second line
/// into the `face` field and shifted everything after it, so the session read back with the
/// wrong avatar and `is_vip` silently false. The line format is still parsed, because a file
/// written by an older build has to keep working.
fn parse_session(content: &str) -> Option<UserSession> {
    // A file that looks like JSON is JSON: falling through to the line parser would take `{}`
    // for a cookie string, which is how an empty session parsed as a logged-in one.
    if content.trim_start().starts_with('{') {
        return parse_session_json(content);
    }
    parse_session_lines(content)
}

fn parse_session_json(content: &str) -> Option<UserSession> {
    let value: Value = serde_json::from_str(content).ok()?;
    let cookie = text(value.get("cookie"));
    if cookie.is_empty() {
        return None;
    }
    Some(UserSession {
        cookie,
        mid: number(value.get("mid")),
        username: text(value.get("username")),
        face: text(value.get("face")),
        is_vip: value
            .get("is_vip")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// The five-line format older builds wrote. Kept only so an existing session survives the
/// upgrade, and kept out of `save_session` so nothing new is ever written this way.
fn parse_session_lines(content: &str) -> Option<UserSession> {
    let mut lines = content.lines();
    let cookie = lines.next()?.to_string();
    if cookie.is_empty() {
        return None;
    }
    Some(UserSession {
        cookie,
        mid: lines
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or_default(),
        username: lines.next().unwrap_or("已登录用户").to_string(),
        face: lines.next().unwrap_or_default().to_string(),
        is_vip: lines
            .next()
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
    })
}

pub(crate) fn save_session(session: &UserSession) -> Result<(), String> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建登录目录失败：{error}"))?;
    }
    // JSON, not five newline-separated lines: a value containing a newline used to shift every
    // field after it, so the session came back with the wrong avatar.
    let content = serde_json::json!({
        "cookie": session.cookie,
        "mid": session.mid,
        "username": session.username,
        "face": session.face,
        "is_vip": session.is_vip,
    })
    .to_string();
    fs::write(&path, content).map_err(|error| format!("保存登录状态失败：{error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("设置登录状态权限失败：{error}"))?;
    }
    Ok(())
}

pub(crate) fn load_session() -> Option<UserSession> {
    if let Ok(content) = fs::read_to_string(session_path())
        && let Some(session) = parse_session(&content)
    {
        return Some(session);
    }

    let legacy_path = legacy_session_path();
    let session = parse_session(&fs::read_to_string(&legacy_path).ok()?)?;
    let _ = save_session(&session);
    Some(session)
}

pub(crate) fn clear_session() {
    let _ = fs::remove_file(session_path());
    let legacy_path = legacy_session_path();
    if legacy_path != session_path() {
        let _ = fs::remove_file(legacy_path);
    }
}
