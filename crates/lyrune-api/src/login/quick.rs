use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use futures_util::future::join_all;
use reqwest::Url;
use reqwest::header::REFERER;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::qq::{
    LOGIN_JUMP, LOGIN_REFERER, MUSIC_APP_ID, QQ_APP_ID, QqSession, parse_redirect, session_cookie,
};
use crate::QqCredential;

const QQ_PORTS: [u16; 5] = [4301, 4303, 4305, 4307, 4309];

pub struct QqAccount {
    pub uin: u64,
    pub nickname: String,
}

pub struct QqQuickLogin {
    session: QqSession,
    local_token: String,
    port: u16,
    accounts: Vec<QqAccount>,
}

impl QqQuickLogin {
    pub async fn discover(client_guid: String) -> Result<Self> {
        let session = QqSession::new(client_guid)?;
        let client = &session.client;
        client.get(LOGIN_REFERER).send().await?.error_for_status()?;
        let local_token = session_cookie(&session.cookies, LOGIN_REFERER, "pt_local_token")?;
        let probes = join_all(QQ_PORTS.map(|port| {
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
            session,
            local_token,
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
            .session
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
            &self.session.cookies,
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
            .session
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
        self.session.finish_login(redirect).await
    }
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
    parse_redirect(args.next().context("QQ 未返回快捷授权地址")?)
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

    #[test]
    fn test_qq_redirect() {
        assert!(
            parse_qlogin(
                "ptui_qlogin_CB('0','https://ssl.ptlogin2.graph.qq.com/check_sig?ticket=ok');"
            )
            .is_ok()
        );
        assert!(parse_qlogin("ptui_qlogin_CB('0','https://example.com/check_sig');").is_err());
        assert!(
            parse_qlogin("ptui_qlogin_CB('1','https://ssl.ptlogin2.graph.qq.com/check_sig');")
                .is_err()
        );
    }
}
