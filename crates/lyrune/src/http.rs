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
    App, AppContext as _, Asset, Entity, Hsla, Image, ImageCache, ImageCacheError, ImageCacheItem,
    ImageFormat, ImageSource, Pixels, RenderImage, Resource, Rgba, Window, hash,
};
use image::imageops::FilterType;
use lru::LruCache;
use palette::{
    FromColor, IsWithinBounds, LinSrgb, Oklab, Oklch, Srgb, convert::FromColorUnclamped,
};

use crate::cache::cache_key;
use crate::runtime::RUNTIME;

const IMAGE_CACHE_DIR: &str = "images-v1";
const THUMBNAIL_CACHE_DIR: &str = "thumbnails-v1";
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

#[derive(Clone, Eq, Hash, PartialEq)]
struct BlurredCoverSource {
    path: Arc<Path>,
    background: Hsla,
    foreground: Hsla,
    overlay_percent: u32,
    contrast_bits: u32,
}

pub struct CachedImageCache {
    items: LruCache<u64, Entity<ImageCacheItem>>,
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
    type Source = BlurredCoverSource;
    type Output = Result<Arc<Image>, ImageCacheError>;

    fn load(
        source: Self::Source,
        _cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        async move {
            RUNTIME
                .spawn_blocking(move || {
                    generate_blurred_cover(
                        &source.path,
                        source.background,
                        source.foreground,
                        source.overlay_percent,
                        f32::from_bits(source.contrast_bits),
                    )
                })
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

pub fn blurred_image_source(
    url: String,
    background: Hsla,
    foreground: Hsla,
    overlay_percent: u32,
    contrast: f32,
) -> ImageSource {
    let source = url;
    (move |window: &mut Window, cx: &mut App| {
        let cover = match blurred_cover(
            &source,
            background,
            foreground,
            overlay_percent,
            contrast,
            window,
            cx,
        )? {
            Ok(cover) => cover,
            Err(error) => return Some(Err(error)),
        };
        cover.use_render_image(window, cx).map(Ok)
    })
    .into()
}

fn blurred_cover(
    url: &str,
    background: Hsla,
    foreground: Hsla,
    overlay_percent: u32,
    contrast: f32,
    window: &mut Window,
    cx: &mut App,
) -> Option<Result<Arc<Image>, ImageCacheError>> {
    let path = match window.use_asset::<CachedImageFile>(&url.to_owned(), cx)? {
        Ok(path) => path,
        Err(error) => return Some(Err(error)),
    };
    window.use_asset::<BlurredCoverImage>(
        &BlurredCoverSource {
            path,
            background,
            foreground,
            overlay_percent,
            contrast_bits: contrast.to_bits(),
        },
        cx,
    )
}

fn generate_blurred_cover(
    path: &Path,
    background: Hsla,
    foreground: Hsla,
    overlay_percent: u32,
    contrast: f32,
) -> anyhow::Result<Arc<Image>> {
    let image = image::ImageReader::open(path)
        .context("无法打开待模糊的封面")?
        .with_guessed_format()
        .context("无法识别封面格式")?
        .decode()
        .context("无法解码待模糊的封面")?;
    let blurred = image
        .resize_to_fill(BLURRED_COVER_SIZE, BLURRED_COVER_SIZE, FilterType::Triangle)
        .blur(10.);
    let blurred = tone_blurred_cover(blurred, background, foreground, overlay_percent, contrast);
    let mut bytes = Cursor::new(Vec::new());
    blurred
        .write_to(&mut bytes, image::ImageFormat::Png)
        .context("无法编码模糊封面")?;
    Ok(Arc::new(Image::from_bytes(
        ImageFormat::Png,
        bytes.into_inner(),
    )))
}

fn tone_blurred_cover(
    image: image::DynamicImage,
    background: Hsla,
    foreground: Hsla,
    overlay_percent: u32,
    contrast: f32,
) -> image::RgbImage {
    let overlay = background.opacity(overlay_percent as f32 / 100.).to_rgb();
    let background = background.to_rgb();
    let foreground_luminance = relative_luminance(foreground.to_rgb());
    let light_foreground = foreground_luminance > relative_luminance(background);
    // 为纹理采样、切歌合成的舍入留少量余量；最终仍在 sRGB 中检查对比度。
    let contrast = contrast + 0.1;
    let luminance_limit = if light_foreground {
        (foreground_luminance + 0.05) / contrast - 0.05
    } else {
        (foreground_luminance + 0.05) * contrast - 0.05
    };
    let colors: Vec<Oklch> = image
        .to_rgba8()
        .pixels()
        .map(|pixel| {
            let alpha = f32::from(pixel[3]) / 255.;
            Oklch::from_color(Srgb::new(
                f32::from(pixel[0]) / 255. * alpha + background.r * (1. - alpha),
                f32::from(pixel[1]) / 255. * alpha + background.g * (1. - alpha),
                f32::from(pixel[2]) / 255. * alpha + background.b * (1. - alpha),
            ))
        })
        .collect();
    // 各通道的上下界同时覆盖局部亮斑和窗口裁剪变化，不依赖歌词当前所在的位置。
    let render = |amount: f32| {
        let mut bounds = [[1_f32; 3], [0_f32; 3]];
        let mut result = image::RgbImage::new(image.width(), image.height());
        for (pixel, color) in result.pixels_mut().zip(&colors) {
            let mut color = *color;
            // 整张图采用同一明度变换，避免逐像素截断造成大块平坦色阶。
            color.l = if light_foreground {
                color.l * (1. - amount)
            } else {
                color.l + (1. - color.l) * amount
            };
            let rgb = oklch_to_srgb(color);
            pixel.0 = [rgb.red, rgb.green, rgb.blue];
            for (channel, value) in pixel.0.into_iter().enumerate() {
                let value = f32::from(value) / 255.;
                bounds[0][channel] = bounds[0][channel].min(value);
                bounds[1][channel] = bounds[1][channel].max(value);
            }
        }
        let [r, g, b] = bounds[usize::from(light_foreground)];
        // 对比度按固定遮罩合成后的颜色计算，避免调色和遮罩重复压暗。
        let mut displayed = Rgba { r, g, b, a: 1. }.blend(overlay);
        if !light_foreground {
            // sRGB 混色不是线性亮度混色。用加权灰阶的亮度下界约束浅色背景，
            // 使不同色相的封面在交叉渐变时仍有足够亮度承托深色文字。
            let gray = 0.2126 * displayed.r + 0.7152 * displayed.g + 0.0722 * displayed.b;
            displayed = Rgba {
                r: gray,
                g: gray,
                b: gray,
                a: 1.,
            };
        }
        let luminance = relative_luminance(displayed);
        let readable = if light_foreground {
            luminance <= luminance_limit
        } else {
            luminance >= luminance_limit
        };
        (result, readable)
    };
    let (image, readable) = render(0.);
    if readable {
        return image;
    }
    let mut result = render(1.);
    let mut lower = 0.;
    let mut upper = 1.;
    for _ in 0..12 {
        let amount = (lower + upper) / 2.;
        let candidate = render(amount);
        if candidate.1 {
            upper = amount;
            result = candidate;
        } else {
            lower = amount;
        }
    }
    result.0
}

fn oklch_to_srgb(color: Oklch) -> Srgb<u8> {
    let color = Oklab::from_color(color);
    let mut rgb = LinSrgb::from_color_unclamped(color);
    if !rgb.is_within_bounds() {
        // 沿同一色相降低彩度进入 sRGB 色域，直接截断 RGB 会改变色相。
        let mut lower = 0.;
        let mut upper = 1.;
        rgb = LinSrgb::from_color_unclamped(Oklab::new(color.l, 0., 0.));
        for _ in 0..12 {
            let chroma = (lower + upper) / 2.;
            let candidate = LinSrgb::from_color_unclamped(Oklab::new(
                color.l,
                color.a * chroma,
                color.b * chroma,
            ));
            if candidate.is_within_bounds() {
                lower = chroma;
                rgb = candidate;
            } else {
                upper = chroma;
            }
        }
    }
    Srgb::from_linear(rgb)
}

fn relative_luminance(color: Rgba) -> f32 {
    let linearize = |channel: f32| {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linearize(color.r) + 0.7152 * linearize(color.g) + 0.0722 * linearize(color.b)
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
    use crate::settings::{
        DEFAULT_LYRIC_BACKGROUND_CONTRAST, DEFAULT_LYRIC_BACKGROUND_OVERLAY_PERCENT,
    };

    const LYRIC_BACKGROUND_OVERLAY_OPACITY: f32 =
        DEFAULT_LYRIC_BACKGROUND_OVERLAY_PERCENT as f32 / 100.;
    const LYRIC_TARGET_CONTRAST: f32 = DEFAULT_LYRIC_BACKGROUND_CONTRAST;

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

        let background = gpui::rgb(0x1f1f28).into();
        let foreground = gpui::rgb(0xdcd7ba).into();
        let blurred = generate_blurred_cover(
            &source,
            background,
            foreground,
            DEFAULT_LYRIC_BACKGROUND_OVERLAY_PERCENT,
            DEFAULT_LYRIC_BACKGROUND_CONTRAST,
        )
        .unwrap();
        let decoded = image::load_from_memory(&blurred.bytes).unwrap();

        assert_eq!(blurred.format, ImageFormat::Png);
        assert_eq!((decoded.width(), decoded.height()), (128, 128));
        assert_eq!(
            image::open(&source).unwrap().to_rgba8().get_pixel(0, 0).0,
            [120, 80, 200, 255]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_blurred_cover_contrast_and_hue() {
        let original = image::RgbImage::from_fn(64, 8, |x, y| {
            image::Rgb([
                (x % 8 * 255 / 7) as u8,
                (x / 8 * 255 / 7) as u8,
                (y * 255 / 7) as u8,
            ])
        });
        for theme in crate::design::ColorTheme::ALL {
            let palette = theme.palette();
            let background = Rgba::try_from(palette.background).unwrap();
            let foreground = Rgba::try_from(palette.foreground).unwrap();
            let toned = tone_blurred_cover(
                image::DynamicImage::ImageRgb8(original.clone()),
                background.into(),
                foreground.into(),
                DEFAULT_LYRIC_BACKGROUND_OVERLAY_PERCENT,
                DEFAULT_LYRIC_BACKGROUND_CONTRAST,
            );
            let foreground_luminance = relative_luminance(foreground);
            for (original, toned) in original.pixels().zip(toned.pixels()) {
                let [r, g, b] = toned.0.map(|channel| f32::from(channel) / 255.);
                let displayed = Rgba { r, g, b, a: 1. }
                    .blend(background.opacity(LYRIC_BACKGROUND_OVERLAY_OPACITY));
                let background_luminance = relative_luminance(displayed);
                assert!(
                    (foreground_luminance.max(background_luminance) + 0.05)
                        / (foreground_luminance.min(background_luminance) + 0.05)
                        >= LYRIC_TARGET_CONTRAST,
                    "{}",
                    theme.id()
                );
                let [original, toned] = [original, toned]
                    .map(|pixel| Oklch::from_color(Srgb::from(pixel.0).into_format::<f32>()));
                if original.chroma > 0.03 && toned.chroma > 0.03 {
                    let hue_difference = (original.hue.into_degrees() - toned.hue.into_degrees()
                        + 180.)
                        .rem_euclid(360.)
                        - 180.;
                    // 低彩度处的 8 位量化会放大角度误差，按色相变化对应的色差检查。
                    let hue_error =
                        2. * toned.chroma * (hue_difference.to_radians() / 2.).sin().abs();
                    assert!(
                        hue_error < 0.004,
                        "{}: {hue_difference}, {original:?} -> {toned:?}",
                        theme.id()
                    );
                }
            }
        }
    }

    #[test]
    fn test_blurred_cover_uses_overlay_contrast() {
        for (background, foreground, original) in [
            (0x1f1f28, 0xdcd7ba, [128, 120, 150]),
            (0xf2ecbc, 0x545464, [192, 184, 200]),
        ] {
            let background = gpui::rgb(background);
            let foreground_luminance = relative_luminance(gpui::rgb(foreground));
            let [r, g, b] = original.map(|channel| f32::from(channel) / 255.);
            let color = Rgba { r, g, b, a: 1. };
            for (color, readable) in [
                (color, false),
                (
                    color.blend(background.opacity(LYRIC_BACKGROUND_OVERLAY_OPACITY)),
                    true,
                ),
            ] {
                let luminance = relative_luminance(color);
                assert_eq!(
                    (foreground_luminance.max(luminance) + 0.05)
                        / (foreground_luminance.min(luminance) + 0.05)
                        >= LYRIC_TARGET_CONTRAST,
                    readable
                );
            }
            let image = image::RgbImage::from_pixel(1, 1, image::Rgb(original));
            let toned = tone_blurred_cover(
                image.into(),
                background.into(),
                gpui::rgb(foreground).into(),
                DEFAULT_LYRIC_BACKGROUND_OVERLAY_PERCENT,
                DEFAULT_LYRIC_BACKGROUND_CONTRAST,
            );
            assert_eq!(toned.get_pixel(0, 0).0, original);
        }
    }

    #[test]
    fn test_blurred_cover_custom_settings() {
        for (background, foreground) in [(0x1f1f28, 0xdcd7ba), (0xf2ecbc, 0x545464)] {
            let background = gpui::rgb(background);
            let foreground = gpui::rgb(foreground);
            let original = image::RgbImage::from_pixel(1, 1, image::Rgb([150, 140, 180]));
            let results = [(0, 3.), (40, 3.), (40, 6.)].map(|(overlay_percent, contrast)| {
                let toned = tone_blurred_cover(
                    original.clone().into(),
                    background.into(),
                    foreground.into(),
                    overlay_percent,
                    contrast,
                );
                let [r, g, b] = toned
                    .get_pixel(0, 0)
                    .0
                    .map(|channel| f32::from(channel) / 255.);
                let displayed = Rgba { r, g, b, a: 1. }
                    .blend(background.opacity(overlay_percent as f32 / 100.));
                let foreground_luminance = relative_luminance(foreground);
                let background_luminance = relative_luminance(displayed);
                assert!(
                    (foreground_luminance.max(background_luminance) + 0.05)
                        / (foreground_luminance.min(background_luminance) + 0.05)
                        >= contrast
                );
                toned
            });
            assert_ne!(results[0], results[1]);
            assert_ne!(results[1], results[2]);
        }
    }

    #[test]
    fn test_blurred_cover_crossfade_contrast() {
        for theme in crate::design::ColorTheme::ALL {
            let palette = theme.palette();
            let background = Rgba::try_from(palette.background).unwrap();
            let foreground_luminance =
                relative_luminance(Rgba::try_from(palette.foreground).unwrap());
            let covers = [[255, 0, 0], [0, 255, 0], [0, 0, 255]].map(|color| {
                let image = image::RgbImage::from_pixel(1, 1, image::Rgb(color));
                let toned = tone_blurred_cover(
                    image.into(),
                    background.into(),
                    Rgba::try_from(palette.foreground).unwrap().into(),
                    DEFAULT_LYRIC_BACKGROUND_OVERLAY_PERCENT,
                    DEFAULT_LYRIC_BACKGROUND_CONTRAST,
                );
                let [r, g, b] = toned
                    .get_pixel(0, 0)
                    .0
                    .map(|channel| f32::from(channel) / 255.);
                Rgba { r, g, b, a: 1. }
            });
            for (index, current) in covers.iter().enumerate() {
                let previous = covers[(index + 1) % covers.len()];
                for progress in [0., 0.125, 0.25, 0.5, 0.75, 0.875, 1.] {
                    let displayed = background
                        .blend(previous.opacity(1. - progress))
                        .blend(current.opacity(progress))
                        .blend(background.opacity(LYRIC_BACKGROUND_OVERLAY_OPACITY));
                    let luminance = relative_luminance(displayed);
                    assert!(
                        (foreground_luminance.max(luminance) + 0.05)
                            / (foreground_luminance.min(luminance) + 0.05)
                            >= LYRIC_TARGET_CONTRAST,
                        "{}: {progress}",
                        theme.id()
                    );
                }
            }
        }
    }
}
