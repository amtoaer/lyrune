//! 网易云 API 请求实现。
//!
//! 该模块负责组装 EAPI 请求、注入可选登录态，并把平台响应转换为 [`crate::models`] 中
//! 的统一返回模型。

use std::sync::LazyLock;
use std::time::Duration;

use cookie::Cookie;
use reqwest::Client;
use reqwest::header::{COOKIE, HeaderMap, SET_COOKIE};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::models::*;
use super::utils;
use crate::error::{MusicClientError, MusicClientResult};
use crate::models::*;

static EAPI_HEADER_JSON: LazyLock<String> = LazyLock::new(|| {
    json!({
        "clientSign": "18:C0:4D:B9:8F:FE@@@\
     453832335F384641365F424635335F303030315F303031425F343434415F343643365F333638332@@@@@@\
     6ff673ef74955b38bce2fa8562d95c976ed4758b1227c4e9ee345987cee17bc9",
        "os": "pc",
        "appver": "3.1.17.204416",
        "deviceId": "121F1C01530F5AF886E1F0A37F597AE2C460EEBDB025AA7649CC",
        "requestId": 0,
        "osver": "Microsoft-Windows-10-Professional-build-19045-64bit",
    })
    .to_string()
});

const EAPI_COOKIE_SUFFIX: &str =
    "os=pc; appver=3.1.17.204416; deviceId=121F1C01530F5AF886E1F0A37F597AE2C460EEBDB025AA7649CC; \
     osver=Microsoft-Windows-10-Professional-build-19045-64bit; \
     clientSign=18:C0:4D:B9:8F:FE@@@\
     453832335F384641365F424635335F303030315F303031425F343434415F343643365F333638332@@@@@@\
     6ff673ef74955b38bce2fa8562d95c976ed4758b1227c4e9ee345987cee17bc9; channel=netease";
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const SONG_DETAIL_BATCH_SIZE: usize = 1000;

pub(crate) struct NeteaseClient {
    client: Client,
}

impl NeteaseClient {
    /// 创建网易云请求客户端。
    pub(crate) fn new() -> Self {
        Self {
            client: Client::builder()
                .no_proxy()
                .timeout(HTTP_TIMEOUT)
                .connect_timeout(HTTP_TIMEOUT)
                .user_agent(
                    "Mozilla/5.0 (Windows NT 10.0; WOW64) AppleWebKit/537.36 (KHTML, like Gecko) \
                     Safari/537.36 Chrome/91.0.4472.164 NeteaseMusicDesktop/3.0.18.203152",
                )
                .build()
                .unwrap(),
        }
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        uri: &str,
        mut body: Value,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<T> {
        body["e_r"] = json!(false);
        body["header"] = json!(EAPI_HEADER_JSON.as_str());

        let url = netease_eapi_url(uri);
        let cookie = match token {
            Some(login) => netease_cookie(login, uri),
            None => EAPI_COOKIE_SUFFIX.to_string(),
        };
        let params = utils::eapi_params(uri, body.to_string().as_str());

        self.client
            .post(&url)
            .header(COOKIE, cookie)
            .form(&[("params", params)])
            .send()
            .await?
            .json::<T>()
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn search_songs(
        &self,
        keyword: &str,
        limit: u64,
        offset: u64,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<SearchSongResult> {
        let response = self
            .post::<NSearchSongResponse>(
                "/api/cloudsearch/pc",
                json!({
                    "s": keyword,
                    "type": 1,
                    "limit": limit,
                    "offset": offset,
                    "total": true
                }),
                token,
            )
            .await?;
        Ok(response.into_with(offset + limit))
    }

    pub(crate) async fn search_artists(
        &self,
        keyword: &str,
        limit: u64,
        offset: u64,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<SearchArtistResult> {
        let response = self
            .post::<NSearchArtistResponse>(
                "/api/v1/search/artist/get",
                json!({
                    "s": keyword,
                    "limit": limit,
                    "offset": offset,
                    "queryCorrect": true
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn search_albums(
        &self,
        keyword: &str,
        limit: u64,
        offset: u64,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<SearchAlbumResult> {
        let response = self
            .post::<NSearchAlbumResponse>(
                "/api/v1/search/album/get",
                json!({
                    "s": keyword,
                    "limit": limit,
                    "offset": offset,
                    "queryCorrect": true
                }),
                token,
            )
            .await?;
        Ok(response.into_with(offset + limit))
    }

    pub(crate) async fn search_playlists(
        &self,
        keyword: &str,
        limit: u64,
        offset: u64,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<SearchPlaylistResult> {
        let response = self
            .post::<NSearchPlaylistResponse>(
                "/api/v1/search/playlist/get",
                json!({
                    "s": keyword,
                    "limit": limit,
                    "offset": offset,
                    "queryCorrect": true
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_search_suggests(
        &self,
        keyword: &str,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<SearchSuggestResult> {
        let response = self
            .post::<NSearchSuggestResponse>(
                "/api/search/pc/suggest/keyword/get",
                json!({
                    "keyword": keyword,
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn songs_detail(
        &self,
        ids: Vec<String>,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<SongsDetailResult> {
        let response = self
            .post::<NSongDetailResponse>(
                "/api/v3/song/detail",
                json!({"c": json!(ids.iter().map(|item| json!({ "id": item })).collect::<Vec<_>>()).to_string()}),
                token,
            )
            .await?;
        Ok(response.into_with())
    }

    pub(crate) async fn artist_detail(
        &self,
        id: &str,
        limit: u64,
        offset: u64,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<ArtistDetailResult> {
        let response = self
            .post::<NArtistDetailResponse>(
                "/api/batch",
                json!({
                    "/api/v2/artist/songs": json!({
                        "id": id,
                        "limit": limit,
                        "offset": offset
                    })
                    .to_string(),
                    "/api/artist/head/info/get": json!({
                        "id": id
                    })
                    .to_string()
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn album_detail(
        &self,
        id: &str,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<AlbumDetailResult> {
        let response = self
            .post::<NAlbumDetailResponse>(
                "/api/album/v3/detail",
                json!({
                    "id": id,
                    "cache_key": utils::album_cache_key(id)
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn playlist_detail(
        &self,
        id: u64,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<PlaylistDetailResult> {
        let response = self
            .post::<NPlaylistDetailResponse>(
                "/api/v6/playlist/detail",
                json!({
                    "id": id,
                    "n": 10000,
                    "s": 8
                }),
                token,
            )
            .await?;
        let ids = response.song_ids();
        let songs = self.songs_detail_in_batches(ids, token).await?;
        Ok(response.into_with_songs(songs))
    }

    async fn songs_detail_in_batches(
        &self,
        ids: Vec<String>,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<Vec<Song>> {
        let mut songs = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(SONG_DETAIL_BATCH_SIZE) {
            songs.extend(self.songs_detail(chunk.to_vec(), token).await?.songs);
        }
        Ok(songs)
    }

    pub(crate) async fn get_lyric(
        &self,
        id: &str,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<LyricResult> {
        let response = self
            .post::<NLyricResponse>(
                "/api/song/lyric/v1",
                json!({
                    "id": id,
                    "lv": -1,
                    "tv": -1,
                }),
                token,
            )
            .await?;
        Ok(response.into_with(id))
    }

    pub(crate) async fn get_url(
        &self,
        id: &str,
        level: SongQuality,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<UrlResult> {
        let ids = json!([id]).to_string();
        let response = self
            .post::<NUrlResponse>(
                "/api/song/enhance/player/url/v1",
                json!({
                    "ids": ids,
                    "level": match level {
                        SongQuality::Master => "jymaster",
                        SongQuality::Surround => "sky",
                        SongQuality::Stereo => "jyeffect",
                        SongQuality::Hires => "hires",
                        SongQuality::Lossless => "lossless",
                        SongQuality::Exhigh => "exhigh",
                        SongQuality::Standard => "standard",
                    },
                    "immerseType": match level {
                    SongQuality::Surround => "ste",
                    _ => "c51"
                },
                    "encodeType": "aac"
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_hotkey(
        &self,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<HotkeyResult> {
        let response = self
            .post::<NHotkeyResponse>(
                "/api/search/pc/chart/detail",
                json!({
                    "id": "HOT_SEARCH#@#"
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_recommend_playlist(
        &self,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<RecommendPlaylistResult> {
        let response = self
            .post::<NRecommendPlaylistResponse>(
                "/api/link/page/rcmd/resource/show",
                json!({
                "pageCode": "HOME_RECOMMEND_PAGE",
                "cursor": 1,
                "refresh": true,
                "blockCodeOrderList": "[\"PAGE_RECOMMEND_SPECIAL_CLOUD_VILLAGE_PLAYLIST\"]"
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_toplist(
        &self,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<ToplistListResult> {
        let response =
            self.post::<NToplistResponse>("/api/toplist/detail/v2", json!({}), token).await?;
        Ok(response.into())
    }

    pub(crate) async fn get_playlist_categories(
        &self,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<PlaylistCategoriesResult> {
        let response = self
            .post::<NPlaylistCategoriesResponse>("/api/playlist/catalogue", json!({}), token)
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_playlist_list(
        &self,
        cat: &str,
        limit: u64,
        offset: u64,
        token: Option<&NeteaseLoginToken>,
    ) -> MusicClientResult<PlaylistListResult> {
        let response = self
            .post::<NPlaylistListResponse>(
                "/api/playlist/list",
                json!({
                  "cat": cat,
                  "limit": limit,
                  "offset": offset,
                  "total": true,
                  "order": "hot"
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_login_qrcode(&self) -> MusicClientResult<LoginQrResult> {
        let response = self
            .post::<NLoginQrResponse>(
                "/api/login/qrcode/unikey",
                json!({
                    "type": 3
                }),
                None,
            )
            .await?;
        response.into_qr_result()
    }

    pub(crate) async fn get_login_token(&self, key: &str) -> MusicClientResult<LoginStatus> {
        let uri = "/api/login/qrcode/client/login";
        let url = netease_eapi_url(uri);
        let mut body = json!({
            "key": key,
            "type": 3
        });
        body["e_r"] = json!(false);
        body["header"] = json!(EAPI_HEADER_JSON.as_str());

        let response = self
            .client
            .post(url)
            .form(&[("params", utils::eapi_params(uri, body.to_string().as_str()))])
            .send()
            .await?;
        let headers = response.headers().clone();
        let response = response.json::<NLoginTokenResponse>().await?;

        match response.code {
            800 => Ok(LoginStatus::QrCodeExpired),
            801 => Ok(LoginStatus::WaitingScan),
            802 => Ok(LoginStatus::WaitingConfirm),
            803 => parse_login_token(&headers)
                .map(LoginToken::Netease)
                .map(LoginStatus::Success)
                .ok_or(MusicClientError::NeteaseLoginTokenInvalid),
            _ => Err(MusicClientError::NeteaseUnexpectedLoginStatus(response.code)),
        }
    }

    pub(crate) async fn refresh_login_token(
        &self,
        token: &NeteaseLoginToken,
    ) -> MusicClientResult<NeteaseLoginToken> {
        let uri = "/api/login/token/refresh";
        let url = netease_eapi_url(uri);
        let mut body = json!({});
        body["e_r"] = json!(false);
        body["header"] = json!(EAPI_HEADER_JSON.as_str());

        let response = self
            .client
            .post(url)
            .header(COOKIE, netease_cookie(token, uri))
            .form(&[("params", utils::eapi_params(uri, body.to_string().as_str()))])
            .send()
            .await?;
        let token = parse_login_token(response.headers());
        let _ = response.bytes().await?; // 或 json::<Value>().await?
        token.ok_or(MusicClientError::NeteaseLoginTokenInvalid)
    }
}

fn netease_eapi_url(uri: &str) -> String {
    // 兼容 "/api/xxx"、"api/xxx" 与纯路径三种写法，统一映射到 eapi 网关。
    let path = uri
        .strip_prefix("/api/")
        .or_else(|| uri.strip_prefix("api/"))
        .unwrap_or_else(|| uri.trim_start_matches('/'));
    format!("https://interface.music.163.com/eapi/{path}")
}

fn netease_cookie(login: &NeteaseLoginToken, uri: &str) -> String {
    // refresh 接口必须携带 MUSIC_R_U，其余接口使用常规 cookie 即可。
    let auth_cookie = if uri == "/api/login/token/refresh" {
        login.to_refresh_cookie()
    } else {
        login.to_cookie()
    };
    format!("{auth_cookie}; {EAPI_COOKIE_SUFFIX}")
}

fn parse_login_token(headers: &HeaderMap) -> Option<NeteaseLoginToken> {
    let mut music_u = None;
    let mut music_r_u = None;
    let mut csrf = None;
    let mut expires_at = None;

    for header in headers.get_all(SET_COOKIE) {
        let raw_cookie = match header.to_str() {
            Ok(value) => value,
            Err(_) => continue,
        };
        let cookie = match Cookie::parse(raw_cookie.to_owned()) {
            Ok(cookie) => cookie,
            Err(_) => continue,
        };

        match cookie.name() {
            "MUSIC_U" => music_u = Some(cookie.value().to_owned()),
            "MUSIC_R_U" => music_r_u = Some(cookie.value().to_owned()),
            "__csrf" => {
                csrf = Some(cookie.value().to_owned());
                expires_at = parse_csrf_expiry(&cookie);
            }
            _ => {}
        }
    }

    let music_u = music_u?;
    // Some successful QR logins return MUSIC_U + __csrf without MUSIC_R_U.
    // Keep the login flow successful and use MUSIC_U as refresh fallback.
    let music_r_u = music_r_u.unwrap_or_else(|| music_u.clone());
    Some(NeteaseLoginToken::new(music_u, music_r_u, csrf?, expires_at))
}

fn parse_csrf_expiry(cookie: &Cookie<'_>) -> Option<i64> {
    if let Some(max_age) = cookie.max_age() {
        return OffsetDateTime::now_utc().checked_add(max_age).map(OffsetDateTime::unix_timestamp);
    }
    cookie.expires_datetime().map(OffsetDateTime::unix_timestamp)
}
