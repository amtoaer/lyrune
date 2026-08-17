//! QQ 音乐平台原始响应模型与统一模型转换。
//!
//! 本模块仅承载 QQ 音乐接口响应结构体与 [`Into`] 转换逻辑；跨平台统一模型定义见
//! [`crate::models`]。

use std::collections::HashMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use time::OffsetDateTime;

use super::super::collect_into;
use crate::error::{MusicClientError, MusicClientResult};
use crate::models::*;

fn to_optional_string(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

fn decode_base64_text(value: String) -> String {
    // 歌词字段通常是 base64；解码失败时保留原文，避免把异常数据吞成空字符串。
    BASE64
        .decode(value.as_bytes())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or(value)
}

fn tencent_album_pic_url(pmid: &str) -> String {
    format!("https://y.gtimg.cn/music/photo_new/T002R300x300M000{pmid}.jpg")
}

fn tencent_singer_pic_url(singer_pmid: &str) -> String {
    format!("https://y.gtimg.cn/music/photo_new/T001R300x300M000{singer_pmid}.jpg")
}

fn tencent_playback_url(purl: &str) -> String {
    let purl = purl.trim();
    if purl.is_empty() {
        return String::new();
    }

    if purl.starts_with("http://") || purl.starts_with("https://") {
        return purl.to_string();
    }

    format!(
        "https://isure.stream.qqmusic.qq.com/{}",
        purl.trim_start_matches('/')
    )
}

fn parse_song_quality(filename: &str) -> SongQuality {
    // 前缀由高到低匹配，与 get_url 里请求的候选文件名约定保持一致。
    if filename.starts_with("AI00") {
        SongQuality::Master
    } else if filename.starts_with("Q001") {
        SongQuality::Surround
    } else if filename.starts_with("Q000") {
        SongQuality::Stereo
    } else if filename.starts_with("F000") {
        SongQuality::Lossless
    } else if filename.starts_with("M800") {
        SongQuality::Exhigh
    } else {
        SongQuality::Standard
    }
}

#[derive(Deserialize)]
struct TSong {
    mid: String,
    name: String,
    singer: Vec<TSongSinger>,
    album: TSongAlbum,
}

#[derive(Deserialize)]
struct TSongSinger {
    mid: String,
    name: String,
}

impl From<TSongSinger> for SongArtist {
    fn from(value: TSongSinger) -> Self {
        Self {
            id: value.mid,
            name: value.name,
        }
    }
}

#[derive(Deserialize)]
struct TSongAlbum {
    mid: String,
    name: String,
    pmid: String,
}

impl From<TSong> for Song {
    fn from(value: TSong) -> Self {
        Self {
            id: value.mid,
            name: value.name,
            artists: collect_into(value.singer),
            album: SongAlbum {
                id: value.album.mid,
                name: value.album.name,
            },
            pic_url: tencent_album_pic_url(value.album.pmid.as_str()),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TSinger {
    #[serde(rename = "singerMID")]
    singer_mid: String,
    singer_name: String,
    singer_pic: String,
}

impl From<TSinger> for Artist {
    fn from(value: TSinger) -> Self {
        Self {
            id: value.singer_mid,
            name: value.singer_name,
            pic_url: value.singer_pic,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TAlbum {
    #[serde(rename = "albumMID")]
    album_mid: String,
    album_name: String,
    album_pic: String,
    #[serde(rename = "singerMID")]
    singer_mid: String,
    singer_name: String,
}

impl From<TAlbum> for Album {
    fn from(value: TAlbum) -> Self {
        Self {
            id: value.album_mid,
            name: value.album_name,
            pic_url: value.album_pic,
            artist: AlbumArtist {
                id: value.singer_mid,
                name: value.singer_name,
            },
        }
    }
}

#[derive(Deserialize)]
struct TSonglist {
    dissid: String,
    dissname: String,
    imgurl: String,
}

impl From<TSonglist> for Playlist {
    fn from(value: TSonglist) -> Self {
        Self {
            id: value.dissid,
            name: value.dissname,
            pic_url: value.imgurl,
        }
    }
}

/// `music.search.SearchCgiService.DoSearchForQQMusicDesktop`（搜索）响应体。
#[derive(Deserialize)]
pub(super) struct TSearchResponse {
    result: TSearchResult,
}

#[derive(Deserialize)]
struct TSearchResult {
    data: TSearchData,
}

#[derive(Deserialize)]
struct TSearchData {
    body: TSearchBody,
    meta: TSearchMeta,
}

#[derive(Deserialize)]
struct TSearchBody {
    song: TSearchSongList,
    singer: TSearchSingerList,
    album: TSearchAlbumList,
    songlist: TSearchSonglistList,
}

#[derive(Deserialize)]
struct TSearchSongList {
    list: Vec<TSong>,
}

#[derive(Deserialize)]
struct TSearchSingerList {
    list: Vec<TSinger>,
}

#[derive(Deserialize)]
struct TSearchAlbumList {
    list: Vec<TAlbum>,
}

#[derive(Deserialize)]
struct TSearchSonglistList {
    list: Vec<TSonglist>,
}

#[derive(Deserialize)]
struct TSearchMeta {
    nextpage: i64,
}

impl From<TSearchResponse> for SearchSongResult {
    fn from(value: TSearchResponse) -> Self {
        let data = value.result.data;
        Self {
            songs: collect_into(data.body.song.list),
            more: data.meta.nextpage > 0,
        }
    }
}

impl From<TSearchResponse> for SearchArtistResult {
    fn from(value: TSearchResponse) -> Self {
        let data = value.result.data;
        Self {
            artists: collect_into(data.body.singer.list),
            more: data.meta.nextpage > 0,
        }
    }
}

impl From<TSearchResponse> for SearchAlbumResult {
    fn from(value: TSearchResponse) -> Self {
        let data = value.result.data;
        Self {
            albums: collect_into(data.body.album.list),
            more: data.meta.nextpage > 0,
        }
    }
}

impl From<TSearchResponse> for SearchPlaylistResult {
    fn from(value: TSearchResponse) -> Self {
        let data = value.result.data;
        Self {
            playlists: collect_into(data.body.songlist.list),
            more: data.meta.nextpage > 0,
        }
    }
}

/// 搜索建议请求（`music.smartboxCgi.SmartBoxCgi.GetSmartBoxResult`）响应体。
#[derive(Deserialize)]
pub(super) struct TSearchSuggestResponse {
    result: TSearchSuggestResult,
}

#[derive(Deserialize)]
struct TSearchSuggestResult {
    data: TSearchSuggestData,
}

#[derive(Deserialize)]
struct TSearchSuggestData {
    items: Vec<TSearchSuggestItem>,
}

#[derive(Deserialize)]
struct TSearchSuggestItem {
    hint: String,
}

impl From<TSearchSuggestResponse> for SearchSuggestResult {
    fn from(value: TSearchSuggestResponse) -> Self {
        Self {
            suggests: value
                .result
                .data
                .items
                .into_iter()
                .map(|item| item.hint)
                .collect(),
        }
    }
}

/// 歌曲详情请求（`music.trackInfo.UniformRuleCtrl`）响应体。
#[derive(Deserialize)]
pub(super) struct TSongsDetailResponse {
    result: TSongsDetailResult,
}

#[derive(Deserialize)]
struct TSongsDetailResult {
    data: TSongsDetailData,
}

#[derive(Deserialize)]
struct TSongsDetailData {
    tracks: Vec<TSong>,
}

impl From<TSongsDetailResponse> for SongsDetailResult {
    fn from(value: TSongsDetailResponse) -> Self {
        Self {
            songs: collect_into(value.result.data.tracks),
        }
    }
}

/// 歌手详情聚合请求（`SingerInfoInter.GetSingerDetail` +
/// `SongListInter.GetSingerSongList`）响应体。
#[derive(Deserialize)]
pub(super) struct TSingerDetailResponse {
    singer_info: TSingerInfo,
    singer_songs: TSingerSongs,
}

#[derive(Deserialize)]
struct TSingerInfo {
    data: TSingerInfoData,
}

#[derive(Deserialize)]
struct TSingerInfoData {
    singer_list: Vec<TSingerInfoItem>,
}

#[derive(Deserialize)]
struct TSingerInfoItem {
    basic_info: TSingerBasicInfo,
    ex_info: TSingerExInfo,
    pic: Option<TSingerPic>,
}

#[derive(Deserialize)]
struct TSingerPic {
    pic: String,
}

#[derive(Deserialize)]
struct TSingerBasicInfo {
    singer_mid: String,
    name: String,
    singer_pmid: String,
}

#[derive(Deserialize)]
struct TSingerExInfo {
    desc: String,
}

#[derive(Deserialize)]
struct TSingerSongs {
    data: TSingerSongsData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TSingerSongsData {
    total_num: u64,
    song_list: Vec<TSingerSongItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TSingerSongItem {
    song_info: TSong,
}

impl From<TSingerSongItem> for Song {
    fn from(value: TSingerSongItem) -> Self {
        value.song_info.into()
    }
}

impl TSingerDetailResponse {
    pub(super) fn into_with(self, end: u64) -> ArtistDetailResult {
        let TSingerDetailResponse {
            singer_info,
            singer_songs,
        } = self;

        let total_num = singer_songs.data.total_num;
        let songs = collect_into(singer_songs.data.song_list);

        match singer_info.data.singer_list.into_iter().next() {
            Some(item) => {
                let basic = item.basic_info;
                let pic_url = item
                    .pic
                    .and_then(|pic| to_optional_string(pic.pic))
                    .unwrap_or_else(|| tencent_singer_pic_url(basic.singer_pmid.as_str()));

                ArtistDetailResult {
                    id: basic.singer_mid,
                    name: basic.name,
                    pic_url,
                    description: item.ex_info.desc,
                    songs,
                    more: total_num > end,
                }
            }
            None => ArtistDetailResult {
                id: String::new(),
                name: String::new(),
                pic_url: String::new(),
                description: String::new(),
                songs,
                more: total_num > end,
            },
        }
    }
}

/// 专辑详情聚合请求（`AlbumInfoServer.GetAlbumDetail` + `AlbumSongList.GetAlbumSongList`）响应体。
#[derive(Deserialize)]
pub(super) struct TAlbumDetailResponse {
    #[serde(rename = "req_1", alias = "album_info")]
    album_info: TAlbumInfo,
    #[serde(rename = "req_2", alias = "album_songs")]
    album_songs: TAlbumSongs,
}

#[derive(Deserialize)]
struct TAlbumInfo {
    data: TAlbumInfoData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TAlbumInfoData {
    basic_info: TAlbumBasicInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TAlbumBasicInfo {
    album_mid: String,
    album_name: String,
    pmid: String,
    desc: String,
}

#[derive(Deserialize)]
struct TAlbumSongs {
    data: TAlbumSongsData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TAlbumSongsData {
    song_list: Vec<TSingerSongItem>,
}

impl From<TAlbumDetailResponse> for AlbumDetailResult {
    fn from(value: TAlbumDetailResponse) -> Self {
        let basic = value.album_info.data.basic_info;
        Self {
            id: basic.album_mid,
            name: basic.album_name,
            pic_url: tencent_album_pic_url(basic.pmid.as_str()),
            description: basic.desc,
            songs: collect_into(value.album_songs.data.song_list),
        }
    }
}

/// `music.srfDissInfo.aiDissInfo.uniform_get_Dissinfo`（歌单详情）响应体。
#[derive(Deserialize)]
pub(super) struct TSonglistDetailResponse {
    result: TSonglistDetailResult,
}

#[derive(Deserialize)]
struct TSonglistDetailResult {
    data: TSonglistDetailData,
}

#[derive(Deserialize)]
struct TSonglistDetailData {
    dirinfo: TSonglistDetailInfo,
    songlist: Vec<TSong>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TSonglistDetailInfo {
    id: u64,
    title: String,
    #[serde(rename = "picurl", alias = "picUrl")]
    pic_url: String,
    desc: String,
}

impl From<TSonglistDetailResponse> for PlaylistDetailResult {
    fn from(value: TSonglistDetailResponse) -> Self {
        Self {
            id: value.result.data.dirinfo.id.to_string(),
            name: value.result.data.dirinfo.title,
            pic_url: value.result.data.dirinfo.pic_url,
            description: value.result.data.dirinfo.desc,
            songs: collect_into(value.result.data.songlist),
        }
    }
}

/// `music.musichallSong.PlayLyricInfo.GetPlayLyricInfo`（歌词）响应体。
#[derive(Deserialize)]
pub(super) struct TLyricResponse {
    result: TLyricResult,
}

#[derive(Deserialize)]
struct TLyricResult {
    data: TLyricData,
}

#[derive(Deserialize)]
struct TLyricData {
    lyric: String,
    trans: Option<String>,
}

impl TLyricResponse {
    pub(super) fn into_with(self, id: &str) -> LyricResult {
        let lyric =
            super::super::normalize_timestamp_lyric(decode_base64_text(self.result.data.lyric));
        let trans_lyric = self
            .result
            .data
            .trans
            .and_then(to_optional_string)
            .map(decode_base64_text)
            .map(super::super::normalize_timestamp_lyric)
            .and_then(to_optional_string);
        LyricResult {
            id: id.to_string(),
            lyric,
            trans_lyric,
            roma_lyric: None,
        }
    }
}

/// `music.vkey.GetVkey.UrlGetVkey`（播放链接）响应体。
#[derive(Deserialize)]
pub(super) struct TUrlResponse {
    result: TUrlResult,
}

#[derive(Deserialize)]
struct TUrlResult {
    data: TUrlData,
}

#[derive(Deserialize)]
struct TUrlData {
    midurlinfo: Vec<TMidUrlInfo>,
}

#[derive(Deserialize)]
struct TMidUrlInfo {
    songmid: String,
    purl: Option<String>,
    filename: String,
}

impl From<TUrlResponse> for UrlResult {
    fn from(value: TUrlResponse) -> Self {
        let mut midurlinfo = value.result.data.midurlinfo.into_iter();
        let first = midurlinfo.next();

        // 优先使用首项：请求顺序已经按期望音质排序，首项命中即可直接返回。
        if let Some(item) = first
            .as_ref()
            .filter(|item| item.purl.as_deref().is_some_and(|purl| !purl.is_empty()))
        {
            return Self {
                id: item.songmid.clone(),
                level: parse_song_quality(item.filename.as_str()),
                url: item
                    .purl
                    .as_deref()
                    .map(tencent_playback_url)
                    .unwrap_or_default(),
            };
        }

        if let Some(item) =
            midurlinfo.find(|item| item.purl.as_deref().is_some_and(|p| !p.is_empty()))
        {
            return Self {
                id: item.songmid,
                level: parse_song_quality(item.filename.as_str()),
                url: item
                    .purl
                    .as_deref()
                    .map(tencent_playback_url)
                    .unwrap_or_default(),
            };
        }

        match first {
            Some(item) => Self {
                id: item.songmid,
                level: parse_song_quality(item.filename.as_str()),
                url: item
                    .purl
                    .as_deref()
                    .map(tencent_playback_url)
                    .unwrap_or_default(),
            },
            None => Self {
                id: "0".to_string(),
                level: SongQuality::Standard,
                url: String::new(),
            },
        }
    }
}

/// `tencent_musicsoso_hotkey.HotkeyService.GetHotkeyForQQMusicPC`（热搜词）响应体。
#[derive(Deserialize)]
pub(super) struct THotkeyResponse {
    result: THotkeyResult,
}

#[derive(Deserialize)]
struct THotkeyResult {
    data: THotkeyData,
}

#[derive(Deserialize)]
struct THotkeyData {
    vec_hotkey: Vec<THotkeyItem>,
}

#[derive(Deserialize)]
struct THotkeyItem {
    query: String,
}

impl From<THotkeyResponse> for HotkeyResult {
    fn from(value: THotkeyResponse) -> Self {
        Self {
            hotkey: value
                .result
                .data
                .vec_hotkey
                .into_iter()
                .take(20)
                .map(|item| item.query)
                .collect(),
        }
    }
}

/// `music.recommend.RecommendFeed.get_recommend_feed`（推荐歌单）响应体。
#[derive(Deserialize)]
pub(super) struct TRecommendSonglistResponse {
    result: TRecommendSonglistResult,
}

#[derive(Deserialize)]
struct TRecommendSonglistResult {
    data: TRecommendSonglistData,
}

#[derive(Deserialize)]
struct TRecommendSonglistData {
    v_shelf: Vec<TRecommendSonglistShelf>,
}

#[derive(Deserialize)]
struct TRecommendSonglistShelf {
    v_niche: [TRecommendSonglistNiche; 1],
}

#[derive(Deserialize)]
struct TRecommendSonglistNiche {
    v_card: [TRecommendSonglistCard; 1],
}

#[derive(Deserialize)]
struct TRecommendSonglistCard {
    id: String,
    title: String,
    cover: String,
}

impl From<TRecommendSonglistCard> for Playlist {
    fn from(value: TRecommendSonglistCard) -> Self {
        Self {
            id: value.id,
            name: value.title,
            pic_url: value.cover,
        }
    }
}

impl From<TRecommendSonglistResponse> for RecommendPlaylistResult {
    fn from(value: TRecommendSonglistResponse) -> Self {
        Self {
            playlists: value
                .result
                .data
                .v_shelf
                .into_iter()
                .take(6)
                .map(|shelf| {
                    let [niche] = shelf.v_niche;
                    let [card] = niche.v_card;
                    card.into()
                })
                .collect(),
        }
    }
}

/// `music.musicToplist.Toplist.GetAll`（榜单列表）响应体。
#[derive(Deserialize)]
pub(super) struct TToplistResponse {
    result: TToplistResult,
}

#[derive(Deserialize)]
struct TToplistResult {
    data: TToplistData,
}

#[derive(Deserialize)]
struct TToplistData {
    group: Vec<TToplistGroup>,
}

#[derive(Deserialize)]
struct TToplistGroup {
    toplist: Vec<TToplistItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TToplistItem {
    top_id: u64,
    title: String,
    intro: String,
    head_pic_url: String,
    song: Vec<TToplistSong>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TToplistSong {
    title: String,
    singer_name: String,
}

impl From<TToplistSong> for ToplistTrack {
    fn from(value: TToplistSong) -> Self {
        Self {
            artist: value.singer_name,
            title: value.title,
        }
    }
}

impl From<TToplistItem> for Toplist {
    fn from(value: TToplistItem) -> Self {
        Self {
            id: value.top_id.to_string(),
            name: value.title,
            pic_url: to_optional_string(value.head_pic_url),
            tracks: if value.song.is_empty() {
                None
            } else {
                Some(collect_into(value.song))
            },
        }
    }
}

impl From<TToplistResponse> for ToplistListResult {
    fn from(value: TToplistResponse) -> Self {
        Self {
            toplists: value
                .result
                .data
                .group
                .into_iter()
                .flat_map(|group| group.toplist)
                .map(Into::into)
                .collect(),
        }
    }
}

/// `music.musicToplist.Toplist.GetDetail`（榜单详情）响应体。
#[derive(Deserialize)]
pub(super) struct TToplistDetailResponse {
    result: TToplistDetailResult,
}

#[derive(Deserialize)]
struct TToplistDetailResult {
    data: TToplistDetailData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TToplistDetailData {
    data: TToplistItem,
    song_info_list: Vec<TSong>,
}

impl From<TToplistDetailResponse> for PlaylistDetailResult {
    fn from(value: TToplistDetailResponse) -> Self {
        let detail = value.result.data;
        Self {
            id: detail.data.top_id.to_string(),
            name: detail.data.title,
            pic_url: detail.data.head_pic_url,
            description: detail.data.intro,
            songs: collect_into(detail.song_info_list),
        }
    }
}

/// `music.playlist.PlaylistSquare.GetAllTag`（歌单分类）响应体。
#[derive(Deserialize)]
pub(super) struct TSonglistCategoriesResponse {
    result: TSonglistCategoriesResult,
}

#[derive(Deserialize)]
struct TSonglistCategoriesResult {
    data: TSonglistCategoriesData,
}

#[derive(Deserialize)]
struct TSonglistCategoriesData {
    v_group: Vec<TSonglistCategoriesGroup>,
}

#[derive(Deserialize)]
struct TSonglistCategoriesGroup {
    group_id: u64,
    group_name: String,
    v_item: Vec<TSonglistCategoriesItem>,
}

#[derive(Deserialize)]
struct TSonglistCategoriesItem {
    id: u64,
    name: String,
}

impl From<TSonglistCategoriesResponse> for PlaylistCategoriesResult {
    fn from(value: TSonglistCategoriesResponse) -> Self {
        const TENCENT_AI_PLAYLIST_CATEGORY_ID: u64 = 9527;
        let mut group = HashMap::new();
        let mut categories = Vec::new();

        for category_group in value.result.data.v_group {
            let category = category_group.group_id;
            group.insert(category, category_group.group_name);
            categories.extend(
                category_group
                    .v_item
                    .into_iter()
                    .filter(|item| item.id != TENCENT_AI_PLAYLIST_CATEGORY_ID)
                    .map(|item| PlaylistCategory {
                        id: item.id.to_string(),
                        name: item.name,
                        category,
                    }),
            );
        }

        Self { group, categories }
    }
}

/// `playlist.PlayListCategoryServer.get_category_content`（分类歌单列表）响应体。
#[derive(Deserialize)]
pub(super) struct TSonglistListResponse {
    result: TSonglistListResult,
}

#[derive(Deserialize)]
struct TSonglistListResult {
    data: TSonglistListData,
}

#[derive(Deserialize)]
struct TSonglistListData {
    content: TSonglistListContent,
}

#[derive(Deserialize)]
struct TSonglistListContent {
    total_cnt: u64,
    v_item: Vec<TSonglistListItem>,
}

#[derive(Deserialize)]
struct TSonglistListItem {
    basic: TSonglistInfo,
}

#[derive(Deserialize)]
struct TSonglistInfo {
    tid: u64,
    title: String,
    cover: TSonglistCover,
}

#[derive(Deserialize)]
struct TSonglistCover {
    default_url: String,
}

impl From<TSonglistInfo> for Playlist {
    fn from(value: TSonglistInfo) -> Self {
        Self {
            id: value.tid.to_string(),
            name: value.title,
            pic_url: value.cover.default_url,
        }
    }
}

impl TSonglistListResponse {
    pub(super) fn into_with(self, category: &str, end: u64) -> PlaylistListResult {
        let content = self.result.data.content;
        PlaylistListResult {
            playlists: content
                .v_item
                .into_iter()
                .map(|item| item.basic.into())
                .collect(),
            more: content.total_cnt > end,
            category: category.to_string(),
        }
    }
}

/// `music.login.LoginServer.CreateQRCode`（登录二维码）响应体。
#[derive(Deserialize)]
pub(super) struct TLoginQrResponse {
    result: TLoginQrResult,
}

#[derive(Deserialize)]
struct TLoginQrResult {
    data: TLoginQrData,
}

#[derive(Deserialize)]
struct TLoginQrData {
    qrcode: String,
    #[serde(rename = "qrcodeID")]
    qrcode_id: String,
}

impl From<TLoginQrResponse> for LoginQrResult {
    fn from(value: TLoginQrResponse) -> Self {
        Self {
            qr_code: value.result.data.qrcode,
            qr_key: value.result.data.qrcode_id,
        }
    }
}

/// `music.login.LoginServer.Login`（扫码换 token / 刷新 token）响应体。
#[derive(Deserialize)]
pub(super) struct TLoginInfoResponse {
    result: TLoginInfoResult,
}

#[derive(Deserialize)]
struct TLoginInfoResult {
    data: TLoginInfoData,
}

#[derive(Deserialize)]
struct TLoginInfoData {
    musicid: u64,
    musickey: String,
    refresh_token: String,
    refresh_key: String,
    expired_at: i64,
    #[serde(rename = "musickeyCreateTime")]
    musickey_create_time: i64,
    #[serde(rename = "keyExpiresIn")]
    key_expires_in: i64,
    #[serde(rename = "loginType")]
    login_type: u64,
    #[serde(rename = "encryptUin", default)]
    encrypted_uin: String,
}

impl TLoginInfoResponse {
    pub(super) fn into_token(self) -> MusicClientResult<TencentLoginToken> {
        let data = self.result.data;
        if data.musicid == 0 {
            return Err(MusicClientError::InvalidTencentLoginTokenField("musicid"));
        }
        if data.musickey.trim().is_empty() {
            return Err(MusicClientError::InvalidTencentLoginTokenField("musickey"));
        }

        let expires_at = if data.expired_at > 0 {
            Some(data.expired_at)
        } else if data.key_expires_in > 0 {
            if data.musickey_create_time > 0 {
                Some(data.musickey_create_time + data.key_expires_in)
            } else {
                Some(OffsetDateTime::now_utc().unix_timestamp() + data.key_expires_in)
            }
        } else {
            None
        };

        Ok(TencentLoginToken {
            music_id: data.musicid,
            music_key: data.musickey,
            refresh_token: data.refresh_token,
            refresh_key: data.refresh_key,
            expires_at,
            login_type: data.login_type,
            encrypted_uin: data.encrypted_uin,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::TLoginInfoResponse;

    #[test]
    fn login_response_preserves_encrypted_uin_without_debugging_secrets() {
        let response: TLoginInfoResponse = serde_json::from_value(json!({
            "result": {
                "data": {
                    "musicid": 10001,
                    "musickey": "secret-music-key",
                    "refresh_token": "secret-refresh-token",
                    "refresh_key": "secret-refresh-key",
                    "expired_at": 2000000000,
                    "musickeyCreateTime": 1900000000,
                    "keyExpiresIn": 100000000,
                    "loginType": 2,
                    "encryptUin": "encrypted-uin"
                }
            }
        }))
        .expect("valid login response");

        let token = response.into_token().expect("valid login token");

        assert_eq!(token.encrypted_uin, "encrypted-uin");
        let debug = format!("{token:?}");
        assert!(!debug.contains("10001"));
        assert!(!debug.contains("secret-"));
        assert!(!debug.contains("encrypted-uin"));
    }
}
