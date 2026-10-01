use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail, ensure};
use cookie::Cookie;
use futures_util::future::join_all;
use reqwest::cookie::{CookieStore as _, Jar};
use reqwest::header::{LOCATION, REFERER};
use reqwest::{Client, Url};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::models::TencentLoginToken;
use crate::platform::{TencentClient, hash33};

use super::{ProtocolClient, QqCredential};

const QQ_APP_ID: &str = "716027609";
const MUSIC_APP_ID: &str = "100497308";
const LOGIN_JUMP: &str = "https://graph.qq.com/oauth2.0/login_jump";
const MUSIC_REDIRECT: &str =
    "https://y.qq.com/portal/wx_redirect.html?login_type=1&surl=https://y.qq.com/";
const LOGIN_REFERER: &str = "https://xui.ptlogin2.qq.com/cgi-bin/xlogin?appid=716027609&daid=383&style=33&target=self&pt_3rd_aid=100497308&s_url=https%3A%2F%2Fgraph.qq.com%2Foauth2.0%2Flogin_jump";
const QQ_PORTS: [u16; 5] = [4301, 4303, 4305, 4307, 4309];

pub struct QqAccount {
    pub uin: u64,
    pub nickname: String,
}

pub struct QqQuickLogin {
    client: Client,
    client_guid: String,
    local_token: String,
    cookies: Arc<Jar>,
    port: u16,
    accounts: Vec<QqAccount>,
}

impl QqQuickLogin {
    pub async fn discover(client_guid: String) -> Result<Self> {
        ensure!(
            !client_guid.trim().is_empty(),
            "设备标识未保存，请检查应用设置后重新启动"
        );
        let cookies = Arc::new(Jar::default());
        let client = Client::builder()
            .no_proxy()
            .cookie_provider(cookies.clone())
            .resolve(
                "localhost.ptlogin2.qq.com",
                SocketAddr::from(([127, 0, 0, 1], 0)),
            )
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .user_agent(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36",
            )
            .build()?;
        client.get(LOGIN_REFERER).send().await?.error_for_status()?;
        let local_token = session_cookie(&cookies, LOGIN_REFERER, "pt_local_token")?;
        let probes = join_all(QQ_PORTS.map(|port| {
            let client = &client;
            let local_token = &local_token;
            async move {
                let result = async {
                    let response = client
                        .get(format!(
                            "https://localhost.ptlogin2.qq.com:{port}/pt_get_uins"
                        ))
                        .query(&[
                            ("callback", "ptui_getuins_CB"),
                            ("r", local_token.as_str()),
                            ("pt_local_tk", local_token.as_str()),
                            ("pt_aid", QQ_APP_ID),
                            ("daid", "383"),
                            ("pt_3rd_aid", MUSIC_APP_ID),
                            ("u1", LOGIN_JUMP),
                        ])
                        .header(REFERER, LOGIN_REFERER)
                        .timeout(Duration::from_secs(2))
                        .send()
                        .await?
                        .error_for_status()?;
                    parse_jsonp::<Vec<Value>>(&response.text().await?, "ptui_getuins_CB")
                }
                .await;
                (port, result)
            }
        }))
        .await;
        let mut response = None;
        for (port, result) in probes {
            let Ok(data) = result else { continue };
            let has_accounts = !data.is_empty();
            response = Some((port, data));
            if has_accounts {
                break;
            }
        }
        let (port, data) =
            response.context("未检测到 QQ 快捷登录服务，请先启动并登录 Linux QQ 后重试")?;
        let accounts = data
            .into_iter()
            .map(|account| {
                let uin = match &account["uin"] {
                    Value::String(uin) => uin.parse().context("QQ 账号格式无效")?,
                    value => value.as_u64().context("QQ 账号格式无效")?,
                };
                ensure!(uin > 0, "QQ 账号格式无效");
                Ok(QqAccount {
                    uin,
                    nickname: account["nickname"]
                        .as_str()
                        .context("QQ 账号昵称格式无效")?
                        .to_owned(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            client,
            client_guid,
            local_token,
            cookies,
            port,
            accounts,
        })
    }

    pub fn accounts(&self) -> &[QqAccount] {
        &self.accounts
    }

    pub async fn login(&self, uin: u64) -> Result<QqCredential> {
        ensure!(
            self.accounts.iter().any(|account| account.uin == uin),
            "请选择检测到的 QQ 账号"
        );
        let response = self
            .client
            .get(format!(
                "https://localhost.ptlogin2.qq.com:{}/pt_get_st",
                self.port
            ))
            .query(&[
                ("clientuin", uin.to_string()),
                ("callback", "ptui_getst_CB".to_owned()),
                ("r", rand::random::<f64>().to_string()),
                ("pt_local_tk", self.local_token.clone()),
                ("pt_aid", QQ_APP_ID.to_owned()),
                ("daid", "383".to_owned()),
                ("pt_3rd_aid", MUSIC_APP_ID.to_owned()),
                ("u1", LOGIN_JUMP.to_owned()),
            ])
            .header(REFERER, LOGIN_REFERER)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(reqwest::Error::without_url)?;
        let client_key = session_cookie(
            &self.cookies,
            "https://ssl.ptlogin2.qq.com/jump",
            "clientkey",
        )?;
        let local_tk = client_key.encode_utf16().fold(0_u32, |hash, ch| {
            hash.wrapping_mul(33).wrapping_add(u32::from(ch))
        }) & 0x7fff_ffff;
        let ticket: Value = parse_jsonp(&response.text().await?, "ptui_getst_CB")?;
        let ticket_uin = match &ticket["uin"] {
            Value::String(uin) => uin.parse().context("QQ 授权账号格式无效")?,
            value => value.as_u64().context("QQ 授权账号格式无效")?,
        };
        ensure!(ticket_uin == uin, "QQ 返回的授权账号与所选账号不一致");
        let key_index = match ticket.get("keyindex") {
            Some(Value::String(index)) => index.parse().context("QQ 授权票据索引无效")?,
            Some(index) => index.as_u64().context("QQ 授权票据索引无效")?,
            None => 9,
        };
        let response = self
            .client
            .get("https://ssl.ptlogin2.qq.com/jump")
            .query(&[
                ("clientuin", uin.to_string()),
                ("keyindex", key_index.to_string()),
                ("pt_aid", QQ_APP_ID.to_owned()),
                ("daid", "383".to_owned()),
                ("u1", LOGIN_JUMP.to_owned()),
                ("pt_local_tk", local_tk.to_string()),
                ("pt_3rd_aid", MUSIC_APP_ID.to_owned()),
                ("ptopt", "1".to_owned()),
                ("style", "40".to_owned()),
            ])
            .header(REFERER, LOGIN_REFERER)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(reqwest::Error::without_url)?;
        let redirect = parse_qlogin(&response.text().await?)?;
        self.client
            .get(redirect)
            .header(REFERER, LOGIN_REFERER)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(reqwest::Error::without_url)?;
        let token = match self.authorize_music().await {
            Err(error)
                if error.chain().any(|cause| {
                    cause
                        .downcast_ref::<reqwest::Error>()
                        .is_some_and(|error| error.is_timeout() || error.is_connect())
                }) =>
            {
                tokio::time::sleep(Duration::from_millis(300)).await;
                self.authorize_music().await?
            }
            result => result?,
        };
        let credential = QqCredential::from_token(token, self.client_guid.clone())?;
        tokio::time::timeout(
            Duration::from_secs(3),
            ProtocolClient::new()?.ensure_encrypted_uin(credential),
        )
        .await
        .context("读取 QQ 音乐用户资料超时，请稍后重试")?
    }

    async fn authorize_music(&self) -> Result<TencentLoginToken> {
        let p_skey = session_cookie(
            &self.cookies,
            "https://graph.qq.com/oauth2.0/authorize",
            "p_skey",
        )?;
        let response = self
            .client
            .post("https://graph.qq.com/oauth2.0/authorize")
            .header(REFERER, "https://graph.qq.com/")
            .form(&[
                ("response_type", "code".to_owned()),
                ("client_id", MUSIC_APP_ID.to_owned()),
                ("redirect_uri", MUSIC_REDIRECT.to_owned()),
                ("scope", "get_user_info,get_app_friends".to_owned()),
                ("state", "state".to_owned()),
                ("switch", String::new()),
                ("from_ptlogin", "1".to_owned()),
                ("src", "1".to_owned()),
                ("update_auth", "1".to_owned()),
                ("openapi", "1010_1030".to_owned()),
                ("g_tk", hash33(&p_skey).to_string()),
                (
                    "auth_time",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)?
                        .as_millis()
                        .to_string(),
                ),
                ("ui", uuid::Uuid::new_v4().to_string()),
            ])
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(reqwest::Error::without_url)?;
        let redirect = Url::parse(
            response
                .headers()
                .get(LOCATION)
                .context("QQ 音乐授权未完成，请重新检测账号后重试")?
                .to_str()?,
        )?;
        ensure!(
            redirect.scheme() == "https" && redirect.host_str() == Some("y.qq.com"),
            "QQ 返回了无效的音乐授权地址"
        );
        let code = redirect
            .query_pairs()
            .find(|(name, _)| name == "code")
            .map(|(_, value)| value.into_owned())
            .filter(|code| !code.is_empty())
            .context("QQ 未返回音乐授权码")?;
        let mut client = TencentClient::new();
        client.guid.clone_from(&self.client_guid);
        client
            .login_with_qq_code(&code)
            .await
            .context("QQ 音乐凭据兑换失败")
    }
}

fn session_cookie(jar: &Jar, url: &str, name: &str) -> Result<String> {
    let header = jar
        .cookies(&Url::parse(url)?)
        .context("QQ 未返回授权会话 Cookie")?;
    for cookie in Cookie::split_parse(header.to_str()?) {
        let cookie = cookie?;
        if cookie.name() == name && !cookie.value().is_empty() {
            return Ok(cookie.value().to_owned());
        }
    }
    bail!("QQ 授权会话缺少 {name}，请重新检测账号后重试")
}

fn parse_jsonp<T: DeserializeOwned>(body: &str, callback: &str) -> Result<T> {
    let body = body.trim().trim_end_matches(';').trim_end();
    let prefix = format!("{callback}(");
    let payload = if let Some(payload) = body.strip_prefix(&prefix) {
        payload.strip_suffix(')').context("QQ 服务响应格式无效")?
    } else {
        let (assignment, call) = body.rsplit_once(';').context("QQ 服务响应格式无效")?;
        let (variable, payload) = assignment
            .trim()
            .strip_prefix("var ")
            .context("QQ 服务响应格式无效")?
            .split_once('=')
            .context("QQ 服务响应格式无效")?;
        ensure!(
            call.trim() == format!("{callback}({})", variable.trim()),
            "QQ 服务响应格式无效"
        );
        payload
    };
    let payload = if callback == "ptui_getst_CB" {
        payload
            .replace("uin:", "\"uin\":")
            .replace("keyindex:", "\"keyindex\":")
    } else {
        payload.to_owned()
    };
    serde_json::from_str(&payload).context("无法解析 QQ 服务响应")
}

fn parse_qlogin(body: &str) -> Result<Url> {
    let args = body
        .trim()
        .strip_prefix("ptui_qlogin_CB(")
        .context("QQ 快捷授权响应格式无效")?;
    let mut args = args.split('\'');
    ensure!(
        args.next().is_some_and(|prefix| prefix.trim().is_empty()),
        "QQ 快捷授权响应格式无效"
    );
    let status = args.next().context("QQ 快捷授权响应格式无效")?;
    ensure!(
        status == "0",
        "QQ 快捷授权失败（错误码 {status}），请重新检测账号或使用扫码登录"
    );
    ensure!(
        args.next().is_some_and(|separator| separator.trim() == ","),
        "QQ 快捷授权响应格式无效"
    );
    let url = Url::parse(args.next().context("QQ 未返回快捷授权地址")?)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("ssl.ptlogin2.graph.qq.com")
            && url.path() == "/check_sig",
        "QQ 返回了无效的快捷授权地址"
    );
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qq_ticket() {
        let ticket: Value = parse_jsonp(
            "var var_sso_get_st_uin={uin: 10001, keyindex: 19}; ptui_getst_CB(var_sso_get_st_uin);",
            "ptui_getst_CB",
        )
        .unwrap();
        assert_eq!(ticket["uin"], 10001);
        assert_eq!(ticket["keyindex"], 19);
    }
}
