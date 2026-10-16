use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail, ensure};
use reqwest::cookie::{CookieStore as _, Jar};
use reqwest::header::{LOCATION, REFERER};
use reqwest::{Client, Url};
use tokio::sync::mpsc;

use super::{LoginClient, LoginToken};
use crate::signing::hash33;
use crate::{LoginEvent, ProtocolClient, QqCredential};

pub(super) const QQ_APP_ID: &str = "716027609";
pub(super) const MUSIC_APP_ID: &str = "100497308";
pub(super) const LOGIN_JUMP: &str = "https://graph.qq.com/oauth2.0/login_jump";
const MUSIC_REDIRECT: &str =
    "https://y.qq.com/portal/wx_redirect.html?login_type=1&surl=https://y.qq.com/";
pub(super) const LOGIN_REFERER: &str = "https://xui.ptlogin2.qq.com/cgi-bin/xlogin?appid=716027609&daid=383&style=33&target=self&pt_3rd_aid=100497308&s_url=https%3A%2F%2Fgraph.qq.com%2Foauth2.0%2Flogin_jump";

// 每次扫码或本机账号检测独占 Cookie，会话只在本次 QQ 授权流程内复用。
pub(super) struct QqSession {
    pub(super) client: Client,
    pub(super) cookies: Arc<Jar>,
    client_guid: String,
}

impl QqSession {
    pub(super) fn new(client_guid: String) -> Result<Self> {
        ensure!(
            !client_guid.trim().is_empty(),
            "设备标识未保存，请检查应用设置后重新启动"
        );
        let _ = rustls::crypto::ring::default_provider().install_default();
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
        Ok(Self {
            client,
            cookies,
            client_guid,
        })
    }

    pub(super) async fn finish_login(&self, redirect: Url) -> Result<QqCredential> {
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
        let credential = QqCredential::from_token(token, self.client_guid.clone());
        tokio::time::timeout(
            Duration::from_secs(3),
            ProtocolClient::new()?.ensure_encrypted_uin(credential),
        )
        .await
        .context("读取 QQ 音乐用户资料超时，请稍后重试")?
    }

    async fn authorize_music(&self) -> Result<LoginToken> {
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
            redirect.scheme() == "https"
                && redirect.host_str() == Some("y.qq.com")
                && redirect.path() == "/portal/wx_redirect.html",
            "QQ 返回了无效的音乐授权地址"
        );
        let code = redirect
            .query_pairs()
            .find(|(name, _)| name == "code")
            .map(|(_, value)| value.into_owned())
            .filter(|code| !code.is_empty())
            .context("QQ 未返回音乐授权码")?;
        let client = LoginClient::new(self.client_guid.clone());
        client
            .login_with_qq_code(&code)
            .await
            .context("QQ 音乐凭据兑换失败")
    }
}

pub(super) fn session_cookie(jar: &Jar, url: &str, name: &str) -> Result<String> {
    let header = jar
        .cookies(&Url::parse(url)?)
        .context("QQ 未返回授权会话 Cookie")?;
    for cookie in header.to_str()?.split(';') {
        if let Some((key, value)) = cookie.trim().split_once('=')
            && key == name
            && !value.is_empty()
        {
            return Ok(value.to_owned());
        }
    }
    bail!("QQ 授权会话缺少 {name}，请重新检测账号后重试")
}

pub async fn run_qq_login(events: mpsc::UnboundedSender<LoginEvent>, client_guid: String) {
    match tokio::time::timeout(Duration::from_secs(180), qr_login(&events, client_guid)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = events.send(LoginEvent::Failed(format!("{error:#}")));
        }
        Err(_) => {
            let _ = events.send(LoginEvent::Expired);
        }
    }
}

async fn qr_login(events: &mpsc::UnboundedSender<LoginEvent>, client_guid: String) -> Result<()> {
    let session = QqSession::new(client_guid)?;
    let image = session
        .client
        .get("https://ssl.ptlogin2.qq.com/ptqrshow")
        .header(REFERER, LOGIN_REFERER)
        .query(&[
            ("appid", QQ_APP_ID),
            ("e", "2"),
            ("l", "M"),
            ("s", "3"),
            ("d", "72"),
            ("v", "4"),
            ("t", &rand::random::<f64>().to_string()),
            ("daid", "383"),
            ("pt_3rd_aid", MUSIC_APP_ID),
        ])
        .timeout(Duration::from_secs(15))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let qrsig = session_cookie(
        &session.cookies,
        "https://ssl.ptlogin2.qq.com/ptqrlogin",
        "qrsig",
    )?;
    // 二维码 token 从 0 开始，OAuth 的 g_tk 则从 5381 开始。
    let token = qrsig.bytes().fold(0_u32, |hash, byte| {
        hash.wrapping_mul(33).wrapping_add(u32::from(byte))
    }) & 0x7fff_ffff;
    let _ = events.send(LoginEvent::QrReady(image.to_vec()));
    loop {
        let action = format!(
            "0-0-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
        );
        let body = session
            .client
            .get("https://ssl.ptlogin2.qq.com/ptqrlogin")
            .header(REFERER, LOGIN_REFERER)
            .query(&[
                ("u1", LOGIN_JUMP),
                ("ptqrtoken", &token.to_string()),
                ("ptredirect", "0"),
                ("h", "1"),
                ("t", "1"),
                ("g", "1"),
                ("from_ui", "1"),
                ("ptlang", "2052"),
                ("action", &action),
                ("js_ver", "20102616"),
                ("js_type", "1"),
                ("pt_uistyle", "40"),
                ("aid", QQ_APP_ID),
                ("daid", "383"),
                ("pt_3rd_aid", MUSIC_APP_ID),
                ("has_onekey", "1"),
            ])
            .timeout(Duration::from_secs(15))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let (status, redirect) = parse_qr_status(&body)?;
        let event = match status {
            0 => LoginEvent::Succeeded(
                session
                    .finish_login(redirect.context("QQ 未返回扫码授权地址")?)
                    .await?,
            ),
            66 => LoginEvent::WaitingScan,
            67 => LoginEvent::WaitingConfirm,
            65 => LoginEvent::Expired,
            68 => bail!("已取消 QQ 登录"),
            _ => bail!("QQ 扫码返回未知状态：{status}"),
        };
        if events.send(event).is_err() || matches!(status, 0 | 65) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn parse_qr_status(body: &str) -> Result<(u16, Option<Url>)> {
    let args = body
        .trim()
        .trim_end_matches(';')
        .strip_prefix("ptuiCB('")
        .and_then(|args| args.strip_suffix("')"))
        .context("QQ 扫码响应格式无效")?;
    let mut args = args.split("','");
    let status = args.next().context("QQ 扫码响应缺少状态码")?.parse()?;
    let redirect = if status == 0 {
        Some(parse_redirect(
            args.nth(1).context("QQ 未返回扫码授权地址")?,
        )?)
    } else {
        None
    };
    Ok((status, redirect))
}

pub(super) fn parse_redirect(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("ssl.ptlogin2.graph.qq.com")
            && url.path() == "/check_sig",
        "QQ 返回了无效的授权地址"
    );
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qr_status() {
        assert_eq!(
            parse_qr_status("ptuiCB('66','0','','0','等待扫码','');").unwrap(),
            (66, None)
        );
        assert_eq!(
            parse_qr_status("ptuiCB('67','0','','0','等待确认','');").unwrap(),
            (67, None)
        );
        let (status, redirect) = parse_qr_status("ptuiCB('0','0','https://ssl.ptlogin2.graph.qq.com/check_sig?uin=10001&ptsigx=ticket','0','成功','昵称');").unwrap();
        assert_eq!(status, 0);
        assert_eq!(redirect.unwrap().query(), Some("uin=10001&ptsigx=ticket"));
        assert!(
            parse_qr_status("ptuiCB('0','0','https://example.com/check_sig','0','','');").is_err()
        );
        assert!(parse_qr_status("<html>error</html>").is_err());
    }

    #[tokio::test]
    #[ignore = "requires live QQ network access; does not authorize a login"]
    async fn test_qq_qr() -> Result<()> {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_qq_login(sender, crate::new_client_guid()));
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            match receiver.recv().await {
                Some(LoginEvent::QrReady(bytes)) => {
                    ensure!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "QQ QR must be PNG")
                }
                Some(LoginEvent::Failed(error)) => bail!("{error}"),
                _ => bail!("未收到 QQ 二维码"),
            }
            match receiver.recv().await {
                Some(LoginEvent::WaitingScan) => Ok(()),
                Some(LoginEvent::Failed(error)) => bail!("{error}"),
                _ => bail!("未收到 QQ 等待扫码状态"),
            }
        })
        .await;
        task.abort();
        result?
    }
}
