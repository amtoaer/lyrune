#![allow(rustdoc::private_intra_doc_links)]
//! QQ 音乐请求辅助工具。
//!
//! # Overview
//!
//! 本模块封装 QQ 音乐请求中复用的 GUID、搜索 ID、签名与哈希算法，供
//! [`super::api::TencentClient`] 复用。

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::RngExt;
use serde_json::Value;
use sha1::{Digest, Sha1};

const GUID_CHARSET: &[u8] = b"ABCDEF1234567890";
const HASH33_MASK: u64 = 2_147_483_647;
const HASH33_INIT: u64 = 5_381;
const SEARCH_ID_E_BASE: u64 = 18_014_398_509_481_984;
const SEARCH_ID_N_BASE: u64 = 4_294_967_296;
const SEARCH_ID_N_MAX: u64 = 4_194_304;
const DAY_MILLIS: u64 = 24 * 60 * 60 * 1000;
const SIGN_PART_1_INDEXES: [usize; 8] = [23, 14, 6, 36, 16, 40, 7, 19];
const SIGN_PART_2_INDEXES: [usize; 8] = [16, 1, 32, 12, 19, 27, 8, 5];
const SIGN_SCRAMBLE_VALUES: [u8; 20] =
    [89, 39, 179, 150, 218, 82, 58, 252, 177, 52, 186, 123, 120, 64, 242, 133, 143, 161, 121, 179];

pub fn get_guid() -> String {
    let mut rng = rand::rng();
    (0..32)
        .map(|_| {
            let idx = rng.random_range(0..GUID_CHARSET.len());
            GUID_CHARSET[idx] as char
        })
        .collect()
}

/// 计算 QQ 登录态常用的 `hash33` 值。
///
/// 该实现依赖 [`HASH33_INIT`] 与 [`HASH33_MASK`] 常量，确保与平台算法一致。
pub fn hash33(s: &str) -> u64 {
    let mut h = HASH33_INIT;
    for c in s.chars() {
        h = h.wrapping_mul(33).wrapping_add(c as u64);
    }
    h & HASH33_MASK
}

/// 生成 QQ 搜索请求 ID。
///
/// 该值依赖 [`SEARCH_ID_E_BASE`]、[`SEARCH_ID_N_BASE`]、[`SEARCH_ID_N_MAX`] 与
/// [`DAY_MILLIS`] 常量，用于保持与平台分页查询协议一致。
pub fn get_search_id() -> String {
    let mut rng = rand::rng();
    let e = rng.random_range(1_u64..=20);
    let t = e * SEARCH_ID_E_BASE;
    let n = rng.random_range(0_u64..=SEARCH_ID_N_MAX) * SEARCH_ID_N_BASE;
    let r = (SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
        % DAY_MILLIS as u128) as u64;
    (t + n + r).to_string()
}

/// 计算 QQ 网关签名字段。
///
/// 该算法依赖 [`SIGN_PART_1_INDEXES`]、[`SIGN_PART_2_INDEXES`] 与
/// [`SIGN_SCRAMBLE_VALUES`] 常量，返回值用于请求验签。
pub fn sign(request: &Value) -> String {
    let payload = serde_json::to_vec(request).expect("serialize qq request body");
    let hash = hex::encode_upper(Sha1::digest(payload));
    let hash_bytes = hash.as_bytes();

    let part1: String = SIGN_PART_1_INDEXES
        .into_iter()
        .filter(|&idx| idx < hash_bytes.len())
        .map(|idx| hash_bytes[idx] as char)
        .collect();
    let part2: String =
        SIGN_PART_2_INDEXES.into_iter().map(|idx| hash_bytes[idx] as char).collect();

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
    fn guid_has_expected_format() {
        let guid = get_guid();
        assert_eq!(guid.len(), 32);
        assert!(guid.chars().all(|c| matches!(c, 'A'..='F' | '0'..='9')));
    }

    #[test]
    fn hash33_matches_known_values() {
        assert_eq!(hash33(""), 5_381);
        assert_eq!(hash33("a"), 177_670);
        assert_eq!(hash33("abc"), 193_485_963);
        assert_eq!(hash33("腾讯"), 6_989_618);
        assert_eq!(hash33("hello"), 261_238_937);
    }

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
    fn qq_sign_matches_known_value() {
        let body = serde_json::json!({
            "foo": "bar",
            "num": 1
        });
        assert_eq!(sign(&body), "zzcf3ea51dcp3xdwnxisjgufsk0znclehf2t85bc1d3d4");
    }
}
