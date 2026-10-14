//! 歌词的内存/磁盘缓存及后台刷新；解析在 lyrics，显示与动画在 lyrics_view。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gpui::Context;
use hashbrown::HashMap;
use lyrune_api::{CredentialSession, ProtocolClient, Track};
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

    fn adopt_pending(&mut self, mid: &str) -> bool {
        let Some(pending) = self.pending.remove(mid) else {
            return false;
        };
        self.adopted.insert(mid.to_owned(), pending);
        true
    }

    fn apply(&mut self, mid: &str, event: LyricLoadEvent, is_current: bool) {
        match event {
            LyricLoadEvent::Disk { lyrics, fresh } => {
                self.errors.remove(mid);
                self.adopted.insert(mid.to_owned(), lyrics);
                if fresh {
                    self.loading.remove(mid);
                }
            }
            LyricLoadEvent::Network { result, had_cached } => {
                self.loading.remove(mid);
                match result {
                    Ok(lyrics) => {
                        self.errors.remove(mid);
                        let unchanged = self
                            .adopted
                            .get(mid)
                            .is_some_and(|cached| cached.parsed == lyrics.parsed);
                        // 正在播放的歌词保持本次显示内容，避免刷新后时间轴和排版突然跳变。
                        // 有变化的新内容暂存到 pending，下次选择该曲时再启用。
                        if had_cached && is_current {
                            if unchanged {
                                if let Some(cached) = self.adopted.get_mut(mid) {
                                    cached.fetched_at_secs = lyrics.fetched_at_secs;
                                }
                                self.pending.remove(mid);
                            } else {
                                self.pending.insert(mid.to_owned(), lyrics);
                            }
                        } else {
                            self.pending.remove(mid);
                            self.adopted.insert(mid.to_owned(), lyrics);
                        }
                    }
                    Err(error) if !had_cached => {
                        self.errors.insert(mid.to_owned(), format!("{error:#}"));
                    }
                    Err(_) => {
                        self.errors.remove(mid);
                    }
                }
            }
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
        if !current_is_same_track && self.lyrics.adopt_pending(&mid) {
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
                        let is_current =
                            this.playback.track().is_some_and(|track| track.mid == mid);
                        this.lyrics.apply(&mid, event, is_current);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                // 保留原通道结束规则：只清理尚无内容的请求；不能把磁盘/网络两阶段改成统一 finally。
                if !this.lyrics.adopted.contains_key(&mid) && this.lyrics.loading.remove(&mid) {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(text: &str, fetched_at_secs: u64) -> MemoryLyrics {
        MemoryLyrics {
            parsed: Arc::new(parse_lyrics(text, None, None)),
            fetched_at_secs,
        }
    }

    #[test]
    fn test_refresh_keeps_current_lyrics() {
        let mut cache = LyricsCache::new(None);
        let old = cached("[00:01.00]old", 1);
        cache.loading.insert("song".to_owned());
        cache.apply(
            "song",
            LyricLoadEvent::Disk {
                lyrics: old.clone(),
                fresh: false,
            },
            true,
        );
        assert!(cache.loading.contains("song"));

        let changed = cached("[00:02.00]changed", 2);
        cache.apply(
            "song",
            LyricLoadEvent::Network {
                result: Ok(changed.clone()),
                had_cached: true,
            },
            true,
        );
        assert!(!cache.loading.contains("song"));
        assert!(Arc::ptr_eq(&cache.adopted["song"].parsed, &old.parsed));
        assert_eq!(cache.pending["song"].parsed, changed.parsed);

        // 相同内容只刷新时间，不替换当前渲染持有的 Arc，并清除之前待采用的不同内容。
        cache.apply(
            "song",
            LyricLoadEvent::Network {
                result: Ok(cached("[00:01.00]old", 3)),
                had_cached: true,
            },
            true,
        );
        assert_eq!(cache.adopted["song"].fetched_at_secs, 3);
        assert!(Arc::ptr_eq(&cache.adopted["song"].parsed, &old.parsed));
        assert!(!cache.pending.contains_key("song"));

        cache.apply(
            "song",
            LyricLoadEvent::Network {
                result: Ok(changed.clone()),
                had_cached: true,
            },
            true,
        );
        assert!(cache.adopt_pending("song"));
        assert_eq!(cache.adopted["song"].parsed, changed.parsed);
        assert!(!cache.adopt_pending("song"));
    }

    #[test]
    fn test_lyric_cache_failure_and_background_result() {
        let mut cache = LyricsCache::new(None);
        cache.loading.insert("song".to_owned());
        cache.apply(
            "song",
            LyricLoadEvent::Network {
                result: Err(anyhow::anyhow!("offline")),
                had_cached: false,
            },
            true,
        );
        assert_eq!(
            cache.errors.get("song").map(String::as_str),
            Some("offline")
        );
        assert!(!cache.loading.contains("song"));

        let old = cached("[00:01.00]old", 1);
        cache.loading.insert("song".to_owned());
        cache.apply(
            "song",
            LyricLoadEvent::Disk {
                lyrics: old.clone(),
                fresh: true,
            },
            false,
        );
        assert!(!cache.loading.contains("song"));
        assert!(!cache.errors.contains_key("song"));
        cache.apply(
            "song",
            LyricLoadEvent::Network {
                result: Err(anyhow::anyhow!("offline")),
                had_cached: true,
            },
            true,
        );
        assert!(Arc::ptr_eq(&cache.adopted["song"].parsed, &old.parsed));
        assert!(!cache.errors.contains_key("song"));

        let changed = cached("[00:02.00]changed", 2);
        cache.apply(
            "song",
            LyricLoadEvent::Network {
                result: Ok(changed.clone()),
                had_cached: true,
            },
            false,
        );
        assert_eq!(cache.adopted["song"].parsed, changed.parsed);
        assert!(!cache.pending.contains_key("song"));
    }
}
