use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use reqwest::Client;
use reqwest::header::{COOKIE, REFERER};
use serde_json::{Map, Value, json};
use sha1::{Digest as _, Sha1};

use super::{QqCredential, Quality, Track};

const API_URL: &str = "https://u.y.qq.com/cgi-bin/musics.fcg";
const PROFILE_URL: &str = "https://c6.y.qq.com/rsc/fcgi-bin/fcg_get_profile_homepage.fcg";
const DEFAULT_STREAM_DOMAIN: &str = "http://dl.stream.qqmusic.qq.com/";
const SIGN_PART_1_INDEXES: [usize; 8] = [23, 14, 6, 36, 16, 40, 7, 19];
const SIGN_PART_2_INDEXES: [usize; 8] = [16, 1, 32, 12, 19, 27, 8, 5];
const SIGN_SCRAMBLE_VALUES: [u8; 20] = [
    89, 39, 179, 150, 218, 82, 58, 252, 177, 52, 186, 123, 120, 64, 242, 133, 143, 161, 121, 179,
];

#[derive(Clone)]
pub struct ProtocolClient {
    client: Client,
}

impl ProtocolClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                 AppleWebKit/537.36 Chrome/131.0 Safari/537.36",
            )
            .build()
            .context("无法创建 QQ 音乐 HTTP 客户端")?;
        Ok(Self { client })
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
                bail!("登录成功，但无法补全“我喜欢”所需的用户标识：{error:#}");
            }
            bail!("登录成功，但 QQ 音乐没有返回“我喜欢”所需的用户标识");
        }

        Ok(credential)
    }

    pub async fn liked_tracks(&self, credential: &QqCredential, limit: u64) -> Result<Vec<Track>> {
        let data = self
            .call(
                "music.srfDissInfo.DissInfo",
                "CgiGetDiss",
                json!({
                    "disstid": 0,
                    "dirid": 201,
                    "tag": true,
                    "song_begin": 0,
                    "song_num": limit.clamp(1, 100),
                    "userinfo": true,
                    "orderlist": true,
                    "enc_host_uin": credential.encrypted_uin,
                }),
                credential,
                None,
            )
            .await
            .context("无法加载 QQ 音乐“我喜欢”")?;

        let songs = data
            .get("songlist")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        songs
            .iter()
            .map(parse_track)
            .collect::<Result<Vec<_>>>()
            .context("QQ 音乐“我喜欢”的歌曲数据格式发生了变化")
    }

    pub async fn playback_url(
        &self,
        credential: &QqCredential,
        track: &Track,
        quality: Quality,
    ) -> Result<String> {
        let filename = playback_filename(track, quality);
        let data = self
            .call(
                "music.vkey.GetVkey",
                "UrlGetVkey",
                json!({
                    "filename": [filename],
                    "guid": credential.client_guid,
                    "songmid": [track.mid],
                    "songtype": [0],
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
        let entry = entries
            .iter()
            .find(|entry| string_field(entry, &["purl"]).is_some_and(|value| !value.is_empty()))
            .or_else(|| entries.first())
            .context("QQ 音乐没有返回播放地址候选项")?;

        let purl = string_field(entry, &["purl"]).unwrap_or_default();
        if purl.is_empty() {
            let code = integer_field(entry, &["result"]).unwrap_or_default();
            bail!("所选音质不可播放（上游结果码 {code}）；可能需要对应会员权益或歌曲受版权限制");
        }
        if purl.starts_with("http://") || purl.starts_with("https://") {
            return Ok(purl);
        }

        let domain = data
            .get("sip")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|domain| domain.contains("dl.stream"))
            })
            .unwrap_or(DEFAULT_STREAM_DOMAIN);
        Ok(format!(
            "{}/{}",
            domain.trim_end_matches('/'),
            purl.trim_start_matches('/')
        ))
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
        let response = self
            .client
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
            .context("QQ 音乐用户资料不是有效 JSON")?;

        find_string_recursively(&response, &["encryptUin", "encrypt_uin"])
            .filter(|value| !value.trim().is_empty())
            .context("QQ 音乐用户资料没有包含加密用户标识")
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
        let signature = sign(&body);
        let response = self
            .client
            .post(API_URL)
            .query(&[("sign", signature)])
            .header(COOKIE, credential.cookie())
            .header(REFERER, "https://y.qq.com/portal/player.html")
            .json(&body)
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
    let value = value
        .get("songInfo")
        .or_else(|| value.get("track"))
        .unwrap_or(value);
    let mid = string_field(value, &["mid", "songmid"])
        .filter(|value| !value.is_empty())
        .context("歌曲缺少 mid")?;
    let title = string_field(value, &["title", "name"])
        .filter(|value| !value.is_empty())
        .context("歌曲缺少标题")?;

    let file = value.get("file").unwrap_or(&Value::Null);
    let media_mid = string_field(file, &["media_mid", "mediaMid"])
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| mid.clone());
    let artists = value
        .get("singer")
        .or_else(|| value.get("singers"))
        .and_then(Value::as_array)
        .map(|artists| {
            artists
                .iter()
                .filter_map(|artist| string_field(artist, &["name", "title"]))
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default();
    let album = value.get("album").unwrap_or(&Value::Null);
    let album_name = string_field(album, &["name", "title"]).unwrap_or_default();

    Ok(Track {
        mid,
        media_mid,
        title,
        artists,
        album: album_name,
        duration_seconds: integer_field(value, &["interval"]).unwrap_or_default(),
    })
}

fn playback_filename(track: &Track, quality: Quality) -> String {
    let (prefix, extension) = quality.file_parts();
    let media_mid = if track.media_mid.is_empty() {
        &track.mid
    } else {
        &track.media_mid
    };
    format!("{prefix}{}{media_mid}{extension}", track.mid)
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(value_to_string)
}

fn integer_field(value: &Value, keys: &[&str]) -> Option<u64> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(|value| match value {
            Value::Number(number) => number.as_u64(),
            Value::String(value) => value.parse().ok(),
            _ => None,
        })
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
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
    use super::*;

    #[test]
    fn qq_signature_matches_upstream_vector() {
        let body = json!({ "foo": "bar", "num": 1 });
        assert_eq!(sign(&body), "zzcf3ea51dcp3xdwnxisjgufsk0znclehf2t85bc1d3d4");
    }

    #[test]
    fn parses_liked_track_and_preserves_media_mid() {
        let track = parse_track(&json!({
            "mid": "song-mid",
            "title": "A Song",
            "interval": 245,
            "singer": [{ "name": "Artist A" }, { "name": "Artist B" }],
            "album": { "name": "Album", "mid": "album-mid" },
            "file": { "media_mid": "different-media-mid" }
        }))
        .unwrap();

        assert_eq!(track.mid, "song-mid");
        assert_eq!(track.media_mid, "different-media-mid");
        assert_eq!(track.artists, "Artist A / Artist B");
        assert_eq!(
            playback_filename(&track, Quality::High),
            "M800song-middifferent-media-mid.mp3"
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
