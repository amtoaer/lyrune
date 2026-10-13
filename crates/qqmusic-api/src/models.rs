//! Lyrune 使用的 QQ 音乐歌词与登录数据。

use std::fmt;

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

/// 登录二维码结果（crate 内部使用）。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LoginQrResult {
    /// 二维码图像内容（通常为 `data:image/png;base64,...`，可直接渲染）。
    pub(crate) qr_code: String,
    /// 二维码 key，用于后续轮询状态。
    pub(crate) qr_key: String,
}

/// 二维码登录状态。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(clippy::large_enum_variant)]
pub enum LoginStatus {
    /// 登录成功并返回 token。
    Success(TencentLoginToken),
    /// 二维码过期（需重新创建会话）。
    QrCodeExpired,
    /// 等待扫码。
    WaitingScan,
    /// 已扫码，等待用户在客户端确认登录。
    WaitingConfirm,
}

/// QQ 音乐登录 token。
///
/// 字段为 crate 内可见，外部请通过构造函数与辅助方法使用。
#[derive(Clone, Deserialize, Serialize)]
pub struct TencentLoginToken {
    pub(crate) music_id: u64,
    pub(crate) music_key: String,
    #[serde(default)]
    pub(crate) open_id: String,
    #[serde(default)]
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    pub(crate) refresh_key: String,
    #[serde(default)]
    pub(crate) union_id: String,
    #[serde(default)]
    pub(crate) string_music_id: String,
    #[serde(default)]
    pub(crate) music_key_create_time: i64,
    #[serde(default)]
    pub(crate) key_expires_in: i64,
    #[serde(default)]
    pub(crate) first_login: i64,
    #[serde(default)]
    pub(crate) bind_account_type: i64,
    #[serde(default)]
    pub(crate) need_refresh_key_in: i64,
    pub(crate) login_type: u64,
    pub(crate) expires_at: Option<i64>,
    #[serde(default)]
    pub(crate) encrypted_uin: String,
}

impl fmt::Debug for TencentLoginToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TencentLoginToken")
            .field("has_music_id", &(self.music_id != 0))
            .field("login_type", &self.login_type)
            .field("expires_at", &self.expires_at)
            .field("has_encrypted_uin", &!self.encrypted_uin.is_empty())
            .finish_non_exhaustive()
    }
}

impl TencentLoginToken {
    /// 构造 QQ 音乐登录 token。
    ///
    /// `expires_at` 为 Unix 时间戳（秒），[`None`] 表示未知过期时间。
    /// `login_type` 为平台定义的登录类型值。
    pub fn new(
        music_id: u64,
        music_key: impl Into<String>,
        refresh_token: impl Into<String>,
        refresh_key: impl Into<String>,
        expires_at: Option<i64>,
        login_type: u64,
    ) -> Self {
        Self {
            music_id,
            music_key: music_key.into(),
            open_id: String::new(),
            access_token: String::new(),
            refresh_token: refresh_token.into(),
            refresh_key: refresh_key.into(),
            union_id: String::new(),
            string_music_id: String::new(),
            music_key_create_time: 0,
            key_expires_in: 0,
            first_login: 0,
            bind_account_type: 0,
            need_refresh_key_in: 0,
            expires_at,
            login_type,
            encrypted_uin: String::new(),
        }
    }

    /// 返回过期时间戳（秒）。
    pub fn expires_at(&self) -> Option<i64> {
        if self.music_key_create_time > 0 && self.key_expires_in > 0 {
            Some(self.music_key_create_time + self.key_expires_in)
        } else {
            self.expires_at
        }
    }

    pub fn to_cookie(&self) -> String {
        format!(
            "uin={}; qqmusic_key={}; qm_keyst={}; tmeLoginType={}",
            self.music_id, self.music_key, self.music_key, self.login_type,
        )
    }
}
