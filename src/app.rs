use crate::{
    api::{
        download_cover, fetch_recommendations, fetch_search_results, format_time, resolve_play_url,
    },
    login::{self, PollResult, UserSession},
    model::{LOADING_VIDEO, Video},
    mpv,
    search_input::{SearchInput, bind_search_keys},
};
use gpui::{
    App, Application, Bounds, ClickEvent, Context, DispatchPhase, Entity, FontWeight, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, RenderImage, SharedString, Timer, Window,
    WindowBounds, WindowOptions, canvas, div, img, prelude::*, px, relative, rgb, size,
    uniform_list,
};
use reqwest::blocking::Client;
use std::{ops::Range, sync::Arc, time::Duration};

struct BiliGuga {
    search_input: Entity<SearchInput>,
    search_query: String,
    active_tab: AppTab,
    videos: Vec<Video>,
    search_results: Vec<Video>,
    selected: usize,
    loading: bool,
    session: Option<UserSession>,
    login_image: Option<Arc<RenderImage>>,
    login_key: Option<String>,
    login_loading: bool,
    login_status: SharedString,
    login_generation: u64,
    playback: PlaybackState,
    playback_request: u64,
    volume: f64,
    speed: f64,
    controls_visible: bool,
    controls_generation: u64,
    message: SharedString,
    player: mpv::MpvPlayer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppTab {
    Home,
    Search,
    Login,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaybackState {
    Idle,
    Buffering,
    Playing,
    Paused,
}

impl BiliGuga {
    fn new(cx: &mut Context<Self>) -> Self {
        let session = login::load_session();
        let login_status = session
            .as_ref()
            .map(|session| format!("已登录：{}", session.username))
            .unwrap_or_else(|| "使用哔哩哔哩 App 扫码登录".into());
        Self {
            search_input: cx.new(SearchInput::new),
            search_query: String::new(),
            active_tab: AppTab::Home,
            videos: Vec::new(),
            search_results: Vec::new(),
            selected: 0,
            loading: true,
            session,
            login_image: None,
            login_key: None,
            login_loading: false,
            login_status: SharedString::from(login_status),
            login_generation: 0,
            playback: PlaybackState::Idle,
            playback_request: 0,
            volume: 100.,
            speed: 1.,
            controls_visible: false,
            controls_generation: 0,
            message: SharedString::from("正在从 B 站加载推荐视频…"),
            player: mpv::MpvPlayer::new(),
        }
    }

    fn current_videos(&self) -> &Vec<Video> {
        match self.active_tab {
            AppTab::Home => &self.videos,
            AppTab::Search => &self.search_results,
            AppTab::Login => &self.videos,
        }
    }

    fn current_videos_mut(&mut self) -> &mut Vec<Video> {
        match self.active_tab {
            AppTab::Home => &mut self.videos,
            AppTab::Search => &mut self.search_results,
            AppTab::Login => &mut self.videos,
        }
    }

    fn stop_current_playback(&mut self) {
        self.player.stop_playback();
        self.selected = 0;
        self.playback = PlaybackState::Idle;
        self.playback_request = self.playback_request.wrapping_add(1);
        self.controls_visible = false;
        self.controls_generation = self.controls_generation.wrapping_add(1);
    }

    fn show_home(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab != AppTab::Home {
            self.active_tab = AppTab::Home;
            self.stop_current_playback();
            self.message = SharedString::from("首页推荐");
            cx.notify();
        }
    }

    fn show_search(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab != AppTab::Search {
            self.active_tab = AppTab::Search;
            self.stop_current_playback();
            self.message = if self.search_results.is_empty() {
                SharedString::from("输入关键词搜索视频")
            } else {
                SharedString::from(format!("找到 {} 个视频", self.search_results.len()))
            };
            cx.notify();
        }
    }

    fn show_login(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab != AppTab::Login {
            self.active_tab = AppTab::Login;
            self.stop_current_playback();
            self.message = SharedString::from("登录账号后可使用更多 B 站功能");
            cx.notify();
        }
        if self.session.is_none() && self.login_image.is_none() && !self.login_loading {
            self.start_login(cx);
        }
    }

    fn start_login(&mut self, cx: &mut Context<Self>) {
        if self.login_loading {
            return;
        }
        self.login_generation = self.login_generation.wrapping_add(1);
        let generation = self.login_generation;
        self.login_loading = true;
        self.login_image = None;
        self.login_key = None;
        self.login_status = SharedString::from("正在获取登录二维码…");
        cx.notify();

        cx.spawn(async move |view, cx| {
            let qr = cx.background_spawn(async { login::fetch_qr_code() }).await;
            let qr = match qr {
                Ok(qr) => qr,
                Err(error) => {
                    view.update(cx, |app, cx| {
                        if app.login_generation == generation {
                            app.login_loading = false;
                            app.login_status = SharedString::from(format!("登录失败：{error}"));
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            let key = qr.key.clone();
            let active = view
                .update(cx, |app, cx| {
                    if app.login_generation != generation {
                        return false;
                    }
                    app.login_image = Some(qr.image);
                    app.login_key = Some(key.clone());
                    app.login_status = SharedString::from("请使用哔哩哔哩 App 扫码");
                    cx.notify();
                    true
                })
                .ok()
                .unwrap_or(false);
            if !active {
                return;
            }

            loop {
                Timer::after(Duration::from_secs(2)).await;
                let active = view
                    .update(cx, |app, _| app.login_generation == generation)
                    .ok()
                    .unwrap_or(false);
                if !active {
                    return;
                }
                let poll_key = key.clone();
                let result = cx
                    .background_spawn(async move { login::poll_qr_code(&poll_key) })
                    .await;
                match result {
                    Ok(PollResult::Waiting) => {}
                    Ok(PollResult::Scanned) => {
                        view.update(cx, |app, cx| {
                            if app.login_generation == generation {
                                app.login_status = SharedString::from("已扫码，请在手机上确认登录");
                                cx.notify();
                            }
                        })
                        .ok();
                    }
                    Ok(PollResult::Expired) => {
                        view.update(cx, |app, cx| {
                            if app.login_generation == generation {
                                app.login_loading = false;
                                app.login_status = SharedString::from("二维码已过期，请点击刷新");
                                cx.notify();
                            }
                        })
                        .ok();
                        return;
                    }
                    Ok(PollResult::LoggedIn(session)) => {
                        let save_error = login::save_session(&session).err();
                        view.update(cx, |app, cx| {
                            if app.login_generation == generation {
                                app.session = Some(session.clone());
                                app.login_loading = false;
                                app.login_image = None;
                                app.login_key = None;
                                app.login_status =
                                    SharedString::from(if let Some(error) = save_error {
                                        format!("登录成功，但保存登录状态失败：{error}")
                                    } else {
                                        format!("登录成功：{}", session.username)
                                    });
                                cx.notify();
                            }
                        })
                        .ok();
                        return;
                    }
                    Err(error) => {
                        view.update(cx, |app, cx| {
                            if app.login_generation == generation {
                                app.login_status =
                                    SharedString::from(format!("等待登录确认（{error}）"));
                                cx.notify();
                            }
                        })
                        .ok();
                    }
                }
            }
        })
        .detach();
    }

    fn refresh_login(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.login_loading {
            return;
        }
        self.start_login(cx);
    }

    fn logout(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        login::clear_session();
        self.session = None;
        self.login_image = None;
        self.login_key = None;
        self.login_status = SharedString::from("已退出登录");
        cx.notify();
    }

    fn start_cover_loading(&self, cx: &mut Context<Self>) {
        let covers = self
            .current_videos()
            .iter()
            .enumerate()
            .map(|(index, video)| (index, video.bvid.clone(), video.cover.clone()))
            .collect::<Vec<_>>();

        for (index, bvid, cover_url) in covers {
            cx.spawn(async move |view, cx| {
                let image = cx
                    .background_spawn(async move {
                        Client::builder()
                            .user_agent("Mozilla/5.0 biliguga/0.1")
                            .build()
                            .ok()
                            .and_then(|client| download_cover(&client, &cover_url))
                    })
                    .await;
                let Some(image) = image else {
                    return;
                };
                view.update(cx, |app, cx| {
                    let is_same_video = app
                        .current_videos()
                        .get(index)
                        .map(|video| video.bvid == bvid)
                        .unwrap_or(false);
                    if is_same_video {
                        if let Some(video) = app.current_videos_mut().get_mut(index) {
                            video.cover_image = Some(image);
                        }
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    fn search_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key.eq_ignore_ascii_case("enter") {
            self.start_search(cx);
        }
    }

    fn submit_search(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.start_search(cx);
    }

    fn start_search(&mut self, cx: &mut Context<Self>) {
        let keyword = self.search_input.read(cx).content.trim().to_string();
        if keyword.is_empty() {
            self.message = SharedString::from("请输入搜索关键词");
            cx.notify();
            return;
        }
        self.active_tab = AppTab::Search;
        self.search_query = keyword.clone();
        self.loading = true;
        self.search_results.clear();
        self.selected = 0;
        self.player.stop_playback();
        self.playback = PlaybackState::Idle;
        self.playback_request = self.playback_request.wrapping_add(1);
        self.controls_visible = false;
        self.controls_generation = self.controls_generation.wrapping_add(1);
        self.message = SharedString::from(format!("正在搜索：{keyword}"));
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move { fetch_search_results(&keyword) })
                .await;
            view.update(cx, |app, cx| {
                app.loading = false;
                match result {
                    Ok(videos) => {
                        app.message = SharedString::from(format!("找到 {} 个视频", videos.len()));
                        app.search_results = videos;
                        app.start_cover_loading(cx);
                    }
                    Err(error) => {
                        app.message = SharedString::from(format!("搜索失败：{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.active_tab = AppTab::Home;
        self.search_query.clear();
        self.search_input.update(cx, |input, cx| input.reset(cx));
        self.loading = true;
        self.videos.clear();
        self.selected = 0;
        self.player.stop_playback();
        self.playback = PlaybackState::Idle;
        self.playback_request = self.playback_request.wrapping_add(1);
        self.controls_visible = false;
        self.controls_generation = self.controls_generation.wrapping_add(1);
        self.message = SharedString::from("正在从 B 站加载推荐视频…");
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move { fetch_recommendations() })
                .await;
            view.update(cx, |app, cx| {
                app.loading = false;
                match result {
                    Ok(videos) => {
                        app.message =
                            SharedString::from(format!("已加载 {} 个真实视频", videos.len()));
                        app.videos = videos;
                        app.start_cover_loading(cx);
                    }
                    Err(error) => {
                        app.message = SharedString::from(format!("加载失败：{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn select_video(
        &mut self,
        index: usize,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.current_videos().len() {
            return;
        }
        if index == self.selected {
            if self.playback == PlaybackState::Paused {
                self.player.set_pause(false);
                self.playback = PlaybackState::Playing;
                self.message = SharedString::from("继续播放");
                cx.notify();
            } else if self.playback == PlaybackState::Idle {
                self.begin_play_selected(cx);
            }
            return;
        }
        self.selected = index;
        self.player.stop_playback();
        self.playback = PlaybackState::Idle;
        self.playback_request = self.playback_request.wrapping_add(1);
        self.controls_visible = false;
        self.controls_generation = self.controls_generation.wrapping_add(1);
        self.begin_play_selected(cx);
    }

    fn play_selected(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.playback == PlaybackState::Paused {
            self.player.set_pause(false);
            self.playback = PlaybackState::Playing;
            self.message = SharedString::from("继续播放");
            cx.notify();
            return;
        }
        self.begin_play_selected(cx);
    }

    fn begin_play_selected(&mut self, cx: &mut Context<Self>) {
        if self.playback == PlaybackState::Buffering {
            return;
        }
        let Some(video) = self.current_videos().get(self.selected).cloned() else {
            self.message = SharedString::from("还没有可播放的视频");
            cx.notify();
            return;
        };
        self.playback = PlaybackState::Buffering;
        self.message = SharedString::from("正在向 B 站获取播放地址…");
        cx.notify();
        let video_bvid = video.bvid.clone();
        let cookie = self.session.as_ref().map(|session| session.cookie.clone());
        self.playback_request = self.playback_request.wrapping_add(1);
        let playback_request = self.playback_request;
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move { resolve_play_url(&video, cookie.as_deref()) })
                .await;
            let message = match result {
                Ok(url) => {
                    view.update(cx, |app, cx| {
                        if app.playback_request != playback_request
                            || app
                                .current_videos()
                                .get(app.selected)
                                .map(|current| current.bvid != video_bvid)
                                .unwrap_or(true)
                        {
                            return;
                        }
                        app.player.load(url);
                        app.playback = PlaybackState::Buffering;
                        app.message = SharedString::from("已获取播放地址，libmpv 正在缓冲");
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Err(error) => format!("播放失败：{error}"),
            };
            view.update(cx, |app, cx| {
                if app.playback_request != playback_request {
                    return;
                }
                app.playback = PlaybackState::Idle;
                app.message = SharedString::from(message);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_pause(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        match self.playback {
            PlaybackState::Playing => {
                self.player.set_pause(true);
                self.playback = PlaybackState::Paused;
                self.message = SharedString::from("已暂停");
                cx.notify();
            }
            PlaybackState::Paused => {
                self.player.set_pause(false);
                self.playback = PlaybackState::Playing;
                self.message = SharedString::from("继续播放");
                cx.notify();
            }
            PlaybackState::Buffering => {
                self.message = SharedString::from("视频正在缓冲");
                cx.notify();
            }
            PlaybackState::Idle => {}
        }
    }

    fn adjust_volume(
        &mut self,
        delta: f64,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.volume = (self.volume + delta).clamp(0., 100.);
        self.player.set_volume(self.volume);
        cx.notify();
    }

    fn cycle_speed(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        const SPEEDS: [f64; 5] = [0.5, 1., 1.25, 1.5, 2.];
        let index = SPEEDS
            .iter()
            .position(|speed| (*speed - self.speed).abs() < 0.01)
            .unwrap_or(1);
        self.speed = SPEEDS[(index + 1) % SPEEDS.len()];
        self.player.set_speed(self.speed);
        cx.notify();
    }

    fn seek_percent(&mut self, percent: f64, cx: &mut Context<Self>) {
        self.player.seek_percent(percent);
        cx.notify();
    }

    fn show_player_controls(&mut self, _: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.controls_visible = true;
        self.controls_generation = self.controls_generation.wrapping_add(1);
        let generation = self.controls_generation;
        cx.notify();
        cx.spawn(async move |view, cx| {
            Timer::after(Duration::from_millis(2400)).await;
            view.update(cx, |app, cx| {
                if app.controls_generation == generation {
                    app.controls_visible = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(64.))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .bg(rgb(0x2f343e))
            .child(clickable_sidebar_item(
                "⌂",
                "首页",
                self.active_tab == AppTab::Home,
                cx.listener(Self::show_home),
            ))
            .child(sidebar_item("▣", "动态", false))
            .child(sidebar_item("♡", "收藏", false))
            .child(sidebar_item("◷", "历史", false))
            .child(clickable_sidebar_item(
                "⌕",
                "搜索",
                self.active_tab == AppTab::Search,
                cx.listener(Self::show_search),
            ))
            .child(clickable_sidebar_item(
                "◉",
                if self.session.is_some() {
                    "账号"
                } else {
                    "登录"
                },
                self.active_tab == AppTab::Login,
                cx.listener(Self::show_login),
            ))
    }

    fn render_video_card(
        index: usize,
        video: Video,
        selected: bool,
        entity: Entity<BiliGuga>,
    ) -> impl IntoElement + use<> {
        let title = video.title.clone();
        let click_entity = entity;
        div()
            .id(SharedString::from(format!("video-card-{index}")))
            .w_full()
            .flex()
            .flex_none()
            .gap_2()
            .cursor_pointer()
            .when(selected, |this| this.bg(rgb(0x454a56)))
            .when(!selected, |this| {
                this.hover(|style| style.bg(rgb(0x363c46)))
            })
            .child(thumbnail(&video, 126., 76.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .py_1()
                    .child(
                        div()
                            .text_sm()
                            .line_height(px(20.))
                            .text_color(rgb(0xdce0e5))
                            .text_ellipsis()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0xa9afbc))
                            .child(format!("{}  ·  {}", video.uploader, video.duration)),
                    ),
            )
            .on_click(move |event, window, cx| {
                click_entity.update(cx, |this, cx| {
                    this.select_video(index, event, window, cx);
                });
            })
    }

    fn render_login(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut panel = div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .text_center();
        if let Some(image) = &self.login_image {
            panel = panel.child(img(image.clone()).w(px(240.)).h(px(240.)));
        } else {
            panel = panel.child(div().text_sm().text_color(rgb(0xa9afbc)).child(
                if self.login_loading {
                    "正在准备登录…"
                } else {
                    "点击刷新获取二维码"
                },
            ));
        }
        panel = panel.child(
            div()
                .text_sm()
                .text_color(rgb(0xa9afbc))
                .child(self.login_status.clone()),
        );

        if let Some(session) = &self.session {
            panel = panel
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x878a98))
                        .child(format!("{} · UID {}", session.username, session.mid)),
                )
                .child(
                    div()
                        .id("logout")
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(rgb(0xd07277))
                        .bg(rgb(0x363c46))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(0x454a56)))
                        .child("退出登录")
                        .on_click(cx.listener(Self::logout)),
                );
        } else if !self.login_loading {
            panel = panel.child(
                div()
                    .id("refresh-login")
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(0x74ade8))
                    .bg(rgb(0x363c46))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x454a56)))
                    .child("刷新二维码")
                    .on_click(cx.listener(Self::refresh_login)),
            );
        }

        div()
            .w(px(414.))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x2f343e))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(0xdce0e5))
                    .child("登录"),
            )
            .child(panel)
    }

    fn render_feed(&self, cx: &mut Context<Self>) -> gpui::Div {
        if self.active_tab == AppTab::Login {
            return self.render_login(cx);
        }
        let is_search = self.active_tab == AppTab::Search;
        let search_input = self.search_input.clone();
        let mut feed = div().id("feed-scroll").flex_1().overflow_x_hidden();
        if self.loading {
            feed = feed.child(
                div()
                    .w_full()
                    .py_8()
                    .text_center()
                    .text_sm()
                    .text_color(rgb(0xa9afbc))
                    .child(if is_search {
                        "正在搜索 B 站视频…"
                    } else {
                        "正在加载 B 站推荐…"
                    }),
            );
        } else if self.current_videos().is_empty() {
            feed = feed.child(
                div()
                    .w_full()
                    .py_8()
                    .text_center()
                    .text_sm()
                    .text_color(rgb(0xd07277))
                    .child(if is_search {
                        "没有找到视频，请换个关键词试试"
                    } else {
                        "没有加载到视频，请点击刷新重试"
                    }),
            );
        } else {
            let entity = cx.entity();
            let list = uniform_list(
                "video-list",
                self.current_videos().len(),
                cx.processor(move |this, range: Range<usize>, _window, _cx| {
                    range
                        .filter_map(|index| {
                            let video = this.current_videos().get(index)?.clone();
                            Some(BiliGuga::render_video_card(
                                index,
                                video,
                                index == this.selected,
                                entity.clone(),
                            ))
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .size_full();
            feed = feed.child(list);
        }
        let mut header = div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(0xdce0e5))
                    .child(if is_search { "搜索" } else { "为你推荐" }),
            );
        if !is_search {
            header = header.child(
                div()
                    .id("refresh-feed")
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(0x74ade8))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x363c46)))
                    .child("刷新")
                    .on_click(cx.listener(Self::refresh)),
            );
        }
        let search_bar = div()
            .flex()
            .items_center()
            .child(
                div()
                    .id("search-box")
                    .flex_1()
                    .h(px(30.))
                    .on_key_down(cx.listener(Self::search_key_down))
                    .child(search_input),
            )
            .child(
                div()
                    .id("search-button")
                    .h(px(30.))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(rgb(0x74ade8))
                    .bg(rgb(0x363c46))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x454a56)))
                    .child("搜索")
                    .on_click(cx.listener(Self::submit_search)),
            );
        let mut root = div()
            .w(px(414.))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x2f343e))
            .child(header);
        if is_search {
            root = root.child(search_bar);
        }
        root.child(feed)
    }

    fn render_player(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let video = self
            .current_videos()
            .get(self.selected)
            .unwrap_or(&LOADING_VIDEO);
        let status = self.player.status();
        let playing = self.playback == PlaybackState::Playing;
        let paused = self.playback == PlaybackState::Paused;
        let buffering = self.playback == PlaybackState::Buffering;
        let frame = self.player.frame();
        let duration = if status.duration.is_finite() && status.duration > 0. {
            status.duration
        } else {
            0.
        };
        let position = if status.time_pos.is_finite() {
            status.time_pos.clamp(0., duration.max(0.))
        } else {
            0.
        };
        let progress = if duration > 0. {
            (position / duration).clamp(0., 1.) as f32
        } else {
            0.
        };
        let entity = cx.entity();
        let mut stage = div()
            .w_full()
            .h(px(480.))
            .overflow_hidden()
            .bg(rgb(0x000000))
            .flex()
            .items_center()
            .justify_center()
            .relative()
            .on_mouse_move(cx.listener(Self::show_player_controls));

        if let Some(frame) = frame {
            stage = stage.child(
                img(frame)
                    .w_full()
                    .h_full()
                    .object_fit(gpui::ObjectFit::Contain),
            );
        } else {
            stage = stage.child(
                div()
                    .text_sm()
                    .text_color(rgb(0x878a98))
                    .child(if buffering {
                        "正在缓冲视频…"
                    } else {
                        "点击播放开始观看"
                    }),
            );
        }

        if self.controls_visible {
            let seek_entity = entity.clone();
            let progress_bar = div()
                .relative()
                .flex_1()
                .h(px(16.))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(6.))
                        .h(px(4.))
                        .bg(rgb(0x464b57)),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(6.))
                        .h(px(4.))
                        .w(relative(progress))
                        .bg(rgb(0x74ade8)),
                )
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let seek_entity = seek_entity.clone();
                            window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                                if phase != DispatchPhase::Bubble
                                    || event.button != MouseButton::Left
                                    || !bounds.contains(&event.position)
                                    || bounds.size.width <= px(0.)
                                {
                                    return;
                                }
                                let percent = ((event.position.x - bounds.origin.x)
                                    / bounds.size.width)
                                    .clamp(0., 1.);
                                seek_entity.update(cx, |app, cx| {
                                    app.seek_percent(percent as f64, cx);
                                });
                            });
                        },
                    )
                    .size_full(),
                );
            let play_label = if buffering {
                "…"
            } else if playing {
                "Ⅱ"
            } else {
                "▶"
            };
            let play_handler = if playing || paused {
                Self::toggle_pause
            } else {
                Self::play_selected
            };
            let controls = div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .px_4()
                .pt_2()
                .pb_3()
                .bg(rgb(0x000000))
                .text_color(rgb(0xdce0e5))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .child(format_time(position))
                        .child(progress_bar)
                        .child(format_time(duration)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .id("player-play-overlay")
                                .w(px(30.))
                                .h(px(30.))
                                .bg(rgb(0x74ade8))
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(play_label)
                                .on_click(cx.listener(play_handler)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(0xa9afbc))
                                .child(if buffering {
                                    "缓冲中"
                                } else if playing {
                                    "播放中"
                                } else if paused {
                                    "已暂停"
                                } else {
                                    "待播放"
                                }),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_xs()
                                .child(
                                    div()
                                        .id("volume-down")
                                        .px_1()
                                        .cursor_pointer()
                                        .child("−")
                                        .on_click(cx.listener(|app, event, window, cx| {
                                            app.adjust_volume(-10., event, window, cx);
                                        })),
                                )
                                .child(format!("🔊 {:.0}%", self.volume))
                                .child(
                                    div()
                                        .id("volume-up")
                                        .px_1()
                                        .cursor_pointer()
                                        .child("+")
                                        .on_click(cx.listener(|app, event, window, cx| {
                                            app.adjust_volume(10., event, window, cx);
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .id("player-speed")
                                .px_2()
                                .py_1()
                                .bg(rgb(0x454a56))
                                .cursor_pointer()
                                .text_xs()
                                .child(format!("{:.2}x", self.speed))
                                .on_click(cx.listener(Self::cycle_speed)),
                        ),
                );
            stage = stage.child(controls);
        }

        div()
            .id("player-scroll")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .bg(rgb(0x3b414d))
            .child(
                div().w_full().flex().flex_col().child(stage).child(
                    div()
                        .w_full()
                        .px_5()
                        .py_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::BOLD)
                                .text_color(rgb(0xdce0e5))
                                .child(video.title.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .text_sm()
                                .text_color(rgb(0xa9afbc))
                                .child(format!(
                                    "{}  ·  {}  ·  {}",
                                    video.uploader, video.stats, video.category
                                )),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(0x878a98))
                                .child(self.message.clone()),
                        ),
                ),
            )
    }
}

impl Render for BiliGuga {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().bg(rgb(0x3b414d)).child(
            div()
                .size_full()
                .flex()
                .child(self.render_sidebar(cx))
                .child(self.render_feed(cx))
                .child(self.render_player(cx)),
        )
    }
}

fn clickable_sidebar_item<F>(
    icon: &'static str,
    label: &'static str,
    active: bool,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    sidebar_item(icon, label, active)
        .id(SharedString::from(format!("sidebar-{label}")))
        .on_click(on_click)
}

fn sidebar_item(icon: &'static str, label: &'static str, active: bool) -> gpui::Div {
    div()
        .w(px(64.))
        .h(px(52.))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .when(active, |this| {
            this.bg(rgb(0x454a56)).text_color(rgb(0xdce0e5))
        })
        .when(!active, |this| this.text_color(rgb(0xa9afbc)))
        .child(div().text_lg().child(icon))
        .child(div().text_xs().child(label))
}

fn thumbnail(video: &Video, width: f32, height: f32) -> impl IntoElement {
    let mut thumbnail = div()
        .w(px(width))
        .h(px(height))
        .flex_none()
        .bg(rgb(video.accent));
    if let Some(image) = &video.cover_image {
        thumbnail = thumbnail.child(
            img(image.clone())
                .w_full()
                .h_full()
                .object_fit(gpui::ObjectFit::Cover),
        );
    }
    thumbnail
}

pub(crate) fn launch() {
    Application::new().run(|cx: &mut App| {
        bind_search_keys(cx);
        let bounds = Bounds::centered(None, size(px(1360.), px(820.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(BiliGuga::new),
            )
            .expect("failed to open biliguga window");
        let view = window.update(cx, |_, _, cx| cx.entity()).unwrap();
        let frame_view = view.clone();
        cx.spawn(async move |cx| {
            loop {
                Timer::after(Duration::from_millis(33)).await;
                if frame_view
                    .update(cx, |app, cx| {
                        app.player.poll_frame();
                        let status = app.player.status();
                        if app.playback != PlaybackState::Idle && status.time_pos.is_finite() {
                            app.playback = if status.paused {
                                PlaybackState::Paused
                            } else {
                                PlaybackState::Playing
                            };
                            if status.volume.is_finite() {
                                app.volume = status.volume.clamp(0., 100.);
                            }
                            if status.speed.is_finite() {
                                app.speed = status.speed.max(0.1);
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |cx| {
            let result = cx
                .background_spawn(async move { fetch_recommendations() })
                .await;
            view.update(cx, |app, cx| {
                app.loading = false;
                match result {
                    Ok(videos) => {
                        app.message =
                            SharedString::from(format!("已加载 {} 个真实视频", videos.len()));
                        app.videos = videos;
                        app.start_cover_loading(cx);
                    }
                    Err(error) => {
                        app.message = SharedString::from(format!("加载失败：{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.activate(true);
    });
}
