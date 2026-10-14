mod qrc_des;
use super::ProtocolClient;
use super::response::{integer_field, string_field};
use crate::{CredentialSession, LyricResult, Track};
use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use flate2::read::ZlibDecoder;
use serde_json::{Value, json};
use std::io::Read as _;

impl ProtocolClient {
    pub async fn lyrics(
        &self,
        credential: &CredentialSession,
        track: &Track,
    ) -> Result<LyricResult> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write as _;

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
}
