//! 跨平台统一数据模型定义。
//!
//! # Overview
//!
//! 本模块承载所有公开返回模型、平台枚举、登录 token 类型与音质枚举。
//! [`crate::client`] 模块中的请求构建器统一返回这里定义的类型，用于屏蔽平台原始响应差异。
//!
//! # Core Concepts
//!
//! - 平台标识：[`Platform`][`crate::models::Platform`]
//! - 搜索/详情/歌单相关结构体：[`crate::models::SearchSongResult`]、
//!   [`crate::models::SearchArtistResult`]、
//!   [`crate::models::SearchAlbumResult`]、[`crate::models::SearchPlaylistResult`]、
//!   [`crate::models::SongsDetailResult`]、
//!   [`crate::models::ArtistDetailResult`]、[`crate::models::AlbumDetailResult`]、
//!   [`crate::models::PlaylistDetailResult`]、
//!   [`crate::models::Playlist`]、[`crate::models::PlaylistCategoriesResult`]、
//!   [`crate::models::PlaylistListResult`]
//! - 登录相关结构体：[`LoginStatus`][`crate::models::LoginStatus`]、
//!   [`LoginToken`][`crate::models::LoginToken`]
//! - 播放相关结构体：[`LyricResult`][`crate::models::LyricResult`]、
//!   [`UrlResult`][`crate::models::UrlResult`]、 [`SongQuality`][`crate::models::SongQuality`]
//!
//! # Errors and Limits
//!
//! 本模块仅定义数据结构，不包含请求与错误处理逻辑。参数校验与失败语义请参考
//! [`client`][`crate::client`] 和 [`error`][`crate::error`]。
//!
//! # See Also
//!
//! - [`client`][`crate::client`]：typed builder 请求入口
//! - [`MusicClientError`][`crate::error::MusicClientError`]：统一错误类型
use std::{collections::HashMap, fmt};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// 支持的平台枚举。
///
/// 大多数请求默认使用 [`Platform::Netease`]，可在请求构建器上显式切换。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum Platform {
    /// 网易云音乐。
    Netease,
    /// QQ 音乐。
    Tencent,
}

/// 歌曲搜索结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchSongResult {
    /// 当前页歌曲列表。
    pub songs: Vec<Song>,
    /// 是否还有更多结果页可供拉取。
    pub more: bool,
}

/// 跨平台统一歌曲模型。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Song {
    /// 歌曲 ID。
    pub id: String,
    /// 歌曲名。
    pub name: String,
    /// 封面图 URL。
    pub pic_url: String,
    /// 歌手列表。
    pub artists: Vec<SongArtist>,
    /// 所属专辑。
    pub album: SongAlbum,
}

/// 歌曲中的歌手信息。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SongArtist {
    /// 歌手 ID。
    pub id: String,
    /// 歌手名。
    pub name: String,
}

/// 歌曲中的专辑信息。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SongAlbum {
    /// 专辑 ID。
    pub id: String,
    /// 专辑名。
    pub name: String,
}

/// 歌手搜索结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchArtistResult {
    /// 当前页歌手列表。
    pub artists: Vec<Artist>,
    /// 是否还有更多结果页可供拉取。
    pub more: bool,
}

/// 跨平台统一歌手模型。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Artist {
    /// 歌手 ID。
    pub id: String,
    /// 歌手名。
    pub name: String,
    /// 歌手头像 URL。
    pub pic_url: String,
}

/// 专辑搜索结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchAlbumResult {
    /// 当前页专辑列表。
    pub albums: Vec<Album>,
    /// 是否还有更多结果页可供拉取。
    pub more: bool,
}

/// 跨平台统一专辑模型。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Album {
    /// 专辑 ID。
    pub id: String,
    /// 专辑名。
    pub name: String,
    /// 专辑封面 URL。
    pub pic_url: String,
    /// 专辑作者信息。
    pub artist: AlbumArtist,
}

/// 专辑关联歌手信息。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlbumArtist {
    /// 歌手 ID。
    pub id: String,
    /// 歌手名。
    pub name: String,
}

/// 歌单搜索结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchPlaylistResult {
    /// 当前页歌单列表。
    pub playlists: Vec<Playlist>,
    /// 是否还有更多结果页可供拉取。
    pub more: bool,
}

/// 跨平台统一歌单概要模型。
///
/// 该结构通常用于搜索与列表场景，[`Playlist::id`] 统一为字符串表示。
/// 详情页结果中的 [`PlaylistDetailResult::id`] 也保持字符串表示。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Playlist {
    /// 歌单 ID（字符串形式，可能来自不同平台的不同编码规则）。
    pub id: String,
    /// 歌单名。
    pub name: String,
    /// 歌单封面 URL。
    pub pic_url: String,
}

/// 搜索建议结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchSuggestResult {
    /// 搜索建议列表。
    pub suggests: Vec<String>,
}

/// 歌曲详情结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SongsDetailResult {
    /// 歌曲列表。
    pub songs: Vec<Song>,
}

/// 歌手详情结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArtistDetailResult {
    /// 歌手 ID。
    pub id: String,
    /// 歌手名。
    pub name: String,
    /// 歌手头像 URL。
    pub pic_url: String,
    /// 歌手简介。
    pub description: String,
    /// 代表歌曲列表。
    pub songs: Vec<Song>,
    /// 是否还有更多歌曲可继续分页拉取。
    pub more: bool,
}

/// 专辑详情结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AlbumDetailResult {
    /// 专辑 ID。
    pub id: String,
    /// 专辑名。
    pub name: String,
    /// 专辑封面 URL。
    pub pic_url: String,
    /// 专辑简介。
    pub description: String,
    /// 专辑歌曲列表。
    pub songs: Vec<Song>,
}

/// 歌单详情结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlaylistDetailResult {
    /// 歌单 ID。
    pub id: String,
    /// 歌单名。
    pub name: String,
    /// 歌单封面 URL。
    pub pic_url: String,
    /// 歌单简介。
    pub description: String,
    /// 歌单歌曲列表。
    pub songs: Vec<Song>,
}

/// 歌词结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LyricResult {
    /// 歌曲 ID。
    pub id: String,
    /// 原文歌词。
    pub lyric: String,
    /// 翻译歌词（若平台提供；缺失时为 [`None`]）。
    pub trans_lyric: Option<String>,
}

/// 播放链接结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UrlResult {
    /// 歌曲 ID。
    pub id: String,
    /// 对应音质等级。
    pub level: SongQuality,
    /// 播放 URL。
    ///
    /// 链接可用性与有效期由上游平台控制，调用方应按需缓存或即时播放。
    pub url: String,
}

/// 热门搜索词结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HotkeyResult {
    /// 热词列表。
    ///
    /// 列表顺序由平台返回决定，通常已按热度排序。
    pub hotkey: Vec<String>,
}

/// 推荐歌单结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecommendPlaylistResult {
    /// 推荐歌单列表。
    pub playlists: Vec<Playlist>,
}

/// 榜单列表结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToplistListResult {
    /// 榜单集合。
    pub toplists: Vec<Toplist>,
}

/// 榜单概要信息。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Toplist {
    /// 榜单 ID。
    pub id: String,
    /// 榜单名称。
    pub name: String,
    /// 榜单封面 URL（部分平台可能为空）。
    pub pic_url: Option<String>,
    /// 榜单前几名歌曲预览（部分平台或榜单类型可能为空）。
    pub tracks: Option<Vec<ToplistTrack>>,
}

/// 榜单歌曲预览条目。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToplistTrack {
    /// 艺术家名称。
    pub artist: String,
    /// 歌曲名称。
    pub title: String,
}

/// 歌单分类结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlaylistCategoriesResult {
    /// 分类分组映射，`group_id -> group_name`。
    pub group: HashMap<u64, String>,
    /// 分类列表。
    pub categories: Vec<PlaylistCategory>,
}

/// 单个歌单分类。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlaylistCategory {
    /// 分类 ID（字符串形式，可直接用于
    /// [`crate::client::DiscoverRequest::playlist_list`] /
    /// [`crate::client::PlaylistRequest::list`] 场景）。
    pub id: String,
    /// 分类名称。
    pub name: String,
    /// 分类所属分组 ID。
    pub category: u64,
}

/// 歌单列表结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlaylistListResult {
    /// 当前页歌单列表。
    pub playlists: Vec<Playlist>,
    /// 当前使用的分类。
    pub category: String,
    /// 是否还有更多结果页可供拉取。
    pub more: bool,
}

/// 登录二维码结果（crate 内部使用）。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LoginQrResult {
    /// 二维码图像内容（通常为 `data:image/png;base64,...`，可直接渲染）。
    pub(crate) qr_code: String,
    /// 二维码 key，用于后续轮询状态。
    pub(crate) qr_key: String,
}

/// 播放音质等级。
///
/// 该枚举会被序列化为平台可识别的字符串值；不同平台对高阶音质支持程度可能不同。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum SongQuality {
    /// 超清母带.
    Master,
    /// 环绕声.
    Surround,
    /// 立体声.
    Stereo,
    /// Hi-Res.
    Hires,
    /// 无损.
    Lossless,
    /// 极高.
    Exhigh,
    /// 标准.
    Standard,
}

/// 二维码登录状态。
///
/// 来自 [`LoginSession::status`][`crate::client::LoginSession::status`] 的轮询结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum LoginStatus {
    /// 登录成功并返回 token。
    Success(LoginToken),
    /// 二维码过期（需重新创建会话）。
    QrCodeExpired,
    /// 等待扫码。
    WaitingScan,
    /// 已扫码，等待用户在客户端确认登录。
    WaitingConfirm,
}

/// 登录 token 的跨平台包装。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum LoginToken {
    /// 网易云登录 token。
    Netease(NeteaseLoginToken),
    /// QQ 音乐登录 token。
    Tencent(TencentLoginToken),
}

/// 网易云登录 token。
///
/// 字段为 crate 内可见，外部请通过构造函数与辅助方法使用。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NeteaseLoginToken {
    pub(crate) music_u: String,
    pub(crate) music_r_u: String,
    pub(crate) csrf: String,
    pub(crate) expires_at: Option<i64>,
}

impl NeteaseLoginToken {
    /// 构造网易云登录 token。
    ///
    /// `expires_at` 为 Unix 时间戳（秒），[`None`] 表示未知过期时间。
    pub fn new(
        music_u: impl Into<String>,
        music_r_u: impl Into<String>,
        csrf: impl Into<String>,
        expires_at: Option<i64>,
    ) -> Self {
        Self { music_u: music_u.into(), music_r_u: music_r_u.into(), csrf: csrf.into(), expires_at }
    }

    /// 返回过期时间戳（秒）。
    pub fn expires_at(&self) -> Option<i64> {
        self.expires_at
    }

    /// 判断 token 是否已过期。
    ///
    /// 当 `expires_at` 为 [`None`] 时返回 `false`。
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expires_at| OffsetDateTime::now_utc().unix_timestamp() >= expires_at)
    }

    /// 生成用于常规请求的 Cookie 字符串。
    ///
    /// 返回值可直接作为 HTTP `Cookie` 请求头值的一部分。
    pub fn to_cookie(&self) -> String {
        format!("MUSIC_U={}; __csrf={}", self.music_u, self.csrf)
    }

    /// 生成用于刷新 token 的 Cookie 字符串。
    ///
    /// 与 [`Self::to_cookie`] 相比会附带 `MUSIC_R_U`。
    pub fn to_refresh_cookie(&self) -> String {
        format!("MUSIC_U={}; MUSIC_R_U={}; __csrf={}", self.music_u, self.music_r_u, self.csrf)
    }
}

/// QQ 音乐登录 token。
///
/// 字段为 crate 内可见，外部请通过构造函数与辅助方法使用。
#[derive(Clone, Deserialize, Serialize)]
pub struct TencentLoginToken {
    pub(crate) music_id: u64,
    pub(crate) music_key: String,
    pub(crate) refresh_token: String,
    pub(crate) refresh_key: String,
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
            refresh_token: refresh_token.into(),
            refresh_key: refresh_key.into(),
            expires_at,
            login_type,
            encrypted_uin: String::new(),
        }
    }

    /// 返回过期时间戳（秒）。
    pub fn expires_at(&self) -> Option<i64> {
        self.expires_at
    }

    pub fn to_cookie(&self) -> String {
        format!(
            "uin={}; qqmusic_key={}; qm_keyst={}; tmeLoginType={}",
            self.music_id, self.music_key, self.music_key, self.login_type,
        )
    }
}
