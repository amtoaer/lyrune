//! 网易云平台实现。
//!
//! 该模块负责网易云接口请求、响应反序列化与统一模型转换。
//! 对外业务入口见 [`crate::client::MusicClient`]，平台统一语义见 [`crate::models`] 与
//! [`crate::error`]。

mod api;
mod models;
mod utils;

pub(crate) use api::NeteaseClient;
