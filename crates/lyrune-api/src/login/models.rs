//! QQ 音乐登录响应解析。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{LoginError, LoginResult};

/// 登录二维码结果（crate 内部使用）。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LoginQrResult {
    /// 二维码图像内容（通常为 `data:image/png;base64,...`，可直接渲染）。
    pub(crate) qr_code: String,
    /// 二维码 key，用于后续轮询状态。
    pub(crate) qr_key: String,
}

/// 二维码登录状态。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum LoginStatus {
    /// 登录成功并返回 token。
    Success(LoginToken),
    /// 二维码过期（需重新创建会话）。
    QrCodeExpired,
    /// 等待扫码。
    WaitingScan,
    /// 已扫码，等待用户在客户端确认登录。
    WaitingConfirm,
}

/// QQ 音乐登录 token。
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct LoginToken {
    pub(crate) music_id: u64,
    pub(crate) music_key: String,
    #[serde(default)]
    pub(crate) open_id: String,
    #[serde(default)]
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    pub(crate) refresh_key: String,
    #[serde(default)]
    pub(crate) union_id: String,
    #[serde(default)]
    pub(crate) string_music_id: String,
    #[serde(default)]
    pub(crate) music_key_create_time: i64,
    #[serde(default)]
    pub(crate) key_expires_in: i64,
    #[serde(default)]
    pub(crate) first_login: i64,
    #[serde(default)]
    pub(crate) bind_account_type: i64,
    #[serde(default)]
    pub(crate) need_refresh_key_in: i64,
    pub(crate) login_type: u64,
    pub(crate) expires_at: Option<i64>,
    #[serde(default)]
    pub(crate) encrypted_uin: String,
}

impl fmt::Debug for LoginToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginToken")
            .field("has_music_id", &(self.music_id != 0))
            .field("login_type", &self.login_type)
            .field("expires_at", &self.expires_at)
            .field("has_encrypted_uin", &!self.encrypted_uin.is_empty())
            .finish_non_exhaustive()
    }
}

impl LoginToken {
    // 登录与曲库请求使用不同的 Cookie 字段，不能直接复用 QqCredential::cookie。
    pub fn to_cookie(&self) -> String {
        format!(
            "uin={}; qqmusic_key={}; qm_keyst={}; tmeLoginType={}",
            self.music_id, self.music_key, self.music_key, self.login_type,
        )
    }
}

/// `music.login.LoginServer.CreateQRCode`（登录二维码）响应体。
#[derive(Deserialize)]
pub(super) struct LoginQrResponse {
    result: TLoginQrResult,
}

#[derive(Deserialize)]
struct TLoginQrResult {
    data: TLoginQrData,
}

#[derive(Deserialize)]
struct TLoginQrData {
    qrcode: String,
    #[serde(rename = "qrcodeID")]
    qrcode_id: String,
}

impl From<LoginQrResponse> for LoginQrResult {
    fn from(value: LoginQrResponse) -> Self {
        Self {
            qr_code: value.result.data.qrcode,
            qr_key: value.result.data.qrcode_id,
        }
    }
}

/// `music.login.LoginServer.Login`（扫码换 token / 刷新 token）响应体。
#[derive(Deserialize)]
pub(super) struct LoginInfoResponse {
    result: TLoginInfoResult,
}

#[derive(Deserialize)]
struct TLoginInfoResult {
    #[serde(default)]
    code: i64,
    data: TLoginInfoData,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct TLoginInfoData {
    musicid: u64,
    musickey: String,
    openid: String,
    access_token: String,
    refresh_token: String,
    refresh_key: String,
    unionid: String,
    str_musicid: String,
    expired_at: i64,
    #[serde(rename = "musickeyCreateTime")]
    musickey_create_time: i64,
    #[serde(rename = "keyExpiresIn")]
    key_expires_in: i64,
    first_login: i64,
    #[serde(rename = "bindAccountType")]
    bind_account_type: i64,
    #[serde(rename = "needRefreshKeyIn")]
    need_refresh_key_in: i64,
    #[serde(rename = "loginType")]
    login_type: u64,
    #[serde(rename = "encryptUin", default)]
    encrypted_uin: String,
    #[serde(rename = "errMsg")]
    error_message: String,
    #[serde(rename = "errTip2")]
    error_tip_2: String,
    errtip: String,
    tip3: String,
    #[serde(rename = "feedbackURL")]
    feedback_url: String,
}

impl LoginInfoResponse {
    pub(super) fn into_token(self) -> LoginResult<LoginToken> {
        let response_code = self.result.code;
        let data = self.result.data;
        if response_code != 0 {
            return Err(login_server_error(response_code, &data));
        }
        if data.musicid == 0 {
            return Err(LoginError::InvalidLoginTokenField("musicid"));
        }
        if data.musickey.trim().is_empty() {
            return Err(LoginError::InvalidLoginTokenField("musickey"));
        }

        let login_type = if data.login_type > 0 {
            data.login_type
        } else if data.musickey.starts_with("W_X") {
            1
        } else {
            2
        };

        Ok(LoginToken {
            music_id: data.musicid,
            music_key: data.musickey,
            open_id: data.openid,
            access_token: data.access_token,
            refresh_token: data.refresh_token,
            refresh_key: data.refresh_key,
            union_id: data.unionid,
            string_music_id: data.str_musicid,
            music_key_create_time: data.musickey_create_time,
            key_expires_in: data.key_expires_in,
            first_login: data.first_login,
            bind_account_type: data.bind_account_type,
            need_refresh_key_in: data.need_refresh_key_in,
            expires_at: (data.expired_at > 0).then_some(data.expired_at),
            login_type,
            encrypted_uin: data.encrypted_uin,
        })
    }
}

fn login_server_error(code: i64, data: &TLoginInfoData) -> LoginError {
    let known_detail = match code {
        1000 | 104_400 | 104_401 => "登录授权已过期，请重新登录",
        20_261 => "登录参数无效，请重新发起登录",
        20_271 => "验证码错误或已经使用",
        20_272 => "账号绑定状态异常",
        20_274 => "账号尚未完成必要绑定",
        20_277 | 20_278 => "账号登录受到限制，请在 QQ 音乐官方客户端中检查账号状态",
        20_279 => "登录设备数量已达上限，请在 QQ 音乐官方客户端的设备管理中移除旧设备后重试",
        20_450 => "账号已被封禁，请在 QQ 音乐官方客户端中检查账号状态",
        104_604 => "登录操作过于频繁，请稍后再试",
        _ => "QQ 音乐拒绝了登录请求",
    };
    let server_detail = [
        data.error_message.as_str(),
        data.error_tip_2.as_str(),
        data.errtip.as_str(),
        data.tip3.as_str(),
    ]
    .into_iter()
    .map(str::trim)
    .find(|detail| !detail.is_empty() && *detail != known_detail);
    let mut detail = match server_detail {
        Some(server_detail) => format!("{known_detail}（腾讯提示：{server_detail}）"),
        None => known_detail.to_owned(),
    };
    let feedback_url = data.feedback_url.trim();
    if feedback_url.starts_with("https://") || feedback_url.starts_with("http://") {
        detail.push_str("；处理链接：");
        detail.push_str(feedback_url);
    }
    LoginError::TencentLoginServerError { code, detail }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::LoginInfoResponse;

    #[test]
    fn login_response_preserves_encrypted_uin_without_debugging_secrets() {
        let response: LoginInfoResponse = serde_json::from_value(json!({
            "result": {
                "data": {
                    "musicid": 10001,
                    "musickey": "secret-music-key",
                    "refresh_token": "secret-refresh-token",
                    "refresh_key": "secret-refresh-key",
                    "expired_at": 2000000000,
                    "musickeyCreateTime": 1900000000,
                    "keyExpiresIn": 100000000,
                    "loginType": 2,
                    "encryptUin": "encrypted-uin"
                }
            }
        }))
        .expect("valid login response");

        let token = response.into_token().expect("valid login token");

        assert_eq!(token.encrypted_uin, "encrypted-uin");
        let debug = format!("{token:?}");
        assert!(!debug.contains("10001"));
        assert!(!debug.contains("secret-"));
        assert!(!debug.contains("encrypted-uin"));
    }

    #[test]
    fn login_response_reports_business_error_before_empty_token_fields() {
        let response: LoginInfoResponse = serde_json::from_value(json!({
            "result": {
                "code": 1000,
                "data": {
                    "musickey": ""
                }
            }
        }))
        .expect("valid error response");

        let error = response.into_token().expect_err("login should fail");
        assert!(matches!(
            error,
            crate::error::LoginError::TencentLoginServerError { code: 1000, .. }
        ));
        assert_eq!(
            error.to_string(),
            "QQ 音乐登录失败（错误码 1000）：登录授权已过期，请重新登录"
        );
    }

    #[test]
    fn login_response_explains_known_device_limit_error() {
        let response: LoginInfoResponse = serde_json::from_value(json!({
            "result": {
                "code": 20279,
                "data": {}
            }
        }))
        .expect("valid error response");

        let error = response.into_token().expect_err("login should fail");
        assert_eq!(
            error.to_string(),
            "QQ 音乐登录失败（错误码 20279）：登录设备数量已达上限，请在 QQ 音乐官方客户端的设备管理中移除旧设备后重试"
        );
    }
}
