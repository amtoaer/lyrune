use anyhow::Result;
use async_channel::Sender;

const ICON_SIZE: u32 = 64;
pub(crate) const ICON_SVG: &[u8] = include_bytes!("../assets/lyrune.svg");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayCommand {
    Show,
    Quit,
}

fn icon_rgba() -> Vec<u8> {
    let tree = resvg::usvg::Tree::from_data(ICON_SVG, &resvg::usvg::Options::default())
        .expect("parse Lyrune tray icon");
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(ICON_SIZE, ICON_SIZE).expect("allocate Lyrune tray icon");
    let scale = ICON_SIZE as f32 / tree.size().width();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    let mut pixels = pixmap.take();
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
    pixels
}

#[cfg(target_os = "linux")]
mod platform {
    use anyhow::{Context as _, Result};
    use async_channel::Sender;
    use ksni::blocking::TrayMethods as _;
    use ksni::menu::{MenuItem, StandardItem};

    use super::{ICON_SIZE, TrayCommand, icon_rgba};

    struct LinuxTray {
        commands: Sender<TrayCommand>,
        icon: ksni::Icon,
    }

    impl LinuxTray {
        fn send(&self, command: TrayCommand) {
            let _ = self.commands.try_send(command);
        }
    }

    impl ksni::Tray for LinuxTray {
        fn id(&self) -> String {
            "lyrune".to_owned()
        }

        fn title(&self) -> String {
            "Lyrune".to_owned()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![self.icon.clone()]
        }

        fn activate(&mut self, _: i32, _: i32) {
            self.send(TrayCommand::Show);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            vec![
                StandardItem {
                    label: "打开 Lyrune".to_owned(),
                    activate: Box::new(|tray: &mut LinuxTray| tray.send(TrayCommand::Show)),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "退出".to_owned(),
                    icon_name: "application-exit".to_owned(),
                    activate: Box::new(|tray: &mut LinuxTray| tray.send(TrayCommand::Quit)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    pub struct DesktopTray {
        handle: ksni::blocking::Handle<LinuxTray>,
    }

    impl DesktopTray {
        pub fn install(commands: Sender<TrayCommand>) -> Result<Self> {
            let mut data = icon_rgba();
            for pixel in data.chunks_exact_mut(4) {
                pixel.rotate_right(1);
            }
            let tray = LinuxTray {
                commands,
                icon: ksni::Icon {
                    width: ICON_SIZE as i32,
                    height: ICON_SIZE as i32,
                    data,
                },
            };
            let handle = tray.spawn().context("无法注册 Linux 系统托盘图标")?;
            Ok(Self { handle })
        }
    }

    impl Drop for DesktopTray {
        fn drop(&mut self) {
            self.handle.shutdown().wait();
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod platform {
    use anyhow::{Context as _, Result};
    use async_channel::Sender;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    use super::{ICON_SIZE, TrayCommand, icon_rgba};

    pub struct DesktopTray {
        _icon: TrayIcon,
    }

    impl DesktopTray {
        pub fn install(commands: Sender<TrayCommand>) -> Result<Self> {
            let menu = Menu::new();
            let show = MenuItem::new("打开 Lyrune", true, None);
            let quit = MenuItem::new("退出", true, None);
            menu.append_items(&[&show, &quit])
                .context("无法创建系统托盘菜单")?;

            let show_id = show.id().clone();
            let quit_id = quit.id().clone();
            let menu_commands = commands.clone();
            MenuEvent::set_event_handler(Some(move |event| {
                let command = if event.id == show_id {
                    Some(TrayCommand::Show)
                } else if event.id == quit_id {
                    Some(TrayCommand::Quit)
                } else {
                    None
                };
                if let Some(command) = command {
                    let _ = menu_commands.try_send(command);
                }
            }));

            let click_commands = commands;
            TrayIconEvent::set_event_handler(Some(move |event| {
                if matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                ) {
                    let _ = click_commands.try_send(TrayCommand::Show);
                }
            }));

            let icon = Icon::from_rgba(icon_rgba(), ICON_SIZE, ICON_SIZE)
                .context("无法创建系统托盘图标")?;
            let icon = TrayIconBuilder::new()
                .with_tooltip("Lyrune")
                .with_icon(icon)
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .build()
                .context("无法注册系统托盘图标")?;
            Ok(Self { _icon: icon })
        }
    }
}

pub use platform::DesktopTray;

pub fn install(commands: Sender<TrayCommand>) -> Result<DesktopTray> {
    DesktopTray::install(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_icon_has_expected_geometry() {
        let icon = icon_rgba();
        assert_eq!(icon.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);

        let pixel = |x: u32, y: u32| {
            let start = ((y * ICON_SIZE + x) * 4) as usize;
            &icon[start..start + 4]
        };
        assert_eq!(pixel(0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(32, 32)[3], 255);
        assert!(
            icon.chunks_exact(4)
                .filter(|pixel| pixel[3] == 255)
                .map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 8
        );
    }
}
