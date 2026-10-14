use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use arc_swap::ArcSwapOption;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc, watch};
use uuid::Uuid;

use crate::error::LoginError;
use crate::login::{LoginClient, LoginStatus, LoginToken, MqttLoginSession};

use crate::client::ProtocolClient;

const QR_DATA_PREFIX: &str = "data:image/png;base64,";

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("QQ 音乐登录凭据已失效（错误码 {code}），请重新登录")]
    Rejected { code: u64 },
    #[error("QQ 音乐登录凭据已注销")]
    Revoked,
}

pub(crate) fn is_credential_rejected(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<CredentialError>(),
            Some(CredentialError::Rejected { .. })
        ) || matches!(
            cause.downcast_ref::<LoginError>(),
            Some(LoginError::TencentLoginServerError { .. })
        )
    })
}

#[derive(Clone)]
pub struct CredentialSession {
    inner: Arc<CredentialSessionInner>,
}

struct CredentialSessionInner {
    current: ArcSwapOption<QqCredential>,
    refresh_gate: Mutex<()>,
    updates: watch::Sender<u64>,
}

impl CredentialSession {
    pub fn new(credential: QqCredential) -> Self {
        let (updates, _) = watch::channel(0);
        Self {
            inner: Arc::new(CredentialSessionInner {
                current: ArcSwapOption::from(Some(Arc::new(credential))),
                refresh_gate: Mutex::new(()),
                updates,
            }),
        }
    }

    pub fn snapshot(&self) -> Option<Arc<QqCredential>> {
        self.inner.current.load_full()
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.updates.subscribe()
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn revoke(&self) -> Option<Arc<QqCredential>> {
        let credential = self.inner.current.swap(None);
        if credential.is_some() {
            self.notify_update();
        }
        credential
    }

    pub(crate) fn revoke_if_rejected(&self, error: &anyhow::Error) {
        if is_credential_rejected(error) {
            self.revoke();
        }
    }

    pub async fn ensure_fresh(&self) -> Result<Arc<QqCredential>> {
        let credential = self.snapshot().ok_or(CredentialError::Revoked)?;
        if !credential.is_expiring() {
            return Ok(credential);
        }

        let _refresh = self.inner.refresh_gate.lock().await;
        // 等锁期间其他请求可能已刷新或注销会话，需要重新读取并判断。
        let credential = self.snapshot().ok_or(CredentialError::Revoked)?;
        if !credential.is_expiring() {
            return Ok(credential);
        }

        let refreshed = match refresh_credential(credential.as_ref().clone()).await {
            Ok(refreshed) => Arc::new(refreshed),
            Err(error) => {
                if is_credential_rejected(&error) {
                    self.revoke();
                }
                return Err(error);
            }
        };
        // 网络刷新期间仍可能注销；仅替换最初的凭据快照，避免让旧会话重新生效。
        let previous = self
            .inner
            .current
            .compare_and_swap(&credential, Some(refreshed.clone()));
        if previous
            .as_ref()
            .is_some_and(|previous| Arc::ptr_eq(previous, &credential))
        {
            self.notify_update();
            Ok(refreshed)
        } else {
            self.snapshot()
                .ok_or_else(|| CredentialError::Revoked.into())
        }
    }

    fn notify_update(&self) {
        self.inner
            .updates
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct QqCredential {
    pub music_id: u64,
    pub music_key: String,
    #[serde(default)]
    pub open_id: String,
    #[serde(default)]
    pub access_token: String,
    pub refresh_token: String,
    pub refresh_key: String,
    #[serde(default)]
    pub union_id: String,
    #[serde(default)]
    pub string_music_id: String,
    #[serde(default)]
    pub music_key_create_time: i64,
    #[serde(default)]
    pub key_expires_in: i64,
    #[serde(default)]
    pub first_login: i64,
    #[serde(default)]
    pub bind_account_type: i64,
    #[serde(default)]
    pub need_refresh_key_in: i64,
    pub login_type: u64,
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub encrypted_uin: String,
    #[serde(default = "new_client_guid")]
    pub client_guid: String,
}

impl QqCredential {
    fn from_token(token: LoginToken) -> Self {
        Self {
            music_id: token.music_id,
            music_key: token.music_key,
            open_id: token.open_id,
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            refresh_key: token.refresh_key,
            union_id: token.union_id,
            string_music_id: token.string_music_id,
            music_key_create_time: token.music_key_create_time,
            key_expires_in: token.key_expires_in,
            first_login: token.first_login,
            bind_account_type: token.bind_account_type,
            need_refresh_key_in: token.need_refresh_key_in,
            login_type: token.login_type,
            expires_at: token.expires_at,
            encrypted_uin: token.encrypted_uin,
            client_guid: new_client_guid(),
        }
    }

    fn to_token(&self) -> LoginToken {
        LoginToken {
            music_id: self.music_id,
            music_key: self.music_key.clone(),
            open_id: self.open_id.clone(),
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            refresh_key: self.refresh_key.clone(),
            union_id: self.union_id.clone(),
            string_music_id: self.string_music_id.clone(),
            music_key_create_time: self.music_key_create_time,
            key_expires_in: self.key_expires_in,
            first_login: self.first_login,
            bind_account_type: self.bind_account_type,
            need_refresh_key_in: self.need_refresh_key_in,
            login_type: self.login_type,
            expires_at: self.expires_at,
            encrypted_uin: self.encrypted_uin.clone(),
        }
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
        let expires_at = if self.music_key_create_time > 0 && self.key_expires_in > 0 {
            Some(self.music_key_create_time + self.key_expires_in)
        } else {
            self.expires_at
        };
        expires_at.is_some_and(|expires_at| expires_at <= now + 300)
    }
}

pub(crate) fn new_client_guid() -> String {
    Uuid::new_v4().simple().to_string().to_ascii_uppercase()
}

#[allow(clippy::large_enum_variant)]
pub enum LoginEvent {
    QrReady(Vec<u8>),
    WaitingScan,
    WaitingConfirm,
    Succeeded(QqCredential),
    Expired,
    Failed(String),
}

pub async fn run_qr_login(events: mpsc::UnboundedSender<LoginEvent>) {
    if let Err(error) = qr_login(&events).await {
        let _ = events.send(LoginEvent::Failed(format!("{error:#}")));
    }
}

async fn qr_login(events: &mpsc::UnboundedSender<LoginEvent>) -> Result<()> {
    let client = LoginClient::new();
    let qr = client
        .get_login_qrcode()
        .await
        .context("无法创建 QQ 音乐扫码会话")?;
    // 扫码循环独占 MQTT 会话，状态随本次登录任务一起结束。
    let mut session = MqttLoginSession::new(&qr.qr_key);

    let encoded = qr
        .qr_code
        .strip_prefix(QR_DATA_PREFIX)
        .context("QQ 音乐返回了无法识别的二维码")?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("无法解码 QQ 音乐二维码")?;
    let _ = events.send(LoginEvent::QrReady(png));

    loop {
        match client
            .get_login_token_with_mqtt_session(&qr.qr_key, &mut session)
            .await
            .context("QQ 音乐扫码状态查询失败")?
        {
            LoginStatus::WaitingScan => {
                let _ = events.send(LoginEvent::WaitingScan);
            }
            LoginStatus::WaitingConfirm => {
                let _ = events.send(LoginEvent::WaitingConfirm);
            }
            LoginStatus::QrCodeExpired => {
                let _ = events.send(LoginEvent::Expired);
                return Ok(());
            }
            LoginStatus::Success(token) => {
                let credential = QqCredential::from_token(token);
                let credential = ProtocolClient::new()?
                    .ensure_encrypted_uin(credential)
                    .await?;
                let _ = events.send(LoginEvent::Succeeded(credential));
                return Ok(());
            }
        }
    }
}

async fn refresh_credential(credential: QqCredential) -> Result<QqCredential> {
    let client = LoginClient::new();
    let previous = credential.to_token();
    let token = client
        .refresh_login_token(&previous)
        .await
        .context("QQ 音乐凭据刷新失败")?;

    let mut refreshed = QqCredential::from_token(token);
    if refreshed.encrypted_uin.is_empty() {
        refreshed.encrypted_uin = credential.encrypted_uin;
    }
    refreshed.client_guid = credential.client_guid;
    Ok(refreshed)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CredentialError, CredentialSession, QqCredential};

    fn credential_from_old_storage(login_type: u64) -> QqCredential {
        serde_json::from_value(json!({
            "music_id": 123,
            "music_key": "music-key",
            "refresh_token": "refresh-token",
            "refresh_key": "refresh-key",
            "login_type": login_type,
            "expires_at": 1,
            "encrypted_uin": "encrypted-uin",
            "client_guid": "client-guid"
        }))
        .expect("deserialize old stored credential")
    }

    fn refreshable_expiring_credential() -> QqCredential {
        let mut credential = credential_from_old_storage(2);
        credential.open_id = "open-id".to_owned();
        credential.access_token = "access-token".to_owned();
        credential
    }

    #[test]
    fn credential_session_shares_and_revokes_snapshot() {
        let credential = credential_from_old_storage(2);
        let session = CredentialSession::new(credential);
        let clone = session.clone();
        let snapshot = session.snapshot().expect("active credential snapshot");

        assert_eq!(snapshot.music_id, 123);
        let revoked = clone.revoke().expect("revoked credential snapshot");
        assert!(std::sync::Arc::ptr_eq(&snapshot, &revoked));
        assert!(session.snapshot().is_none());
    }

    #[test]
    fn credential_session_revokes_only_for_explicit_server_rejection() {
        use anyhow::Context as _;

        let rejected = CredentialSession::new(refreshable_expiring_credential());
        let error = Err::<(), _>(CredentialError::Rejected { code: 104_401 })
            .context("读取用户资料失败")
            .expect_err("credential rejection");
        rejected.revoke_if_rejected(&error);
        assert!(rejected.snapshot().is_none());

        let refresh_rejected = CredentialSession::new(refreshable_expiring_credential());
        let error = Err::<(), _>(crate::error::LoginError::TencentLoginServerError {
            code: 1000,
            detail: "登录授权已过期，请重新登录".to_owned(),
        })
        .context("QQ 音乐凭据刷新失败")
        .expect_err("refresh rejection");
        refresh_rejected.revoke_if_rejected(&error);
        assert!(refresh_rejected.snapshot().is_none());

        let transient = CredentialSession::new(refreshable_expiring_credential());
        let error = anyhow::anyhow!("temporary network failure");
        transient.revoke_if_rejected(&error);
        assert!(transient.snapshot().is_some());
    }
}
