use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use rand::RngExt;
use reqwest::header::{COOKIE, REFERER};
use serde_json::{Value, json};

use super::library::{
    album_cover_url, force_https, parse_track, playlist_owner_avatar_url, playlist_owner_name,
    singer_cover_url,
};
use super::response::{find_array_recursively, integer_field, string_field};
use super::{ProtocolClient, is_credential_rejection_code};
use crate::{
    CredentialError, CredentialSession, SearchAlbum, SearchArtist, SearchPage, SearchResults,
    Track, UserPlaylist, UserPlaylistId,
};

const MOBILE_SEARCH_API_URL: &str = "https://u.y.qq.com/cgi-bin/musicu.fcg";
const MOBILE_SEARCH_CLIENT_VERSION: u64 = 13_020_508;
const MOBILE_SEARCH_QIMEI36: &str = "6c9d3cd110abca9b16311cee10001e717614";
const MOBILE_SEARCH_UID: &str = "3931641530";

const SEARCH_ID_E_BASE: u64 = 18_014_398_509_481_984;
const SEARCH_ID_N_BASE: u64 = 4_294_967_296;
const SEARCH_ID_N_MAX: u64 = 4_194_304;
const DAY_MILLIS: u64 = 24 * 60 * 60 * 1000;

fn get_search_id() -> String {
    let mut rng = rand::rng();
    let e = rng.random_range(1_u64..=20);
    let t = e * SEARCH_ID_E_BASE;
    let n = rng.random_range(0_u64..=SEARCH_ID_N_MAX) * SEARCH_ID_N_BASE;
    let r = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        % DAY_MILLIS as u128) as u64;
    (t + n + r).to_string()
}

impl ProtocolClient {
    pub async fn search(
        &self,
        credential: &CredentialSession,
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
        credential: &CredentialSession,
        query: &str,
        offset: u64,
        limit: u64,
    ) -> Result<SearchPage<Track>> {
        let limit = limit.clamp(1, 50);
        let data = self
            .mobile_search_data(credential, query, offset, limit)
            .await?;
        parse_mobile_search_songs(&data, offset, limit).context("QQ 音乐单曲搜索结果格式发生了变化")
    }

    pub async fn search_artists(
        &self,
        credential: &CredentialSession,
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
        credential: &CredentialSession,
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
        credential: &CredentialSession,
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
        credential: &CredentialSession,
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

    async fn search_data(
        &self,
        credential: &CredentialSession,
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

    async fn mobile_search_data(
        &self,
        credential: &CredentialSession,
        query: &str,
        offset: u64,
        limit: u64,
    ) -> Result<Value> {
        let current = credential.ensure_fresh().await?;
        let page = offset / limit + 1;
        let body = json!({
            "comm": {
                "ct": "11",
                "cv": MOBILE_SEARCH_CLIENT_VERSION,
                "v": MOBILE_SEARCH_CLIENT_VERSION,
                "QIMEI36": MOBILE_SEARCH_QIMEI36,
                "tmeAppID": "qqmusic",
                "format": "json",
                "inCharset": "utf-8",
                "outCharset": "utf-8",
                "uid": MOBILE_SEARCH_UID,
                "qq": current.music_id.to_string(),
                "authst": current.music_key.clone(),
                "tmeLoginType": current.login_type.to_string(),
            },
            "req_0": {
                "module": "music.search.SearchCgiService",
                "method": "DoSearchForQQMusicMobile",
                "param": {
                    "searchid": get_search_id(),
                    "query": query.trim(),
                    "search_type": 0,
                    "num_per_page": limit,
                    "page_num": page,
                    "highlight": 1,
                    "grp": 1,
                    "selectors": {},
                    "vec_selectors": [],
                },
            },
        });
        let response = self
            .client
            .post(MOBILE_SEARCH_API_URL)
            .header(COOKIE, current.cookie())
            .header(REFERER, "https://y.qq.com/")
            .header(
                "User-Agent",
                format!("QQMusic {MOBILE_SEARCH_CLIENT_VERSION}(android 10)"),
            )
            .json(&body)
            .send()
            .await
            .context("QQ 音乐移动端搜索请求失败")?
            .error_for_status()
            .context("QQ 音乐移动端搜索请求被拒绝")?
            .json::<Value>()
            .await
            .context("QQ 音乐移动端搜索响应不是有效 JSON")?;
        let global_code = integer_field(&response, &["code"]).unwrap_or_default();
        if global_code != 0 {
            if is_credential_rejection_code(global_code) {
                credential.revoke();
                return Err(CredentialError::Rejected { code: global_code }.into());
            }
            bail!("QQ 音乐移动端搜索返回错误码 {global_code}");
        }
        let request = response
            .get("req_0")
            .context("QQ 音乐移动端搜索响应缺少 req_0")?;
        let request_code = integer_field(request, &["code"]).unwrap_or_default();
        if request_code != 0 {
            if is_credential_rejection_code(request_code) {
                credential.revoke();
                return Err(CredentialError::Rejected { code: request_code }.into());
            }
            bail!("QQ 音乐移动端搜索返回错误码 {request_code}");
        }
        request
            .get("data")
            .cloned()
            .context("QQ 音乐移动端搜索响应缺少 data")
    }
}

fn parse_mobile_search_songs(data: &Value, offset: u64, limit: u64) -> Result<SearchPage<Track>> {
    let items = data
        .pointer("/body/item_song")
        .and_then(Value::as_array)
        .context("搜索结果缺少 item_song")?
        .iter()
        .map(parse_mobile_search_track)
        .collect::<Result<Vec<_>>>()?;
    let next_page = data
        .get("meta")
        .and_then(|meta| integer_field(meta, &["nextpage"]))
        .filter(|page| *page > 0);
    let has_more = next_page.is_some() && !items.is_empty();
    let next_offset = if has_more {
        (next_page.unwrap() - 1).saturating_mul(limit)
    } else {
        offset.saturating_add(items.len() as u64)
    };
    Ok(SearchPage {
        items,
        has_more,
        next_offset,
    })
}

fn parse_mobile_search_track(value: &Value) -> Result<Track> {
    let mut track = parse_track(value)?;
    let song_type = integer_field(value, &["type"]).unwrap_or_default();
    track.song_type = if song_type == 1 { 0 } else { song_type };
    Ok(track)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_id_is_numeric_and_in_expected_range() {
        let search_id = get_search_id();
        assert!(!search_id.is_empty());
        assert!(search_id.chars().all(|c| c.is_ascii_digit()));

        let value: u64 = search_id.parse().expect("search_id should parse to u64");
        let min = SEARCH_ID_E_BASE;
        let max = (20 * SEARCH_ID_E_BASE) + (SEARCH_ID_N_MAX * SEARCH_ID_N_BASE) + (DAY_MILLIS - 1);
        assert!(value >= min);
        assert!(value <= max);
    }

    #[test]
    fn parses_each_qq_music_search_category() {
        let song_page = parse_mobile_search_songs(
            &json!({
                "body": { "item_song": [{
                    "id": 123,
                    "type": 1,
                    "mid": "song-mid",
                    "title": "Song",
                    "interval": 180,
                    "singer": [{ "name": "Singer" }],
                    "album": { "name": "Album", "mid": "album-mid" },
                    "file": { "media_mid": "media-mid", "size_flac": 1024 }
                }, {
                    "type": 13,
                    "mid": "special-mid",
                    "title": "Special Song"
                }] },
                "meta": { "nextpage": 2 }
            }),
            0,
            20,
        )
        .unwrap();
        assert_eq!(song_page.items[0].song_id, Some(123));
        assert_eq!(song_page.items[0].song_type, 0);
        assert_eq!(song_page.items[0].mid, "song-mid");
        assert_eq!(song_page.items[0].media_mid.as_deref(), Some("media-mid"));
        assert_eq!(song_page.items[1].song_type, 13);
        assert!(song_page.has_more);
        assert_eq!(song_page.next_offset, 20);

        let last_page = parse_mobile_search_songs(
            &json!({
                "body": { "item_song": [] },
                "meta": { "nextpage": -1 }
            }),
            song_page.next_offset,
            20,
        )
        .unwrap();
        assert!(last_page.items.is_empty());
        assert!(!last_page.has_more);
        assert_eq!(last_page.next_offset, 20);

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
}
