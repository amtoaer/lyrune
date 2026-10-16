//! QQ 音乐扫码登录与凭据刷新请求。

mod models;
mod mqtt;
pub(crate) mod wechat;

pub(crate) use models::{LoginStatus, LoginToken};
pub(crate) use mqtt::MqttLoginSession;

use std::time::Duration;

use reqwest::Client;
use reqwest::header::COOKIE;
use serde_json::{Value, json};

use self::models::{LoginInfoResponse, LoginQrResponse, LoginQrResult};
use self::mqtt::MqttLoginEvent;
use crate::error::{LoginError, LoginResult};
use crate::signing;

const TENCENT_API_URL: &str = "https://u.y.qq.com/cgi-bin/musics.fcg";
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) struct LoginClient {
    client: Client,
    guid: String,
}

impl LoginClient {
    /// 创建 QQ 音乐登录客户端。
    pub(crate) fn new(guid: String) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            guid,
            client: Client::builder()
                .no_proxy()
                .timeout(HTTP_TIMEOUT)
                .connect_timeout(HTTP_TIMEOUT)
                .user_agent(
                    "Mozilla/5.0 (compatible; MSIE 9.0; Windows NT 6.1; WOW64; Trident/5.0)",
                )
                .build()
                .unwrap(),
        }
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        mut body: Value,
        token: Option<&LoginToken>,
    ) -> LoginResult<T> {
        let mut comm = match token {
            Some(login_token) => json!({
                "ct": 19,
                "cv": 2201,
                "chid": "0",
                "uin": login_token.music_id.to_string(),
                "g_tk": signing::hash33(login_token.music_key.as_str()),
                "guid": self.guid
            }),
            None => json!({
                "ct": 19,
                "cv": 2201,
                "chid": "0",
                "guid": self.guid
            }),
        };

        // 业务请求可以在 body.comm 覆盖默认公共参数（例如登录类型）。
        if let Some(custom_comm) = body.get("comm").and_then(Value::as_object) {
            for (key, value) in custom_comm {
                comm[key] = value.clone();
            }
        }
        body["comm"] = comm;
        // sign 必须基于最终 body 计算，否则服务端会验签失败。
        let sign = signing::sign(&body);

        self.client
            .post(TENCENT_API_URL)
            .query(&[("sign", sign.as_str())])
            .header(COOKIE, token.map(|v| v.to_cookie()).unwrap_or_default())
            .json(&body)
            .send()
            .await?
            .json::<T>()
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn get_login_qrcode(&self) -> LoginResult<LoginQrResult> {
        let response = self
            .post::<LoginQrResponse>(
                json!({
                  "result": {
                    "module": "music.login.LoginServer",
                    "method": "CreateQRCode",
                    "param": {
                      "tmeAppID": "qqmusic",
                      "ct": 19,
                      "cv": 2201
                    }
                  }
                }),
                None,
            )
            .await?;
        Ok(response.into())
    }

    pub(crate) async fn get_login_token_with_mqtt_session(
        &self,
        qrcode_id: &str,
        session: &mut mqtt::MqttLoginSession,
    ) -> LoginResult<LoginStatus> {
        let event = session.poll_event().await?;
        match event {
            MqttLoginEvent::WaitingScan => Ok(LoginStatus::WaitingScan),
            MqttLoginEvent::WaitingConfirm => Ok(LoginStatus::WaitingConfirm),
            MqttLoginEvent::QrCodeExpired => Ok(LoginStatus::QrCodeExpired),
            MqttLoginEvent::Canceled => Err(LoginError::TencentLoginCanceled),
            MqttLoginEvent::LoginFailed => Err(LoginError::TencentLoginFailed),
            MqttLoginEvent::Cookies {
                music_id,
                music_key,
            } => self
                .login_with_mobile_ticket(qrcode_id, music_id, music_key.as_str())
                .await
                .map(LoginStatus::Success),
        }
    }

    pub(crate) async fn refresh_login_token(&self, token: &LoginToken) -> LoginResult<LoginToken> {
        let mut param = json!({
            "refresh_key": token.refresh_key,
            "refresh_token": token.refresh_token,
            "musickey": token.music_key,
            "musicid": token.music_id,
        });
        if token.login_type == 1 {
            param["openid"] = json!(token.open_id);
            param["unionid"] = json!(token.union_id);
            param["str_musicid"] = json!(token.music_id.to_string());
            param["loginMode"] = json!(2);
        }
        let response = self
            .post::<LoginInfoResponse>(
                json!({
                    "result": {
                        "module": "music.login.LoginServer",
                        "method": "Login",
                        "param": param
                    },
                    "comm": {
                        "tmeLoginType": token.login_type
                    }
                }),
                Some(token),
            )
            .await?;
        response.into_token()
    }

    async fn login_with_mobile_ticket(
        &self,
        qrcode_id: &str,
        music_id: u64,
        music_key: &str,
    ) -> LoginResult<LoginToken> {
        let response = self
            .post::<LoginInfoResponse>(
                json!({
                    "result": {
                        "module": "music.login.LoginServer",
                        "method": "Login",
                        "param": {
                            "musicid": music_id,
                            "qrCodeID": qrcode_id,
                            "token": music_key
                        }
                    },
                    "comm": {
                        "tmeLoginType": 6
                    }
                }),
                None,
            )
            .await?;
        response.into_token()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn guid_has_expected_format() {
        let guid = crate::account::new_client_guid();
        assert_eq!(guid.len(), 32);
        assert!(guid.chars().all(|c| matches!(c, 'A'..='F' | '0'..='9')));
    }
}
