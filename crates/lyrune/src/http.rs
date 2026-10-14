use std::io::Cursor;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use directories::ProjectDirs;
use futures_util::{AsyncReadExt as _, future::BoxFuture};
use gpui::http_client::{self, AsyncBody, HttpClient, Request, Response, Url};
use gpui::{
    App, AppContext as _, Asset, Entity, Image, ImageCache, ImageCacheError, ImageCacheItem,
    ImageFormat, ImageSource, Pixels, RenderImage, Resource, Window, hash,
};
use image::imageops::FilterType;
use lru::LruCache;

use crate::cache::cache_key;
use crate::runtime::RUNTIME;

const IMAGE_CACHE_DIR: &str = "images-v1";
const THUMBNAIL_CACHE_DIR: &str = "thumbnails-v1";
const MAX_SATURATION_COMPRESSION: f32 = 0.35;
pub(crate) const BLURRED_COVER_SIZE: u32 = 128;

#[derive(Clone)]
enum CachedImageFile {}

#[derive(Clone)]
enum CachedThumbnailFile {}

#[derive(Clone)]
enum BlurredCoverImage {}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CachedImageSize {
    Px64,
    Px128,
    Px256,
    Px512,
}

impl CachedImageSize {
    const ALL: [Self; 4] = [Self::Px64, Self::Px128, Self::Px256, Self::Px512];

    fn pixels(self) -> u32 {
        match self {
            Self::Px64 => 64,
            Self::Px128 => 128,
            Self::Px256 => 256,
            Self::Px512 => 512,
        }
    }

    fn for_display_size(size: Pixels, scale_factor: f32) -> Option<Self> {
        let physical_size = (f32::from(size) * scale_factor).ceil();
        if physical_size <= 64. {
            Some(Self::Px64)
        } else if physical_size <= 128. {
            Some(Self::Px128)
        } else if physical_size <= 256. {
            Some(Self::Px256)
        } else if physical_size <= 512. {
            Some(Self::Px512)
        } else {
            None
        }
    }

    fn resource_suffix(self) -> &'static str {
        match self {
            Self::Px64 => "#lyrune-thumbnail-v1-64",
            Self::Px128 => "#lyrune-thumbnail-v1-128",
            Self::Px256 => "#lyrune-thumbnail-v1-256",
            Self::Px512 => "#lyrune-thumbnail-v1-512",
        }
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct ThumbnailSource {
    original: Arc<Path>,
    cache_key: String,
    size: CachedImageSize,
}

pub struct CachedImageCache {
    items: LruCache<u64, Entity<ImageCacheItem>>,
}

pub struct BlurredCover {
    image: Arc<Image>,
    /// 各 RGB 通道的最小值、最大值；边界颜色不一定对应同一个像素。
    pub rgb_bounds: [[f32; 3]; 2],
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

#[allow(clippy::manual_async_fn)]
impl Asset for CachedThumbnailFile {
    type Source = ThumbnailSource;
    type Output = Result<Arc<Path>, ImageCacheError>;

    fn load(
        source: Self::Source,
        _cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        async move {
            RUNTIME
                .spawn_blocking(move || cache_thumbnail_file(source))
                .await
                .context("等待缩略图缓存任务失败")?
                .map(Arc::from)
                .map_err(ImageCacheError::from)
        }
    }
}

impl CachedImageCache {
    pub fn new(capacity: usize, cx: &mut App) -> Entity<Self> {
        let capacity = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN);
        cx.new(|_| Self {
            items: LruCache::new(capacity),
        })
    }

    pub fn set_capacity(&mut self, capacity: usize) {
        let capacity = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN);
        self.items.resize(capacity);
    }

    fn load_item(
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Entity<ImageCacheItem>, ImageCacheError>> {
        let source = match resource {
            Resource::Uri(uri) => {
                let (url, thumbnail_size) = split_thumbnail_resource(&uri.to_string());
                let path = match window.use_asset::<CachedImageFile>(&url, cx)? {
                    Ok(path) => path,
                    Err(error) => return Some(Err(error)),
                };
                let path = if let Some(size) = thumbnail_size {
                    let source = ThumbnailSource {
                        original: path.clone(),
                        cache_key: image_cache_key(&url),
                        size,
                    };
                    match window.use_asset::<CachedThumbnailFile>(&source, cx)? {
                        Ok(thumbnail) => thumbnail,
                        Err(_) => path,
                    }
                } else {
                    path
                };
                Resource::Path(path)
            }
            _ => resource.clone(),
        };
        let item = cx.new(|cx| ImageCacheItem::new(&source, cx));
        cx.observe_release(&item, |item, cx| {
            if let Some(Ok(image)) = item.get() {
                cx.drop_image(image, None);
            }
        })
        .detach();
        Some(Ok(item))
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
        // cached 视图重放绘制指令时不访问 LRU，但会保留 element state。
        // 让视图与 LRU 共同持有图片，最后一个引用释放后才能回收 GPU 区域。
        window.with_global_id(("cached-image", key).into(), |id, window| {
            window.with_element_state(
                id,
                |retained: Option<Option<Entity<ImageCacheItem>>>, window| {
                    let item = self.items.get(&key).cloned().or_else(|| retained.flatten());
                    let item = match item
                        .map(Ok)
                        .or_else(|| Self::load_item(resource, window, cx))
                    {
                        Some(Ok(item)) => item,
                        Some(Err(error)) => return (Some(Err(error)), None),
                        None => return (None, None),
                    };
                    if !self.items.contains(&key) {
                        self.items.put(key, item.clone());
                    }
                    let image = item.read(cx).use_image(window);
                    (image, Some(item))
                },
            )
        })
    }
}

#[allow(clippy::manual_async_fn)]
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

pub fn cached_image_source(url: String, size: Pixels, scale_factor: f32) -> ImageSource {
    match CachedImageSize::for_display_size(size, scale_factor) {
        Some(size) => ImageSource::Resource(Resource::Uri(
            format!("{url}{}", size.resource_suffix()).into(),
        )),
        None => url.into(),
    }
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
            .resize_to_fill(BLURRED_COVER_SIZE, BLURRED_COVER_SIZE, FilterType::Triangle)
            .blur(10.),
    );
    // 各通道的上下界同时覆盖局部亮斑和窗口裁剪变化，不依赖歌词当前所在的位置。
    let mut rgb_bounds = [[1_f32; 3], [0_f32; 3]];
    for pixel in blurred.to_rgb8().pixels() {
        for (channel, value) in pixel.0.into_iter().enumerate() {
            let value = f32::from(value) / 255.;
            rgb_bounds[0][channel] = rgb_bounds[0][channel].min(value);
            rgb_bounds[1][channel] = rgb_bounds[1][channel].max(value);
        }
    }
    let mut bytes = Cursor::new(Vec::new());
    blurred
        .write_to(&mut bytes, image::ImageFormat::Png)
        .context("无法编码模糊封面")?;
    Ok(Arc::new(BlurredCover {
        image: Arc::new(Image::from_bytes(ImageFormat::Png, bytes.into_inner())),
        rgb_bounds,
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

async fn cache_image_file(client: Arc<dyn HttpClient>, url: String) -> anyhow::Result<PathBuf> {
    let root = image_cache_root()?;
    cache_image_file_at(&root, client, url).await
}

fn image_cache_root() -> anyhow::Result<PathBuf> {
    Ok(ProjectDirs::from("dev", "lyrune", "Lyrune")
        .context("无法确定 Lyrune 图片缓存目录")?
        .cache_dir()
        .join(IMAGE_CACHE_DIR))
}

fn cache_thumbnail_file(source: ThumbnailSource) -> anyhow::Result<PathBuf> {
    let root = image_cache_root()?;
    cache_thumbnail_file_at(&root, source)
}

fn cache_thumbnail_file_at(root: &Path, source: ThumbnailSource) -> anyhow::Result<PathBuf> {
    let target_size = source.size.pixels();
    let root = root.join(THUMBNAIL_CACHE_DIR).join(target_size.to_string());
    let path = root.join(&source.cache_key);
    if is_nonempty_file_sync(&path) {
        return Ok(path);
    }

    let (width, height) = image::ImageReader::open(source.original.as_ref())
        .context("无法打开待缩放图片")?
        .with_guessed_format()
        .context("无法识别待缩放图片格式")?
        .into_dimensions()
        .context("无法读取待缩放图片的尺寸")?;
    if width <= target_size && height <= target_size {
        return Ok(source.original.to_path_buf());
    }

    std::fs::create_dir_all(&root).context("无法创建缩略图缓存目录")?;
    if is_nonempty_file_sync(&path) {
        return Ok(path);
    }

    let image = image::ImageReader::open(source.original.as_ref())
        .context("无法打开待缩放图片")?
        .with_guessed_format()
        .context("无法识别待缩放图片格式")?
        .decode()
        .context("无法解码待缩放图片")?
        .resize(target_size, target_size, FilterType::Triangle);
    let temporary = root.join(format!(".{}.{}.tmp", source.cache_key, std::process::id()));
    image
        .save_with_format(&temporary, image::ImageFormat::Png)
        .context("无法写入缩略图缓存临时文件")?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        if is_nonempty_file_sync(&path) {
            let _ = std::fs::remove_file(&temporary);
        } else {
            let _ = std::fs::remove_file(&temporary);
            return Err(error).context("无法提交缩略图缓存文件");
        }
    }
    Ok(path)
}

fn is_nonempty_file_sync(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

fn split_thumbnail_resource(resource: &str) -> (String, Option<CachedImageSize>) {
    for size in CachedImageSize::ALL {
        if let Some(url) = resource.strip_suffix(size.resource_suffix()) {
            return (url.to_owned(), Some(size));
        }
    }
    (resource.to_owned(), None)
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use gpui::{
        Context, IntoElement, Render, StyleRefinement, TestAppContext, div, img, prelude::*, px,
    };

    use super::*;

    struct CachedCover {
        source: Resource,
        renders: usize,
    }

    impl Render for CachedCover {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.renders += 1;
            img(ImageSource::Resource(self.source.clone())).size(px(64.))
        }
    }

    struct ImageCacheTestView {
        cache: Entity<CachedImageCache>,
        sidebar: Entity<CachedCover>,
        show_sidebar: bool,
        cover: Resource,
    }

    impl Render for ImageCacheTestView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .image_cache(self.cache.clone())
                .flex()
                .when(self.show_sidebar, |view| {
                    view.child(
                        self.sidebar
                            .clone()
                            .cached(StyleRefinement::default().size(px(64.))),
                    )
                })
                .child(img(ImageSource::Resource(self.cover.clone())).size(px(64.)))
        }
    }

    #[gpui::test]
    fn test_cached_cover_survives_eviction(cx: &mut TestAppContext) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lyrune-image-lifetime-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let sources = (0..6)
            .map(|index| {
                let path = root.join(format!("{index}.png"));
                image::RgbaImage::from_pixel(64, 64, image::Rgba([index * 40, 80, 200, 255]))
                    .save(&path)
                    .unwrap();
                Resource::from(path)
            })
            .collect::<Vec<_>>();
        let cache = cx.update(|cx| CachedImageCache::new(2, cx));
        let sidebar = cx.new(|_| CachedCover {
            source: sources[0].clone(),
            renders: 0,
        });
        let (view, cx) = cx.add_window_view(|_, _| ImageCacheTestView {
            cache: cache.clone(),
            sidebar: sidebar.clone(),
            show_sidebar: true,
            cover: sources[1].clone(),
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let key = hash(&sources[0]);
        let image = cache.read_with(cx, |cache, cx| {
            cache
                .items
                .peek(&key)
                .unwrap()
                .read(cx)
                .get()
                .unwrap()
                .unwrap()
        });
        let renders = sidebar.read_with(cx, |view, _| view.renders);
        assert!(cx.update(|window, _| window.has_image_atlas_entry(&image)));

        for source in &sources[2..] {
            view.update(cx, |view, cx| {
                view.cover = source.clone();
                cx.notify();
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        assert_eq!(sidebar.read_with(cx, |view, _| view.renders), renders);
        assert!(!cache.read_with(cx, |cache, _| cache.items.contains(&key)));
        assert!(
            cx.update(|window, _| window.has_image_atlas_entry(&image)),
            "cached view still uses the evicted image"
        );

        cache.update(cx, |cache, _| cache.set_capacity(1));
        sidebar.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.update(|window, _| window.has_image_atlas_entry(&image)));

        view.update(cx, |view, cx| {
            view.show_sidebar = false;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
        assert!(!cx.update(|window, _| window.has_image_atlas_entry(&image)));
        std::fs::remove_dir_all(root).unwrap();
    }

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
    fn encodes_thumbnail_size_in_the_resource() {
        let url = "https://example.com/cover.jpg?quality=100";

        for size in CachedImageSize::ALL {
            let resource = format!("{url}{}", size.resource_suffix());
            assert_eq!(
                split_thumbnail_resource(&resource),
                (url.to_owned(), Some(size))
            );
        }
        assert_eq!(split_thumbnail_resource(url), (url.to_owned(), None));
    }

    #[test]
    fn selects_thumbnail_size_from_display_size_and_scale_factor() {
        assert_eq!(
            CachedImageSize::for_display_size(gpui::px(44.), 1.),
            Some(CachedImageSize::Px64)
        );
        assert_eq!(
            CachedImageSize::for_display_size(gpui::px(44.), 2.),
            Some(CachedImageSize::Px128)
        );
        assert_eq!(
            CachedImageSize::for_display_size(gpui::px(176.), 1.5),
            Some(CachedImageSize::Px512)
        );
        assert_eq!(CachedImageSize::for_display_size(gpui::px(520.), 1.), None);
    }

    #[test]
    fn creates_and_reuses_resized_thumbnail_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "lyrune-thumbnail-cache-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("original");
        image::RgbImage::from_pixel(320, 160, image::Rgb([120, 80, 200]))
            .save_with_format(&original, image::ImageFormat::Jpeg)
            .unwrap();
        let source = ThumbnailSource {
            original: Arc::from(original.as_path()),
            cache_key: "cover".to_owned(),
            size: CachedImageSize::Px64,
        };

        let thumbnail = cache_thumbnail_file_at(&root, source.clone()).unwrap();
        let decoded = image::ImageReader::open(&thumbnail)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!((decoded.width(), decoded.height()), (64, 32));
        assert_eq!(thumbnail.file_name().unwrap(), "cover");

        std::fs::remove_file(original).unwrap();
        assert_eq!(cache_thumbnail_file_at(&root, source).unwrap(), thumbnail);
        std::fs::remove_dir_all(root).unwrap();
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
        let sampled_rgb = blurred.rgb_bounds[0];
        assert_eq!(blurred.rgb_bounds[0], blurred.rgb_bounds[1]);
        assert!(sampled_rgb[0] > 120. / 255.);
        assert!(sampled_rgb[1] > 80. / 255.);
        assert!(sampled_rgb[2] < 200. / 255.);
        let compressed_lightness = (sampled_rgb[1] + sampled_rgb[2]) / 2.;
        assert!((compressed_lightness - 140. / 255.).abs() < 0.01);

        image::RgbImage::from_fn(128, 128, |x, _| {
            image::Rgb([if x < 64 { 0 } else { 255 }; 3])
        })
        .save(&source)
        .unwrap();
        let blurred = generate_blurred_cover(&source).unwrap();
        assert!(blurred.rgb_bounds[0].into_iter().all(|value| value < 0.1));
        assert!(blurred.rgb_bounds[1].into_iter().all(|value| value > 0.9));
        std::fs::remove_dir_all(root).unwrap();
    }
}
