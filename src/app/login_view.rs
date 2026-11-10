//! The login panel: the QR code, the account card, and the door out of it.

pub(crate) use super::*;

pub(crate) fn render_login(app: &BiliGuga) -> El {
    let body = if let Some(session) = &app.session {
        let initial: String = session
            .username
            .chars()
            .next()
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_else(|| "B".into());
        let mut card = col()
            .w(320.0)
            .shrink()
            .gap(14.0)
            .padding(24.0)
            .items_center()
            .radius(16.0)
            .fill(theme::CARD)
            .border(1.0, theme::BORDER)
            .shadow(12.0, 40.0, Color::BLACK.alpha(0.35))
            .child(match app.account_avatar.as_ref() {
                Some(avatar) => el().size(64.0, 64.0).radius(32.0).clip_rounded(32.0).child(
                    el().w_full()
                        .h_full()
                        .pixels_fit(avatar.pixels(), SurfaceFit::Fill),
                ),
                None => el()
                    .size(64.0, 64.0)
                    .center()
                    .radius(32.0)
                    .fill(theme::ACCENT_DIM)
                    .child(
                        text(initial)
                            .font(TextStyle::new(24.0, 650.0))
                            .color(theme::ACCENT_SOFT),
                    ),
            })
            .child(
                text(&session.username)
                    .font(TextStyle::new(17.0, 600.0))
                    .color(theme::TEXT),
            )
            .child(
                text(if session.is_vip {
                    "大会员 · 已登录"
                } else {
                    "已登录"
                })
                .font(TextStyle::new(12.0, 400.0))
                .color(theme::TEXT_FAINT),
            )
            .child(pill("sign-out", None, "退出登录", Tone::Raised).on_press(
                |app: &mut BiliGuga| {
                    login::clear_session();
                    app.session = None;
                    app.account_avatar = None;
                    app.login_status = "使用哔哩哔哩 App 扫码登录".into();
                    app.active_tab = AppTab::Home;
                },
            ));
        if app.session.is_some() {
            card = card.child(
                row()
                    .gap(8.0)
                    .child(
                        pill("to-history", Some(&icons::HISTORY), "观看历史", Tone::Quiet)
                            .on_press(|app: &mut BiliGuga| app.show_tab(AppTab::History)),
                    )
                    .child(
                        pill("to-favs", Some(&icons::STAR), "我的收藏", Tone::Quiet)
                            .on_press(|app: &mut BiliGuga| app.show_tab(AppTab::Favorites)),
                    ),
            );
        }
        card
    } else {
        let mut card = col()
            .w(320.0)
            .shrink()
            .gap(14.0)
            .padding(24.0)
            .items_center()
            .radius(16.0)
            .fill(theme::CARD)
            .border(1.0, theme::BORDER)
            .shadow(12.0, 40.0, Color::BLACK.alpha(0.35))
            .child(
                text("扫码登录")
                    .font(TextStyle::new(17.0, 600.0))
                    .color(theme::TEXT),
            )
            .child(
                text("用哔哩哔哩 App 扫一扫，收藏和历史就会出现在这里")
                    .font(TextStyle::new(12.0, 400.0))
                    .color(theme::TEXT_FAINT)
                    .max_w(240.0)
                    .align(rune::Align::Center),
            );
        // The code sits on a white plate whatever the theme: scanners need the quiet zone.
        let plate = el()
            .size(196.0, 196.0)
            .center()
            .radius(10.0)
            .fill(Color::WHITE)
            .clip_rounded(10.0);
        card = card.child(match &app.login_qr {
            Some(qr) => plate.child(
                el().size(172.0, 172.0)
                    .pixels_fit(qr.pixels(), SurfaceFit::Fit),
            ),
            None => plate.child(el().size(172.0, 172.0).center().child(glyph(
                &icons::REFRESH,
                theme::TEXT_FAINT,
                26.0,
            ))),
        });
        card = card
            .child(
                text(app.login_status.as_ref())
                    .font(TextStyle::new(12.0, 400.0))
                    .color(if app.login_qr.is_some() {
                        theme::TEXT_DIM
                    } else {
                        theme::TEXT_FAINT
                    })
                    .align(rune::Align::Center),
            )
            .child(
                pill(
                    "refresh-qr",
                    Some(&icons::REFRESH),
                    "刷新二维码",
                    Tone::Accent,
                )
                .on_press(|app: &mut BiliGuga| app.start_login()),
            );
        card
    };
    col()
        .w_full()
        .h_full()
        .grow(1.0)
        .fill(theme::BG)
        .child(render_topbar(app))
        .child(hairline())
        .child(col().w_full().grow(1.0).center().child(body))
}
