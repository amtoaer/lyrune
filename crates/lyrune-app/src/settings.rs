use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::design::ColorTheme;
use qqmusic_api::integration::{
    CdnCache, Quality, Track, UserPlaylist, UserPlaylistId, UserProfile,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PersistedPlayback {
    pub account_id: u64,
    pub playlist_id: UserPlaylistId,
    pub track_mid: String,
    pub position_ms: u64,
    #[serde(default)]
    pub queue_tracks: Vec<Track>,
    #[serde(default)]
    pub queue_continuation: Option<PersistedQueueContinuation>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PersistedQueueContinuation {
    Radar { next_page: u64, has_more: bool },
    Guess,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PersistedLibraryView {
    pub account_id: u64,
    pub playlist_id: UserPlaylistId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PersistedWindowSize {
    pub width: u32,
    pub height: u32,
}

impl PersistedPlayback {
    pub fn resume_position(&self, duration_seconds: u64) -> Duration {
        let position = Duration::from_millis(self.position_ms);
        let duration = Duration::from_secs(duration_seconds);
        if duration.is_zero() || position >= duration {
            Duration::ZERO
        } else {
            position
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AppSettings {
    pub volume: f32,
    pub last_nonzero_volume: f32,
    pub color_theme: ColorTheme,
    pub playback_quality: Quality,
    pub last_library_view: Option<PersistedLibraryView>,
    pub current_playback: Option<PersistedPlayback>,
    pub window_size: Option<PersistedWindowSize>,
    pub sidebar_width: Option<u32>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            volume: 1.,
            last_nonzero_volume: 1.,
            color_theme: ColorTheme::default(),
            playback_quality: Quality::default(),
            last_library_view: None,
            current_playback: None,
            window_size: None,
            sidebar_width: None,
        }
    }
}

impl AppSettings {
    fn normalized(mut self) -> Self {
        self.volume = normalized_volume(self.volume, 1.);
        self.last_nonzero_volume = normalized_volume(self.last_nonzero_volume, 1.).max(0.01);
        if self.current_playback.as_ref().is_some_and(|playback| {
            playback.track_mid.trim().is_empty()
                || playback.queue_tracks.is_empty()
                || !playback
                    .queue_tracks
                    .iter()
                    .any(|track| track.mid == playback.track_mid)
        }) {
            self.current_playback = None;
        }
        self
    }
}

pub struct SettingsStore;

impl SettingsStore {
    pub fn load() -> Result<AppSettings> {
        Self::load_from(&settings_path()?)
    }

    pub fn save(settings: &AppSettings) -> Result<()> {
        Self::save_to(&settings_path()?, settings)
    }

    fn load_from(path: &Path) -> Result<AppSettings> {
        let serialized = match fs::read_to_string(path) {
            Ok(serialized) => serialized,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(AppSettings::default()),
            Err(error) => return Err(error).context("无法读取应用设置"),
        };
        serde_json::from_str::<AppSettings>(&serialized)
            .context("应用设置格式无效")
            .map(AppSettings::normalized)
    }

    fn save_to(path: &Path, settings: &AppSettings) -> Result<()> {
        let parent = path.parent().context("应用设置路径缺少父目录")?;
        fs::create_dir_all(parent).context("无法创建应用设置目录")?;
        let serialized = serde_json::to_vec_pretty(&settings.clone().normalized())
            .context("无法序列化应用设置")?;
        fs::write(path, serialized).context("无法保存应用设置")
    }
}

pub struct CdnCacheStore;

impl CdnCacheStore {
    pub fn load() -> Result<CdnCache> {
        Self::load_from(&cdn_cache_path()?)
    }

    pub fn save(cache: &CdnCache) -> Result<()> {
        Self::save_to(&cdn_cache_path()?, cache)
    }

    fn load_from(path: &Path) -> Result<CdnCache> {
        let serialized = match fs::read_to_string(path) {
            Ok(serialized) => serialized,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(CdnCache::default()),
            Err(error) => return Err(error).context("无法读取 CDN 缓存"),
        };
        serde_json::from_str(&serialized).context("CDN 缓存格式无效")
    }

    fn save_to(path: &Path, cache: &CdnCache) -> Result<()> {
        let parent = path.parent().context("CDN 缓存路径缺少父目录")?;
        fs::create_dir_all(parent).context("无法创建 CDN 缓存目录")?;
        let serialized = serde_json::to_vec_pretty(cache).context("无法序列化 CDN 缓存")?;
        fs::write(path, serialized).context("无法保存 CDN 缓存")
    }
}

#[derive(Debug, Default)]
pub struct LibraryCache {
    directories: Vec<CachedLibraryDirectory>,
    playlists: Vec<CachedPlaylistSnapshot>,
}

#[derive(Debug)]
struct CachedLibraryDirectory {
    account_id: u64,
    fetched_at_secs: u64,
    profile: UserProfile,
    playlists: Vec<UserPlaylist>,
}

#[derive(Debug)]
struct CachedPlaylistSnapshot {
    account_id: u64,
    fetched_at_secs: u64,
    revision: u64,
    playlist: UserPlaylist,
    tracks: Vec<Track>,
    has_more: bool,
    next_offset: u64,
}

#[derive(Clone, Debug)]
pub struct PlaylistSnapshot {
    pub revision: u64,
    pub playlist: UserPlaylist,
    pub tracks: Vec<Track>,
    pub has_more: bool,
    pub next_offset: u64,
}

impl LibraryCache {
    pub fn fresh_directory(
        &self,
        account_id: u64,
        now_secs: u64,
        ttl: Duration,
    ) -> Option<(UserProfile, Vec<UserPlaylist>)> {
        self.directories
            .iter()
            .find(|directory| {
                directory.account_id == account_id
                    && is_fresh(directory.fetched_at_secs, now_secs, ttl)
            })
            .map(|directory| (directory.profile.clone(), directory.playlists.clone()))
    }

    pub fn replace_directory(
        &mut self,
        account_id: u64,
        profile: UserProfile,
        playlists: Vec<UserPlaylist>,
        fetched_at_secs: u64,
    ) {
        if let Some(directory) = self
            .directories
            .iter_mut()
            .find(|directory| directory.account_id == account_id)
        {
            *directory = CachedLibraryDirectory {
                account_id,
                fetched_at_secs,
                profile,
                playlists,
            };
        } else {
            self.directories.push(CachedLibraryDirectory {
                account_id,
                fetched_at_secs,
                profile,
                playlists,
            });
        }
    }

    pub fn fresh_playlist(
        &self,
        account_id: u64,
        playlist_id: &UserPlaylistId,
        now_secs: u64,
        ttl: Duration,
    ) -> Option<PlaylistSnapshot> {
        self.playlists
            .iter()
            .find(|snapshot| {
                snapshot.account_id == account_id
                    && snapshot.playlist.id == *playlist_id
                    && (matches!(playlist_id, UserPlaylistId::Search { .. })
                        || is_fresh(snapshot.fetched_at_secs, now_secs, ttl))
            })
            .map(|snapshot| PlaylistSnapshot {
                revision: snapshot.revision,
                playlist: snapshot.playlist.clone(),
                tracks: snapshot.tracks.clone(),
                has_more: snapshot.has_more,
                next_offset: snapshot.next_offset,
            })
    }

    pub fn store_playlist_page(
        &mut self,
        account_id: u64,
        playlist: UserPlaylist,
        tracks: Vec<Track>,
        has_more: bool,
        next_offset: u64,
        offset: u64,
        fetched_at_secs: u64,
        revision: u64,
    ) -> bool {
        if offset == 0 {
            return self.replace_playlist(
                account_id,
                playlist,
                tracks,
                has_more,
                next_offset,
                fetched_at_secs,
                revision,
            );
        }

        let Some(snapshot) = self.playlists.iter_mut().find(|snapshot| {
            snapshot.account_id == account_id
                && snapshot.playlist.id == playlist.id
                && snapshot.next_offset == offset
                && snapshot.revision == revision
        }) else {
            return false;
        };
        snapshot.playlist = playlist;
        snapshot.tracks.extend(tracks);
        snapshot.has_more = has_more;
        snapshot.next_offset = next_offset;
        true
    }

    pub fn replace_playlist(
        &mut self,
        account_id: u64,
        playlist: UserPlaylist,
        tracks: Vec<Track>,
        has_more: bool,
        next_offset: u64,
        fetched_at_secs: u64,
        revision: u64,
    ) -> bool {
        if self.playlists.iter().any(|snapshot| {
            snapshot.account_id == account_id
                && snapshot.playlist.id == playlist.id
                && snapshot.revision > revision
        }) {
            return false;
        }
        let cached = CachedPlaylistSnapshot {
            account_id,
            fetched_at_secs,
            revision,
            playlist,
            tracks,
            has_more,
            next_offset,
        };
        if let Some(snapshot) = self.playlists.iter_mut().find(|snapshot| {
            snapshot.account_id == account_id && snapshot.playlist.id == cached.playlist.id
        }) {
            *snapshot = cached;
        } else {
            self.playlists.push(cached);
        }
        true
    }
}

fn settings_path() -> Result<PathBuf> {
    ProjectDirs::from("dev", "lyrune", "Lyrune")
        .map(|dirs| dirs.config_dir().join("settings.json"))
        .context("无法确定应用设置目录")
}

fn cdn_cache_path() -> Result<PathBuf> {
    ProjectDirs::from("dev", "lyrune", "Lyrune")
        .map(|dirs| dirs.cache_dir().join("cdn.json"))
        .context("无法确定 CDN 缓存目录")
}

fn is_fresh(fetched_at_secs: u64, now_secs: u64, ttl: Duration) -> bool {
    now_secs.saturating_sub(fetched_at_secs) < ttl.as_secs()
}

fn normalized_volume(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0., 1.)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playlist() -> UserPlaylist {
        UserPlaylist {
            id: UserPlaylistId::Created { tid: 42, dir_id: 0 },
            title: "测试歌单".to_owned(),
            cover_url: None,
            description: String::new(),
            owner: "tester".to_owned(),
            owner_avatar_url: None,
            track_count: 2,
        }
    }

    fn track(mid: &str) -> Track {
        Track {
            song_id: None,
            mid: mid.to_owned(),
            media_mid: None,
            standard_size_bytes: None,
            high_size_bytes: None,
            lossless_size_bytes: None,
            hi_res_size_bytes: None,
            atmos_stereo_size_bytes: None,
            atmos_surround_size_bytes: None,
            master_size_bytes: None,
            title: mid.to_owned(),
            artists: String::new(),
            artist_details: Vec::new(),
            album: String::new(),
            album_mid: String::new(),
            cover_url: None,
            duration_seconds: 180,
            added_at: None,
        }
    }

    #[test]
    fn missing_settings_fields_use_defaults() {
        let settings: AppSettings = serde_json::from_str("{}").expect("deserialize defaults");
        assert_eq!(settings.volume, 1.);
        assert_eq!(settings.last_nonzero_volume, 1.);
        assert_eq!(settings.color_theme, ColorTheme::LyruneNeutral);
        assert_eq!(settings.playback_quality, Quality::Standard);
        assert_eq!(settings.last_library_view, None);
        assert_eq!(settings.current_playback, None);
        assert_eq!(settings.window_size, None);
        assert_eq!(settings.sidebar_width, None);
    }

    #[test]
    fn persisted_volumes_are_clamped() {
        let settings = AppSettings {
            volume: 2.,
            last_nonzero_volume: -1.,
            color_theme: ColorTheme::CatppuccinMocha,
            playback_quality: Quality::High,
            last_library_view: None,
            current_playback: None,
            window_size: None,
            sidebar_width: None,
        }
        .normalized();
        assert_eq!(settings.volume, 1.);
        assert_eq!(settings.last_nonzero_volume, 0.01);
    }

    #[test]
    fn settings_round_trip_through_disk() {
        let directory = std::env::temp_dir().join(format!(
            "lyrune-settings-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        let path = directory.join("settings.json");
        let expected = AppSettings {
            volume: 0.37,
            last_nonzero_volume: 0.64,
            color_theme: ColorTheme::EverforestDark,
            playback_quality: Quality::HiRes,
            last_library_view: Some(PersistedLibraryView {
                account_id: 10001,
                playlist_id: UserPlaylistId::Created { tid: 84, dir_id: 0 },
            }),
            current_playback: Some(PersistedPlayback {
                account_id: 10001,
                playlist_id: UserPlaylistId::Recommendation {
                    kind: qqmusic_api::integration::RecommendationKind::Radar,
                },
                track_mid: "restored-track".to_owned(),
                position_ms: 92_345,
                queue_tracks: vec![track("restored-track")],
                queue_continuation: Some(PersistedQueueContinuation::Radar {
                    next_page: 4,
                    has_more: true,
                }),
            }),
            window_size: Some(PersistedWindowSize {
                width: 1440,
                height: 900,
            }),
            sidebar_width: Some(296),
        };

        SettingsStore::save_to(&path, &expected).expect("save settings");
        let restored = SettingsStore::load_from(&path).expect("load settings");

        assert_eq!(restored.volume, expected.volume);
        assert_eq!(restored.last_nonzero_volume, expected.last_nonzero_volume);
        assert_eq!(restored.color_theme, expected.color_theme);
        assert_eq!(restored.playback_quality, expected.playback_quality);
        assert_eq!(restored.last_library_view, expected.last_library_view);
        assert_eq!(restored.current_playback, expected.current_playback);
        assert_eq!(restored.window_size, expected.window_size);
        assert_eq!(restored.sidebar_width, expected.sidebar_width);
        fs::remove_dir_all(directory).expect("remove test settings directory");
    }

    #[test]
    fn playback_resume_position_preserves_progress_but_not_eof() {
        let mut playback = PersistedPlayback {
            account_id: 10001,
            playlist_id: UserPlaylistId::Liked,
            track_mid: "track-mid".to_owned(),
            position_ms: 92_345,
            queue_tracks: vec![track("track-mid")],
            queue_continuation: None,
        };
        assert_eq!(playback.resume_position(180), Duration::from_millis(92_345));

        playback.position_ms = 180_000;
        assert_eq!(playback.resume_position(180), Duration::ZERO);
        playback.position_ms = 200_000;
        assert_eq!(playback.resume_position(180), Duration::ZERO);
    }

    #[test]
    fn playback_from_before_queue_persistence_is_discarded() {
        let settings = serde_json::from_value::<AppSettings>(serde_json::json!({
            "current_playback": {
                "account_id": 10001,
                "playlist_id": UserPlaylistId::Liked,
                "track_mid": "track-mid",
                "position_ms": 1234
            }
        }))
        .expect("deserialize old playback settings")
        .normalized();

        assert_eq!(settings.current_playback, None);
    }

    #[test]
    fn cdn_cache_round_trips_through_disk() {
        let directory = std::env::temp_dir().join(format!(
            "lyrune-cdn-cache-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        let path = directory.join("cdn.json");
        let expected = CdnCache::default();

        CdnCacheStore::save_to(&path, &expected).expect("save CDN cache");
        let restored = CdnCacheStore::load_from(&path).expect("load CDN cache");

        assert_eq!(restored, expected);
        fs::remove_dir_all(directory).expect("remove test CDN cache directory");
    }

    #[test]
    fn library_cache_uses_ttl_and_only_appends_contiguous_pages() {
        let mut cache = LibraryCache::default();
        let profile = UserProfile {
            id: "10001".to_owned(),
            nickname: "tester".to_owned(),
            avatar_url: None,
        };
        let playlist = playlist();
        cache.replace_directory(10001, profile, vec![playlist.clone()], 100);
        assert!(
            cache
                .fresh_directory(10001, 399, Duration::from_secs(300))
                .is_some()
        );
        assert!(
            cache
                .fresh_directory(10001, 400, Duration::from_secs(300))
                .is_none()
        );

        assert!(cache.store_playlist_page(
            10001,
            playlist.clone(),
            vec![track("first")],
            true,
            1,
            0,
            100,
            1,
        ));
        assert!(!cache.store_playlist_page(
            10001,
            playlist.clone(),
            vec![track("skipped")],
            false,
            3,
            2,
            100,
            1,
        ));
        assert!(cache.store_playlist_page(
            10001,
            playlist.clone(),
            vec![track("second")],
            false,
            2,
            1,
            100,
            1,
        ));

        let snapshot = cache
            .fresh_playlist(10001, &playlist.id, 399, Duration::from_secs(300))
            .expect("fresh playlist snapshot");
        assert_eq!(
            snapshot
                .tracks
                .iter()
                .map(|track| track.mid.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(!snapshot.has_more);
        assert_eq!(snapshot.next_offset, 2);
        assert!(
            cache
                .fresh_playlist(10001, &playlist.id, 400, Duration::from_secs(300))
                .is_none()
        );

        assert!(cache.store_playlist_page(
            10001,
            playlist.clone(),
            vec![track("refreshed")],
            false,
            1,
            0,
            400,
            2,
        ));
        let refreshed = cache
            .fresh_playlist(10001, &playlist.id, 400, Duration::from_secs(300))
            .expect("refreshed playlist snapshot");
        assert_eq!(refreshed.tracks.len(), 1);
        assert_eq!(refreshed.tracks[0].mid, "refreshed");
        assert!(!cache.replace_playlist(
            10001,
            playlist.clone(),
            vec![track("stale")],
            false,
            1,
            401,
            1,
        ));
        assert_eq!(
            cache
                .fresh_playlist(10001, &playlist.id, 401, Duration::from_secs(300))
                .expect("newer snapshot remains")
                .tracks[0]
                .mid,
            "refreshed"
        );
    }

    #[test]
    fn search_queue_snapshot_remains_available_within_session() {
        let mut cache = LibraryCache::default();
        let playlist = UserPlaylist {
            id: UserPlaylistId::Search {
                query: "search query".to_owned(),
            },
            title: "Search results".to_owned(),
            cover_url: None,
            description: String::new(),
            owner: String::new(),
            owner_avatar_url: None,
            track_count: 1,
        };
        assert!(cache.replace_playlist(
            10001,
            playlist.clone(),
            vec![track("search-track")],
            false,
            1,
            100,
            1,
        ));
        let snapshot = cache
            .fresh_playlist(10001, &playlist.id, 10_000, Duration::from_secs(300))
            .expect("search queue snapshot");
        assert_eq!(snapshot.tracks[0].mid, "search-track");
    }
}
