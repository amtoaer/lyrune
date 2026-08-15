use anyhow::Result;
use async_channel::Sender;

const ICON_SIZE: u32 = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayCommand {
    Show,
    Quit,
}

fn icon_rgba() -> Vec<u8> {
    const BACKGROUND: [u8; 4] = [0x89, 0xb4, 0xfa, 0xff];
    const FOREGROUND: [u8; 4] = [0x1e, 0x1e, 0x2e, 0xff];
    const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

    let mut pixels = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = if x < 16 {
                16 - x
            } else if x > 47 {
                x - 47
            } else {
                0
            };
            let dy = if y < 16 {
                16 - y
            } else if y > 47 {
                y - 47
            } else {
                0
            };
            let in_background = dx * dx + dy * dy <= 12 * 12;
            let in_letter = (25..=31).contains(&x) && (18..=44).contains(&y)
                || (25..=43).contains(&x) && (38..=44).contains(&y);
            pixels.extend_from_slice(if in_letter {
                &FOREGROUND
            } else if in_background {
                &BACKGROUND
            } else {
                &TRANSPARENT
            });
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
        assert_eq!(pixel(32, 32), [0x89, 0xb4, 0xfa, 0xff]);
        assert_eq!(pixel(28, 28), [0x1e, 0x1e, 0x2e, 0xff]);
        assert_eq!(pixel(39, 41), [0x1e, 0x1e, 0x2e, 0xff]);
    }
}
