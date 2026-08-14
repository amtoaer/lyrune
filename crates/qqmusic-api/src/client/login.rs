//! 登录域 typed builder。
//!
//! # Overview
//!
//! 本模块提供二维码登录会话创建与登录 token 刷新能力。
//! 二维码登录通过 [`LoginSession::status`] 轮询登录状态，成功后返回 [`crate::models::LoginToken`]。
//!
//! # Quick Start
//!
//! ```no_run
//! # async fn run() -> Result<(), netease_qq_music_api::MusicClientError> {
//! let client = netease_qq_music_api::MusicClient::new();
//! let session = client.login().session().send().await?;
//! println!("qr code: {}", session.qr_code());
//! # Ok(())
//! # }
//! ```

use std::marker::PhantomData;

use tokio::sync::Mutex;

use super::utils::validate_auth_platform;
use super::{LoginTokenRef, MusicClient};
use crate::error::{MusicClientError, MusicClientResult};
use crate::models::{LoginQrResult, LoginStatus, LoginToken, Platform};
use crate::platform::TencentMqttLoginSession;

/// 登录域根类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct LoginKind;

/// 刷新 token 请求类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct LoginRefreshKind;

/// 二维码登录会话请求类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct LoginSessionKind;

/// 二维码登录会话。
///
/// 通过 [`Self::status`] 轮询扫码登录进度，调用 [`Self::qr_code`] 获取二维码图像内容。
pub struct LoginSession<'a> {
    client: &'a MusicClient,
    qr: LoginQrResult,
    state: LoginSessionState,
}

struct TencentLoginSession {
    qrcode_id: String,
    mqtt: Mutex<TencentMqttLoginSession>,
}

enum LoginSessionState {
    Netease,
    Tencent(Box<TencentLoginSession>),
}

impl TencentLoginSession {
    fn new(qrcode_id: &str) -> Self {
        Self {
            qrcode_id: qrcode_id.to_owned(),
            mqtt: Mutex::new(TencentMqttLoginSession::new(qrcode_id)),
        }
    }
}

impl<'a> LoginSession<'a> {
    /// 返回当前登录会话所属平台。
    pub fn platform(&self) -> Platform {
        match &self.state {
            LoginSessionState::Netease => Platform::Netease,
            LoginSessionState::Tencent(_) => Platform::Tencent,
        }
    }

    /// 返回二维码图像内容（`data:image/png;base64,...`）。
    pub fn qr_code(&self) -> &str {
        self.qr.qr_code.as_str()
    }

    /// 返回二维码会话 key，用于轮询登录状态。
    fn qr_key(&self) -> &str {
        self.qr.qr_key.as_str()
    }

    /// 查询二维码登录状态。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`crate::client::LoginRequest::session`] 创建 [`LoginSession`]。
    /// - 对于 [`Platform::Tencent`] 平台，会话内部需要存在 MQTT 上下文。
    ///
    /// # Errors
    ///
    /// - [`MusicClientError::NeteaseUnexpectedLoginStatus`] - 网易登录状态码异常
    /// - [`MusicClientError::NeteaseLoginTokenInvalid`] - 网易登录成功但 token cookie 解析失败
    /// - [`MusicClientError::TencentLoginCanceled`] - QQ 登录被用户取消
    /// - [`MusicClientError::TencentLoginFailed`] - QQ 登录失败
    /// - [`MusicClientError::TencentMqttLogin`] - QQ MQTT 登录链路失败
    /// - [`MusicClientError::NetworkError`] - 网络请求失败
    pub async fn status(&self) -> MusicClientResult<LoginStatus> {
        match &self.state {
            LoginSessionState::Netease => self.client.netease.get_login_token(self.qr_key()).await,
            LoginSessionState::Tencent(session) => {
                let mut mqtt = session.mqtt.lock().await;
                self.client
                    .tencent
                    .get_login_token_with_mqtt_session(session.qrcode_id.as_str(), &mut mqtt)
                    .await
            }
        }
    }
}

/// 登录请求构建器。
///
/// `K` 为类型态参数，用于限制当前可执行的登录子能力。
pub struct LoginRequest<'a, K> {
    client: &'a MusicClient,
    platform: Platform,
    key: Option<String>,
    token: Option<LoginTokenRef<'a>>,
    _kind: PhantomData<K>,
}

impl<'a, K> LoginRequest<'a, K> {
    fn into_kind<T>(self) -> LoginRequest<'a, T> {
        LoginRequest {
            client: self.client,
            platform: self.platform,
            key: self.key,
            token: self.token,
            _kind: PhantomData,
        }
    }

    /// 设置登录平台，默认 [`Platform::Netease`]。
    pub fn platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    /// 切换为二维码登录会话请求。
    pub fn session(self) -> LoginRequest<'a, LoginSessionKind> {
        self.into_kind()
    }

    /// 切换为 token 刷新请求。
    pub fn refresh(self) -> LoginRequest<'a, LoginRefreshKind> {
        self.into_kind()
    }
}

impl<'a> LoginRequest<'a, LoginKind> {
    pub(super) fn new(client: &'a MusicClient) -> Self {
        Self { client, platform: Platform::Netease, key: None, token: None, _kind: PhantomData }
    }
}

impl<'a> LoginRequest<'a, LoginSessionKind> {
    /// 创建二维码登录会话。
    ///
    /// 返回的 [`LoginSession`] 可用于获取二维码并轮询登录状态。
    ///
    /// # 前置条件
    ///
    /// - 可选通过 [`LoginRequest::platform`] 指定平台，不设置时默认 [`Platform::Netease`]。
    ///
    /// # Errors
    ///
    /// - [`MusicClientError::NetworkError`] - 网络请求失败
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), netease_qq_music_api::MusicClientError> {
    /// let client = netease_qq_music_api::MusicClient::new();
    /// let session = client
    ///     .login()
    ///     .session()
    ///     .platform(netease_qq_music_api::models::Platform::Tencent)
    ///     .send()
    ///     .await?;
    /// assert!(!session.qr_code().is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(self) -> MusicClientResult<LoginSession<'a>> {
        let qr = match self.platform {
            Platform::Netease => self.client.netease.get_login_qrcode().await?,
            Platform::Tencent => self.client.tencent.get_login_qrcode().await?,
        };
        let state = match self.platform {
            Platform::Netease => LoginSessionState::Netease,
            Platform::Tencent => {
                LoginSessionState::Tencent(Box::new(TencentLoginSession::new(qr.qr_key.as_str())))
            }
        };
        Ok(LoginSession { client: self.client, qr, state })
    }
}

impl<'a> LoginRequest<'a, LoginRefreshKind> {
    /// 设置用于刷新的登录 token（必填）。
    pub fn token(mut self, token: impl Into<LoginTokenRef<'a>>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// 刷新登录 token。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`Self::token`] 注入登录 token。
    /// - token 所属平台必须与 [`LoginRequest::platform`] 一致。
    ///
    /// # Errors
    ///
    /// - [`MusicClientError::MissingRefreshToken`] - 未设置 `token`
    /// - [`MusicClientError::AuthTokenPlatformMismatch`] - token 所属平台与请求平台不匹配
    /// - [`MusicClientError::NeteaseLoginTokenInvalid`] - 网易刷新成功但 token cookie 解析失败
    /// - [`MusicClientError::InvalidTencentLoginTokenField`] - QQ 刷新响应字段非法
    /// - [`MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<LoginToken> {
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                let token = self
                    .token
                    .and_then(LoginTokenRef::as_netease)
                    .ok_or(MusicClientError::MissingRefreshToken)?;
                self.client.netease.refresh_login_token(token).await.map(LoginToken::Netease)
            }
            Platform::Tencent => {
                let token = self
                    .token
                    .and_then(LoginTokenRef::as_tencent)
                    .ok_or(MusicClientError::MissingRefreshToken)?;
                self.client.tencent.refresh_login_token(token).await.map(LoginToken::Tencent)
            }
        }
    }
}
