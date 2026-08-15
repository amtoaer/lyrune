use std::{rc::Rc, sync::OnceLock};

#[cfg(target_os = "linux")]
use std::process::Command;

use gpui::{App, Window};
use gpui_component::{Theme, ThemeConfig, ThemeConfigColors, ThemeMode};
use serde::{Deserialize, Serialize};

static SYSTEM_UI_FONT_FAMILY: OnceLock<String> = OnceLock::new();

fn system_ui_font_family() -> &'static str {
    SYSTEM_UI_FONT_FAMILY
        .get_or_init(|| {
            #[cfg(target_os = "linux")]
            if let Ok(output) = Command::new("fc-match")
                .args(["--format=%{family[0]}", "system-ui"])
                .output()
                && output.status.success()
                && let Ok(family) = String::from_utf8(output.stdout)
                && !family.trim().is_empty()
            {
                return family.trim().to_owned();
            }

            ".SystemUIFont".to_owned()
        })
        .as_str()
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorTheme {
    #[default]
    LyruneNeutral,
    CatppuccinMocha,
    AyuDark,
    EverforestDark,
}

impl ColorTheme {
    pub const ALL: [Self; 4] = [
        Self::LyruneNeutral,
        Self::CatppuccinMocha,
        Self::AyuDark,
        Self::EverforestDark,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::LyruneNeutral => "lyrune-neutral",
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::AyuDark => "ayu-dark",
            Self::EverforestDark => "everforest-dark",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::LyruneNeutral => "Lyrune Neutral",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::AyuDark => "Ayu Dark",
            Self::EverforestDark => "Everforest Dark",
        }
    }

    pub const fn icon_foreground(self) -> &'static str {
        match self {
            Self::LyruneNeutral => "#18181b",
            Self::CatppuccinMocha => "#cdd6f4",
            Self::AyuDark => "#b3b1ad",
            Self::EverforestDark => "#d3c6aa",
        }
    }

    pub const fn icon_accent(self) -> &'static str {
        match self {
            Self::LyruneNeutral => "#18181b",
            Self::CatppuccinMocha => "#89b4fa",
            Self::AyuDark => "#5ac1fe",
            Self::EverforestDark => "#e69875",
        }
    }

    pub const fn icon_on_accent(self) -> &'static str {
        match self {
            Self::LyruneNeutral => "#fafafa",
            Self::CatppuccinMocha => "#1e1e2e",
            Self::AyuDark => "#1f2430",
            Self::EverforestDark => "#262e34",
        }
    }

    fn palette(self) -> Palette {
        match self {
            Self::LyruneNeutral => Palette {
                mode: ThemeMode::Light,
                background: "#f7f7f8",
                surface: "#ffffff",
                surface_alt: "#fafafa",
                sidebar: "#fafafa",
                outer: "#f4f4f5",
                foreground: "#18181b",
                subtext_foreground: "#52525b",
                muted: "#f4f4f5",
                muted_foreground: "#71717a",
                border: "#e4e4e7",
                primary: "#18181b",
                primary_foreground: "#fafafa",
                primary_hover: "#27272a",
                primary_active: "#09090b",
                accent: "#f4f4f5",
                accent_foreground: "#18181b",
                active: "#e4e4e7",
                hover: "#f4f4f5",
                ring: "#a1a1aa",
                emotion: "#e11d48",
                emotion_foreground: "#ffffff",
                scrollbar_thumb: "#d4d4d8",
            },
            Self::CatppuccinMocha => Palette {
                mode: ThemeMode::Dark,
                background: "#1e1e2e",
                surface: "#181825",
                surface_alt: "#313244",
                sidebar: "#181825",
                outer: "#11111b",
                foreground: "#cdd6f4",
                subtext_foreground: "#a6adc8",
                muted: "#313244",
                muted_foreground: "#6c7086",
                border: "#313244",
                primary: "#89b4fa",
                primary_foreground: "#1e1e2e",
                primary_hover: "#74c7ec",
                primary_active: "#89b4fa",
                accent: "#2e2e3e",
                accent_foreground: "#cdd6f4",
                active: "#89b4fa1f",
                hover: "#31324499",
                ring: "#cba6f7",
                emotion: "#f5c2e7",
                emotion_foreground: "#1e1e2e",
                scrollbar_thumb: "#45475a",
            },
            Self::AyuDark => Palette {
                mode: ThemeMode::Dark,
                background: "#0d1016",
                surface: "#16191f",
                surface_alt: "#1f2127",
                sidebar: "#16191f",
                outer: "#090b10",
                foreground: "#b3b1ad",
                subtext_foreground: "#9da0a2",
                muted: "#1f2127",
                muted_foreground: "#73777b",
                border: "#292a2c",
                primary: "#5ac1fe",
                primary_foreground: "#1f2430",
                primary_hover: "#3daee9",
                primary_active: "#36a3d9",
                accent: "#20242b",
                accent_foreground: "#b3b1ad",
                active: "#36a3d922",
                hover: "#191f2a99",
                ring: "#ffb454",
                emotion: "#f07178",
                emotion_foreground: "#0d1016",
                scrollbar_thumb: "#bfbdb64c",
            },
            Self::EverforestDark => Palette {
                mode: ThemeMode::Dark,
                background: "#262e34",
                surface: "#2e383b",
                surface_alt: "#343f44",
                sidebar: "#1f262b",
                outer: "#1e2326",
                foreground: "#d3c6aa",
                subtext_foreground: "#9da9a0",
                muted: "#2e383b",
                muted_foreground: "#849087",
                border: "#40484c",
                primary: "#e69875",
                primary_foreground: "#262e34",
                primary_hover: "#dbbc7f",
                primary_active: "#e69875",
                accent: "#3c4448",
                accent_foreground: "#d3c6aa",
                active: "#a7c08022",
                hover: "#3e474b99",
                ring: "#7fbbb3",
                emotion: "#e67e80",
                emotion_foreground: "#262e34",
                scrollbar_thumb: "#485156",
            },
        }
    }
}

struct Palette {
    mode: ThemeMode,
    background: &'static str,
    surface: &'static str,
    surface_alt: &'static str,
    sidebar: &'static str,
    outer: &'static str,
    foreground: &'static str,
    subtext_foreground: &'static str,
    muted: &'static str,
    muted_foreground: &'static str,
    border: &'static str,
    primary: &'static str,
    primary_foreground: &'static str,
    primary_hover: &'static str,
    primary_active: &'static str,
    accent: &'static str,
    accent_foreground: &'static str,
    active: &'static str,
    hover: &'static str,
    ring: &'static str,
    emotion: &'static str,
    emotion_foreground: &'static str,
    scrollbar_thumb: &'static str,
}

pub fn apply(color_theme: ColorTheme, window: Option<&mut Window>, cx: &mut App) {
    let palette = color_theme.palette();
    let mode = palette.mode;
    let config = Rc::new(theme_config(color_theme.label(), palette));

    if mode.is_dark() {
        Theme::global_mut(cx).dark_theme = config;
    } else {
        Theme::global_mut(cx).light_theme = config;
    }
    Theme::change(mode, window, cx);
    Theme::global_mut(cx).list.active_highlight = false;
}

fn theme_config(name: &'static str, palette: Palette) -> ThemeConfig {
    let mut colors = ThemeConfigColors::default();
    colors.background = Some(palette.background.into());
    colors.foreground = Some(palette.foreground.into());
    colors.border = Some(palette.border.into());
    colors.input = Some(palette.border.into());
    colors.ring = Some(palette.ring.into());
    colors.caret = Some(palette.primary.into());
    colors.selection = Some(palette.active.into());
    colors.link = Some(palette.primary.into());
    colors.link_hover = Some(palette.primary_hover.into());
    colors.link_active = Some(palette.primary_active.into());
    colors.muted = Some(palette.muted.into());
    colors.muted_foreground = Some(palette.muted_foreground.into());
    colors.accent = Some(palette.accent.into());
    colors.accent_foreground = Some(palette.accent_foreground.into());
    colors.primary = Some(palette.primary.into());
    colors.primary_foreground = Some(palette.primary_foreground.into());
    colors.primary_hover = Some(palette.primary_hover.into());
    colors.primary_active = Some(palette.primary_active.into());
    colors.secondary = Some(palette.muted.into());
    colors.secondary_foreground = Some(palette.subtext_foreground.into());
    colors.secondary_hover = Some(palette.hover.into());
    colors.secondary_active = Some(palette.accent.into());
    colors.danger = Some(palette.emotion.into());
    colors.danger_foreground = Some(palette.emotion_foreground.into());
    colors.danger_hover = Some(palette.emotion.into());
    colors.danger_active = Some(palette.emotion.into());
    colors.button = Some(palette.surface.into());
    colors.button_foreground = Some(palette.foreground.into());
    colors.button_hover = Some(palette.hover.into());
    colors.button_active = Some(palette.accent.into());
    colors.button_primary = Some(palette.primary.into());
    colors.button_primary_foreground = Some(palette.primary_foreground.into());
    colors.button_primary_hover = Some(palette.primary_hover.into());
    colors.button_primary_active = Some(palette.primary_active.into());
    colors.button_secondary = Some(palette.muted.into());
    colors.button_secondary_foreground = Some(palette.foreground.into());
    colors.button_secondary_hover = Some(palette.hover.into());
    colors.button_secondary_active = Some(palette.accent.into());
    colors.group_box = Some(palette.surface.into());
    colors.group_box_foreground = Some(palette.foreground.into());
    colors.popover = Some(palette.surface.into());
    colors.popover_foreground = Some(palette.foreground.into());
    colors.sidebar = Some(palette.sidebar.into());
    colors.sidebar_foreground = Some(palette.foreground.into());
    colors.sidebar_accent = Some(palette.surface_alt.into());
    colors.sidebar_accent_foreground = Some(palette.accent_foreground.into());
    colors.sidebar_border = Some(palette.border.into());
    colors.sidebar_primary = Some(palette.primary.into());
    colors.sidebar_primary_foreground = Some(palette.primary_foreground.into());
    colors.list = Some(palette.sidebar.into());
    colors.list_active = Some(palette.surface_alt.into());
    colors.list_active_border = Some("#00000000".into());
    colors.list_hover = Some(palette.hover.into());
    colors.list_even = Some(palette.sidebar.into());
    colors.list_head = Some(palette.sidebar.into());
    colors.table = Some(palette.background.into());
    colors.table_head = Some(palette.background.into());
    colors.table_head_foreground = Some(palette.muted_foreground.into());
    colors.table_hover = Some(palette.hover.into());
    colors.table_active = Some(palette.active.into());
    colors.table_active_border = Some("#00000000".into());
    colors.table_even = Some(palette.background.into());
    colors.table_row_border = Some("#00000000".into());
    colors.slider_bar = Some(palette.primary.into());
    colors.slider_thumb = Some(palette.primary.into());
    colors.progress_bar = Some(palette.primary.into());
    colors.scrollbar = Some("#00000000".into());
    colors.scrollbar_thumb = Some(palette.scrollbar_thumb.into());
    colors.scrollbar_thumb_hover = Some(palette.muted_foreground.into());
    colors.title_bar = Some(palette.outer.into());
    colors.title_bar_border = Some(palette.border.into());
    colors.status_bar = Some(palette.outer.into());
    colors.status_bar_border = Some(palette.border.into());
    colors.window_border = Some(palette.border.into());

    ThemeConfig {
        is_default: true,
        name: name.into(),
        mode: palette.mode,
        font_size: Some(14.),
        font_family: Some(system_ui_font_family().into()),
        radius: Some(8),
        radius_lg: Some(12),
        shadow: Some(true),
        colors,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_theme_has_a_stable_serialized_id() {
        for theme in ColorTheme::ALL {
            let json = serde_json::to_string(&theme).expect("serialize color theme");
            let restored: ColorTheme =
                serde_json::from_str(&json).expect("deserialize color theme");
            assert_eq!(restored, theme);
            assert_eq!(json, format!("\"{}\"", theme.id()));
        }
    }
}
