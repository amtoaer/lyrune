use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use async_channel::Receiver;
use directories::ProjectDirs;

const INSTANCE_FILE_HEADER: &str = "LYRUNE_INSTANCE_V1";
const SHOW_COMMAND: &[u8] = b"show\n";
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstanceCommand {
    Show,
}

pub enum InstanceClaim {
    Primary(PrimaryInstance),
    Secondary,
}

pub struct PrimaryInstance {
    commands: Receiver<InstanceCommand>,
    shutdown: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<()>>,
    _lock: File,
}

impl PrimaryInstance {
    pub fn commands(&self) -> Receiver<InstanceCommand> {
        self.commands.clone()
    }
}

impl Drop for PrimaryInstance {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.listener_thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn acquire() -> Result<InstanceClaim> {
    acquire_at(&instance_path()?)
}

fn instance_path() -> Result<PathBuf> {
    let dirs =
        ProjectDirs::from("dev", "lyrune", "Lyrune").context("无法确定 Lyrune 单例状态目录")?;
    Ok(dirs.cache_dir().join("instance.lock"))
}

fn acquire_at(path: &Path) -> Result<InstanceClaim> {
    let parent = path.parent().context("单例状态路径缺少父目录")?;
    fs::create_dir_all(parent).context("无法创建单例状态目录")?;
    let mut lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .context("无法打开单例状态文件")?;

    match lock.try_lock() {
        Ok(()) => start_primary(lock).map(InstanceClaim::Primary),
        Err(fs::TryLockError::WouldBlock) => {
            notify_primary(&mut lock)?;
            Ok(InstanceClaim::Secondary)
        }
        Err(fs::TryLockError::Error(error)) => Err(error).context("无法锁定单例状态文件"),
    }
}

fn start_primary(mut lock: File) -> Result<PrimaryInstance> {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .context("无法创建单例激活端点")?;
    listener
        .set_nonblocking(true)
        .context("无法配置单例激活端点")?;
    let address = listener.local_addr().context("无法读取单例激活地址")?;

    lock.set_len(0).context("无法清空单例状态")?;
    lock.seek(SeekFrom::Start(0)).context("无法写入单例状态")?;
    write!(lock, "{INSTANCE_FILE_HEADER}\n{}\n", address.port()).context("无法写入单例激活地址")?;
    lock.sync_data().context("无法同步单例状态")?;

    let (commands, command_events) = async_channel::unbounded();
    let shutdown = Arc::new(AtomicBool::new(false));
    let listener_shutdown = shutdown.clone();
    let listener_thread = thread::Builder::new()
        .name("lyrune-instance".to_owned())
        .spawn(move || {
            while !listener_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                        let mut command = [0; SHOW_COMMAND.len()];
                        if stream.read_exact(&mut command).is_ok() && command == SHOW_COMMAND {
                            let _ = commands.try_send(InstanceCommand::Show);
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(POLL_INTERVAL);
                    }
                    Err(_) => break,
                }
            }
        })
        .context("无法启动单例激活监听")?;

    Ok(PrimaryInstance {
        commands: command_events,
        shutdown,
        listener_thread: Some(listener_thread),
        _lock: lock,
    })
}

fn notify_primary(lock: &mut File) -> Result<()> {
    let deadline = Instant::now() + ACTIVATE_TIMEOUT;
    let mut last_error = None;
    while Instant::now() < deadline {
        match read_primary_address(lock).and_then(send_show_command) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        thread::sleep(POLL_INTERVAL);
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("主实例尚未发布激活地址")))
        .context("无法唤醒正在运行的 Lyrune")
}

fn read_primary_address(lock: &mut File) -> Result<SocketAddr> {
    lock.seek(SeekFrom::Start(0)).context("无法读取单例状态")?;
    let mut contents = String::new();
    lock.read_to_string(&mut contents)
        .context("无法读取单例状态")?;
    let mut lines = contents.lines();
    if lines.next() != Some(INSTANCE_FILE_HEADER) {
        bail!("单例状态尚未就绪");
    }
    let port = lines
        .next()
        .context("单例状态缺少激活端口")?
        .parse::<u16>()
        .context("单例激活端口无效")?;
    Ok(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into())
}

fn send_show_command(address: SocketAddr) -> Result<()> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(250))
        .context("无法连接主实例")?;
    stream
        .write_all(SHOW_COMMAND)
        .context("无法发送窗口激活命令")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static TEST_NONCE: AtomicU64 = AtomicU64::new(0);

    fn test_instance_path() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "lyrune-instance-{}-{timestamp}-{}",
            std::process::id(),
            TEST_NONCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn secondary_launch_notifies_the_primary_instance() {
        let path = test_instance_path();
        let primary = match acquire_at(&path).expect("claim primary instance") {
            InstanceClaim::Primary(primary) => primary,
            InstanceClaim::Secondary => panic!("first claim must be primary"),
        };

        assert!(matches!(
            acquire_at(&path).expect("notify primary instance"),
            InstanceClaim::Secondary
        ));
        assert_eq!(
            primary
                .commands
                .recv_blocking()
                .expect("receive activation command"),
            InstanceCommand::Show
        );

        drop(primary);
        let _ = fs::remove_file(path);
    }
}
