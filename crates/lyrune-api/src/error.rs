use thiserror::Error;

/// QQ 音乐登录请求与 MQTT 会话错误。
#[derive(Error, Debug)]
pub enum LoginError {
    /// 网络请求失败（请求构建、连接、响应解析等底层错误）。
    #[error("failed to connect to server")]
    NetworkError(#[from] reqwest::Error),
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
    /// QQ 音乐登录响应字段缺失或非法。
    #[error("invalid tencent login token response field: `{0}`")]
    InvalidLoginTokenField(&'static str),
}

pub type LoginResult<T> = std::result::Result<T, LoginError>;
