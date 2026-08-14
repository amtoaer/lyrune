//! QQ 音乐平台实现。
//!
//! 该模块负责 QQ 音乐接口请求、响应反序列化与统一模型转换。
//! 对外业务入口见 [`crate::client::MusicClient`]，平台统一语义见 [`crate::models`] 与
//! [`crate::error`]。

pub mod api;
pub mod models;
mod mqtt;
pub mod utils;

pub(crate) use api::TencentClient;
pub(crate) use mqtt::MqttLoginSession as TencentMqttLoginSession;
