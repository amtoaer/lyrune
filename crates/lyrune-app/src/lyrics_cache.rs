use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use directories::ProjectDirs;
use qqmusic_api::models::LyricResult;
use serde::{Deserialize, Serialize};

use crate::cache::cache_key;

const CACHE_SCHEMA_VERSION: u32 = 1;
const CACHE_DIRECTORY: &str = "lyrics-v1";

#[derive(Debug)]
pub struct CachedLyrics {
    pub fetched_at_secs: u64,
    pub lyrics: LyricResult,
}

impl CachedLyrics {
    pub fn is_fresh(&self, now_secs: u64, ttl: Duration) -> bool {
        now_secs.saturating_sub(self.fetched_at_secs) < ttl.as_secs()
    }
}

#[derive(Clone)]
pub struct LyricDiskCache {
    root: Arc<PathBuf>,
}

impl LyricDiskCache {
    pub fn new() -> Result<Self> {
        let project_dirs =
            ProjectDirs::from("dev", "lyrune", "Lyrune").context("无法确定 Lyrune 歌词缓存目录")?;
        Ok(Self::with_root(
            project_dirs.cache_dir().join(CACHE_DIRECTORY),
        ))
    }

    fn with_root(root: PathBuf) -> Self {
        Self {
            root: Arc::new(root),
        }
    }

    pub async fn load(&self, mid: &str) -> Result<Option<CachedLyrics>> {
        let path = self.path(mid);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("无法读取歌词缓存"),
        };
        let stored = serde_json::from_slice::<StoredLyrics>(&bytes).context("歌词缓存格式无效")?;
        if stored.schema_version != CACHE_SCHEMA_VERSION || stored.lyrics.id != mid {
            return Ok(None);
        }
        Ok(Some(CachedLyrics {
            fetched_at_secs: stored.fetched_at_secs,
            lyrics: stored.lyrics,
        }))
    }

    pub async fn save(&self, fetched_at_secs: u64, lyrics: &LyricResult) -> Result<()> {
        tokio::fs::create_dir_all(self.root.as_ref())
            .await
            .context("无法创建歌词缓存目录")?;
        let stored = StoredLyrics {
            schema_version: CACHE_SCHEMA_VERSION,
            fetched_at_secs,
            lyrics: lyrics.clone(),
        };
        let bytes = serde_json::to_vec(&stored).context("无法序列化歌词缓存")?;
        let path = self.path(&lyrics.id);
        let temporary = self.root.join(format!(
            ".{}.{}.tmp",
            cache_key(lyrics.id.as_bytes()),
            std::process::id()
        ));
        tokio::fs::write(&temporary, bytes)
            .await
            .context("无法写入歌词缓存临时文件")?;
        if let Err(error) = tokio::fs::rename(&temporary, &path).await {
            if matches!(
                error.kind(),
                ErrorKind::AlreadyExists | ErrorKind::PermissionDenied
            ) && tokio::fs::remove_file(&path).await.is_ok()
            {
                if let Err(error) = tokio::fs::rename(&temporary, &path).await {
                    let _ = tokio::fs::remove_file(&temporary).await;
                    return Err(error).context("无法替换歌词缓存文件");
                }
            } else {
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(error).context("无法提交歌词缓存文件");
            }
        }
        Ok(())
    }

    fn path(&self, mid: &str) -> PathBuf {
        self.root
            .join(format!("{}.json", cache_key(mid.as_bytes())))
    }
}

#[derive(Deserialize, Serialize)]
struct StoredLyrics {
    schema_version: u32,
    fetched_at_secs: u64,
    lyrics: LyricResult,
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn test_cache() -> LyricDiskCache {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        LyricDiskCache::with_root(std::env::temp_dir().join(format!(
            "lyrune-lyrics-cache-test-{}-{nonce}",
            std::process::id()
        )))
    }

    fn lyrics(text: &str) -> LyricResult {
        LyricResult {
            id: "song-mid".to_owned(),
            lyric: text.to_owned(),
            trans_lyric: Some("翻译".to_owned()),
            roma_lyric: Some("romanization".to_owned()),
        }
    }

    #[tokio::test]
    async fn raw_lyrics_round_trip_and_replace_atomically() {
        let cache = test_cache();
        cache.save(100, &lyrics("old")).await.unwrap();
        cache.save(200, &lyrics("new")).await.unwrap();

        let cached = cache.load("song-mid").await.unwrap().unwrap();
        assert_eq!(cached.fetched_at_secs, 200);
        assert_eq!(cached.lyrics.lyric, "new");
        assert_eq!(cached.lyrics.trans_lyric.as_deref(), Some("翻译"));
        assert_eq!(cached.lyrics.roma_lyric.as_deref(), Some("romanization"));
        tokio::fs::remove_dir_all(cache.root.as_ref())
            .await
            .unwrap();
    }

    #[test]
    fn freshness_keeps_stale_lyrics_available() {
        let cached = CachedLyrics {
            fetched_at_secs: 100,
            lyrics: lyrics("cached"),
        };
        let ttl = Duration::from_secs(30);
        assert!(cached.is_fresh(129, ttl));
        assert!(!cached.is_fresh(130, ttl));
        assert_eq!(cached.lyrics.lyric, "cached");
    }
}
