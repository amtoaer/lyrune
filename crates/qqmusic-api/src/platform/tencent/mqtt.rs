#![allow(rustdoc::private_intra_doc_links)]
//! QQ 扫码登录 MQTT 会话实现。
//!
//! # Overview
//!
//! 本模块负责维护扫码登录期间的 MQTT 连接状态，并将平台事件映射为
//! [`MqttLoginEvent`]。
//!
//! # Errors and Limits
//!
//! 重试与超时行为由 [`MQTT_CONNECT_TIMEOUT`]、[`MQTT_SUBACK_TIMEOUT`]、
//! [`MQTT_EVENT_WAIT_TIMEOUT`]、[`MQTT_CALL_TIMEOUT`]、[`MQTT_DEFAULT_INTERVAL`] 与
//! [`MQTT_ERROR_INTERVAL`] 控制。

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_tungstenite::tokio::{ConnectStream, connect_async};
use async_tungstenite::tungstenite::Message;
use async_tungstenite::tungstenite::client::IntoClientRequest;
use bytes::BytesMut;
use futures_util::StreamExt;
use rand::RngExt;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::{
    Connect, ConnectProperties, ConnectReturnCode, Filter, Packet, PingResp, Publish, Subscribe,
    SubscribeProperties, SubscribeReasonCode,
};
use serde_json::Value;

use crate::error::{MusicClientError, MusicClientResult};

const MQTT_HOST: &str = "mu.y.qq.com";
const MQTT_PORT: u16 = 443;
const MQTT_PATH: &str = "/ws/handshake";
const MQTT_KEEP_ALIVE: u16 = 45;
const MQTT_MAX_REDIRECTS: usize = 3;
const MQTT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MQTT_SUBACK_TIMEOUT: Duration = Duration::from_secs(5);
const MQTT_EVENT_WAIT_TIMEOUT: Duration = Duration::from_millis(1500);
const MQTT_CALL_TIMEOUT: Duration = Duration::from_secs(6);
const MQTT_DEFAULT_INTERVAL: Duration = Duration::from_millis(1500);
const MQTT_ERROR_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MqttLoginEvent {
    WaitingScan,
    WaitingConfirm,
    QrCodeExpired,
    Canceled,
    LoginFailed,
    Cookies { music_id: u64, music_key: String },
}

impl MqttLoginEvent {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::QrCodeExpired | Self::Canceled | Self::LoginFailed | Self::Cookies { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingLoginState {
    WaitingScan,
    WaitingConfirm,
}

impl PendingLoginState {
    fn as_event(self) -> MqttLoginEvent {
        match self {
            Self::WaitingScan => MqttLoginEvent::WaitingScan,
            Self::WaitingConfirm => MqttLoginEvent::WaitingConfirm,
        }
    }

    fn update(self, event: &MqttLoginEvent) -> Self {
        match event {
            MqttLoginEvent::WaitingScan => Self::WaitingScan,
            MqttLoginEvent::WaitingConfirm => Self::WaitingConfirm,
            _ => self,
        }
    }
}

pub(crate) struct MqttLoginSession {
    qrcode_id: String,
    socket: Option<MqttWebSocket>,
    pending_state: PendingLoginState,
}

impl MqttLoginSession {
    /// 基于二维码 ID 创建 MQTT 登录会话。
    pub(crate) fn new(qrcode_id: &str) -> Self {
        Self {
            qrcode_id: qrcode_id.to_owned(),
            socket: None,
            pending_state: PendingLoginState::WaitingScan,
        }
    }

    fn fallback_event(&self) -> MqttLoginEvent {
        self.pending_state.as_event()
    }

    fn update_pending_state(&mut self, event: &MqttLoginEvent) {
        self.pending_state = self.pending_state.update(event);
        if event.is_terminal() {
            self.pending_state = PendingLoginState::WaitingScan;
        }
    }

    pub(crate) async fn poll_event(&mut self) -> MusicClientResult<MqttLoginEvent> {
        let deadline = Instant::now() + MQTT_CALL_TIMEOUT;
        let mut retries = 0u32;

        loop {
            match self.poll_event_once().await {
                Ok(event) => return Ok(event),
                Err(err) => {
                    if !is_transient_mqtt_error(&err) {
                        self.socket = None;
                        self.pending_state = PendingLoginState::WaitingScan;
                        return Err(err);
                    }

                    let now = Instant::now();
                    if now >= deadline {
                        self.socket = None;
                        return Ok(self.fallback_event());
                    }

                    self.socket = None;
                    let backoff = retry_backoff(retries);
                    let remain = deadline.saturating_duration_since(now);
                    tokio::time::sleep(backoff.min(remain)).await;
                    retries = retries.saturating_add(1);
                }
            }
        }
    }

    async fn poll_event_once(&mut self) -> MusicClientResult<MqttLoginEvent> {
        if self.socket.is_none() {
            let mut mqtt = MqttWebSocket::connect(self.qrcode_id.as_str()).await?;
            mqtt.subscribe(self.qrcode_id.as_str()).await?;
            self.socket = Some(mqtt);
        }

        let event = {
            let mqtt = self.socket.as_mut().expect("socket should be initialized");
            match mqtt.next_login_event(MQTT_EVENT_WAIT_TIMEOUT).await? {
                Some(event) => event,
                None => self.fallback_event(),
            }
        };
        self.update_pending_state(&event);

        if event.is_terminal() {
            self.socket = None;
        }

        Ok(event)
    }
}

fn is_transient_mqtt_error(err: &MusicClientError) -> bool {
    match err {
        MusicClientError::TencentMqttLogin(message) => {
            message.contains("timed out")
                || message.contains("websocket connect failed")
                || message.contains("mqtt read frame failed")
                || message.contains("send mqtt packet failed")
        }
        _ => false,
    }
}

fn retry_backoff(retries: u32) -> Duration {
    let factor = 2f64.powi(retries.min(10) as i32);
    let secs =
        (MQTT_DEFAULT_INTERVAL.as_secs_f64() * factor).min(MQTT_ERROR_INTERVAL.as_secs_f64());
    Duration::from_secs_f64(secs)
}

struct MqttWebSocket {
    stream: async_tungstenite::WebSocketStream<ConnectStream>,
    pending_event: Option<MqttLoginEvent>,
}

enum ConnectHandshake {
    Connected,
    Redirect(String),
}

impl MqttWebSocket {
    async fn connect(qrcode_id: &str) -> MusicClientResult<Self> {
        let mut server_reference: Option<String> = None;

        for redirect_count in 0..=MQTT_MAX_REDIRECTS {
            let path = handshake_path(server_reference.as_deref());
            let mut socket = Self::open(path.as_str()).await?;
            match socket.connect_mqtt(qrcode_id).await? {
                ConnectHandshake::Connected => return Ok(socket),
                ConnectHandshake::Redirect(next_server_reference) => {
                    if redirect_count == MQTT_MAX_REDIRECTS {
                        return Err(MusicClientError::TencentMqttLogin(format!(
                            "too many mqtt redirects, last server reference: \
                             {next_server_reference}"
                        )));
                    }
                    server_reference = Some(next_server_reference);
                }
            }
        }

        Err(MusicClientError::TencentMqttLogin(
            "unreachable redirect state".to_owned(),
        ))
    }

    async fn open(path: &str) -> MusicClientResult<Self> {
        let url = format!("wss://{MQTT_HOST}:{MQTT_PORT}{path}");
        let mut request = url.as_str().into_client_request().map_err(|err| {
            MusicClientError::TencentMqttLogin(format!("build websocket request failed: {err}"))
        })?;

        let headers = request.headers_mut();
        headers.insert(
            "Sec-WebSocket-Protocol",
            "mqtt".parse().expect("valid static header"),
        );
        headers.insert(
            "Origin",
            "https://y.qq.com".parse().expect("valid static header"),
        );
        headers.insert(
            "Referer",
            "https://y.qq.com/".parse().expect("valid static header"),
        );
        headers.insert(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/123.0.0.0 Safari/537.36"
                .parse()
                .expect("valid static header"),
        );

        let (stream, _) = tokio::time::timeout(MQTT_CONNECT_TIMEOUT, connect_async(request))
            .await
            .map_err(|_| {
                MusicClientError::TencentMqttLogin("websocket connect timed out".to_owned())
            })?
            .map_err(|err| {
                MusicClientError::TencentMqttLogin(format!("websocket connect failed: {err}"))
            })?;

        Ok(Self {
            stream,
            pending_event: None,
        })
    }

    async fn connect_mqtt(&mut self, qrcode_id: &str) -> MusicClientResult<ConnectHandshake> {
        let mut connect_props = ConnectProperties::new();
        connect_props.authentication_method = Some("pass".to_owned());
        connect_props.user_properties = vec![
            ("tmeAppID".to_owned(), "qqmusic".to_owned()),
            ("business".to_owned(), "management".to_owned()),
            ("hashTag".to_owned(), qrcode_id.to_owned()),
            ("clientTag".to_owned(), "management.user".to_owned()),
            ("userID".to_owned(), qrcode_id.to_owned()),
        ];

        self.send_packet(Packet::Connect(
            Connect {
                keep_alive: MQTT_KEEP_ALIVE,
                client_id: build_client_id(),
                clean_start: true,
                properties: Some(connect_props),
            },
            None,
            None,
        ))
        .await?;

        let packet = self.next_packet(MQTT_CONNECT_TIMEOUT, false).await?;
        let connack = match packet {
            Some(Packet::ConnAck(connack)) => connack,
            Some(other) => {
                return Err(MusicClientError::TencentMqttLogin(format!(
                    "expected connack, got packet: {other:?}"
                )));
            }
            None => {
                return Err(MusicClientError::TencentMqttLogin(
                    "mqtt connack timed out".to_owned(),
                ));
            }
        };

        match connack.code {
            ConnectReturnCode::Success => Ok(ConnectHandshake::Connected),
            ConnectReturnCode::UseAnotherServer | ConnectReturnCode::ServerMoved => {
                let Some(server_reference) = connack
                    .properties
                    .and_then(|properties| properties.server_reference)
                    .filter(|value| !value.is_empty())
                else {
                    return Err(MusicClientError::TencentMqttLogin(
                        "mqtt redirect missing server reference".to_owned(),
                    ));
                };
                Ok(ConnectHandshake::Redirect(server_reference))
            }
            code => Err(MusicClientError::TencentMqttLogin(format!(
                "mqtt connect rejected: {code:?}"
            ))),
        }
    }

    async fn subscribe(&mut self, qrcode_id: &str) -> MusicClientResult<()> {
        let topic = format!("management.qrcode_login/{qrcode_id}");
        let mut subscribe = Subscribe::new(
            Filter::new(topic, QoS::AtMostOnce),
            Some(SubscribeProperties {
                id: None,
                user_properties: vec![
                    ("authorization".to_owned(), "tmelogin".to_owned()),
                    ("pubsub".to_owned(), "unicast".to_owned()),
                ],
            }),
        );
        subscribe.pkid = 1;
        self.send_packet(Packet::Subscribe(subscribe)).await?;

        loop {
            let packet = self.next_packet(MQTT_SUBACK_TIMEOUT, false).await?;
            match packet {
                Some(Packet::SubAck(suback)) if suback.pkid == 1 => {
                    if suback
                        .return_codes
                        .iter()
                        .any(|code| !matches!(code, SubscribeReasonCode::Success(_)))
                    {
                        return Err(MusicClientError::TencentMqttLogin(format!(
                            "mqtt subscribe rejected: {:?}",
                            suback.return_codes
                        )));
                    }
                    return Ok(());
                }
                Some(Packet::Publish(publish)) => {
                    if self.pending_event.is_none() {
                        self.pending_event = parse_publish_event(&publish);
                    }
                }
                Some(_) => continue,
                None => {
                    return Err(MusicClientError::TencentMqttLogin(
                        "mqtt suback timed out".to_owned(),
                    ));
                }
            }
        }
    }

    async fn next_login_event(
        &mut self,
        wait_timeout: Duration,
    ) -> MusicClientResult<Option<MqttLoginEvent>> {
        if let Some(event) = self.pending_event.take() {
            return Ok(Some(event));
        }

        loop {
            let packet = self.next_packet(wait_timeout, true).await?;
            let Some(packet) = packet else {
                return Ok(None);
            };

            match packet {
                Packet::Publish(publish) => {
                    if let Some(event) = parse_publish_event(&publish) {
                        return Ok(Some(event));
                    }
                }
                Packet::PingReq(_) => {
                    self.send_packet(Packet::PingResp(PingResp)).await?;
                }
                Packet::Disconnect(_) => return Ok(Some(MqttLoginEvent::LoginFailed)),
                _ => {}
            }
        }
    }

    async fn next_packet(
        &mut self,
        timeout: Duration,
        timeout_as_none: bool,
    ) -> MusicClientResult<Option<Packet>> {
        loop {
            let frame = match tokio::time::timeout(timeout, self.stream.next()).await {
                Ok(frame) => frame,
                Err(_) if timeout_as_none => return Ok(None),
                Err(_) => {
                    return Err(MusicClientError::TencentMqttLogin(
                        "mqtt packet read timed out".to_owned(),
                    ));
                }
            };

            let Some(frame) = frame else {
                return Ok(None);
            };
            let frame = frame.map_err(|err| {
                MusicClientError::TencentMqttLogin(format!("mqtt read frame failed: {err}"))
            })?;

            match frame {
                Message::Binary(payload) => {
                    let mut bytes = BytesMut::from(payload.as_ref());
                    let packet = Packet::read(&mut bytes, None).map_err(|err| {
                        MusicClientError::TencentMqttLogin(format!(
                            "decode mqtt packet failed: {err}"
                        ))
                    })?;
                    return Ok(Some(packet));
                }
                Message::Close(_) => return Ok(None),
                Message::Ping(_) | Message::Pong(_) | Message::Text(_) | Message::Frame(_) => {
                    continue;
                }
            }
        }
    }

    async fn send_packet(&mut self, packet: Packet) -> MusicClientResult<()> {
        let mut bytes = BytesMut::new();
        packet.write(&mut bytes, None).map_err(|err| {
            MusicClientError::TencentMqttLogin(format!("encode mqtt packet failed: {err}"))
        })?;

        self.stream
            .send(Message::Binary(bytes.freeze()))
            .await
            .map_err(|err| {
                MusicClientError::TencentMqttLogin(format!("send mqtt packet failed: {err}"))
            })
    }
}

fn build_client_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let random = rand::rng().random_range(1000..=9999);
    format!("{millis}{random}")
}

fn parse_publish_event(publish: &Publish) -> Option<MqttLoginEvent> {
    // 登录状态由 MQTT user_properties.type 标识，不同事件共用同一 topic。
    let event_type = publish.properties.as_ref().and_then(|properties| {
        properties
            .user_properties
            .iter()
            .find_map(|(key, value)| (key == "type").then_some(value.as_str()))
    })?;

    match event_type {
        "scanned" => Some(MqttLoginEvent::WaitingConfirm),
        "canceled" => Some(MqttLoginEvent::Canceled),
        "timeout" => Some(MqttLoginEvent::QrCodeExpired),
        "loginFailed" => Some(MqttLoginEvent::LoginFailed),
        "cookies" => Some(
            parse_cookies_event(publish.payload.as_ref()).unwrap_or(MqttLoginEvent::LoginFailed),
        ),
        _ => None,
    }
}

fn parse_cookies_event(payload: &[u8]) -> Option<MqttLoginEvent> {
    let payload: Value = serde_json::from_slice(payload).ok()?;
    let cookies = payload.get("cookies")?.as_object()?;

    let music_id = extract_cookie_value(cookies, "qqmusic_uin")?
        .parse::<u64>()
        .ok()?;
    let music_key = extract_cookie_value(cookies, "qqmusic_key")?;
    if music_key.is_empty() {
        return Some(MqttLoginEvent::LoginFailed);
    }

    Some(MqttLoginEvent::Cookies {
        music_id,
        music_key,
    })
}

fn extract_cookie_value(cookies: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    // 兼容 cookies 的三种形态：直接字符串、数字、以及 { "value": ... } 包装对象。
    let value = cookies.get(key)?;
    if let Some(raw) = value.as_str() {
        return Some(raw.to_owned());
    }
    if let Some(raw) = value.as_u64() {
        return Some(raw.to_string());
    }
    let nested = value.as_object()?.get("value")?;
    if let Some(raw) = nested.as_str() {
        return Some(raw.to_owned());
    }
    nested.as_u64().map(|raw| raw.to_string())
}

fn handshake_path(server_reference: Option<&str>) -> String {
    // 部分握手响应会返回重定向节点，需要拼接到基础握手路径。
    match server_reference {
        Some(server_reference) => format!("{MQTT_PATH}/{server_reference}"),
        None => MQTT_PATH.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use rumqttc::v5::mqttbytes::v5::PublishProperties;

    use super::*;

    fn mock_publish(event_type: &str, payload: &[u8]) -> Publish {
        Publish::new(
            "management.qrcode_login/test",
            QoS::AtMostOnce,
            payload.to_vec(),
            Some(PublishProperties {
                user_properties: vec![("type".to_owned(), event_type.to_owned())],
                ..Default::default()
            }),
        )
    }

    #[test]
    fn handshake_path_without_redirect_uses_default_path() {
        let path = handshake_path(None);
        assert_eq!(path, "/ws/handshake");
    }

    #[test]
    fn handshake_path_with_redirect_appends_server_reference() {
        let path = handshake_path(Some("2.2.2.2:29001"));
        assert_eq!(path, "/ws/handshake/2.2.2.2:29001");
    }

    #[test]
    fn timeout_keeps_waiting_confirm_state() {
        let mut session = MqttLoginSession::new("qrcode_id");
        session.update_pending_state(&MqttLoginEvent::WaitingConfirm);
        assert_eq!(session.fallback_event(), MqttLoginEvent::WaitingConfirm);
    }

    #[test]
    fn terminal_event_resets_fallback_state() {
        let mut session = MqttLoginSession::new("qrcode_id");
        session.update_pending_state(&MqttLoginEvent::WaitingConfirm);
        session.update_pending_state(&MqttLoginEvent::LoginFailed);
        assert_eq!(session.fallback_event(), MqttLoginEvent::WaitingScan);
    }

    #[test]
    fn parse_scanned_event() {
        let publish = mock_publish("scanned", b"{}");
        assert_eq!(
            parse_publish_event(&publish),
            Some(MqttLoginEvent::WaitingConfirm)
        );
    }

    #[test]
    fn parse_cookies_event() {
        let publish = mock_publish(
            "cookies",
            br#"{"cookies":{"qqmusic_uin":{"value":"10001"},"qqmusic_key":{"value":"Q_H_L_test"}}}"#,
        );
        assert_eq!(
            parse_publish_event(&publish),
            Some(MqttLoginEvent::Cookies {
                music_id: 10001,
                music_key: "Q_H_L_test".to_owned()
            })
        );
    }

    #[test]
    fn parse_invalid_cookies_as_login_failed() {
        let publish = mock_publish("cookies", br#"{"cookies":{}}"#);
        assert_eq!(
            parse_publish_event(&publish),
            Some(MqttLoginEvent::LoginFailed)
        );
    }
}
