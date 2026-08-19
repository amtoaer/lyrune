use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use gpui::{AnyElement, Hsla, Image, ImageFormat, IntoElement as _, Pixels, Styled as _, img};

use crate::design::ColorTheme;

static LYRUNE_ICONS: LazyLock<[Arc<Image>; ColorTheme::ALL.len()]> = LazyLock::new(|| {
    ColorTheme::ALL.map(|theme| {
        Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            themed_lyrune_svg(theme),
        ))
    })
});

static MEDIA_ICONS: LazyLock<Mutex<HashMap<(MediaIcon, u32), Arc<Image>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn themed_lyrune_svg(theme: ColorTheme) -> Vec<u8> {
    let colors = theme.logo_palette();
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" role="img" aria-labelledby="title desc">
  <title id="title">Lyrune</title>
  <desc id="desc">A musical note moving forward on three smooth sound trails.</desc>
  <rect x="32" y="32" width="448" height="448" rx="112" fill="{background}"/>
  <g fill="none" stroke-linecap="round" stroke-width="25">
    <path d="M82 185h108c48 0 58-48 106-48h30" stroke="{trail_primary}"/>
    <path d="M82 248h102c50 0 62-48 112-48h30" stroke="{trail_secondary}"/>
    <path d="M82 311h116c45 0 56-48 101-48h27" stroke="{trail_tertiary}"/>
  </g>
  <path d="M307 139 421 108v70l-114 31Z" fill="{foreground}"/>
  <path d="M307 165v188" fill="none" stroke="{foreground}" stroke-width="34" stroke-linecap="round"/>
  <ellipse cx="264" cy="361" rx="61" ry="42" transform="rotate(-15 264 361)" fill="{foreground}"/>
  <path d="m249 337 38 22-38 22Z" fill="{background}"/>
  <path d="M383 219h47" fill="none" stroke="{detail}" stroke-width="25" stroke-linecap="round"/>
</svg>"#,
        background = colors.background,
        foreground = colors.foreground,
        trail_primary = colors.trail_primary,
        trail_secondary = colors.trail_secondary,
        trail_tertiary = colors.trail_tertiary,
        detail = colors.detail,
    )
    .into_bytes()
}

pub fn lyrune_icon(theme: ColorTheme, size: Pixels) -> AnyElement {
    let index = ColorTheme::ALL
        .iter()
        .position(|candidate| *candidate == theme)
        .expect("color theme is part of the built-in theme list");
    img(LYRUNE_ICONS[index].clone())
        .size(size)
        .into_any_element()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaIcon {
    Back,
    Forward,
    ChevronUp,
    ChevronDown,
    Home,
    Search,
    Music,
    Artist,
    Album,
    Playlist,
    Radar,
    Headphones,
    Play,
    Pause,
    Folder,
    Library,
    Loading,
    Shuffle,
    SkipBack,
    SkipForward,
    Repeat,
    RepeatOne,
    Settings,
    Heart,
    HeartFilled,
    Volume,
    VolumeMuted,
}

impl MediaIcon {
    fn body(self) -> &'static str {
        match self {
            Self::Back => r#"<path d="m15 18-6-6 6-6"/>"#,
            Self::Forward => r#"<path d="m9 18 6-6-6-6"/>"#,
            Self::ChevronUp => r#"<path d="m18 15-6-6-6 6"/>"#,
            Self::ChevronDown => r#"<path d="m6 9 6 6 6-6"/>"#,
            Self::Home => {
                r#"<path d="m3 11 9-8 9 8"/><path d="M5 10v10h14V10"/><path d="M9 20v-6h6v6"/>"#
            }
            Self::Search => r#"<circle cx="11" cy="11" r="7"/><path d="m20 20-4-4"/>"#,
            Self::Music => {
                r#"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"#
            }
            Self::Artist => r#"<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>"#,
            Self::Album => {
                r#"<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="2"/><path d="M12 3v7"/>"#
            }
            Self::Playlist => {
                r#"<path d="M4 6h10M4 10h10M4 14h7"/><path d="M17 5v10.5a2.5 2.5 0 1 1-2-2.45V7l5-1"/>"#
            }
            Self::Radar => {
                r#"<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="5"/><circle cx="12" cy="12" r="1"/><path d="m12 12 6.4-6.4"/>"#
            }
            Self::Headphones => {
                r#"<path d="M4 14v-2a8 8 0 0 1 16 0v2"/><path d="M18 19h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3v5a2 2 0 0 1-2 2Z"/><path d="M6 19H5a2 2 0 0 1-2-2v-5h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2Z"/>"#
            }
            Self::Play => r#"<path d="m6 4 14 8-14 8V4Z"/>"#,
            Self::Pause => r#"<path d="M8 5v14"/><path d="M16 5v14"/>"#,
            Self::Folder => {
                r#"<path d="M3 6a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6Z"/>"#
            }
            Self::Library => {
                r#"<rect width="7" height="18" x="3" y="3" rx="1"/><path d="M7 3v18"/><path d="m14.5 5.2 3.8-1a1.5 1.5 0 0 1 1.8 1.1l3.1 11.6a1.5 1.5 0 0 1-1.1 1.8l-3.8 1a1.5 1.5 0 0 1-1.8-1.1L13.4 7a1.5 1.5 0 0 1 1.1-1.8Z"/>"#
            }
            Self::Loading => r#"<path d="M21 12a9 9 0 1 1-5.2-8.2"/><path d="M21 3v6h-6"/>"#,
            Self::Shuffle => {
                r#"<path d="m18 14 4 4-4 4"/><path d="m18 2 4 4-4 4"/><path d="M2 18h1.4a4 4 0 0 0 3.5-2.1L11.1 8a4 4 0 0 1 3.5-2H22"/><path d="M2 6h1.4a4 4 0 0 1 3.5 2.1l.6 1.1"/><path d="M14.6 18H22"/>"#
            }
            Self::SkipBack => r#"<path d="M19 20 9 12l10-8v16Z"/><path d="M5 19V5"/>"#,
            Self::SkipForward => r#"<path d="m5 4 10 8-10 8V4Z"/><path d="M19 5v14"/>"#,
            Self::Repeat => {
                r#"<path d="m17 2 4 4-4 4"/><path d="M3 11V9a3 3 0 0 1 3-3h15"/><path d="m7 22-4-4 4-4"/><path d="M21 13v2a3 3 0 0 1-3 3H3"/>"#
            }
            Self::RepeatOne => {
                r#"<path d="m17 2 4 4-4 4"/><path d="M3 11V9a3 3 0 0 1 3-3h15"/><path d="m7 22-4-4 4-4"/><path d="M21 13v2a3 3 0 0 1-3 3H3"/><path d="M11 10h1v4"/>"#
            }
            Self::Settings => {
                r#"<path d="M4 6h8M16 6h4M4 12h2M10 12h10M4 18h10M18 18h2"/><circle cx="14" cy="6" r="2"/><circle cx="8" cy="12" r="2"/><circle cx="16" cy="18" r="2"/>"#
            }
            Self::Heart => {
                r#"<path d="M20.8 4.6a5.5 5.5 0 0 0-7.8 0L12 5.7l-1.1-1.1a5.5 5.5 0 0 0-7.8 7.8l1.1 1.1L12 21l7.8-7.5 1.1-1.1a5.5 5.5 0 0 0-.1-7.8Z"/>"#
            }
            Self::HeartFilled => {
                r#"<path d="M20.8 4.6a5.5 5.5 0 0 0-7.8 0L12 5.7l-1.1-1.1a5.5 5.5 0 0 0-7.8 7.8l1.1 1.1L12 21l7.8-7.5 1.1-1.1a5.5 5.5 0 0 0-.1-7.8Z" fill="currentColor"/>"#
            }
            Self::Volume => {
                r#"<path d="M11 5 6 9H2v6h4l5 4V5Z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/><path d="M19 5a10 10 0 0 1 0 14"/>"#
            }
            Self::VolumeMuted => {
                r#"<path d="M11 5 6 9H2v6h4l5 4V5Z"/><path d="m22 9-6 6"/><path d="m16 9 6 6"/>"#
            }
        }
    }
}

pub fn media_icon(icon: MediaIcon, color: &str, size: Pixels) -> AnyElement {
    let color = color
        .strip_prefix('#')
        .and_then(|color| match color.len() {
            6 => u32::from_str_radix(color, 16)
                .ok()
                .map(|color| color << 8 | 0xff),
            8 => u32::from_str_radix(color, 16).ok(),
            _ => None,
        })
        .expect("media icon colors are hexadecimal RGBA values");
    render_media_icon(icon, color, size)
}

pub fn media_icon_hsla(icon: MediaIcon, color: Hsla, size: Pixels) -> AnyElement {
    let rgba: u32 = color.to_rgb().into();
    render_media_icon(icon, rgba, size)
}

fn render_media_icon(icon: MediaIcon, color: u32, size: Pixels) -> AnyElement {
    let image = {
        let mut icons = MEDIA_ICONS.lock().expect("media icon cache lock poisoned");
        icons
            .entry((icon, color))
            .or_insert_with(|| {
                let svg = format!(
                    r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" color="#{color:08x}" stroke="#{color:08x}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{}</svg>"##,
                    icon.body()
                );
                Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()))
            })
            .clone()
    };
    img(image).size(size).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_generates_a_valid_logo() {
        for theme in ColorTheme::ALL {
            resvg::usvg::Tree::from_data(
                &themed_lyrune_svg(theme),
                &resvg::usvg::Options::default(),
            )
            .expect("parse themed Lyrune logo");
        }
    }
}
