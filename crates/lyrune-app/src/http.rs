use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use directories::ProjectDirs;
use futures_util::{AsyncReadExt as _, FutureExt as _, future::BoxFuture};
use gpui::http_client::{self, AsyncBody, HttpClient, Request, Response, Url};
use gpui::{
    App, AppContext as _, Asset, Entity, Image, ImageCache, ImageCacheError, ImageCacheItem,
    ImageFormat, ImageSource, ImgResourceLoader, RenderImage, Resource, Window, hash,
};
use image::imageops::FilterType;

use crate::app::RUNTIME;
use crate::cache::cache_key;

const IMAGE_CACHE_DIR: &str = "images-v1";
const DECODED_IMAGE_CACHE_CAPACITY: usize = 48;
const MAX_SATURATION_COMPRESSION: f32 = 0.35;

#[derive(Clone)]
enum CachedImageFile {}

#[derive(Clone)]
enum BlurredCoverImage {}

pub struct CachedImageCache {
    usages: Vec<u64>,
    items: HashMap<u64, ImageCacheItem>,
}

pub struct BlurredCover {
    image: Arc<Image>,
    wide_lyrics_rgb: [f32; 3],
    narrow_lyrics_rgb: [f32; 3],
}

impl BlurredCover {
    pub fn sampled_rgb(&self, narrow: bool) -> [f32; 3] {
        if narrow {
            self.narrow_lyrics_rgb
        } else {
            self.wide_lyrics_rgb
        }
    }
}

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

impl CachedImageCache {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let cache = cx.new(|_| Self {
            usages: Vec::with_capacity(DECODED_IMAGE_CACHE_CAPACITY),
            items: HashMap::with_capacity(DECODED_IMAGE_CACHE_CAPACITY),
        });
        cx.observe_release(&cache, |cache, cx| {
            for (_, mut item) in std::mem::take(&mut cache.items) {
                if let Some(Ok(image)) = item.get() {
                    cx.drop_image(image, None);
                }
            }
        })
        .detach();
        cache
    }
}

impl ImageCache for CachedImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = hash(resource);
        if let Some(item) = self.items.get_mut(&key) {
            let index = self
                .usages
                .iter()
                .position(|cached| *cached == key)
                .expect("图片缓存条目与使用顺序应保持一致");
            self.usages.remove(index);
            self.usages.insert(0, key);
            return item.get();
        }

        let source = match resource {
            Resource::Uri(uri) => {
                let path = match window.use_asset::<CachedImageFile>(&uri.to_string(), cx)? {
                    Ok(path) => path,
                    Err(error) => return Some(Err(error)),
                };
                Resource::Path(path)
            }
            _ => resource.clone(),
        };
        let future = ImgResourceLoader::load(source, cx);
        let task = cx.background_executor().spawn(future).shared();

        if self.usages.len() == DECODED_IMAGE_CACHE_CAPACITY {
            let oldest = self.usages.pop().expect("已满图片缓存应包含最旧条目");
            let mut item = self
                .items
                .remove(&oldest)
                .expect("图片缓存条目与使用顺序应保持一致");
            if let Some(Ok(image)) = item.get() {
                cx.drop_image(image, Some(window));
            }
        }
        self.items
            .insert(key, ImageCacheItem::Loading(task.clone()));
        self.usages.insert(0, key);

        let entity = window.current_view();
        window
            .spawn(cx, async move |cx| {
                _ = task.await;
                cx.on_next_frame(move |_, cx| cx.notify(entity));
            })
            .detach();
        None
    }
}

impl Asset for BlurredCoverImage {
    type Source = String;
    type Output = Result<Arc<BlurredCover>, ImageCacheError>;

    fn load(
        path: Self::Source,
        _cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        async move {
            RUNTIME
                .spawn_blocking(move || generate_blurred_cover(Path::new(&path)))
                .await
                .context("等待模糊封面生成任务失败")?
                .map_err(ImageCacheError::from)
        }
    }
}

pub fn cached_image_source(url: String) -> ImageSource {
    url.into()
}

pub fn blurred_image_source(url: String) -> ImageSource {
    let source = url;
    (move |window: &mut Window, cx: &mut App| {
        let cover = match blurred_cover(&source, window, cx)? {
            Ok(cover) => cover,
            Err(error) => return Some(Err(error)),
        };
        cover.image.clone().use_render_image(window, cx).map(Ok)
    })
    .into()
}

pub fn blurred_cover(
    url: &str,
    window: &mut Window,
    cx: &mut App,
) -> Option<Result<Arc<BlurredCover>, ImageCacheError>> {
    let path = match window.use_asset::<CachedImageFile>(&url.to_owned(), cx)? {
        Ok(path) => path,
        Err(error) => return Some(Err(error)),
    };
    window.use_asset::<BlurredCoverImage>(&path.to_string_lossy().into_owned(), cx)
}

fn generate_blurred_cover(path: &Path) -> anyhow::Result<Arc<BlurredCover>> {
    let image = image::ImageReader::open(path)
        .context("无法打开待模糊的封面")?
        .with_guessed_format()
        .context("无法识别封面格式")?
        .decode()
        .context("无法解码待模糊的封面")?;
    let blurred = compress_high_saturation(
        image
            .resize_to_fill(128, 128, FilterType::Triangle)
            .blur(10.),
    );
    let wide_lyrics_rgb = sample_lyrics_region(&blurred, false);
    let narrow_lyrics_rgb = sample_lyrics_region(&blurred, true);
    let mut bytes = Cursor::new(Vec::new());
    blurred
        .write_to(&mut bytes, image::ImageFormat::Png)
        .context("无法编码模糊封面")?;
    Ok(Arc::new(BlurredCover {
        image: Arc::new(Image::from_bytes(ImageFormat::Png, bytes.into_inner())),
        wide_lyrics_rgb,
        narrow_lyrics_rgb,
    }))
}

fn compress_high_saturation(image: image::DynamicImage) -> image::DynamicImage {
    let mut image = image.to_rgba8();
    for pixel in image.pixels_mut() {
        let rgb = [
            f32::from(pixel[0]) / 255.,
            f32::from(pixel[1]) / 255.,
            f32::from(pixel[2]) / 255.,
        ];
        let maximum = rgb.into_iter().fold(0_f32, f32::max);
        let minimum = rgb.into_iter().fold(1_f32, f32::min);
        let lightness = (maximum + minimum) / 2.;
        let denominator = 1. - (2. * lightness - 1.).abs();
        if denominator <= f32::EPSILON {
            continue;
        }

        let saturation = (maximum - minimum) / denominator;
        let scale = 1. - MAX_SATURATION_COMPRESSION * saturation;
        for channel in 0..3 {
            pixel[channel] = ((lightness + (rgb[channel] - lightness) * scale) * 255.)
                .round()
                .clamp(0., 255.) as u8;
        }
    }
    image::DynamicImage::ImageRgba8(image)
}

fn sample_lyrics_region(image: &image::DynamicImage, narrow: bool) -> [f32; 3] {
    let image = image.to_rgb8();
    let (width, height) = image.dimensions();
    let (start_x, end_x) = if narrow {
        (width / 10, width * 4 / 5)
    } else {
        (width * 2 / 5, width * 4 / 5)
    };
    let mut totals = [0_u64; 3];
    let mut count = 0_u64;
    for pixel in image
        .rows()
        .skip((height / 4) as usize)
        .take((height / 2) as usize)
    {
        for color in pixel
            .skip(start_x as usize)
            .take(end_x.saturating_sub(start_x) as usize)
        {
            totals[0] += u64::from(color[0]);
            totals[1] += u64::from(color[1]);
            totals[2] += u64::from(color[2]);
            count += 1;
        }
    }
    if count == 0 {
        return [0.; 3];
    }
    [
        totals[0] as f32 / count as f32 / 255.,
        totals[1] as f32 / count as f32 / 255.,
        totals[2] as f32 / count as f32 / 255.,
    ]
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

    #[test]
    fn creates_small_blurred_cover_in_memory() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lyrune-blurred-cover-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("cover.png");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([120, 80, 200, 255]))
            .save(&source)
            .unwrap();

        let blurred = generate_blurred_cover(&source).unwrap();
        let decoded = image::load_from_memory(&blurred.image.bytes).unwrap();

        assert_eq!(blurred.image.format, ImageFormat::Png);
        assert_eq!((decoded.width(), decoded.height()), (128, 128));
        let sampled_rgb = blurred.sampled_rgb(false);
        assert!(sampled_rgb[0] > 120. / 255.);
        assert!(sampled_rgb[1] > 80. / 255.);
        assert!(sampled_rgb[2] < 200. / 255.);
        let compressed_lightness = (sampled_rgb[1] + sampled_rgb[2]) / 2.;
        assert!((compressed_lightness - 140. / 255.).abs() < 0.01);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn samples_the_area_occupied_by_wide_lyrics() {
        let mut image = image::RgbImage::from_pixel(100, 100, image::Rgb([32, 32, 32]));
        for y in 0..100 {
            for x in 80..100 {
                image.put_pixel(x, y, image::Rgb([240, 240, 240]));
            }
        }

        let sampled = sample_lyrics_region(&image::DynamicImage::ImageRgb8(image), false);

        assert_eq!(sampled, [32. / 255.; 3]);
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
