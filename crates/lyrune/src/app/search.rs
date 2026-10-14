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
use lyrune_api::{Track, UserPlaylistId};

use super::navigation::{
    SearchCategory, SearchMoreResults, SearchVisibleCounts, SharedSearchResource, lock_resource,
};
use super::queue::{PlaybackQueue, insert_external_track_after_current};
use super::{LyruneView, SEARCH_PAGE_SIZE, page_horizontal_padding};
use crate::icons::media_icon_hsla;
use crate::library::SearchCard;
use crate::player::AudioPlayer;
use crate::runtime::RUNTIME;

pub(super) struct SearchPageState {
    pub(super) query: String,
    pub(super) resource: Option<SharedSearchResource>,
    pub(super) visible_counts: SearchVisibleCounts,
    pub(super) category: SearchCategory,
    pub(super) scroll_offsets: [Pixels; 4],
    pub(super) pending_scroll: bool,
    pub(super) pending_results_sync: bool,
}

enum SearchRenderData {
    Songs {
        tracks: Option<Vec<Arc<Track>>>,
        has_more: bool,
        is_empty: bool,
    },
    Cards {
        cards: Option<Vec<SearchCard>>,
        has_more: bool,
        is_empty: bool,
        caption_height: Pixels,
    },
}

impl Default for SearchPageState {
    fn default() -> Self {
        Self {
            query: String::new(),
            resource: None,
            visible_counts: SearchVisibleCounts::default(),
            category: SearchCategory::Songs,
            scroll_offsets: [px(0.); 4],
            pending_scroll: false,
            pending_results_sync: true,
        }
    }
}

impl SearchPageState {
    // 初次/分页结果、类别切换及历史页绑定时同步数据；其他重绘只读取状态。
    // 在进入 GPUI 控件更新前释放共享资源锁。
    fn take_render_data(&mut self) -> (Option<SearchRenderData>, bool, bool, Option<String>) {
        let active_category = self.category;
        let sync_results = std::mem::take(&mut self.pending_results_sync);
        self.resource.as_ref().map_or_else(
            || (None, false, false, None),
            |resource| {
                let state = lock_resource(resource);
                (
                    state.results.as_ref().map(|results| match active_category {
                        SearchCategory::Songs => SearchRenderData::Songs {
                            tracks: sync_results.then(|| results.songs.items.clone()),
                            has_more: results.songs.has_more,
                            is_empty: results.songs.items.is_empty(),
                        },
                        SearchCategory::Artists => SearchRenderData::Cards {
                            cards: sync_results.then(|| {
                                results
                                    .artists
                                    .items
                                    .iter()
                                    .cloned()
                                    .map(SearchCard::Artist)
                                    .collect()
                            }),
                            has_more: results.artists.has_more,
                            is_empty: results.artists.items.is_empty(),
                            caption_height: px(62.),
                        },
                        SearchCategory::Albums => SearchRenderData::Cards {
                            cards: sync_results.then(|| {
                                results
                                    .albums
                                    .items
                                    .iter()
                                    .cloned()
                                    .map(SearchCard::Album)
                                    .collect()
                            }),
                            has_more: results.albums.has_more,
                            is_empty: results.albums.items.is_empty(),
                            caption_height: px(74.),
                        },
                        SearchCategory::Playlists => SearchRenderData::Cards {
                            cards: sync_results.then(|| {
                                results
                                    .playlists
                                    .items
                                    .iter()
                                    .cloned()
                                    .map(SearchCard::Playlist)
                                    .collect()
                            }),
                            has_more: results.playlists.has_more,
                            is_empty: results.playlists.items.is_empty(),
                            caption_height: px(74.),
                        },
                    }),
                    state.loading,
                    state.loading_more[active_category.index()],
                    state.error.clone(),
                )
            },
        )
    }
}

impl LyruneView {
    pub(super) fn start_search(
        &mut self,
        resource: SharedSearchResource,
        query: String,
        cx: &mut Context<Self>,
    ) {
        let Some(credential) = self.account.read(cx).credential.clone() else {
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
                // 离开页面后仍保存结果，返回历史页面时可直接复用；仅当前资源需要刷新界面。
                lock_resource(&resource).apply_results(result);
                if this
                    .search
                    .resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource))
                {
                    this.search.pending_results_sync = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn load_more_search(&mut self, cx: &mut Context<Self>) {
        let Some(resource) = self.search.resource.clone() else {
            return;
        };
        let category = self.search.category;
        let target_count = self.search.visible_counts.get(category) + SEARCH_PAGE_SIZE;
        let Some(credential) = self.account.read(cx).credential.clone() else {
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
                *self.search.visible_counts.get_mut(category) = target_count;
                cx.notify();
                return;
            }
            state.loading_more[category.index()] = true;
            state.error = None;
            offset
        };
        *self.search.visible_counts.get_mut(category) = target_count;
        let query = self.search.query.clone();
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
                let results_changed = result.is_ok();
                lock_resource(&resource).apply_more_results(category, result);
                if this
                    .search
                    .resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource))
                {
                    if results_changed && this.search.category == category {
                        this.search.pending_results_sync = true;
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn select_search_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(resource) = self.search.resource.as_ref() else {
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
            .playback
            .read(cx)
            .state
            .track()
            .is_some_and(|current| current.mid == track.mid)
        {
            if self.playback.read(cx).state.load.track.is_none() {
                self.playback
                    .update(cx, |playback, cx| playback.toggle_playback(cx));
            }
            return;
        }

        self.home.recommendation_loading = None;
        let query = self.search.query.clone();
        self.playback.update(cx, |playback, cx| {
            playback.state.pending_restore = None;
            let current_index = playback.state.current_track;
            // 搜索插队保留队列版本；已启动的同队列补页仍可追加，不等同于换歌单。
            let queue_index = if let Some(queue) = &mut playback.state.queue {
                insert_external_track_after_current(queue, current_index, track)
            } else {
                playback.state.queue = Some(PlaybackQueue {
                    playlist_id: UserPlaylistId::Search { query },
                    tracks: vec![track],
                    modified: false,
                    continuation: None,
                });
                0
            };
            playback.start_playback(queue_index, Duration::ZERO, None, true, cx)
        });
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
        let active_category = self.search.category;
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
                        this.search.scroll_offsets = this.current_search_scroll_offsets(cx);
                        if this.search.category != category {
                            this.search.pending_results_sync = true;
                        }
                        this.search.category = category;
                        this.search.pending_scroll = true;
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();

        let (results, search_loading, search_loading_more, search_error) =
            self.search.take_render_data();
        let has_results = results.is_some();
        let mut content = v_flex().w_full();
        let mut is_empty = false;
        if let Some(results) = results {
            match results {
                SearchRenderData::Songs {
                    tracks,
                    has_more,
                    is_empty: empty,
                } => {
                    is_empty = empty;
                    let playback_active = self
                        .playback
                        .read(cx)
                        .state
                        .audio
                        .as_deref()
                        .is_some_and(AudioPlayer::is_playing);
                    self.search_track_table.update(cx, |table, cx| {
                        let delegate = table.delegate_mut();
                        let mut changed = if let Some(tracks) = tracks {
                            delegate.set_tracks(tracks, has_more, search_loading_more);
                            true
                        } else {
                            delegate.set_loading(search_loading_more)
                        };
                        let (playing_index, loading_index) =
                            self.playback.read(cx).state.queue.as_ref().map_or(
                                (None, None),
                                |queue| {
                                    queue.playback_indices_in(
                                        delegate.tracks(),
                                        self.playback.read(cx).state.current_track,
                                        self.playback.read(cx).state.load.track,
                                    )
                                },
                            );
                        changed |= delegate.set_playback_state(
                            playing_index,
                            loading_index,
                            playback_active,
                        );
                        if changed {
                            cx.notify();
                        }
                    });
                    content = v_flex().w_full().flex_1().min_h_0().child(
                        DataTable::new(&self.search_track_table)
                            .bordered(false)
                            .stripe(false)
                            .with_size(px(64.)),
                    );
                }
                SearchRenderData::Cards {
                    cards,
                    has_more,
                    caption_height,
                    is_empty: empty,
                } => {
                    is_empty = empty;
                    let cover_size = if compact { px(132.) } else { px(148.) };
                    let card_width = cover_size + px(16.);
                    let available_width =
                        (content_width - if narrow { px(40.) } else { px(64.) }).max(px(1.));
                    let columns = ((f32::from(available_width) + 16.)
                        / (f32::from(card_width) + 16.))
                        .floor() as usize;
                    self.search_card_grid.update(cx, |list, cx| {
                        if list.delegate_mut().set_cards(
                            cards,
                            columns,
                            card_width,
                            cover_size + caption_height,
                            cover_size,
                            scale_factor,
                            has_more,
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

        if virtual_list_active && self.search.pending_scroll {
            let offset = point(px(0.), self.search.scroll_offsets[active_category.index()]);
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
            self.search.pending_scroll = false;
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
                                        let Some(resource) = this.search.resource.clone() else {
                                            return;
                                        };
                                        this.start_search(resource, this.search.query.clone(), cx)
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
                        .child(format!("没有找到相关{}", self.search.category.label())),
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use lyrune_api::{SearchAlbum, SearchPage, SearchResults};

    use super::{
        SearchCategory, SearchMoreResults, SearchPageState, SearchRenderData, lock_resource,
    };
    use crate::app::navigation::SearchResource;

    #[test]
    fn test_search_result_sync() {
        let resource = Arc::new(Mutex::new(SearchResource::default()));
        lock_resource(&resource).apply_results(Ok(SearchResults {
            songs: SearchPage {
                items: vec![],
                has_more: false,
                next_offset: 0,
            },
            artists: SearchPage {
                items: vec![],
                has_more: false,
                next_offset: 0,
            },
            playlists: SearchPage {
                items: vec![],
                has_more: false,
                next_offset: 0,
            },
            albums: SearchPage {
                items: vec![SearchAlbum {
                    mid: "album".into(),
                    title: "Album".into(),
                    artist: "Artist".into(),
                    cover_url: None,
                }],
                has_more: true,
                next_offset: 1,
            },
        }));
        let mut page = SearchPageState {
            resource: Some(resource.clone()),
            category: SearchCategory::Albums,
            ..Default::default()
        };
        let Some(SearchRenderData::Cards {
            cards: Some(cards),
            is_empty: false,
            ..
        }) = page.take_render_data().0
        else {
            panic!("initial result must reach the grid")
        };
        assert_eq!(cards.len(), 1);
        assert!(matches!(
            page.take_render_data().0,
            Some(SearchRenderData::Cards {
                cards: None,
                is_empty: false,
                ..
            })
        ));

        lock_resource(&resource).loading_more[SearchCategory::Albums.index()] = true;
        let (data, _, loading_more, _) = page.take_render_data();
        assert!(loading_more);
        assert!(matches!(
            data,
            Some(SearchRenderData::Cards { cards: None, .. })
        ));
        lock_resource(&resource)
            .apply_more_results(SearchCategory::Albums, Err(anyhow::anyhow!("offline")));
        let (data, _, loading_more, error) = page.take_render_data();
        assert!(!loading_more && error.is_some());
        assert!(matches!(
            data,
            Some(SearchRenderData::Cards {
                cards: None,
                is_empty: false,
                ..
            })
        ));

        // 空补页也可能改变 has_more；仍需同步数据及分页状态。
        lock_resource(&resource).apply_more_results(
            SearchCategory::Albums,
            Ok(SearchMoreResults::Albums(SearchPage {
                items: vec![],
                has_more: false,
                next_offset: 1,
            })),
        );
        page.pending_results_sync = true;
        assert!(matches!(
            page.take_render_data().0,
            Some(SearchRenderData::Cards {
                cards: Some(_),
                has_more: false,
                ..
            })
        ));

        page.category = SearchCategory::Songs;
        page.pending_results_sync = true;
        assert!(matches!(
            page.take_render_data().0,
            Some(SearchRenderData::Songs {
                tracks: Some(_),
                is_empty: true,
                ..
            })
        ));
        // 恢复之前的类别时，控件必须重新接收该类别的数据。
        page.category = SearchCategory::Albums;
        page.pending_results_sync = true;
        assert!(matches!(
            page.take_render_data().0,
            Some(SearchRenderData::Cards {
                cards: Some(_),
                is_empty: false,
                ..
            })
        ));
    }
}
