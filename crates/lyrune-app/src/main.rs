mod app;
mod cache;
mod credentials;
mod library;
mod player;

use app::LyruneView;
use gpui::*;
use gpui_component::Root;

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        gpui_component::init(cx);
        let bounds = Bounds::centered(None, size(px(1080.), px(760.)), cx);

        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Lyrune".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(LyruneView::new);
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
