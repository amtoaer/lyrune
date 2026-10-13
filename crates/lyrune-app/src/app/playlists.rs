//! 用户歌单、分页浏览与喜欢状态。页面资源和播放队列可共享分页请求，但各自保存状态。

use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::Context as _;
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    table::DataTable,
    v_flex,
};
use qqmusic_api::integration::{
    CredentialSession, PlaylistPage, ProtocolClient, Track, UserPlaylist, UserPlaylistId,
    UserProfile,
};

use super::navigation::{
    MainContent, NavigationPage, PlaylistCachePolicy, PlaylistScrollPosition,
    SharedPlaylistResource, lock_resource, resolved_playlist_scroll_row, share_items,
    update_liked_playlist_resource,
};
use super::{
    LIBRARY_CACHE_TTL, LyruneView, PAGE_SIZE, page_horizontal_padding, unix_timestamp_secs,
};
use crate::http::cached_image_source;
use crate::icons::{MediaIcon, media_icon};
use crate::library::playlist_cover;
use crate::player::AudioPlayer;
use crate::runtime::RUNTIME;
use crate::settings::PersistedLibraryView;
use crate::singleflight::SingleFlight;

pub(super) fn playlist_title_is_long(title: &str) -> bool {
    title
        .chars()
        .map(|character| if character.is_ascii() { 1 } else { 2 })
        .sum::<usize>()
        > 24
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct PlaylistPageKey {
    pub(super) account_id: u64,
    pub(super) playlist_id: UserPlaylistId,
    pub(super) offset: u64,
}

// 页面补页和播放队列补全会请求同一页；账号、歌单、offset 共同确定 SingleFlight 身份。
pub(super) async fn request_playlist_page(
    requests: SingleFlight<PlaylistPageKey, PlaylistPage>,
    client: ProtocolClient,
    credential: CredentialSession,
    playlist: UserPlaylist,
    offset: u64,
    force: bool,
) -> anyhow::Result<PlaylistPage> {
    let account_id = credential
        .snapshot()
        .context("QQ 音乐登录凭据已注销")?
        .music_id;
    let key = PlaylistPageKey {
        account_id,
        playlist_id: playlist.id.clone(),
        offset,
    };
    requests
        .run(key, force, move || async move {
            tokio::time::timeout(
                Duration::from_secs(30),
                client.playlist_page(&credential, &playlist, offset, PAGE_SIZE),
            )
            .await
            .context("QQ 音乐歌单分页请求等待超过 30 秒")?
        })
        .await
}

impl LyruneView {
    pub(super) fn load_library(&mut self, force_refresh: bool, cx: &mut Context<Self>) {
        let Some(credential) = self.account.credential.clone() else {
            return;
        };
        let Some(account_id) = credential.snapshot().map(|credential| credential.music_id) else {
            return;
        };
        if !force_refresh
            && let Some((profile, playlists)) = self.library_cache.fresh_directory(
                account_id,
                unix_timestamp_secs(),
                LIBRARY_CACHE_TTL,
            )
        {
            self.library_loading = false;
            self.apply_library(account_id, profile, playlists, false, cx);
            self.validate_cached_credential(cx);
            return;
        }
        let Some(client) = self.protocol_client.clone() else {
            self.notify_error("QQ 音乐客户端不可用", cx);
            cx.notify();
            return;
        };
        self.library_generation = self.library_generation.wrapping_add(1);
        let generation = self.library_generation;
        self.library_loading = true;
        cx.notify();
        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(Duration::from_secs(30), async {
                tokio::try_join!(
                    client.user_profile(&credential),
                    client.user_playlists(&credential)
                )
            })
            .await
            .context("QQ 音乐用户资料和歌单请求等待超过 30 秒")?
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.library_generation != generation {
                    return;
                }
                this.library_loading = false;
                match result {
                    Ok((profile, playlists)) => {
                        this.library_cache.replace_directory(
                            account_id,
                            profile.clone(),
                            playlists.clone(),
                            unix_timestamp_secs(),
                        );
                        this.apply_library(account_id, profile, playlists, force_refresh, cx);
                    }
                    Err(error) => {
                        this.notify_error(format!("加载 QQ 音乐资料失败：{error:#}"), cx);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn apply_library(
        &mut self,
        account_id: u64,
        profile: UserProfile,
        playlists: Vec<UserPlaylist>,
        force_refresh: bool,
        cx: &mut Context<Self>,
    ) {
        self.account.profile = Some(profile);
        let count = playlists.len();
        let viewed_index = self
            .settings
            .last_library_view
            .as_ref()
            .filter(|view| view.account_id == account_id)
            .and_then(|view| {
                playlists
                    .iter()
                    .position(|playlist| playlist.id == view.playlist_id)
            });
        let playback_restore = self
            .pending_playback_restore
            .clone()
            .filter(|restore| restore.account_id == account_id);
        if self.pending_playback_restore.is_some() && playback_restore.is_none() {
            self.clear_persisted_playback();
        }
        self.playlist_list.update(cx, |list, cx| {
            list.delegate_mut().set_playlists(playlists);
            cx.notify();
        });
        if count > 0 && self.main_content == MainContent::Playlist {
            self.select_playlist_with_refresh(
                viewed_index.unwrap_or(0),
                force_refresh,
                false,
                None,
                cx,
            );
        } else if count == 0 {
            cx.notify();
        }
        if let Some(restore) = playback_restore {
            self.restore_playback_queue(restore, cx);
        }
    }

    pub(super) fn select_playlist(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_playlist_with_refresh(index, false, true, Some(window), cx);
    }

    fn select_playlist_with_refresh(
        &mut self,
        index: usize,
        force_refresh: bool,
        record_navigation: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let playlist = self
            .playlist_list
            .read(cx)
            .delegate()
            .playlist(index)
            .cloned();
        let Some(playlist) = playlist else {
            return;
        };

        if let Some(account_id) = self.account_id() {
            let view = PersistedLibraryView {
                account_id,
                playlist_id: playlist.id.clone(),
            };
            if self.settings.last_library_view.as_ref() != Some(&view) {
                self.settings.last_library_view = Some(view);
                self.persist_settings();
            }
        }

        let resource = self.shared_playlist_resource(
            playlist.clone(),
            force_refresh,
            PlaylistCachePolicy::Fresh,
        );
        if record_navigation {
            let target = NavigationPage::Playlist {
                playlist: playlist.clone(),
                selected_index: Some(index),
                scroll_position: PlaylistScrollPosition::top(),
                resource: Some(resource.clone()),
            };
            let current = self.current_navigation_page(cx);
            if let Some(window) = window {
                self.record_navigation(current, &target, window, cx);
            } else {
                self.navigation_history.record(current, &target);
            }
        }
        self.open_playlist(
            playlist,
            Some(index),
            force_refresh,
            PlaylistCachePolicy::Fresh,
            PlaylistScrollPosition::top(),
            Some(resource),
            cx,
        );
    }

    pub(super) fn open_home_playlist(
        &mut self,
        playlist: UserPlaylist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let resource =
            self.shared_playlist_resource(playlist.clone(), false, PlaylistCachePolicy::Fresh);
        let target = NavigationPage::Playlist {
            playlist: playlist.clone(),
            selected_index: None,
            scroll_position: PlaylistScrollPosition::top(),
            resource: Some(resource.clone()),
        };
        let current = self.current_navigation_page(cx);
        self.record_navigation(current, &target, window, cx);
        self.playlist_list.update(cx, |list, cx| {
            list.set_selected_index(None, window, cx);
        });
        self.open_playlist(
            playlist,
            None,
            false,
            PlaylistCachePolicy::Fresh,
            PlaylistScrollPosition::top(),
            Some(resource),
            cx,
        );
    }

    pub(super) fn restore_pending_playlist_scroll(&mut self, cx: &mut Context<Self>) {
        let Some(position) = self.pending_playlist_scroll_position else {
            return;
        };
        let target_row = {
            let table = self.track_table.read(cx);
            resolved_playlist_scroll_row(
                position.row,
                table.delegate().tracks().len(),
                table.delegate().has_more(),
            )
        };
        // 历史位置可能超出当前已加载页，等补页完成后再恢复，避免提前钳制到末行。
        let Some(target_row) = target_row else {
            return;
        };
        self.pending_playlist_scroll_position = None;
        self.track_table.update(cx, |table, cx| {
            if target_row == position.row {
                let scroll_state = &mut *table.vertical_scroll_handle.0.borrow_mut();
                scroll_state.deferred_scroll_to_item = None;
                let current_offset = scroll_state.base_handle.offset();
                scroll_state
                    .base_handle
                    .set_offset(point(current_offset.x, position.offset_y));
            } else {
                table.scroll_to_row(target_row, cx);
            }
            cx.notify();
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_playlist(
        &mut self,
        playlist: UserPlaylist,
        selected_index: Option<usize>,
        force_refresh: bool,
        cache_policy: PlaylistCachePolicy,
        scroll_position: PlaylistScrollPosition,
        resource: Option<SharedPlaylistResource>,
        cx: &mut Context<Self>,
    ) {
        self.main_content = MainContent::Playlist;
        self.search_resource = None;
        self.artist_resource = None;

        self.playlist_force_refresh = force_refresh;
        self.selected_playlist_index = selected_index;
        self.pending_playlist_scroll_position = Some(scroll_position);
        if let Some(index) = selected_index {
            self.playlist_list.update(cx, |list, cx| {
                list.delegate_mut().set_selected(index);
                cx.notify();
            });
        }
        self.track_table.update(cx, |table, cx| {
            table.clear_selection(cx);
            table
                .delegate_mut()
                .reset(playlist.id == UserPlaylistId::Liked);
            table.refresh(cx);
            cx.notify();
        });
        let resource = resource.unwrap_or_else(|| {
            self.shared_playlist_resource(playlist.clone(), force_refresh, cache_policy)
        });
        let (playlist, tracks, has_more, loading, loaded) = {
            let state = lock_resource(&resource);
            (
                state.playlist.clone(),
                state.tracks.clone(),
                state.has_more,
                state.loading,
                state.fetched_at_secs != 0,
            )
        };
        self.selected_playlist_resource = Some(resource);
        self.selected_playlist = Some(playlist.clone());
        if let Some(index) = self.selected_playlist_index {
            self.playlist_list.update(cx, |list, cx| {
                list.delegate_mut().update_playlist(index, playlist.clone());
                cx.notify();
            });
        }
        self.track_table.update(cx, |table, cx| {
            table
                .delegate_mut()
                .set_tracks(tracks.clone(), has_more, loading);
            cx.notify();
        });
        self.restore_pending_playlist_scroll(cx);
        self.sync_table_playback_state(cx);
        cx.notify();
        self.prune_page_resources();
        if !loaded || self.pending_playlist_scroll_position.is_some() {
            self.load_playlist_page(cx);
        }
    }

    pub(super) fn load_playlist_page(&mut self, cx: &mut Context<Self>) {
        let Some(credential) = self.account.credential.clone() else {
            return;
        };
        let Some(resource) = self.selected_playlist_resource.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.notify_error("QQ 音乐客户端不可用", cx);
            cx.notify();
            return;
        };

        let (playlist, offset) = {
            let mut state = lock_resource(&resource);
            if state.loading || !state.has_more {
                return;
            }
            state.loading = true;
            (state.playlist.clone(), state.next_offset)
        };
        let force_refresh = offset == 0 && self.playlist_force_refresh;
        self.playlist_force_refresh = false;
        let requests = self.playlist_page_requests.clone();
        self.track_table.update(cx, |table, cx| {
            table.delegate_mut().set_loading(true);
            cx.notify();
        });

        let task = RUNTIME.spawn(request_playlist_page(
            requests,
            client,
            credential,
            playlist,
            offset,
            force_refresh,
        ));

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let active = this
                    .selected_playlist_resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource));
                let mut state = lock_resource(&resource);
                state.loading = false;
                let error = match result {
                    Ok(page) => {
                        state.apply_page(
                            page.playlist,
                            share_items(page.tracks),
                            page.has_more,
                            page.next_offset,
                            offset,
                        );
                        None
                    }
                    Err(error) => Some(format!("加载歌单失败：{error:#}")),
                };
                let snapshot =
                    active.then(|| (state.playlist.clone(), state.tracks.clone(), state.has_more));
                drop(state);
                if let Some((playlist, tracks, has_more)) = snapshot {
                    this.selected_playlist = Some(playlist.clone());
                    if let Some(index) = this.selected_playlist_index {
                        this.playlist_list.update(cx, |list, cx| {
                            list.delegate_mut().update_playlist(index, playlist.clone());
                            cx.notify();
                        });
                    }
                    this.track_table.update(cx, |table, cx| {
                        table
                            .delegate_mut()
                            .set_tracks(tracks.clone(), has_more, false);
                        cx.notify();
                    });
                    if let Some(error) = error {
                        this.notify_error(error, cx);
                    }
                    this.restore_pending_playlist_scroll(cx);
                    if this.pending_playlist_scroll_position.is_some() {
                        this.load_playlist_page(cx);
                    }
                    this.sync_table_playback_state(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn select_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let (tracks, has_more) = {
            let table = self.track_table.read(cx);
            (
                table.delegate().tracks().to_vec(),
                table.delegate().has_more(),
            )
        };
        let Some(playlist) = self.selected_playlist.clone() else {
            return;
        };
        self.select_playlist_track(playlist, tracks, has_more, index, cx);
    }

    pub(super) fn sync_table_playback_state(&mut self, cx: &mut Context<Self>) {
        let (playing, loading) = {
            let table = self.track_table.read(cx);
            self.playback_queue.as_ref().map_or((None, None), |queue| {
                queue.playback_indices_in(
                    table.delegate().tracks(),
                    self.current_track,
                    self.playback_load.track,
                )
            })
        };
        let playback_active = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
        self.track_table.update(cx, |table, cx| {
            table
                .delegate_mut()
                .set_playback_state(playing, loading, playback_active);
            cx.notify();
        });
    }

    pub(super) fn ensure_track_like_state(&mut self, mid: String, cx: &mut Context<Self>) {
        if self.liked_tracks.contains_key(&mid) || self.liked_state_loading.contains(&mid) {
            return;
        }
        let Some(credential) = self.account.credential.clone() else {
            return;
        };
        let Some(account_id) = credential.snapshot().map(|credential| credential.music_id) else {
            return;
        };
        let liked = self
            .page_resource_cache
            .playlists
            .get(&(account_id, UserPlaylistId::Liked))
            .and_then(Weak::upgrade)
            .and_then(|resource| {
                let state = lock_resource(&resource);
                if !state.is_fresh(unix_timestamp_secs()) || state.fetched_at_secs == 0 {
                    None
                } else if state.tracks.iter().any(|track| track.mid == mid) {
                    Some(true)
                } else if state.has_more {
                    None
                } else {
                    Some(false)
                }
            });
        if let Some(liked) = liked {
            self.liked_tracks.insert(mid, liked);
            cx.notify();
            return;
        }
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        self.liked_state_loading.insert(mid.clone());
        let request_mid = mid.clone();
        let task =
            RUNTIME.spawn(async move { client.track_liked(&credential, &request_mid).await });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.liked_state_loading.remove(&mid);
                if this.account_id().is_none_or(|id| id != account_id) {
                    return;
                }
                match result {
                    Ok(liked) => {
                        this.liked_tracks.insert(mid.clone(), liked);
                    }
                    Err(error) => {
                        if this
                            .current_track_data()
                            .is_some_and(|track| track.mid == mid)
                        {
                            this.notify_error(format!("读取当前歌曲的喜欢状态失败：{error:#}"), cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn toggle_current_track_liked(&mut self, cx: &mut Context<Self>) {
        let Some(track) = self.current_track_data().cloned() else {
            return;
        };
        let Some(liked) = self.liked_tracks.get(&track.mid).copied() else {
            self.ensure_track_like_state(track.mid.clone(), cx);
            return;
        };
        self.set_track_liked(track, !liked, cx);
    }

    pub(super) fn unlike_track(&mut self, track: Track, cx: &mut Context<Self>) {
        if self.liked_toggle_loading.contains(&track.mid) {
            return;
        }
        self.liked_tracks.insert(track.mid.clone(), true);
        self.set_track_liked(track, false, cx);
    }

    fn set_track_liked(&mut self, track: Track, liked: bool, cx: &mut Context<Self>) {
        let mid = track.mid.clone();
        if self.liked_toggle_loading.contains(&mid) {
            return;
        }
        let Some(credential) = self.account.credential.clone() else {
            return;
        };
        let Some(account_id) = credential.snapshot().map(|credential| credential.music_id) else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.notify_error("QQ 音乐客户端不可用", cx);
            cx.notify();
            return;
        };
        if track.song_id.is_none() {
            self.notify_error(
                format!("歌曲“{}”缺少数字 ID，暂时无法修改喜欢状态", track.title),
                cx,
            );
            cx.notify();
            return;
        }
        let previous = self.liked_tracks.insert(mid.clone(), liked);
        self.liked_toggle_loading.insert(mid.clone());
        cx.notify();

        let request_track = track.clone();
        let task = RUNTIME.spawn(async move {
            client
                .set_track_liked(&credential, &request_track, liked)
                .await
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.liked_toggle_loading.remove(&mid);
                if this.account_id().is_none_or(|id| id != account_id) {
                    return;
                }
                match result {
                    Ok(()) => {
                        this.apply_track_liked(account_id, track.clone(), liked, cx);
                    }
                    Err(error) => {
                        if this.liked_tracks.get(&mid) == Some(&liked) {
                            if let Some(previous) = previous {
                                this.liked_tracks.insert(mid.clone(), previous);
                            } else {
                                this.liked_tracks.remove(&mid);
                            }
                        }
                        this.notify_error(
                            format!(
                                "{}失败：{error:#}",
                                if liked {
                                    "喜欢歌曲"
                                } else {
                                    "取消喜欢"
                                }
                            ),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_track_liked(
        &mut self,
        account_id: u64,
        track: Track,
        liked: bool,
        cx: &mut Context<Self>,
    ) {
        self.library_cache
            .update_liked_track_count(account_id, liked);
        self.playlist_list.update(cx, |list, cx| {
            list.delegate_mut().update_liked_track_count(liked);
            cx.notify();
        });
        let cached_resource = self
            .page_resource_cache
            .playlists
            .get(&(account_id, UserPlaylistId::Liked))
            .and_then(Weak::upgrade);
        let active_resource = self
            .selected_playlist_resource
            .as_ref()
            .and_then(|resource| {
                (lock_resource(resource).playlist.id == UserPlaylistId::Liked)
                    .then(|| resource.clone())
            });
        // 喜欢歌单可能同时被当前页和多条历史记录持有；逐个更新，并按 Arc 身份去重。
        let mut resources = self
            .navigation_history
            .playlist_resources(&UserPlaylistId::Liked);
        for resource in active_resource.iter().chain(cached_resource.iter()) {
            if !resources
                .iter()
                .any(|current| Arc::ptr_eq(current, resource))
            {
                resources.push(resource.clone());
            }
        }
        let mut active_snapshot = None;
        for resource in resources {
            let snapshot = update_liked_playlist_resource(&resource, &track, liked);
            if active_resource
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &resource))
            {
                active_snapshot = snapshot;
            }
        }
        let Some((playlist, tracks, has_more)) = active_snapshot else {
            return;
        };
        self.selected_playlist = Some(playlist);
        self.track_table.update(cx, |table, cx| {
            table.delegate_mut().set_tracks(tracks, has_more, false);
            table.clear_selection(cx);
            cx.notify();
        });
        self.sync_table_playback_state(cx);
    }

    fn render_playlist_header(
        &mut self,
        compact: bool,
        narrow: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let Some(playlist) = self.selected_playlist.clone() else {
            return div()
                .h(px(220.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("从左侧选择一个歌单")
                .into_any_element();
        };
        let cover_size = if narrow {
            px(112.)
        } else if compact {
            px(142.)
        } else {
            px(176.)
        };
        let cover = div().rounded(px(18.)).shadow_md().child(playlist_cover(
            &playlist,
            cover_size,
            px(18.),
            scale_factor,
            cx,
        ));
        let owned_by_profile = matches!(
            &playlist.id,
            UserPlaylistId::Liked | UserPlaylistId::Created { .. }
        );
        let owner = (!playlist.owner.is_empty())
            .then(|| playlist.owner.clone())
            .or_else(|| {
                if owned_by_profile {
                    self.account
                        .profile
                        .as_ref()
                        .map(|profile| profile.nickname.clone())
                } else {
                    None
                }
            });
        let owner_avatar_url = playlist.owner_avatar_url.clone().or_else(|| {
            owned_by_profile
                .then(|| self.account.profile.as_ref()?.avatar_url.clone())
                .flatten()
        });
        let owner_identity = owner.zip(owner_avatar_url);
        let has_owner = owner_identity.is_some();
        let has_tracks = !self.track_table.read(cx).delegate().tracks().is_empty();
        let long_title = playlist_title_is_long(&playlist.title);
        let title_size = if long_title {
            if narrow {
                px(22.)
            } else if compact {
                px(28.)
            } else {
                px(36.)
            }
        } else if narrow {
            px(28.)
        } else if compact {
            px(34.)
        } else {
            px(44.)
        };
        let title_line_height = if long_title {
            if narrow {
                px(29.)
            } else if compact {
                px(36.)
            } else {
                px(46.)
            }
        } else if narrow {
            px(36.)
        } else if compact {
            px(44.)
        } else {
            px(56.)
        };
        div()
            .min_h(if narrow {
                px(182.)
            } else if compact {
                px(206.)
            } else {
                px(238.)
            })
            .w_full()
            .flex_shrink_0()
            .px(page_horizontal_padding(narrow))
            .pt_4()
            .pb_3()
            .child(
                h_flex()
                    .w_full()
                    .items_end()
                    .gap(if narrow {
                        px(16.)
                    } else if compact {
                        px(20.)
                    } else {
                        px(28.)
                    })
                    .child(cover)
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_2()
                            .child(
                                h_flex().child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded(px(999.))
                                        .bg(theme.muted)
                                        .text_xs()
                                        .font_medium()
                                        .text_color(theme.muted_foreground)
                                        .child(match &playlist.id {
                                            UserPlaylistId::Artist { .. } => "歌手",
                                            UserPlaylistId::Album { .. } => "专辑",
                                            _ => "歌单",
                                        }),
                                ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .w_full()
                                    .when_else(
                                        long_title,
                                        |title| title.line_clamp(3),
                                        |title| title.truncate(),
                                    )
                                    .text_size(title_size)
                                    .line_height(title_line_height)
                                    .font_semibold()
                                    .child(playlist.title),
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_2()
                                    .text_sm()
                                    .font_medium()
                                    .when_some(owner_identity, |this, (owner, url)| {
                                        this.child(
                                            img(cached_image_source(url, px(18.), scale_factor))
                                                .size(px(18.))
                                                .flex_shrink_0()
                                                .rounded(px(999.)),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .max_w(if narrow { px(120.) } else { px(200.) })
                                                .truncate()
                                                .child(owner),
                                        )
                                    })
                                    .child(
                                        div()
                                            .font_normal()
                                            .text_color(theme.secondary_foreground)
                                            .child(format!(
                                                "{}{} 首歌曲",
                                                if has_owner { "· " } else { "" },
                                                playlist.track_count
                                            )),
                                    ),
                            )
                            .child(
                                h_flex().pt_2().child(
                                    Button::new("play-all")
                                        .primary()
                                        .rounded(px(999.))
                                        .h(px(44.))
                                        .min_w(px(44.))
                                        .px_4()
                                        .tooltip("从第一首开始播放")
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .text_color(theme.primary_foreground)
                                                .child(media_icon(
                                                    MediaIcon::Play,
                                                    self.color_theme.palette().primary_foreground,
                                                    px(17.),
                                                ))
                                                .child("播放全部"),
                                        )
                                        .when(!has_tracks, |button| button.bg(theme.button_primary))
                                        .disabled(!has_tracks)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.select_track(0, cx)),
                                        ),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_playlist_content(
        &mut self,
        compact: bool,
        narrow: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme.background)
            .child(self.render_playlist_header(compact, narrow, scale_factor, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(page_horizontal_padding(narrow))
                    .pb_4()
                    .child(
                        div()
                            .size_full()
                            .overflow_hidden()
                            .bg(theme.background)
                            .child(
                                DataTable::new(&self.track_table)
                                    .bordered(false)
                                    .stripe(false)
                                    .with_size(px(64.)),
                            ),
                    ),
            )
            .into_any_element()
    }
}
