//! 网易云平台原始响应模型与统一模型转换。
//!
//! 本模块仅承载网易云接口响应结构体与 [`Into`] 转换逻辑；跨平台统一模型定义见
//! [`crate::models`]。

use std::collections::HashMap;
use std::io::Cursor;

use base64::Engine as _;
use image::{DynamicImage, ImageFormat, Luma};
use qrcode::QrCode;
use serde::Deserialize;

use super::super::collect_into;
use crate::error::{MusicClientError, MusicClientResult};
use crate::models::*;

/// `/api/cloudsearch/pc`（单曲搜索）响应体。
#[derive(Deserialize)]
pub(super) struct NSearchSongResponse {
    result: NSearchSongData,
}

#[derive(Deserialize)]
struct NSearchSongData {
    songs: Vec<NSong>,
    #[serde(rename = "songCount", alias = "total")]
    song_count: u64,
    more: Option<bool>,
}

#[derive(Deserialize)]
struct NSong {
    id: u64,
    name: String,
    #[serde(rename = "ar", alias = "artists")]
    artists: Vec<NSongArtist>,
    #[serde(rename = "al", alias = "album")]
    album: NSongAlbum,
}

#[derive(Deserialize)]
struct NSongArtist {
    id: u64,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NSongAlbum {
    id: u64,
    name: String,
    pic_url: String,
}

impl NSearchSongResponse {
    pub(super) fn into_with(self, end: u64) -> SearchSongResult {
        SearchSongResult {
            songs: collect_into(self.result.songs),
            more: self.result.song_count > end,
        }
    }
}

impl From<NSong> for Song {
    fn from(value: NSong) -> Self {
        Self {
            id: value.id.to_string(),
            name: value.name,
            artists: value
                .artists
                .into_iter()
                .map(|artist| SongArtist { id: artist.id.to_string(), name: artist.name })
                .collect(),
            album: SongAlbum { id: value.album.id.to_string(), name: value.album.name },
            pic_url: value.album.pic_url,
        }
    }
}

/// `/api/v1/search/artist/get`（歌手搜索）响应体。
#[derive(Deserialize)]
pub(super) struct NSearchArtistResponse {
    result: NSearchArtistData,
}

#[derive(Deserialize)]
struct NSearchArtistData {
    artists: Vec<NArtist>,
    #[serde(rename = "hasMore")]
    more: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NArtist {
    id: u64,
    name: String,
    img1v1_url: String,
}

impl From<NSearchArtistResponse> for SearchArtistResult {
    fn from(value: NSearchArtistResponse) -> Self {
        Self { artists: collect_into(value.result.artists), more: value.result.more }
    }
}

impl From<NArtist> for Artist {
    fn from(value: NArtist) -> Self {
        Self { id: value.id.to_string(), name: value.name, pic_url: value.img1v1_url }
    }
}

/// `/api/v1/search/album/get`（专辑搜索）响应体。
#[derive(Deserialize)]
pub(super) struct NSearchAlbumResponse {
    result: NSearchAlbumData,
}

#[derive(Deserialize)]
struct NSearchAlbumData {
    albums: Vec<NAlbum>,
    #[serde(rename = "albumCount")]
    album_count: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NAlbum {
    id: u64,
    name: String,
    pic_url: String,
    artist: NAlbumArtist,
}

#[derive(Deserialize)]
struct NAlbumArtist {
    id: u64,
    name: String,
}

impl NSearchAlbumResponse {
    pub(super) fn into_with(self, end: u64) -> SearchAlbumResult {
        SearchAlbumResult {
            albums: collect_into(self.result.albums),
            more: self.result.album_count > end,
        }
    }
}

impl From<NAlbum> for Album {
    fn from(value: NAlbum) -> Self {
        Self {
            id: value.id.to_string(),
            name: value.name,
            pic_url: value.pic_url,
            artist: AlbumArtist { id: value.artist.id.to_string(), name: value.artist.name },
        }
    }
}

/// `/api/v1/search/playlist/get`（歌单搜索）响应体。
#[derive(Deserialize)]
pub(super) struct NSearchPlaylistResponse {
    result: NSearchPlaylistData,
}

#[derive(Deserialize)]
struct NSearchPlaylistData {
    playlists: Vec<NPlaylist>,
    #[serde(rename = "hasMore")]
    more: bool,
}

#[derive(Deserialize)]
struct NPlaylist {
    id: u64,
    name: String,
    #[serde(rename = "coverImgUrl")]
    pic_url: String,
}

impl From<NSearchPlaylistResponse> for SearchPlaylistResult {
    fn from(value: NSearchPlaylistResponse) -> Self {
        Self { playlists: collect_into(value.result.playlists), more: value.result.more }
    }
}

impl From<NPlaylist> for Playlist {
    fn from(value: NPlaylist) -> Self {
        Self { id: value.id.to_string(), name: value.name, pic_url: value.pic_url }
    }
}

/// `/api/search/pc/suggest/keyword/get`（搜索建议) 响应体。
#[derive(Deserialize)]
pub(super) struct NSearchSuggestResponse {
    data: NSearchSuggestData,
}

#[derive(Deserialize)]
struct NSearchSuggestData {
    suggests: Vec<NSearchSuggestItem>,
}

#[derive(Deserialize)]
struct NSearchSuggestItem {
    keyword: String,
}

impl From<NSearchSuggestResponse> for SearchSuggestResult {
    fn from(value: NSearchSuggestResponse) -> Self {
        Self { suggests: value.data.suggests.into_iter().map(|item| item.keyword).collect() }
    }
}

/// `/api/batch`（歌手详情与歌曲列表）聚合响应体。
#[derive(Deserialize)]
pub(super) struct NArtistDetailResponse {
    #[serde(rename = "/api/artist/head/info/get")]
    detail: NArtistInfoResponse,
    #[serde(rename = "/api/v2/artist/songs")]
    song_result: NSearchSongData,
}

/// `/api/batch` 中 `/api/artist/head/info/get`（歌手详情） 子响应体。
#[derive(Deserialize)]
pub(super) struct NArtistInfoResponse {
    data: NArtistInfoData,
}

#[derive(Deserialize)]
struct NArtistInfoData {
    artist: NArtistProfile,
}

#[derive(Deserialize)]
struct NArtistProfile {
    id: u64,
    name: String,
    #[serde(rename = "cover")]
    pic_url: String,
    #[serde(rename = "briefDesc")]
    description: String,
}

impl From<NArtistDetailResponse> for ArtistDetailResult {
    fn from(value: NArtistDetailResponse) -> Self {
        Self {
            id: value.detail.data.artist.id.to_string(),
            name: value.detail.data.artist.name,
            pic_url: value.detail.data.artist.pic_url,
            description: value.detail.data.artist.description,
            songs: collect_into(value.song_result.songs),
            more: value.song_result.more.unwrap_or(false),
        }
    }
}

/// `/api/album/v3/detail`（专辑详情）响应体。
#[derive(Deserialize)]
pub(super) struct NAlbumDetailResponse {
    songs: Vec<NSong>,
    album: NAlbumProfile,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NAlbumProfile {
    id: u64,
    name: String,
    pic_url: String,
    description: String,
}

impl From<NAlbumDetailResponse> for AlbumDetailResult {
    fn from(value: NAlbumDetailResponse) -> Self {
        Self {
            id: value.album.id.to_string(),
            name: value.album.name,
            pic_url: value.album.pic_url,
            description: value.album.description,
            songs: collect_into(value.songs),
        }
    }
}

/// `/api/v6/playlist/detail`（歌单详情）响应体。
#[derive(Deserialize)]
pub(super) struct NPlaylistDetailResponse {
    playlist: NPlaylistDetailData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NPlaylistDetailData {
    id: u64,
    name: String,
    cover_img_url: String,
    description: String,
    track_ids: Vec<NTrackId>,
}

#[derive(Deserialize)]
struct NTrackId {
    id: u64,
}

impl NPlaylistDetailResponse {
    pub(super) fn song_ids(&self) -> Vec<String> {
        self.playlist.track_ids.iter().map(|item| item.id.to_string()).collect()
    }

    pub(super) fn into_with_songs(self, songs: Vec<Song>) -> PlaylistDetailResult {
        PlaylistDetailResult {
            id: self.playlist.id.to_string(),
            name: self.playlist.name,
            pic_url: self.playlist.cover_img_url,
            description: self.playlist.description,
            songs,
        }
    }
}

/// `/api/v3/song/detail`（歌曲详情批量查询）响应体。
#[derive(Deserialize)]
pub(super) struct NSongDetailResponse {
    songs: Vec<NSong>,
}

impl NSongDetailResponse {
    pub(super) fn into_with(self) -> SongsDetailResult {
        SongsDetailResult { songs: collect_into(self.songs) }
    }
}

/// `/api/song/lyric/v1`（歌词查询）响应体。
#[derive(Deserialize)]
pub(super) struct NLyricResponse {
    lrc: NLyricData,
    tlyric: Option<NLyricData>,
}

#[derive(Deserialize)]
struct NLyricData {
    lyric: String,
}

impl NLyricResponse {
    pub(super) fn into_with(self, id: &str) -> LyricResult {
        let lyric = super::super::normalize_timestamp_lyric(self.lrc.lyric);
        let trans_lyric = super::super::normalize_timestamp_lyric(
            self.tlyric.map_or("".to_string(), |item| item.lyric),
        );
        LyricResult {
            id: id.to_string(),
            lyric,
            trans_lyric: if trans_lyric.is_empty() { None } else { Some(trans_lyric) },
        }
    }
}

/// `/api/song/enhance/player/url/v1`（播放链接查询）响应体。
#[derive(Deserialize)]
pub(super) struct NUrlResponse {
    data: Vec<NUrlData>,
}

#[derive(Deserialize)]
struct NUrlData {
    id: u64,
    url: Option<String>,
    level: Option<String>,
}

impl From<NUrlResponse> for UrlResult {
    fn from(value: NUrlResponse) -> Self {
        match value.data.into_iter().next() {
            Some(item) => UrlResult {
                id: item.id.to_string(),
                url: item.url.unwrap_or_default(),
                level: match item.level.as_deref() {
                    Some("jymaster") => SongQuality::Master,
                    Some("sky") => SongQuality::Surround,
                    Some("jyeffect") => SongQuality::Stereo,
                    Some("hires") => SongQuality::Hires,
                    Some("lossless") => SongQuality::Lossless,
                    Some("exhigh") => SongQuality::Exhigh,
                    Some("standard") | Some(_) | None => SongQuality::Standard,
                },
            },
            None => {
                UrlResult { id: 0.to_string(), url: String::new(), level: SongQuality::Standard }
            }
        }
    }
}

/// `/api/search/pc/chart/detail`（热搜词）响应体。
#[derive(Deserialize)]
pub(super) struct NHotkeyResponse {
    data: NHotkeyData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NHotkeyData {
    item_list: Vec<NHotkeyItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NHotkeyItem {
    search_word: String,
}

impl From<NHotkeyResponse> for HotkeyResult {
    fn from(value: NHotkeyResponse) -> Self {
        Self { hotkey: value.data.item_list.into_iter().map(|item| item.search_word).collect() }
    }
}

/// `/api/link/page/rcmd/resource/show`（推荐歌单）响应体。
#[derive(Deserialize)]
pub(super) struct NRecommendPlaylistResponse {
    data: NRecommendPlaylistData,
}

#[derive(Deserialize)]
struct NRecommendPlaylistData {
    blocks: Vec<NRecommendPlaylistBlock>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NRecommendPlaylistBlock {
    dsl_data: NRecommendPlaylistDsl,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NRecommendPlaylistDsl {
    block_resource: NRecommendPlaylistBlockResource,
}

#[derive(Deserialize)]
struct NRecommendPlaylistBlockResource {
    resources: Vec<NRecommendPlaylistResource>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NRecommendPlaylistResource {
    resource_id: String,
    title: String,
    cover_img: String,
}

impl From<NRecommendPlaylistResponse> for RecommendPlaylistResult {
    fn from(value: NRecommendPlaylistResponse) -> Self {
        Self {
            playlists: value
                .data
                .blocks
                .into_iter()
                .flat_map(|block| block.dsl_data.block_resource.resources)
                .map(Into::into)
                .collect(),
        }
    }
}

impl From<NRecommendPlaylistResource> for Playlist {
    fn from(value: NRecommendPlaylistResource) -> Self {
        Self { id: value.resource_id, name: value.title, pic_url: value.cover_img }
    }
}

/// `/api/toplist/detail/v2`（榜单列表）响应体。
#[derive(Deserialize)]
pub(super) struct NToplistResponse {
    data: Vec<NToplistData>,
}

#[derive(Deserialize)]
struct NToplistData {
    list: Vec<NToplist>,
}

#[derive(Deserialize)]
struct NToplist {
    id: u64,
    name: String,
    #[serde(rename = "coverUrl")]
    pic_url: Option<String>,
    tracks: Option<Vec<NToplistTrack>>,
}

#[derive(Deserialize)]
struct NToplistTrack {
    first: String,
    second: Option<String>,
}

impl From<NToplistResponse> for ToplistListResult {
    fn from(value: NToplistResponse) -> Self {
        let mut data = value.data;
        if data.len() >= 2 {
            let first = data.remove(0);
            if let Some(second) = data.first_mut() {
                second.list.extend(first.list);
            }
        }
        Self { toplists: data.into_iter().flat_map(|datum| datum.list).map(Into::into).collect() }
    }
}

impl From<NToplist> for Toplist {
    fn from(value: NToplist) -> Self {
        Self {
            id: value.id.to_string(),
            name: value.name,
            pic_url: value.pic_url,
            tracks: value.tracks.map(collect_into),
        }
    }
}

impl From<NToplistTrack> for ToplistTrack {
    fn from(value: NToplistTrack) -> Self {
        Self { artist: value.second.unwrap_or_default(), title: value.first }
    }
}

/// `/api/playlist/catalogue`（歌单分类）响应体。
#[derive(Deserialize)]
pub(super) struct NPlaylistCategoriesResponse {
    categories: HashMap<u64, String>,
    all: NPlaylistCategory,
    sub: Vec<NPlaylistCategory>,
}

#[derive(Deserialize)]
struct NPlaylistCategory {
    name: String,
    category: u64,
}

impl From<NPlaylistCategoriesResponse> for PlaylistCategoriesResult {
    fn from(value: NPlaylistCategoriesResponse) -> Self {
        let mut categories = Vec::with_capacity(value.sub.len() + 1);
        categories.push(PlaylistCategory::from(value.all));
        categories.extend(value.sub.into_iter().map(PlaylistCategory::from));
        Self { group: value.categories, categories }
    }
}

impl From<NPlaylistCategory> for PlaylistCategory {
    fn from(value: NPlaylistCategory) -> Self {
        Self { id: value.name.clone(), name: value.name, category: value.category }
    }
}

/// `/api/playlist/list`（分类歌单列表）响应体。
#[derive(Deserialize)]
pub(super) struct NPlaylistListResponse {
    cat: String,
    more: bool,
    playlists: Vec<NPlaylist>,
}

impl From<NPlaylistListResponse> for PlaylistListResult {
    fn from(value: NPlaylistListResponse) -> Self {
        Self { more: value.more, category: value.cat, playlists: collect_into(value.playlists) }
    }
}

/// `/api/login/qrcode/unikey`（二维码登录 key）响应体。
#[derive(Deserialize)]
pub(super) struct NLoginQrResponse {
    unikey: String,
}

impl NLoginQrResponse {
    pub(super) fn into_qr_result(self) -> MusicClientResult<LoginQrResult> {
        let code = QrCode::new(format!("https://music.163.com/login?codekey={}", self.unikey))
            .map_err(|err| MusicClientError::NeteaseLoginQrCode(err.to_string()))?;
        let image = code.render::<Luma<u8>>().quiet_zone(true).module_dimensions(8, 8).build();
        let mut png_bytes = Vec::new();
        DynamicImage::ImageLuma8(image)
            .write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)
            .map_err(|err| MusicClientError::NeteaseLoginQrCode(err.to_string()))?;
        let qr_code = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png_bytes)
        );
        Ok(LoginQrResult { qr_key: self.unikey, qr_code })
    }
}

/// `/api/login/qrcode/client/login`（二维码登录状态轮询）响应体。
#[derive(Deserialize)]
pub(super) struct NLoginTokenResponse {
    pub(super) code: u16,
}
