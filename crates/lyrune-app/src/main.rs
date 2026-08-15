mod app;
mod cache;
mod credentials;
mod design;
mod http;
mod icons;
mod library;
mod player;
mod settings;
mod singleflight;

use app::LyruneView;
use gpui::*;
use gpui_component::Root;
use settings::{AppSettings, SettingsStore};

const DEFAULT_WINDOW_WIDTH: f32 = 1080.;
const DEFAULT_WINDOW_HEIGHT: f32 = 760.;
const MIN_WINDOW_WIDTH: f32 = 800.;
const MIN_WINDOW_HEIGHT: f32 = 600.;

fn initial_window_size(settings: &AppSettings, cx: &App) -> Size<Pixels> {
    let (mut width, mut height) = settings
        .window_size
        .map_or((DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT), |size| {
            (size.width as f32, size.height as f32)
        });
    width = width.max(MIN_WINDOW_WIDTH);
    height = height.max(MIN_WINDOW_HEIGHT);
    if let Some(display) = cx.primary_display() {
        let display_size = display.bounds().size;
        width = width.min(f32::from(display_size.width).max(MIN_WINDOW_WIDTH));
        height = height.min(f32::from(display_size.height).max(MIN_WINDOW_HEIGHT));
    }
    size(px(width), px(height))
}

fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http_client = http::client().expect("create image HTTP client");
    gpui_platform::application()
        .with_http_client(http_client)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            let settings = SettingsStore::load().unwrap_or_default();
            design::apply(settings.color_theme, None, cx);
            let bounds = Bounds::centered(None, initial_window_size(&settings, cx), cx);

            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some("Lyrune".into()),
                        ..Default::default()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
                    ..Default::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| LyruneView::new(window, settings, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                },
            )
            .expect("open Lyrune window");

            cx.activate(true);
            cx.on_window_closed(|cx, _window_id| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        });
}
