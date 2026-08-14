//! 发现域 typed builder。
//!
//! # Overview
//!
//! 该模块覆盖发现页相关能力：热词、推荐歌单、榜单列表、歌单分类、分类歌单列表。
//! 默认平台为 [`Platform::Netease`]，可通过 [`DiscoverRequest::platform`] 切换。
//!
//! # Quick Start
//!
//! ```no_run
//! # async fn run() -> Result<(), netease_qq_music_api::MusicClientError> {
//! let client = netease_qq_music_api::MusicClient::new();
//! let hotkey = client.discover().hotkey().send().await?;
//! println!("hotkeys: {}", hotkey.hotkey.len());
//! # Ok(())
//! # }
//! ```

use std::marker::PhantomData;

use super::utils::{
    netease_token, require_category, require_keyword, tencent_token, validate_auth_platform,
};
use super::{LoginTokenRef, MusicClient};
use crate::error::MusicClientResult;
use crate::models::{
    HotkeyResult, Platform, PlaylistCategoriesResult, PlaylistListResult, RecommendPlaylistResult,
    SearchSuggestResult, ToplistListResult,
};

/// 发现域根类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DiscoverKind;

/// 搜索建议类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DiscoverSearchSuggestsKind;

/// 热词能力类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DiscoverHotkeyKind;

/// 推荐歌单能力类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DiscoverRecommendPlaylistKind;

/// 榜单列表能力类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DiscoverToplistListKind;

/// 歌单分类能力类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DiscoverPlaylistCategoriesKind;

/// 歌单列表能力类型态标记。
#[derive(Clone, Copy, Debug)]
pub struct DiscoverPlaylistListKind;

/// 发现请求构建器。
///
/// `K` 为类型态参数，用于限制当前子能力。
///
/// 歌单列表子能力默认分页参数由内部常量维护。
pub struct DiscoverRequest<'a, K> {
    client: &'a MusicClient,
    category: Option<String>,
    keyword: Option<String>,
    offset: u64,
    limit: u64,
    platform: Platform,
    token: Option<LoginTokenRef<'a>>,
    _kind: PhantomData<K>,
}

impl<'a, K> DiscoverRequest<'a, K> {
    const DEFAULT_LIMIT: u64 = 20;
    const DEFAULT_OFFSET: u64 = 0;

    fn into_kind<T>(self) -> DiscoverRequest<'a, T> {
        DiscoverRequest {
            client: self.client,
            category: self.category,
            keyword: self.keyword,
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
    /// [`crate::error::MusicClientError::AuthTokenPlatformMismatch`]。
    pub fn login(mut self, token: impl Into<LoginTokenRef<'a>>) -> Self {
        self.token = Some(token.into());
        self
    }
}

impl<'a> DiscoverRequest<'a, DiscoverKind> {
    pub(super) fn new(client: &'a MusicClient) -> Self {
        Self {
            client,
            category: None,
            keyword: None,
            offset: DiscoverRequest::<DiscoverKind>::DEFAULT_OFFSET,
            limit: DiscoverRequest::<DiscoverKind>::DEFAULT_LIMIT,
            platform: Platform::Netease,
            token: None,
            _kind: PhantomData,
        }
    }

    /// 切换为搜索建议请求。
    pub fn suggests(self) -> DiscoverRequest<'a, DiscoverSearchSuggestsKind> {
        self.into_kind()
    }

    /// 切换为热词请求。
    pub fn hotkey(self) -> DiscoverRequest<'a, DiscoverHotkeyKind> {
        self.into_kind()
    }

    /// 切换为推荐歌单请求。
    pub fn recommend_playlist(self) -> DiscoverRequest<'a, DiscoverRecommendPlaylistKind> {
        self.into_kind()
    }

    /// 切换为榜单列表请求。
    pub fn toplist_list(self) -> DiscoverRequest<'a, DiscoverToplistListKind> {
        self.into_kind()
    }

    /// 切换为歌单分类请求。
    pub fn playlist_categories(self) -> DiscoverRequest<'a, DiscoverPlaylistCategoriesKind> {
        self.into_kind()
    }

    /// 切换为歌单列表请求。
    pub fn playlist_list(self) -> DiscoverRequest<'a, DiscoverPlaylistListKind> {
        self.into_kind()
    }
}

impl<'a> DiscoverRequest<'a, DiscoverSearchSuggestsKind> {
    /// 设置搜索关键词（必填且不能为空字符串）。
    pub fn keyword(mut self, keyword: impl Into<String>) -> Self {
        self.keyword = Some(keyword.into());
        self
    }

    /// 发送搜索建议请求。
    ///
    /// # 前置条件
    ///
    /// - 若调用过 [`DiscoverRequest::login`]，其 token 所属平台必须与 [`DiscoverRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<SearchSuggestResult> {
        let keyword = require_keyword(self.keyword.as_deref())?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client.netease.get_search_suggests(keyword, netease_token(self.token)).await
            }
            Platform::Tencent => {
                self.client.tencent.get_search_suggests(keyword, tencent_token(self.token)).await
            }
        }
    }
}

impl<'a> DiscoverRequest<'a, DiscoverHotkeyKind> {
    /// 发送热词请求。
    ///
    /// # 前置条件
    ///
    /// - 若调用过 [`DiscoverRequest::login`]，其 token 所属平台必须与 [`DiscoverRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<HotkeyResult> {
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => self.client.netease.get_hotkey(netease_token(self.token)).await,
            Platform::Tencent => self.client.tencent.get_hotkey(tencent_token(self.token)).await,
        }
    }
}

impl<'a> DiscoverRequest<'a, DiscoverRecommendPlaylistKind> {
    /// 发送推荐歌单请求。
    ///
    /// # 前置条件
    ///
    /// - 若调用过 [`DiscoverRequest::login`]，其 token 所属平台必须与 [`DiscoverRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<RecommendPlaylistResult> {
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client.netease.get_recommend_playlist(netease_token(self.token)).await
            }
            Platform::Tencent => {
                self.client.tencent.get_recommend_playlist(tencent_token(self.token)).await
            }
        }
    }
}

impl<'a> DiscoverRequest<'a, DiscoverToplistListKind> {
    /// 发送榜单列表请求。
    ///
    /// # 前置条件
    ///
    /// - 若调用过 [`DiscoverRequest::login`]，其 token 所属平台必须与 [`DiscoverRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<ToplistListResult> {
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => self.client.netease.get_toplist(netease_token(self.token)).await,
            Platform::Tencent => self.client.tencent.get_toplist(tencent_token(self.token)).await,
        }
    }
}

impl<'a> DiscoverRequest<'a, DiscoverPlaylistCategoriesKind> {
    /// 发送歌单分类请求。
    ///
    /// # 前置条件
    ///
    /// - 若调用过 [`DiscoverRequest::login`]，其 token 所属平台必须与 [`DiscoverRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<PlaylistCategoriesResult> {
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client.netease.get_playlist_categories(netease_token(self.token)).await
            }
            Platform::Tencent => {
                self.client.tencent.get_playlist_categories(tencent_token(self.token)).await
            }
        }
    }
}

impl<'a> DiscoverRequest<'a, DiscoverPlaylistListKind> {
    /// 设置歌单分类（必填且不能为空字符串）。
    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
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

    /// 发送歌单列表请求。
    ///
    /// # 前置条件
    ///
    /// - 需要先通过 [`Self::category`] 设置非空分类名称或分类
    ///   ID（字符串形式，去除首尾空白后不能为空）。
    /// - 若调用过 [`DiscoverRequest::login`]，其 token 所属平台必须与 [`DiscoverRequest::platform`]
    ///   一致。
    ///
    /// # Errors
    ///
    /// - [`crate::error::MusicClientError::MissingCategory`] - 未设置 `category` 或 `category`
    ///   为空字符串
    /// - [`crate::error::MusicClientError::AuthTokenPlatformMismatch`] - token
    ///   所属平台与请求平台不匹配
    /// - [`crate::error::MusicClientError::NetworkError`] - 网络请求失败
    pub async fn send(self) -> MusicClientResult<PlaylistListResult> {
        let category = require_category(self.category.as_deref())?;
        validate_auth_platform(self.platform, self.token)?;
        match self.platform {
            Platform::Netease => {
                self.client
                    .netease
                    .get_playlist_list(category, self.limit, self.offset, netease_token(self.token))
                    .await
            }
            Platform::Tencent => {
                self.client
                    .tencent
                    .get_playlist_list(category, self.limit, self.offset, tencent_token(self.token))
                    .await
            }
        }
    }
}
