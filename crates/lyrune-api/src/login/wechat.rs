use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail, ensure};
use reqwest::header::REFERER;
use serde_json::json;
use tokio::sync::mpsc;

use super::LoginClient;
use super::models::LoginInfoResponse;
use crate::{LoginEvent, ProtocolClient, QqCredential};

const APP_ID: &str = "wx48db31d50e334801";
const CONNECT_URL: &str = "https://open.weixin.qq.com/connect/qrconnect";

pub async fn run_wechat_login(events: mpsc::UnboundedSender<LoginEvent>, client_guid: String) {
    match tokio::time::timeout(Duration::from_secs(180), login(&events, client_guid)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = events.send(LoginEvent::Failed(format!("{error:#}")));
        }
        Err(_) => {
            let _ = events.send(LoginEvent::Expired);
        }
    }
}

async fn login(events: &mpsc::UnboundedSender<LoginEvent>, client_guid: String) -> Result<()> {
    ensure!(
        !client_guid.trim().is_empty(),
        "设备标识未保存，请检查应用设置后重新启动"
    );
    let client = LoginClient::new(client_guid.clone());
    let page = client
        .client
        .get(CONNECT_URL)
        .query(&[
            ("appid", APP_ID),
            (
                "redirect_uri",
                "https://y.qq.com/portal/wx_redirect.html?login_type=2&surl=https://y.qq.com/",
            ),
            ("response_type", "code"),
            ("scope", "snsapi_login"),
            ("state", "STATE"),
            (
                "href",
                "https://y.qq.com/mediastyle/music_v17/src/css/popup_wechat.css#wechat_redirect",
            ),
        ])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let uuid = page
        .split_once("uuid=")
        .and_then(|(_, tail)| tail.split_once('"'))
        .map(|(uuid, _)| uuid)
        .context("微信登录页面缺少二维码标识")?;
    ensure!(
        !uuid.is_empty()
            && uuid
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-=".contains(&c)),
        "微信二维码标识格式无效"
    );
    let image = client
        .client
        .get(format!("https://open.weixin.qq.com/connect/qrcode/{uuid}"))
        .header(REFERER, CONNECT_URL)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let _ = events.send(LoginEvent::QrReady(image.to_vec()));
    loop {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .to_string();
        let response = client
            .client
            .get("https://lp.open.weixin.qq.com/connect/l/qrconnect")
            .header(REFERER, "https://open.weixin.qq.com/")
            .query(&[("uuid", uuid), ("_", timestamp.as_str())])
            .timeout(Duration::from_secs(35))
            .send()
            .await;
        // 微信以长轮询等待状态变化；单次超时不代表扫码失败。
        let response = match response {
            Err(error) if error.is_timeout() => continue,
            response => response?,
        };
        let body = response.error_for_status()?.text().await?;
        let (status, code) = parse_status(&body)?;
        match status {
            408 => {}
            404 => {
                let _ = events.send(LoginEvent::WaitingConfirm);
            }
            402 => {
                let _ = events.send(LoginEvent::Expired);
                return Ok(());
            }
            403 => bail!("已取消微信登录"),
            405 => {
                ensure!(!code.is_empty(), "微信确认登录后未返回授权码");
                // 跳转 URL 中 login_type=2，音乐 RPC 中微信却使用 tmeLoginType=1。
                let response = client
                    .post::<LoginInfoResponse>(
                        json!({
                            "result": {"module": "music.login.LoginServer", "method": "Login",
                                "param": {"code": code, "strAppid": APP_ID}},
                            "comm": {"tmeLoginType": 1}
                        }),
                        None,
                    )
                    .await?;
                let credential = QqCredential::from_token(response.into_token()?, client_guid);
                let credential = ProtocolClient::new()?
                    .ensure_encrypted_uin(credential)
                    .await?;
                let _ = events.send(LoginEvent::Succeeded(credential));
                return Ok(());
            }
            _ => bail!("微信扫码返回未知状态：{status}"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn parse_status(body: &str) -> Result<(u16, &str)> {
    let (status, tail) = body
        .split_once("window.wx_errcode=")
        .and_then(|(_, tail)| tail.split_once(';'))
        .context("微信扫码响应缺少状态码")?;
    let code = tail
        .split_once("window.wx_code='")
        .and_then(|(_, tail)| tail.split_once('\''))
        .map(|(code, _)| code)
        .context("微信扫码响应缺少授权码字段")?;
    Ok((status.parse().context("微信扫码状态码无效")?, code))
}

#[cfg(test)]
mod tests {
    use super::parse_status;

    #[tokio::test]
    #[ignore = "requires live WeChat network access; does not authorize a login"]
    async fn test_wechat_qr() -> anyhow::Result<()> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(super::run_wechat_login(sender, crate::new_client_guid()));
        let event = tokio::time::timeout(std::time::Duration::from_secs(40), receiver.recv()).await;
        task.abort();
        match event? {
            Some(crate::LoginEvent::QrReady(bytes)) => {
                anyhow::ensure!(
                    bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.starts_with(b"\xff\xd8\xff"),
                    "QR image must be PNG or JPEG"
                );
                Ok(())
            }
            Some(crate::LoginEvent::Failed(error)) => anyhow::bail!("{error}"),
            _ => anyhow::bail!("QR login ended before receiving an image"),
        }
    }

    #[test]
    fn test_scan_status() {
        assert_eq!(
            parse_status("window.wx_errcode=408;window.wx_code='';").unwrap(),
            (408, "")
        );
        assert_eq!(
            parse_status("window.wx_errcode=405;window.wx_code='code';").unwrap(),
            (405, "code")
        );
        assert!(parse_status("<html>error</html>").is_err());
    }
}
