//! QQ 音乐 API 请求实现。
//!
//! 该模块负责组装 QQ 音乐网关请求、注入可选登录态，并把平台响应转换为
//! [`crate::models`] 中的统一返回模型。

use std::sync::LazyLock;
use std::time::Duration;

use reqwest::Client;
use reqwest::header::COOKIE;
use serde_json::{Value, json};

use super::models::*;
use super::mqtt::{self, MqttLoginEvent};
use super::utils;
use crate::error::{MusicClientError, MusicClientResult};
use crate::models::*;

const TENCENT_API_URL: &str = "https://u.y.qq.com/cgi-bin/musics.fcg";
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
static TENCENT_GUID: LazyLock<String> = LazyLock::new(utils::get_guid);

pub(crate) struct TencentClient {
    client: Client,
}

impl TencentClient {
    /// 创建 QQ 音乐请求客户端。
    pub(crate) fn new() -> Self {
        Self {
            client: Client::builder()
                .no_proxy()
                .timeout(HTTP_TIMEOUT)
                .connect_timeout(HTTP_TIMEOUT)
                .user_agent(
                    "Mozilla/5.0 (compatible; MSIE 9.0; Windows NT 6.1; WOW64; Trident/5.0)",
                )
                .build()
                .unwrap(),
        }
    }

    fn page_num(offset: u64, limit: u64) -> u64 {
        // Tencent 搜索分页从 1 开始，limit=0 时兜底到第一页，避免除零。
        if limit == 0 { 1 } else { offset / limit + 1 }
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        mut body: Value,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<T> {
        let mut comm = match token {
            Some(login_token) => json!({
                "ct": 19,
                "cv": 2201,
                "chid": "0",
                "uin": login_token.music_id.to_string(),
                "g_tk": utils::hash33(login_token.music_key.as_str()),
                "guid": TENCENT_GUID.as_str()
            }),
            None => json!({
                "ct": 19,
                "cv": 2201,
                "chid": "0",
                "guid": TENCENT_GUID.as_str()
            }),
        };

        // 业务请求可以在 body.comm 覆盖默认公共参数（例如登录类型）。
        if let Some(custom_comm) = body.get("comm").and_then(Value::as_object) {
            for (key, value) in custom_comm {
                comm[key] = value.clone();
            }
        }
        body["comm"] = comm;
        // sign 必须基于最终 body 计算，否则服务端会验签失败。
        let sign = utils::sign(&body);

        self.client
            .post(TENCENT_API_URL)
            .query(&[("sign", sign.as_str())])
            .header(COOKIE, token.map(|v| v.to_cookie()).unwrap_or_default())
            .json(&body)
            .send()
            .await?
            .json::<T>()
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn search_songs(
        &self,
        query: &str,
        limit: u64,
        offset: u64,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<SearchSongResult> {
        let page_num = Self::page_num(offset, limit);
        let response = self
            .post::<TSearchResponse>(
                json!({"result": {
                  "method": "DoSearchForQQMusicDesktop",
                  "module": "music.search.SearchCgiService",
                  "param": {
                    "grp": 0,
                    "num_per_page": limit,
                    "page_num": page_num,
                    "query": query,
                    "search_type": 0,
                    "searchid": utils::get_search_id(),
                  }
                }}),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn search_artists(
        &self,
        query: &str,
        limit: u64,
        offset: u64,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<SearchArtistResult> {
        let page_num = Self::page_num(offset, limit);
        let response = self
            .post::<TSearchResponse>(
                json!({"result": {
                  "method": "DoSearchForQQMusicDesktop",
                  "module": "music.search.SearchCgiService",
                  "param": {
                    "grp": 0,
                    "num_per_page": limit,
                    "page_num": page_num,
                    "query": query,
                    "search_type": 1,
                    "searchid": utils::get_search_id(),
                  }
                }}),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn search_albums(
        &self,
        query: &str,
        limit: u64,
        offset: u64,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<SearchAlbumResult> {
        let page_num = Self::page_num(offset, limit);
        let response = self
            .post::<TSearchResponse>(
                json!({"result": {
                  "method": "DoSearchForQQMusicDesktop",
                  "module": "music.search.SearchCgiService",
                  "param": {
                    "grp": 0,
                    "num_per_page": limit,
                    "page_num": page_num,
                    "query": query,
                    "search_type": 2,
                    "searchid": utils::get_search_id(),
                  }
                }}),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn search_playlists(
        &self,
        query: &str,
        limit: u64,
        offset: u64,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<SearchPlaylistResult> {
        let page_num = Self::page_num(offset, limit);
        let response = self
            .post::<TSearchResponse>(
                json!({
                    "result": {
                      "method": "DoSearchForQQMusicDesktop",
                      "module": "music.search.SearchCgiService",
                      "param": {
                        "grp": 0,
                        "num_per_page": limit,
                        "page_num": page_num,
                        "query": query,
                        "search_type": 3,
                        "searchid": utils::get_search_id(),
                      }
                    }
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_search_suggests(
        &self,
        query: &str,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<SearchSuggestResult> {
        let response = self
            .post::<TSearchSuggestResponse>(
                json!({
                    "result": {
                      "method": "GetSmartBoxResult",
                      "module": "music.smartboxCgi.SmartBoxCgi",
                      "param": {
                        "num_per_page": 10,
                        "page_idx": 0,
                        "query": query,
                        "search_id": utils::get_search_id(),
                      }
                    }
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn songs_detail(
        &self,
        ids: Vec<String>,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<SongsDetailResult> {
        let response = self
            .post::<TSongsDetailResponse>(
                json!({
                    "result": {
                        "module": "music.trackInfo.UniformRuleCtrl",
                        "method": "CgiGetTrackInfo",
                        "param": {
                            "types": vec![0_u64; ids.len()],
                            "modify_stamp": vec![0_u64; ids.len()],
                            "ctx": 0,
                            "client": 1,
                            "mids": ids
                        }
                    }
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn artist_detail(
        &self,
        id: &str,
        limit: u64,
        offset: u64,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<ArtistDetailResult> {
        let response = self
            .post::<TSingerDetailResponse>(
                json!({
                  "singer_info": {
                    "module": "music.musichallSinger.SingerInfoInter",
                    "method": "GetSingerDetail",
                    "param": {
                      "singer_mids": [
                        id
                      ],
                      "pic": 0,
                      "group_singer": 0,
                      "wiki_singer": 1,
                      "ex_singer": 1
                    }
                  },
                  "singer_songs": {
                    "module": "music.musichallSong.SongListInter",
                    "method": "GetSingerSongList",
                    "param": {
                      "singerMid": id,
                      "begin": offset,
                      "num": limit,
                      "order": 1
                    }
                  }
                }),
                token,
            )
            .await?;
        Ok(response.into_with(offset.saturating_add(limit)))
    }

    pub(crate) async fn album_detail(
        &self,
        id: &str,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<AlbumDetailResult> {
        let response = self
            .post::<TAlbumDetailResponse>(
                json!({
                  "album_info": {
                    "module": "music.musichallAlbum.AlbumInfoServer",
                    "method": "GetAlbumDetail",
                    "param": {
                      "albumMid": id
                    }
                  },
                  "album_songs": {
                    "module": "music.musichallAlbum.AlbumSongList",
                    "method": "GetAlbumSongList",
                    "param": {
                      "albumMid": id,
                      "begin": 0,
                      "num": 1000,
                      "order": 2
                    }
                  }
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn playlist_detail(
        &self,
        id: u64,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<PlaylistDetailResult> {
        let response = self
            .post::<TSonglistDetailResponse>(
                json!({
                  "result": {
                    "module": "music.srfDissInfo.aiDissInfo",
                    "method": "uniform_get_Dissinfo",
                    "param": {
                      "disstid": id,
                      "userinfo": 0,
                      "tag": 0,
                      "is_pc": 1,
                      "guid": TENCENT_GUID.as_str()
                    }
                  }
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn toplist_detail(
        &self,
        id: u64,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<PlaylistDetailResult> {
        let response = self
            .post::<TToplistDetailResponse>(
                json!({
                  "result": {
                    "module": "music.musicToplist.Toplist",
                    "method": "GetDetail",
                    "param": {
                      "topid": id,
                      "num": 300
                    }
                  }
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_lyric(
        &self,
        id: &str,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<LyricResult> {
        let response = self
            .post::<TLyricResponse>(
                json!({
                  "result": {
                    "method": "GetPlayLyricInfo",
                    "module": "music.musichallSong.PlayLyricInfo",
                    "param": {
                      "crypt": 0,
                      "roma": 0,
                      "songMID": id,
                      "trans": 1,
                      "type": 0
                    }
                  }
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
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<UrlResult> {
        // 按音质从高到低排列候选文件名，并在末尾追加 RS02 普通音质兜底。
        let mut filename = match level {
            SongQuality::Master => vec![
                format!("AI00{id}{id}.flac"),
                format!("Q001{id}{id}.flac"),
                format!("Q000{id}{id}.flac"),
                format!("F000{id}{id}.flac"),
                format!("M800{id}{id}.mp3"),
                format!("M500{id}{id}.mp3"),
            ],
            SongQuality::Surround => vec![
                format!("Q001{id}{id}.flac"),
                format!("Q000{id}{id}.flac"),
                format!("F000{id}{id}.flac"),
                format!("M800{id}{id}.mp3"),
                format!("M500{id}{id}.mp3"),
            ],
            SongQuality::Stereo => vec![
                format!("Q000{id}{id}.flac"),
                format!("F000{id}{id}.flac"),
                format!("M800{id}{id}.mp3"),
                format!("M500{id}{id}.mp3"),
            ],
            SongQuality::Hires | SongQuality::Lossless => vec![
                format!("F000{id}{id}.flac"),
                format!("M800{id}{id}.mp3"),
                format!("M500{id}{id}.mp3"),
            ],
            SongQuality::Exhigh => vec![format!("M800{id}{id}.mp3"), format!("M500{id}{id}.mp3")],
            SongQuality::Standard => vec![format!("M500{id}{id}.mp3")],
        };
        filename.push(format!("RS02{id}.mp3"));
        let songmid = vec![id; filename.len()];
        let mut songtype = vec![0_u8; filename.len()];
        if let Some(last) = songtype.last_mut() {
            *last = 1;
        }
        let response = self
            .post::<TUrlResponse>(
                json!({
                  "result": {
                    "method": "UrlGetVkey",
                    "module": "music.vkey.GetVkey",
                    "param": {
                      "uin": token.map(|v| v.music_id.to_string()).unwrap_or_default(),
                      "filename": filename,
                      "guid": TENCENT_GUID.as_str(),
                      "songmid": songmid,
                      "songtype": songtype,
                      "ctx": 0
                    }
                  }
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_hotkey(
        &self,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<HotkeyResult> {
        let response = self
            .post::<THotkeyResponse>(
                json!({
                  "result": {
                    "method": "GetHotkeyForQQMusicPC",
                    "module": "tencent_musicsoso_hotkey.HotkeyService",
                    "param": {
                      "search_id": utils::get_search_id(),
                      "uin": 0
                    }
                  }
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_recommend_playlist(
        &self,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<RecommendPlaylistResult> {
        let response = self
            .post::<TRecommendSonglistResponse>(
                json!({
                  "result": {
                    "module": "music.recommend.RecommendFeed",
                    "method": "get_recommend_feed",
                    "param": {
                      "direction": 0,
                      "page": 1,
                      "v_cache": [],
                      "v_uniq": [],
                      "s_num": 0
                    }
                  }
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_toplist(
        &self,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<ToplistListResult> {
        let response = self
            .post::<TToplistResponse>(
                json!({
                  "result": {
                    "module": "music.musicToplist.Toplist",
                    "method": "GetAll",
                    "param": {}
                  }
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_playlist_categories(
        &self,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<PlaylistCategoriesResult> {
        let response = self
            .post::<TSonglistCategoriesResponse>(
                json!({
                  "result": {
                    "method": "GetAllTag",
                    "param": {},
                    "module": "music.playlist.PlaylistSquare"
                  }
                }),
                token,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_playlist_list(
        &self,
        cat: &str,
        limit: u64,
        offset: u64,
        token: Option<&TencentLoginToken>,
    ) -> MusicClientResult<PlaylistListResult> {
        let page = Self::page_num(offset, limit);
        let category_id =
            cat.parse::<u64>().map_err(|_| MusicClientError::InvalidCategoryId(cat.to_owned()))?;
        let response = self
            .post::<TSonglistListResponse>(
                json!({
                  "result": {
                    "module": "playlist.PlayListCategoryServer",
                    "method": "get_category_content",
                    "param": {
                      "caller": "0",
                      "category_id": category_id,
                      "page": page,
                      "use_page": 1,
                      "size": limit
                    }
                  }
                }),
                token,
            )
            .await?;
        Ok(response.into_with(cat, offset.saturating_add(limit)))
    }

    pub(crate) async fn get_login_qrcode(&self) -> MusicClientResult<LoginQrResult> {
        let response = self
            .post::<TLoginQrResponse>(
                json!({
                  "result": {
                    "module": "music.login.LoginServer",
                    "method": "CreateQRCode",
                    "param": {
                      "tmeAppID": "qqmusic",
                      "ct": 19,
                      "cv": 2201
                    }
                  }
                }),
                None,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_login_token_with_mqtt_session(
        &self,
        qrcode_id: &str,
        session: &mut mqtt::MqttLoginSession,
    ) -> MusicClientResult<LoginStatus> {
        let event = session.poll_event().await?;
        match event {
            MqttLoginEvent::WaitingScan => Ok(LoginStatus::WaitingScan),
            MqttLoginEvent::WaitingConfirm => Ok(LoginStatus::WaitingConfirm),
            MqttLoginEvent::QrCodeExpired => Ok(LoginStatus::QrCodeExpired),
            MqttLoginEvent::Canceled => Err(MusicClientError::TencentLoginCanceled),
            MqttLoginEvent::LoginFailed => Err(MusicClientError::TencentLoginFailed),
            MqttLoginEvent::Cookies { music_id, music_key } => self
                .login_with_mobile_ticket(qrcode_id, music_id, music_key.as_str())
                .await
                .map(LoginToken::Tencent)
                .map(LoginStatus::Success),
        }
    }

    pub(crate) async fn refresh_login_token(
        &self,
        token: &TencentLoginToken,
    ) -> MusicClientResult<TencentLoginToken> {
        let response = self
            .post::<TLoginInfoResponse>(
                json!({
                    "result": {
                        "module": "music.login.LoginServer",
                        "method": "Login",
                        "param": {
                            "refresh_key": token.refresh_key,
                            "refresh_token": token.refresh_token,
                            "musickey": token.music_key,
                            "musicid": token.music_id,
                        }
                    },
                    "comm": {
                        "tmeLoginType": token.login_type
                    }
                }),
                Some(token),
            )
            .await?;
        response.into_token()
    }

    async fn login_with_mobile_ticket(
        &self,
        qrcode_id: &str,
        music_id: u64,
        music_key: &str,
    ) -> MusicClientResult<TencentLoginToken> {
        let response = self
            .post::<TLoginInfoResponse>(
                json!({
                    "result": {
                        "module": "music.login.LoginServer",
                        "method": "Login",
                        "param": {
                            "musicid": music_id,
                            "qrCodeID": qrcode_id,
                            "token": music_key
                        }
                    },
                    "comm": {
                        "tmeLoginType": 6
                    }
                }),
                None,
            )
            .await?;
        response.into_token()
    }
}
