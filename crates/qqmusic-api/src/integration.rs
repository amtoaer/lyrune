mod protocol;

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use async_channel::Sender;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::MusicClient;
use crate::models::{LoginStatus, LoginToken, Platform, TencentLoginToken};

pub use protocol::ProtocolClient;

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

#[derive(Clone)]
pub struct Track {
    pub mid: String,
    pub media_mid: String,
    pub title: String,
    pub artists: String,
    pub album: String,
    pub duration_seconds: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Standard,
    High,
    Lossless,
}

impl Quality {
    pub const ALL: [Self; 3] = [Self::Standard, Self::High, Self::Lossless];

    pub fn cache_id(self) -> &'static str {
        match self {
            Self::Standard => "standard-mp3",
            Self::High => "high-mp3",
            Self::Lossless => "lossless-flac",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "标准 128k",
            Self::High => "高品质 320k",
            Self::Lossless => "无损 FLAC",
        }
    }

    pub(crate) fn file_parts(self) -> (&'static str, &'static str) {
        match self {
            Self::Standard => ("M500", ".mp3"),
            Self::High => ("M800", ".mp3"),
            Self::Lossless => ("F000", ".flac"),
        }
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
    }
}
