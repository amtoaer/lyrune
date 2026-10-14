//! 播放队列的身份、恢复、分页补全与推荐续播。浏览其他页面不会切换当前队列。

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use gpui::Context;
use hashbrown::HashSet;
use lyrune_api::{RecommendationKind, Track, UserPlaylist, UserPlaylistId};

use super::LyruneView;
use super::navigation::{lock_resource, share_items};
use super::playlists::request_playlist_page;
use crate::runtime::RUNTIME;
use crate::settings::{PersistedPlayback, PersistedQueueContinuation};

pub(super) fn insert_track_after_current(
    tracks: &mut Vec<Arc<Track>>,
    current_index: Option<usize>,
    track: Arc<Track>,
) -> usize {
    let mut current_index = current_index.filter(|index| *index < tracks.len());
    if let Some(existing_index) = tracks.iter().position(|item| item.mid == track.mid) {
        if current_index == Some(existing_index) {
            return existing_index;
        }
        tracks.remove(existing_index);
        if let Some(index) = &mut current_index
            && existing_index < *index
        {
            *index -= 1;
        }
    }
    let insert_index = current_index.map_or(tracks.len(), |index| index + 1);
    tracks.insert(insert_index, track);
    insert_index
}

pub(super) fn insert_external_track_after_current(
    queue: &mut PlaybackQueue,
    current_index: Option<usize>,
    track: Arc<Track>,
) -> usize {
    let index = insert_track_after_current(&mut queue.tracks, current_index, track);
    queue.modified = true;
    index
}

pub(super) fn canonical_queue_track_index(
    queue: &PlaybackQueue,
    playlist_id: &UserPlaylistId,
    track_mid: &str,
) -> Option<usize> {
    (!queue.modified && queue.playlist_id == *playlist_id)
        .then(|| queue.tracks.iter().position(|track| track.mid == track_mid))
        .flatten()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RepeatMode {
    Off,
    All,
    One,
}

pub(super) struct PlaybackQueue {
    pub(super) playlist_id: UserPlaylistId,
    pub(super) tracks: Vec<Arc<Track>>,
    // 插入外部歌曲后不再代表原歌单的顺序，不能仅凭 playlist_id 复用。
    pub(super) modified: bool,
    pub(super) continuation: Option<PersistedQueueContinuation>,
}

impl PlaybackQueue {
    // 页面顺序与队列顺序独立；播放和加载标记都按歌曲身份映射到页面行号。
    pub(super) fn playback_indices_in(
        &self,
        tracks: &[Arc<Track>],
        current: Option<usize>,
        loading: Option<usize>,
    ) -> (Option<usize>, Option<usize>) {
        let index_in_page = |index: usize| {
            let track = self.tracks.get(index)?;
            tracks.iter().position(|item| item.mid == track.mid)
        };
        (
            current.and_then(index_in_page),
            loading.and_then(index_in_page),
        )
    }

    // 返回第一首新加入的歌曲，供队尾等待中的推荐续播使用；已有歌曲保留原位置和数据。
    pub(super) fn append_tracks(
        &mut self,
        tracks: impl IntoIterator<Item = Arc<Track>>,
    ) -> Option<usize> {
        let tracks = tracks.into_iter().collect::<Vec<_>>();
        if tracks.is_empty() {
            return None;
        }
        let first_added = self.tracks.len();
        // 集合借用两批歌曲的 mid；先完成筛选、结束借用，再扩展队列。
        let pending = {
            let mut seen = HashSet::with_capacity(first_added + tracks.len());
            seen.extend(self.tracks.iter().map(|track| track.mid.as_str()));
            let mut pending = Vec::with_capacity(tracks.len());
            for track in &tracks {
                if seen.insert(track.mid.as_str()) {
                    pending.push(Arc::clone(track));
                }
            }
            pending
        };
        self.tracks.extend(pending);
        (self.tracks.len() > first_added).then_some(first_added)
    }
}

impl PersistedQueueContinuation {
    pub(super) fn can_load_more(self) -> bool {
        match self {
            Self::Radar { has_more, .. } => has_more,
            Self::Guess => true,
        }
    }
}

impl RepeatMode {
    pub(super) fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Off => "循环",
            Self::All => "列表循环",
            Self::One => "单曲循环",
        }
    }
}

impl LyruneView {
    pub(super) fn select_playlist_track(
        &mut self,
        playlist: UserPlaylist,
        tracks: Vec<Arc<Track>>,
        has_more: bool,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(selected_track) = tracks.get(index) else {
            return;
        };
        if let Some(queue_index) =
            self.playback.queue.as_ref().and_then(|queue| {
                canonical_queue_track_index(queue, &playlist.id, &selected_track.mid)
            })
        {
            if self.playback.current_track == Some(queue_index) {
                if self.playback.load.track.is_none() {
                    self.toggle_playback(cx);
                }
            } else {
                self.start_playback(queue_index, Duration::ZERO, None, true, cx);
            }
            return;
        }

        self.playback.pending_restore = None;
        self.replace_playback_queue(playlist, tracks, has_more, cx);
        self.start_playback(index, Duration::ZERO, None, true, cx);
    }

    pub(super) fn start_home_recommendation(
        &mut self,
        kind: RecommendationKind,
        cx: &mut Context<Self>,
    ) {
        let Some(credential) = self.account.read(cx).credential.clone() else {
            self.notify_error("请先登录 QQ 音乐", cx);
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.notify_error("QQ 音乐客户端不可用", cx);
            cx.notify();
            return;
        };
        let generation = self.home.begin_recommendation(kind);
        cx.notify();

        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(Duration::from_secs(30), async {
                match kind {
                    RecommendationKind::Radar => {
                        let page = client.radar_tracks(&credential, 1).await?;
                        Ok::<_, anyhow::Error>((
                            page.tracks,
                            PersistedQueueContinuation::Radar {
                                next_page: page.next_page,
                                has_more: page.has_more,
                            },
                        ))
                    }
                    RecommendationKind::Guess => Ok((
                        client.guess_tracks(&credential, 5).await?,
                        PersistedQueueContinuation::Guess,
                    )),
                }
            })
            .await
            .context("QQ 音乐个性化推荐请求等待超过 30 秒")
            .and_then(|result| result)
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if !this.home.finish_recommendation(generation, kind) {
                    return;
                }
                match result {
                    Ok((tracks, continuation)) if !tracks.is_empty() => {
                        this.playback.pending_restore = None;
                        this.playback.replace_queue(PlaybackQueue {
                            playlist_id: UserPlaylistId::Recommendation { kind },
                            tracks: share_items(tracks),
                            modified: false,
                            continuation: Some(continuation),
                        });
                        this.start_playback(0, Duration::ZERO, None, true, cx);
                    }
                    Ok(_) => {
                        this.notify_error("QQ 音乐没有返回可播放的推荐歌曲", cx);
                    }
                    Err(error) => {
                        this.notify_error(format!("加载个性化推荐失败：{error:#}"), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn maybe_load_queue_recommendations(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.playback.recommendation_loading {
            return;
        }
        let Some((continuation, remaining)) = self.playback.queue.as_ref().and_then(|queue| {
            let continuation = queue.continuation?;
            let current = self.playback.current_track.unwrap_or_default();
            Some((
                continuation,
                queue.tracks.len().saturating_sub(current.saturating_add(1)),
            ))
        }) else {
            return;
        };
        if !continuation.can_load_more() || (!force && remaining > 2) {
            return;
        }
        let Some(credential) = self.account.read(cx).credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        let generation = self.playback.queue_generation;
        self.playback.recommendation_loading = true;

        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(Duration::from_secs(30), async {
                match continuation {
                    PersistedQueueContinuation::Radar { next_page, .. } => {
                        let page = client.radar_tracks(&credential, next_page).await?;
                        Ok::<_, anyhow::Error>((
                            page.tracks,
                            Some(PersistedQueueContinuation::Radar {
                                next_page: page.next_page,
                                has_more: page.has_more,
                            }),
                        ))
                    }
                    PersistedQueueContinuation::Guess => {
                        let tracks = client.guess_tracks(&credential, 5).await?;
                        let next =
                            (!tracks.is_empty()).then_some(PersistedQueueContinuation::Guess);
                        Ok((tracks, next))
                    }
                }
            })
            .await
            .context("QQ 音乐推荐队列请求等待超过 30 秒")
            .and_then(|result| result)
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.playback.queue_generation != generation {
                    return;
                }
                this.playback.recommendation_loading = false;
                match result {
                    Ok((tracks, continuation)) => {
                        let mut first_added = None;
                        if let Some(queue) = &mut this.playback.queue {
                            queue.continuation = continuation;
                            first_added = queue.append_tracks(tracks.into_iter().map(Arc::new));
                        }
                        this.persist_current_playback(cx);
                        // 已播到队尾时，续播从第一首新加入的歌开始；去重后的空批次不能越界跳歌。
                        if this.playback.waiting_for_recommendation {
                            this.playback.waiting_for_recommendation = false;
                            if let Some(index) = first_added {
                                this.start_playback(index, Duration::ZERO, None, true, cx);
                            }
                        }
                    }
                    Err(error) => {
                        this.playback.waiting_for_recommendation = false;
                        this.notify_error(format!("继续加载推荐歌曲失败：{error:#}"), cx);
                    }
                }
                #[cfg(target_os = "linux")]
                this.sync_mpris(false, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn restore_playback_queue(
        &mut self,
        restore: PersistedPlayback,
        cx: &mut Context<Self>,
    ) {
        let index = restore
            .queue_tracks
            .iter()
            .position(|track| track.mid == restore.track_mid);
        if let Some(index) = index {
            let resume_at = restore.resume_position(restore.queue_tracks[index].duration_seconds);
            self.playback.pending_restore = None;
            self.playback.replace_queue(PlaybackQueue {
                playlist_id: restore.playlist_id,
                tracks: restore.queue_tracks,
                modified: restore.queue_modified,
                continuation: restore.queue_continuation,
            });
            self.start_playback(index, resume_at, None, false, cx);
        } else {
            self.clear_persisted_playback(cx);
        }
    }

    pub(super) fn replace_playback_queue(
        &mut self,
        playlist: UserPlaylist,
        tracks: Vec<Arc<Track>>,
        has_more: bool,
        cx: &mut Context<Self>,
    ) {
        self.home.recommendation_loading = None;
        let playlist_resource = self
            .playlist
            .resource
            .as_ref()
            .filter(|resource| lock_resource(resource).playlist.id == playlist.id)
            .cloned();
        let mut offset = playlist_resource
            .as_ref()
            .map_or(tracks.len() as u64, |resource| {
                lock_resource(resource).next_offset
            });
        let generation = self.playback.replace_queue(PlaybackQueue {
            playlist_id: playlist.id.clone(),
            tracks,
            modified: false,
            continuation: None,
        });

        if !has_more {
            return;
        }
        let Some(credential) = self.account.read(cx).credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        let requests = self.playlist_page_requests.clone();
        // 切换队列只阻止回填当前队列；后台仍可补全它持有的页面资源，供历史页面复用。
        let queue_resource = playlist_resource.clone();

        let task = RUNTIME.spawn(async move {
            let mut remaining = Vec::new();
            let mut has_more = true;
            while has_more {
                let page_offset = offset;
                let page = request_playlist_page(
                    requests.clone(),
                    client.clone(),
                    credential.clone(),
                    playlist.clone(),
                    offset,
                    false,
                )
                .await
                .context("无法补全 QQ 音乐播放队列")?;
                offset = page.next_offset;
                has_more = page.has_more;
                let tracks = share_items(page.tracks);
                if let Some(resource) = &queue_resource {
                    lock_resource(resource).apply_page(
                        page.playlist,
                        tracks.clone(),
                        page.has_more,
                        page.next_offset,
                        page_offset,
                    );
                }
                remaining.extend(tracks);
            }
            Ok::<_, anyhow::Error>(remaining)
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.playback.queue_generation != generation {
                    return;
                }
                if let (Some(queue), Ok(tracks)) = (&mut this.playback.queue, result) {
                    queue.append_tracks(tracks);
                    if let Some(resource) = playlist_resource {
                        let state = lock_resource(&resource);
                        let active = this
                            .playlist
                            .resource
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &resource));
                        let snapshot = active.then(|| (state.tracks.clone(), state.has_more));
                        drop(state);
                        if let Some((tracks, has_more)) = snapshot {
                            this.track_table.update(cx, |table, cx| {
                                table.delegate_mut().set_tracks(tracks, has_more, false);
                                cx.notify();
                            });
                            this.restore_pending_playlist_scroll(cx);
                            this.sync_table_playback_state(cx);
                        }
                    }
                    this.persist_current_playback(cx);
                    #[cfg(target_os = "linux")]
                    this.sync_mpris(false, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn persist_current_playback(&mut self, cx: &mut gpui::App) {
        let Some(account_id) = self.account.read(cx).id() else {
            return;
        };
        let Some(current_playback) = self.playback.snapshot(account_id) else {
            return;
        };
        if self.preferences.read(cx).settings.current_playback.as_ref() != Some(&current_playback) {
            self.preferences.update(cx, |preferences, _| {
                preferences.settings.current_playback = Some(current_playback);
            });
            self.preferences.read(cx).save();
        }
        self.playback.last_persisted_at = Instant::now();
    }

    pub(super) fn clear_persisted_playback(&mut self, cx: &mut gpui::App) {
        self.playback.pending_restore = None;
        if self.preferences.update(cx, |preferences, _| {
            preferences.settings.current_playback.take().is_some()
        }) {
            self.preferences.read(cx).save();
        }
        self.playback.last_persisted_at = Instant::now();
    }
}
