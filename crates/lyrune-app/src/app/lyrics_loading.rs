//! 歌词的内存/磁盘缓存及后台刷新；解析在 lyrics，显示与动画在 lyrics_view。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::Context;
use qqmusic_api::integration::{CredentialSession, ProtocolClient, Track};
use tokio::sync::mpsc;

use super::lyrics::{ParsedLyrics, parse_lyrics};
use super::{LyruneView, unix_timestamp_secs};
use crate::lyrics_cache::LyricDiskCache;
use crate::runtime::RUNTIME;

const LYRIC_CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Clone)]
pub(super) struct MemoryLyrics {
    pub(super) parsed: Arc<ParsedLyrics>,
    fetched_at_secs: u64,
}

impl MemoryLyrics {
    fn is_fresh(&self, now_secs: u64) -> bool {
        now_secs.saturating_sub(self.fetched_at_secs) < LYRIC_CACHE_TTL.as_secs()
    }
}

pub(super) struct LyricsCache {
    disk: Option<LyricDiskCache>,
    pub(super) adopted: HashMap<String, MemoryLyrics>,
    pending: HashMap<String, MemoryLyrics>,
    pub(super) loading: HashSet<String>,
    pub(super) errors: HashMap<String, String>,
}

impl LyricsCache {
    pub(super) fn new(disk: Option<LyricDiskCache>) -> Self {
        Self {
            disk,
            adopted: HashMap::new(),
            pending: HashMap::new(),
            loading: HashSet::new(),
            errors: HashMap::new(),
        }
    }
}

enum LyricLoadEvent {
    Disk {
        lyrics: MemoryLyrics,
        fresh: bool,
    },
    Network {
        result: anyhow::Result<MemoryLyrics>,
        had_cached: bool,
    },
}

impl LyruneView {
    pub(super) fn ensure_track_lyrics(
        &mut self,
        client: ProtocolClient,
        credential: CredentialSession,
        track: Track,
        cx: &mut Context<Self>,
    ) {
        let mid = track.mid.clone();
        if mid.is_empty() {
            return;
        }

        let current_is_same_track = self
            .playback
            .track()
            .is_some_and(|current| current.mid == mid);
        if !current_is_same_track && let Some(pending) = self.lyrics.pending.remove(&mid) {
            self.lyrics.adopted.insert(mid.clone(), pending);
            cx.notify();
        }
        let had_cached = self.lyrics.adopted.contains_key(&mid);
        if self
            .lyrics
            .pending
            .get(&mid)
            .is_some_and(|lyrics| current_is_same_track && lyrics.is_fresh(unix_timestamp_secs()))
            || self
                .lyrics
                .adopted
                .get(&mid)
                .is_some_and(|lyrics| lyrics.is_fresh(unix_timestamp_secs()))
            || !self.lyrics.loading.insert(mid.clone())
        {
            return;
        }

        self.lyrics.errors.remove(&mid);
        cx.notify();

        let disk_cache = self.lyrics.disk.clone();
        let (sender, mut receiver) = mpsc::channel(2);
        let worker_mid = mid.clone();
        drop(RUNTIME.spawn(async move {
            // 先显示磁盘缓存；过期缓存也可用，网络失败时仍保留已有歌词。
            let mut had_cached = had_cached;
            if !had_cached
                && let Some(cache) = disk_cache.as_ref()
                && let Ok(Some(cached)) = cache.load(&worker_mid).await
            {
                let fresh = cached.is_fresh(unix_timestamp_secs(), LYRIC_CACHE_TTL);
                let lyrics = MemoryLyrics {
                    parsed: Arc::new(parse_lyrics(
                        &cached.lyrics.lyric,
                        cached.lyrics.trans_lyric.as_deref(),
                        cached.lyrics.roma_lyric.as_deref(),
                    )),
                    fetched_at_secs: cached.fetched_at_secs,
                };
                if sender
                    .send(LyricLoadEvent::Disk { lyrics, fresh })
                    .await
                    .is_err()
                {
                    return;
                }
                if fresh {
                    return;
                }
                had_cached = true;
            }

            let result = match client.lyrics(&credential, &track).await {
                Ok(result) => {
                    let fetched_at_secs = unix_timestamp_secs();
                    if let Some(cache) = disk_cache.as_ref() {
                        let _ = cache.save(fetched_at_secs, &result).await;
                    }
                    Ok(MemoryLyrics {
                        parsed: Arc::new(parse_lyrics(
                            &result.lyric,
                            result.trans_lyric.as_deref(),
                            result.roma_lyric.as_deref(),
                        )),
                        fetched_at_secs,
                    })
                }
                Err(error) => Err(error),
            };
            let _ = sender
                .send(LyricLoadEvent::Network { result, had_cached })
                .await;
        }));

        cx.spawn(async move |this, cx| {
            while let Some(event) = receiver.recv().await {
                if this
                    .update(cx, |this, cx| {
                        match event {
                            LyricLoadEvent::Disk { lyrics, fresh } => {
                                this.lyrics.errors.remove(&mid);
                                this.lyrics.adopted.insert(mid.clone(), lyrics);
                                if fresh {
                                    this.lyrics.loading.remove(&mid);
                                }
                            }
                            LyricLoadEvent::Network { result, had_cached } => {
                                this.lyrics.loading.remove(&mid);
                                match result {
                                    Ok(lyrics) => {
                                        this.lyrics.errors.remove(&mid);
                                        let is_current = this
                                            .playback
                                            .track()
                                            .is_some_and(|track| track.mid == mid);
                                        let unchanged =
                                            this.lyrics.adopted.get(&mid).is_some_and(|cached| {
                                                cached.parsed == lyrics.parsed
                                            });
                                        // 正在播放的歌词保持本次显示内容，避免刷新后时间轴和排版突然跳变。
                                        // 有变化的新内容暂存到 pending，下次选择该曲时再启用。
                                        if had_cached && is_current {
                                            if unchanged {
                                                if let Some(cached) =
                                                    this.lyrics.adopted.get_mut(&mid)
                                                {
                                                    cached.fetched_at_secs = lyrics.fetched_at_secs;
                                                }
                                                this.lyrics.pending.remove(&mid);
                                            } else {
                                                this.lyrics.pending.insert(mid.clone(), lyrics);
                                            }
                                        } else {
                                            this.lyrics.pending.remove(&mid);
                                            this.lyrics.adopted.insert(mid.clone(), lyrics);
                                        }
                                    }
                                    Err(error) if !had_cached => {
                                        this.lyrics
                                            .errors
                                            .insert(mid.clone(), format!("{error:#}"));
                                    }
                                    Err(_) => {
                                        this.lyrics.errors.remove(&mid);
                                    }
                                }
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if !this.lyrics.adopted.contains_key(&mid) && this.lyrics.loading.remove(&mid) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
