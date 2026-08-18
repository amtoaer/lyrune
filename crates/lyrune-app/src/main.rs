mod app;
mod cache;
mod credentials;
mod design;
mod http;
mod icons;
mod library;
mod lyrics_cache;
#[cfg(target_os = "linux")]
mod mpris;
mod player;
mod settings;
mod single_instance;
mod singleflight;
mod tray;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use app::LyruneView;
use gpui::*;
use gpui_component::Root;
use settings::{PersistedWindowSize, SettingsStore};
use tray::TrayCommand;

const DEFAULT_WINDOW_WIDTH: f32 = 1080.;
const DEFAULT_WINDOW_HEIGHT: f32 = 760.;
const MIN_WINDOW_WIDTH: f32 = 800.;
const MIN_WINDOW_HEIGHT: f32 = 600.;
const INACTIVE_WINDOW_FRAME_INTERVAL: Duration = Duration::from_millis(40);

fn initial_window_size(window_size: Option<PersistedWindowSize>, cx: &App) -> Size<Pixels> {
    let (mut width, mut height) = window_size
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

fn main_window_options(window_size: Option<PersistedWindowSize>, cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, initial_window_size(window_size, cx), cx);
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Lyrune".into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
        inactive_frame_interval: Some(INACTIVE_WINDOW_FRAME_INTERVAL),
        app_id: Some("lyrune".to_owned()),
        ..Default::default()
    }
}

struct MainWindowState {
    view: Entity<LyruneView>,
    window: Option<WindowHandle<Root>>,
}

struct TrayState {
    _tray: tray::DesktopTray,
}

impl Global for TrayState {}

#[cfg(target_os = "linux")]
struct MprisState {
    _service: mpris::MprisService,
}

#[cfg(target_os = "linux")]
impl Global for MprisState {}

fn open_restored_window(
    view: Entity<LyruneView>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Root>> {
    let options = main_window_options(view.read(cx).window_size(), cx);
    cx.open_window(options, move |window, cx| {
        view.update(cx, |view, cx| view.attach_window(window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    })
}

fn show_main_window(state: &Rc<RefCell<MainWindowState>>, cx: &mut App) {
    if let Some(window_handle) = state.borrow().window
        && window_handle
            .update(cx, |_, window, cx| {
                cx.activate(true);
                window.activate_window();
            })
            .is_ok()
    {
        return;
    }

    let view = state.borrow().view.clone();
    match open_restored_window(view, cx) {
        Ok(window_handle) => {
            state.borrow_mut().window = Some(window_handle);
            cx.activate(true);
            let _ = window_handle.update(cx, |_, window, _| window.activate_window());
        }
        Err(error) => eprintln!("无法重新打开 Lyrune 窗口：{error:#}"),
    }
}

fn main() {
    let instance = match single_instance::acquire() {
        Ok(single_instance::InstanceClaim::Primary(instance)) => instance,
        Ok(single_instance::InstanceClaim::Secondary) => return,
        Err(error) => {
            eprintln!("无法建立 Lyrune 单例：{error:#}");
            return;
        }
    };
    let instance_commands = instance.commands();

    let _ = rustls::crypto::ring::default_provider().install_default();
    let http_client = http::client().expect("create image HTTP client");
    gpui_platform::application()
        .with_http_client(http_client)
        .with_quit_mode(QuitMode::Explicit)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            let settings = SettingsStore::load().unwrap_or_default();
            let fonts = design::resolve_fonts(
                &settings.ui_font_families,
                &settings.monospace_font_families,
                &settings.lyric_font_families,
                cx,
            );
            design::apply(settings.color_theme, &fonts, None, cx);
            let (tray_commands, tray_events) = async_channel::unbounded();
            let tray_available = match tray::install(tray_commands) {
                Ok(tray) => {
                    cx.set_global(TrayState { _tray: tray });
                    true
                }
                Err(error) => {
                    eprintln!("系统托盘不可用，将保留关闭窗口即退出的行为：{error:#}");
                    false
                }
            };

            let view_slot = Rc::new(RefCell::new(None));
            let view_slot_for_window = view_slot.clone();
            let window_handle = cx
                .open_window(
                    main_window_options(settings.window_size, cx),
                    move |window, cx| {
                        let view = cx.new(|cx| LyruneView::new(window, settings, fonts, cx));
                        *view_slot_for_window.borrow_mut() = Some(view.clone());
                        cx.new(|cx| Root::new(view, window, cx))
                    },
                )
                .expect("open Lyrune window");
            let view = view_slot
                .borrow_mut()
                .take()
                .expect("Lyrune view was created with its window");

            view.update(cx, |view, cx| view.start_background_tick(cx));

            let main_window = Rc::new(RefCell::new(MainWindowState {
                view,
                window: Some(window_handle),
            }));
            let main_window_for_close = main_window.clone();
            cx.on_window_closed(move |cx, window_id| {
                let is_main_window = main_window_for_close
                    .borrow()
                    .window
                    .is_some_and(|window| window.window_id() == window_id);
                if is_main_window {
                    main_window_for_close.borrow_mut().window = None;
                }
                if !tray_available && cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let main_window_for_instance = main_window.clone();
            cx.spawn(async move |cx| {
                while let Ok(command) = instance_commands.recv().await {
                    match command {
                        single_instance::InstanceCommand::Show => {
                            let _ = cx.update(|cx| show_main_window(&main_window_for_instance, cx));
                        }
                    }
                }
            })
            .detach();

            #[cfg(target_os = "linux")]
            match mpris::install() {
                Ok((service, mpris_events)) => {
                    let mpris_handle = service.handle();
                    main_window.borrow().view.update(cx, |view, _| {
                        view.attach_mpris(mpris_handle);
                    });
                    cx.set_global(MprisState { _service: service });

                    let main_window_for_mpris = main_window.clone();
                    cx.spawn(async move |cx| {
                        while let Ok(command) = mpris_events.recv().await {
                            let quitting = matches!(command, mpris::MprisCommand::Quit);
                            cx.update(|cx| match command {
                                mpris::MprisCommand::Raise => {
                                    show_main_window(&main_window_for_mpris, cx);
                                }
                                mpris::MprisCommand::Quit => cx.quit(),
                                command => {
                                    let view = main_window_for_mpris.borrow().view.clone();
                                    view.update(cx, |view, cx| {
                                        view.handle_mpris_command(command, cx);
                                    });
                                }
                            });
                            if quitting {
                                break;
                            }
                        }
                    })
                    .detach();
                }
                Err(error) => eprintln!("MPRIS 服务不可用：{error:#}"),
            }

            let main_window_for_tray = main_window;
            cx.spawn(async move |cx| {
                while let Ok(command) = tray_events.recv().await {
                    match command {
                        TrayCommand::Show => {
                            let _ = cx.update(|cx| show_main_window(&main_window_for_tray, cx));
                        }
                        TrayCommand::Quit => {
                            let _ = cx.update(|cx| cx.quit());
                            break;
                        }
                    }
                }
            })
            .detach();

            cx.activate(true);
        });

    drop(instance);
}
