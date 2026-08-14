use std::collections::HashMap;
use std::convert::Infallible;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use bytes::Bytes;
use directories::ProjectDirs;
use futures_util::{Stream, StreamExt as _};
use qqmusic_api::integration::{Quality, Track};
use reqwest::header::{
    ACCEPT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderMap, IF_RANGE,
    LAST_MODIFIED, RANGE, REFERER,
};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use stream_download::source::{SourceStream, StreamOutcome};
use stream_download::storage::StorageProvider;
use stream_download::{Settings, StreamDownload};
use tokio::io::AsyncReadExt as _;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

const CACHE_SCHEMA_VERSION: u32 = 1;
const PREFETCH_BYTES: u64 = 1024 * 1024;
const PREFIX_PROBE_BYTES: u64 = 64 * 1024;
const QQ_REFERER: &str = "https://y.qq.com/";

type CacheKeyLock = Arc<AsyncMutex<()>>;
type ByteStream = Box<dyn Stream<Item = io::Result<Bytes>> + Unpin + Send + Sync>;
type StreamingSource = StreamDownload<CacheStorageProvider>;

#[derive(Clone)]
pub struct AudioCache {
    root: Arc<PathBuf>,
    client: Client,
    key_locks: Arc<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
}

impl AudioCache {
    pub fn new() -> Result<Self> {
        let project_dirs =
            ProjectDirs::from("dev", "lyrune", "Lyrune").context("无法确定 Lyrune 音频缓存目录")?;
        Self::with_root(project_dirs.cache_dir().join("audio-v1"))
    }

    fn with_root(root: PathBuf) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                 AppleWebKit/537.36 Chrome/131.0 Safari/537.36",
            )
            .build()
            .context("无法创建音频流 HTTP 客户端")?;

        Ok(Self {
            root: Arc::new(root),
            client,
            key_locks: Arc::default(),
        })
    }

    pub async fn prepare(
        &self,
        url: &str,
        track: &Track,
        quality: Quality,
    ) -> Result<PreparedStream> {
        tokio::fs::create_dir_all(self.root.as_ref())
            .await
            .context("无法创建音频缓存目录")?;

        let identity = CacheIdentity::new(track, quality);
        let key = identity.key();
        let lock = self.key_lock(&key);
        let guard = lock.lock_owned().await;
        let paths = CachePaths::new(self.root.as_ref(), &key);

        let mut metadata = read_metadata(&paths.metadata).await;
        let mut existing_length = file_length(&paths.media).await.unwrap_or_default();
        if metadata
            .as_ref()
            .is_some_and(|metadata| metadata.identity != identity)
            || (existing_length > 0 && metadata.is_none())
        {
            reset_media(&paths.media).await?;
            metadata = None;
            existing_length = 0;
        }

        let remote = self.inspect_remote(url).await.ok();
        if let (Some(cached_metadata), Some(remote)) = (&metadata, &remote)
            && cached_metadata.conflicts_with(remote)
        {
            reset_media(&paths.media).await?;
            metadata = None;
            existing_length = 0;
        }

        if existing_length > 0
            && !has_shared_validator(metadata.as_ref(), remote.as_ref())
            && self
                .prefix_matches(url, &paths.media, existing_length)
                .await
                .is_ok_and(|matches| !matches)
        {
            reset_media(&paths.media).await?;
            metadata = None;
            existing_length = 0;
        }

        let expected_length = remote
            .as_ref()
            .and_then(|remote| remote.content_length)
            .or_else(|| {
                metadata
                    .as_ref()
                    .and_then(|metadata| metadata.content_length)
            });
        if expected_length.is_some_and(|expected| existing_length > expected) {
            reset_media(&paths.media).await?;
            metadata = None;
            existing_length = 0;
        }

        if existing_length > 0 && expected_length == Some(existing_length) && metadata.is_some() {
            let source = File::open(&paths.media).context("无法打开已缓存的歌曲")?;
            drop(guard);
            return Ok(PreparedStream {
                source: CachedAudioSource::Complete(source),
                content_length: Some(existing_length),
                format_hint: quality_format_hint(quality),
                cache_status: CacheStatus::Complete,
                cancellation: None,
            });
        }

        let validator = metadata
            .as_ref()
            .and_then(CacheMetadata::if_range_validator)
            .map(str::to_owned);
        let (response, resume_from) = self
            .open_stream_response(url, &paths.media, existing_length, validator.as_deref())
            .await?;
        if existing_length > 0 && resume_from == 0 {
            metadata = None;
        }

        let response_remote = RemoteMetadata::from_response(&response, resume_from);
        let content_length = response_remote
            .content_length
            .or(expected_length)
            .or_else(|| {
                response
                    .content_length()
                    .map(|remaining| resume_from.saturating_add(remaining))
            });
        if content_length == Some(0) {
            bail!("QQ 音乐返回了空音频流");
        }

        let metadata = CacheMetadata {
            schema_version: CACHE_SCHEMA_VERSION,
            identity,
            content_length,
            etag: response_remote
                .etag
                .or_else(|| remote.as_ref().and_then(|remote| remote.etag.clone()))
                .or_else(|| metadata.as_ref().and_then(|metadata| metadata.etag.clone())),
            last_modified: response_remote
                .last_modified
                .or_else(|| {
                    remote
                        .as_ref()
                        .and_then(|remote| remote.last_modified.clone())
                })
                .or_else(|| {
                    metadata
                        .as_ref()
                        .and_then(|metadata| metadata.last_modified.clone())
                }),
            content_type: response_remote
                .content_type
                .or_else(|| {
                    remote
                        .as_ref()
                        .and_then(|remote| remote.content_type.clone())
                })
                .or_else(|| {
                    metadata
                        .as_ref()
                        .and_then(|metadata| metadata.content_type.clone())
                }),
            complete: false,
        };
        write_metadata(&paths.metadata, &metadata).await?;

        let local_stream: ByteStream = if resume_from > 0 {
            let local = tokio::fs::File::open(&paths.media)
                .await
                .context("无法读取部分歌曲缓存")?;
            Box::new(ReaderStream::new(local.take(resume_from)))
        } else {
            Box::new(futures_util::stream::empty())
        };
        let network_stream = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|error| io::Error::other(error.to_string())));
        let stream: ByteStream = Box::new(local_stream.chain(network_stream));
        let source = ResumeSource {
            stream,
            content_length,
            media_path: paths.media.clone(),
            metadata_path: paths.metadata.clone(),
            metadata,
            _guard: guard,
        };
        let settings = Settings::default()
            .prefetch_bytes(
                content_length.map_or(PREFETCH_BYTES, |length| PREFETCH_BYTES.min(length)),
            )
            .retry_timeout(Duration::from_secs(30));
        let download = StreamDownload::from_stream(
            source,
            CacheStorageProvider { path: paths.media },
            settings,
        )
        .await
        .map_err(|error| anyhow!("无法初始化歌曲流：{error}"))?;
        let cancellation = Some(download.cancellation_token());

        Ok(PreparedStream {
            source: CachedAudioSource::Streaming(download),
            content_length,
            format_hint: quality_format_hint(quality),
            cache_status: if resume_from == 0 {
                CacheStatus::Fresh
            } else {
                CacheStatus::Resumed(resume_from)
            },
            cancellation,
        })
    }

    fn key_lock(&self, key: &str) -> CacheKeyLock {
        let mut locks = self
            .key_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return lock;
        }

        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key.to_owned(), Arc::downgrade(&lock));
        lock
    }

    async fn inspect_remote(&self, url: &str) -> Result<RemoteMetadata> {
        let response = self
            .client
            .head(url)
            .header(REFERER, QQ_REFERER)
            .header(ACCEPT_ENCODING, "identity")
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .context("无法检查远端音频状态")?
            .error_for_status()
            .context("远端音频状态检查失败")?;
        let content_length = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        Ok(RemoteMetadata::from_headers(
            response.headers(),
            content_length,
        ))
    }

    async fn prefix_matches(&self, url: &str, path: &Path, local_length: u64) -> Result<bool> {
        let probe_length = PREFIX_PROBE_BYTES.min(local_length);
        if probe_length == 0 {
            return Ok(true);
        }

        let response = self
            .client
            .get(url)
            .header(REFERER, QQ_REFERER)
            .header(ACCEPT_ENCODING, "identity")
            .header(RANGE, format!("bytes=0-{}", probe_length - 1))
            .send()
            .await
            .context("无法验证歌曲缓存")?
            .error_for_status()
            .context("歌曲缓存验证请求失败")?;

        let mut remote_prefix = Vec::with_capacity(probe_length as usize);
        let mut stream = response.bytes_stream();
        while remote_prefix.len() < probe_length as usize {
            let Some(chunk) = stream.next().await else {
                break;
            };
            let chunk = chunk.context("读取歌曲缓存验证数据失败")?;
            let remaining = probe_length as usize - remote_prefix.len();
            remote_prefix.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }

        let mut local = tokio::fs::File::open(path)
            .await
            .context("无法打开歌曲缓存进行验证")?;
        let mut local_prefix = vec![0; probe_length as usize];
        local
            .read_exact(&mut local_prefix)
            .await
            .context("歌曲缓存长度与记录不一致")?;

        Ok(remote_prefix == local_prefix)
    }

    async fn open_stream_response(
        &self,
        url: &str,
        path: &Path,
        existing_length: u64,
        validator: Option<&str>,
    ) -> Result<(Response, u64)> {
        let mut request = self
            .client
            .get(url)
            .header(REFERER, QQ_REFERER)
            .header(ACCEPT_ENCODING, "identity");
        if existing_length > 0 {
            request = request.header(RANGE, format!("bytes={existing_length}-"));
            if let Some(validator) = validator {
                request = request.header(IF_RANGE, validator);
            }
        }

        let response = request.send().await.context("歌曲流请求失败")?;
        if existing_length > 0 && response.status() == StatusCode::PARTIAL_CONTENT {
            let range_start = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_content_range)
                .map(|range| range.start);
            if range_start == Some(existing_length) {
                return Ok((response, existing_length));
            }
        }

        if existing_length == 0 || response.status() == StatusCode::OK {
            response
                .error_for_status_ref()
                .context("歌曲下载地址拒绝了请求")?;
            if existing_length > 0 {
                reset_media(path).await?;
            }
            return Ok((response, 0));
        }

        drop(response);
        reset_media(path).await?;
        let response = self
            .client
            .get(url)
            .header(REFERER, QQ_REFERER)
            .header(ACCEPT_ENCODING, "identity")
            .send()
            .await
            .context("重新请求完整歌曲流失败")?
            .error_for_status()
            .context("歌曲下载地址拒绝了完整请求")?;
        Ok((response, 0))
    }
}

pub struct PreparedStream {
    pub source: CachedAudioSource,
    pub content_length: Option<u64>,
    pub format_hint: &'static str,
    pub cache_status: CacheStatus,
    pub cancellation: Option<CancellationToken>,
}

#[derive(Clone, Copy)]
pub enum CacheStatus {
    Fresh,
    Resumed(u64),
    Complete,
}

impl CacheStatus {
    pub fn description(self) -> String {
        match self {
            Self::Fresh => "边下载边播放".to_owned(),
            Self::Resumed(bytes) => format!("从 {} 缓存续传", format_bytes(bytes)),
            Self::Complete => "使用完整本地缓存".to_owned(),
        }
    }
}

pub enum CachedAudioSource {
    Complete(File),
    Streaming(StreamingSource),
}

impl Read for CachedAudioSource {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Complete(file) => file.read(buffer),
            Self::Streaming(stream) => stream.read(buffer),
        }
    }
}

impl Seek for CachedAudioSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Complete(file) => file.seek(position),
            Self::Streaming(stream) => stream.seek(position),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
struct CacheIdentity {
    provider: String,
    track_mid: String,
    media_mid: String,
    quality: String,
}

impl CacheIdentity {
    fn new(track: &Track, quality: Quality) -> Self {
        Self {
            provider: "qqmusic".to_owned(),
            track_mid: track.mid.clone(),
            media_mid: track.media_mid.clone(),
            quality: quality.cache_id().to_owned(),
        }
    }

    fn key(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(format!("lyrune-audio-v{CACHE_SCHEMA_VERSION}\0"));
        digest.update(self.provider.as_bytes());
        digest.update([0]);
        digest.update(self.track_mid.as_bytes());
        digest.update([0]);
        digest.update(self.media_mid.as_bytes());
        digest.update([0]);
        digest.update(self.quality.as_bytes());
        hex::encode(digest.finalize())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CacheMetadata {
    schema_version: u32,
    identity: CacheIdentity,
    content_length: Option<u64>,
    etag: Option<String>,
    last_modified: Option<String>,
    content_type: Option<String>,
    complete: bool,
}

impl CacheMetadata {
    fn conflicts_with(&self, remote: &RemoteMetadata) -> bool {
        self.schema_version != CACHE_SCHEMA_VERSION
            || different(self.content_length, remote.content_length)
            || different_ref(self.etag.as_deref(), remote.etag.as_deref())
            || different_ref(
                self.last_modified.as_deref(),
                remote.last_modified.as_deref(),
            )
    }

    fn if_range_validator(&self) -> Option<&str> {
        self.etag
            .as_deref()
            .filter(|etag| !etag.starts_with("W/"))
            .or(self.last_modified.as_deref())
    }
}

#[derive(Clone, Debug)]
struct RemoteMetadata {
    content_length: Option<u64>,
    etag: Option<String>,
    last_modified: Option<String>,
    content_type: Option<String>,
}

impl RemoteMetadata {
    fn from_headers(headers: &HeaderMap, content_length: Option<u64>) -> Self {
        Self {
            content_length,
            etag: header_string(headers, ETAG),
            last_modified: header_string(headers, LAST_MODIFIED),
            content_type: header_string(headers, CONTENT_TYPE),
        }
    }

    fn from_response(response: &Response, resume_from: u64) -> Self {
        let content_range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_content_range);
        let content_length = content_range.and_then(|range| range.total).or_else(|| {
            response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(|remaining| resume_from.saturating_add(remaining))
        });
        Self::from_headers(response.headers(), content_length)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ContentRange {
    start: u64,
    total: Option<u64>,
}

fn parse_content_range(value: &str) -> Option<ContentRange> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, _) = range.split_once('-')?;
    Some(ContentRange {
        start: start.parse().ok()?,
        total: (total != "*").then(|| total.parse().ok()).flatten(),
    })
}

fn has_shared_validator(metadata: Option<&CacheMetadata>, remote: Option<&RemoteMetadata>) -> bool {
    let Some((metadata, remote)) = metadata.zip(remote) else {
        return false;
    };
    (metadata.etag.is_some() && remote.etag.is_some())
        || (metadata.last_modified.is_some() && remote.last_modified.is_some())
}

fn different<T: Eq + Copy>(left: Option<T>, right: Option<T>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left != right)
}

fn different_ref<T: Eq + ?Sized>(left: Option<&T>, right: Option<&T>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left != right)
}

fn header_string(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn quality_format_hint(quality: Quality) -> &'static str {
    match quality {
        Quality::Standard | Quality::High => "mp3",
        Quality::Lossless => "flac",
    }
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes < 1024 * 1024 {
        format!("{:.0} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / MIB)
    }
}

struct CachePaths {
    media: PathBuf,
    metadata: PathBuf,
}

impl CachePaths {
    fn new(root: &Path, key: &str) -> Self {
        Self {
            media: root.join(format!("{key}.media")),
            metadata: root.join(format!("{key}.json")),
        }
    }
}

async fn read_metadata(path: &Path) -> Option<CacheMetadata> {
    let bytes = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn write_metadata(path: &Path, metadata: &CacheMetadata) -> Result<()> {
    let bytes = serde_json::to_vec(metadata).context("无法序列化歌曲缓存信息")?;
    tokio::fs::write(path, bytes)
        .await
        .context("无法保存歌曲缓存信息")
}

async fn reset_media(path: &Path) -> Result<()> {
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .await
        .context("无法重置过期的歌曲缓存")?;
    file.sync_data().await.context("无法同步歌曲缓存")?;
    Ok(())
}

async fn file_length(path: &Path) -> Option<u64> {
    tokio::fs::metadata(path)
        .await
        .ok()
        .map(|metadata| metadata.len())
}

#[derive(Clone, Debug)]
pub(crate) struct CacheStorageProvider {
    path: PathBuf,
}

impl StorageProvider for CacheStorageProvider {
    type Reader = File;
    type Writer = File;

    fn into_reader_writer(
        self,
        _content_length: Option<u64>,
    ) -> io::Result<(Self::Reader, Self::Writer)> {
        let mut writer = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.path)?;
        writer.seek(SeekFrom::Start(0))?;
        let reader = OpenOptions::new().read(true).open(&self.path)?;
        Ok((reader, writer))
    }
}

struct ResumeSource {
    stream: ByteStream,
    content_length: Option<u64>,
    media_path: PathBuf,
    metadata_path: PathBuf,
    metadata: CacheMetadata,
    _guard: OwnedMutexGuard<()>,
}

impl Stream for ResumeSource {
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut *self.stream).poll_next(context)
    }
}

impl SourceStream for ResumeSource {
    type Params = Self;
    type StreamCreationError = Infallible;

    async fn create(params: Self::Params) -> Result<Self, Self::StreamCreationError> {
        Ok(params)
    }

    fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    async fn seek_range(&mut self, _start: u64, _end: Option<u64>) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "persistent sequential stream does not support random range seeks",
        ))
    }

    async fn reconnect(&mut self, _current_position: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "persistent sequential stream cannot reconnect in place",
        ))
    }

    fn supports_seek(&self) -> bool {
        false
    }

    async fn on_finish(
        &mut self,
        result: io::Result<()>,
        outcome: StreamOutcome,
    ) -> io::Result<()> {
        let file_length = tokio::fs::metadata(&self.media_path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or_default();
        let expected_complete = self
            .content_length
            .is_none_or(|content_length| content_length == file_length);
        self.metadata.complete =
            result.is_ok() && outcome == StreamOutcome::Completed && expected_complete;
        if let Ok(bytes) = serde_json::to_vec(&self.metadata) {
            let _ = tokio::fs::write(&self.metadata_path, bytes).await;
        }

        if result.is_ok() && outcome == StreamOutcome::Completed && !expected_complete {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "stream ended at {file_length} bytes, expected {:?}",
                    self.content_length
                ),
            ));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Read as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use qqmusic_api::integration::{Quality, Track};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};

    use super::{
        AudioCache, CacheIdentity, CachePaths, ContentRange, file_length, parse_content_range,
    };

    fn track(mid: &str, media_mid: &str, title: &str) -> Track {
        Track {
            mid: mid.to_owned(),
            media_mid: media_mid.to_owned(),
            title: title.to_owned(),
            artists: String::new(),
            album: String::new(),
            duration_seconds: 0,
        }
    }

    #[test]
    fn cache_key_uses_stable_media_identity_and_quality() {
        let original = CacheIdentity::new(&track("song", "media", "Old title"), Quality::High);
        let renamed = CacheIdentity::new(&track("song", "media", "New title"), Quality::High);
        let lossless = CacheIdentity::new(&track("song", "media", "Old title"), Quality::Lossless);
        let replaced = CacheIdentity::new(&track("song", "media-v2", "Old title"), Quality::High);

        assert_eq!(original.key(), renamed.key());
        assert_ne!(original.key(), lossless.key());
        assert_ne!(original.key(), replaced.key());
    }

    #[test]
    fn parses_http_content_range() {
        assert_eq!(
            parse_content_range("bytes 1024-2047/4096"),
            Some(ContentRange {
                start: 1024,
                total: Some(4096),
            })
        );
        assert_eq!(
            parse_content_range("bytes 1024-2047/*"),
            Some(ContentRange {
                start: 1024,
                total: None,
            })
        );
        assert_eq!(parse_content_range("invalid"), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn partial_cache_is_played_locally_and_resumed_with_http_range() {
        let payload: Arc<Vec<u8>> = Arc::new(
            (0..2 * 1024 * 1024)
                .map(|index| (index % 251) as u8)
                .collect(),
        );
        let requested_ranges = Arc::new(Mutex::new(Vec::new()));
        let first_full_request = Arc::new(AtomicBool::new(true));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test HTTP server");
        let address = listener.local_addr().expect("test server address");
        let server = {
            let payload = payload.clone();
            let requested_ranges = requested_ranges.clone();
            tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    let payload = payload.clone();
                    let requested_ranges = requested_ranges.clone();
                    let first_full_request = first_full_request.clone();
                    tokio::spawn(async move {
                        serve_audio_request(socket, payload, requested_ranges, first_full_request)
                            .await;
                    });
                }
            })
        };

        let root = test_cache_dir();
        let cache = AudioCache::with_root(root.clone()).expect("create test audio cache");
        let track = track("song", "media", "Streaming test");
        let identity = CacheIdentity::new(&track, Quality::High);
        let paths = CachePaths::new(&root, &identity.key());
        let url = format!("http://{address}/audio");

        let first = cache
            .prepare(&url, &track, Quality::High)
            .await
            .expect("prepare first stream");
        tokio::task::spawn_blocking(move || {
            let mut source = first.source;
            let mut buffer = vec![0; 128 * 1024];
            source.read_exact(&mut buffer).expect("read cached prefix");
        })
        .await
        .expect("first reader task");
        tokio::time::sleep(Duration::from_millis(100)).await;

        let partial_length = file_length(&paths.media).await.expect("partial cache file");
        assert!(partial_length >= 1024 * 1024);
        assert!(partial_length < payload.len() as u64);

        let second = cache
            .prepare(&url, &track, Quality::High)
            .await
            .expect("prepare resumed stream");
        let resumed = tokio::task::spawn_blocking(move || {
            let mut source = second.source;
            let mut bytes = Vec::new();
            source.read_to_end(&mut bytes).expect("read resumed stream");
            bytes
        })
        .await
        .expect("resumed reader task");

        assert_eq!(resumed, payload.as_ref().as_slice());
        assert_eq!(
            requested_ranges
                .lock()
                .expect("requested range lock")
                .last()
                .copied(),
            Some(partial_length)
        );
        assert_eq!(
            fs::read(&paths.media).expect("completed cache file"),
            payload.as_ref().as_slice()
        );

        server.abort();
        let _ = fs::remove_dir_all(root);
    }

    fn test_cache_dir() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("lyrune-cache-test-{}-{nonce}", std::process::id()))
    }

    async fn serve_audio_request(
        mut socket: TcpStream,
        payload: Arc<Vec<u8>>,
        requested_ranges: Arc<Mutex<Vec<u64>>>,
        first_full_request: Arc<AtomicBool>,
    ) {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let Ok(read) = socket.read(&mut buffer).await else {
                return;
            };
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let request = String::from_utf8_lossy(&request);
        let is_head = request.starts_with("HEAD ");
        let range_start = request.lines().find_map(|line| {
            line.trim()
                .to_ascii_lowercase()
                .strip_prefix("range: bytes=")
                .and_then(|range| range.strip_suffix('-'))
                .and_then(|start| start.parse::<u64>().ok())
        });

        if is_head {
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"stream-v1\"\r\n\
                 Accept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            let _ = socket.write_all(headers.as_bytes()).await;
            return;
        }

        let start = range_start.unwrap_or_default() as usize;
        if let Some(start) = range_start {
            requested_ranges
                .lock()
                .expect("requested range lock")
                .push(start);
        }
        let status = if range_start.is_some() {
            "206 Partial Content"
        } else {
            "200 OK"
        };
        let mut headers = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: \"stream-v1\"\r\n\
             Accept-Ranges: bytes\r\n",
            payload.len() - start
        );
        if range_start.is_some() {
            headers.push_str(&format!(
                "Content-Range: bytes {start}-{}/{}\r\n",
                payload.len() - 1,
                payload.len()
            ));
        }
        headers.push_str("Connection: close\r\n\r\n");
        if socket.write_all(headers.as_bytes()).await.is_err() {
            return;
        }

        if range_start.is_none() && first_full_request.swap(false, Ordering::SeqCst) {
            let initial = 1100 * 1024;
            if socket.write_all(&payload[..initial]).await.is_err() {
                return;
            }
            let _ = socket.flush().await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            let _ = socket.write_all(&payload[initial..]).await;
        } else {
            let _ = socket.write_all(&payload[start..]).await;
        }
    }
}
