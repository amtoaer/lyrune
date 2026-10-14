use std::collections::HashSet;

use anyhow::{Context as _, Result, bail};
use reqwest::header::{COOKIE, REFERER};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::ProtocolClient;
use super::cdn::{
    DEFAULT_STREAM_DOMAIN, append_unique_domain, append_unique_domains, playback_stream_domains,
    playback_urls,
};
use super::response::{integer_field, optional_integer, optional_string};
use crate::{CredentialSession, PlaybackOption, Quality, Track};

const EVKEY_API_URL: &str = "https://u.y.qq.com/cgi-bin/musicu.fcg";

#[derive(Default, Deserialize)]
#[serde(default)]
struct PlaybackEntry {
    #[serde(deserialize_with = "optional_integer")]
    result: Option<u64>,
    // 外层区分缺失与存在：purl 为 null/非法时仍优先，不能改用 wifiurl。
    #[serde(deserialize_with = "present_path")]
    purl: Option<Option<String>>,
    #[serde(deserialize_with = "optional_string")]
    wifiurl: Option<String>,
    #[serde(deserialize_with = "optional_string")]
    filename: Option<String>,
    #[serde(deserialize_with = "optional_string")]
    ekey: Option<String>,
}

fn present_path<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    optional_string(deserializer).map(Some)
}

impl ProtocolClient {
    pub async fn playback_options(
        &self,
        credential: &CredentialSession,
        track: &Track,
    ) -> Result<Vec<PlaybackOption>> {
        let normal = self
            .playback_options_for(credential, track, &Quality::ALL)
            .await;
        let mut options = match normal {
            Ok(options) if !options.is_empty() => options,
            Ok(_) => self.encrypted_playback_options(credential, track).await?,
            // 加密回退也失败时沿用普通请求的首个错误，保留原有诊断。
            Err(error) => match self.encrypted_playback_options(credential, track).await {
                Ok(options) => options,
                Err(_) => return Err(error),
            },
        };
        options.sort_unstable_by_key(|option| option.quality);
        Ok(options)
    }

    async fn playback_options_for(
        &self,
        credential: &CredentialSession,
        track: &Track,
        qualities: &[Quality],
    ) -> Result<Vec<PlaybackOption>> {
        let current = credential.ensure_fresh().await?;
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
        let song_type = vec![track.song_type; requests.len()];
        let data = self
            .call_with_session(
                "music.vkey.GetVkey",
                "UrlGetVkey",
                json!({
                    "filename": filenames,
                    "guid": current.client_guid,
                    "songmid": song_mid,
                    "songtype": song_type,
                    "uin": current.music_id.to_string(),
                    "ctx": 0,
                }),
                credential,
                &current,
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
                entry.as_object()?;
                let entry = PlaybackEntry::deserialize(entry).ok()?;
                playback_entry_succeeded(&entry).then_some(())?;
                let purl = entry
                    .purl
                    .flatten()
                    .filter(|purl| !purl.trim().is_empty())?;
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
                    encrypted: false,
                    ekey: None,
                })
            })
            .collect())
    }

    async fn encrypted_playback_options(
        &self,
        credential: &CredentialSession,
        track: &Track,
    ) -> Result<Vec<PlaybackOption>> {
        let current = credential.ensure_fresh().await?;
        let media_mid = track.media_mid.as_deref().unwrap_or(&track.mid);
        let candidates = [
            (Quality::Master, "AIM0", ".mflac"),
            (Quality::AtmosStereo, "Q0M0", ".mflac"),
            (Quality::AtmosSurround, "Q0M1", ".mflac"),
            (Quality::Lossless, "F0M0", ".mflac"),
            (Quality::Lossless, "O8M1", ".mgg"),
            (Quality::Lossless, "O8M0", ".mgg"),
            (Quality::High, "O6M0", ".mgg"),
            (Quality::Standard, "O4M0", ".mgg"),
        ];
        let requests = candidates
            .into_iter()
            .filter(|(quality, _, _)| track.metadata_allows_quality(*quality))
            .collect::<Vec<_>>();
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let filename = requests
            .iter()
            .map(|(_, prefix, extension)| format!("{prefix}{media_mid}{extension}"))
            .collect::<Vec<_>>();
        let count = filename.len();
        let body = json!({
            "comm": {
                "authst": current.music_key.clone(),
                "ct": "19",
                "cv": "1859",
                "uin": current.music_id.to_string(),
                "tmeLoginType": "3",
            },
            "req_1": {
                "module": "music.vkey.GetEVkey",
                "method": "CgiGetEVkey",
                "param": {
                    "filename": filename,
                    "guid": "10000",
                    "songmid": vec![media_mid; count],
                    "songtype": vec![1_u8; count],
                    "uin": current.music_id.to_string(),
                    "loginflag": 1,
                    "platform": "27",
                    "ctx": 1,
                },
            },
        });
        let response = self
            .client
            .post(EVKEY_API_URL)
            .header(REFERER, "https://y.qq.com/")
            .header(COOKIE, current.cookie())
            .json(&body)
            .send()
            .await
            .context("QQ 音乐加密播放地址请求失败")?
            .error_for_status()
            .context("QQ 音乐加密播放地址被拒绝")?
            .json::<Value>()
            .await
            .context("QQ 音乐加密播放地址不是有效 JSON")?;
        let top_code = integer_field(&response, &["code"]).unwrap_or_default();
        if top_code != 0 {
            bail!("QQ 音乐加密播放接口返回错误码 {top_code}");
        }
        let request = response
            .get("req_1")
            .context("QQ 音乐加密播放响应缺少 req_1")?;
        let request_code = integer_field(request, &["code"]).unwrap_or_default();
        if request_code != 0 {
            bail!("QQ 音乐加密播放接口返回错误码 {request_code}");
        }
        let entries = request
            .get("data")
            .and_then(|data| data.get("midurlinfo"))
            .and_then(Value::as_array)
            .context("QQ 音乐加密播放响应缺少 midurlinfo")?;
        let mut domains = self.cached_cdn_domains().await;
        append_unique_domains(
            &mut domains,
            request
                .get("data")
                .map(playback_stream_domains)
                .unwrap_or_default(),
        );
        append_unique_domain(&mut domains, DEFAULT_STREAM_DOMAIN);
        let mut seen = HashSet::new();
        Ok(entries
            .iter()
            .filter_map(|entry| {
                entry.as_object()?;
                let entry = PlaybackEntry::deserialize(entry).ok()?;
                if !playback_entry_succeeded(&entry) {
                    return None;
                }
                let purl = entry
                    .purl
                    .unwrap_or(entry.wifiurl)
                    .filter(|value| !value.trim().is_empty())?;
                let filename = entry.filename.unwrap_or_default();
                let quality =
                    encrypted_path_quality(&filename).or_else(|| encrypted_path_quality(&purl))?;
                if !seen.insert(quality) {
                    return None;
                }
                let ekey = entry.ekey.filter(|value| !value.is_empty())?;
                let mut urls = playback_urls(&domains, &purl);
                let url = urls.first()?.clone();
                let fallback_urls = urls.drain(1..).collect();
                Some(PlaybackOption {
                    quality,
                    url,
                    fallback_urls,
                    encrypted: true,
                    ekey: Some(ekey),
                })
            })
            .collect())
    }
}

pub(super) fn playback_filename(track: &Track, quality: Quality) -> String {
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

fn encrypted_path_quality(path: &str) -> Option<Quality> {
    let filename = path
        .split('?')
        .next()
        .unwrap_or(path)
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    [
        ("AIM0", Quality::Master),
        ("Q0M0", Quality::AtmosStereo),
        ("Q0M1", Quality::AtmosSurround),
        ("F0M0", Quality::Lossless),
        ("O8M1", Quality::Lossless),
        ("O8M0", Quality::Lossless),
        ("O6M0", Quality::High),
        ("O4M0", Quality::Standard),
    ]
    .into_iter()
    .find_map(|(prefix, quality)| filename.starts_with(prefix).then_some(quality))
}

fn playback_entry_succeeded(entry: &PlaybackEntry) -> bool {
    entry.result.is_none_or(|result| result == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vkey_response() {
        for (data, succeeded, path) in [
            (
                json!({"result": "0", "purl": "song.mp3"}),
                true,
                Some("song.mp3"),
            ),
            (
                json!({"result": 104003, "purl": "song.mp3"}),
                false,
                Some("song.mp3"),
            ),
            (json!({"wifiurl": "song.mflac"}), true, Some("song.mflac")),
            (json!({"purl": null, "wifiurl": "song.mflac"}), true, None),
            (json!({"purl": false, "wifiurl": "song.mflac"}), true, None),
            (json!({"result": [], "purl": 123}), true, Some("123")),
        ] {
            let entry: PlaybackEntry = serde_json::from_value(data).unwrap();
            assert_eq!(playback_entry_succeeded(&entry), succeeded);
            assert_eq!(entry.purl.unwrap_or(entry.wifiurl).as_deref(), path);
        }
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
}
