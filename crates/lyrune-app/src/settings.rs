use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::design::ColorTheme;
use qqmusic_api::integration::{
    CdnCache, Quality, Track, UserPlaylist, UserPlaylistId, UserProfile,
};

pub const DEFAULT_AUDIO_CACHE_LIMIT_GB: u64 = 10;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PersistedPlayback {
    pub account_id: u64,
    pub playlist_id: UserPlaylistId,
    pub track_mid: String,
    pub position_ms: u64,
    #[serde(default)]
    pub queue_tracks: Vec<Arc<Track>>,
    #[serde(default)]
    pub queue_modified: bool,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LyricFrameRate {
    Fps30,
    Fps60,
    Fps90,
    Fps120,
    Display,
}

impl LyricFrameRate {
    pub const ALL: [Self; 5] = [
        Self::Fps30,
        Self::Fps60,
        Self::Fps90,
        Self::Fps120,
        Self::Display,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Fps30 => "lyrics-fps-30",
            Self::Fps60 => "lyrics-fps-60",
            Self::Fps90 => "lyrics-fps-90",
            Self::Fps120 => "lyrics-fps-120",
            Self::Display => "lyrics-fps-display",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Fps30 => "30",
            Self::Fps60 => "60",
            Self::Fps90 => "90",
            Self::Fps120 => "120",
            Self::Display => "默认",
        }
    }

    pub const fn frame_interval(self) -> Option<Duration> {
        match self {
            Self::Fps30 => Some(Duration::from_nanos(1_000_000_000 / 30)),
            Self::Fps60 => Some(Duration::from_nanos(1_000_000_000 / 60)),
            Self::Fps90 => Some(Duration::from_nanos(1_000_000_000 / 90)),
            Self::Fps120 => Some(Duration::from_nanos(1_000_000_000 / 120)),
            Self::Display => None,
        }
    }
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
    pub ui_font_families: Vec<String>,
    pub monospace_font_families: Vec<String>,
    pub lyric_font_families: Vec<String>,
    pub audio_cache_limit_gb: u64,
    pub playback_quality: Quality,
    #[serde(alias = "lyric_frame_rate")]
    pub lyric_highlight_frame_rate: LyricFrameRate,
    pub lyric_scroll_frame_rate: LyricFrameRate,
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
            ui_font_families: default_ui_font_families(),
            monospace_font_families: default_monospace_font_families(),
            lyric_font_families: default_lyric_font_families(),
            audio_cache_limit_gb: DEFAULT_AUDIO_CACHE_LIMIT_GB,
            playback_quality: Quality::default(),
            lyric_highlight_frame_rate: LyricFrameRate::Fps30,
            lyric_scroll_frame_rate: LyricFrameRate::Display,
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
        self.ui_font_families =
            normalize_font_families(self.ui_font_families, default_ui_font_families());
        self.monospace_font_families = normalize_font_families(
            self.monospace_font_families,
            default_monospace_font_families(),
        );
        self.lyric_font_families =
            normalize_font_families(self.lyric_font_families, default_lyric_font_families());
        self.audio_cache_limit_gb = self.audio_cache_limit_gb.max(1);
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

pub(crate) fn default_ui_font_families() -> Vec<String> {
    vec![".SystemUIFont".to_owned()]
}

pub(crate) fn default_monospace_font_families() -> Vec<String> {
    let family = if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    };
    vec![family.to_owned()]
}

pub(crate) fn default_lyric_font_families() -> Vec<String> {
    vec![".SystemUIFont".to_owned()]
}

pub(crate) fn parse_font_families(value: &str) -> Vec<String> {
    normalize_font_families(
        value
            .split([',', '，'])
            .map(str::trim)
            .filter(|family| !family.is_empty())
            .map(str::to_owned)
            .collect(),
        Vec::new(),
    )
}

fn normalize_font_families(families: Vec<String>, defaults: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::with_capacity(families.len());
    for family in families {
        let family = family.trim();
        if !family.is_empty()
            && !normalized
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(family))
        {
            normalized.push(family.to_owned());
        }
    }
    if normalized.is_empty() {
        defaults
    } else {
        normalized
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
}

#[derive(Debug)]
struct CachedLibraryDirectory {
    account_id: u64,
    fetched_at_secs: u64,
    profile: UserProfile,
    playlists: Vec<UserPlaylist>,
}

impl LibraryCache {
    pub fn update_liked_track_count(&mut self, account_id: u64, liked: bool) {
        if let Some(directory) = self
            .directories
            .iter_mut()
            .find(|directory| directory.account_id == account_id)
            && let Some(playlist) = directory
                .playlists
                .iter_mut()
                .find(|playlist| playlist.id == UserPlaylistId::Liked)
        {
            playlist.track_count = if liked {
                playlist.track_count.saturating_add(1)
            } else {
                playlist.track_count.saturating_sub(1)
            };
        }
    }

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

    fn track(mid: &str) -> Track {
        Track {
            song_id: None,
            song_type: 0,
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
        }
    }

    #[test]
    fn missing_settings_fields_use_defaults() {
        let settings: AppSettings = serde_json::from_str("{}").expect("deserialize defaults");
        assert_eq!(settings.volume, 1.);
        assert_eq!(settings.last_nonzero_volume, 1.);
        assert_eq!(settings.color_theme, ColorTheme::EverforestLight);
        assert_eq!(settings.ui_font_families, [".SystemUIFont"]);
        assert_eq!(
            settings.monospace_font_families,
            default_monospace_font_families()
        );
        assert_eq!(settings.lyric_font_families, [".SystemUIFont"]);
        assert_eq!(settings.audio_cache_limit_gb, DEFAULT_AUDIO_CACHE_LIMIT_GB);
        assert_eq!(settings.playback_quality, Quality::Standard);
        assert_eq!(settings.lyric_highlight_frame_rate, LyricFrameRate::Fps30);
        assert_eq!(settings.lyric_scroll_frame_rate, LyricFrameRate::Display);
        assert_eq!(settings.last_library_view, None);
        assert_eq!(settings.current_playback, None);
        assert_eq!(settings.window_size, None);
        assert_eq!(settings.sidebar_width, None);
    }

    #[test]
    fn font_family_input_is_trimmed_and_deduplicated_in_order() {
        assert_eq!(
            parse_font_families(" Inter, Noto Sans CJK SC，inter, , Noto Color Emoji "),
            ["Inter", "Noto Sans CJK SC", "Noto Color Emoji"]
        );
    }

    #[test]
    fn persisted_volumes_are_clamped() {
        let settings = AppSettings {
            volume: 2.,
            last_nonzero_volume: -1.,
            color_theme: ColorTheme::CatppuccinMocha,
            ui_font_families: default_ui_font_families(),
            monospace_font_families: default_monospace_font_families(),
            lyric_font_families: default_lyric_font_families(),
            audio_cache_limit_gb: 0,
            playback_quality: Quality::High,
            lyric_highlight_frame_rate: LyricFrameRate::Fps30,
            lyric_scroll_frame_rate: LyricFrameRate::Display,
            last_library_view: None,
            current_playback: None,
            window_size: None,
            sidebar_width: None,
        }
        .normalized();
        assert_eq!(settings.volume, 1.);
        assert_eq!(settings.last_nonzero_volume, 0.01);
        assert_eq!(settings.audio_cache_limit_gb, 1);
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
            ui_font_families: vec!["Inter".to_owned(), "Noto Sans CJK SC".to_owned()],
            monospace_font_families: vec!["JetBrains Mono".to_owned()],
            lyric_font_families: vec!["LXGW WenKai".to_owned(), "Noto Sans JP".to_owned()],
            audio_cache_limit_gb: 24,
            playback_quality: Quality::HiRes,
            lyric_highlight_frame_rate: LyricFrameRate::Fps120,
            lyric_scroll_frame_rate: LyricFrameRate::Display,
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
                queue_tracks: vec![Arc::new(track("restored-track"))],
                queue_modified: true,
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
        assert_eq!(restored.ui_font_families, expected.ui_font_families);
        assert_eq!(
            restored.monospace_font_families,
            expected.monospace_font_families
        );
        assert_eq!(restored.lyric_font_families, expected.lyric_font_families);
        assert_eq!(restored.audio_cache_limit_gb, expected.audio_cache_limit_gb);
        assert_eq!(restored.playback_quality, expected.playback_quality);
        assert_eq!(
            restored.lyric_highlight_frame_rate,
            expected.lyric_highlight_frame_rate
        );
        assert_eq!(
            restored.lyric_scroll_frame_rate,
            expected.lyric_scroll_frame_rate
        );
        assert_eq!(restored.last_library_view, expected.last_library_view);
        assert_eq!(restored.current_playback, expected.current_playback);
        assert_eq!(restored.window_size, expected.window_size);
        assert_eq!(restored.sidebar_width, expected.sidebar_width);
        fs::remove_dir_all(directory).expect("remove test settings directory");
    }

    #[test]
    fn lyric_frame_rates_map_to_expected_intervals() {
        assert_eq!(
            LyricFrameRate::Fps30.frame_interval(),
            Some(Duration::from_nanos(1_000_000_000 / 30))
        );
        assert_eq!(
            LyricFrameRate::Fps60.frame_interval(),
            Some(Duration::from_nanos(1_000_000_000 / 60))
        );
        assert_eq!(
            LyricFrameRate::Fps90.frame_interval(),
            Some(Duration::from_nanos(1_000_000_000 / 90))
        );
        assert_eq!(
            LyricFrameRate::Fps120.frame_interval(),
            Some(Duration::from_nanos(1_000_000_000 / 120))
        );
        assert_eq!(LyricFrameRate::Display.frame_interval(), None);
    }

    #[test]
    fn legacy_lyric_frame_rate_becomes_the_highlight_rate() {
        let settings = serde_json::from_value::<AppSettings>(serde_json::json!({
            "lyric_frame_rate": "fps120"
        }))
        .expect("deserialize legacy lyric frame rate");

        assert_eq!(settings.lyric_highlight_frame_rate, LyricFrameRate::Fps120);
        assert_eq!(settings.lyric_scroll_frame_rate, LyricFrameRate::Display);
    }

    #[test]
    fn playback_resume_position_preserves_progress_but_not_eof() {
        let mut playback = PersistedPlayback {
            account_id: 10001,
            playlist_id: UserPlaylistId::Liked,
            track_mid: "track-mid".to_owned(),
            position_ms: 92_345,
            queue_tracks: vec![Arc::new(track("track-mid"))],
            queue_modified: false,
            queue_continuation: None,
        };
        assert_eq!(playback.resume_position(180), Duration::from_millis(92_345));

        playback.position_ms = 180_000;
        assert_eq!(playback.resume_position(180), Duration::ZERO);
        playback.position_ms = 200_000;
        assert_eq!(playback.resume_position(180), Duration::ZERO);
    }

    #[test]
    fn persisted_playback_defaults_old_queues_to_unmodified() {
        let playback: PersistedPlayback = serde_json::from_value(serde_json::json!({
            "account_id": 10001,
            "playlist_id": UserPlaylistId::Liked,
            "track_mid": "track-mid",
            "position_ms": 1234,
            "queue_tracks": [track("track-mid")]
        }))
        .expect("deserialize playback without queue_modified");

        assert!(!playback.queue_modified);
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
    fn library_directory_uses_ttl_and_tracks_liked_count_changes() {
        let mut cache = LibraryCache::default();
        let profile = UserProfile {
            id: "10001".to_owned(),
            nickname: "tester".to_owned(),
            avatar_url: None,
        };
        let mut liked = UserPlaylist::liked();
        liked.track_count = 2;
        cache.replace_directory(10001, profile, vec![liked], 100);
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

        cache.update_liked_track_count(10001, true);
        assert_eq!(cache.directories[0].playlists[0].track_count, 3);
        cache.update_liked_track_count(10001, false);
        assert_eq!(cache.directories[0].playlists[0].track_count, 2);
    }
}
