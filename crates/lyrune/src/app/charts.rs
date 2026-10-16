use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    popover::Popover,
    scroll::ScrollableElement as _,
    spinner::Spinner,
    v_flex,
};
use lyrune_api::{ChartGroup, UserPlaylist, UserPlaylistId};

use super::browser::BrowserView;
use super::navigation::{NavigationPage, PlaylistCachePolicy, PlaylistScrollPosition};
use super::{LIBRARY_CACHE_TTL, home_content_width, page_horizontal_padding, unix_timestamp_secs};
use crate::icons::{MediaIcon, media_icon_hsla};
use crate::library::{card_grid_dimensions, playlist_cover};
use crate::runtime::RUNTIME;

#[derive(Default)]
pub(super) struct ChartsPageState {
    pub(super) groups: Arc<Vec<ChartGroup>>,
    pub(super) loading: bool,
    pub(super) generation: u64,
    pub(super) scroll_handle: ScrollHandle,
    fetched_at_secs: u64,
    error: Option<String>,
    period_year: Option<u32>,
    period_year_scroll: ScrollHandle,
    period_week_scroll: ScrollHandle,
}

impl BrowserView {
    pub(super) fn show_charts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = NavigationPage::Charts {
            scroll_offset: px(0.),
        };
        let current = self.current_navigation_page(cx);
        self.record_navigation(current, &target, window, cx);
        self.apply_navigation_page(target, window, cx);
    }

    pub(super) fn load_charts(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.charts.loading
            || (!force
                && self.charts.fetched_at_secs != 0
                && unix_timestamp_secs().saturating_sub(self.charts.fetched_at_secs)
                    < LIBRARY_CACHE_TTL.as_secs())
        {
            return;
        }
        let Some(credential) = self.account.read(cx).credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.charts.error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        self.charts.generation = self.charts.generation.wrapping_add(1);
        let generation = self.charts.generation;
        self.charts.loading = true;
        self.charts.error = None;
        cx.notify();
        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(Duration::from_secs(30), client.chart_groups(&credential))
                .await
                .context("QQ 音乐排行榜请求等待超过 30 秒")?
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .context("排行榜加载任务异常退出")
                .and_then(|result| result);
            let _ = this.update(cx, |this, cx| {
                if this.charts.generation != generation {
                    return;
                }
                this.charts.loading = false;
                match result {
                    Ok(mut groups) => {
                        for group in &mut groups {
                            group
                                .toplist
                                .retain(|chart| !matches!(chart.top_id, 201 | 67));
                        }
                        groups.retain(|group| !group.toplist.is_empty());
                        this.charts.groups = Arc::new(groups);
                        this.charts.fetched_at_secs = unix_timestamp_secs();
                    }
                    Err(error) => this.charts.error = Some(format!("加载排行榜失败：{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_charts(
        &mut self,
        compact: bool,
        narrow: bool,
        content_width: Pixels,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let grid_width = home_content_width(compact, narrow);
        let available_width = (content_width - page_horizontal_padding(narrow) * 2.)
            .max(px(1.))
            .min(grid_width);
        let groups = self.charts.groups.clone();
        let sections = groups
            .iter()
            .map(|group| {
                let featured = group.group_id == 0;
                let (columns, card_width) = if featured {
                    let columns = if available_width < px(640.) { 1 } else { 2 };
                    (
                        columns,
                        (available_width - px(16.) * (columns - 1) as f32) / columns as f32,
                    )
                } else {
                    card_grid_dimensions(available_width, narrow)
                };
                let cover_size = if featured {
                    px(112.)
                } else {
                    card_width - px(16.)
                };
                let cards = group
                    .toplist
                    .iter()
                    .map(|chart| {
                        let playlist = chart.playlist(chart.period.clone());
                        let cover =
                            playlist_cover(&playlist, cover_size, px(12.), scale_factor, cx);
                        let content = if featured {
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_4()
                                .child(div().flex_shrink_0().child(cover))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .items_start()
                                        .gap_2()
                                        .child(
                                            div()
                                                .w_full()
                                                .truncate()
                                                .font_semibold()
                                                .text_lg()
                                                .child(chart.title.clone()),
                                        )
                                        .children(chart.song.iter().take(3).enumerate().map(
                                            |(index, track)| {
                                                div()
                                                    .w_full()
                                                    .truncate()
                                                    .text_sm()
                                                    .text_color(theme.muted_foreground)
                                                    .child(format!(
                                                        "{}  {} · {}",
                                                        index + 1,
                                                        track.title,
                                                        track.singer_name
                                                    ))
                                            },
                                        )),
                                )
                                .into_any_element()
                        } else {
                            v_flex()
                                .w_full()
                                .items_start()
                                .gap_2()
                                .child(cover)
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .font_medium()
                                        .child(chart.title.clone()),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(chart.update_tips.clone()),
                                )
                                .into_any_element()
                        };
                        Button::new(format!("chart-{}", chart.top_id))
                            .debug_selector({
                                let id = chart.top_id;
                                move || format!("chart-{id}")
                            })
                            .ghost()
                            .w(card_width)
                            .min_w_0()
                            .h(if featured {
                                cover_size + px(24.)
                            } else {
                                cover_size + px(74.)
                            })
                            .p_2()
                            .rounded(px(12.))
                            .when(featured, |button| button.bg(theme.muted.opacity(0.45)))
                            .tooltip(chart.title.clone())
                            .child(content)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_home_playlist(playlist.clone(), window, cx);
                            }))
                    })
                    .collect::<Vec<_>>();
                v_flex()
                    .w_full()
                    .gap_3()
                    .when(!featured, |section| {
                        section.child(
                            div()
                                .px_2()
                                .text_size(if narrow { px(22.) } else { px(24.) })
                                .font_semibold()
                                .child(group.group_name.clone()),
                        )
                    })
                    .child(
                        div()
                            .grid()
                            .grid_cols(columns as u16)
                            .w_full()
                            .gap_4()
                            .children(cards),
                    )
            })
            .collect::<Vec<_>>();

        div()
            .id("charts-page")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.charts.scroll_handle)
            .vertical_scrollbar(&self.charts.scroll_handle)
            .child(
                v_flex()
                    .w_full()
                    .items_center()
                    .px(page_horizontal_padding(narrow))
                    .pt(if narrow { px(22.) } else { px(32.) })
                    .pb_8()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(grid_width)
                            .gap_3()
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_h_8()
                                    .px_2()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(if narrow { px(22.) } else { px(24.) })
                                            .font_semibold()
                                            .child("排行榜"),
                                    )
                                    .when(self.charts.error.is_some(), |header| {
                                        header.child(
                                            Button::new("retry-charts")
                                                .ghost()
                                                .label("重试")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.load_charts(true, cx)
                                                })),
                                        )
                                    }),
                            )
                            .when(self.charts.loading, |view| {
                                view.child(
                                    h_flex()
                                        .gap_2()
                                        .text_color(theme.muted_foreground)
                                        .child(
                                            Spinner::new().with_size(px(20.)).color(theme.primary),
                                        )
                                        .child("正在加载排行榜…"),
                                )
                            })
                            .when_some(self.charts.error.clone(), |view, error| {
                                view.child(div().text_color(theme.muted_foreground).child(error))
                            })
                            .when(
                                !self.charts.loading
                                    && self.charts.error.is_none()
                                    && groups.is_empty(),
                                |view| view.child("暂无排行榜"),
                            )
                            .child(v_flex().w_full().gap_6().children(sections)),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_chart_period(
        &self,
        playlist: &UserPlaylist,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let UserPlaylistId::Chart { top_id, period } = &playlist.id else {
            return div().into_any_element();
        };
        let label = period.split_once('_').map_or_else(
            || format!("更新于 {period}"),
            |(year, week)| format!("{year} 年第 {week} 周"),
        );
        let chart = self
            .charts
            .groups
            .iter()
            .flat_map(|group| &group.toplist)
            .find(|chart| chart.top_id == *top_id && !chart.history.year.is_empty());
        let Some(chart) = chart else {
            return div()
                .text_sm()
                .font_normal()
                .text_color(cx.theme().secondary_foreground)
                .child(label)
                .into_any_element();
        };
        let chart = Arc::new(chart.clone());
        let period = period.clone();
        let playlist = playlist.clone();
        let selected_year = period
            .split_once('_')
            .and_then(|(year, _)| year.parse::<u32>().ok());
        let selected_year_index = chart
            .history
            .year
            .iter()
            .position(|year| Some(*year) == selected_year)
            .unwrap_or(0);
        let selected_week_index = chart
            .history
            .sub_period
            .get(selected_year_index)
            .and_then(|weeks| {
                weeks.iter().position(|week| {
                    format!("{}_{week}", chart.history.year[selected_year_index]) == period
                })
            })
            .unwrap_or(0);
        let row_height = px(36.);
        let year_scroll = self.charts.period_year_scroll.clone();
        let week_scroll = self.charts.period_week_scroll.clone();
        let browser = cx.entity().downgrade();
        let on_open_browser = browser.clone();
        Popover::new(format!("chart-period-{}-{period}", chart.top_id))
            .trigger(
                Button::new("chart-period")
                    .debug_selector(|| "chart-period".to_owned())
                    .text()
                    .font_normal()
                    .text_color(cx.theme().secondary_foreground)
                    .label(label)
                    .child(media_icon_hsla(
                        MediaIcon::ChevronDown,
                        cx.theme().secondary_foreground,
                        px(14.),
                    )),
            )
            .on_open_change(move |open, _, cx| {
                if *open {
                    let _ = on_open_browser.update(cx, |this, _| {
                        this.charts.period_year = selected_year;
                        this.charts
                            .period_year_scroll
                            .set_offset(point(px(0.), -row_height * selected_year_index as f32));
                        this.charts
                            .period_week_scroll
                            .set_offset(point(px(0.), -row_height * selected_week_index as f32));
                    });
                }
            })
            .content(move |_, _, cx| {
                let theme = cx.theme().clone();
                let year = browser
                    .read_with(cx, |this, _| this.charts.period_year)
                    .ok()
                    .flatten();
                let year_index = chart
                    .history
                    .year
                    .iter()
                    .position(|item| Some(*item) == year)
                    .unwrap_or(0);
                let years = chart.history.year.iter().copied().map(|year| {
                    let browser = browser.clone();
                    let week_index = if Some(year) == selected_year {
                        selected_week_index
                    } else {
                        0
                    };
                    Button::new(format!("chart-year-{year}"))
                        .debug_selector(move || format!("chart-year-{year}"))
                        .ghost()
                        .w_full()
                        .h(row_height)
                        .flex_shrink_0()
                        .label(year.to_string())
                        .when(chart.history.year[year_index] == year, |button| {
                            button.text_color(theme.primary).bg(theme.muted)
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            let _ = browser.update(cx, |this, _| {
                                this.charts.period_year = Some(year);
                                this.charts
                                    .period_week_scroll
                                    .set_offset(point(px(0.), -row_height * week_index as f32));
                            });
                            cx.notify();
                        }))
                });
                let weeks = chart
                    .history
                    .sub_period
                    .get(year_index)
                    .into_iter()
                    .flatten()
                    .map(|week| {
                        let value = format!("{}_{week}", chart.history.year[year_index]);
                        let mut playlist = playlist.clone();
                        playlist.id = UserPlaylistId::Chart {
                            top_id: chart.top_id,
                            period: value.clone(),
                        };
                        let browser = browser.clone();
                        Button::new(format!("chart-week-{value}"))
                            .debug_selector({
                                let value = value.clone();
                                move || format!("chart-week-{value}")
                            })
                            .ghost()
                            .w_full()
                            .h(row_height)
                            .flex_shrink_0()
                            .label(format!("第 {week} 周"))
                            .when(value == period, |button| {
                                button.text_color(theme.primary).bg(theme.muted)
                            })
                            .on_click(cx.listener(move |state, _, window, cx| {
                                state.dismiss(window, cx);
                                let _ = browser.update(cx, |this, cx| {
                                    this.open_playlist(
                                        playlist.clone(),
                                        None,
                                        false,
                                        PlaylistCachePolicy::Fresh,
                                        PlaylistScrollPosition::top(),
                                        None,
                                        cx,
                                    );
                                });
                            }))
                    });
                h_flex()
                    .w(px(260.))
                    .h(px(280.))
                    .gap_2()
                    .items_start()
                    .child(
                        v_flex()
                            .id("chart-years")
                            .debug_selector(|| "chart-years".to_owned())
                            .w(px(88.))
                            .h_full()
                            .overflow_y_scroll()
                            .track_scroll(&year_scroll)
                            .children(years)
                            .vertical_scrollbar(&year_scroll),
                    )
                    .child(
                        v_flex()
                            .id(format!(
                                "chart-weeks-{}-{}",
                                chart.top_id, chart.history.year[year_index]
                            ))
                            .debug_selector(|| "chart-weeks".to_owned())
                            .flex_1()
                            .h_full()
                            .overflow_y_scroll()
                            .track_scroll(&week_scroll)
                            .children(weeks)
                            .vertical_scrollbar(&week_scroll),
                    )
            })
            .into_any_element()
    }
}
