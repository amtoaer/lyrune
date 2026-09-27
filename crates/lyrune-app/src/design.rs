use std::rc::Rc;

use gpui::{Anchor, App, Font, FontFallbacks, Window, WindowAppearance, font};
use gpui_component::{Theme, ThemeConfig, ThemeConfigColors, ThemeMode};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub(crate) struct AppFonts {
    pub ui: Font,
    pub monospace: Font,
    pub lyrics: Font,
}

pub(crate) fn resolve_fonts(
    ui: &[String],
    monospace: &[String],
    lyrics: &[String],
    cx: &App,
) -> AppFonts {
    let available = cx.text_system().all_font_names();
    AppFonts {
        ui: resolve_font_chain(ui, ".SystemUIFont", &available),
        monospace: resolve_font_chain(monospace, ".SystemUIFont", &available),
        lyrics: resolve_font_chain(lyrics, ".SystemUIFont", &available),
    }
}

fn resolve_font_chain(families: &[String], default: &str, available: &[String]) -> Font {
    let mut resolved = families
        .iter()
        .filter_map(|family| {
            available
                .iter()
                .find(|available| available.eq_ignore_ascii_case(family))
                .cloned()
        })
        .collect::<Vec<_>>();
    if resolved.is_empty() {
        resolved.push(default.to_owned());
    }

    let mut resolved_font = font(resolved.remove(0));
    if !resolved.is_empty() {
        resolved_font.fallbacks = Some(FontFallbacks::from_fonts(resolved));
    }
    resolved_font
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorTheme {
    #[serde(alias = "lyrune-neutral")]
    CatppuccinLatte,
    CatppuccinMocha,
    AyuLight,
    AyuDark,
    #[default]
    EverforestLight,
    EverforestDark,
    RosePineDawn,
    RosePineMoon,
    KanagawaLotus,
    KanagawaWave,
    AyuMirage,
    OneDark,
    OneLight,
    GruvboxLight,
    GruvboxDark,
    DraculaLight,
    DraculaDark,
}

impl ColorTheme {
    pub const ALL: [Self; 17] = [
        Self::EverforestLight,
        Self::EverforestDark,
        Self::CatppuccinLatte,
        Self::CatppuccinMocha,
        Self::AyuLight,
        Self::AyuDark,
        Self::AyuMirage,
        Self::RosePineDawn,
        Self::RosePineMoon,
        Self::KanagawaLotus,
        Self::KanagawaWave,
        Self::OneDark,
        Self::OneLight,
        Self::GruvboxLight,
        Self::GruvboxDark,
        Self::DraculaLight,
        Self::DraculaDark,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::CatppuccinLatte => "catppuccin-latte",
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::AyuLight => "ayu-light",
            Self::AyuDark => "ayu-dark",
            Self::EverforestLight => "everforest-light",
            Self::EverforestDark => "everforest-dark",
            Self::RosePineDawn => "rose-pine-dawn",
            Self::RosePineMoon => "rose-pine-moon",
            Self::KanagawaLotus => "kanagawa-lotus",
            Self::KanagawaWave => "kanagawa-wave",
            Self::AyuMirage => "ayu-mirage",
            Self::OneDark => "one-dark",
            Self::OneLight => "one-light",
            Self::GruvboxLight => "gruvbox-light",
            Self::GruvboxDark => "gruvbox-dark",
            Self::DraculaLight => "dracula-light",
            Self::DraculaDark => "dracula-dark",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::CatppuccinLatte => "Catppuccin Latte",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::AyuLight => "Ayu Light",
            Self::AyuDark => "Ayu Dark",
            Self::EverforestLight => "Everforest Light",
            Self::EverforestDark => "Everforest Dark",
            Self::RosePineDawn => "Rosé Pine Dawn",
            Self::RosePineMoon => "Rosé Pine Moon",
            Self::KanagawaLotus => "Kanagawa Lotus",
            Self::KanagawaWave => "Kanagawa Wave",
            Self::AyuMirage => "Ayu Mirage",
            Self::OneDark => "One Dark",
            Self::OneLight => "One Light",
            Self::GruvboxLight => "GruvBox Light",
            Self::GruvboxDark => "GruvBox Dark",
            Self::DraculaLight => "Dracula Light",
            Self::DraculaDark => "Dracula Dark",
        }
    }

    pub(crate) const fn palette(self) -> Palette {
        match self {
            Self::CatppuccinLatte => Palette {
                background: "#eff1f5",
                surface: "#e6e9ef",
                sidebar: "#e6e9ef",
                outer: "#dce0e8",
                foreground: "#4c4f69",
                subtext_foreground: "#6c6f85",
                muted: "#ccd0da",
                muted_foreground: "#9ca0b0",
                border: "#ccd0da",
                primary: "#1e66f5",
                primary_foreground: "#eff1f5",
                primary_hover: "#209fb5",
                primary_active: "#1e66f5",
                accent: "#dfe2e9",
                accent_foreground: "#4c4f69",
                active: "#1e66f51f",
                hover: "#ccd0da99",
                ring: "#8839ef",
                emotion: "#ea76cb",
                emotion_foreground: "#eff1f5",
                info: "#1e66f5",
                info_foreground: "#eff1f5",
                success: "#40a02b",
                success_foreground: "#eff1f5",
                warning: "#df8e1d",
                warning_foreground: "#4c4f69",
                scrollbar_thumb: "#bcc0cc",
            },
            Self::CatppuccinMocha => Palette {
                background: "#1e1e2e",
                surface: "#181825",
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
                info: "#89b4fa",
                info_foreground: "#1e1e2e",
                success: "#a6e3a1",
                success_foreground: "#1e1e2e",
                warning: "#f9e2af",
                warning_foreground: "#1e1e2e",
                scrollbar_thumb: "#45475a",
            },
            Self::AyuLight => Palette {
                background: "#fcfcfc",
                surface: "#f8f9fa",
                sidebar: "#f8f9fa",
                outer: "#ebeef0",
                foreground: "#5c6166",
                subtext_foreground: "#828e9f",
                muted: "#ebeef0",
                muted_foreground: "#939498",
                border: "#6b7d8f1f",
                primary: "#22a4e6",
                primary_foreground: "#ffffff",
                primary_hover: "#1a91cd",
                primary_active: "#1a91cd",
                accent: "#f3f4f5",
                accent_foreground: "#5c6166",
                active: "#035bd626",
                hover: "#6b7d8f1f",
                ring: "#f29718",
                emotion: "#e65050",
                emotion_foreground: "#ffffff",
                info: "#22a4e6",
                info_foreground: "#ffffff",
                success: "#86b300",
                success_foreground: "#ffffff",
                warning: "#f29718",
                warning_foreground: "#ffffff",
                scrollbar_thumb: "#c5c5c8",
            },
            Self::AyuDark => Palette {
                background: "#0d1016",
                surface: "#16191f",
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
                info: "#5ac1fe",
                info_foreground: "#0d1016",
                success: "#aad94c",
                success_foreground: "#0d1016",
                warning: "#ffb454",
                warning_foreground: "#0d1016",
                scrollbar_thumb: "#bfbdb64c",
            },
            Self::EverforestLight => Palette {
                background: "#fdf6e3",
                surface: "#f4f0d9",
                sidebar: "#f4f0d9",
                outer: "#efebd4",
                foreground: "#5c6a72",
                subtext_foreground: "#829181",
                muted: "#efebd4",
                muted_foreground: "#939f91",
                border: "#e0dcc7",
                primary: "#f57d26",
                primary_foreground: "#fdf6e3",
                primary_hover: "#dfa000",
                primary_active: "#f57d26",
                accent: "#e6e2cc",
                accent_foreground: "#5c6a72",
                active: "#8da10122",
                hover: "#efebd499",
                ring: "#3a94c5",
                emotion: "#f85552",
                emotion_foreground: "#fdf6e3",
                info: "#3a94c5",
                info_foreground: "#fdf6e3",
                success: "#8da101",
                success_foreground: "#fdf6e3",
                warning: "#dfa000",
                warning_foreground: "#5c6a72",
                scrollbar_thumb: "#bdc3af",
            },
            Self::EverforestDark => Palette {
                background: "#262e34",
                surface: "#2e383b",
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
                info: "#7fbbb3",
                info_foreground: "#262e34",
                success: "#a7c080",
                success_foreground: "#262e34",
                warning: "#dbbc7f",
                warning_foreground: "#262e34",
                scrollbar_thumb: "#485156",
            },
            Self::RosePineDawn => Palette {
                background: "#faf4ed",
                surface: "#fffaf3",
                sidebar: "#fffaf3",
                outer: "#f2e9e1",
                foreground: "#575279",
                subtext_foreground: "#797593",
                muted: "#dfdad9",
                muted_foreground: "#9893a5",
                border: "#dfdad9",
                primary: "#907aa9",
                primary_foreground: "#faf4ed",
                primary_hover: "#56949f",
                primary_active: "#907aa9",
                accent: "#f4ede8",
                accent_foreground: "#575279",
                active: "#907aa91f",
                hover: "#dfdad999",
                ring: "#56949f",
                emotion: "#b4637a",
                emotion_foreground: "#faf4ed",
                info: "#56949f",
                info_foreground: "#faf4ed",
                success: "#286983",
                success_foreground: "#faf4ed",
                warning: "#ea9d34",
                warning_foreground: "#575279",
                scrollbar_thumb: "#cecacd",
            },
            Self::RosePineMoon => Palette {
                background: "#232136",
                surface: "#2a273f",
                sidebar: "#2a273f",
                outer: "#191724",
                foreground: "#e0def4",
                subtext_foreground: "#908caa",
                muted: "#393552",
                muted_foreground: "#6e6a86",
                border: "#393552",
                primary: "#c4a7e7",
                primary_foreground: "#232136",
                primary_hover: "#9ccfd8",
                primary_active: "#c4a7e7",
                accent: "#393552",
                accent_foreground: "#e0def4",
                active: "#c4a7e71f",
                hover: "#39355299",
                ring: "#9ccfd8",
                emotion: "#eb6f92",
                emotion_foreground: "#232136",
                info: "#9ccfd8",
                info_foreground: "#232136",
                success: "#31748f",
                success_foreground: "#232136",
                warning: "#f6c177",
                warning_foreground: "#232136",
                scrollbar_thumb: "#56526e",
            },
            Self::KanagawaLotus => Palette {
                background: "#f2ecbc",
                surface: "#e5ddb0",
                sidebar: "#e5ddb0",
                outer: "#dcd5ac",
                foreground: "#545464",
                subtext_foreground: "#716e61",
                muted: "#e7dba0",
                muted_foreground: "#8a8980",
                border: "#d5cea3",
                primary: "#4d699b",
                primary_foreground: "#f2ecbc",
                primary_hover: "#6693bf",
                primary_active: "#4d699b",
                accent: "#dcd5ac",
                accent_foreground: "#545464",
                active: "#4d699b22",
                hover: "#dcd5ac99",
                ring: "#766b90",
                emotion: "#b35b79",
                emotion_foreground: "#f2ecbc",
                info: "#4d699b",
                info_foreground: "#f2ecbc",
                success: "#6f894e",
                success_foreground: "#f2ecbc",
                warning: "#cc6d00",
                warning_foreground: "#545464",
                scrollbar_thumb: "#a09cac",
            },
            Self::KanagawaWave => Palette {
                background: "#1f1f28",
                surface: "#181820",
                sidebar: "#181820",
                outer: "#16161d",
                foreground: "#dcd7ba",
                subtext_foreground: "#c8c093",
                muted: "#2a2a37",
                muted_foreground: "#727169",
                border: "#363646",
                primary: "#7e9cd8",
                primary_foreground: "#1f1f28",
                primary_hover: "#7fb4ca",
                primary_active: "#7e9cd8",
                accent: "#223249",
                accent_foreground: "#dcd7ba",
                active: "#7e9cd822",
                hover: "#2d4f6799",
                ring: "#957fb8",
                emotion: "#d27e99",
                emotion_foreground: "#1f1f28",
                info: "#7e9cd8",
                info_foreground: "#1f1f28",
                success: "#98bb6c",
                success_foreground: "#1f1f28",
                warning: "#e6c384",
                warning_foreground: "#1f1f28",
                scrollbar_thumb: "#54546d",
            },
            Self::AyuMirage => Palette {
                background: "#1f2430",
                surface: "#232834",
                sidebar: "#232834",
                outer: "#191e29",
                foreground: "#cbccc6",
                subtext_foreground: "#a3a6ad",
                muted: "#232834",
                muted_foreground: "#707a8c",
                border: "#3a4251",
                primary: "#ffcc66",
                primary_foreground: "#1f2430",
                primary_hover: "#ffd580",
                primary_active: "#ffcc66",
                accent: "#2b3240",
                accent_foreground: "#cbccc6",
                active: "#ffcc6622",
                hover: "#3a425199",
                ring: "#80cbc4",
                emotion: "#f28779",
                emotion_foreground: "#1f2430",
                info: "#80cbc4",
                info_foreground: "#1f2430",
                success: "#bae67e",
                success_foreground: "#1f2430",
                warning: "#ffcc66",
                warning_foreground: "#1f2430",
                scrollbar_thumb: "#4b5568",
            },
            Self::OneDark => Palette {
                background: "#282c34",
                surface: "#21252b",
                sidebar: "#21252b",
                outer: "#1b1d23",
                foreground: "#abb2bf",
                subtext_foreground: "#9da5b4",
                muted: "#21252b",
                muted_foreground: "#5c6370",
                border: "#3e4451",
                primary: "#61afef",
                primary_foreground: "#282c34",
                primary_hover: "#74bff8",
                primary_active: "#61afef",
                accent: "#2c323c",
                accent_foreground: "#abb2bf",
                active: "#61afef22",
                hover: "#3e445199",
                ring: "#c678dd",
                emotion: "#e06c75",
                emotion_foreground: "#282c34",
                info: "#61afef",
                info_foreground: "#282c34",
                success: "#98c379",
                success_foreground: "#282c34",
                warning: "#e5c07b",
                warning_foreground: "#282c34",
                scrollbar_thumb: "#4b5263",
            },
            Self::OneLight => Palette {
                background: "#fafafa",
                surface: "#f0f0f1",
                sidebar: "#f0f0f1",
                outer: "#e7e7e8",
                foreground: "#383a42",
                subtext_foreground: "#696c77",
                muted: "#f0f0f1",
                muted_foreground: "#a0a1a7",
                border: "#d4d4d5",
                primary: "#4078f2",
                primary_foreground: "#ffffff",
                primary_hover: "#526fff",
                primary_active: "#4078f2",
                accent: "#e5e5e6",
                accent_foreground: "#383a42",
                active: "#4078f222",
                hover: "#d4d4d599",
                ring: "#a626a4",
                emotion: "#e45649",
                emotion_foreground: "#ffffff",
                info: "#4078f2",
                info_foreground: "#ffffff",
                success: "#50a14f",
                success_foreground: "#ffffff",
                warning: "#c18401",
                warning_foreground: "#383a42",
                scrollbar_thumb: "#c8c8c9",
            },
            Self::GruvboxLight => Palette {
                background: "#fbf1c7",
                surface: "#f2e5bc",
                sidebar: "#f2e5bc",
                outer: "#d5c4a1",
                foreground: "#654735",
                subtext_foreground: "#7c6f64",
                muted: "#f2e5bc",
                muted_foreground: "#928374",
                border: "#d5c4a1",
                primary: "#af3a03",
                primary_foreground: "#fbf1c7",
                primary_hover: "#b57614",
                primary_active: "#af3a03",
                accent: "#ebdbb2",
                accent_foreground: "#654735",
                active: "#af3a0322",
                hover: "#d5c4a199",
                ring: "#427b58",
                emotion: "#9d0006",
                emotion_foreground: "#fbf1c7",
                info: "#458588",
                info_foreground: "#fbf1c7",
                success: "#98971a",
                success_foreground: "#fbf1c7",
                warning: "#d79921",
                warning_foreground: "#282828",
                scrollbar_thumb: "#bdae93",
            },
            Self::GruvboxDark => Palette {
                background: "#282828",
                surface: "#1d2021",
                sidebar: "#1d2021",
                outer: "#1d2021",
                foreground: "#ebdbb2",
                subtext_foreground: "#d5c4a1",
                muted: "#1d2021",
                muted_foreground: "#928374",
                border: "#504945",
                primary: "#fabd2f",
                primary_foreground: "#282828",
                primary_hover: "#fe8019",
                primary_active: "#fabd2f",
                accent: "#3c3836",
                accent_foreground: "#ebdbb2",
                active: "#fabd2f22",
                hover: "#50494599",
                ring: "#83a598",
                emotion: "#fb4934",
                emotion_foreground: "#282828",
                info: "#83a598",
                info_foreground: "#282828",
                success: "#b8bb26",
                success_foreground: "#282828",
                warning: "#fabd2f",
                warning_foreground: "#282828",
                scrollbar_thumb: "#665c54",
            },
            Self::DraculaLight => Palette {
                background: "#f8f8f2",
                surface: "#f0f0e8",
                sidebar: "#f0f0e8",
                outer: "#deded5",
                foreground: "#424450",
                subtext_foreground: "#6272a4",
                muted: "#f0f0e8",
                muted_foreground: "#858585",
                border: "#c8c8be",
                primary: "#644bcb",
                primary_foreground: "#ffffff",
                primary_hover: "#7959e6",
                primary_active: "#644bcb",
                accent: "#e6e6dc",
                accent_foreground: "#424450",
                active: "#644bcb22",
                hover: "#c8c8be99",
                ring: "#ff79c6",
                emotion: "#d12f1b",
                emotion_foreground: "#ffffff",
                info: "#036a96",
                info_foreground: "#1f1f1f",
                success: "#14710a",
                success_foreground: "#1f1f1f",
                warning: "#846e15",
                warning_foreground: "#1f1f1f",
                scrollbar_thumb: "#b8b8ad",
            },
            Self::DraculaDark => Palette {
                background: "#282a36",
                surface: "#21222c",
                sidebar: "#21222c",
                outer: "#191a21",
                foreground: "#f8f8f2",
                subtext_foreground: "#bfc0c5",
                muted: "#21222c",
                muted_foreground: "#6272a4",
                border: "#44475a",
                primary: "#bd93f9",
                primary_foreground: "#282a36",
                primary_hover: "#d6b4ff",
                primary_active: "#bd93f9",
                accent: "#44475a",
                accent_foreground: "#f8f8f2",
                active: "#bd93f922",
                hover: "#6272a499",
                ring: "#ff79c6",
                emotion: "#ff5555",
                emotion_foreground: "#282a36",
                info: "#8be9fd",
                info_foreground: "#282a36",
                success: "#50fa7b",
                success_foreground: "#282a36",
                warning: "#ffb86c",
                warning_foreground: "#282a36",
                scrollbar_thumb: "#6272a4",
            },
        }
    }
}

pub struct Palette {
    pub background: &'static str,
    pub surface: &'static str,
    pub sidebar: &'static str,
    pub outer: &'static str,
    pub foreground: &'static str,
    pub subtext_foreground: &'static str,
    pub muted: &'static str,
    pub muted_foreground: &'static str,
    pub border: &'static str,
    pub primary: &'static str,
    pub primary_foreground: &'static str,
    pub primary_hover: &'static str,
    pub primary_active: &'static str,
    pub accent: &'static str,
    pub accent_foreground: &'static str,
    pub active: &'static str,
    pub hover: &'static str,
    pub ring: &'static str,
    pub emotion: &'static str,
    pub emotion_foreground: &'static str,
    pub info: &'static str,
    pub info_foreground: &'static str,
    pub success: &'static str,
    pub success_foreground: &'static str,
    pub warning: &'static str,
    pub warning_foreground: &'static str,
    pub scrollbar_thumb: &'static str,
}

pub(crate) fn apply(
    light_theme: ColorTheme,
    dark_theme: ColorTheme,
    mode: ThemeMode,
    follow_system: bool,
    fonts: &AppFonts,
    window: Option<&mut Window>,
    cx: &mut App,
) {
    let light_config = Rc::new(theme_config(
        light_theme.label(),
        light_theme.palette(),
        ThemeMode::Light,
        fonts,
    ));
    let dark_config = Rc::new(theme_config(
        dark_theme.label(),
        dark_theme.palette(),
        ThemeMode::Dark,
        fonts,
    ));
    let theme = Theme::global_mut(cx);
    theme.light_theme = light_config;
    theme.dark_theme = dark_config;
    cx.set_window_appearance(if follow_system {
        None
    } else {
        Some(match mode {
            ThemeMode::Light => WindowAppearance::Light,
            ThemeMode::Dark => WindowAppearance::Dark,
        })
    });
    Theme::change(mode, window, cx);
    Theme::global_mut(cx).list.active_highlight = false;
    let notification = &mut Theme::global_mut(cx).notification;
    notification.placement = Anchor::BottomCenter;
}

fn theme_config(
    name: &'static str,
    palette: Palette,
    mode: ThemeMode,
    fonts: &AppFonts,
) -> ThemeConfig {
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
    colors.info = Some(palette.info.into());
    colors.info_foreground = Some(palette.info_foreground.into());
    colors.info_hover = Some(palette.info.into());
    colors.info_active = Some(palette.info.into());
    colors.success = Some(palette.success.into());
    colors.success_foreground = Some(palette.success_foreground.into());
    colors.success_hover = Some(palette.success.into());
    colors.success_active = Some(palette.success.into());
    colors.warning = Some(palette.warning.into());
    colors.warning_foreground = Some(palette.warning_foreground.into());
    colors.warning_hover = Some(palette.warning.into());
    colors.warning_active = Some(palette.warning.into());
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
    colors.sidebar_border = Some(palette.border.into());
    colors.sidebar_primary = Some(palette.primary.into());
    colors.sidebar_primary_foreground = Some(palette.primary_foreground.into());
    colors.list = Some(palette.sidebar.into());
    colors.list_active = Some(palette.active.into());
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
        mode,
        font_size: Some(14.),
        font_family: Some(fonts.ui.family.clone()),
        mono_font_family: Some(fonts.monospace.family.clone()),
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

    #[test]
    fn legacy_neutral_theme_migrates_to_catppuccin_latte() {
        let restored: ColorTheme =
            serde_json::from_str("\"lyrune-neutral\"").expect("deserialize legacy theme");
        assert_eq!(restored, ColorTheme::CatppuccinLatte);
    }

    #[test]
    fn font_chain_skips_missing_families_and_preserves_order() {
        let configured = vec![
            "Missing Font".to_owned(),
            "inter".to_owned(),
            "Noto Sans CJK SC".to_owned(),
        ];
        let available = vec!["Inter".to_owned(), "Noto Sans CJK SC".to_owned()];
        let resolved = resolve_font_chain(&configured, ".SystemUIFont", &available);

        assert_eq!(resolved.family.as_ref(), "Inter");
        assert_eq!(
            resolved
                .fallbacks
                .expect("configured fallback")
                .fallback_list(),
            ["Noto Sans CJK SC"]
        );
    }
}
