mod protocol;

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use async_channel::Sender;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::MusicClient;
use crate::models::{LoginStatus, LoginToken, Platform, TencentLoginToken};

pub use protocol::{CdnCache, ProtocolClient};

const QR_DATA_PREFIX: &str = "data:image/png;base64,";

#[derive(Clone, Serialize, Deserialize)]
pub struct QqCredential {
    pub music_id: u64,
    pub music_key: String,
    pub refresh_token: String,
    pub refresh_key: String,
    pub login_type: u64,
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub encrypted_uin: String,
    #[serde(default = "new_client_guid")]
    pub client_guid: String,
}

impl QqCredential {
    pub fn from_token(token: TencentLoginToken) -> Result<Self> {
        Ok(Self {
            music_id: token.music_id,
            music_key: token.music_key,
            refresh_token: token.refresh_token,
            refresh_key: token.refresh_key,
            login_type: token.login_type,
            expires_at: token.expires_at,
            encrypted_uin: token.encrypted_uin,
            client_guid: new_client_guid(),
        })
    }

    pub fn to_token(&self) -> TencentLoginToken {
        TencentLoginToken::new(
            self.music_id,
            self.music_key.clone(),
            self.refresh_token.clone(),
            self.refresh_key.clone(),
            self.expires_at,
            self.login_type,
        )
    }

    pub fn cookie(&self) -> String {
        format!(
            "uin={}; qqmusic_uin={}; qqmusic_key={}; qm_keyst={}; tmeLoginType={}",
            self.music_id, self.music_id, self.music_key, self.music_key, self.login_type,
        )
    }

    pub fn is_expiring(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.expires_at
            .is_some_and(|expires_at| expires_at <= now + 300)
    }
}

fn new_client_guid() -> String {
    Uuid::new_v4().simple().to_string().to_ascii_uppercase()
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
    Created { tid: u64, dir_id: u64 },
    Favorite { diss_id: u64 },
    Recommended { diss_id: u64 },
    Artist { mid: String },
    Album { mid: String },
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

#[derive(Clone, Debug)]
pub struct SearchPage<T> {
    pub items: Vec<T>,
    pub has_more: bool,
    pub next_offset: u64,
}

#[derive(Clone, Debug)]
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
    pub album: String,
    pub album_mid: String,
    pub cover_url: Option<String>,
    pub duration_seconds: u64,
    pub added_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
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

    pub fn best_available(available: &[Self], preferred: Self) -> Option<Self> {
        Self::fallback_order(available, preferred)
            .into_iter()
            .next()
    }

    pub fn fallback_order(available: &[Self], preferred: Self) -> Vec<Self> {
        let Some(preferred_rank) = Self::ALL.iter().position(|quality| *quality == preferred)
        else {
            return Vec::new();
        };
        Self::ALL[..=preferred_rank]
            .iter()
            .rev()
            .copied()
            .filter(|quality| available.contains(quality))
            .collect()
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
}

impl PlaybackOption {
    pub fn urls(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.url.as_str()).chain(self.fallback_urls.iter().map(String::as_str))
    }
}

pub enum LoginEvent {
    QrReady(Vec<u8>),
    WaitingScan,
    WaitingConfirm,
    Succeeded(QqCredential),
    Expired,
    Failed(String),
}

pub async fn run_qr_login(events: Sender<LoginEvent>) {
    if let Err(error) = qr_login(&events).await {
        let _ = events.send(LoginEvent::Failed(format!("{error:#}"))).await;
    }
}

async fn qr_login(events: &Sender<LoginEvent>) -> Result<()> {
    let client = MusicClient::new();
    let session = client
        .login()
        .session()
        .platform(Platform::Tencent)
        .send()
        .await
        .context("无法创建 QQ 音乐扫码会话")?;

    let encoded = session
        .qr_code()
        .strip_prefix(QR_DATA_PREFIX)
        .context("QQ 音乐返回了无法识别的二维码")?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("无法解码 QQ 音乐二维码")?;
    let _ = events.send(LoginEvent::QrReady(png)).await;

    loop {
        match session.status().await.context("QQ 音乐扫码状态查询失败")? {
            LoginStatus::WaitingScan => {
                let _ = events.send(LoginEvent::WaitingScan).await;
            }
            LoginStatus::WaitingConfirm => {
                let _ = events.send(LoginEvent::WaitingConfirm).await;
            }
            LoginStatus::QrCodeExpired => {
                let _ = events.send(LoginEvent::Expired).await;
                return Ok(());
            }
            LoginStatus::Success(LoginToken::Tencent(token)) => {
                let credential = QqCredential::from_token(token)?;
                let credential = ProtocolClient::new()?
                    .complete_credential(credential)
                    .await?;
                let _ = events.send(LoginEvent::Succeeded(credential)).await;
                return Ok(());
            }
            LoginStatus::Success(LoginToken::Netease(_)) => {
                bail!("QQ 音乐登录返回了错误的平台凭据");
            }
        }
    }
}

pub async fn refresh_credential(credential: QqCredential) -> Result<QqCredential> {
    if !credential.is_expiring() {
        return ProtocolClient::new()?.complete_credential(credential).await;
    }

    let client = MusicClient::new();
    let refreshed = client
        .login()
        .refresh()
        .platform(Platform::Tencent)
        .token(&credential.to_token())
        .send()
        .await
        .context("QQ 音乐凭据已过期且刷新失败")?;

    let LoginToken::Tencent(token) = refreshed else {
        bail!("QQ 音乐刷新返回了错误的平台凭据");
    };
    let mut refreshed = QqCredential::from_token(token)?;
    if refreshed.encrypted_uin.is_empty() {
        refreshed.encrypted_uin = credential.encrypted_uin;
    }
    refreshed.client_guid = credential.client_guid;
    ProtocolClient::new()?.complete_credential(refreshed).await
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
        let available = [Quality::Standard, Quality::High, Quality::Lossless];
        assert_eq!(
            Quality::best_available(&available, Quality::High),
            Some(Quality::High)
        );
        assert_eq!(
            Quality::best_available(&available, Quality::Master),
            Some(Quality::Lossless)
        );
        assert_eq!(
            Quality::best_available(&[Quality::Standard, Quality::Lossless], Quality::High),
            Some(Quality::Standard)
        );
        assert_eq!(
            Quality::best_available(
                &[Quality::High, Quality::Standard, Quality::Lossless],
                Quality::HiRes
            ),
            Some(Quality::Lossless)
        );
        assert_eq!(Quality::best_available(&[], Quality::High), None);
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
        assert!(
            Quality::fallback_order(&[Quality::High], Quality::Standard).is_empty(),
            "a fallback must never select a higher quality"
        );
    }
}
