//! QQ 音乐登录与资源请求共用的签名和哈希算法。

use base64::Engine;
use serde_json::Value;
use sha1::{Digest, Sha1};

const HASH33_MASK: u64 = 2_147_483_647;
const HASH33_INIT: u64 = 5_381;
const SIGN_PART_1_INDEXES: [usize; 8] = [23, 14, 6, 36, 16, 40, 7, 19];
const SIGN_PART_2_INDEXES: [usize; 8] = [16, 1, 32, 12, 19, 27, 8, 5];
const SIGN_SCRAMBLE_VALUES: [u8; 20] = [
    89, 39, 179, 150, 218, 82, 58, 252, 177, 52, 186, 123, 120, 64, 242, 133, 143, 161, 121, 179,
];

/// 计算 QQ 登录态常用的 `hash33` 值。
///
/// 该实现依赖 [`HASH33_INIT`] 与 [`HASH33_MASK`] 常量，确保与平台算法一致。
pub(crate) fn hash33(s: &str) -> u64 {
    let mut h = HASH33_INIT;
    for c in s.chars() {
        h = h.wrapping_mul(33).wrapping_add(c as u64);
    }
    h & HASH33_MASK
}

/// 计算 QQ 网关签名字段。
///
/// 根据最终 JSON 请求体计算；修改参数后必须重新签名。
// Adapted from netease-qq-music-api (MIT); see THIRD_PARTY_NOTICES.md.
pub(crate) fn sign(request: &Value) -> String {
    let payload = serde_json::to_vec(request).expect("serialize qq request body");
    let hash = hex::encode_upper(Sha1::digest(payload));
    let hash_bytes = hash.as_bytes();

    // 索引表中的 40 超出 SHA-1 的 40 字符十六进制串；沿用上游跳过规则。
    let part1: String = SIGN_PART_1_INDEXES
        .into_iter()
        .filter(|&idx| idx < hash_bytes.len())
        .map(|idx| hash_bytes[idx] as char)
        .collect();
    let part2: String = SIGN_PART_2_INDEXES
        .into_iter()
        .map(|idx| hash_bytes[idx] as char)
        .collect();

    let mut scrambled = [0_u8; 20];
    for (i, &value) in SIGN_SCRAMBLE_VALUES.iter().enumerate() {
        let hi = decode_hex_nibble(hash_bytes[i * 2]);
        let lo = decode_hex_nibble(hash_bytes[i * 2 + 1]);
        scrambled[i] = value ^ ((hi << 4) | lo);
    }

    let b64_part: String = base64::engine::general_purpose::STANDARD
        .encode(scrambled)
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | '+' | '='))
        .collect();

    format!("zzc{part1}{b64_part}{part2}").to_ascii_lowercase()
}

fn decode_hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        b'A'..=b'F' => value - b'A' + 10,
        _ => unreachable!("sha1 hex only contains [0-9a-fA-F]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash33_matches_known_values() {
        assert_eq!(hash33(""), 5_381);
        assert_eq!(hash33("a"), 177_670);
        assert_eq!(hash33("abc"), 193_485_963);
        assert_eq!(hash33("腾讯"), 6_989_618);
        assert_eq!(hash33("hello"), 261_238_937);
    }

    #[test]
    fn qq_sign_matches_known_value() {
        let body = serde_json::json!({
            "foo": "bar",
            "num": 1
        });
        assert_eq!(sign(&body), "zzcf3ea51dcp3xdwnxisjgufsk0znclehf2t85bc1d3d4");
    }
}
