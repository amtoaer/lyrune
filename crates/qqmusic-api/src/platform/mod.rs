//! 平台实现层。
//!
//! # Overview
//!
//! 本模块承载 [`crate::client::MusicClient`] 的平台侧实现细节，不作为公开 API 暴露。
//! 统一模型与错误语义以 [`crate::models`] 与 [`crate::error`] 为准。
//!
//! # Core Concepts
//!
//! - [`NeteaseClient`]：网易云平台请求实现。
//! - [`TencentClient`]：QQ 音乐平台请求实现。
//! - [`TencentMqttLoginSession`]：QQ 扫码登录使用的 MQTT 会话状态。

pub(crate) mod netease;
mod tencent;

pub(crate) use netease::NeteaseClient;
pub(crate) use tencent::{TencentClient, TencentMqttLoginSession};

/// 将平台原始模型集合转换为统一模型集合。
fn collect_into<T, U>(values: Vec<T>) -> Vec<U>
where
    T: Into<U>,
{
    values.into_iter().map(Into::into).collect()
}

/// 解析单个 LRC 时间标签并标准化到毫秒精度。
fn parse_lrc_timestamp_tag(segment: &str) -> Option<(usize, String)> {
    if !segment.starts_with('[') {
        return None;
    }
    let end = segment.find(']')?;
    let inner = &segment[1..end];
    let (minute, sec_part) = inner.split_once(':')?;
    if minute.len() != 2 || !minute.as_bytes().iter().all(u8::is_ascii_digit) {
        return None;
    }

    let (second, ms_part) = match sec_part.split_once('.') {
        Some((second, ms_part)) => (second, ms_part),
        None => (sec_part, ""),
    };
    if second.len() != 2 || !second.as_bytes().iter().all(u8::is_ascii_digit) {
        return None;
    }
    if !ms_part.is_empty() && !ms_part.as_bytes().iter().all(u8::is_ascii_digit) {
        return None;
    }

    let millisecond = match ms_part.len() {
        0 => "000".to_string(),
        1 => format!("{ms_part}00"),
        2 => format!("{ms_part}0"),
        _ => ms_part[..3].to_string(),
    };

    Some((end + 1, format!("[{minute}:{second}.{millisecond}]")))
}

/// 规范化单行 LRC，只保留合法时间标签前缀并原样拼接歌词正文。
fn normalize_lrc_line(line: &str) -> Option<String> {
    let line = line.trim_start_matches('\u{feff}');
    let mut cursor = 0;
    let mut normalized = String::new();
    while let Some((consumed, tag)) = parse_lrc_timestamp_tag(&line[cursor..]) {
        normalized.push_str(tag.as_str());
        cursor += consumed;
    }
    if normalized.is_empty() {
        return None;
    }
    normalized.push_str(&line[cursor..]);
    Some(normalized)
}

/// 规范化带时间戳歌词，仅保留包含有效时间标签的行。
fn normalize_timestamp_lyric(raw: String) -> String {
    raw.lines().filter_map(normalize_lrc_line).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::normalize_timestamp_lyric;

    #[test]
    fn normalize_timestamp_lyric_should_keep_only_timed_lines() {
        let raw = "[ti:sample]\n[ar:test]\n{\"t\":0,\"c\":[{\"tx\":\"作词\"}]}\n[00:00.27]line \
                   a\n[00:10.3]line b\n[00:11]line c\n[00:20.12][00:30.456]line d";

        let normalized = normalize_timestamp_lyric(raw.to_string());
        assert_eq!(
            normalized,
            "[00:00.270]line a\n[00:10.300]line b\n[00:11.000]line c\n[00:20.120][00:30.456]line d"
        );
    }
}
