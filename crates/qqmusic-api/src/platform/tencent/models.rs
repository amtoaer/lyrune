//! QQ 音乐登录响应解析。

use serde::Deserialize;

use crate::error::{MusicClientError, MusicClientResult};
use crate::models::{LoginQrResult, TencentLoginToken};

/// `music.login.LoginServer.CreateQRCode`（登录二维码）响应体。
#[derive(Deserialize)]
pub(super) struct TLoginQrResponse {
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

impl From<TLoginQrResponse> for LoginQrResult {
    fn from(value: TLoginQrResponse) -> Self {
        Self {
            qr_code: value.result.data.qrcode,
            qr_key: value.result.data.qrcode_id,
        }
    }
}

/// `music.login.LoginServer.Login`（扫码换 token / 刷新 token）响应体。
#[derive(Deserialize)]
pub(super) struct TLoginInfoResponse {
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

impl TLoginInfoResponse {
    pub(super) fn into_token(self) -> MusicClientResult<TencentLoginToken> {
        let response_code = self.result.code;
        let data = self.result.data;
        if response_code != 0 {
            return Err(login_server_error(response_code, &data));
        }
        if data.musicid == 0 {
            return Err(MusicClientError::InvalidTencentLoginTokenField("musicid"));
        }
        if data.musickey.trim().is_empty() {
            return Err(MusicClientError::InvalidTencentLoginTokenField("musickey"));
        }

        let login_type = if data.login_type > 0 {
            data.login_type
        } else if data.musickey.starts_with("W_X") {
            1
        } else {
            2
        };

        Ok(TencentLoginToken {
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

fn login_server_error(code: i64, data: &TLoginInfoData) -> MusicClientError {
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
    MusicClientError::TencentLoginServerError { code, detail }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::TLoginInfoResponse;

    #[test]
    fn login_response_preserves_encrypted_uin_without_debugging_secrets() {
        let response: TLoginInfoResponse = serde_json::from_value(json!({
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
        let response: TLoginInfoResponse = serde_json::from_value(json!({
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
            crate::error::MusicClientError::TencentLoginServerError { code: 1000, .. }
        ));
        assert_eq!(
            error.to_string(),
            "QQ 音乐登录失败（错误码 1000）：登录授权已过期，请重新登录"
        );
    }

    #[test]
    fn login_response_explains_known_device_limit_error() {
        let response: TLoginInfoResponse = serde_json::from_value(json!({
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
