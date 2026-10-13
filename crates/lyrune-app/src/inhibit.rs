use anyhow::{Result, bail};
use ashpd::desktop::Request;
use ashpd::desktop::inhibit::{InhibitFlags, InhibitOptions, InhibitProxy};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use zbus_systemd::login1::ManagerProxy;
use zbus_systemd::zbus::{Connection, zvariant::OwnedFd};

use crate::runtime::RUNTIME;

const INHIBIT_APP_ID: &str = "Lyrune";
const INHIBIT_REASON: &str = "Lyrune 正在播放音乐";

pub struct InhibitHandle {
    updates: watch::Sender<bool>,
    _task: JoinHandle<()>,
}

impl InhibitHandle {
    pub fn set_active(&self, active: bool) {
        self.updates.send_replace(active);
    }
}

enum Backend {
    Portal(InhibitProxy),
    Logind(Connection),
    Unavailable,
}

#[allow(clippy::large_enum_variant)]
enum Inhibition {
    Portal(Request<()>),
    Logind(OwnedFd),
}

pub fn install() -> InhibitHandle {
    let (updates, update_events) = watch::channel(false);
    let task = RUNTIME.spawn(serve(None, update_events));
    InhibitHandle {
        updates,
        _task: task,
    }
}

async fn serve(mut backend: Option<Backend>, mut updates: watch::Receiver<bool>) {
    let mut held = None;
    while updates.changed().await.is_ok() {
        let should_inhibit = *updates.borrow_and_update();
        if should_inhibit == held.is_some() {
            continue;
        }
        if should_inhibit {
            if backend.is_none() {
                let (selected_backend, inhibition) = acquire_initial().await;
                (backend, held) = (Some(selected_backend), inhibition);
            } else if let Some(backend) = backend.as_ref()
                && !matches!(backend, Backend::Unavailable)
            {
                match acquire(backend).await {
                    Ok(inhibition) => held = Some(inhibition),
                    Err(error) => eprintln!("无法阻止系统睡眠：{error:#}"),
                }
            }
        } else if let Some(inhibition) = held.take() {
            release(inhibition).await;
        }
    }
    if let Some(inhibition) = held {
        release(inhibition).await;
    }
}

async fn acquire_initial() -> (Backend, Option<Inhibition>) {
    let result = async {
        if let Ok(portal) = InhibitProxy::new().await
            && let Ok(inhibition) = acquire_portal(&portal).await
        {
            return Ok((Backend::Portal(portal), Some(inhibition)));
        };
        let connection = Connection::system().await?;
        let dbus = zbus_systemd::zbus::fdo::DBusProxy::new(&connection).await?;
        let has_logind = dbus
            .name_has_owner(
                zbus_systemd::zbus::names::BusName::try_from("org.freedesktop.login1").unwrap(),
            )
            .await
            .unwrap_or(false);
        if has_logind && let Ok(inhibition) = acquire_logind(&connection).await {
            return Ok((Backend::Logind(connection), Some(inhibition)));
        }
        bail!("没有可用的睡眠抑制后端");
    };
    result
        .await
        .inspect_err(|e| eprintln!("{}", e))
        .unwrap_or((Backend::Unavailable, None))
}

async fn acquire(backend: &Backend) -> Result<Inhibition> {
    match backend {
        Backend::Portal(proxy) => acquire_portal(proxy).await,
        Backend::Logind(connection) => acquire_logind(connection).await,
        _ => unreachable!(),
    }
}

async fn acquire_portal(proxy: &InhibitProxy) -> Result<Inhibition> {
    let request = proxy
        .inhibit(
            None,
            InhibitFlags::Suspend.into(),
            InhibitOptions::default().set_reason(INHIBIT_REASON),
        )
        .await?;
    request.response()?;
    Ok(Inhibition::Portal(request))
}

async fn acquire_logind(connection: &Connection) -> Result<Inhibition> {
    let manager = ManagerProxy::new(connection).await?;
    Ok(Inhibition::Logind(
        manager
            .inhibit(
                "sleep".to_owned(),
                INHIBIT_APP_ID.to_owned(),
                INHIBIT_REASON.to_owned(),
                "block".to_owned(),
            )
            .await?,
    ))
}

async fn release(inhibition: Inhibition) {
    match inhibition {
        Inhibition::Portal(request) => {
            if let Err(error) = request.close().await {
                eprintln!("无法解除 Portal 睡眠抑制：{error:#}");
            }
        }
        Inhibition::Logind(fd) => drop(fd),
    }
}
