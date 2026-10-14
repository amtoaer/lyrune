use std::collections::HashSet;

use anyhow::{Context as _, Result, bail};
use reqwest::header::{COOKIE, REFERER};
use serde_json::{Value, json};

use super::ProtocolClient;
use super::response::{
    bool_field, bool_value, find_array_recursively, find_object_recursively, find_string_in_object,
    find_string_recursively, integer_array_field, integer_field, string_field,
};
use crate::account::is_credential_rejected;
use crate::signing::hash33;
use crate::{
    CredentialSession, PlaylistPage, QqCredential, RadarTrackPage, SearchArtist, SearchPage, Track,
    UserPlaylist, UserPlaylistId, UserProfile,
};

const PROFILE_URL: &str = "https://c6.y.qq.com/rsc/fcgi-bin/fcg_get_profile_homepage.fcg";

impl ProtocolClient {
    pub async fn ensure_encrypted_uin(&self, mut credential: QqCredential) -> Result<QqCredential> {
        if credential.encrypted_uin.trim().is_empty() {
            credential.encrypted_uin = self
                .fetch_encrypted_uin(&credential)
                .await
                .context("无法补全“已点赞的歌曲”所需的用户标识")?;
        }
        Ok(credential)
    }

    pub async fn logout(&self, credential: &QqCredential) -> Result<()> {
        self.call_with_credential(
            "music.login.LoginServer",
            "Logout",
            json!({}),
            credential,
            Some(json!({ "tmeLoginType": credential.login_type })),
        )
        .await
        .map(|_| ())
    }

    pub async fn validate_credential(&self, credential: &CredentialSession) -> Result<()> {
        self.call(
            "music.UserInfo.userInfoServer",
            "GetLoginUserInfo",
            json!({}),
            credential,
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn user_profile(&self, credential: &CredentialSession) -> Result<UserProfile> {
        let current = credential.ensure_fresh().await?;
        let primary = match self
            .call_with_session(
                "music.UserInfo.userInfoServer",
                "GetLoginUserInfo",
                json!({}),
                credential,
                &current,
                None,
            )
            .await
        {
            Ok(profile) => Some(profile),
            Err(error) if is_credential_rejected(&error) => return Err(error),
            Err(_) => None,
        };
        let primary_is_complete = primary.as_ref().is_some_and(|profile| {
            find_string_recursively(profile, &["nickname", "nick", "userName"]).is_some()
                && find_string_recursively(
                    profile,
                    &[
                        "avatarUrl",
                        "headurl",
                        "headUrl",
                        "headpic",
                        "headPic",
                        "logo",
                    ],
                )
                .is_some()
        });
        let legacy = if primary_is_complete {
            None
        } else {
            self.fetch_legacy_profile(&current).await.ok()
        };
        let profiles = [primary.as_ref(), legacy.as_ref()];

        let nickname = profiles
            .into_iter()
            .flatten()
            .find_map(|profile| {
                find_string_recursively(profile, &["nickname", "nick", "userName"])
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| "QQ 音乐用户".to_owned());
        let avatar_url = profiles
            .into_iter()
            .flatten()
            .find_map(|profile| {
                find_string_recursively(
                    profile,
                    &[
                        "avatarUrl",
                        "headurl",
                        "headUrl",
                        "headpic",
                        "headPic",
                        "logo",
                    ],
                )
                .filter(|value| !value.trim().is_empty())
            })
            .map(force_https)
            .or_else(|| {
                Some(format!(
                    "https://q1.qlogo.cn/g?b=qq&nk={}&s=100",
                    current.music_id
                ))
            });
        let id = profiles
            .into_iter()
            .flatten()
            .find_map(|profile| {
                find_string_recursively(profile, &["str_musicid", "musicid", "music_id", "uin"])
                    .filter(|value| value != "0")
            })
            .unwrap_or_else(|| current.music_id.to_string());

        Ok(UserProfile {
            id,
            nickname,
            avatar_url,
        })
    }

    pub async fn user_playlists(
        &self,
        credential: &CredentialSession,
    ) -> Result<Vec<UserPlaylist>> {
        let current = credential.ensure_fresh().await?;
        let liked_data = self
            .playlist_data(credential, &UserPlaylistId::Liked, 0, 1)
            .await
            .context("无法读取“已点赞的歌曲”概要")?;
        let mut liked = playlist_from_detail(&liked_data, UserPlaylist::liked());
        liked.track_count =
            integer_field(&liked_data, &["total_song_num", "total"]).unwrap_or_default();

        let created_data = self
            .call_with_session(
                "music.musicasset.PlaylistBaseRead",
                "GetPlaylistByUin",
                json!({ "uin": current.music_id.to_string() }),
                credential,
                &current,
                None,
            )
            .await
            .context("无法加载用户创建的 QQ 音乐歌单")?;
        let created = find_array_recursively(&created_data, &["v_playlist", "playlist"])
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(parse_created_playlist)
            .collect::<Vec<_>>();

        let mut favorites = Vec::new();
        let mut offset = 0_u64;
        loop {
            let data = self
                .call_with_session(
                    "music.musicasset.PlaylistFavRead",
                    "CgiGetPlaylistFavInfo",
                    json!({
                        "uin": current.encrypted_uin,
                        "offset": offset,
                        "size": 100,
                    }),
                    credential,
                    &current,
                    None,
                )
                .await
                .context("无法加载用户收藏的 QQ 音乐歌单")?;
            let page = find_array_recursively(&data, &["v_list", "playlist"])
                .map(Vec::as_slice)
                .unwrap_or_default();
            favorites.extend(page.iter().filter_map(parse_favorite_playlist));
            let has_more = bool_field(&data, &["hasmore", "has_more"]).unwrap_or(false);
            if !has_more || page.is_empty() {
                break;
            }
            offset = offset.saturating_add(page.len() as u64);
        }

        let mut seen = HashSet::new();
        let mut playlists = Vec::with_capacity(1 + created.len() + favorites.len());
        seen.insert(liked.id.clone());
        playlists.push(liked);
        for playlist in created.into_iter().chain(favorites) {
            if seen.insert(playlist.id.clone()) {
                playlists.push(playlist);
            }
        }
        Ok(playlists)
    }

    pub async fn recommended_playlists(
        &self,
        credential: &CredentialSession,
        offset: u64,
        limit: u64,
    ) -> Result<SearchPage<UserPlaylist>> {
        let limit = limit.clamp(1, 100);
        let data = self
            .call(
                "music.playlist.PlaylistSquare",
                "GetRecommendFeed",
                json!({
                    "From": offset,
                    "Size": limit,
                }),
                credential,
                None,
            )
            .await
            .context("无法加载 QQ 音乐推荐歌单")?;
        parse_recommended_playlist_page(&data, offset)
    }

    pub async fn radar_tracks(
        &self,
        credential: &CredentialSession,
        page: u64,
    ) -> Result<RadarTrackPage> {
        let page = page.max(1);
        let data = self
            .call(
                "music.recommend.TrackRelationServer",
                "GetRadarSong",
                json!({
                    "Page": page,
                    "ReqType": 0,
                    "FavSongs": [],
                    "EntranceSongs": [],
                }),
                credential,
                None,
            )
            .await
            .context("无法加载 QQ 音乐专属雷达")?;
        let tracks = recommendation_tracks(&data, &["VecSongs", "vecSongs"])
            .context("QQ 音乐专属雷达数据格式发生了变化")?;
        let has_more =
            bool_field(&data, &["HasMore", "hasMore", "has_more"]).unwrap_or(!tracks.is_empty());
        Ok(RadarTrackPage {
            tracks,
            has_more,
            next_page: page.saturating_add(1),
        })
    }

    pub async fn guess_tracks(
        &self,
        credential: &CredentialSession,
        limit: u64,
    ) -> Result<Vec<Track>> {
        let data = self
            .call(
                "music.radioProxy.MbTrackRadioSvr",
                "get_radio_track",
                json!({
                    "id": 99,
                    "num": limit.clamp(1, 30),
                    "from": 0,
                    "scene": 0,
                    "song_ids": [],
                }),
                credential,
                None,
            )
            .await
            .context("无法加载 QQ 音乐猜你喜欢")?;
        recommendation_tracks(&data, &["Tracks", "tracks"])
            .context("QQ 音乐猜你喜欢数据格式发生了变化")
    }

    pub async fn playlist_page(
        &self,
        credential: &CredentialSession,
        playlist: &UserPlaylist,
        offset: u64,
        limit: u64,
    ) -> Result<PlaylistPage> {
        let limit = limit.clamp(1, 100);
        let data = match &playlist.id {
            UserPlaylistId::Artist { mid } => {
                self.call(
                    "music.musichallSong.SongListInter",
                    "GetSingerSongList",
                    json!({
                        "singerMid": mid,
                        "begin": offset,
                        "num": limit,
                        "order": 1,
                    }),
                    credential,
                    None,
                )
                .await
            }
            UserPlaylistId::Album { mid } => {
                self.call(
                    "music.musichallAlbum.AlbumSongList",
                    "GetAlbumSongList",
                    json!({
                        "albumMid": mid,
                        "begin": offset,
                        "num": limit,
                        "order": 2,
                    }),
                    credential,
                    None,
                )
                .await
            }
            UserPlaylistId::Search { .. } => {
                bail!("搜索播放队列只能从本地缓存恢复")
            }
            UserPlaylistId::Recommendation { .. } => {
                bail!("推荐播放队列不使用歌单详情接口")
            }
            _ => {
                self.playlist_data(credential, &playlist.id, offset, limit)
                    .await
            }
        }
        .with_context(|| format!("无法加载 QQ 音乐媒体集合“{}”", playlist.title))?;
        let songs = find_array_recursively(&data, &["songlist", "song_list"])
            .map(Vec::as_slice)
            .unwrap_or_default();
        let tracks = songs
            .iter()
            .filter(|value| {
                let value = value
                    .get("songInfo")
                    .or_else(|| value.get("track"))
                    .or_else(|| value.get("Track"))
                    .unwrap_or(value);
                string_field(value, &["mid", "songmid"]).is_some_and(|mid| !mid.is_empty())
            })
            .map(parse_track)
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("QQ 音乐歌单“{}”的数据格式发生了变化", playlist.title))?;
        let total = integer_field(&data, &["total_song_num", "total", "totalNum", "total_num"])
            .unwrap_or_else(|| offset.saturating_add(songs.len() as u64));
        let next_offset = offset.saturating_add(songs.len() as u64);
        let has_more = bool_field(&data, &["hasmore", "has_more"]).unwrap_or(next_offset < total)
            && !songs.is_empty();

        let mut resolved_playlist = playlist_from_detail(&data, playlist.clone());
        if resolved_playlist.track_count == 0 {
            resolved_playlist.track_count = total;
        }
        Ok(PlaylistPage {
            playlist: resolved_playlist,
            tracks,
            total,
            has_more,
            next_offset,
        })
    }

    pub async fn track_liked(&self, credential: &CredentialSession, mid: &str) -> Result<bool> {
        let data = self
            .call(
                "music.musicasset.SongFavRead",
                "IsSongFanByMid",
                json!({ "v_songMid": [mid] }),
                credential,
                None,
            )
            .await
            .with_context(|| format!("无法读取歌曲 {mid} 的喜欢状态"))?;
        parse_track_liked(&data, mid).context("QQ 音乐喜欢状态响应格式发生了变化")
    }

    pub async fn set_track_liked(
        &self,
        credential: &CredentialSession,
        track: &Track,
        liked: bool,
    ) -> Result<()> {
        let song_id = track
            .song_id
            .with_context(|| format!("歌曲“{}”缺少数字 ID，无法修改喜欢状态", track.title))?;
        let method = if liked { "AddSonglist" } else { "DelSonglist" };
        let data = self
            .call(
                "music.musicasset.PlaylistDetailWrite",
                method,
                json!({
                    "dirId": 201,
                    "bFmtUtf8": true,
                    "v_songInfo": [{
                        "songId": song_id,
                    }],
                }),
                credential,
                None,
            )
            .await
            .with_context(|| {
                format!(
                    "无法{}歌曲“{}”",
                    if liked { "喜欢" } else { "取消喜欢" },
                    track.title
                )
            })?;
        let ret_code = integer_field(&data, &["retCode", "ret_code"]).unwrap_or_default();
        if ret_code != 0 {
            bail!("QQ 音乐歌单写入接口返回错误码 {ret_code}");
        }
        Ok(())
    }

    async fn playlist_data(
        &self,
        credential: &CredentialSession,
        id: &UserPlaylistId,
        offset: u64,
        limit: u64,
    ) -> Result<Value> {
        let current = credential.ensure_fresh().await?;
        let (diss_id, dir_id, encrypted_uin) = match id {
            UserPlaylistId::Liked => (0, 201, Some(current.encrypted_uin.as_str())),
            UserPlaylistId::Created { tid, .. } => (*tid, 0, None),
            UserPlaylistId::Favorite { diss_id } | UserPlaylistId::Recommended { diss_id } => {
                (*diss_id, 0, None)
            }
            UserPlaylistId::Artist { .. }
            | UserPlaylistId::Album { .. }
            | UserPlaylistId::Search { .. }
            | UserPlaylistId::Recommendation { .. } => {
                bail!("该媒体集合不使用歌单详情接口")
            }
        };
        let mut param = json!({
            "disstid": diss_id,
            "dirid": dir_id,
            "tag": 1,
            "song_begin": offset,
            "song_num": limit.clamp(1, 100),
            "userinfo": 1,
            "orderlist": 1,
            "onlysonglist": 0,
        });
        if let Some(encrypted_uin) = encrypted_uin {
            param
                .as_object_mut()
                .expect("playlist params are always an object")
                .insert("enc_host_uin".to_owned(), encrypted_uin.into());
        }
        self.call_with_session(
            "music.srfDissInfo.DissInfo",
            "CgiGetDiss",
            param,
            credential,
            &current,
            None,
        )
        .await
    }

    async fn fetch_encrypted_uin(&self, credential: &QqCredential) -> Result<String> {
        let response = self.fetch_legacy_profile(credential).await?;
        find_string_recursively(&response, &["encryptUin", "encrypt_uin"])
            .filter(|value| !value.trim().is_empty())
            .context("QQ 音乐用户资料没有包含加密用户标识")
    }

    async fn fetch_legacy_profile(&self, credential: &QqCredential) -> Result<Value> {
        self.client
            .get(PROFILE_URL)
            .header(COOKIE, credential.cookie())
            .header(REFERER, "https://y.qq.com/")
            .query(&[
                ("ct", "19".to_owned()),
                ("cv", "2201".to_owned()),
                ("format", "json".to_owned()),
                ("cid", "205360838".to_owned()),
                ("userid", credential.music_id.to_string()),
                ("uin", credential.music_id.to_string()),
                ("g_tk", hash33(&credential.music_key).to_string()),
                ("guid", credential.client_guid.clone()),
            ])
            .send()
            .await
            .context("QQ 音乐用户资料请求失败")?
            .error_for_status()
            .context("QQ 音乐用户资料接口拒绝了请求")?
            .json::<Value>()
            .await
            .context("QQ 音乐用户资料不是有效 JSON")
    }
}

pub(super) fn parse_track(value: &Value) -> Result<Track> {
    let wrapper = value;
    let value = wrapper
        .get("songInfo")
        .or_else(|| value.get("track"))
        .or_else(|| value.get("Track"))
        .unwrap_or(value);
    let mid = string_field(value, &["mid", "songmid"])
        .filter(|value| !value.is_empty())
        .context("歌曲缺少 mid")?;
    let title = string_field(value, &["title", "name"])
        .filter(|value| !value.is_empty())
        .context("歌曲缺少标题")?;

    let file = value.get("file").unwrap_or(&Value::Null);
    let media_mid =
        string_field(file, &["media_mid", "mediaMid"]).filter(|value| !value.is_empty());
    let standard_size_bytes = integer_field(file, &["size_128mp3", "size128"]);
    let high_size_bytes = integer_field(file, &["size_320mp3", "size320"]);
    let lossless_size_bytes = integer_field(file, &["size_flac", "sizeflac"]);
    let hi_res_size_bytes = integer_field(file, &["size_hires", "sizeHires"]);
    let master_size_bytes = integer_array_field(file, &["size_new", "sizeNew"], 0);
    let atmos_stereo_size_bytes = integer_array_field(file, &["size_new", "sizeNew"], 1);
    let atmos_surround_size_bytes = integer_array_field(file, &["size_new", "sizeNew"], 2);
    let artist_values = value
        .get("singer")
        .or_else(|| value.get("singers"))
        .and_then(Value::as_array);
    let artists = artist_values
        .map(|artists| {
            artists
                .iter()
                .filter_map(|artist| string_field(artist, &["name", "title"]))
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default();
    let artist_details = artist_values
        .map(|artists| {
            artists
                .iter()
                .filter_map(|artist| {
                    let mid =
                        string_field(artist, &["singerMID", "singerMid", "singer_mid", "mid"])
                            .filter(|mid| !mid.trim().is_empty())?;
                    let name = string_field(artist, &["name", "title", "singerName"])
                        .filter(|name| !name.trim().is_empty())?;
                    let cover_url = string_field(artist, &["singerPic", "picUrl", "pic"])
                        .filter(|url| !url.trim().is_empty())
                        .map(force_https)
                        .or_else(|| singer_cover_url(&mid));
                    Some(SearchArtist {
                        mid,
                        name,
                        cover_url,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let album = value.get("album").unwrap_or(&Value::Null);
    let album_name = string_field(album, &["name", "title"]).unwrap_or_default();
    let album_mid = string_field(album, &["mid", "pmid", "albumMid", "albummid"])
        .or_else(|| string_field(value, &["albumMid", "albummid"]))
        .unwrap_or_default();
    let cover_url = string_field(album, &["coverUrl", "picUrl", "picurl"])
        .filter(|value| !value.is_empty())
        .map(force_https)
        .or_else(|| album_cover_url(&album_mid));

    Ok(Track {
        song_id: integer_field(value, &["id", "songid", "songId"]),
        song_type: integer_field(value, &["type", "songtype", "songType"]).unwrap_or_default(),
        mid,
        media_mid,
        standard_size_bytes,
        high_size_bytes,
        lossless_size_bytes,
        hi_res_size_bytes,
        atmos_stereo_size_bytes,
        atmos_surround_size_bytes,
        master_size_bytes,
        title,
        artists,
        artist_details,
        album: album_name,
        album_mid,
        cover_url,
        duration_seconds: integer_field(value, &["interval"]).unwrap_or_default(),
    })
}

fn recommendation_tracks(data: &Value, keys: &[&str]) -> Result<Vec<Track>> {
    let values = find_array_recursively(data, keys).context("推荐结果缺少歌曲列表")?;
    values.iter().map(parse_track).collect()
}

fn parse_created_playlist(value: &Value) -> Option<UserPlaylist> {
    let tid = integer_field(value, &["tid", "id", "dissid"])?;
    let dir_id = integer_field(value, &["dirId", "dirid"]).unwrap_or_default();
    if dir_id == 201 {
        return None;
    }
    parse_playlist_summary(value, UserPlaylistId::Created { tid, dir_id })
}

fn parse_favorite_playlist(value: &Value) -> Option<UserPlaylist> {
    let diss_id = integer_field(value, &["dissid", "tid", "id"])?;
    parse_playlist_summary(value, UserPlaylistId::Favorite { diss_id })
}

fn parse_recommended_playlist_page(data: &Value, offset: u64) -> Result<SearchPage<UserPlaylist>> {
    let entries = data
        .get("List")
        .and_then(Value::as_array)
        .context("推荐歌单结果缺少 List")?;
    let mut seen = HashSet::new();
    let items = entries
        .iter()
        .filter_map(|entry| {
            entry
                .get("Playlist")
                .and_then(|playlist| playlist.get("basic"))
        })
        .filter_map(parse_recommended_playlist)
        .filter(|playlist| seen.insert(playlist.id.clone()))
        .collect::<Vec<_>>();
    let next_offset = integer_field(data, &["FromLimit", "fromLimit", "from_limit"])
        .unwrap_or_else(|| offset.saturating_add(entries.len() as u64));
    let has_more =
        bool_field(data, &["HasMore", "hasMore", "has_more"]).unwrap_or(!items.is_empty());
    Ok(SearchPage {
        items,
        has_more,
        next_offset,
    })
}

fn parse_recommended_playlist(value: &Value) -> Option<UserPlaylist> {
    let diss_id = integer_field(value, &["tid", "dissid", "dissId", "id"])?;
    let title = string_field(value, &["title", "dissname", "dissName", "name"])
        .filter(|title| !title.trim().is_empty())?;
    let cover_url = value
        .get("cover")
        .and_then(|cover| string_field(cover, &["default_url", "defaultUrl", "url"]))
        .or_else(|| string_field(value, &["imgurl", "imgUrl", "picUrl", "logo"]))
        .filter(|url| !url.trim().is_empty())
        .map(force_https);
    Some(UserPlaylist {
        id: UserPlaylistId::Recommended { diss_id },
        title,
        cover_url,
        description: string_field(value, &["desc", "description", "subtitle"]).unwrap_or_default(),
        owner: playlist_owner_name(value).unwrap_or_default(),
        owner_avatar_url: playlist_owner_avatar_url(value),
        track_count: integer_field(
            value,
            &[
                "song_cnt",
                "songNum",
                "songnum",
                "song_count",
                "total_song_num",
            ],
        )
        .unwrap_or_default(),
    })
}

fn parse_playlist_summary(value: &Value, id: UserPlaylistId) -> Option<UserPlaylist> {
    let title = string_field(value, &["dirName", "dirname", "dissname", "title", "name"])
        .filter(|value| !value.trim().is_empty())?;
    let cover_url = string_field(
        value,
        &["bigpicUrl", "picUrl", "picurl", "coverUrl", "logo"],
    )
    .filter(|value| !value.trim().is_empty())
    .map(force_https);
    Some(UserPlaylist {
        id,
        title,
        cover_url,
        description: string_field(value, &["desc", "description"]).unwrap_or_default(),
        owner: playlist_owner_name(value).unwrap_or_default(),
        owner_avatar_url: playlist_owner_avatar_url(value),
        track_count: integer_field(value, &["songNum", "songnum", "total_song_num"])
            .unwrap_or_default(),
    })
}

fn playlist_from_detail(data: &Value, fallback: UserPlaylist) -> UserPlaylist {
    let Some(info) = find_object_recursively(data, &["dirinfo", "info"]) else {
        return fallback;
    };
    let value = Value::Object(info.clone());
    let mut playlist =
        parse_playlist_summary(&value, fallback.id.clone()).unwrap_or_else(|| fallback.clone());
    playlist.title = fallback.title;
    playlist.cover_url = fallback.cover_url.or(playlist.cover_url);
    if playlist.description.is_empty() {
        playlist.description = fallback.description;
    }
    if playlist.owner.is_empty() {
        playlist.owner = playlist_owner_name(data).unwrap_or(fallback.owner);
    }
    if playlist.owner_avatar_url.is_none() {
        playlist.owner_avatar_url = playlist_owner_avatar_url(data).or(fallback.owner_avatar_url);
    }
    if playlist.track_count == 0 {
        playlist.track_count = fallback.track_count;
    }
    playlist
}

pub(super) fn playlist_owner_name(value: &Value) -> Option<String> {
    const DIRECT_KEYS: &[&str] = &[
        "creatorName",
        "creator_name",
        "host_nick",
        "hostname",
        "nickname",
        "nick",
    ];
    const CREATOR_KEYS: &[&str] = &["name", "nickname", "nick", "host_nick", "hostname"];

    string_field(value, DIRECT_KEYS)
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            find_object_recursively(value, &["creator", "creatorInfo", "creator_info"])
                .and_then(|creator| find_string_in_object(creator, CREATOR_KEYS))
                .filter(|name| !name.trim().is_empty())
        })
}

pub(super) fn playlist_owner_avatar_url(value: &Value) -> Option<String> {
    const AVATAR_KEYS: &[&str] = &[
        "avatar",
        "avatarUrl",
        "avatarurl",
        "avatar_url",
        "headurl",
        "headUrl",
        "head_url",
        "headpic",
        "headPic",
        "creatorAvatar",
        "creatorAvatarUrl",
    ];

    string_field(value, AVATAR_KEYS)
        .filter(|url| !url.trim().is_empty())
        .or_else(|| {
            find_object_recursively(value, &["creator", "creatorInfo", "creator_info"])
                .and_then(|creator| find_string_in_object(creator, AVATAR_KEYS))
                .filter(|url| !url.trim().is_empty())
        })
        .map(force_https)
}

pub(super) fn album_cover_url(album_mid: &str) -> Option<String> {
    (!album_mid.trim().is_empty()).then(|| {
        format!(
            "https://y.gtimg.cn/music/photo_new/T002R300x300M000{}.jpg?max_age=2592000",
            album_mid.trim()
        )
    })
}

pub(super) fn singer_cover_url(singer_mid: &str) -> Option<String> {
    (!singer_mid.trim().is_empty()).then(|| {
        format!(
            "https://y.gtimg.cn/music/photo_new/T001R300x300M000{}.jpg?max_age=2592000",
            singer_mid.trim()
        )
    })
}

pub(super) fn force_https(url: String) -> String {
    url.strip_prefix("http://")
        .map(|url| format!("https://{url}"))
        .or_else(|| url.strip_prefix("//").map(|url| format!("https://{url}")))
        .unwrap_or(url)
}

fn parse_track_liked(value: &Value, mid: &str) -> Option<bool> {
    value
        .get("m_fan")
        .and_then(Value::as_object)
        .and_then(|liked| liked.get(mid))
        .and_then(bool_value)
}

#[cfg(test)]
mod tests {
    use super::super::playback::playback_filename;
    use super::*;
    use crate::Quality;

    #[test]
    fn parses_liked_track_and_preserves_media_mid() {
        let track = parse_track(&json!({
            "type": 13,
            "mid": "song-mid",
            "title": "A Song",
            "interval": 245,
            "singer": [
                { "name": "Artist A", "mid": "artist-a" },
                { "name": "Artist B", "mid": "artist-b" }
            ],
            "album": { "name": "Album", "mid": "album-mid" },
            "file": { "media_mid": "different-media-mid" }
        }))
        .unwrap();

        assert_eq!(track.mid, "song-mid");
        assert_eq!(track.song_type, 13);
        assert_eq!(track.media_mid.as_deref(), Some("different-media-mid"));
        assert_eq!(track.artists, "Artist A / Artist B");
        assert_eq!(
            track
                .artist_details
                .iter()
                .map(|artist| artist.mid.as_str())
                .collect::<Vec<_>>(),
            ["artist-a", "artist-b"]
        );
        let mut persisted_track = serde_json::to_value(&track).unwrap();
        persisted_track
            .as_object_mut()
            .unwrap()
            .remove("artist_details");
        let restored_track: Track = serde_json::from_value(persisted_track).unwrap();
        assert!(restored_track.artist_details.is_empty());
        assert_eq!(
            playback_filename(&track, Quality::High),
            "M800different-media-mid.mp3"
        );

        let track_without_media_mid = parse_track(&json!({
            "mid": "song-mid",
            "title": "A Song"
        }))
        .unwrap();
        assert_eq!(track_without_media_mid.media_mid, None);
        assert_eq!(
            playback_filename(&track_without_media_mid, Quality::High),
            "M800song-midsong-mid.mp3"
        );
    }

    #[test]
    fn parses_track_liked_response() {
        assert_eq!(
            parse_track_liked(&json!({ "m_fan": { "song-mid": true } }), "song-mid"),
            Some(true)
        );
        assert_eq!(
            parse_track_liked(&json!({ "m_fan": { "song-mid": 0 } }), "song-mid"),
            Some(false)
        );
    }

    #[test]
    fn parses_recommendation_track_wrappers_and_list_casing() {
        let radar = recommendation_tracks(
            &json!({
                "VecSongs": [{
                    "Track": {
                        "mid": "radar-mid",
                        "title": "Radar Song",
                        "interval": 180
                    }
                }]
            }),
            &["VecSongs", "vecSongs"],
        )
        .unwrap();
        assert_eq!(radar[0].mid, "radar-mid");

        let guess = recommendation_tracks(
            &json!({
                "tracks": [{
                    "track": {
                        "mid": "guess-mid",
                        "title": "Guess Song",
                        "interval": 200
                    }
                }]
            }),
            &["Tracks", "tracks"],
        )
        .unwrap();
        assert_eq!(guess[0].mid, "guess-mid");
    }

    #[test]
    fn only_zero_file_metadata_rules_out_a_quality() {
        let missing = parse_track(&json!({
            "mid": "missing-files",
            "title": "Missing Files",
            "file": {
                "size_128mp3": 0,
                "size_320mp3": 0,
                "size_flac": 0,
                "size_hires": 0,
                "size_new": [0, 0, 0]
            }
        }))
        .unwrap();
        let available = parse_track(&json!({
            "mid": "available-files",
            "title": "Available Files",
            "file": {
                "size_128mp3": 4_000_000,
                "size_320mp3": 10_000_000,
                "size_flac": 30_000_000,
                "size_hires": 42_000_000,
                "size_new": [80_000_000, 50_000_000, 70_000_000]
            }
        }))
        .unwrap();
        let unknown = parse_track(&json!({
            "mid": "unknown-files",
            "title": "Unknown Files"
        }))
        .unwrap();

        for quality in Quality::ALL {
            assert!(!missing.metadata_allows_quality(quality));
            assert!(available.metadata_allows_quality(quality));
            assert!(unknown.metadata_allows_quality(quality));
        }
    }

    #[test]
    fn maps_advanced_quality_sizes_from_size_new_indices() {
        let track = parse_track(&json!({
            "mid": "advanced-files",
            "title": "Advanced Files",
            "file": {
                "size_new": [80_000_000, 0, 70_000_000]
            }
        }))
        .unwrap();

        assert_eq!(track.master_size_bytes, Some(80_000_000));
        assert_eq!(track.atmos_stereo_size_bytes, Some(0));
        assert_eq!(track.atmos_surround_size_bytes, Some(70_000_000));
        assert!(track.metadata_allows_quality(Quality::Master));
        assert!(!track.metadata_allows_quality(Quality::AtmosStereo));
        assert!(track.metadata_allows_quality(Quality::AtmosSurround));
    }

    #[test]
    fn normalizes_qq_image_urls_to_https() {
        assert_eq!(
            force_https("//y.gtimg.cn/music/photo.jpg".to_owned()),
            "https://y.gtimg.cn/music/photo.jpg"
        );
        assert_eq!(
            force_https("http://y.gtimg.cn/music/photo.jpg".to_owned()),
            "https://y.gtimg.cn/music/photo.jpg"
        );
    }

    #[test]
    fn playlist_detail_preserves_the_summary_title() {
        let fallback = UserPlaylist {
            id: UserPlaylistId::Created { tid: 30, dir_id: 0 },
            title: "amtoaer的每日30首".to_owned(),
            cover_url: None,
            description: String::new(),
            owner: "amtoaer".to_owned(),
            owner_avatar_url: None,
            track_count: 30,
        };

        let playlist = playlist_from_detail(
            &json!({
                "dirinfo": {
                    "dirName": "的今日私享",
                    "desc": "每日更新",
                    "songNum": 30
                }
            }),
            fallback,
        );

        assert_eq!(playlist.title, "amtoaer的每日30首");
        assert_eq!(playlist.description, "每日更新");
        assert_eq!(playlist.track_count, 30);
    }

    #[test]
    fn playlist_detail_only_fills_a_missing_summary_cover() {
        let detail = json!({
            "dirinfo": {
                "dirName": "今日歌单",
                "bigpicUrl": "http://example.test/detail.jpg"
            }
        });
        let mut summary = UserPlaylist {
            id: UserPlaylistId::Recommended { diss_id: 30 },
            title: "今日歌单".to_owned(),
            cover_url: Some("https://example.test/summary.jpg".to_owned()),
            description: String::new(),
            owner: String::new(),
            owner_avatar_url: None,
            track_count: 30,
        };

        let playlist = playlist_from_detail(&detail, summary.clone());
        assert_eq!(
            playlist.cover_url.as_deref(),
            Some("https://example.test/summary.jpg")
        );

        summary.cover_url = None;
        let playlist = playlist_from_detail(&detail, summary);
        assert_eq!(
            playlist.cover_url.as_deref(),
            Some("https://example.test/detail.jpg")
        );
    }

    #[test]
    fn parses_recommended_playlist_page_and_server_cursor() {
        let page = parse_recommended_playlist_page(
            &json!({
                "List": [{
                    "Playlist": {
                        "basic": {
                            "tid": "4270386076",
                            "title": "每日30首",
                            "cover": { "default_url": "http://example.test/daily.jpg" },
                            "creator": {
                                "nick": "amtoaer",
                                "head_url": "http://example.test/avatar.jpg"
                            },
                            "song_cnt": 30
                        }
                    }
                }],
                "HasMore": 1,
                "FromLimit": 10
            }),
            0,
        )
        .unwrap();

        assert_eq!(page.items.len(), 1);
        assert_eq!(
            page.items[0].id,
            UserPlaylistId::Recommended {
                diss_id: 4_270_386_076
            }
        );
        assert_eq!(page.items[0].title, "每日30首");
        assert_eq!(
            page.items[0].cover_url.as_deref(),
            Some("https://example.test/daily.jpg")
        );
        assert_eq!(page.items[0].owner, "amtoaer");
        assert_eq!(page.items[0].track_count, 30);
        assert!(page.has_more);
        assert_eq!(page.next_offset, 10);
    }

    #[test]
    fn extracts_encrypted_uin_from_nested_response() {
        let value = json!({
            "profile": {
                "creator": {
                    "encrypt_uin": "opaque-user-id"
                }
            }
        });
        assert_eq!(
            find_string_recursively(&value, &["encryptUin", "encrypt_uin"]),
            Some("opaque-user-id".to_owned())
        );
    }
}
