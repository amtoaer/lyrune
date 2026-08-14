//! 搜索域 typed builder。
//!
//! # Overview
//!
//! 该模块提供跨平台搜索能力，包括歌曲、歌手、专辑、歌单四类检索。
//! 所有请求默认平台为 [`Platform::Netease`]，可通过 [`SearchRequest::platform`] 切换。
//!
//! # Quick Start
//!
//! ```no_run
//! # async fn run() -> Result<(), netease_qq_music_api::MusicClientError> {
//! let client = netease_qq_music_api::MusicClient::new();
//! let songs = client.search().song().keyword("江南").limit(10).send().await?;
//! println!("songs: {}", songs.songs.len());
//! # Ok(())
//! # }
//! ```
//!
//! # Common Workflow
//!
//! 1. 通过 [`MusicClient::search`] 获取根构建器。
//! 2. 选择子能力：[`SearchRequest::song`]、[`SearchRequest::artist`]、
//!    [`SearchRequest::album`]、[`SearchRequest::playlist`]。
//! 3. 设置必填参数 [`SearchRequest::keyword`]，并按需设置分页参数 [`SearchRequest::offset`] 与
//!    [`SearchRequest::limit`]。
//! 4. 调用 `send().await`。

use std::marker::PhantomData;

use super::utils::{netease_token, require_keyword, tencent_token, validate_auth_platform};
use super::{LoginTokenRef, MusicClient};
use crate::error::MusicClientResult;
use crate::models::{
    Platform, SearchAlbumResult, SearchArtistResult, SearchPlaylistResult, SearchSongResult,
};

/// 搜索域根类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct SearchKind;

/// 歌曲搜索类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct SearchSongKind;

/// 歌手搜索类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct SearchArtistKind;

/// 专辑搜索类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct SearchAlbumKind;

/// 歌单搜索类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct SearchPlaylistKind;

/// 搜索请求构建器。
///
/// `K` 为类型态参数，用于约束当前已选择的搜索子能力。
///
/// 默认分页参数由内部常量维护。
pub struct SearchRequest<'a, K> {
    client: &'a MusicClient,
    keyword: Option<String>,
    offset: u64,
    limit: u64,
    platform: Platform,
    token: Option<LoginTokenRef<'a>>,
    _kind: PhantomData<K>,
}

impl<'a, K> SearchRequest<'a, K> {
    const DEFAULT_LIMIT: u64 = 20;
    const DEFAULT_OFFSET: u64 = 0;

    fn into_kind<T>(self) -> SearchRequest<'a, T> {
        SearchRequest {
            client: self.client,
            keyword: self.keyword,
            offset: self.offset,
            limit: self.limit,
            platform: self.platform,
            token: self.token,
            _kind: PhantomData,
        }
    }

    /// 设置搜索关键词（必填且不能为空字符串）。
    pub fn keyword(mut self, keyword: impl Into<String>) -> Self {
        self.keyword = Some(keyword.into());
        self
    }

    #[allow(rustdoc::private_intra_doc_links)]
    /// 设置分页偏移量，默认值见 [`Self::DEFAULT_OFFSET`]。
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = offset;
        self
    }

    #[allow(rustdoc::private_intra_doc_links)]
    /// 设置分页大小，默认值见 [`Self::DEFAULT_LIMIT`]。
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = limit;
        self
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

impl<'a> SearchRequest<'a, SearchKind> {
    pub(super) fn new(client: &'a MusicClient) -> Self {
        Self {
            client,
            keyword: None,
            offset: SearchRequest::<SearchKind>::DEFAULT_OFFSET,
            limit: SearchRequest::<SearchKind>::DEFAULT_LIMIT,
            platform: Platform::Netease,
            token: None,
            _kind: PhantomData,
        }
    }

    /// 切换为歌曲搜索请求。
    pub fn song(self) -> SearchRequest<'a, SearchSongKind> {
        self.into_kind()
    }

    /// 切换为歌手搜索请求。
    pub fn artist(self) -> SearchRequest<'a, SearchArtistKind> {
        self.into_kind()
    }

    /// 切换为专辑搜索请求。
    pub fn album(self) -> SearchRequest<'a, SearchAlbumKind> {
        self.into_kind()
    }

    /// 切换为歌单搜索请求。
    pub fn playlist(self) -> SearchRequest<'a, SearchPlaylistKind> {
        self.into_kind()
    }

    /// 直接发送默认搜索（等价于歌曲搜索）。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`Self::keyword`] 设置非空关键词（去除首尾空白后不能为空）。
    /// - 若调用过 [`Self::login`]，其 token 所属平台必须与 [`Self::platform`] 一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::MissingKeyword`] - 未设置 `keyword` 或 `keyword`
    ///   为空字符串
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn run() -> Result<(), netease_qq_music_api::MusicClientError> {
    /// let client = netease_qq_music_api::MusicClient::new();
    /// let result = client.search().keyword("林俊杰").send().await?;
    /// assert!(!result.songs.is_empty() || result.more);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(self) -> MusicClientResult<SearchSongResult> {
        let keyword = require_keyword(self.keyword.as_deref())?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client
                    .netease
                    .search_songs(keyword, self.limit, self.offset, netease_token(self.token))
                    .await
            }
            Platform::Tencent => {
                self.client
                    .tencent
                    .search_songs(keyword, self.limit, self.offset, tencent_token(self.token))
                    .await
            }
        }
    }
}

impl<'a> SearchRequest<'a, SearchSongKind> {
    /// 发送歌曲搜索请求。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`SearchRequest::keyword`] 设置非空关键词（去除首尾空白后不能为空）。
    /// - 若调用过 [`SearchRequest::login`]，其 token 所属平台必须与 [`SearchRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::MissingKeyword`] - 未设置 `keyword` 或 `keyword`
    ///   为空字符串
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<SearchSongResult> {
        let keyword = require_keyword(self.keyword.as_deref())?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client
                    .netease
                    .search_songs(keyword, self.limit, self.offset, netease_token(self.token))
                    .await
            }
            Platform::Tencent => {
                self.client
                    .tencent
                    .search_songs(keyword, self.limit, self.offset, tencent_token(self.token))
                    .await
            }
        }
    }
}

impl<'a> SearchRequest<'a, SearchArtistKind> {
    /// 发送歌手搜索请求。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`SearchRequest::keyword`] 设置非空关键词（去除首尾空白后不能为空）。
    /// - 若调用过 [`SearchRequest::login`]，其 token 所属平台必须与 [`SearchRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::MissingKeyword`] - 未设置 `keyword` 或 `keyword`
    ///   为空字符串
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<SearchArtistResult> {
        let keyword = require_keyword(self.keyword.as_deref())?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client
                    .netease
                    .search_artists(keyword, self.limit, self.offset, netease_token(self.token))
                    .await
            }
            Platform::Tencent => {
                self.client
                    .tencent
                    .search_artists(keyword, self.limit, self.offset, tencent_token(self.token))
                    .await
            }
        }
    }
}

impl<'a> SearchRequest<'a, SearchAlbumKind> {
    /// 发送专辑搜索请求。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`SearchRequest::keyword`] 设置非空关键词（去除首尾空白后不能为空）。
    /// - 若调用过 [`SearchRequest::login`]，其 token 所属平台必须与 [`SearchRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::MissingKeyword`] - 未设置 `keyword` 或 `keyword`
    ///   为空字符串
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<SearchAlbumResult> {
        let keyword = require_keyword(self.keyword.as_deref())?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client
                    .netease
                    .search_albums(keyword, self.limit, self.offset, netease_token(self.token))
                    .await
            }
            Platform::Tencent => {
                self.client
                    .tencent
                    .search_albums(keyword, self.limit, self.offset, tencent_token(self.token))
                    .await
            }
        }
    }
}

impl<'a> SearchRequest<'a, SearchPlaylistKind> {
    /// 发送歌单搜索请求。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`SearchRequest::keyword`] 设置非空关键词（去除首尾空白后不能为空）。
    /// - 若调用过 [`SearchRequest::login`]，其 token 所属平台必须与 [`SearchRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::MissingKeyword`] - 未设置 `keyword` 或 `keyword`
    ///   为空字符串
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<SearchPlaylistResult> {
        let keyword = require_keyword(self.keyword.as_deref())?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client
                    .netease
                    .search_playlists(keyword, self.limit, self.offset, netease_token(self.token))
                    .await
            }
            Platform::Tencent => {
                self.client
                    .tencent
                    .search_playlists(keyword, self.limit, self.offset, tencent_token(self.token))
                    .await
            }
        }
    }
}
