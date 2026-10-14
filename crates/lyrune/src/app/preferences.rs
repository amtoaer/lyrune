//! 已提交的偏好、解析后的字体和唯一的设置保存队列；输入草稿仍属于设置页。

use gpui::{App, Context, Window};

use crate::design::{self, AppFonts, ColorTheme};
use crate::settings::{AppSettings, ColorThemeMode, SettingsWriter};

pub(super) struct Preferences {
    pub(super) settings: AppSettings,
    pub(super) fonts: AppFonts,
    pub(super) color_theme: ColorTheme,
    pub(super) writer: Option<SettingsWriter>,
}

impl Preferences {
    pub(super) fn new(settings: AppSettings, fonts: AppFonts, cx: &App) -> Self {
        Self {
            color_theme: settings.active_color_theme(cx.window_appearance()),
            settings,
            fonts,
            writer: SettingsWriter::new().ok(),
        }
    }

    pub(super) fn save(&self) {
        if let Some(writer) = &self.writer {
            writer.save(&self.settings);
        }
    }

    pub(super) fn finish(&mut self) {
        if let Some(writer) = self.writer.take() {
            // 播放快照未变化时会跳过保存，这里仍保存窗口大小等其他偏好并排空 FIFO。
            writer.save(&self.settings);
            writer.finish();
        }
    }

    pub(super) fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let appearance = cx.window_appearance();
        let mode = self.settings.color_theme_mode.resolve(appearance);
        self.color_theme = match mode {
            gpui_component::ThemeMode::Light => self.settings.light_color_theme,
            gpui_component::ThemeMode::Dark => self.settings.dark_color_theme,
        };
        design::apply(
            self.settings.light_color_theme,
            self.settings.dark_color_theme,
            mode,
            matches!(self.settings.color_theme_mode, ColorThemeMode::Auto),
            &self.fonts,
            Some(window),
            cx,
        );
        crate::set_tray_icon_style(self.settings.tray_icon_style.resolve(appearance), cx);
        cx.notify();
    }
}
