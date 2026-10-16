mod cdn;
mod charts;
mod library;
mod lyrics;
mod playback;
mod response;
mod search;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use reqwest::Client;
use reqwest::header::{COOKIE, REFERER};
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};

use self::response::integer_field;
use crate::account::CredentialError;
use crate::signing::{hash33, sign};
use crate::{CredentialSession, QqCredential};
pub use cdn::CdnCache;

const API_URL: &str = "https://u.y.qq.com/cgi-bin/musics.fcg";

#[derive(Clone)]
pub struct ProtocolClient {
    client: Client,
    cdn: Arc<RwLock<CdnCache>>,
    cdn_refresh: Arc<Mutex<()>>,
}

impl ProtocolClient {
    pub fn new() -> Result<Self> {
        Self::new_with_cdn_cache(CdnCache::default())
    }

    pub fn new_with_cdn_cache(cdn_cache: CdnCache) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                 AppleWebKit/537.36 Chrome/131.0 Safari/537.36",
            )
            .build()
            .context("无法创建 QQ 音乐 HTTP 客户端")?;
        Ok(Self {
            client,
            cdn: Arc::new(RwLock::new(cdn_cache.normalized())),
            cdn_refresh: Arc::default(),
        })
    }

    async fn call(
        &self,
        module: &str,
        method: &str,
        param: Value,
        credential: &CredentialSession,
        comm_overrides: Option<Value>,
    ) -> Result<Value> {
        let current = credential.ensure_fresh().await?;
        self.call_with_session(module, method, param, credential, &current, comm_overrides)
            .await
    }

    async fn call_with_session(
        &self,
        module: &str,
        method: &str,
        param: Value,
        credential: &CredentialSession,
        current: &QqCredential,
        comm_overrides: Option<Value>,
    ) -> Result<Value> {
        let result = self
            .call_with_credential(module, method, param, current, comm_overrides)
            .await;
        if let Err(error) = &result {
            credential.revoke_if_rejected(error);
        }
        result
    }

    async fn call_with_credential(
        &self,
        module: &str,
        method: &str,
        param: Value,
        credential: &QqCredential,
        comm_overrides: Option<Value>,
    ) -> Result<Value> {
        let mut comm = json!({
            "ct": 19,
            "cv": 2201,
            "chid": "0",
            "uin": credential.music_id.to_string(),
            "g_tk": hash33(&credential.music_key),
            "guid": credential.client_guid,
            "tmeLoginType": credential.login_type,
        });
        if let Some(overrides) = comm_overrides.and_then(|value| value.as_object().cloned()) {
            let comm = comm
                .as_object_mut()
                .expect("QQ comm is always initialized as an object");
            comm.extend(overrides);
        }

        let body = json!({
            "comm": comm,
            "result": {
                "module": module,
                "method": method,
                "param": param,
            },
        });
        self.send_call(body, Some(credential.cookie())).await
    }

    async fn call_anonymous(
        &self,
        module: &str,
        method: &str,
        param: Value,
        client_guid: &str,
    ) -> Result<Value> {
        let body = json!({
            "comm": {
                "ct": 19,
                "cv": 2201,
                "chid": "0",
                "uin": "0",
                "g_tk": hash33(""),
                "guid": client_guid,
            },
            "result": {
                "module": module,
                "method": method,
                "param": param,
            },
        });
        self.send_call(body, None).await
    }

    async fn send_call(&self, body: Value, cookie: Option<String>) -> Result<Value> {
        let signature = sign(&body);
        let mut request = self
            .client
            .post(API_URL)
            .query(&[("sign", signature)])
            .header(REFERER, "https://y.qq.com/portal/player.html")
            .json(&body);
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie);
        }
        let response = request
            .send()
            .await
            .context("QQ 音乐网关请求失败")?
            .error_for_status()
            .context("QQ 音乐网关拒绝了请求")?
            .json::<Value>()
            .await
            .context("QQ 音乐网关返回了无效 JSON")?;
        response_data(response)
    }
}

// 先识别凭据拒绝，再解释成功数据；错误响应不保证包含 result/data。
fn response_data(mut response: Value) -> Result<Value> {
    let global_code = integer_field(&response, &["code"]).unwrap_or_default();
    if global_code != 0 {
        if is_credential_rejection_code(global_code) {
            return Err(CredentialError::Rejected { code: global_code }.into());
        }
        bail!("QQ 音乐网关返回错误码 {global_code}");
    }

    let result = response
        .get_mut("result")
        .context("QQ 音乐网关响应缺少 result")?;
    let result_code = integer_field(result, &["code"]).unwrap_or_default();
    if result_code != 0 {
        if is_credential_rejection_code(result_code) {
            return Err(CredentialError::Rejected { code: result_code }.into());
        }
        bail!("QQ 音乐业务接口返回错误码 {result_code}");
    }

    result
        .get_mut("data")
        .map(Value::take)
        .context("QQ 音乐网关响应缺少 data")
}

fn is_credential_rejection_code(code: u64) -> bool {
    matches!(code, 1000 | 104_400 | 104_401)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_response_error_precedence() {
        assert!(
            response_data(json!({
                "code": 0,
                "result": {"code": 2000, "data": {"data": {"period": "2026_42"}}}
            }))
            .is_err()
        );
        for response in [json!({"code": "1000"}), json!({"result": {"code": 104401}})] {
            let error = response_data(response).unwrap_err();
            assert!(crate::account::is_credential_rejected(&error));
        }
        assert_eq!(
            response_data(json!({"result": {"data": null}})).unwrap(),
            Value::Null
        );
        assert_eq!(
            response_data(json!({"result": {}}))
                .unwrap_err()
                .to_string(),
            "QQ 音乐网关响应缺少 data"
        );
    }

    #[test]
    fn classifies_only_confirmed_credential_rejection_codes() {
        for code in [1000, 104_400, 104_401] {
            assert!(is_credential_rejection_code(code));
        }
        for code in [0, 20279, 24001, 50006] {
            assert!(!is_credential_rejection_code(code));
        }
    }
}
