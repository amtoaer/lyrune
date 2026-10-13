//! 搜索请求、分页与结果显示。历史页面持有的资源由 navigation 管理。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    list::List,
    scroll::ScrollableElement as _,
    spinner::Spinner,
    table::DataTable,
    v_flex,
};
use qqmusic_api::integration::UserPlaylistId;

use super::navigation::{
    SearchCategory, SearchMoreResults, SharedSearchResource, append_shared_search_page,
    lock_resource,
};
use super::queue::{PlaybackQueue, insert_external_track_after_current};
use super::{LyruneView, SEARCH_PAGE_SIZE, page_horizontal_padding};
use crate::icons::media_icon_hsla;
use crate::library::SearchCard;
use crate::player::AudioPlayer;
use crate::runtime::RUNTIME;

impl LyruneView {
    pub(super) fn start_search(
        &mut self,
        resource: SharedSearchResource,
        query: String,
        cx: &mut Context<Self>,
    ) {
        let Some(credential) = self.credential.clone() else {
            lock_resource(&resource).error = Some("请先登录 QQ 音乐".to_owned());
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            lock_resource(&resource).error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        {
            let mut state = lock_resource(&resource);
            if state.results.is_some() || state.loading {
                cx.notify();
                return;
            }
            state.loading = true;
            state.error = None;
        }
        cx.notify();

        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(30),
                client.search(&credential, &query, SEARCH_PAGE_SIZE as u64),
            )
            .await
            .context("QQ 音乐搜索等待超过 30 秒")
            .and_then(|result| result)
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let mut state = lock_resource(&resource);
                state.loading = false;
                match result {
                    Ok(results) => {
                        state.results = Some(results.into());
                        state.error = None;
                    }
                    Err(error) => {
                        state.results = None;
                        state.error = Some(format!("搜索失败：{error:#}"));
                    }
                }
                // 离开页面后仍保存结果，返回历史页面时可直接复用；仅当前资源需要刷新界面。
                drop(state);
                if this
                    .search_resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource))
                {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn load_more_search(&mut self, cx: &mut Context<Self>) {
        let Some(resource) = self.search_resource.clone() else {
            return;
        };
        let category = self.search_category;
        let target_count = self.search_visible_counts.get(category) + SEARCH_PAGE_SIZE;
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            lock_resource(&resource).error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        let offset = {
            let mut state = lock_resource(&resource);
            if state.loading || state.loading_more[category.index()] {
                return;
            }
            let Some(results) = state.results.as_ref() else {
                return;
            };
            let (len, offset, has_more) = match category {
                SearchCategory::Songs => (
                    results.songs.items.len(),
                    results.songs.next_offset,
                    results.songs.has_more,
                ),
                SearchCategory::Artists => (
                    results.artists.items.len(),
                    results.artists.next_offset,
                    results.artists.has_more,
                ),
                SearchCategory::Albums => (
                    results.albums.items.len(),
                    results.albums.next_offset,
                    results.albums.has_more,
                ),
                SearchCategory::Playlists => (
                    results.playlists.items.len(),
                    results.playlists.next_offset,
                    results.playlists.has_more,
                ),
            };
            // 可见数量属于页面历史，已加载数量属于共享资源；返回旧页面时不重复拉取已有数据。
            if len >= target_count || !has_more {
                *self.search_visible_counts.get_mut(category) = target_count;
                cx.notify();
                return;
            }
            state.loading_more[category.index()] = true;
            state.error = None;
            offset
        };
        *self.search_visible_counts.get_mut(category) = target_count;
        let query = self.search_query.clone();
        cx.notify();

        let task = RUNTIME.spawn(async move {
            match category {
                SearchCategory::Songs => client
                    .search_songs(&credential, &query, offset, SEARCH_PAGE_SIZE as u64)
                    .await
                    .map(SearchMoreResults::Songs),
                SearchCategory::Artists => client
                    .search_artists(&credential, &query, offset, SEARCH_PAGE_SIZE as u64)
                    .await
                    .map(SearchMoreResults::Artists),
                SearchCategory::Albums => client
                    .search_albums(&credential, &query, offset, SEARCH_PAGE_SIZE as u64)
                    .await
                    .map(SearchMoreResults::Albums),
                SearchCategory::Playlists => client
                    .search_playlists(&credential, &query, offset, SEARCH_PAGE_SIZE as u64)
                    .await
                    .map(SearchMoreResults::Playlists),
            }
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let mut state = lock_resource(&resource);
                state.loading_more[category.index()] = false;
                match (state.results.as_mut(), result) {
                    (Some(results), Ok(SearchMoreResults::Songs(page))) => {
                        append_shared_search_page(&mut results.songs, page)
                    }
                    (Some(results), Ok(SearchMoreResults::Artists(page))) => {
                        append_shared_search_page(&mut results.artists, page)
                    }
                    (Some(results), Ok(SearchMoreResults::Albums(page))) => {
                        append_shared_search_page(&mut results.albums, page)
                    }
                    (Some(results), Ok(SearchMoreResults::Playlists(page))) => {
                        append_shared_search_page(&mut results.playlists, page)
                    }
                    (_, Err(error)) => {
                        state.error = Some(format!("继续加载搜索结果失败：{error:#}"));
                    }
                    _ => {}
                }
                drop(state);
                if this
                    .search_resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource))
                {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn select_search_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(resource) = self.search_resource.as_ref() else {
            return;
        };
        let Some(track) = lock_resource(resource)
            .results
            .as_ref()
            .and_then(|results| results.songs.items.get(index))
            .cloned()
        else {
            return;
        };
        if self
            .current_track_data()
            .is_some_and(|current| current.mid == track.mid)
        {
            if self.loading_track.is_none() {
                self.toggle_playback(cx);
            }
            return;
        }

        self.pending_playback_restore = None;
        self.home_recommendation_loading = None;
        let current_index = self.current_track;
        let queue_index = if let Some(queue) = &mut self.playback_queue {
            insert_external_track_after_current(queue, current_index, track)
        } else {
            self.playback_queue = Some(PlaybackQueue {
                playlist_id: UserPlaylistId::Search {
                    query: self.search_query.clone(),
                },
                tracks: vec![track],
                modified: false,
                continuation: None,
            });
            0
        };
        self.start_playback(queue_index, Duration::ZERO, None, true, cx);
    }

    pub(super) fn render_search(
        &mut self,
        compact: bool,
        narrow: bool,
        scale_factor: f32,
        content_width: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let active_category = self.search_category;
        let tabs = SearchCategory::ALL
            .into_iter()
            .map(|category| {
                let selected = active_category == category;
                Button::new(format!("search-category-{}", category.label()))
                    .ghost()
                    .h(px(36.))
                    .px_3()
                    .rounded(px(999.))
                    .selected(selected)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(media_icon_hsla(
                                category.icon(),
                                if selected {
                                    theme.primary
                                } else {
                                    theme.secondary_foreground
                                },
                                px(17.),
                            ))
                            .child(category.label()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.search_scroll_offsets = this.current_search_scroll_offsets(cx);
                        this.search_category = category;
                        this.pending_search_scroll = true;
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();

        let (results, search_loading, search_loading_more, search_error) =
            self.search_resource.as_ref().map_or_else(
                || (None, false, false, None),
                |resource| {
                    let state = lock_resource(resource);
                    (
                        state.results.clone(),
                        state.loading,
                        state.loading_more[active_category.index()],
                        state.error.clone(),
                    )
                },
            );
        let has_results = results.is_some();
        let mut content = v_flex().w_full();
        let mut is_empty = false;
        if let Some(results) = results {
            match self.search_category {
                SearchCategory::Songs => {
                    is_empty = results.songs.items.is_empty();
                    let playing_mid = self.current_track_data().map(|track| track.mid.as_str());
                    let loading_mid = self
                        .loading_track
                        .and_then(|index| self.playback_queue.as_ref()?.tracks.get(index))
                        .map(|track| track.mid.as_str());
                    let playing_index = results
                        .songs
                        .items
                        .iter()
                        .position(|track| Some(track.mid.as_str()) == playing_mid);
                    let loading_index = results
                        .songs
                        .items
                        .iter()
                        .position(|track| Some(track.mid.as_str()) == loading_mid);
                    let playback_active = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
                    self.search_track_table.update(cx, |table, cx| {
                        let delegate = table.delegate_mut();
                        delegate.set_tracks(
                            results.songs.items.clone(),
                            results.songs.has_more,
                            search_loading_more,
                        );
                        delegate.set_playback_state(playing_index, loading_index, playback_active);
                        cx.notify();
                    });
                    content = v_flex().w_full().flex_1().min_h_0().child(
                        DataTable::new(&self.search_track_table)
                            .bordered(false)
                            .stripe(false)
                            .with_size(px(64.)),
                    );
                }
                SearchCategory::Artists => {
                    is_empty = results.artists.items.is_empty();
                    let cover_size = if compact { px(132.) } else { px(148.) };
                    let card_width = cover_size + px(16.);
                    let available_width =
                        (content_width - if narrow { px(40.) } else { px(64.) }).max(px(1.));
                    let columns = ((f32::from(available_width) + 16.)
                        / (f32::from(card_width) + 16.))
                        .floor() as usize;
                    self.search_card_grid.update(cx, |list, cx| {
                        if list.delegate_mut().set_cards(
                            results
                                .artists
                                .items
                                .iter()
                                .cloned()
                                .map(SearchCard::Artist)
                                .collect(),
                            columns,
                            card_width,
                            cover_size + px(62.),
                            cover_size,
                            scale_factor,
                            results.artists.has_more,
                            search_loading_more,
                        ) {
                            cx.notify();
                        }
                    });
                    content = v_flex().w_full().flex_1().min_h_0().child(
                        List::new(&self.search_card_grid)
                            .size_full()
                            .scrollbar_visible(false),
                    );
                }
                SearchCategory::Albums => {
                    is_empty = results.albums.items.is_empty();
                    let cover_size = if compact { px(132.) } else { px(148.) };
                    let card_width = cover_size + px(16.);
                    let available_width =
                        (content_width - if narrow { px(40.) } else { px(64.) }).max(px(1.));
                    let columns = ((f32::from(available_width) + 16.)
                        / (f32::from(card_width) + 16.))
                        .floor() as usize;
                    self.search_card_grid.update(cx, |list, cx| {
                        if list.delegate_mut().set_cards(
                            results
                                .albums
                                .items
                                .iter()
                                .cloned()
                                .map(SearchCard::Album)
                                .collect(),
                            columns,
                            card_width,
                            cover_size + px(74.),
                            cover_size,
                            scale_factor,
                            results.albums.has_more,
                            search_loading_more,
                        ) {
                            cx.notify();
                        }
                    });
                    content = v_flex().w_full().flex_1().min_h_0().child(
                        List::new(&self.search_card_grid)
                            .size_full()
                            .scrollbar_visible(false),
                    );
                }
                SearchCategory::Playlists => {
                    is_empty = results.playlists.items.is_empty();
                    let cover_size = if compact { px(132.) } else { px(148.) };
                    let card_width = cover_size + px(16.);
                    let available_width =
                        (content_width - if narrow { px(40.) } else { px(64.) }).max(px(1.));
                    let columns = ((f32::from(available_width) + 16.)
                        / (f32::from(card_width) + 16.))
                        .floor() as usize;
                    self.search_card_grid.update(cx, |list, cx| {
                        if list.delegate_mut().set_cards(
                            results
                                .playlists
                                .items
                                .iter()
                                .cloned()
                                .map(SearchCard::Playlist)
                                .collect(),
                            columns,
                            card_width,
                            cover_size + px(74.),
                            cover_size,
                            scale_factor,
                            results.playlists.has_more,
                            search_loading_more,
                        ) {
                            cx.notify();
                        }
                    });
                    content = v_flex().w_full().flex_1().min_h_0().child(
                        List::new(&self.search_card_grid)
                            .size_full()
                            .scrollbar_visible(false),
                    );
                }
            }
        }
        let song_list_active = active_category == SearchCategory::Songs
            && !search_loading
            && has_results
            && !is_empty
            && search_error.is_none();
        let card_results_active = active_category != SearchCategory::Songs
            && !search_loading
            && has_results
            && !is_empty
            && search_error.is_none();
        let virtual_list_active = song_list_active || card_results_active;

        if virtual_list_active && self.pending_search_scroll {
            let offset = point(px(0.), self.search_scroll_offsets[active_category.index()]);
            if song_list_active {
                self.search_track_table.update(cx, |table, cx| {
                    let scroll = &mut *table.vertical_scroll_handle.0.borrow_mut();
                    scroll.deferred_scroll_to_item = None;
                    scroll.base_handle.set_offset(offset);
                    cx.notify();
                });
            } else {
                self.search_card_grid.update(cx, |list, cx| {
                    list.scroll_handle().set_offset(offset);
                    cx.notify();
                });
            }
            self.pending_search_scroll = false;
        }

        let body_content = v_flex()
            .w_full()
            .mx_auto()
            .px(page_horizontal_padding(narrow))
            .when(virtual_list_active, |this| this.h_full().min_h_0())
            .when(!virtual_list_active, |this| this.pb_8())
            .gap_5()
            .when(search_loading, |this| {
                this.child(
                    v_flex()
                        .h(px(260.))
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .text_color(theme.muted_foreground)
                        .child(Spinner::new().with_size(px(24.)).color(theme.primary))
                        .child("正在搜索…"),
                )
            })
            .when(
                !search_loading && !has_results && search_error.is_some(),
                |this| {
                    this.child(
                        v_flex()
                            .h(px(260.))
                            .items_center()
                            .justify_center()
                            .gap_4()
                            .text_color(theme.muted_foreground)
                            .child(search_error.clone().unwrap_or_default())
                            .child(
                                Button::new("retry-search")
                                    .outline()
                                    .h(px(44.))
                                    .px_4()
                                    .label("重新搜索")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let Some(resource) = this.search_resource.clone() else {
                                            return;
                                        };
                                        this.start_search(resource, this.search_query.clone(), cx)
                                    })),
                            ),
                    )
                },
            )
            .when(!search_loading && has_results && is_empty, |this| {
                this.child(
                    div()
                        .h(px(220.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.muted_foreground)
                        .child(format!("没有找到相关{}", self.search_category.label())),
                )
            })
            .when(!search_loading && has_results && !is_empty, |this| {
                this.child(content)
            })
            .when(has_results && search_error.is_some(), |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded(px(9.))
                        .bg(theme.danger.opacity(0.1))
                        .text_sm()
                        .text_color(theme.danger)
                        .child(search_error.clone().unwrap_or_default()),
                )
            });
        let body = if virtual_list_active {
            div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(body_content)
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(body_content)
                .into_any_element()
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .bg(theme.background)
            .child(
                v_flex()
                    .w_full()
                    .mx_auto()
                    .px(page_horizontal_padding(narrow))
                    .pt_2()
                    .pb_3()
                    .flex_shrink_0()
                    .child(h_flex().gap_1().children(tabs)),
            )
            .child(body)
            .into_any_element()
    }
}
