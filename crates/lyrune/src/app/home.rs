//! 首页推荐歌单的加载与显示；个性化歌曲的播放和续播由 queue 管理。

use std::time::Duration;

use anyhow::Context as _;
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    spinner::Spinner,
    v_flex,
};
use lyrune_api::{RecommendationKind, UserPlaylist, UserPlaylistId};

use super::browser::BrowserView;
use super::{home_content_width, page_horizontal_padding};
use crate::icons::{MediaIcon, media_icon_hsla};
use crate::library::playlist_cover;
use crate::runtime::RUNTIME;

#[derive(Default)]
pub(super) struct HomePageState {
    pub(super) playlists: Vec<UserPlaylist>,
    pub(super) loading: bool,
    pub(super) loaded: bool,
    pub(super) error: Option<String>,
    pub(super) generation: u64,
    pub(super) recommendation_loading: Option<RecommendationKind>,
    // 与首页歌单请求独立；清空加载状态时保留版本，避免旧结果匹配下一次同类别请求。
    recommendation_generation: u64,
}

impl HomePageState {
    pub(super) fn begin_recommendation(&mut self, kind: RecommendationKind) -> u64 {
        self.recommendation_generation = self.recommendation_generation.wrapping_add(1);
        self.recommendation_loading = Some(kind);
        self.recommendation_generation
    }

    pub(super) fn finish_recommendation(
        &mut self,
        generation: u64,
        kind: RecommendationKind,
    ) -> bool {
        if self.recommendation_generation != generation || self.recommendation_loading != Some(kind)
        {
            return false;
        }
        self.recommendation_loading = None;
        true
    }
}

impl BrowserView {
    pub(super) fn load_home(&mut self, cx: &mut Context<Self>) {
        if self.home.loading {
            return;
        }
        let Some(credential) = self.account.read(cx).credential.clone() else {
            self.home.error = Some("请先登录 QQ 音乐".to_owned());
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.home.error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        self.home.generation = self.home.generation.wrapping_add(1);
        let generation = self.home.generation;
        self.home.loading = true;
        self.home.error = None;
        cx.notify();

        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(30),
                client.recommended_playlists(&credential, 0, 20),
            )
            .await
            .context("QQ 音乐主页请求等待超过 30 秒")
            .and_then(|result| result)
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.home.generation != generation {
                    return;
                }
                this.home.loading = false;
                match result {
                    Ok(page) => {
                        this.home.loaded = true;
                        this.home.playlists = page.items;
                        this.home.error = None;
                    }
                    Err(error) => {
                        this.home.error = Some(format!("加载主页推荐失败：{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_home(
        &mut self,
        compact: bool,
        narrow: bool,
        content_width: Pixels,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let cover_size = if narrow {
            px(132.)
        } else if compact {
            px(148.)
        } else {
            px(168.)
        };
        let card_width = cover_size + px(16.);
        let grid_width = home_content_width(compact, narrow);
        let stack_recommendations = content_width - page_horizontal_padding(narrow) * 2. < px(600.);
        let cards = self
            .home
            .playlists
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, playlist)| {
                let cover = playlist_cover(&playlist, cover_size, px(14.), scale_factor, cx);
                let title = playlist.title.clone();
                let subtitle = if playlist.description.is_empty() {
                    "为你推荐".to_owned()
                } else {
                    playlist.description.clone()
                };
                Button::new(format!("home-playlist-{index}"))
                    .ghost()
                    .w(card_width)
                    .h(cover_size + px(74.))
                    .p_2()
                    .rounded(px(12.))
                    .tooltip(title.clone())
                    .child(
                        v_flex()
                            .size_full()
                            .items_start()
                            .gap_2()
                            .child(div().rounded(px(14.)).shadow_sm().child(cover))
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .font_medium()
                                    .text_color(theme.foreground)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(subtitle),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_home_playlist(playlist.clone(), window, cx)
                    }))
            })
            .collect::<Vec<_>>();
        let recommendation_loading = self.home.recommendation_loading.is_some();
        let radar_icon = media_icon_hsla(MediaIcon::Radar, theme.primary, px(25.));
        let guess_icon = media_icon_hsla(MediaIcon::Headphones, theme.primary, px(25.));
        let [daily_card, radar_entry, guess_entry] = [
            Button::new("home-daily")
                .ghost()
                .w_full()
                .min_w_0()
                .h(px(156.))
                .p(px(22.))
                .rounded(px(12.))
                .bg(linear_gradient(
                    110.,
                    linear_color_stop(theme.primary.opacity(0.08), 0.),
                    linear_color_stop(theme.primary.opacity(0.22), 1.),
                ))
                .when(recommendation_loading, |button| {
                    button.text_color(theme.secondary_foreground)
                })
                .disabled(recommendation_loading)
                .child(
                    h_flex()
                        .size_full()
                        .gap_4()
                        .justify_between()
                        .child(
                            v_flex()
                                .min_w_0()
                                .items_start()
                                .gap_2()
                                .child(div().text_size(px(22.)).font_semibold().child("每日推荐"))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("为你每天更新"),
                                ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(104.))
                                .line_height(relative(1.))
                                .font_bold()
                                .text_color(theme.primary.opacity(0.38))
                                .child("30"),
                        ),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    let profile = this.account.read(cx).profile.as_ref();
                    let playlist = UserPlaylist {
                        id: UserPlaylistId::Daily,
                        title: "每日 30 首".to_owned(),
                        cover_url: None,
                        description: "为你每天更新".to_owned(),
                        owner: profile
                            .map(|profile| profile.nickname.clone())
                            .unwrap_or_default(),
                        owner_avatar_url: profile.and_then(|profile| profile.avatar_url.clone()),
                        track_count: 0,
                    };
                    this.open_home_playlist(playlist, window, cx);
                })),
            Button::new("home-radar")
                .ghost()
                .w_full()
                .h(px(72.))
                .px_4()
                .py_3()
                .rounded(px(12.))
                .bg(theme.muted.opacity(0.7))
                .when(recommendation_loading, |button| {
                    button
                        .bg(theme.muted.opacity(0.7))
                        .text_color(theme.secondary_foreground)
                })
                .disabled(recommendation_loading)
                .child(
                    h_flex()
                        .size_full()
                        .gap_4()
                        .child(
                            div()
                                .size(px(48.))
                                .flex_shrink_0()
                                .rounded(px(10.))
                                .bg(theme.background.opacity(0.55))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(radar_icon),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .items_start()
                                .gap_1()
                                .child(div().font_semibold().child("专属雷达"))
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("不断更新的个性推荐"),
                                ),
                        ),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.start_home_recommendation(RecommendationKind::Radar, cx)
                })),
            Button::new("home-guess")
                .ghost()
                .w_full()
                .h(px(72.))
                .px_4()
                .py_3()
                .rounded(px(12.))
                .bg(theme.muted.opacity(0.7))
                .when(recommendation_loading, |button| {
                    button
                        .bg(theme.muted.opacity(0.7))
                        .text_color(theme.secondary_foreground)
                })
                .disabled(recommendation_loading)
                .child(
                    h_flex()
                        .size_full()
                        .gap_4()
                        .child(
                            div()
                                .size(px(48.))
                                .flex_shrink_0()
                                .rounded(px(10.))
                                .bg(theme.background.opacity(0.55))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(guess_icon),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .items_start()
                                .gap_1()
                                .child(div().font_semibold().child("猜你喜欢"))
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("持续生成的个性漫游"),
                                ),
                        ),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.start_home_recommendation(RecommendationKind::Guess, cx)
                })),
        ];

        div()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .w_full()
                    .px(page_horizontal_padding(narrow))
                    .pt(if narrow { px(22.) } else { px(32.) })
                    .pb_8()
                    .child(
                        h_flex().w_full().justify_center().child(
                            v_flex()
                                .w_full()
                                .max_w(grid_width)
                                .gap_6()
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap_3()
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .px_2()
                                                .justify_between()
                                                .child(
                                                    div()
                                                        .text_size(if narrow {
                                                            px(22.)
                                                        } else {
                                                            px(24.)
                                                        })
                                                        .font_semibold()
                                                        .child("智能推荐"),
                                                )
                                                .child(
                                                    Button::new("home-charts")
                                                        .ghost()
                                                        .label("排行榜 ›")
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.show_charts(window, cx)
                                                            },
                                                        )),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .grid()
                                                .grid_cols(if stack_recommendations {
                                                    1
                                                } else {
                                                    2
                                                })
                                                .gap_4()
                                                .child(daily_card)
                                                .child(
                                                    v_flex()
                                                        .w_full()
                                                        .min_w_0()
                                                        .gap_3()
                                                        .child(radar_entry)
                                                        .child(guess_entry),
                                                ),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .w_full()
                                        .px_2()
                                        .child(
                                            div()
                                                .text_size(if narrow { px(22.) } else { px(24.) })
                                                .font_semibold()
                                                .child("今日歌单"),
                                        )
                                        .when_some(self.home.error.clone(), |header, error| {
                                            header.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(error),
                                            )
                                        }),
                                )
                                .when(self.home.loading, |view| {
                                    view.child(
                                        h_flex()
                                            .gap_2()
                                            .text_color(theme.muted_foreground)
                                            .child(
                                                Spinner::new()
                                                    .with_size(px(20.))
                                                    .color(theme.primary),
                                            )
                                            .child("正在加载主页推荐…"),
                                    )
                                })
                                .when(self.home.error.is_some(), |view| {
                                    view.child(
                                        Button::new("retry-home")
                                            .outline()
                                            .label("重新加载")
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.load_home(cx)),
                                            ),
                                    )
                                })
                                .when(self.home.loaded && self.home.playlists.is_empty(), |view| {
                                    view.child("QQ 音乐暂时没有返回可显示的歌单推荐")
                                })
                                .child(h_flex().items_start().flex_wrap().gap_4().children(cards)),
                        ),
                    ),
            )
            .into_any_element()
    }
}
