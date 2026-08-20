use reqwest;
use thiserror::Error;

/// 音乐客户端统一错误类型。
///
/// 所有公开异步请求都返回该错误类型，调用方可以按错误变体进行分级处理：
/// - 参数校验错误（如 [`MusicClientError::MissingKeyword`]、[`MusicClientError::MissingId`]、
///   [`MusicClientError::InvalidIdFormat`]）
/// - 鉴权错误（如 [`MusicClientError::AuthTokenPlatformMismatch`]）
/// - 平台登录流程错误（如 [`MusicClientError::TencentMqttLogin`]）
/// - 网络错误（[`MusicClientError::NetworkError`]）
#[derive(Error, Debug)]
pub enum MusicClientError {
    /// 网络请求失败（请求构建、连接、响应解析等底层错误）。
    #[error("failed to connect to server")]
    NetworkError(#[from] reqwest::Error),
    /// 搜索请求缺少关键词。
    #[error("missing required search keyword")]
    MissingKeyword,
    /// 请求缺少必须的资源 ID。
    #[error("missing required id")]
    MissingId,
    /// 资源 ID 格式非法（例如要求纯数字时传入了非数字）。
    #[error("invalid id format: `{0}`")]
    InvalidIdFormat(String),
    /// 请求缺少必须的分类参数。
    #[error("missing required category")]
    MissingCategory,
    /// 分类 ID 非法（通常用于平台分类参数转换失败）。
    #[error("invalid category id: `{0}`")]
    InvalidCategoryId(String),
    /// 刷新登录 token 时缺少 refresh token。
    #[error("missing required refresh token")]
    MissingRefreshToken,
    /// 用户取消了 QQ 音乐登录流程。
    #[error("tencent login was canceled by user")]
    TencentLoginCanceled,
    /// QQ 音乐登录失败。
    #[error("tencent login failed")]
    TencentLoginFailed,
    /// QQ 音乐登录服务返回了业务错误码。
    #[error("QQ 音乐登录失败（错误码 {code}）：{detail}")]
    TencentLoginServerError { code: i64, detail: String },
    /// QQ 音乐 MQTT 登录链路失败。
    #[error("tencent mqtt login failed: {0}")]
    TencentMqttLogin(String),
    /// 网易云登录状态码不在已知范围内。
    #[error("unexpected netease login status code: {0}")]
    NeteaseUnexpectedLoginStatus(u16),
    /// 网易云登录响应缺少必要 cookie，无法构造登录 token。
    #[error("netease login token cookies missing or invalid")]
    NeteaseLoginTokenInvalid,
    /// 网易云登录二维码生成失败。
    #[error("failed to generate netease login qrcode: {0}")]
    NeteaseLoginQrCode(String),
    /// QQ 音乐登录响应字段缺失或非法。
    #[error("invalid tencent login token response field: `{0}`")]
    InvalidTencentLoginTokenField(&'static str),
    /// 登录 token 所属平台与请求平台不一致。
    #[error(
        "token platform mismatch: expected `{expected_platform}`, got `{token_platform}` token"
    )]
    AuthTokenPlatformMismatch {
        expected_platform: &'static str,
        token_platform: &'static str,
    },
}

/// 音乐客户端统一结果类型。
///
/// 约定所有公开 API 都返回 [`MusicClientResult`]。
pub type MusicClientResult<T> = std::result::Result<T, MusicClientError>;
