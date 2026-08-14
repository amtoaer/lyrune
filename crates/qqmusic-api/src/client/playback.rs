//! 播放域 typed builder。
//!
//! # Overview
//!
//! 该模块提供歌词查询与播放链接查询两类能力。
//! 播放链接默认音质为 [`crate::models::SongQuality::Standard`]。
//!
//! # Quick Start
//!
//! ```no_run
//! # async fn run() -> Result<(), netease_qq_music_api::MusicClientError> {
//! let client = netease_qq_music_api::MusicClient::new();
//! let url = client.playback().url().id("108914").send().await?;
//! println!("play url: {}", url.url);
//! # Ok(())
//! # }
//! ```

use std::marker::PhantomData;

use super::utils::{
    MusicIdInput, netease_token, require_id, tencent_token, validate_auth_platform,
};
use super::{LoginTokenRef, MusicClient};
use crate::error::MusicClientResult;
use crate::models::{LyricResult, Platform, SongQuality, UrlResult};

/// 播放域根类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct PlaybackKind;

/// 歌词请求类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct PlaybackLyricKind;

/// 播放链接请求类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct PlaybackUrlKind;

/// 播放请求构建器。
///
/// `K` 为类型态参数，用于限制当前子能力。
pub struct PlaybackRequest<'a, K> {
    client: &'a MusicClient,
    id: Option<String>,
    level: SongQuality,
    platform: Platform,
    token: Option<LoginTokenRef<'a>>,
    _kind: PhantomData<K>,
}

impl<'a, K> PlaybackRequest<'a, K> {
    fn into_kind<T>(self) -> PlaybackRequest<'a, T> {
        PlaybackRequest {
            client: self.client,
            id: self.id,
            level: self.level,
            platform: self.platform,
            token: self.token,
            _kind: PhantomData,
        }
    }

    /// 设置请求平台，默认 [`Platform::Netease`]。
    pub fn platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    /// 注入登录 token。
    ///
    /// 若 token 所属平台与 `.platform(...)` 不一致，`send()` 会返回
    /// [`crate::error::MusicClientError::AuthTokenPlatformMismatch`]。
    pub fn login(mut self, token: impl Into<LoginTokenRef<'a>>) -> Self {
        self.token = Some(token.into());
        self
    }
}

impl<'a> PlaybackRequest<'a, PlaybackKind> {
    pub(super) fn new(client: &'a MusicClient) -> Self {
        Self {
            client,
            id: None,
            level: SongQuality::Standard,
            platform: Platform::Netease,
            token: None,
            _kind: PhantomData,
        }
    }

    /// 切换为歌词请求。
    pub fn lyric(self) -> PlaybackRequest<'a, PlaybackLyricKind> {
        self.into_kind()
    }

    /// 切换为播放链接请求。
    pub fn url(self) -> PlaybackRequest<'a, PlaybackUrlKind> {
        self.into_kind()
    }
}

impl<'a> PlaybackRequest<'a, PlaybackLyricKind> {
    /// 设置歌曲 ID（支持 [`MusicIdInput`]，必填且不能为空）。
    pub fn id(mut self, id: impl MusicIdInput) -> Self {
        self.id = Some(id.into_id_string());
        self
    }

    /// 发送歌词请求。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`Self::id`] 设置歌曲 ID。
    /// - 若调用过 [`PlaybackRequest::login`]，其 token 所属平台必须与 [`PlaybackRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::MissingId`] - 未设置歌曲 `id` 或 `id` 为空字符串
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<LyricResult> {
        let id = require_id(self.id.as_deref())?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => self.client.netease.get_lyric(id, netease_token(self.token)).await,
            Platform::Tencent => self.client.tencent.get_lyric(id, tencent_token(self.token)).await,
        }
    }
}

impl<'a> PlaybackRequest<'a, PlaybackUrlKind> {
    /// 设置歌曲 ID（支持 [`MusicIdInput`]，必填且不能为空）。
    pub fn id(mut self, id: impl MusicIdInput) -> Self {
        self.id = Some(id.into_id_string());
        self
    }

    /// 设置音质等级，默认 [`SongQuality::Standard`]。
    pub fn level(mut self, level: SongQuality) -> Self {
        self.level = level;
        self
    }

    /// 发送播放链接请求。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`Self::id`] 设置歌曲 ID。
    /// - 可选通过 [`Self::level`] 指定音质；默认是 [`SongQuality::Standard`]。
    /// - 若调用过 [`PlaybackRequest::login`]，其 token 所属平台必须与 [`PlaybackRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::MissingId`] - 未设置歌曲 `id` 或 `id` 为空字符串
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<UrlResult> {
        let id = require_id(self.id.as_deref())?;
        validate_auth_platform(self.platform, self.token)?;
        let netease_token_ref = netease_token(self.token);
        let tencent_token_ref = tencent_token(self.token);
        let level = self.level;
        match self.platform {
            Platform::Netease => self.client.netease.get_url(id, level, netease_token_ref).await,
            Platform::Tencent => self.client.tencent.get_url(id, level, tencent_token_ref).await,
        }
    }
}
