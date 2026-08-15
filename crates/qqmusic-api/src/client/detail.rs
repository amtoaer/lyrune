//! 详情域 typed builder。
//!
//! # Overview
//!
//! 该模块提供歌曲、歌手、专辑、歌单和榜单详情查询。
//! 所有 `id(...)` 接口统一接受 [`crate::client::MusicIdInput`]；
//! 其中歌单/榜单请求在发送前会校验并要求纯数字 ID。
//!
//! # Quick Start
//!
//! ```no_run
//! # async fn run() -> Result<(), netease_qq_music_api::MusicClientError> {
//! let client = netease_qq_music_api::MusicClient::new();
//! let artist = client.detail().artist().id("3684").limit(10).send().await?;
//! println!("artist: {}", artist.name);
//! # Ok(())
//! # }
//! ```

use std::marker::PhantomData;

use super::utils::{
    MusicIdInput, netease_token, require_id, tencent_token, validate_auth_platform,
};
use super::{LoginTokenRef, MusicClient};
use crate::error::{MusicClientError, MusicClientResult};
use crate::models::{
    AlbumDetailResult, ArtistDetailResult, Platform, PlaylistDetailResult, SongsDetailResult,
};

/// 详情域根类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DetailKind;

/// 歌曲详情类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DetailSongKind;

/// 歌手详情类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DetailArtistKind;

/// 专辑详情类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DetailAlbumKind;

/// 歌单详情类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DetailPlaylistKind;

/// 榜单详情类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DetailToplistKind;

/// 详情请求构建器。
///
/// `K` 为类型态参数，用于限制可调用的方法组合。
///
/// 当目标能力支持歌曲分页时，会使用内部默认分页参数。
pub struct DetailRequest<'a, K> {
    client: &'a MusicClient,
    id: Option<String>,
    numeric_id: Option<MusicClientResult<u64>>,
    song_ids: Option<Vec<String>>,
    offset: u64,
    limit: u64,
    platform: Platform,
    token: Option<LoginTokenRef<'a>>,
    _kind: PhantomData<K>,
}

impl<'a, K> DetailRequest<'a, K> {
    const DEFAULT_LIMIT: u64 = 20;
    const DEFAULT_OFFSET: u64 = 0;

    fn into_kind<T>(self) -> DetailRequest<'a, T> {
        DetailRequest {
            client: self.client,
            id: self.id,
            numeric_id: self.numeric_id,
            song_ids: self.song_ids,
            offset: self.offset,
            limit: self.limit,
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
    /// [`MusicClientError::AuthTokenPlatformMismatch`]。
    pub fn login(mut self, token: impl Into<LoginTokenRef<'a>>) -> Self {
        self.token = Some(token.into());
        self
    }
}

impl<'a> DetailRequest<'a, DetailKind> {
    pub(super) fn new(client: &'a MusicClient) -> Self {
        Self {
            client,
            id: None,
            numeric_id: None,
            song_ids: None,
            offset: DetailRequest::<DetailKind>::DEFAULT_OFFSET,
            limit: DetailRequest::<DetailKind>::DEFAULT_LIMIT,
            platform: Platform::Netease,
            token: None,
            _kind: PhantomData,
        }
    }

    /// 切换为歌曲详情请求。
    pub fn song(self) -> DetailRequest<'a, DetailSongKind> {
        self.into_kind()
    }

    /// 切换为歌手详情请求。
    pub fn artist(self) -> DetailRequest<'a, DetailArtistKind> {
        self.into_kind()
    }

    /// 切换为专辑详情请求。
    pub fn album(self) -> DetailRequest<'a, DetailAlbumKind> {
        self.into_kind()
    }

    /// 切换为歌单详情请求。
    pub fn playlist(self) -> DetailRequest<'a, DetailPlaylistKind> {
        self.into_kind()
    }

    /// 切换为榜单详情请求。
    pub fn toplist(self) -> DetailRequest<'a, DetailToplistKind> {
        self.into_kind()
    }
}

impl<'a> DetailRequest<'a, DetailSongKind> {
    /// 设置单个歌曲 ID（支持 [`MusicIdInput`]，必填且不能为空）。
    pub fn id(mut self, id: impl MusicIdInput) -> Self {
        self.song_ids
            .get_or_insert_with(Vec::new)
            .push(id.into_id_string());
        self
    }

    /// 设置歌曲 ID 列表（必填，且元素不能为空）。
    pub fn ids<I, S>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: MusicIdInput,
    {
        self.song_ids
            .get_or_insert_with(Vec::new)
            .extend(ids.into_iter().map(MusicIdInput::into_id_string));
        self
    }

    /// 发送歌曲详情请求。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`Self::id`] 或 [`Self::ids`] 设置歌曲 ID。
    /// - 若调用过 [`DetailRequest::login`]，其 token 所属平台必须与 [`DetailRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`MusicClientError::MissingId`] - 未设置歌曲 `id`，或任一 `id` 为空字符串
    /// - [`MusicClientError::AuthTokenPlatformMismatch`] - token 所属平台与请求平台不匹配
    /// - [`MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<SongsDetailResult> {
        validate_auth_platform(self.platform, self.token)?;
        let ids = match self.song_ids {
            Some(ids) if !ids.is_empty() && ids.iter().all(|id| !id.trim().is_empty()) => ids,
            _ => return Err(MusicClientError::MissingId),
        };
        match self.platform {
            Platform::Netease => {
                self.client
                    .netease
                    .songs_detail(ids, netease_token(self.token))
                    .await
            }
            Platform::Tencent => {
                self.client
                    .tencent
                    .songs_detail(ids, tencent_token(self.token))
                    .await
            }
        }
    }
}

impl<'a> DetailRequest<'a, DetailArtistKind> {
    /// 设置歌手 ID（支持 [`MusicIdInput`]，必填且不能为空）。
    pub fn id(mut self, id: impl MusicIdInput) -> Self {
        self.id = Some(id.into_id_string());
        self
    }

    #[allow(rustdoc::private_intra_doc_links)]
    /// 设置歌曲列表分页偏移量，默认值见 [`Self::DEFAULT_OFFSET`]。
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = offset;
        self
    }

    #[allow(rustdoc::private_intra_doc_links)]
    /// 设置歌曲列表分页大小，默认值见 [`Self::DEFAULT_LIMIT`]。
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = limit;
        self
    }

    /// 发送歌手详情请求。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`Self::id`] 设置歌手 ID。
    /// - 若调用过 [`DetailRequest::login`]，其 token 所属平台必须与 [`DetailRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`MusicClientError::MissingId`] - 未设置歌手 `id` 或 `id` 为空字符串
    /// - [`MusicClientError::AuthTokenPlatformMismatch`] - token 所属平台与请求平台不匹配
    /// - [`MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<ArtistDetailResult> {
        let id = require_id(self.id.as_deref())?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client
                    .netease
                    .artist_detail(id, self.limit, self.offset, netease_token(self.token))
                    .await
            }
            Platform::Tencent => {
                self.client
                    .tencent
                    .artist_detail(id, self.limit, self.offset, tencent_token(self.token))
                    .await
            }
        }
    }
}

impl<'a> DetailRequest<'a, DetailAlbumKind> {
    /// 设置专辑 ID（支持 [`MusicIdInput`]，必填且不能为空）。
    pub fn id(mut self, id: impl MusicIdInput) -> Self {
        self.id = Some(id.into_id_string());
        self
    }

    /// 发送专辑详情请求。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`Self::id`] 设置专辑 ID。
    /// - 若调用过 [`DetailRequest::login`]，其 token 所属平台必须与 [`DetailRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`MusicClientError::MissingId`] - 未设置专辑 `id` 或 `id` 为空字符串
    /// - [`MusicClientError::AuthTokenPlatformMismatch`] - token 所属平台与请求平台不匹配
    /// - [`MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<AlbumDetailResult> {
        let id = require_id(self.id.as_deref())?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client
                    .netease
                    .album_detail(id, netease_token(self.token))
                    .await
            }
            Platform::Tencent => {
                self.client
                    .tencent
                    .album_detail(id, tencent_token(self.token))
                    .await
            }
        }
    }
}

impl<'a> DetailRequest<'a, DetailPlaylistKind> {
    /// 设置歌单 ID（支持 [`MusicIdInput`]，必填）。
    pub fn id(mut self, id: impl MusicIdInput) -> Self {
        self.numeric_id = Some(id.try_into_id_u64());
        self
    }

    /// 发送歌单详情请求。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`Self::id`] 设置歌单 ID。
    /// - 若调用过 [`DetailRequest::login`]，其 token 所属平台必须与 [`DetailRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`MusicClientError::MissingId`] - 未设置歌单 `id`
    /// - [`MusicClientError::InvalidIdFormat`] - 歌单 `id` 不是纯数字
    /// - [`MusicClientError::AuthTokenPlatformMismatch`] - token 所属平台与请求平台不匹配
    /// - [`MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<PlaylistDetailResult> {
        let id = self
            .numeric_id
            .transpose()?
            .ok_or(MusicClientError::MissingId)?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client
                    .netease
                    .playlist_detail(id, netease_token(self.token))
                    .await
            }
            Platform::Tencent => {
                self.client
                    .tencent
                    .playlist_detail(id, tencent_token(self.token))
                    .await
            }
        }
    }
}

impl<'a> DetailRequest<'a, DetailToplistKind> {
    /// 设置榜单 ID（支持 [`MusicIdInput`]，必填）。
    pub fn id(mut self, id: impl MusicIdInput) -> Self {
        self.numeric_id = Some(id.try_into_id_u64());
        self
    }

    /// 发送榜单详情请求。
    ///
    /// 在 [`Platform::Netease`] 平台会复用歌单详情接口，在
    /// [`Platform::Tencent`] 平台会调用独立榜单详情接口。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`Self::id`] 设置榜单 ID。
    /// - 若调用过 [`DetailRequest::login`]，其 token 所属平台必须与 [`DetailRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`MusicClientError::MissingId`] - 未设置榜单 `id`
    /// - [`MusicClientError::InvalidIdFormat`] - 榜单 `id` 不是纯数字
    /// - [`MusicClientError::AuthTokenPlatformMismatch`] - token 所属平台与请求平台不匹配
    /// - [`MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<PlaylistDetailResult> {
        let id = self
            .numeric_id
            .transpose()?
            .ok_or(MusicClientError::MissingId)?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client
                    .netease
                    .playlist_detail(id, netease_token(self.token))
                    .await
            }
            Platform::Tencent => {
                self.client
                    .tencent
                    .toplist_detail(id, tencent_token(self.token))
                    .await
            }
        }
    }
}
