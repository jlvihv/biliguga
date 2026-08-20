use gpui::RenderImage;
use image::{Frame, Luma};
use qrcode::QrCode;
use reqwest::header::SET_COOKIE;
use reqwest::{Client, RequestBuilder};
use serde_json::Value;
use smallvec::SmallVec;
use std::{fs, path::PathBuf, sync::Arc, time::Duration};
use url::Url;

pub(crate) struct QrCodeData {
    pub(crate) key: String,
    pub(crate) image: Arc<RenderImage>,
}

#[derive(Clone, Debug)]
pub(crate) struct UserSession {
    pub(crate) cookie: String,
    pub(crate) mid: i64,
    pub(crate) username: String,
    pub(crate) face: String,
}

pub(crate) enum PollResult {
    Waiting,
    Scanned,
    Expired,
    LoggedIn(UserSession),
}

fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent("Mozilla/5.0 biliguga/0.1")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())
}

fn with_cookie(request: RequestBuilder, cookie: &str) -> RequestBuilder {
    if cookie.is_empty() {
        request
    } else {
        request.header("Cookie", cookie)
    }
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string().trim_matches('"').to_string(),
        None => String::new(),
    }
}

fn number(value: Option<&Value>) -> i64 {
    value
        .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
        .unwrap_or_default()
}

pub(crate) async fn fetch_qr_code() -> Result<QrCodeData, String> {
    let client = client()?;
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
        image: Arc::new(RenderImage::new(SmallVec::from_elem(Frame::new(rgba), 1))),
    })
}

pub(crate) async fn poll_qr_code(key: &str) -> Result<PollResult, String> {
    let client = client()?;
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
            let user = fetch_user(&cookie).await.ok();
            Ok(PollResult::LoggedIn(UserSession {
                cookie,
                mid: user.as_ref().map(|user| user.mid).unwrap_or(cookie_mid),
                username: user
                    .as_ref()
                    .map(|user| user.username.clone())
                    .unwrap_or_else(|| "已登录用户".into()),
                face: user.map(|user| user.face).unwrap_or_default(),
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

async fn fetch_user(cookie: &str) -> Result<UserSession, String> {
    let client = client()?;
    let response: Value = with_cookie(
        client
            .get("https://api.bilibili.com/x/web-interface/nav")
            .header("Referer", "https://www.bilibili.com/"),
        cookie,
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
    })
}

fn session_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library")
        .join("Application Support");

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    base.join("biliguga").join("session")
}

pub(crate) fn save_session(session: &UserSession) -> Result<(), String> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建登录目录失败：{error}"))?;
    }
    let content = format!(
        "{}\n{}\n{}\n{}\n",
        session.cookie, session.mid, session.username, session.face
    );
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
    let content = fs::read_to_string(session_path()).ok()?;
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
    })
}

pub(crate) fn clear_session() {
    let _ = fs::remove_file(session_path());
}
