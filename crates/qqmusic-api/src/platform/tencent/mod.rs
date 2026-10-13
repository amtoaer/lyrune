//! QQ 音乐登录实现。

pub mod api;
pub mod models;
mod mqtt;
pub mod utils;

pub(crate) use api::TencentClient;
pub(crate) use mqtt::MqttLoginSession as TencentMqttLoginSession;
