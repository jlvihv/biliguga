//! The browse column: the rail, the search bar, the feed and its cards.

pub(crate) use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RailItem {
    Home,
    Dynamic,
    History,
    WatchLater,
    Favorites,
}

impl RailItem {
    /// The tabs this item stands for. Search and author pages are reached from the home rail.
    fn owns(self, tab: AppTab) -> bool {
        matches!(
            (self, tab),
            (
                RailItem::Home,
                AppTab::Home | AppTab::Search | AppTab::Author
            ) | (RailItem::Dynamic, AppTab::Dynamic)
                | (RailItem::History, AppTab::History)
                | (RailItem::WatchLater, AppTab::WatchLater)
                | (RailItem::Favorites, AppTab::Favorites)
        )
    }
}

pub(crate) fn rail_entry(
    item: RailItem,
    icon: &'static rune::Icon,
    caption: &str,
    active: bool,
) -> El {
    let icon_color = if active {
        theme::ACCENT
    } else {
        theme::TEXT_DIM
    };
    let text_color = if active {
        theme::TEXT
    } else {
        theme::TEXT_FAINT
    };
    let mut entry = col()
        .w_full()
        .h(56.0)
        .center()
        .gap(4.0)
        .radius(12.0)
        .hover(theme::CARD_HOVER)
        .press(theme::PRESS)
        .cursor(rune::Cursor::Pointer)
        .help(caption)
        .on_press(move |app: &mut BiliGuga| app.show_tab(tab_of(item)))
        .child(glyph(icon, icon_color, 19.0))
        .child(
            text(caption)
                .font(TextStyle::new(10.5, 500.0))
                .color(text_color)
                .single_line(),
        );
    if active {
        // A wash plus a bar down the inner edge: the bar is a child with `.h(..)` and its own
        // offset rather than an absolute overlay, so it needs no ancestor positioning.
        entry = entry
            .fill(theme::CARD)
            .border(1.0, theme::ACCENT.alpha(0.22));
    }
    entry
}

pub(crate) fn tab_of(item: RailItem) -> AppTab {
    match item {
        RailItem::Home => AppTab::Home,
        RailItem::Dynamic => AppTab::Dynamic,
        RailItem::History => AppTab::History,
        RailItem::WatchLater => AppTab::WatchLater,
        RailItem::Favorites => AppTab::Favorites,
    }
}

pub(crate) fn render_rail(app: &BiliGuga) -> El {
    let mut rail = col()
        .key("rail")
        .w(RAIL_WIDTH)
        .h_full()
        .shrink()
        .items_center()
        .py(14.0)
        .gap(6.0)
        .fill(theme::RAIL)
        .border_sides([0.0, 1.0, 0.0, 0.0], theme::HAIRLINE)
        .child(col().w_full().items_center().pb(10.0).child(
            el().size(36.0, 36.0).center().radius(11.0).child(glyph(
                &icons::LOGO,
                theme::ACCENT,
                24.0,
            )),
        ));
    for (item, icon, caption) in [
        (RailItem::Home, &icons::HOME, "首页"),
        (RailItem::Dynamic, &icons::PULSE, "动态"),
        (RailItem::History, &icons::HISTORY, "历史"),
        (RailItem::WatchLater, &icons::CLOCK, "稍后"),
        (RailItem::Favorites, &icons::STAR, "收藏"),
    ] {
        rail = rail.child(el().w(RAIL_WIDTH - 20.0).child(rail_entry(
            item,
            icon,
            caption,
            item.owns(app.active_tab),
        )));
    }
    rail = rail.child(el().grow(1.0));
    // The account is the last thing in the rail, always in the same place: an avatar when
    // signed in, the sign-in door when not.
    let has_session = app.session.is_some();
    let mut account = el()
        .key("account")
        .size(40.0, 40.0)
        .center()
        .radius(20.0)
        .clip_rounded(20.0)
        .cursor(rune::Cursor::Pointer)
        .hover(theme::CARD_HOVER)
        .press(theme::PRESS)
        .help(if has_session { "账号" } else { "登录" })
        .on_press(|app: &mut BiliGuga| app.show_tab(AppTab::Login));
    match (has_session, app.account_avatar.as_ref()) {
        (true, Some(avatar)) => {
            account = account.child(
                el().w_full()
                    .h_full()
                    .pixels_fit(avatar.pixels(), SurfaceFit::Fill),
            );
        }
        (true, None) => {
            account = account.fill(theme::ACCENT_DIM).child(glyph(
                &icons::USER,
                theme::ACCENT_SOFT,
                18.0,
            ));
        }
        (false, _) => {
            account = account
                .fill(theme::CARD)
                .border(1.0, theme::BORDER)
                .child(glyph(&icons::USER, theme::TEXT_DIM, 17.0));
        }
    }
    rail.child(account)
}

// ---- The browse pane ------------------------------------------------------------------------

/// The feed column's heading: a fixed sentence for every tab but one, which borrows the
/// uploader's name. Returning a `Cow` keeps the seven fixed cases allocation-free.
pub(crate) fn feed_title<'a>(tab: AppTab, author_name: &'a str) -> std::borrow::Cow<'a, str> {
    match tab {
        AppTab::Home => "为你推荐".into(),
        AppTab::Search => "搜索".into(),
        AppTab::Dynamic => "动态".into(),
        AppTab::WatchLater => "稍后再看".into(),
        AppTab::Favorites => "收藏夹".into(),
        AppTab::History => "观看历史".into(),
        AppTab::Author => author_name.into(),
        AppTab::Login => "登录".into(),
    }
}

/// What an empty feed means, in the tab's own terms.
pub(crate) fn empty_message(app: &BiliGuga) -> &'static str {
    let logged_in = app.session.is_some();
    match app.active_tab {
        AppTab::Search => "没有找到视频",
        AppTab::Dynamic if !logged_in => "登录后可以看到动态",
        AppTab::Dynamic => "关注的人最近没有更新",
        AppTab::WatchLater if !logged_in => "登录后可以看到稍后再看",
        AppTab::WatchLater => "稍后再看还是空的",
        AppTab::Favorites if !logged_in => "登录后可以看到收藏夹",
        AppTab::Favorites => "收藏夹还是空的",
        AppTab::History if !logged_in => "登录后可以看到观看历史",
        AppTab::History => "还没有观看记录",
        AppTab::Author => "这位 UP 主还没有公开投稿",
        _ => "没有加载到视频",
    }
}

/// What the spinner says while a feed is on its way.
pub(crate) fn loading_message(tab: AppTab) -> &'static str {
    match tab {
        AppTab::Search => "正在搜索…",
        AppTab::Dynamic => "正在加载动态…",
        AppTab::WatchLater => "正在加载稍后再看…",
        AppTab::Favorites => "正在加载收藏夹…",
        AppTab::History => "正在加载观看历史…",
        AppTab::Author => "正在加载 UP 主的投稿…",
        _ => "正在加载推荐…",
    }
}

/// The heading above the feed: what this column is, the one control that acts on it, and the
/// search field.
///
/// The field is here on every tab rather than only on the search results page. It used to be
/// reachable only *from* the search results, which meant the only way to search was to already
/// have searched: a first run had no search at all.
pub(crate) fn render_topbar(app: &BiliGuga) -> El {
    let mut header = col()
        .key("feed-header")
        .w_full()
        .shrink()
        .child(render_title_row(app));
    if app.active_tab != AppTab::Login {
        header = header.child(render_search(app));
    }
    header
}

/// The title, the refresh button, and "load more" when an author page has another batch.
pub(crate) fn render_title_row(app: &BiliGuga) -> El {
    let title = match app.active_tab {
        AppTab::Author => app.author_name().to_string(),
        tab => feed_title(tab, app.author_name()).into_owned(),
    };
    let busy = app.is_loading();
    let mut title_row = row()
        .key("topbar")
        .w_full()
        .shrink()
        .px(PANE_PAD)
        .pt(14.0)
        .pb(10.0)
        .items_center()
        .gap(12.0)
        .child(
            text(title)
                .font(TextStyle::new(19.0, 650.0))
                .color(theme::TEXT)
                .single_line()
                .truncate()
                .grow(1.0),
        );
    if app.active_tab == AppTab::Author && app.author_has_more {
        title_row = title_row.child(
            pill("author-more", None, "更多", Tone::Raised)
                .on_press(|app: &mut BiliGuga| app.load_author_page(false)),
        );
    }
    // Loading says so in the corner, in the same place the refresh button lives, so the two
    // never both occupy it: a second refresh of a request in flight would be a no-op anyway.
    if app.active_tab == AppTab::Login {
        return title_row;
    }
    title_row.child(if busy {
        el().key("refresh")
            .size(34.0, 34.0)
            .center()
            .help("正在加载…")
            .child(pulse_dot())
    } else {
        square_button("refresh", &icons::REFRESH, "刷新")
            .on_press(|app: &mut BiliGuga| app.refresh_feed())
    })
}

/// The search field. Always present at the top of the browse pane; Enter submits, and the
/// button beside it does the same for people who would rather click.
pub(crate) fn render_search(app: &BiliGuga) -> El {
    row()
        .key("search")
        .w_full()
        .shrink()
        .px(PANE_PAD)
        .pb(12.0)
        .gap(8.0)
        .child(
            row()
                .key("search-field")
                .grow(1.0)
                .h(36.0)
                .gap(8.0)
                .px(11.0)
                .items_center()
                .radius(10.0)
                .fill(theme::FIELD)
                .border(1.0, theme::BORDER)
                .child(glyph(&icons::SEARCH, theme::TEXT_FAINT, 14.0))
                .child(
                    text_input(&app.search_edit, TextStyle::new(13.0, 400.0))
                        .key("search-input")
                        .grow(1.0)
                        .single_line()
                        .placeholder("搜视频、UP 主、话题", theme::TEXT_FAINT)
                        .color(theme::TEXT)
                        .on_edit(|app: &mut BiliGuga| &mut app.search_edit)
                        .on_submit(|app: &mut BiliGuga, _: &mut Tx| app.load_search()),
                ),
        )
        .child(
            pill("search-button", None, "搜索", Tone::Accent)
                .h(36.0)
                .on_press(|app: &mut BiliGuga| app.load_search()),
        )
}

/// A poster card for the browse grid: cover, two-line title, uploader, and a play badge on
/// hover so it reads as playable rather than as a link.
pub(crate) fn browse_card(app: &BiliGuga, index: usize, video: &Video, slot: f32) -> El {
    // The 6 pt padding is drawn inside the slot, so the card's own box is the slot less it.
    let width = (slot - 2.0 * CARD_PAD).max(80.0);
    let cover_width = width;
    let cover_height = (width * 9.0 / 16.0).round();
    let selected = app.selected == Some(index);
    let playing = app
        .playing_video
        .as_ref()
        .is_some_and(|current| current.bvid == video.bvid);
    let mut card = col()
        .w(width)
        .shrink()
        .gap(8.0)
        .padding(CARD_PAD)
        .radius(12.0)
        .hover(theme::CARD_HOVER)
        .hover_group()
        .cursor(rune::Cursor::Pointer)
        .on_press(move |app: &mut BiliGuga| app.select(index))
        .child(el().w(cover_width).h(cover_height).shrink().child(poster(
            app,
            video,
            cover_width,
            cover_height,
            10.0,
            true,
        )))
        .child(
            col()
                .w_full()
                .min_w(0.0)
                .gap(5.0)
                .px(2.0)
                .child(
                    text(&video.title)
                        .font(TextStyle::new(13.0, 550.0))
                        .color(if playing {
                            theme::ACCENT_SOFT
                        } else {
                            theme::TEXT
                        })
                        .max_lines(2),
                )
                .child(
                    row()
                        .w_full()
                        .gap(5.0)
                        .items_center()
                        .child(glyph(&icons::USER, theme::TEXT_FAINT, 11.0))
                        .child(label(&video.uploader, 11.5, 400.0, theme::TEXT_FAINT).grow(1.0)),
                ),
        );
    if selected {
        card = selection_wash(card);
    }
    card
}

/// A list row: 16:9 cover on the left, title and metadata stacked beside it.
pub(crate) fn video_row(app: &BiliGuga, index: usize, video: &Video) -> El {
    let selected = app.selected == Some(index);
    let playing = app
        .playing_video
        .as_ref()
        .is_some_and(|current| current.bvid == video.bvid);
    let mut body = row()
        .w_full()
        .padding(6.0)
        .gap(10.0)
        .radius(10.0)
        .hover(theme::CARD_HOVER)
        .hover_group()
        .cursor(rune::Cursor::Pointer)
        .on_press(move |app: &mut BiliGuga| app.select(index))
        .child(poster(app, video, 132.0, 74.0, 8.0, true))
        .child(
            col()
                .grow(1.0)
                .min_w(0.0)
                .shrink()
                .gap(4.0)
                .py(1.0)
                .child(
                    text(&video.title)
                        .font(TextStyle::new(12.5, 550.0))
                        .color(if playing {
                            theme::ACCENT_SOFT
                        } else {
                            theme::TEXT
                        })
                        .max_lines(2),
                )
                .child(
                    row()
                        .w_full()
                        .gap(5.0)
                        .items_center()
                        .child(glyph(&icons::USER, theme::TEXT_FAINT, 10.0))
                        .child(label(&video.uploader, 11.0, 400.0, theme::TEXT_FAINT).grow(1.0)),
                ),
        );
    if selected {
        body = selection_wash(body);
    }
    body
}

/// A spinner-ish placeholder that says what is happening without a fake progress bar: a dot
/// that breathes, and the message beside it.
pub(crate) fn pulse_dot() -> El {
    el().size(7.0, 7.0)
        .radius(4.0)
        .fill(theme::ACCENT)
        .looping(rune::LoopSpec::opacity(1.4, 0.0, 0.22, 1.0))
}

/// A centred message for the empty, loading and error states.
/// A centred message for the empty, loading and error states, with an optional action under it.
/// The action lives in the same centred column: as a sibling of a grown panel it was pushed to
/// the bottom of the pane, a screen away from the sentence it belonged to.
pub(crate) fn state_panel(
    icon: &'static rune::Icon,
    headline: &str,
    detail: Option<&str>,
    tone: Color,
    action: Option<El>,
) -> El {
    let mut panel = col()
        .w_full()
        .grow(1.0)
        .center()
        .gap(10.0)
        .padding(24.0)
        .child(
            el().size(52.0, 52.0)
                .center()
                .radius(26.0)
                .fill(theme::CARD)
                .border(1.0, theme::BORDER)
                .child(glyph(icon, tone, 22.0)),
        )
        .child(
            text(headline)
                .font(TextStyle::new(14.0, 550.0))
                .color(theme::TEXT_DIM),
        );
    if let Some(detail) = detail {
        panel = panel.child(
            text(detail)
                .font(TextStyle::new(12.0, 400.0))
                .color(theme::TEXT_FAINT)
                .max_w(280.0)
                .align(rune::Align::Center),
        );
    }
    if let Some(action) = action {
        panel = panel.child(el().h(4.0)).child(action);
    }
    panel
}

pub(crate) fn render_feed(app: &BiliGuga) -> El {
    if app.active_tab == AppTab::Login {
        return render_login(app);
    }
    let videos = app.current_videos();
    let is_loading = app.is_loading();

    let body = if is_loading && videos.is_empty() {
        col()
            .w_full()
            .grow(1.0)
            .center()
            .gap(12.0)
            .child(pulse_dot())
            .child(
                text(loading_message(app.active_tab))
                    .font(TextStyle::new(12.5, 400.0))
                    .color(theme::TEXT_FAINT),
            )
    } else if let Some(error) = &app.feed_error {
        // A failure offers the way out of it: the old panel said "no" and left the fixed
        // refresh button in the corner as the only way to try again.
        state_panel(
            &icons::CLOSE,
            "没能加载出来",
            Some(error.as_str()),
            theme::DANGER,
            Some(
                pill("retry", Some(&icons::REFRESH), "重试", Tone::Raised)
                    .on_press(|app: &mut BiliGuga| app.refresh_feed()),
            ),
        )
    } else if videos.is_empty() {
        let needs_session = app.active_tab.signed_in().is_some() && app.session.is_none();
        state_panel(
            &icons::TV,
            empty_message(app),
            // A signed-out feed already says what to do in its headline; a second line under
            // it would only repeat the offer the button makes.
            (app.session.is_some() || app.active_tab.signed_in().is_none()).then_some(
                match app.active_tab {
                    AppTab::Home => "点右上角刷新试试",
                    AppTab::Search => "换一个关键词",
                    _ => "这里还是空的",
                },
            ),
            theme::TEXT_FAINT,
            needs_session.then(|| {
                pill(
                    "empty-sign-in",
                    Some(&icons::USER),
                    "扫码登录",
                    Tone::Accent,
                )
                .on_press(|app: &mut BiliGuga| app.show_tab(AppTab::Login))
            }),
        )
    } else if app.card_columns.get() >= 2 {
        // The grid takes as many equal columns as the pane allows, so the same view is two
        // columns in a narrow window and five in a wide one, without a measurement pass: the
        // view is handed the pane's width by `card_columns`. One column is a list, not a grid:
        // a poster card as wide as the pane is a billboard, and the pane is this narrow only
        // when the player has taken the rest of the window.
        let columns = app.card_columns.get();
        let width = app.card_width.get();
        let rows = videos.len().div_ceil(columns);
        col()
            .key("home-grid")
            .w_full()
            .growth(GrowthSpec::default())
            .px(PANE_PAD)
            .py(6.0)
            .virtual_rows(
                rows,
                (width * 9.0 / 16.0 + 78.0).round(),
                // Keyed by the first video of the row, not by its index. An index stays the same
                // when a page lands and the oldest rows are trimmed off the top, so every row
                // kept its key while showing a different video: layout saw no change at all, the
                // reader was silently swapped onto other videos, and — since the trimmed rows
                // and the new ones cancel out — the content never got taller either. A content
                // key makes a landing a real change: rows keep their identity, the reader's row
                // stays where it is, and what is below them is what arrived.
                |app: &BiliGuga, row_index| {
                    app.current_videos()
                        .get(row_index * app.card_columns.get())
                        .map(|video| format!("grid-row-{}", video.bvid))
                        .unwrap_or_else(|| format!("grid-row-{row_index}"))
                        .into()
                },
                move |app: &BiliGuga, row_index| {
                    let videos = app.current_videos();
                    let mut line = row().w_full().gap(CARD_GAP);
                    for column in 0..columns {
                        let index = row_index * columns + column;
                        if let Some(video) = videos.get(index) {
                            line = line.child(browse_card(app, index, video, width));
                        }
                    }
                    line
                },
            )
    } else {
        col()
            .key("video-list")
            .w_full()
            .growth(GrowthSpec::default())
            .px(PANE_PAD - 6.0)
            .py(6.0)
            .virtual_rows(
                videos.len(),
                LIST_ROW_HEIGHT,
                |app: &BiliGuga, index| {
                    app.current_videos()
                        .get(index)
                        .map(|video| format!("row-{}", video.bvid))
                        .unwrap_or_else(|| format!("row-{index}"))
                        .into()
                },
                |app: &BiliGuga, index| match app.current_videos().get(index) {
                    Some(video) => video_row(app, index, video),
                    None => el(),
                },
            )
    };

    // Paging hangs off the list itself: it reports the range of rows it has mounted, once per
    // change rather than per frame, and the feed asks for more when that range gets close to the
    // end. Nothing here depends on how tall the loaded page happens to be.
    let scroll_read = app.scroll_pos.clone();
    let scroll_wheel_read = app.scroll_pos.clone();
    let scroll_wheel = app.scroll_pos.clone();
    let reader_row = app.reader_row.clone();
    let wheel_row = app.reader_row.clone();
    // One row of the browse grid, as this frame lays it out: turns a scroll offset into the row
    // count paging is about. A plain value, not a `Cell`: it cannot change under a handler.
    let row_height = app.feed_row_height();
    let viewport = col()
        .key("feed-scroll")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        // Where it stands, for the wheel handler below, and for `poll`'s paging check. Every
        // way of scrolling the feed passes through here — the wheel below, a trackpad, a drag,
        // the arrow keys, Page Down — which is why the row that drives paging is counted here
        // and not in the wheel handler.
        .on_scroll_offset(move |at| {
            let at = (at.offset[1], at.max[1]);
            scroll_read.set(Some(at));
            scroll_wheel_read.set(Some(at));
            if row_height > 0.0 {
                reader_row.set(Some((at.0 / row_height).floor() as usize));
            }
        })
        // A notched wheel takes the feed further per click than Rune's flat 40 points. Taking
        // the delta means placing the viewport here, so the offset above is read back; a
        // trackpad (`precise`) and horizontal gestures are left to Rune.
        .on_wheel(
            move |_: &mut BiliGuga, ev: WheelEvent, tx: &mut Tx| -> bool {
                if ev.precise || ev.dy == 0.0 || ev.dx != 0.0 || ev.momentum {
                    return false;
                }
                let Some((offset, max)) = scroll_wheel.get() else {
                    // Not laid out yet: nothing to place the viewport at, so let Rune scroll.
                    return false;
                };
                if max <= 0.0 {
                    // Everything fits: nothing to scroll, and nothing to take from Rune.
                    return false;
                }
                let step = ev.dy * (WHEEL_NOTCH / RUNE_WHEEL_NOTCH);
                let to = (offset - step).clamp(0.0, max);
                // Two notches can arrive between frames; the second starts where the first left
                // off, not where the last frame's offset was. Clamped here as well as by the
                // viewport, so holding the wheel at the end cannot build up an offset past it
                // for the next page's growth to spring to.
                scroll_wheel.set(Some((to, max)));
                // This handler places the viewport itself, so the offset handler that counts
                // rows hears about it a frame later; count it now so a reader scrolling faster
                // than a page a frame still pages as fast as they read.
                if row_height > 0.0 {
                    wheel_row.set(Some((to / row_height).floor() as usize));
                }
                tx.scroll_to_instant("feed-scroll", 0.0, to);
                true
            },
        );
    // "Loading the next page" is a footer inside the viewport, so the reader sees the feed
    // still filling rather than a frozen list.
    let footer = if is_loading && !videos.is_empty() {
        row()
            .w_full()
            .py(16.0)
            .justify_center()
            .gap(8.0)
            .child(pulse_dot())
            .child(
                text("正在加载更多…")
                    .font(TextStyle::new(11.5, 400.0))
                    .color(theme::TEXT_FAINT),
            )
    } else {
        el().h(12.0).w_full()
    };

    col()
        .key("feed")
        .h_full()
        .min_h(0.0)
        .min_w(0.0)
        .grow(1.0)
        .fill(theme::BG)
        .child(render_topbar(app))
        .child(el().w_full().h(1.0).shrink().fill(theme::HAIRLINE))
        .child(viewport.child(body).child(footer))
}

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
        .child(el().w_full().h(1.0).shrink().fill(theme::HAIRLINE))
        .child(col().w_full().grow(1.0).center().child(body))
}

// ---- The player -----------------------------------------------------------------------------
