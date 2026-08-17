use std::collections::HashSet;
use std::io::Read as _;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use flate2::read::ZlibDecoder;
use futures_util::StreamExt as _;
use reqwest::Client;
use reqwest::header::{ACCEPT_ENCODING, COOKIE, RANGE, REFERER};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha1::{Digest as _, Sha1};
use tokio::sync::{Mutex, RwLock};

use crate::models::LyricResult;
use crate::platform::get_search_id;

use super::{
    PlaybackOption, PlaylistPage, QqCredential, Quality, RadarTrackPage, SearchAlbum, SearchArtist,
    SearchPage, SearchResults, Track, UserPlaylist, UserPlaylistId, UserProfile, new_client_guid,
    qrc_des,
};

const API_URL: &str = "https://u.y.qq.com/cgi-bin/musics.fcg";
const PROFILE_URL: &str = "https://c6.y.qq.com/rsc/fcgi-bin/fcg_get_profile_homepage.fcg";
const DEFAULT_STREAM_DOMAIN: &str = "http://dl.stream.qqmusic.qq.com/";
const CDN_PROBE_BYTES: usize = 64 * 1024;
const CDN_PROBE_NODE_LIMIT: usize = 4;
const CDN_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
const DEFAULT_CDN_REFRESH: Duration = Duration::from_secs(30 * 60);
const MIN_CDN_REFRESH: Duration = Duration::from_secs(60);
const SIGN_PART_1_INDEXES: [usize; 8] = [23, 14, 6, 36, 16, 40, 7, 19];
const SIGN_PART_2_INDEXES: [usize; 8] = [16, 1, 32, 12, 19, 27, 8, 5];
const SIGN_SCRAMBLE_VALUES: [u8; 20] = [
    89, 39, 179, 150, 218, 82, 58, 252, 177, 52, 186, 123, 120, 64, 242, 133, 143, 161, 121, 179,
];

#[derive(Clone)]
pub struct ProtocolClient {
    client: Client,
    cdn: Arc<RwLock<CdnCache>>,
    cdn_refresh: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct CdnCache {
    client_guid: String,
    domains: Vec<String>,
    ranked_domains: Vec<String>,
    test_file: String,
    fetched_at: u64,
    refresh_time: u64,
    cache_time: u64,
    expiration: u64,
    measured_at: u64,
}

impl Default for CdnCache {
    fn default() -> Self {
        Self {
            client_guid: new_client_guid(),
            domains: Vec::new(),
            ranked_domains: Vec::new(),
            test_file: String::new(),
            fetched_at: 0,
            refresh_time: DEFAULT_CDN_REFRESH.as_secs(),
            cache_time: DEFAULT_CDN_REFRESH.as_secs(),
            expiration: DEFAULT_CDN_REFRESH.as_secs(),
            measured_at: 0,
        }
    }
}

impl CdnCache {
    fn normalized(mut self) -> Self {
        if self.client_guid.trim().is_empty() {
            self.client_guid = new_client_guid();
        }

        let mut domains = Vec::new();
        append_unique_domains(&mut domains, self.domains);
        let has_direct_domain = domains.iter().any(|domain| !is_ws_stream_domain(domain));
        if has_direct_domain {
            domains.retain(|domain| !is_ws_stream_domain(domain));
        }

        let mut ranked_domains = Vec::new();
        append_unique_domains(&mut ranked_domains, self.ranked_domains);
        ranked_domains.retain(|domain| domains.contains(domain));
        append_unique_domains(&mut ranked_domains, domains.iter());

        self.domains = domains;
        self.ranked_domains = ranked_domains;
        self.refresh_time = self.refresh_time.max(MIN_CDN_REFRESH.as_secs());
        self.cache_time = self.cache_time.max(MIN_CDN_REFRESH.as_secs());
        self.expiration = self.expiration.max(MIN_CDN_REFRESH.as_secs());
        self
    }

    fn is_valid_at(&self, now: u64) -> bool {
        self.fetched_at != 0
            && !self.domains.is_empty()
            && now < self.fetched_at.saturating_add(self.expiration)
    }

    fn refresh_delay_at(&self, now: u64) -> Duration {
        if !self.is_valid_at(now) {
            return Duration::ZERO;
        }
        let refresh_at = self
            .fetched_at
            .saturating_add(self.refresh_time.min(self.cache_time).min(self.expiration));
        Duration::from_secs(refresh_at.saturating_sub(now))
    }

    fn measurement_is_fresh_at(&self, now: u64) -> bool {
        self.is_valid_at(now)
            && self.measured_at != 0
            && now < self.measured_at.saturating_add(self.cache_time)
    }

    fn has_same_nodes(&self, other: &Self) -> bool {
        self.domains.len() == other.domains.len()
            && self
                .domains
                .iter()
                .all(|domain| other.domains.contains(domain))
    }

    fn playback_domains_at(&self, now: u64) -> Vec<String> {
        if self.is_valid_at(now) {
            self.ranked_domains.clone()
        } else {
            Vec::new()
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct CdnProbe {
    first_byte: Duration,
    elapsed: Duration,
}

impl CdnProbe {
    fn score(self) -> Duration {
        self.first_byte.saturating_add(self.elapsed)
    }
}

impl ProtocolClient {
    pub fn new() -> Result<Self> {
        Self::new_with_cdn_cache(CdnCache::default())
    }

    pub fn new_with_cdn_cache(cdn_cache: CdnCache) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                 AppleWebKit/537.36 Chrome/131.0 Safari/537.36",
            )
            .build()
            .context("无法创建 QQ 音乐 HTTP 客户端")?;
        Ok(Self {
            client,
            cdn: Arc::new(RwLock::new(cdn_cache.normalized())),
            cdn_refresh: Arc::default(),
        })
    }

    pub async fn cdn_refresh_delay(&self) -> Duration {
        self.cdn.read().await.refresh_delay_at(unix_timestamp())
    }

    pub async fn refresh_cdn(&self) -> Result<CdnCache> {
        let _refresh = self.cdn_refresh.lock().await;
        let previous = self.cdn.read().await.clone();
        let data = self
            .call_anonymous(
                "music.audioCdnDispatch.cdnDispatch",
                "GetCdnDispatch",
                json!({
                    "guid": previous.client_guid.clone(),
                    "uid": "0",
                    "use_new_domain": 1,
                    "use_ipv6": 1,
                }),
                &previous.client_guid,
            )
            .await
            .context("无法获取 QQ 音乐 CDN 调度信息")?;
        let now = unix_timestamp();
        let mut dispatch = parse_cdn_dispatch(&data, previous.client_guid.clone(), now)?;

        let reuse_measurement =
            previous.has_same_nodes(&dispatch) && previous.measurement_is_fresh_at(now);
        if reuse_measurement {
            dispatch.ranked_domains = previous.ranked_domains;
            dispatch.measured_at = previous.measured_at;
        }

        // Publish the service-provided order immediately. Probing runs in the
        // caller's background maintenance task and never blocks song changes.
        *self.cdn.write().await = dispatch.clone();

        if !reuse_measurement
            && !dispatch.test_file.is_empty()
            && let Some(ranked_domains) = self
                .rank_cdn_domains(&dispatch.domains, &dispatch.test_file)
                .await
        {
            dispatch.ranked_domains = ranked_domains;
            dispatch.measured_at = now;
            *self.cdn.write().await = dispatch.clone();
        }

        Ok(dispatch)
    }

    pub async fn complete_credential(&self, mut credential: QqCredential) -> Result<QqCredential> {
        if !credential.encrypted_uin.trim().is_empty() {
            return Ok(credential);
        }

        let refresh_error = match self.refresh_full_credential(&credential).await {
            Ok(data) => {
                apply_credential_response(&mut credential, &data);
                None
            }
            Err(error) => Some(error),
        };

        if credential.encrypted_uin.trim().is_empty() {
            if let Ok(encrypted_uin) = self.fetch_encrypted_uin(&credential).await {
                credential.encrypted_uin = encrypted_uin;
            }
        }

        if credential.encrypted_uin.trim().is_empty() {
            if let Some(error) = refresh_error {
                bail!("登录成功，但无法补全“已点赞的歌曲”所需的用户标识：{error:#}");
            }
            bail!("登录成功，但 QQ 音乐没有返回“已点赞的歌曲”所需的用户标识");
        }

        Ok(credential)
    }

    pub async fn user_profile(&self, credential: &QqCredential) -> Result<UserProfile> {
        let primary = self
            .call(
                "music.UserInfo.userInfoServer",
                "GetLoginUserInfo",
                json!({}),
                credential,
                None,
            )
            .await
            .ok();
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
            self.fetch_legacy_profile(credential).await.ok()
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
                    credential.music_id
                ))
            });
        let id = profiles
            .into_iter()
            .flatten()
            .find_map(|profile| {
                find_string_recursively(profile, &["str_musicid", "musicid", "music_id", "uin"])
                    .filter(|value| value != "0")
            })
            .unwrap_or_else(|| credential.music_id.to_string());

        Ok(UserProfile {
            id,
            nickname,
            avatar_url,
        })
    }

    pub async fn user_playlists(&self, credential: &QqCredential) -> Result<Vec<UserPlaylist>> {
        let liked_data = self
            .playlist_data(credential, &UserPlaylistId::Liked, 0, 1)
            .await
            .context("无法读取“已点赞的歌曲”概要")?;
        let mut liked = playlist_from_detail(&liked_data, UserPlaylist::liked());
        liked.track_count =
            integer_field(&liked_data, &["total_song_num", "total"]).unwrap_or_default();

        let created_data = self
            .call(
                "music.musicasset.PlaylistBaseRead",
                "GetPlaylistByUin",
                json!({ "uin": credential.music_id.to_string() }),
                credential,
                None,
            )
            .await
            .context("无法加载用户创建的 QQ 音乐歌单")?;
        let created = find_array_recursively(&created_data, &["v_playlist", "playlist"])
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(parse_created_playlist)
            .collect::<Vec<_>>();

        let mut favorites = Vec::new();
        let mut offset = 0_u64;
        loop {
            let data = self
                .call(
                    "music.musicasset.PlaylistFavRead",
                    "CgiGetPlaylistFavInfo",
                    json!({
                        "uin": credential.encrypted_uin,
                        "offset": offset,
                        "size": 100,
                    }),
                    credential,
                    None,
                )
                .await
                .context("无法加载用户收藏的 QQ 音乐歌单")?;
            let page = find_array_recursively(&data, &["v_list", "playlist"])
                .cloned()
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
        credential: &QqCredential,
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
        credential: &QqCredential,
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

    pub async fn guess_tracks(&self, credential: &QqCredential, limit: u64) -> Result<Vec<Track>> {
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

    pub async fn search(
        &self,
        credential: &QqCredential,
        query: &str,
        limit: u64,
    ) -> Result<SearchResults> {
        let query = query.trim();
        if query.is_empty() {
            bail!("搜索关键词不能为空");
        }
        let (songs, artists, albums, playlists) = tokio::try_join!(
            self.search_songs(credential, query, 0, limit),
            self.search_artists(credential, query, 0, limit),
            self.search_albums(credential, query, 0, limit),
            self.search_playlists(credential, query, 0, limit),
        )?;
        Ok(SearchResults {
            songs,
            artists,
            albums,
            playlists,
        })
    }

    pub async fn search_songs(
        &self,
        credential: &QqCredential,
        query: &str,
        offset: u64,
        limit: u64,
    ) -> Result<SearchPage<Track>> {
        let data = self
            .search_data(credential, query, 0, offset, limit)
            .await?;
        parse_search_page(&data, "song", offset, parse_track)
            .context("QQ 音乐单曲搜索结果格式发生了变化")
    }

    pub async fn search_artists(
        &self,
        credential: &QqCredential,
        query: &str,
        offset: u64,
        limit: u64,
    ) -> Result<SearchPage<SearchArtist>> {
        let data = self
            .search_data(credential, query, 1, offset, limit)
            .await?;
        parse_search_page(&data, "singer", offset, parse_search_artist)
            .context("QQ 音乐歌手搜索结果格式发生了变化")
    }

    pub async fn search_albums(
        &self,
        credential: &QqCredential,
        query: &str,
        offset: u64,
        limit: u64,
    ) -> Result<SearchPage<SearchAlbum>> {
        let data = self
            .search_data(credential, query, 2, offset, limit)
            .await?;
        parse_search_page(&data, "album", offset, parse_search_album)
            .context("QQ 音乐专辑搜索结果格式发生了变化")
    }

    pub async fn search_playlists(
        &self,
        credential: &QqCredential,
        query: &str,
        offset: u64,
        limit: u64,
    ) -> Result<SearchPage<UserPlaylist>> {
        let data = self
            .search_data(credential, query, 3, offset, limit)
            .await?;
        parse_search_page(&data, "songlist", offset, parse_search_playlist)
            .context("QQ 音乐歌单搜索结果格式发生了变化")
    }

    pub async fn artist_albums(
        &self,
        credential: &QqCredential,
        artist: &SearchArtist,
        offset: u64,
        limit: u64,
    ) -> Result<SearchPage<SearchAlbum>> {
        let limit = limit.clamp(1, 100);
        let data = self
            .call(
                "music.musichallAlbum.AlbumListServer",
                "GetAlbumList",
                json!({
                    "singerMid": artist.mid,
                    "begin": offset,
                    "number": limit,
                    "order": 1,
                }),
                credential,
                None,
            )
            .await
            .with_context(|| format!("无法加载歌手“{}”的专辑", artist.name))?;
        parse_artist_album_page(&data, artist, offset, limit)
            .context("QQ 音乐歌手专辑列表格式发生了变化")
    }

    pub async fn playlist_page(
        &self,
        credential: &QqCredential,
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
            .cloned()
            .unwrap_or_default();
        let tracks = songs
            .iter()
            .map(parse_track)
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("QQ 音乐歌单“{}”的数据格式发生了变化", playlist.title))?;
        let total = integer_field(&data, &["total_song_num", "total", "totalNum", "total_num"])
            .unwrap_or_else(|| offset.saturating_add(tracks.len() as u64));
        let next_offset = offset.saturating_add(tracks.len() as u64);
        let has_more = bool_field(&data, &["hasmore", "has_more"]).unwrap_or(next_offset < total)
            && !tracks.is_empty();

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

    pub async fn liked_tracks(&self, credential: &QqCredential, limit: u64) -> Result<Vec<Track>> {
        Ok(self
            .playlist_page(credential, &UserPlaylist::liked(), 0, limit)
            .await?
            .tracks)
    }

    pub async fn track_liked(&self, credential: &QqCredential, mid: &str) -> Result<bool> {
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
        credential: &QqCredential,
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
                    "tid": 0,
                    "bFmtUtf8": true,
                    "v_songInfo": [{
                        "songId": song_id,
                        "songType": track.song_type,
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

    pub async fn lyrics(&self, credential: &QqCredential, track: &Track) -> Result<LyricResult> {
        let preferred = self
            .call(
                "music.musichallSong.PlayLyricInfo",
                "GetPlayLyricInfo",
                json!({
                    "crypt": 1,
                    "lrc_t": 0,
                    "qrc": 1,
                    "qrc_t": 0,
                    "roma": 1,
                    "roma_t": 0,
                    "songMid": track.mid,
                    "trans": 1,
                    "trans_t": 0,
                    "type": 1,
                }),
                credential,
                None,
            )
            .await;
        if let Ok(lyrics) = preferred.and_then(|data| parse_lyric_result(&data, &track.mid))
            && !lyrics.lyric.trim().is_empty()
        {
            return Ok(lyrics);
        }

        let fallback = self
            .call(
                "music.musichallSong.PlayLyricInfo",
                "GetPlayLyricInfo",
                json!({
                    "crypt": 0,
                    "roma": 0,
                    "songMID": track.mid,
                    "trans": 1,
                    "type": 0,
                }),
                credential,
                None,
            )
            .await
            .with_context(|| format!("无法加载“{}”的歌词", track.title))?;
        parse_lyric_result(&fallback, &track.mid)
            .with_context(|| format!("无法解析“{}”的歌词", track.title))
    }

    pub async fn playback_url(
        &self,
        credential: &QqCredential,
        track: &Track,
        quality: Quality,
    ) -> Result<String> {
        self.playback_options_for(credential, track, &[quality])
            .await?
            .into_iter()
            .next()
            .map(|option| option.url)
            .with_context(|| {
                format!(
                    "“{}”的{}当前不可播放；可能需要对应会员权益或歌曲受版权限制",
                    track.title,
                    quality.label()
                )
            })
    }

    pub async fn playback_options(
        &self,
        credential: &QqCredential,
        track: &Track,
    ) -> Result<Vec<PlaybackOption>> {
        self.playback_options_for(credential, track, &Quality::ALL)
            .await
    }

    async fn playback_options_for(
        &self,
        credential: &QqCredential,
        track: &Track,
        qualities: &[Quality],
    ) -> Result<Vec<PlaybackOption>> {
        let requests = qualities
            .iter()
            .copied()
            .filter(|quality| track.metadata_allows_quality(*quality))
            .map(|quality| (quality, playback_filename(track, quality)))
            .collect::<Vec<_>>();
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let filenames = requests
            .iter()
            .map(|(_, filename)| filename.clone())
            .collect::<Vec<_>>();
        let song_mid = vec![track.mid.clone(); requests.len()];
        let song_type = vec![0; requests.len()];
        let data = self
            .call(
                "music.vkey.GetVkey",
                "UrlGetVkey",
                json!({
                    "filename": filenames,
                    "guid": credential.client_guid,
                    "songmid": song_mid,
                    "songtype": song_type,
                    "uin": credential.music_id.to_string(),
                    "ctx": 0,
                }),
                credential,
                None,
            )
            .await
            .with_context(|| format!("无法获取“{}”的播放地址", track.title))?;

        let entries = data
            .get("midurlinfo")
            .and_then(Value::as_array)
            .context("QQ 音乐播放地址响应缺少 midurlinfo")?;

        let mut domains = self.cached_cdn_domains().await;
        append_unique_domains(&mut domains, playback_stream_domains(&data));
        append_unique_domain(&mut domains, DEFAULT_STREAM_DOMAIN);
        let mut seen = HashSet::new();
        Ok(entries
            .iter()
            .filter_map(|entry| {
                playback_entry_succeeded(entry).then_some(())?;
                let purl = string_field(entry, &["purl"]).filter(|purl| !purl.trim().is_empty())?;
                let quality = requests
                    .iter()
                    .map(|(quality, _)| *quality)
                    .find(|quality| playback_path_matches_quality(&purl, *quality))?;
                seen.insert(quality).then_some(())?;
                let mut urls = playback_urls(&domains, &purl);
                let url = urls.first()?.clone();
                let fallback_urls = urls.drain(1..).collect();
                Some(PlaybackOption {
                    quality,
                    url,
                    fallback_urls,
                })
            })
            .collect())
    }

    async fn cached_cdn_domains(&self) -> Vec<String> {
        self.cdn.read().await.playback_domains_at(unix_timestamp())
    }

    async fn rank_cdn_domains(&self, domains: &[String], test_file: &str) -> Option<Vec<String>> {
        let mut successful = Vec::new();
        for (index, domain) in domains.iter().take(CDN_PROBE_NODE_LIMIT).enumerate() {
            if let Some(probe) = self.probe_cdn(domain, test_file).await {
                successful.push((index, probe.score()));
            }
        }
        successful.sort_by_key(|(_, score)| *score);
        if successful.is_empty() {
            return None;
        }

        let mut ranked = Vec::with_capacity(domains.len());
        for (index, _) in successful {
            append_unique_domain(&mut ranked, &domains[index]);
        }
        append_unique_domains(&mut ranked, domains.iter().cloned());
        Some(ranked)
    }

    async fn probe_cdn(&self, domain: &str, test_file: &str) -> Option<CdnProbe> {
        let url = playback_url(domain, &playback_path_and_query(test_file));
        let started = Instant::now();
        let response = self
            .client
            .get(url)
            .header(REFERER, "https://y.qq.com/")
            .header(ACCEPT_ENCODING, "identity")
            .header(RANGE, format!("bytes=0-{}", CDN_PROBE_BYTES - 1))
            .timeout(CDN_PROBE_TIMEOUT)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        let mut stream = response.bytes_stream();
        let mut received = 0;
        let mut first_byte = None;
        while received < CDN_PROBE_BYTES {
            let Some(chunk) = stream.next().await else {
                break;
            };
            let chunk = chunk.ok()?;
            if chunk.is_empty() {
                continue;
            }
            first_byte.get_or_insert_with(|| started.elapsed());
            received += chunk.len().min(CDN_PROBE_BYTES - received);
        }
        Some(CdnProbe {
            first_byte: first_byte?,
            elapsed: started.elapsed(),
        })
    }

    async fn playlist_data(
        &self,
        credential: &QqCredential,
        id: &UserPlaylistId,
        offset: u64,
        limit: u64,
    ) -> Result<Value> {
        let (diss_id, dir_id, encrypted_uin) = match id {
            UserPlaylistId::Liked => (0, 201, Some(credential.encrypted_uin.as_str())),
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
        self.call(
            "music.srfDissInfo.DissInfo",
            "CgiGetDiss",
            param,
            credential,
            None,
        )
        .await
    }

    async fn search_data(
        &self,
        credential: &QqCredential,
        query: &str,
        search_type: u8,
        offset: u64,
        limit: u64,
    ) -> Result<Value> {
        let limit = limit.clamp(1, 50);
        let page = offset / limit + 1;
        self.call(
            "music.search.SearchCgiService",
            "DoSearchForQQMusicDesktop",
            json!({
                "grp": 0,
                "num_per_page": limit,
                "page_num": page,
                "query": query.trim(),
                "search_type": search_type,
                "searchid": get_search_id(),
            }),
            credential,
            None,
        )
        .await
        .with_context(|| format!("无法搜索 QQ 音乐中的“{}”", query.trim()))
    }

    async fn refresh_full_credential(&self, credential: &QqCredential) -> Result<Value> {
        self.call(
            "music.login.LoginServer",
            "Login",
            json!({
                "refresh_key": credential.refresh_key,
                "refresh_token": credential.refresh_token,
                "musickey": credential.music_key,
                "musicid": credential.music_id,
            }),
            credential,
            Some(json!({ "tmeLoginType": credential.login_type })),
        )
        .await
        .context("QQ 音乐没有接受凭据补全请求")
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

    async fn call(
        &self,
        module: &str,
        method: &str,
        param: Value,
        credential: &QqCredential,
        comm_overrides: Option<Value>,
    ) -> Result<Value> {
        let mut comm = json!({
            "ct": 19,
            "cv": 2201,
            "chid": "0",
            "uin": credential.music_id.to_string(),
            "g_tk": hash33(&credential.music_key),
            "guid": credential.client_guid,
        });
        if let Some(overrides) = comm_overrides.and_then(|value| value.as_object().cloned()) {
            let comm = comm
                .as_object_mut()
                .expect("QQ comm is always initialized as an object");
            comm.extend(overrides);
        }

        let body = json!({
            "comm": comm,
            "result": {
                "module": module,
                "method": method,
                "param": param,
            },
        });
        self.send_call(body, Some(credential.cookie())).await
    }

    async fn call_anonymous(
        &self,
        module: &str,
        method: &str,
        param: Value,
        client_guid: &str,
    ) -> Result<Value> {
        let body = json!({
            "comm": {
                "ct": 19,
                "cv": 2201,
                "chid": "0",
                "uin": "0",
                "g_tk": hash33(""),
                "guid": client_guid,
            },
            "result": {
                "module": module,
                "method": method,
                "param": param,
            },
        });
        self.send_call(body, None).await
    }

    async fn send_call(&self, body: Value, cookie: Option<String>) -> Result<Value> {
        let signature = sign(&body);
        let mut request = self
            .client
            .post(API_URL)
            .query(&[("sign", signature)])
            .header(REFERER, "https://y.qq.com/portal/player.html")
            .json(&body);
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie);
        }
        let response = request
            .send()
            .await
            .context("QQ 音乐网关请求失败")?
            .error_for_status()
            .context("QQ 音乐网关拒绝了请求")?
            .json::<Value>()
            .await
            .context("QQ 音乐网关返回了无效 JSON")?;

        let global_code = integer_field(&response, &["code"]).unwrap_or_default();
        if global_code != 0 {
            bail!("QQ 音乐网关返回错误码 {global_code}");
        }

        let result = response
            .get("result")
            .context("QQ 音乐网关响应缺少 result")?;
        let result_code = integer_field(result, &["code"]).unwrap_or_default();
        if result_code != 0 {
            bail!("QQ 音乐业务接口返回错误码 {result_code}");
        }

        result
            .get("data")
            .cloned()
            .context("QQ 音乐网关响应缺少 data")
    }
}

fn apply_credential_response(credential: &mut QqCredential, data: &Value) {
    if let Some(value) = integer_field(data, &["musicid", "music_id"]).filter(|value| *value > 0) {
        credential.music_id = value;
    }
    if let Some(value) =
        string_field(data, &["musickey", "music_key"]).filter(|value| !value.is_empty())
    {
        credential.music_key = value;
    }
    if let Some(value) = string_field(data, &["refresh_token"]).filter(|value| !value.is_empty()) {
        credential.refresh_token = value;
    }
    if let Some(value) = string_field(data, &["refresh_key"]).filter(|value| !value.is_empty()) {
        credential.refresh_key = value;
    }
    if let Some(value) =
        integer_field(data, &["loginType", "login_type"]).filter(|value| *value > 0)
    {
        credential.login_type = value;
    }
    if let Some(value) = integer_field(data, &["expired_at"]).filter(|value| *value > 0) {
        credential.expires_at = Some(value as i64);
    } else if let Some(lifetime) =
        integer_field(data, &["keyExpiresIn", "key_expires_in"]).filter(|value| *value > 0)
    {
        let created = integer_field(data, &["musickeyCreateTime", "musickey_create_time"])
            .filter(|value| *value > 0)
            .unwrap_or_else(unix_timestamp);
        credential.expires_at = Some((created + lifetime) as i64);
    }
    if let Some(value) = find_string_recursively(data, &["encryptUin", "encrypt_uin"])
        .filter(|value| !value.trim().is_empty())
    {
        credential.encrypted_uin = value;
    }
}

fn parse_track(value: &Value) -> Result<Track> {
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

fn parse_search_page<T>(
    data: &Value,
    category: &str,
    offset: u64,
    parse: impl Fn(&Value) -> Result<T>,
) -> Result<SearchPage<T>> {
    let items = data
        .get("body")
        .and_then(|body| body.get(category))
        .and_then(|category| category.get("list"))
        .and_then(Value::as_array)
        .with_context(|| format!("搜索结果缺少 {category}.list"))?;
    let items = items.iter().map(parse).collect::<Result<Vec<_>>>()?;
    let next_offset = offset.saturating_add(items.len() as u64);
    let has_more = data
        .get("meta")
        .and_then(|meta| integer_field(meta, &["nextpage", "nextPage"]))
        .is_some_and(|next_page| next_page > 0)
        && !items.is_empty();
    Ok(SearchPage {
        items,
        has_more,
        next_offset,
    })
}

fn parse_search_artist(value: &Value) -> Result<SearchArtist> {
    let mid = string_field(value, &["singerMID", "singerMid", "mid"])
        .filter(|mid| !mid.trim().is_empty())
        .context("歌手缺少 mid")?;
    let name = string_field(value, &["singerName", "name"])
        .filter(|name| !name.trim().is_empty())
        .context("歌手缺少名称")?;
    let cover_url = string_field(value, &["singerPic", "picUrl", "pic"])
        .filter(|url| !url.trim().is_empty())
        .map(force_https)
        .or_else(|| singer_cover_url(&mid));
    Ok(SearchArtist {
        mid,
        name,
        cover_url,
    })
}

fn parse_search_album(value: &Value) -> Result<SearchAlbum> {
    let mid = string_field(value, &["albumMID", "albumMid", "mid"])
        .filter(|mid| !mid.trim().is_empty())
        .context("专辑缺少 mid")?;
    let title = string_field(value, &["albumName", "name", "title"])
        .filter(|title| !title.trim().is_empty())
        .context("专辑缺少标题")?;
    let cover_url = string_field(value, &["albumPic", "picUrl", "pic"])
        .filter(|url| !url.trim().is_empty())
        .map(force_https)
        .or_else(|| album_cover_url(&mid));
    let artist = string_field(value, &["singerName", "artistName", "artist"]).unwrap_or_default();
    Ok(SearchAlbum {
        mid,
        title,
        cover_url,
        artist,
    })
}

fn parse_artist_album_page(
    data: &Value,
    artist: &SearchArtist,
    offset: u64,
    limit: u64,
) -> Result<SearchPage<SearchAlbum>> {
    let albums = find_array_recursively(data, &["albumList", "album_list"])
        .context("歌手专辑列表缺少 albumList")?;
    let items = albums
        .iter()
        .take(limit as usize)
        .map(|value| {
            let mut album = parse_search_album(value)?;
            if album.artist.is_empty() {
                album.artist = artist.name.clone();
            }
            Ok(album)
        })
        .collect::<Result<Vec<_>>>()?;
    let next_offset = offset.saturating_add(items.len() as u64);
    let total = integer_field(data, &["total", "totalNum", "total_num"]).unwrap_or(next_offset);
    Ok(SearchPage {
        has_more: !items.is_empty() && next_offset < total,
        items,
        next_offset,
    })
}

fn parse_search_playlist(value: &Value) -> Result<UserPlaylist> {
    let diss_id = integer_field(value, &["dissid", "dissId", "id"]).context("歌单缺少 dissid")?;
    let title = string_field(value, &["dissname", "dissName", "title", "name"])
        .filter(|title| !title.trim().is_empty())
        .context("歌单缺少标题")?;
    Ok(UserPlaylist {
        id: UserPlaylistId::Recommended { diss_id },
        title,
        cover_url: string_field(value, &["imgurl", "imgUrl", "picUrl", "logo"])
            .filter(|url| !url.trim().is_empty())
            .map(force_https),
        description: string_field(value, &["desc", "description"]).unwrap_or_default(),
        owner: playlist_owner_name(value).unwrap_or_default(),
        owner_avatar_url: playlist_owner_avatar_url(value),
        track_count: integer_field(value, &["songNum", "songnum", "song_count"])
            .unwrap_or_default(),
    })
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

fn playlist_owner_name(value: &Value) -> Option<String> {
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

fn playlist_owner_avatar_url(value: &Value) -> Option<String> {
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

fn album_cover_url(album_mid: &str) -> Option<String> {
    (!album_mid.trim().is_empty()).then(|| {
        format!(
            "https://y.gtimg.cn/music/photo_new/T002R300x300M000{}.jpg?max_age=2592000",
            album_mid.trim()
        )
    })
}

fn singer_cover_url(singer_mid: &str) -> Option<String> {
    (!singer_mid.trim().is_empty()).then(|| {
        format!(
            "https://y.gtimg.cn/music/photo_new/T001R300x300M000{}.jpg?max_age=2592000",
            singer_mid.trim()
        )
    })
}

fn force_https(url: String) -> String {
    url.strip_prefix("http://")
        .map(|url| format!("https://{url}"))
        .or_else(|| url.strip_prefix("//").map(|url| format!("https://{url}")))
        .unwrap_or(url)
}

fn playback_filename(track: &Track, quality: Quality) -> String {
    let (prefix, extension) = quality.file_parts();
    match track.media_mid.as_deref() {
        Some(media_mid) => format!("{prefix}{media_mid}{extension}"),
        None => format!("{prefix}{mid}{mid}{extension}", mid = track.mid),
    }
}

fn playback_path_matches_quality(path: &str, quality: Quality) -> bool {
    let filename = path
        .split('?')
        .next()
        .unwrap_or(path)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let (prefix, extension) = quality.file_parts();
    filename.starts_with(prefix) && filename.ends_with(extension)
}

fn playback_entry_succeeded(entry: &Value) -> bool {
    integer_field(entry, &["result"]).is_none_or(|result| result == 0)
}

fn parse_cdn_dispatch(data: &Value, client_guid: String, fetched_at: u64) -> Result<CdnCache> {
    let mut domains = Vec::new();
    if let Some(items) = data.get("sip").and_then(Value::as_array) {
        append_unique_domains(&mut domains, items.iter().filter_map(Value::as_str));
    }
    if let Some(items) = data.get("sipinfo").and_then(Value::as_array) {
        append_unique_domains(
            &mut domains,
            items.iter().filter_map(|item| string_field(item, &["cdn"])),
        );
    }
    let has_direct_domain = domains.iter().any(|domain| !is_ws_stream_domain(domain));
    if has_direct_domain {
        domains.retain(|domain| !is_ws_stream_domain(domain));
    }
    if domains.is_empty() {
        bail!("QQ 音乐 CDN 调度没有返回可用节点");
    }

    let refresh_time = cdn_policy_seconds(
        data,
        &["refreshTime", "refresh_time"],
        DEFAULT_CDN_REFRESH.as_secs(),
    );
    let cache_time = cdn_policy_seconds(data, &["cacheTime", "cache_time"], refresh_time);
    let expiration = cdn_policy_seconds(data, &["expiration"], cache_time);
    Ok(CdnCache {
        client_guid,
        ranked_domains: domains.clone(),
        domains,
        test_file: string_field(data, &["keepalivefile", "test_file"]).unwrap_or_default(),
        fetched_at,
        refresh_time,
        cache_time,
        expiration,
        measured_at: 0,
    })
}

fn cdn_policy_seconds(data: &Value, keys: &[&str], fallback: u64) -> u64 {
    integer_field(data, keys)
        .filter(|seconds| *seconds > 0)
        .unwrap_or(fallback)
        .max(MIN_CDN_REFRESH.as_secs())
}

fn playback_stream_domains(data: &Value) -> Vec<String> {
    let mut domains = Vec::new();
    if let Some(items) = data.get("sip").and_then(Value::as_array) {
        append_unique_domains(&mut domains, items.iter().filter_map(Value::as_str));
    }
    let has_direct_domain = domains.iter().any(|domain| !is_ws_stream_domain(domain));
    if has_direct_domain {
        domains.retain(|domain| !is_ws_stream_domain(domain));
    }
    domains
}

fn append_unique_domains<I, S>(domains: &mut Vec<String>, candidates: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for domain in candidates {
        append_unique_domain(domains, domain.as_ref());
    }
}

fn append_unique_domain(domains: &mut Vec<String>, domain: &str) {
    let domain = domain.trim();
    if domain.is_empty() || !(domain.starts_with("http://") || domain.starts_with("https://")) {
        return;
    }
    let domain = format!("{}/", domain.trim_end_matches('/'));
    if !domains.contains(&domain) {
        domains.push(domain);
    }
}

fn playback_urls(domains: &[String], path: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let absolute =
        (path.starts_with("http://") || path.starts_with("https://")).then(|| path.to_owned());
    let path = playback_path_and_query(path);
    for domain in domains {
        let url = playback_url(domain, &path);
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    if let Some(absolute) = absolute
        && !urls.contains(&absolute)
    {
        urls.push(absolute);
    }
    urls
}

fn playback_path_and_query(path: &str) -> String {
    reqwest::Url::parse(path).map_or_else(
        |_| path.trim_start_matches('/').to_owned(),
        |url| {
            let mut value = url.path().trim_start_matches('/').to_owned();
            if let Some(query) = url.query() {
                value.push('?');
                value.push_str(query);
            }
            value
        },
    )
}

fn playback_url(domain: &str, path: &str) -> String {
    format!(
        "{}/{}",
        domain.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg(test)]
fn playback_stream_domain(data: &Value) -> &str {
    data.get("sip")
        .and_then(Value::as_array)
        .and_then(|items| {
            let first = items
                .iter()
                .filter_map(Value::as_str)
                .find(|domain| !domain.trim().is_empty());
            items
                .iter()
                .filter_map(Value::as_str)
                .find(|domain| !domain.trim().is_empty() && !is_ws_stream_domain(domain))
                .or(first)
        })
        .unwrap_or(DEFAULT_STREAM_DOMAIN)
}

fn is_ws_stream_domain(domain: &str) -> bool {
    let domain = domain.trim();
    let host = domain
        .strip_prefix("http://")
        .or_else(|| domain.strip_prefix("https://"))
        .unwrap_or(domain)
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default();
    let label = host.split('.').next().unwrap_or_default();
    label == "ws"
        || label.strip_prefix("ws").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(value_to_string)
}

fn parse_lyric_result(data: &Value, id: &str) -> Result<LyricResult> {
    let encrypted = integer_field(data, &["crypt"]) == Some(1);
    let lyric = string_field(data, &["lyric"])
        .map(|value| decode_lyric_text(value, encrypted))
        .transpose()?
        .unwrap_or_default();
    let trans_lyric = string_field(data, &["trans"])
        .filter(|value| !value.trim().is_empty())
        .map(|value| decode_lyric_text(value, encrypted))
        .transpose()?
        .filter(|value| !value.trim().is_empty());
    let roma_lyric = string_field(data, &["roma"])
        .filter(|value| !value.trim().is_empty())
        .map(|value| decode_lyric_text(value, encrypted))
        .transpose()?
        .filter(|value| !value.trim().is_empty());
    Ok(LyricResult {
        id: id.to_owned(),
        lyric,
        trans_lyric,
        roma_lyric,
    })
}

fn decode_lyric_text(value: String, encrypted: bool) -> Result<String> {
    if encrypted {
        decrypt_qrc_text(&value)
    } else {
        Ok(decode_base64_text(value))
    }
}

fn decrypt_qrc_text(value: &str) -> Result<String> {
    let mut encrypted = hex::decode(value).context("QRC 歌词不是有效的十六进制数据")?;
    if encrypted.len() % 8 != 0 {
        bail!("QRC 歌词密文长度不是 3DES 块大小的整数倍");
    }

    qrc_des::decrypt_in_place(&mut encrypted);

    let mut decoded = String::new();
    ZlibDecoder::new(encrypted.as_slice())
        .read_to_string(&mut decoded)
        .context("无法解压 QRC 歌词")?;
    Ok(decoded)
}

fn decode_base64_text(value: String) -> String {
    base64::engine::general_purpose::STANDARD
        .decode(value.as_bytes())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or(value)
}

fn integer_field(value: &Value, keys: &[&str]) -> Option<u64> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(integer_value)
}

fn integer_array_field(value: &Value, keys: &[&str], index: usize) -> Option<u64> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(Value::as_array)
        .and_then(|values| values.get(index))
        .and_then(integer_value)
}

fn integer_value(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn bool_field(value: &Value, keys: &[&str]) -> Option<bool> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(|value| match value {
            Value::Bool(value) => Some(*value),
            Value::Number(value) => value.as_u64().map(|value| value != 0),
            Value::String(value) => match value.as_str() {
                "1" | "true" | "TRUE" => Some(true),
                "0" | "false" | "FALSE" => Some(false),
                _ => None,
            },
            _ => None,
        })
}

fn parse_track_liked(value: &Value, mid: &str) -> Option<bool> {
    value
        .get("m_fan")
        .and_then(Value::as_object)
        .and_then(|liked| liked.get(mid))
        .and_then(bool_value)
}

fn bool_value(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => value.as_u64().map(|value| value != 0),
        Value::String(value) => match value.as_str() {
            "1" | "true" | "TRUE" => Some(true),
            "0" | "false" | "FALSE" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn find_array_recursively<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {
    match value {
        Value::Object(object) => object
            .iter()
            .find_map(|(key, value)| {
                keys.iter()
                    .any(|candidate| key.eq_ignore_ascii_case(candidate))
                    .then(|| value.as_array())
                    .flatten()
            })
            .or_else(|| {
                object
                    .values()
                    .find_map(|value| find_array_recursively(value, keys))
            }),
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_array_recursively(value, keys)),
        _ => None,
    }
}

fn find_object_recursively<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Map<String, Value>> {
    match value {
        Value::Object(object) => object
            .iter()
            .find_map(|(key, value)| {
                keys.iter()
                    .any(|candidate| key.eq_ignore_ascii_case(candidate))
                    .then(|| value.as_object())
                    .flatten()
            })
            .or_else(|| {
                object
                    .values()
                    .find_map(|value| find_object_recursively(value, keys))
            }),
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_object_recursively(value, keys)),
        _ => None,
    }
}

fn find_string_recursively(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => find_string_in_object(object, keys).or_else(|| {
            object
                .values()
                .find_map(|value| find_string_recursively(value, keys))
        }),
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_string_recursively(value, keys)),
        _ => None,
    }
}

fn find_string_in_object(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    object.iter().find_map(|(key, value)| {
        keys.iter()
            .any(|candidate| key.eq_ignore_ascii_case(candidate))
            .then(|| value_to_string(value))
            .flatten()
    })
}

fn hash33(value: &str) -> u64 {
    value.chars().fold(5_381_u64, |hash, character| {
        hash.wrapping_mul(33).wrapping_add(character as u64)
    }) & 2_147_483_647
}

// Adapted from netease-qq-music-api (MIT); see THIRD_PARTY_NOTICES.md.
fn sign(request: &Value) -> String {
    let payload = serde_json::to_vec(request).expect("serialize QQ Music request");
    let hash = hex::encode_upper(Sha1::digest(payload));
    let hash_bytes = hash.as_bytes();

    let part_1: String = SIGN_PART_1_INDEXES
        .into_iter()
        .filter(|index| *index < hash_bytes.len())
        .map(|index| hash_bytes[index] as char)
        .collect();
    let part_2: String = SIGN_PART_2_INDEXES
        .into_iter()
        .map(|index| hash_bytes[index] as char)
        .collect();

    let mut scrambled = [0_u8; 20];
    for (index, value) in SIGN_SCRAMBLE_VALUES.iter().enumerate() {
        let high = decode_hex_nibble(hash_bytes[index * 2]);
        let low = decode_hex_nibble(hash_bytes[index * 2 + 1]);
        scrambled[index] = value ^ ((high << 4) | low);
    }

    let base64: String = base64::engine::general_purpose::STANDARD
        .encode(scrambled)
        .chars()
        .filter(|character| !matches!(character, '/' | '\\' | '+' | '='))
        .collect();
    format!("zzc{part_1}{base64}{part_2}").to_ascii_lowercase()
}

fn decode_hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        b'A'..=b'F' => value - b'A' + 10,
        _ => unreachable!("SHA-1 hex only contains hexadecimal digits"),
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use flate2::{Compression, write::ZlibEncoder};

    use super::*;

    #[test]
    fn qq_signature_matches_upstream_vector() {
        let body = json!({ "foo": "bar", "num": 1 });
        assert_eq!(sign(&body), "zzcf3ea51dcp3xdwnxisjgufsk0znclehf2t85bc1d3d4");
    }

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
    fn decodes_lyric_and_translation_payloads() {
        let data = json!({
            "lyric": base64::engine::general_purpose::STANDARD.encode("[00:01.000]原文"),
            "trans": base64::engine::general_purpose::STANDARD.encode("[00:01.000]translation"),
        });

        let lyrics = parse_lyric_result(&data, "song-mid").unwrap();

        assert_eq!(lyrics.id, "song-mid");
        assert_eq!(lyrics.lyric, "[00:01.000]原文");
        assert_eq!(
            lyrics.trans_lyric.as_deref(),
            Some("[00:01.000]translation")
        );
        assert_eq!(lyrics.roma_lyric, None);
    }

    #[test]
    fn decrypts_qrc_payloads() {
        let original = "<QrcInfos><LyricInfo><Lyric_1 LyricContent=\"[1000,500]原(1000,250)文(1250,250)\"/></LyricInfo></QrcInfos>";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(original.as_bytes()).unwrap();
        let mut encrypted = encoder.finish().unwrap();
        encrypted.resize(encrypted.len().next_multiple_of(8), 0);
        qrc_des::encrypt_in_place(&mut encrypted);
        let data = json!({
            "crypt": 1,
            "lyric": hex::encode_upper(encrypted),
        });

        let lyrics = parse_lyric_result(&data, "song-mid").unwrap();

        assert_eq!(lyrics.lyric, original);
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
    fn parses_each_qq_music_search_category() {
        let song_page = parse_search_page(
            &json!({
                "body": { "song": { "list": [{
                    "mid": "song-mid",
                    "title": "Song",
                    "interval": 180,
                    "singer": [{ "name": "Singer" }],
                    "album": { "name": "Album", "mid": "album-mid" },
                    "file": { "media_mid": "media-mid", "size_flac": 1024 }
                }] } },
                "meta": { "nextpage": 2 }
            }),
            "song",
            0,
            parse_track,
        )
        .unwrap();
        assert_eq!(song_page.items[0].mid, "song-mid");
        assert_eq!(song_page.items[0].media_mid.as_deref(), Some("media-mid"));
        assert!(song_page.has_more);
        assert_eq!(song_page.next_offset, 1);

        let artist = parse_search_artist(&json!({
            "singerMID": "artist-mid",
            "singerName": "Artist",
            "singerPic": "http://example.test/artist.jpg"
        }))
        .unwrap();
        assert_eq!(artist.mid, "artist-mid");
        assert_eq!(
            artist.cover_url.as_deref(),
            Some("https://example.test/artist.jpg")
        );

        let album = parse_search_album(&json!({
            "albumMID": "album-mid",
            "albumName": "Album",
            "albumPic": "https://example.test/album.jpg",
            "singerName": "Artist"
        }))
        .unwrap();
        assert_eq!(album.title, "Album");
        assert_eq!(album.artist, "Artist");

        let playlist = parse_search_playlist(&json!({
            "dissid": "42",
            "dissname": "Playlist",
            "imgurl": "//example.test/playlist.jpg",
            "creator": {
                "name": "Owner",
                "headUrl": "//example.test/owner.jpg"
            }
        }))
        .unwrap();
        assert_eq!(playlist.id, UserPlaylistId::Recommended { diss_id: 42 });
        assert_eq!(playlist.owner, "Owner");
        assert_eq!(
            playlist.cover_url.as_deref(),
            Some("https://example.test/playlist.jpg")
        );
        assert_eq!(
            playlist.owner_avatar_url.as_deref(),
            Some("https://example.test/owner.jpg")
        );
    }

    #[test]
    fn parses_paginated_artist_albums() {
        let artist = SearchArtist {
            mid: "artist-mid".to_owned(),
            name: "Artist".to_owned(),
            cover_url: None,
        };
        let page = parse_artist_album_page(
            &json!({
                "singerMid": "artist-mid",
                "total": 3,
                "albumList": [
                    {
                        "albumMid": "album-mid",
                        "albumName": "Album"
                    },
                    {
                        "albumMid": "ignored-album-mid",
                        "albumName": "Ignored album"
                    }
                ]
            }),
            &artist,
            0,
            1,
        )
        .unwrap();

        assert_eq!(page.items[0].mid, "album-mid");
        assert_eq!(page.items[0].artist, "Artist");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.next_offset, 1);
        assert!(page.has_more);
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
    fn requires_a_successful_vkey_result_when_it_is_present() {
        assert!(playback_entry_succeeded(&json!({
            "result": 0,
            "purl": "M500song.mp3?vkey=opaque"
        })));
        assert!(playback_entry_succeeded(&json!({
            "purl": "M500song.mp3?vkey=opaque"
        })));
        assert!(!playback_entry_succeeded(&json!({
            "result": 104003,
            "purl": "M500song.mp3?vkey=opaque"
        })));
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
    fn validates_that_a_playback_url_matches_its_requested_quality() {
        assert!(playback_path_matches_quality(
            "https://example.test/RS01song.flac?vkey=opaque",
            Quality::HiRes
        ));
        assert!(!playback_path_matches_quality(
            "https://example.test/M500song.mp3?vkey=opaque",
            Quality::HiRes
        ));
    }

    #[test]
    fn prefers_a_non_ws_stream_domain_returned_by_qq_music() {
        let data = json!({
            "sip": [
                "http://ws6.stream.qqmusic.qq.com/",
                "https://isure.stream.qqmusic.qq.com/",
                "http://dl.stream.qqmusic.qq.com/"
            ]
        });

        assert_eq!(
            playback_stream_domain(&data),
            "https://isure.stream.qqmusic.qq.com/"
        );
        assert_eq!(
            playback_stream_domain(&json!({
                "sip": ["https://ws.stream.qqmusic.qq.com/"]
            })),
            "https://ws.stream.qqmusic.qq.com/"
        );
        assert!(is_ws_stream_domain("http://ws12.stream.qqmusic.qq.com/"));
        assert!(!is_ws_stream_domain("https://isure.stream.qqmusic.qq.com/"));
        assert_eq!(
            playback_stream_domain(&json!({ "sip": [] })),
            DEFAULT_STREAM_DOMAIN
        );
    }

    #[test]
    fn parses_cdn_dispatch_nodes_and_refresh_policy() {
        let dispatch = parse_cdn_dispatch(
            &json!({
                "sip": [
                    "https://ws.stream.qqmusic.qq.com/",
                    "https://first.example/"
                ],
                "sipinfo": [
                    { "cdn": "https://first.example/", "quic": 1 },
                    { "cdn": "https://second.example/" }
                ],
                "keepalivefile": "test/keepalive.bin",
                "refreshTime": 900,
                "cacheTime": 1800,
                "expiration": 3600
            }),
            "installation-guid".to_owned(),
            10_000,
        )
        .expect("parse CDN dispatch");

        assert_eq!(
            dispatch.domains,
            ["https://first.example/", "https://second.example/"]
        );
        assert_eq!(dispatch.test_file, "test/keepalive.bin");
        assert_eq!(dispatch.client_guid, "installation-guid");
        assert_eq!(dispatch.fetched_at, 10_000);
        assert_eq!(dispatch.refresh_time, 900);
        assert_eq!(dispatch.cache_time, 1800);
        assert_eq!(dispatch.expiration, 3600);
        assert_eq!(dispatch.refresh_delay_at(10_200), Duration::from_secs(700));
        assert_eq!(dispatch.refresh_delay_at(10_900), Duration::ZERO);
        assert!(dispatch.is_valid_at(13_599));
        assert!(!dispatch.is_valid_at(13_600));
    }

    #[test]
    fn persisted_cdn_ranking_is_reused_until_its_cache_time() {
        let mut previous = parse_cdn_dispatch(
            &json!({
                "sip": ["https://first.example/", "https://second.example/"],
                "refreshTime": 900,
                "cacheTime": 1800,
                "expiration": 3600
            }),
            "installation-guid".to_owned(),
            10_000,
        )
        .expect("parse previous CDN dispatch");
        previous.ranked_domains.reverse();
        previous.measured_at = 10_100;
        let current = parse_cdn_dispatch(
            &json!({
                "sip": ["https://second.example/", "https://first.example/"],
                "refreshTime": 900,
                "cacheTime": 1800,
                "expiration": 3600
            }),
            "installation-guid".to_owned(),
            10_900,
        )
        .expect("parse current CDN dispatch");

        assert!(previous.has_same_nodes(&current));
        assert!(previous.measurement_is_fresh_at(11_899));
        assert!(!previous.measurement_is_fresh_at(11_900));
    }

    #[tokio::test]
    #[ignore = "requires live QQ Music network access"]
    async fn anonymously_loads_cdn_dispatch() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = ProtocolClient::new().expect("create protocol client");

        let cache = client
            .refresh_cdn()
            .await
            .expect("load anonymous CDN dispatch");

        assert!(!cache.domains.is_empty());
        assert!(cache.refresh_time > 0);
        assert!(cache.cache_time > 0);
        assert!(cache.expiration > 0);
    }

    #[test]
    fn builds_cdn_fallbacks_without_changing_the_vkey_path() {
        let domains = vec![
            "https://fast.example/".to_owned(),
            "https://backup.example/".to_owned(),
        ];
        let urls = playback_urls(
            &domains,
            "https://original.example/C400media.m4a?vkey=opaque",
        );

        assert_eq!(
            urls,
            [
                "https://fast.example/C400media.m4a?vkey=opaque",
                "https://backup.example/C400media.m4a?vkey=opaque",
                "https://original.example/C400media.m4a?vkey=opaque",
            ]
        );
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
