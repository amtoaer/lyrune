#![allow(rustdoc::private_intra_doc_links)]
//! 网易云 EAPI 参数与辅助加密工具。
//!
//! # Overview
//!
//! 本模块封装网易云 EAPI 所需的参数签名与密文编码规则，供 [`super::api::NeteaseClient`]
//! 复用。

use aes::Aes128;
use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockEncryptMut, KeyInit};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

type Aes128EcbEnc = ecb::Encryptor<Aes128>;

const EAPI_KEY: &[u8; 16] = b"e82ckenh8dichen8";
const ALBUM_CACHE_KEY: &[u8; 16] = b")(13daqP@ssw0rd~";
const MAGIC: &str = "36cd479b6b5";

fn aes_ecb_pkcs7_encrypt(plaintext: &[u8], key: &[u8; 16]) -> Vec<u8> {
    Aes128EcbEnc::new(key.into()).encrypt_padded_vec_mut::<Pkcs7>(plaintext)
}

/// 生成专辑详情请求所需的 `cache_key`。
///
/// 该计算依赖 [`ALBUM_CACHE_KEY`]，用于保持与网易云服务端约定一致。
pub(super) fn album_cache_key(id: &str) -> String {
    // 专辑详情接口要求 cache_key 为固定格式串经过 AES-ECB 后的 base64。
    let plain = format!("e_r=false&id={id}");
    BASE64.encode(aes_ecb_pkcs7_encrypt(plain.as_bytes(), ALBUM_CACHE_KEY))
}

/// 生成 EAPI `params` 字段。
///
/// 该计算依赖 [`EAPI_KEY`] 与 [`MAGIC`]，用于满足网易云服务端验签规则。
pub(super) fn eapi_params(api_path: &str, body_json: &str) -> String {
    // EAPI params: 先按官方拼接规则计算 md5，再把明文做 AES-ECB 并输出大写 HEX。
    let md5_src = format!("nobody{api_path}use{body_json}md5forencrypt");
    let digest = format!("{:x}", md5::compute(md5_src.as_bytes()));
    let plain = format!("{api_path}-{MAGIC}-{body_json}-{MAGIC}-{digest}");
    hex::encode_upper(aes_ecb_pkcs7_encrypt(plain.as_bytes(), EAPI_KEY))
}
