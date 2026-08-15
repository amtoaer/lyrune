use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use directories::ProjectDirs;
use futures_util::{AsyncReadExt as _, future::BoxFuture};
use gpui::http_client::{self, AsyncBody, HttpClient, Request, Response, Url};
use gpui::{App, Asset, ImageCacheError, ImageSource, ImgResourceLoader, Resource, Window};

use crate::app::RUNTIME;
use crate::cache::cache_key;

const IMAGE_CACHE_DIR: &str = "images-v1";

#[derive(Clone)]
enum CachedImageFile {}

impl Asset for CachedImageFile {
    type Source = String;
    type Output = Result<Arc<Path>, ImageCacheError>;

    fn load(
        url: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let client = cx.http_client();
        async move {
            RUNTIME
                .spawn(cache_image_file(client, url))
                .await
                .context("等待图片缓存任务失败")?
                .map(Arc::from)
                .map_err(ImageCacheError::from)
        }
    }
}

pub fn cached_image_source(url: String) -> ImageSource {
    let source = url;
    (move |window: &mut Window, cx: &mut App| {
        let path = match window.use_asset::<CachedImageFile>(&source, cx)? {
            Ok(path) => path,
            Err(error) => return Some(Err(error)),
        };
        window.use_asset::<ImgResourceLoader>(&Resource::Path(path), cx)
    })
    .into()
}

async fn cache_image_file(client: Arc<dyn HttpClient>, url: String) -> anyhow::Result<PathBuf> {
    let root = ProjectDirs::from("dev", "lyrune", "Lyrune")
        .context("无法确定 Lyrune 图片缓存目录")?
        .cache_dir()
        .join(IMAGE_CACHE_DIR);
    cache_image_file_at(&root, client, url).await
}

async fn cache_image_file_at(
    root: &Path,
    client: Arc<dyn HttpClient>,
    url: String,
) -> anyhow::Result<PathBuf> {
    let path = root.join(image_cache_key(&url));
    if is_nonempty_file(&path).await {
        return Ok(path);
    }

    tokio::fs::create_dir_all(&root)
        .await
        .context("无法创建图片缓存目录")?;
    if is_nonempty_file(&path).await {
        return Ok(path);
    }

    let mut response = client
        .get(&url, AsyncBody::default(), true)
        .await
        .with_context(|| format!("下载图片失败：{url}"))?;
    if !response.status().is_success() {
        bail!("下载图片失败：{url} 返回 {}", response.status());
    }

    let mut bytes = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("读取图片响应失败：{url}"))?;
    if bytes.is_empty() {
        bail!("下载图片失败：{url} 返回空文件");
    }

    let temporary = root.join(format!(
        ".{}.{}.tmp",
        image_cache_key(&url),
        std::process::id()
    ));
    tokio::fs::write(&temporary, bytes)
        .await
        .context("无法写入图片缓存临时文件")?;
    if let Err(error) = tokio::fs::rename(&temporary, &path).await {
        if is_nonempty_file(&path).await {
            let _ = tokio::fs::remove_file(&temporary).await;
        } else {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error).context("无法提交图片缓存文件");
        }
    }
    Ok(path)
}

async fn is_nonempty_file(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

fn image_cache_key(url: &str) -> String {
    cache_key(url.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct MockHttpClient {
        calls: Arc<AtomicUsize>,
        body: Vec<u8>,
    }

    impl HttpClient for MockHttpClient {
        fn user_agent(&self) -> Option<&http_client::http::HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            _request: Request<AsyncBody>,
        ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let body = self.body.clone();
            Box::pin(async move {
                Ok(Response::builder()
                    .status(200)
                    .body(AsyncBody::from(body))?)
            })
        }
    }

    #[tokio::test]
    async fn reuses_cached_image_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lyrune-image-cache-{}-{unique}",
            std::process::id()
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let client: Arc<dyn HttpClient> = Arc::new(MockHttpClient {
            calls: calls.clone(),
            body: b"image bytes".to_vec(),
        });
        let url = "https://example.com/cover.jpg".to_owned();

        let first = cache_image_file_at(&root, client.clone(), url.clone())
            .await
            .unwrap();
        let second = cache_image_file_at(&root, client, url).await.unwrap();

        assert_eq!(first, second);
        assert_eq!(tokio::fs::read(first).await.unwrap(), b"image bytes");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(image_cache_key("https://example.com/cover.jpg").len(), 32);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}

pub struct QqImageHttpClient {
    client: reqwest::Client,
    user_agent: http_client::http::HeaderValue,
}

impl QqImageHttpClient {
    pub fn new() -> anyhow::Result<Self> {
        let user_agent = http_client::http::HeaderValue::from_static("Lyrune/0.1");
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::REFERER,
            reqwest::header::HeaderValue::from_static("https://y.qq.com/"),
        );
        let client = reqwest::Client::builder()
            .user_agent(user_agent.clone())
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { client, user_agent })
    }
}

impl HttpClient for QqImageHttpClient {
    fn user_agent(&self) -> Option<&http_client::http::HeaderValue> {
        Some(&self.user_agent)
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let client = self.client.clone();
        let request = RUNTIME.spawn(async move {
            let (parts, mut body) = request.into_parts();
            let mut body_bytes = Vec::new();
            body.read_to_end(&mut body_bytes).await?;

            let mut request = client
                .request(parts.method, parts.uri.to_string())
                .headers(parts.headers);
            if !body_bytes.is_empty() {
                request = request.body(body_bytes);
            }

            let response = request.send().await?;
            let status = response.status();
            let headers = response.headers().clone();
            let bytes = response.bytes().await?;
            let mut response = Response::builder().status(status);
            *response
                .headers_mut()
                .expect("response builder accepts headers before body") = headers;
            anyhow::Ok(response.body(AsyncBody::from(bytes))?)
        });
        Box::pin(async move { request.await? })
    }
}

pub fn client() -> anyhow::Result<Arc<dyn HttpClient>> {
    Ok(Arc::new(QqImageHttpClient::new()?))
}
