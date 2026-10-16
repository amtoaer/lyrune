use serde::{Deserialize, Serialize};

/// 歌词结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LyricResult {
    /// 歌曲 ID。
    pub id: String,
    /// 原文歌词。
    pub lyric: String,
    /// 翻译歌词（若平台提供；缺失时为 [`None`]）。
    pub trans_lyric: Option<String>,
    /// 罗马音歌词（若平台提供；缺失时为 [`None`]）。
    #[serde(default)]
    pub roma_lyric: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UserProfile {
    pub id: String,
    pub nickname: String,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Hash, Serialize)]
pub enum UserPlaylistId {
    Liked,
    Daily,
    Created { tid: u64, dir_id: u64 },
    Favorite { diss_id: u64 },
    Recommended { diss_id: u64 },
    Artist { mid: String },
    Album { mid: String },
    Chart { top_id: u64, period: String },
    Search { query: String },
    Recommendation { kind: RecommendationKind },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash, Serialize)]
pub enum RecommendationKind {
    Radar,
    Guess,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UserPlaylist {
    pub id: UserPlaylistId,
    pub title: String,
    pub cover_url: Option<String>,
    pub description: String,
    pub owner: String,
    #[serde(default)]
    pub owner_avatar_url: Option<String>,
    pub track_count: u64,
}

#[derive(Clone, Debug)]
pub struct RadarTrackPage {
    pub tracks: Vec<Track>,
    pub has_more: bool,
    pub next_page: u64,
}

impl UserPlaylist {
    pub fn liked() -> Self {
        Self {
            id: UserPlaylistId::Liked,
            title: "已点赞的歌曲".to_owned(),
            cover_url: None,
            description: "QQ 音乐中已收藏的歌曲".to_owned(),
            owner: String::new(),
            owner_avatar_url: None,
            track_count: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlaylistPage {
    pub playlist: UserPlaylist,
    pub tracks: Vec<Track>,
    pub total: u64,
    pub has_more: bool,
    pub next_offset: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartGroup {
    pub group_id: u64,
    pub group_name: String,
    pub toplist: Vec<Chart>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chart {
    pub top_id: u64,
    pub title: String,
    pub period: String,
    pub front_pic_url: String,
    pub update_tips: String,
    pub total_num: u64,
    pub song: Vec<ChartPreviewTrack>,
    pub history: ChartHistory,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartPreviewTrack {
    pub title: String,
    pub singer_name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartHistory {
    pub year: Vec<u32>,
    pub sub_period: Vec<Vec<u32>>,
}

impl Chart {
    pub fn playlist(&self, period: String) -> UserPlaylist {
        UserPlaylist {
            id: UserPlaylistId::Chart {
                top_id: self.top_id,
                period,
            },
            title: self.title.clone(),
            cover_url: Some(self.front_pic_url.clone()),
            description: self.update_tips.clone(),
            owner: "QQ 音乐".to_owned(),
            owner_avatar_url: None,
            track_count: self.total_num,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchPage<T> {
    pub items: Vec<T>,
    pub has_more: bool,
    pub next_offset: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchArtist {
    pub mid: String,
    pub name: String,
    pub cover_url: Option<String>,
}

impl SearchArtist {
    pub fn into_playlist(self) -> UserPlaylist {
        UserPlaylist {
            id: UserPlaylistId::Artist { mid: self.mid },
            title: self.name,
            cover_url: self.cover_url,
            description: String::new(),
            owner: "歌手".to_owned(),
            owner_avatar_url: None,
            track_count: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchAlbum {
    pub mid: String,
    pub title: String,
    pub cover_url: Option<String>,
    pub artist: String,
}

impl SearchAlbum {
    pub fn into_playlist(self) -> UserPlaylist {
        UserPlaylist {
            id: UserPlaylistId::Album { mid: self.mid },
            title: self.title,
            cover_url: self.cover_url,
            description: String::new(),
            owner: self.artist,
            owner_avatar_url: None,
            track_count: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchResults {
    pub songs: SearchPage<Track>,
    pub artists: SearchPage<SearchArtist>,
    pub albums: SearchPage<SearchAlbum>,
    pub playlists: SearchPage<UserPlaylist>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Track {
    pub song_id: Option<u64>,
    #[serde(default)]
    pub song_type: u64,
    pub mid: String,
    pub media_mid: Option<String>,
    pub standard_size_bytes: Option<u64>,
    pub high_size_bytes: Option<u64>,
    pub lossless_size_bytes: Option<u64>,
    pub hi_res_size_bytes: Option<u64>,
    pub atmos_stereo_size_bytes: Option<u64>,
    pub atmos_surround_size_bytes: Option<u64>,
    pub master_size_bytes: Option<u64>,
    pub title: String,
    pub artists: String,
    #[serde(default)]
    pub artist_details: Vec<SearchArtist>,
    pub album: String,
    pub album_mid: String,
    pub cover_url: Option<String>,
    pub duration_seconds: u64,
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Quality {
    #[default]
    Standard,
    High,
    Lossless,
    HiRes,
    AtmosStereo,
    AtmosSurround,
    Master,
}

impl Quality {
    pub const ALL: [Self; 7] = [
        Self::Standard,
        Self::High,
        Self::Lossless,
        Self::HiRes,
        Self::AtmosStereo,
        Self::AtmosSurround,
        Self::Master,
    ];

    pub fn cache_id(self) -> &'static str {
        match self {
            Self::Standard => "standard-mp3",
            Self::High => "high-mp3",
            Self::Lossless => "lossless-flac",
            Self::HiRes => "hi-res-flac",
            Self::AtmosStereo => "atmos-stereo-flac",
            Self::AtmosSurround => "atmos-surround-flac",
            Self::Master => "master-flac",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "标准品质",
            Self::High => "HQ 高品质",
            Self::Lossless => "SQ 无损品质",
            Self::HiRes => "Hi-Res 无损品质",
            Self::AtmosStereo => "臻品音质",
            Self::AtmosSurround => "臻品全景声",
            Self::Master => "臻品母带",
        }
    }

    pub fn badge_label(self) -> &'static str {
        match self {
            Self::Standard => "标准",
            Self::High => "HQ",
            Self::Lossless => "SQ",
            Self::HiRes => "Hi-Res",
            Self::AtmosStereo => "臻品音质",
            Self::AtmosSurround => "臻品全景声",
            Self::Master => "臻品母带",
        }
    }

    pub fn fallback_order(available: &[Self], preferred: Self) -> Vec<Self> {
        let lower_end = available.partition_point(|quality| *quality <= preferred);
        if lower_end > 0 {
            return available[..lower_end].iter().rev().copied().collect();
        }
        available.iter().copied().next().into_iter().collect()
    }

    pub(crate) fn file_parts(self) -> (&'static str, &'static str) {
        match self {
            Self::Standard => ("M500", ".mp3"),
            Self::High => ("M800", ".mp3"),
            Self::Lossless => ("F000", ".flac"),
            Self::HiRes => ("RS01", ".flac"),
            Self::AtmosStereo => ("Q000", ".flac"),
            Self::AtmosSurround => ("Q001", ".flac"),
            Self::Master => ("AI00", ".flac"),
        }
    }
}

impl Track {
    pub(crate) fn metadata_allows_quality(&self, quality: Quality) -> bool {
        let size = match quality {
            Quality::Standard => self.standard_size_bytes,
            Quality::High => self.high_size_bytes,
            Quality::Lossless => self.lossless_size_bytes,
            Quality::HiRes => self.hi_res_size_bytes,
            Quality::AtmosStereo => self.atmos_stereo_size_bytes,
            Quality::AtmosSurround => self.atmos_surround_size_bytes,
            Quality::Master => self.master_size_bytes,
        };
        size != Some(0)
    }
}

#[derive(Clone, Debug)]
pub struct PlaybackOption {
    pub quality: Quality,
    pub url: String,
    pub fallback_urls: Vec<String>,
    pub encrypted: bool,
    pub ekey: Option<String>,
}

impl PlaybackOption {
    pub fn urls(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.url.as_str()).chain(self.fallback_urls.iter().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use super::Quality;

    #[test]
    fn quality_maps_to_expected_qq_file_type() {
        assert_eq!(Quality::Standard.file_parts(), ("M500", ".mp3"));
        assert_eq!(Quality::High.file_parts(), ("M800", ".mp3"));
        assert_eq!(Quality::Lossless.file_parts(), ("F000", ".flac"));
        assert_eq!(Quality::HiRes.file_parts(), ("RS01", ".flac"));
        assert_eq!(Quality::AtmosStereo.file_parts(), ("Q000", ".flac"));
        assert_eq!(Quality::AtmosSurround.file_parts(), ("Q001", ".flac"));
        assert_eq!(Quality::Master.file_parts(), ("AI00", ".flac"));
    }

    #[test]
    fn quality_uses_distinct_menu_and_player_labels() {
        let expected = [
            (Quality::Standard, "标准品质", "标准"),
            (Quality::High, "HQ 高品质", "HQ"),
            (Quality::Lossless, "SQ 无损品质", "SQ"),
            (Quality::HiRes, "Hi-Res 无损品质", "Hi-Res"),
            (Quality::AtmosStereo, "臻品音质", "臻品音质"),
            (Quality::AtmosSurround, "臻品全景声", "臻品全景声"),
            (Quality::Master, "臻品母带", "臻品母带"),
        ];

        for (quality, menu_label, player_label) in expected {
            assert_eq!(quality.label(), menu_label);
            assert_eq!(quality.badge_label(), player_label);
        }
    }

    #[test]
    fn quality_fallback_prefers_the_closest_lower_tier() {
        assert!(Quality::ALL.is_sorted());
        let available = [Quality::Standard, Quality::High, Quality::Lossless];
        assert_eq!(
            Quality::fallback_order(&available, Quality::High)
                .into_iter()
                .next(),
            Some(Quality::High)
        );
        assert_eq!(
            Quality::fallback_order(&available, Quality::Master)
                .into_iter()
                .next(),
            Some(Quality::Lossless)
        );
        assert_eq!(
            Quality::fallback_order(&[Quality::Standard, Quality::Lossless], Quality::High)
                .into_iter()
                .next(),
            Some(Quality::Standard)
        );
        assert_eq!(
            Quality::fallback_order(
                &[Quality::High, Quality::Standard, Quality::Lossless],
                Quality::HiRes
            )
            .into_iter()
            .next(),
            Some(Quality::Lossless)
        );
        assert_eq!(
            Quality::fallback_order(&[], Quality::High)
                .into_iter()
                .next(),
            None
        );
    }

    #[test]
    fn quality_fallback_order_keeps_trying_lower_available_tiers() {
        let available = [
            Quality::Standard,
            Quality::High,
            Quality::Lossless,
            Quality::HiRes,
            Quality::AtmosStereo,
            Quality::AtmosSurround,
        ];
        assert_eq!(
            Quality::fallback_order(&available, Quality::AtmosSurround),
            vec![
                Quality::AtmosSurround,
                Quality::AtmosStereo,
                Quality::HiRes,
                Quality::Lossless,
                Quality::High,
                Quality::Standard,
            ]
        );
        assert_eq!(
            Quality::fallback_order(&[Quality::High, Quality::Lossless], Quality::Standard),
            vec![Quality::High],
            "when no lower quality exists, try the lowest available quality"
        );
    }
}
