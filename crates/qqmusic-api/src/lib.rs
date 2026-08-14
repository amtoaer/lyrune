//! 网易云与 QQ 音乐的统一异步 API 封装库。
//!
//! # Overview
//!
//! 本 crate 提供统一入口 [`MusicClient`]，并通过 typed builder 约束请求构建流程。
//! 调用方可以在一致的 API
//! 语义下完成搜索、详情、播放、发现、歌单和登录请求，而无需直接处理平台实现差异。
//!
//! # Quick Start
//!
//! ```no_run
//! # async fn run() -> Result<(), netease_qq_music_api::MusicClientError> {
//! let client = netease_qq_music_api::MusicClient::new();
//! let result = client.search().song().keyword("江南").limit(10).send().await?;
//! println!("songs: {}", result.songs.len());
//! # Ok(())
//! # }
//! ```
//!
//! # Core Concepts
//!
//! - [`MusicClient`] - 全部请求的统一入口。
//! - typed builders: 通过 `Kind` 类型态限制“当前可调用步骤”。
//! - [`Platform`][`models::Platform`] - 平台选择，默认值为
//!   [`Platform::Netease`][`models::Platform::Netease`]。
//! - [`LoginTokenRef`][`client::LoginTokenRef`] - 平台登录 token 的统一借用包装，用于受保护接口。
//!
//! # Terminology
//!
//! - “请求构建器”：指 `*Request<'a, K>` 链式类型。
//! - “类型态（Kind）”：指 `*Kind` 标记类型，用于编译期约束调用顺序。
//! - “登录 token”：指平台登录凭证对象（如
//!   [`models::NeteaseLoginToken`]、[`models::TencentLoginToken`]）。
//! - “token 所属平台”：指登录 token 对应的平台，必须与请求平台一致。
//!
//! # Common Workflow
//!
//! 1. 创建 [`MusicClient`]。
//! 2. 选择业务域（如 [`MusicClient::search`]、[`MusicClient::detail`]、
//!    [`MusicClient::playback`]）。
//! 3. 选择具体子能力并设置必填参数。
//! 4. 需要鉴权时调用对应请求构建器的登录注入方法并传入平台 token。
//! 5. 调用对应请求构建器的发送方法并 `await` 结果。
//!
//! # Errors and Limits
//!
//! - 参数缺失会返回 [`MusicClientError`] 的对应变体（如
//!   [`MusicClientError::MissingKeyword`]、[`MusicClientError::MissingId`]）。
//! - token 所属平台与请求平台不一致时返回 [`MusicClientError::AuthTokenPlatformMismatch`]。
//! - 网络与响应解析异常统一为 [`MusicClientError::NetworkError`]。
//! - 公开 API 通过 [`MusicClientResult`] 返回失败信息，不以 panic 作为业务错误路径。
//!
//! # See Also
//!
//! - [`client`] - 对外请求构建器与入口类型。
//! - [`models`] - 跨平台统一数据模型。
//! - [`error`] - 统一错误类型与结果别名。

/// 对外请求构建器与 API 入口模块。
pub mod client;
/// 全局统一错误定义模块。
pub mod error;
/// Lyrune 使用的 QQ 音乐登录、曲库与播放集成。
pub mod integration;
/// 跨平台统一数据模型模块。
pub mod models;
mod platform;

/// 统一异步 API 入口。
pub use client::MusicClient;
/// 对外暴露的错误与结果类型别名。
pub use error::{MusicClientError, MusicClientResult};
