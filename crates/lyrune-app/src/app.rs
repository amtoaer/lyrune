use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use futures_util::{
    future::{Either, select},
    pin_mut,
};
use gpui::{prelude::*, *};
use gpui_base::{
    Slider as BaseSlider, SliderIndicator, SliderThumb, SliderTrack,
    motion::{Transition, transition},
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IndexPath, ResizableState, Selectable as _, Sizable as _,
    StyledExt as _,
    avatar::Avatar,
    button::{Button, ButtonVariants as _},
    h_flex, h_resizable,
    input::{Input, InputEvent, InputState, MaskPattern, NumberInput},
    list::{List, ListEvent, ListState},
    resizable_panel,
    scroll::ScrollableElement as _,
    slider::{Slider, SliderEvent, SliderState},
    spinner::Spinner,
    table::{DataTable, TableEvent, TableState},
    v_flex,
};
use quick_xml::{Reader, escape::unescape, events::Event};
use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;
use wana_kana::{ConvertJapanese as _, IsJapaneseStr as _};

use crate::cache::{AudioCache, audio_cache_limit_bytes};
use crate::credentials::CredentialStore;
use crate::design::{self, AppFonts, ColorTheme};
use crate::http::{blurred_cover, blurred_image_source, cached_image_source};
use crate::icons::{MediaIcon, lyrune_icon, media_icon, media_icon_hsla};
use crate::library::{
    PlaylistListDelegate, TrackTableDelegate, TrackTableEvent, format_duration, playlist_cover,
};
use crate::lyrics_cache::LyricDiskCache;
#[cfg(target_os = "linux")]
use crate::mpris::{
    MprisCommand, MprisHandle, MprisLoopStatus, MprisPlaybackStatus, MprisSnapshot, MprisTrack,
};
use crate::player::{AudioPlayer, PreparedPlayback};
use crate::settings::{
    AppSettings, CdnCacheStore, LibraryCache, LyricFrameRate, PersistedLibraryView,
    PersistedPlayback, PersistedQueueContinuation, PersistedWindowSize, SettingsStore,
    default_lyric_font_families, default_monospace_font_families, default_ui_font_families,
    parse_font_families,
};
use crate::singleflight::SingleFlight;
use qqmusic_api::integration::{
    LoginEvent, PlaylistPage, ProtocolClient, QqCredential, Quality, RecommendationKind,
    SearchAlbum, SearchArtist, SearchPage, SearchResults, Track, UserPlaylist, UserPlaylistId,
    UserProfile, refresh_credential, run_qr_login,
};
#[cfg(target_os = "linux")]
use xxhash_rust::xxh3::xxh3_128;

const PAGE_SIZE: u64 = 100;
const ARTIST_PAGE_SIZE: u64 = 5;
const PROGRESS_TICK: Duration = Duration::from_millis(250);
const PLAYBACK_PERSIST_INTERVAL: Duration = Duration::from_secs(5);
const CDN_REFRESH_RETRY: Duration = Duration::from_secs(60);
const LIBRARY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const LYRIC_CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const PLAYER_BAR_HEIGHT: f32 = 112.;
const LYRIC_ROW_HEIGHT: f32 = 104.;
const LYRIC_EDGE_FADE_DISTANCE: f32 = 48.;
const LYRIC_SCROLL_DURATION: Duration = Duration::from_millis(360);
const LYRIC_STYLE_DURATION: Duration = Duration::from_millis(240);
const LYRIC_TRACK_SWITCH_DURATION: Duration = Duration::from_millis(420);
const LYRIC_BACKGROUND_OVERLAY_OPACITY: f32 = 0.5;
const LYRIC_MINIMUM_CONTRAST: f32 = 5.;
const LYRIC_HORIZONTAL_ANCHOR: f32 = 0.42;
const LYRIC_HORIZONTAL_STEP: f32 = 0.5;
const TRANSLATION_ALIGNMENT_TOLERANCE: Duration = Duration::from_millis(500);

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

fn contrast_ratio(first: Rgba, second: Rgba) -> f32 {
    let first = relative_luminance(first);
    let second = relative_luminance(second);
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

fn interpolate_color(from: Hsla, to: Hsla, progress: f32) -> Hsla {
    let from = from.to_rgb();
    let to = to.to_rgb();
    let progress = progress.clamp(0., 1.);
    Rgba {
        r: from.r + (to.r - from.r) * progress,
        g: from.g + (to.g - from.g) * progress,
        b: from.b + (to.b - from.b) * progress,
        a: from.a + (to.a - from.a) * progress,
    }
    .into()
}

fn readable_lyric_color(sampled_rgb: [f32; 3], overlay: Hsla, preferred: Hsla) -> Hsla {
    let overlay = overlay.to_rgb();
    let background = Rgba {
        r: sampled_rgb[0] * (1. - overlay.a) + overlay.r * overlay.a,
        g: sampled_rgb[1] * (1. - overlay.a) + overlay.g * overlay.a,
        b: sampled_rgb[2] * (1. - overlay.a) + overlay.b * overlay.a,
        a: 1.,
    };
    let mut preferred = preferred.to_rgb();
    preferred.a = 1.;
    if contrast_ratio(preferred, background) >= LYRIC_MINIMUM_CONTRAST {
        return preferred.into();
    }

    let black = black().to_rgb();
    let white = white().to_rgb();
    let neutral = if contrast_ratio(black, background) >= contrast_ratio(white, background) {
        black
    } else {
        white
    };
    let mut lower = 0.;
    let mut upper = 1.;
    for _ in 0..12 {
        let amount = (lower + upper) / 2.;
        let candidate = Rgba {
            r: preferred.r + (neutral.r - preferred.r) * amount,
            g: preferred.g + (neutral.g - preferred.g) * amount,
            b: preferred.b + (neutral.b - preferred.b) * amount,
            a: 1.,
        };
        if contrast_ratio(candidate, background) >= LYRIC_MINIMUM_CONTRAST {
            upper = amount;
        } else {
            lower = amount;
        }
    }

    Rgba {
        r: preferred.r + (neutral.r - preferred.r) * upper,
        g: preferred.g + (neutral.g - preferred.g) * upper,
        b: preferred.b + (neutral.b - preferred.b) * upper,
        a: 1.,
    }
    .into()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LyricWord {
    range: Range<usize>,
    start: Duration,
    end: Duration,
    ruby: Option<SharedString>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LyricLine {
    start: Duration,
    text: String,
    words: Vec<LyricWord>,
    translation: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LyricLayoutStyle {
    Normal,
    Active,
}

#[derive(Clone, Debug)]
struct PreparedLyricFragment {
    layout: Arc<LineLayout>,
    ruby_layout: Option<Arc<LineLayout>>,
    timing: Option<(Duration, Duration)>,
}

#[derive(Clone, Debug)]
struct PreparedLyricLine {
    fragments: Vec<PreparedLyricFragment>,
    font_size: Pixels,
    line_height: Pixels,
    ruby_line_height: Pixels,
    has_word_timing: bool,
}

#[derive(Clone, Debug)]
struct PreparedLyricTranslation {
    layout: Arc<LineLayout>,
    line_height: Pixels,
}

#[derive(Debug, PartialEq, Eq)]
struct LyricLayoutCacheSource {
    lyrics: usize,
    mid: String,
    compact: bool,
    narrow: bool,
    font: Font,
}

struct CachedLyricRow {
    normal: Option<Arc<PreparedLyricLine>>,
    active: Option<Arc<PreparedLyricLine>>,
    translation: Option<Arc<PreparedLyricTranslation>>,
    translation_prepared: bool,
}

#[derive(Default)]
struct LyricLayoutCache {
    source: Option<LyricLayoutCacheSource>,
    rows: Vec<CachedLyricRow>,
}

struct PreparedLyricRow {
    normal: Arc<PreparedLyricLine>,
    active: Arc<PreparedLyricLine>,
    translation: Option<Arc<PreparedLyricTranslation>>,
    emphasis: f32,
    opacity: f32,
    current: bool,
    estimated_line_progress: Option<f32>,
}

struct PreparedLyricsElement {
    rows: Vec<PreparedLyricRow>,
    foreground: Hsla,
    position: Duration,
    translation_line_height: Pixels,
}

struct LyricMotionState {
    mid: String,
    scroll_from: f32,
    style_from: f32,
    target: f32,
    started_at: Instant,
}

impl LyricMotionState {
    fn progress(&self, duration: Duration, now: Instant) -> f32 {
        if duration.is_zero() {
            return 1.;
        }
        (now.saturating_duration_since(self.started_at).as_secs_f32() / duration.as_secs_f32())
            .clamp(0., 1.)
    }

    fn sample(&self, from: f32, progress: f32) -> f32 {
        if from == self.target || progress >= 1. {
            return self.target;
        }
        from + (self.target - from) * progress
    }

    fn scroll_anchor(&self, now: Instant) -> f32 {
        let progress = self.progress(LYRIC_SCROLL_DURATION, now);
        self.sample(self.scroll_from, 1. - (1. - progress).powi(3))
    }

    fn style_anchor(&self, now: Instant) -> f32 {
        let progress = self.progress(LYRIC_STYLE_DURATION, now);
        self.sample(self.style_from, progress * progress * (3. - 2. * progress))
    }
}

#[cfg(test)]
impl LyricWord {
    fn highlight_progress(&self, position: Duration) -> f32 {
        lyric_highlight_progress(self.start, self.end, position)
    }
}

fn lyric_highlight_progress(start: Duration, end: Duration, position: Duration) -> f32 {
    if position <= start {
        return 0.;
    }
    if position >= end || end <= start {
        return 1.;
    }

    position.saturating_sub(start).as_secs_f32() / end.saturating_sub(start).as_secs_f32()
}

fn adjacent_lyric_timing(
    previous: Option<(Duration, Duration)>,
    next: Option<(Duration, Duration)>,
    line_start: Duration,
    line_end: Duration,
) -> (Duration, Duration) {
    match (previous, next) {
        (Some((_, previous_end)), Some((next_start, _))) => {
            (previous_end, next_start.max(previous_end))
        }
        (Some((_, previous_end)), None) => (previous_end, line_end.max(previous_end)),
        (None, Some((next_start, _))) => (line_start, next_start.max(line_start)),
        (None, None) => (line_start, line_end.max(line_start)),
    }
}

fn lyric_horizontal_scroll_offset(
    line_width: Pixels,
    viewport_width: Pixels,
    playhead_x: Pixels,
) -> Pixels {
    let overflow = (line_width - viewport_width).max(px(0.));
    let offset = (playhead_x - viewport_width * LYRIC_HORIZONTAL_ANCHOR)
        .max(px(0.))
        .min(overflow);
    px((f32::from(offset) / LYRIC_HORIZONTAL_STEP).round() * LYRIC_HORIZONTAL_STEP)
}

fn combined_lyric_frame_interval(
    highlight: LyricFrameRate,
    scroll: LyricFrameRate,
) -> Option<Duration> {
    match (highlight.frame_interval(), scroll.frame_interval()) {
        (Some(highlight), Some(scroll)) => Some(highlight.min(scroll)),
        _ => None,
    }
}

fn lyric_position_for_frame_rate(position: Duration, frame_rate: LyricFrameRate) -> Duration {
    let Some(interval) = frame_rate.frame_interval() else {
        return position;
    };
    let interval_nanos = interval.as_nanos();
    let quantized_nanos = position.as_nanos() / interval_nanos * interval_nanos;
    Duration::from_nanos(quantized_nanos.min(u64::MAX as u128) as u64)
}

fn lyric_frame_is_due(
    now: Instant,
    frame_rate: LyricFrameRate,
    next_frame: &mut Option<Instant>,
) -> bool {
    let Some(interval) = frame_rate.frame_interval() else {
        *next_frame = None;
        return true;
    };
    let Some(deadline) = *next_frame else {
        *next_frame = Some(now + interval);
        return false;
    };
    if now < deadline {
        return false;
    }

    let delay = now.saturating_duration_since(deadline);
    *next_frame = Some(if delay < interval {
        deadline + interval
    } else {
        now + interval
    });
    true
}

fn lyric_line_opacity(anchor: usize, index: usize) -> f32 {
    match index.abs_diff(anchor) {
        0 => 1.,
        1 => 0.72,
        2 => 0.48,
        3 => 0.3,
        _ => 0.16,
    }
}

fn interpolated_lyric_line_opacity(anchor: f32, index: usize) -> f32 {
    let previous = anchor.floor() as usize;
    let next = anchor.ceil() as usize;
    let progress = anchor - previous as f32;
    let from = lyric_line_opacity(previous, index);
    from + (lyric_line_opacity(next, index) - from) * progress
}

fn lyric_edge_opacity(
    row_top: Pixels,
    row_bottom: Pixels,
    viewport_top: Pixels,
    viewport_bottom: Pixels,
) -> f32 {
    let top = (f32::from(row_top - viewport_top) / LYRIC_EDGE_FADE_DISTANCE).clamp(0., 1.);
    let bottom = (f32::from(viewport_bottom - row_bottom) / LYRIC_EDGE_FADE_DISTANCE).clamp(0., 1.);
    top.min(bottom)
}

impl PreparedLyricLine {
    fn new(
        line: &LyricLine,
        line_end: Duration,
        style: LyricLayoutStyle,
        compact: bool,
        narrow: bool,
        configured_font: &Font,
        window: &Window,
    ) -> Self {
        let active = style == LyricLayoutStyle::Active;
        let font_size = match (active, compact) {
            (true, true) => px(24.),
            (true, false) => px(28.),
            (false, true) => px(17.),
            (false, false) => px(19.),
        };
        let ruby_font_size = if narrow { px(11.) } else { px(12.) };
        let ruby_line_height = if narrow { px(13.) } else { px(15.) };
        let mut font = configured_font.clone();
        font.weight = if active {
            FontWeight::BOLD
        } else {
            FontWeight::MEDIUM
        };
        let has_word_timing = !line.words.is_empty();
        let has_ruby = line.words.iter().any(|word| word.ruby.is_some());
        let split_fragments = (active && has_word_timing) || has_ruby;
        let mut fragments = Vec::with_capacity(if split_fragments {
            line.words.len() * 2 + 1
        } else {
            1
        });

        let mut push_fragment =
            |text: &str, ruby: Option<&SharedString>, timing: Option<(Duration, Duration)>| {
                if text.is_empty() {
                    return;
                }
                let run = TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color: black(),
                    ..Default::default()
                };
                let layout = window
                    .text_system()
                    .layout_line(text, font_size, &[run], None);
                let ruby_layout = ruby.map(|ruby| {
                    let run = TextRun {
                        len: ruby.len(),
                        font: font.clone(),
                        color: black(),
                        ..Default::default()
                    };
                    window
                        .text_system()
                        .layout_line(ruby, ruby_font_size, &[run], None)
                });
                fragments.push(PreparedLyricFragment {
                    layout,
                    ruby_layout,
                    timing,
                });
            };

        if split_fragments {
            let mut cursor = 0;
            let mut previous_timing = None;
            for word in &line.words {
                let timing = (word.start, word.end);
                if cursor < word.range.start {
                    push_fragment(
                        &line.text[cursor..word.range.start],
                        None,
                        active.then_some(adjacent_lyric_timing(
                            previous_timing,
                            Some(timing),
                            line.start,
                            line_end,
                        )),
                    );
                }
                push_fragment(
                    &line.text[word.range.clone()],
                    word.ruby.as_ref(),
                    active.then_some(timing),
                );
                cursor = word.range.end;
                previous_timing = Some(timing);
            }
            if cursor < line.text.len() {
                push_fragment(
                    &line.text[cursor..],
                    None,
                    active.then_some(adjacent_lyric_timing(
                        previous_timing,
                        None,
                        line.start,
                        line_end,
                    )),
                );
            }
        } else {
            push_fragment(&line.text, None, None);
        }

        Self {
            fragments,
            font_size,
            line_height: font_size * 1.3,
            ruby_line_height,
            has_word_timing,
        }
    }

    fn height(&self) -> Pixels {
        self.ruby_line_height + self.line_height
    }

    fn fragment_width(fragment: &PreparedLyricFragment, scale: f32) -> Pixels {
        let text_width = fragment.layout.width * scale;
        fragment
            .ruby_layout
            .as_ref()
            .map_or(text_width, |ruby| text_width.max(ruby.width))
    }

    fn width(&self, scale: f32) -> Pixels {
        self.fragments.iter().fold(px(0.), |width, fragment| {
            width + Self::fragment_width(fragment, scale)
        })
    }

    fn timed_playhead_x(&self, position: Duration, scale: f32) -> Option<Pixels> {
        let mut x = px(0.);
        let mut last_timed_end = None;

        for fragment in &self.fragments {
            let text_width = fragment.layout.width * scale;
            let fragment_width = Self::fragment_width(fragment, scale);
            if let Some((start, end)) = fragment.timing {
                let text_start = x + (fragment_width - text_width) / 2.;
                let text_end = text_start + text_width;
                if position < start {
                    return last_timed_end.or(Some(text_start));
                }
                if position < end {
                    return Some(
                        text_start + text_width * lyric_highlight_progress(start, end, position),
                    );
                }
                last_timed_end = Some(text_end);
            }
            x += fragment_width;
        }

        last_timed_end
    }
}

impl PreparedLyricTranslation {
    fn new(text: &str, narrow: bool, configured_font: &Font, window: &Window) -> Self {
        let font_size = if narrow { px(13.) } else { px(14.) };
        let line_height = if narrow { px(18.) } else { px(20.) };
        let mut font = configured_font.clone();
        font.weight = FontWeight::MEDIUM;
        let run = TextRun {
            len: text.len(),
            font,
            color: black(),
            ..Default::default()
        };
        Self {
            layout: window
                .text_system()
                .layout_line(text, font_size, &[run], None),
            line_height,
        }
    }
}

impl LyricLayoutCache {
    fn reset_if_needed(
        &mut self,
        lyrics: &Arc<ParsedLyrics>,
        mid: &str,
        compact: bool,
        narrow: bool,
        font: &Font,
    ) {
        let lyrics_identity = Arc::as_ptr(lyrics) as usize;
        let matches = self.source.as_ref().is_some_and(|source| {
            source.lyrics == lyrics_identity
                && source.mid == mid
                && source.compact == compact
                && source.narrow == narrow
                && source.font == *font
        });
        if matches {
            return;
        }

        self.source = Some(LyricLayoutCacheSource {
            lyrics: lyrics_identity,
            mid: mid.to_owned(),
            compact,
            narrow,
            font: font.clone(),
        });
        self.rows = (0..lyrics.lines.len())
            .map(|_| CachedLyricRow {
                normal: None,
                active: None,
                translation: None,
                translation_prepared: false,
            })
            .collect();
    }

    fn line(
        &mut self,
        index: usize,
        lyric: &LyricLine,
        line_end: Duration,
        style: LyricLayoutStyle,
        compact: bool,
        narrow: bool,
        font: &Font,
        window: &Window,
    ) -> Arc<PreparedLyricLine> {
        let cached = match style {
            LyricLayoutStyle::Normal => &mut self.rows[index].normal,
            LyricLayoutStyle::Active => &mut self.rows[index].active,
        };
        cached
            .get_or_insert_with(|| {
                Arc::new(PreparedLyricLine::new(
                    lyric, line_end, style, compact, narrow, font, window,
                ))
            })
            .clone()
    }

    fn translation(
        &mut self,
        index: usize,
        lyric: &LyricLine,
        narrow: bool,
        font: &Font,
        window: &Window,
    ) -> Option<Arc<PreparedLyricTranslation>> {
        let row = &mut self.rows[index];
        if !row.translation_prepared {
            row.translation = lyric.translation.as_deref().map(|translation| {
                Arc::new(PreparedLyricTranslation::new(
                    translation,
                    narrow,
                    font,
                    window,
                ))
            });
            row.translation_prepared = true;
        }
        row.translation.clone()
    }
}

impl PreparedLyricsElement {
    fn paint_layout(
        layout: &LineLayout,
        origin: Point<Pixels>,
        line_height: Pixels,
        color: Hsla,
        scale: f32,
        window: &mut Window,
    ) -> anyhow::Result<()> {
        let baseline = (line_height / scale - layout.ascent - layout.descent) / 2. + layout.ascent;
        let scale_factor = window.scale_factor();
        let transformation = TransformationMatrix::unit()
            .translate(origin.scale(scale_factor))
            .scale(size(scale, scale))
            .translate(origin.scale(-scale_factor));
        window.paint_layer(
            Bounds::new(origin, size(layout.width * scale, line_height)),
            |window| {
                for run in &layout.runs {
                    for glyph in &run.glyphs {
                        if glyph.is_emoji {
                            window.paint_emoji(
                                point(
                                    origin.x + glyph.position.x * scale,
                                    origin.y + (baseline + glyph.position.y) * scale,
                                ),
                                run.font_id,
                                glyph.id,
                                layout.font_size * scale,
                            )?;
                        } else {
                            window.paint_glyph_with_transformation(
                                point(
                                    origin.x + glyph.position.x,
                                    origin.y + baseline + glyph.position.y,
                                ),
                                run.font_id,
                                glyph.id,
                                layout.font_size,
                                transformation,
                                color,
                            )?;
                        }
                    }
                }
                Ok(())
            },
        )
    }

    fn paint_line(
        line: &PreparedLyricLine,
        origin: Point<Pixels>,
        bounds: Bounds<Pixels>,
        style: LyricLayoutStyle,
        opacity: f32,
        target_font_size: Pixels,
        foreground: Hsla,
        position: Duration,
        window: &mut Window,
    ) -> anyhow::Result<()> {
        if opacity <= 0.001 {
            return Ok(());
        }

        let scale = f32::from(target_font_size) / f32::from(line.font_size);
        let line_height = line.line_height * scale;
        let height = line.ruby_line_height + line_height;
        let origin = point(origin.x, origin.y + (bounds.size.height - height) / 2.);
        let base_origin_y = origin.y + line.ruby_line_height;
        let base_color = foreground.opacity(
            opacity
                * if style == LyricLayoutStyle::Active && line.has_word_timing {
                    0.42
                } else {
                    1.
                },
        );
        let highlight_color = foreground.opacity(opacity);
        let ruby_color = foreground.opacity(opacity * 0.78);
        let mut x = origin.x;

        for fragment in &line.fragments {
            let text_width = fragment.layout.width * scale;
            let fragment_width = fragment
                .ruby_layout
                .as_ref()
                .map_or(text_width, |ruby| text_width.max(ruby.width));
            let text_origin = point(x + (fragment_width - text_width) / 2., base_origin_y);
            if style == LyricLayoutStyle::Active && line.has_word_timing {
                if let Some((start, end)) = fragment.timing {
                    let progress = lyric_highlight_progress(start, end, position);
                    if progress <= 0. {
                        Self::paint_layout(
                            &fragment.layout,
                            text_origin,
                            line_height,
                            base_color,
                            scale,
                            window,
                        )?;
                    } else if progress >= 1. {
                        Self::paint_layout(
                            &fragment.layout,
                            text_origin,
                            line_height,
                            highlight_color,
                            scale,
                            window,
                        )?;
                    } else {
                        Self::paint_layout(
                            &fragment.layout,
                            text_origin,
                            line_height,
                            base_color,
                            scale,
                            window,
                        )?;
                        let highlight_bounds = Bounds::new(
                            point(text_origin.x, bounds.top()),
                            size(text_width * progress, bounds.size.height),
                        );
                        window.with_content_mask(
                            Some(ContentMask {
                                bounds: highlight_bounds,
                            }),
                            |window| {
                                Self::paint_layout(
                                    &fragment.layout,
                                    text_origin,
                                    line_height,
                                    highlight_color,
                                    scale,
                                    window,
                                )
                            },
                        )?;
                    }
                } else {
                    Self::paint_layout(
                        &fragment.layout,
                        text_origin,
                        line_height,
                        base_color,
                        scale,
                        window,
                    )?;
                }
            } else {
                Self::paint_layout(
                    &fragment.layout,
                    text_origin,
                    line_height,
                    highlight_color,
                    scale,
                    window,
                )?;
            }

            if let Some(ruby) = &fragment.ruby_layout {
                Self::paint_layout(
                    ruby,
                    point(x + (fragment_width - ruby.width) / 2., origin.y),
                    line.ruby_line_height,
                    ruby_color,
                    1.,
                    window,
                )?;
            }
            x += fragment_width;
        }
        Ok(())
    }
}

impl IntoElement for PreparedLyricsElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PreparedLyricsElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(self.rows.len() as f32 * LYRIC_ROW_HEIGHT).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let viewport_bounds = window.content_mask().bounds;
        let visible_bounds = bounds.intersect(&viewport_bounds);
        if visible_bounds.is_empty() {
            return;
        }
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (index, row) in self.rows.iter().enumerate() {
                let emphasis = row.emphasis.clamp(0., 1.);
                let target_font_size =
                    row.normal.font_size + (row.active.font_size - row.normal.font_size) * emphasis;
                let horizontal_offset = if row.current {
                    let scale = f32::from(target_font_size) / f32::from(row.active.font_size);
                    let line_width = row.active.width(scale);
                    let playhead_x =
                        row.active
                            .timed_playhead_x(self.position, scale)
                            .or_else(|| {
                                row.estimated_line_progress
                                    .map(|progress| line_width * progress)
                            });
                    playhead_x.map_or(px(0.), |playhead_x| {
                        lyric_horizontal_scroll_offset(line_width, bounds.size.width, playhead_x)
                    })
                } else {
                    px(0.)
                };
                let row_bounds = Bounds::new(
                    point(
                        bounds.origin.x,
                        bounds.origin.y + px(index as f32 * LYRIC_ROW_HEIGHT),
                    ),
                    size(bounds.size.width, px(LYRIC_ROW_HEIGHT)),
                );
                if !row_bounds.intersects(&visible_bounds) {
                    continue;
                }
                let edge_opacity = lyric_edge_opacity(
                    row_bounds.top(),
                    row_bounds.bottom(),
                    viewport_bounds.top(),
                    viewport_bounds.bottom(),
                );
                if edge_opacity <= 0.001 {
                    continue;
                }
                let opacity = row.opacity * edge_opacity;
                let lyric_height = row.normal.height().max(row.active.height());
                let content_height = lyric_height + px(4.) + self.translation_line_height;
                let lyric_bounds = Bounds::new(
                    point(
                        row_bounds.origin.x,
                        row_bounds.origin.y + (row_bounds.size.height - content_height) / 2.,
                    ),
                    size(row_bounds.size.width, lyric_height),
                );
                let _ = Self::paint_line(
                    &row.normal,
                    point(
                        lyric_bounds.origin.x - horizontal_offset,
                        lyric_bounds.origin.y,
                    ),
                    lyric_bounds,
                    LyricLayoutStyle::Normal,
                    opacity * (1. - emphasis),
                    target_font_size,
                    self.foreground,
                    self.position,
                    window,
                );
                let _ = Self::paint_line(
                    &row.active,
                    point(
                        lyric_bounds.origin.x - horizontal_offset,
                        lyric_bounds.origin.y,
                    ),
                    lyric_bounds,
                    LyricLayoutStyle::Active,
                    opacity * emphasis,
                    target_font_size,
                    self.foreground,
                    self.position,
                    window,
                );
                if let Some(translation) = &row.translation {
                    let _ = Self::paint_layout(
                        &translation.layout,
                        point(row_bounds.origin.x, lyric_bounds.bottom() + px(4.)),
                        translation.line_height,
                        self.foreground.opacity(opacity * 0.72),
                        1.,
                        window,
                    );
                }
            }
        });
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ParsedLyrics {
    lines: Vec<LyricLine>,
}

#[derive(Clone)]
struct MemoryLyrics {
    parsed: Arc<ParsedLyrics>,
    fetched_at_secs: u64,
}

impl MemoryLyrics {
    fn is_fresh(&self, now_secs: u64) -> bool {
        now_secs.saturating_sub(self.fetched_at_secs) < LYRIC_CACHE_TTL.as_secs()
    }
}

enum LyricLoadEvent {
    Disk {
        lyrics: MemoryLyrics,
        fresh: bool,
    },
    Network {
        result: anyhow::Result<MemoryLyrics>,
        had_cached: bool,
    },
}

impl ParsedLyrics {
    fn active_index(&self, position: Duration) -> Option<usize> {
        self.lines
            .partition_point(|line| line.start <= position)
            .checked_sub(1)
    }
}

fn parse_lyrics(original: &str, translated: Option<&str>, romanized: Option<&str>) -> ParsedLyrics {
    let mut original = parse_timed_lyrics(original);
    let translated = translated.map(parse_timed_lyrics).unwrap_or_default();
    if original.is_empty() {
        original = translated;
        return ParsedLyrics { lines: original };
    }

    align_lyric_lines(&mut original, &translated, |line, translation| {
        line.translation = provider_translation(&translation.text);
    });

    if original.iter().any(|line| {
        line.text
            .chars()
            .any(|character| matches!(character, '\u{3040}'..='\u{30ff}'))
    }) {
        let romanized = romanized.map(parse_timed_lyrics).unwrap_or_default();
        align_lyric_lines(&mut original, &romanized, attach_ruby);
    }

    ParsedLyrics { lines: original }
}

fn align_lyric_lines(
    original: &mut [LyricLine],
    auxiliary: &[LyricLine],
    mut merge: impl FnMut(&mut LyricLine, &LyricLine),
) {
    if original.len() == auxiliary.len() {
        for (line, auxiliary) in original.iter_mut().zip(auxiliary) {
            merge(line, auxiliary);
        }
        return;
    }

    let mut search_from = 0;
    for line in original {
        let Some((offset, auxiliary)) = auxiliary[search_from..]
            .iter()
            .enumerate()
            .take_while(|(_, auxiliary)| {
                auxiliary.start <= line.start.saturating_add(TRANSLATION_ALIGNMENT_TOLERANCE)
            })
            .filter(|(_, auxiliary)| {
                line.start.abs_diff(auxiliary.start) <= TRANSLATION_ALIGNMENT_TOLERANCE
            })
            .min_by_key(|(_, auxiliary)| line.start.abs_diff(auxiliary.start))
        else {
            continue;
        };
        merge(line, auxiliary);
        search_from += offset + 1;
    }
}

fn attach_ruby(line: &mut LyricLine, romanized: &LyricLine) {
    let mut readings = vec![String::new(); line.words.len()];
    let mut word_index = 0;
    let mut romanized_index = 0;
    let mut best_match = None;
    while word_index < line.words.len() && romanized_index < romanized.words.len() {
        let word = &line.words[word_index];
        let romanized_word = &romanized.words[romanized_index];
        let start = word.start.max(romanized_word.start);
        let end = word.end.min(romanized_word.end);
        if start < end {
            let overlap = end - start;
            match best_match {
                Some((_, best_overlap)) if best_overlap > overlap => {}
                _ => best_match = Some((word_index, overlap)),
            }
        }

        if romanized_word.end <= word.end {
            if let Some((matched_word_index, _)) = best_match.take() {
                readings[matched_word_index]
                    .push_str(&romanized.text[romanized_word.range.clone()]);
            }
            romanized_index += 1;
        }
        if word.end <= romanized_word.end {
            word_index += 1;
        }
    }
    if let (Some((matched_word_index, _)), Some(romanized_word)) =
        (best_match, romanized.words.get(romanized_index))
    {
        readings[matched_word_index].push_str(&romanized.text[romanized_word.range.clone()]);
    }

    for (word, reading) in line.words.iter_mut().zip(readings) {
        let text = &line.text[word.range.clone()];
        if !text.contains_kanji() {
            continue;
        }

        let reading = reading
            .chars()
            .filter(|character| !character.is_whitespace() && *character != '\'')
            .collect::<String>()
            .to_hiragana();
        if !reading.is_empty()
            && reading
                .chars()
                .all(|character| matches!(character, '\u{3040}'..='\u{309f}' | 'ー'))
        {
            word.ruby = Some(reading.into());
        }
    }
}

fn provider_translation(text: &str) -> Option<String> {
    (!text.trim().is_empty() && text.trim() != "//").then(|| text.to_owned())
}

fn parse_timed_lyrics(raw: &str) -> Vec<LyricLine> {
    let Some(content) = extract_qrc_content(raw) else {
        return parse_lrc(raw);
    };
    let qrc = parse_qrc(&content);
    if qrc.is_empty() {
        parse_lrc(&content)
    } else {
        qrc
    }
}

fn parse_qrc(content: &str) -> Vec<LyricLine> {
    let mut lines = content
        .lines()
        .filter_map(|raw_line| {
            let raw_line = raw_line.trim_start_matches('\u{feff}');
            let (start, consumed) = parse_qrc_line_timestamp(raw_line)?;
            let (text, words) = parse_qrc_words(&raw_line[consumed..]);
            (!text.is_empty()).then_some(LyricLine {
                start,
                text,
                words,
                translation: None,
            })
        })
        .collect::<Vec<_>>();
    lines.sort_by_key(|line| line.start);
    lines
}

fn extract_qrc_content(raw: &str) -> Option<String> {
    if !raw.trim_start().starts_with('<') {
        return Some(raw.to_owned());
    }

    extract_qrc_content_strict(raw).or_else(|| extract_qrc_content_relaxed(raw))
}

fn extract_qrc_content_strict(raw: &str) -> Option<String> {
    let mut reader = Reader::from_str(raw);
    let mut lyric_content = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                for attribute in element.attributes() {
                    let attribute = attribute.ok()?;
                    let decoded = reader.decoder().decode(attribute.value.as_ref()).ok()?;
                    let decoded = unescape(&decoded).ok()?.into_owned();
                    if attribute.key.as_ref() == b"LyricContent" && lyric_content.is_none() {
                        lyric_content = Some(decoded);
                    }
                }
            }
            Ok(Event::Text(text)) => {
                let decoded = reader.decoder().decode(text.as_ref()).ok()?;
                unescape(&decoded).ok()?;
            }
            Ok(Event::Eof) => return lyric_content,
            Err(_) => return None,
            _ => {}
        }
    }
}

fn extract_qrc_content_relaxed(raw: &str) -> Option<String> {
    const TAG_NAME: &str = "<Lyric_1";
    let bytes = raw.as_bytes();
    let mut search_from = 0;

    while let Some(offset) = raw[search_from..].find(TAG_NAME) {
        let mut cursor = search_from + offset + TAG_NAME.len();
        if bytes
            .get(cursor)
            .is_some_and(|byte| is_xml_name_byte(*byte))
        {
            search_from = cursor;
            continue;
        }

        loop {
            skip_ascii_whitespace(bytes, &mut cursor);
            if cursor >= bytes.len() || bytes[cursor] == b'>' || bytes[cursor..].starts_with(b"/>")
            {
                break;
            }

            let name_start = cursor;
            while bytes
                .get(cursor)
                .is_some_and(|byte| is_xml_name_byte(*byte))
            {
                cursor += 1;
            }
            if cursor == name_start {
                break;
            }
            let name = &raw[name_start..cursor];

            skip_ascii_whitespace(bytes, &mut cursor);
            if bytes.get(cursor) != Some(&b'=') {
                break;
            }
            cursor += 1;
            skip_ascii_whitespace(bytes, &mut cursor);
            let Some(quote @ (b'"' | b'\'')) = bytes.get(cursor).copied() else {
                break;
            };
            cursor += 1;
            let value_start = cursor;

            if name == "LyricContent" {
                while let Some(relative_end) =
                    bytes[cursor..].iter().position(|byte| *byte == quote)
                {
                    let value_end = cursor + relative_end;
                    if qrc_attribute_tail_is_valid(&raw[value_end + 1..]) {
                        return Some(decode_qrc_entities(&raw[value_start..value_end]));
                    }
                    cursor = value_end + 1;
                }
                break;
            }

            let Some(relative_end) = bytes[cursor..].iter().position(|byte| *byte == quote) else {
                break;
            };
            cursor += relative_end + 1;
        }

        search_from = search_from + offset + TAG_NAME.len();
    }

    None
}

fn qrc_attribute_tail_is_valid(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut cursor = 0;
    loop {
        skip_ascii_whitespace(bytes, &mut cursor);
        if bytes[cursor..].starts_with(b"/>") || bytes.get(cursor) == Some(&b'>') {
            return true;
        }

        let name_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| is_xml_name_byte(*byte))
        {
            cursor += 1;
        }
        if cursor == name_start {
            return false;
        }

        skip_ascii_whitespace(bytes, &mut cursor);
        if bytes.get(cursor) != Some(&b'=') {
            return false;
        }
        cursor += 1;
        skip_ascii_whitespace(bytes, &mut cursor);
        let Some(quote @ (b'"' | b'\'')) = bytes.get(cursor).copied() else {
            return false;
        };
        cursor += 1;
        let Some(relative_end) = bytes[cursor..].iter().position(|byte| *byte == quote) else {
            return false;
        };
        cursor += relative_end + 1;
    }
}

fn skip_ascii_whitespace(bytes: &[u8], cursor: &mut usize) {
    while bytes
        .get(*cursor)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        *cursor += 1;
    }
}

fn is_xml_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'.' | b'-')
}

fn decode_qrc_entities(input: &str) -> String {
    let mut decoded = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(relative_start) = input[cursor..].find('&') {
        let entity_start = cursor + relative_start;
        decoded.push_str(&input[cursor..entity_start]);
        let Some(relative_end) = input[entity_start + 1..].find(';') else {
            decoded.push_str(&input[entity_start..]);
            return decoded;
        };
        let entity_end = entity_start + 1 + relative_end;
        let entity = &input[entity_start + 1..entity_end];
        if let Some(character) = decode_qrc_entity(entity) {
            decoded.push(character);
            cursor = entity_end + 1;
        } else {
            decoded.push('&');
            cursor = entity_start + 1;
        }
    }

    decoded.push_str(&input[cursor..]);
    decoded
}

fn decode_qrc_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "apos" => Some('\''),
        "gt" => Some('>'),
        "lt" => Some('<'),
        "quot" => Some('"'),
        _ => entity
            .strip_prefix("#x")
            .or_else(|| entity.strip_prefix("#X"))
            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
            .or_else(|| {
                entity
                    .strip_prefix('#')
                    .and_then(|digits| digits.parse::<u32>().ok())
            })
            .and_then(char::from_u32),
    }
}

fn parse_qrc_line_timestamp(input: &str) -> Option<(Duration, usize)> {
    let end = input.strip_prefix('[')?.find(']')? + 1;
    let (start, duration) = input[1..end].split_once(',')?;
    let start = start.parse::<u64>().ok()?;
    duration.parse::<u64>().ok()?;
    Some((Duration::from_millis(start), end + 1))
}

fn parse_qrc_words(input: &str) -> (String, Vec<LyricWord>) {
    let mut text = String::with_capacity(input.len());
    let mut words = Vec::new();
    let mut cursor = 0;
    while let Some(open) = input[cursor..].find('(').map(|offset| cursor + offset) {
        let Some(close) = input[open + 1..].find(')').map(|offset| open + 1 + offset) else {
            break;
        };
        if let Some((start, duration)) = parse_qrc_word_timestamp(&input[open + 1..close]) {
            let range_start = text.len();
            text.push_str(&input[cursor..open]);
            let range_end = text.len();
            if range_start < range_end {
                words.push(LyricWord {
                    range: range_start..range_end,
                    start,
                    end: start.saturating_add(duration),
                    ruby: None,
                });
            }
            cursor = close + 1;
        } else {
            text.push_str(&input[cursor..=open]);
            cursor = open + 1;
        }
    }
    text.push_str(&input[cursor..]);
    trim_lyric_text(text, words)
}

fn parse_qrc_word_timestamp(input: &str) -> Option<(Duration, Duration)> {
    let mut parts = input.split(',');
    let start = parts.next()?.parse::<u64>().ok()?;
    let duration = parts.next()?.parse::<u64>().ok()?;
    Some((
        Duration::from_millis(start),
        Duration::from_millis(duration),
    ))
}

fn trim_lyric_text(text: String, words: Vec<LyricWord>) -> (String, Vec<LyricWord>) {
    let start = text.len() - text.trim_start().len();
    let end = text.trim_end().len();
    if start >= end {
        return (String::new(), Vec::new());
    }
    if start == 0 && end == text.len() {
        return (text, words);
    }

    let words = words
        .into_iter()
        .filter_map(|word| {
            let range_start = word.range.start.clamp(start, end);
            let range_end = word.range.end.clamp(start, end);
            (range_start < range_end).then_some(LyricWord {
                range: range_start - start..range_end - start,
                start: word.start,
                end: word.end,
                ruby: word.ruby,
            })
        })
        .collect();
    (text[start..end].to_owned(), words)
}

fn parse_lrc(raw: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw_line in raw.lines() {
        let mut remainder = raw_line.trim_start_matches('\u{feff}');
        let mut timestamps = Vec::new();
        while let Some((timestamp, consumed)) = parse_lrc_timestamp(remainder) {
            timestamps.push(timestamp);
            remainder = &remainder[consumed..];
        }
        let text = remainder.trim();
        if text.is_empty() {
            continue;
        }
        lines.extend(timestamps.into_iter().map(|start| LyricLine {
            start,
            text: text.to_owned(),
            words: Vec::new(),
            translation: None,
        }));
    }
    lines.sort_by_key(|line| line.start);
    lines
}

fn parse_lrc_timestamp(input: &str) -> Option<(Duration, usize)> {
    if !input.starts_with('[') {
        return None;
    }
    let end = input.find(']')?;
    let (minutes, seconds) = input[1..end].split_once(':')?;
    let minutes = minutes.parse::<u64>().ok()?;
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let seconds = seconds.parse::<u64>().ok()?;
    if seconds >= 60 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let milliseconds = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().ok()? * 100,
        2 => fraction.parse::<u64>().ok()? * 10,
        _ => fraction[..3].parse::<u64>().ok()?,
    };
    Some((
        Duration::from_millis(
            minutes
                .saturating_mul(60_000)
                .saturating_add(seconds.saturating_mul(1_000))
                .saturating_add(milliseconds),
        ),
        end + 1,
    ))
}

fn progress_slider_state(value: f32) -> SliderState {
    SliderState::new()
        .min(0.)
        .max(1.)
        .step(0.001)
        .default_value(value)
}

fn volume_slider_state(value: f32) -> SliderState {
    SliderState::new()
        .min(0.)
        .max(1.)
        .step(0.01)
        .default_value(value)
}

fn progress_fraction(position: Duration, duration: Duration) -> f32 {
    if duration.is_zero() {
        0.
    } else {
        (position.as_secs_f32() / duration.as_secs_f32()).clamp(0., 1.)
    }
}

fn format_playback_time(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 60 * 60 {
        format!(
            "{}:{:02}:{:02}",
            seconds / (60 * 60),
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

fn playlist_title_is_long(title: &str) -> bool {
    title
        .chars()
        .map(|character| if character.is_ascii() { 1 } else { 2 })
        .sum::<usize>()
        > 24
}

#[cfg(target_os = "linux")]
fn duration_micros(duration: Duration) -> i64 {
    duration.as_micros().min(i64::MAX as u128) as i64
}

#[cfg(target_os = "linux")]
fn mpris_track_id(track_mid: &str) -> String {
    format!(
        "/dev/lyrune/track/id_{:032x}",
        xxh3_128(track_mid.as_bytes())
    )
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn new_cache_revision() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

pub(crate) static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .thread_keep_alive(Duration::from_secs(2))
        .enable_all()
        .thread_name("lyrune-worker")
        .build()
        .expect("create Lyrune Tokio runtime")
});

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccountState {
    Restoring,
    SignedOut,
    SigningIn,
    SignedIn,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MainContent {
    Home,
    Search,
    Artist,
    Playlist,
    Settings,
}

#[derive(Clone, Copy)]
enum PlaylistCachePolicy {
    Fresh,
    AllowStale,
}

#[derive(Clone, Copy)]
struct PlaylistScrollPosition {
    row: usize,
    offset_y: Pixels,
}

impl PlaylistScrollPosition {
    fn top() -> Self {
        Self {
            row: 0,
            offset_y: px(0.),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchCategory {
    Songs,
    Playlists,
    Albums,
    Artists,
}

impl SearchCategory {
    const ALL: [Self; 4] = [Self::Songs, Self::Playlists, Self::Albums, Self::Artists];

    fn label(self) -> &'static str {
        match self {
            Self::Songs => "单曲",
            Self::Playlists => "歌单",
            Self::Albums => "专辑",
            Self::Artists => "歌手",
        }
    }

    fn icon(self) -> MediaIcon {
        match self {
            Self::Songs => MediaIcon::Music,
            Self::Artists => MediaIcon::Artist,
            Self::Albums => MediaIcon::Album,
            Self::Playlists => MediaIcon::Playlist,
        }
    }
}

enum SearchMoreResults {
    Songs(SearchPage<Track>),
    Artists(SearchPage<SearchArtist>),
    Albums(SearchPage<SearchAlbum>),
    Playlists(SearchPage<UserPlaylist>),
}

#[derive(Clone, Copy)]
enum SongRowSource {
    Search,
    Artist,
}

fn append_search_page<T>(target: &mut SearchPage<T>, mut page: SearchPage<T>) {
    target.items.append(&mut page.items);
    target.has_more = page.has_more;
    target.next_offset = page.next_offset;
}

fn insert_track_after_current(
    tracks: &mut Vec<Track>,
    current_index: Option<usize>,
    track: Track,
) -> usize {
    let mut current_index = current_index.filter(|index| *index < tracks.len());
    if let Some(existing_index) = tracks.iter().position(|item| item.mid == track.mid) {
        if current_index == Some(existing_index) {
            return existing_index;
        }
        tracks.remove(existing_index);
        if let Some(index) = &mut current_index
            && existing_index < *index
        {
            *index -= 1;
        }
    }
    let insert_index = current_index.map_or(tracks.len(), |index| index + 1);
    tracks.insert(insert_index, track);
    insert_index
}

fn insert_external_track_after_current(
    queue: &mut PlaybackQueue,
    current_index: Option<usize>,
    track: Track,
) -> usize {
    let index = insert_track_after_current(&mut queue.tracks, current_index, track);
    queue.modified = true;
    index
}

fn canonical_queue_track_index(
    queue: &PlaybackQueue,
    playlist_id: &UserPlaylistId,
    track_mid: &str,
) -> Option<usize> {
    (!queue.modified && queue.playlist_id == *playlist_id)
        .then(|| queue.tracks.iter().position(|track| track.mid == track_mid))
        .flatten()
}

fn resolved_playlist_scroll_row(
    requested_row: usize,
    track_count: usize,
    has_more: bool,
) -> Option<usize> {
    if requested_row >= track_count && has_more {
        None
    } else {
        Some(requested_row.min(track_count.saturating_sub(1)))
    }
}

#[derive(Clone)]
enum NavigationPage {
    Home,
    Settings,
    Search {
        query: String,
        category: SearchCategory,
    },
    Artist {
        artist: SearchArtist,
    },
    Playlist {
        playlist: UserPlaylist,
        selected_index: Option<usize>,
        scroll_position: PlaylistScrollPosition,
    },
}

impl NavigationPage {
    fn same_destination(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Home, Self::Home) => true,
            (Self::Settings, Self::Settings) => true,
            (Self::Search { query, .. }, Self::Search { query: other, .. }) => query == other,
            (Self::Artist { artist, .. }, Self::Artist { artist: other, .. }) => {
                artist.mid == other.mid
            }
            (
                Self::Playlist { playlist, .. },
                Self::Playlist {
                    playlist: other, ..
                },
            ) => playlist.id == other.id,
            _ => false,
        }
    }
}

#[derive(Default)]
struct NavigationHistory {
    back: Vec<NavigationPage>,
    forward: Vec<NavigationPage>,
}

impl NavigationHistory {
    fn record(&mut self, current: Option<NavigationPage>, target: &NavigationPage) {
        if current
            .as_ref()
            .is_some_and(|current| current.same_destination(target))
        {
            return;
        }
        if let Some(current) = current {
            self.back.push(current);
        }
        self.forward.clear();
    }

    fn go_back(&mut self, current: Option<NavigationPage>) -> Option<NavigationPage> {
        let target = self.back.pop()?;
        if let Some(current) = current {
            self.forward.push(current);
        }
        Some(target)
    }

    fn go_forward(&mut self, current: Option<NavigationPage>) -> Option<NavigationPage> {
        let target = self.forward.pop()?;
        if let Some(current) = current {
            self.back.push(current);
        }
        Some(target)
    }

    fn clear(&mut self) {
        self.back.clear();
        self.forward.clear();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RepeatMode {
    Off,
    All,
    One,
}

#[derive(Clone)]
struct PlaybackLocation {
    track_mid: String,
    quality: Quality,
    urls: Vec<String>,
}

struct PlaybackQueue {
    playlist_id: UserPlaylistId,
    tracks: Vec<Track>,
    modified: bool,
    continuation: Option<PersistedQueueContinuation>,
}

impl PersistedQueueContinuation {
    fn can_load_more(self) -> bool {
        match self {
            Self::Radar { has_more, .. } => has_more,
            Self::Guess => true,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PlaylistPageKey {
    account_id: u64,
    playlist_id: UserPlaylistId,
    offset: u64,
}

async fn request_playlist_page(
    requests: SingleFlight<PlaylistPageKey, PlaylistPage>,
    client: ProtocolClient,
    credential: QqCredential,
    playlist: UserPlaylist,
    offset: u64,
    force: bool,
) -> anyhow::Result<PlaylistPage> {
    let key = PlaylistPageKey {
        account_id: credential.music_id,
        playlist_id: playlist.id.clone(),
        offset,
    };
    requests
        .run(key, force, move || async move {
            tokio::time::timeout(
                Duration::from_secs(30),
                client.playlist_page(&credential, &playlist, offset, PAGE_SIZE),
            )
            .await
            .context("QQ 音乐歌单分页请求等待超过 30 秒")?
        })
        .await
}

enum PlaybackLoadEvent {
    ResolvingOptions,
    Options(Vec<Quality>),
    Finished(anyhow::Result<(PreparedPlayback, PlaybackLocation, Vec<Quality>)>),
}

struct StatusMessage {
    text: String,
    is_error: bool,
}

impl StatusMessage {
    fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

impl RepeatMode {
    fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Off => "循环",
            Self::All => "列表循环",
            Self::One => "单曲循环",
        }
    }
}

pub struct LyruneView {
    account_state: AccountState,
    credential: Option<QqCredential>,
    profile: Option<UserProfile>,
    qr_image: Option<Arc<Image>>,
    library_loading: bool,
    selected_playlist_index: Option<usize>,
    selected_playlist: Option<UserPlaylist>,
    page_offset: u64,
    page_loading: bool,
    pending_playlist_scroll_position: Option<PlaylistScrollPosition>,
    library_generation: u64,
    playlist_generation: u64,
    playlist_force_refresh: bool,
    playlist_cache_revision: u64,
    playlist_page_requests: SingleFlight<PlaylistPageKey, PlaylistPage>,
    main_content: MainContent,
    navigation_history: NavigationHistory,
    home_playlists: Vec<UserPlaylist>,
    home_loading: bool,
    home_loaded: bool,
    home_error: Option<String>,
    home_generation: u64,
    home_recommendation_loading: Option<RecommendationKind>,
    search_query: String,
    search_results: Option<SearchResults>,
    search_category: SearchCategory,
    search_loading: bool,
    search_loading_more: bool,
    search_error: Option<String>,
    search_generation: u64,
    selected_artist: Option<SearchArtist>,
    artist_songs: Option<SearchPage<Track>>,
    artist_track_count: u64,
    artist_songs_loading: bool,
    artist_songs_loading_more: bool,
    artist_song_error: Option<String>,
    artist_albums: Option<SearchPage<SearchAlbum>>,
    artist_albums_loading: bool,
    artist_albums_loading_more: bool,
    artist_album_error: Option<String>,
    artist_generation: u64,

    playlist_list: Entity<ListState<PlaylistListDelegate>>,
    track_table: Entity<TableState<TrackTableDelegate>>,
    search_input: Entity<InputState>,
    ui_font_input: Entity<InputState>,
    monospace_font_input: Entity<InputState>,
    lyric_font_input: Entity<InputState>,
    audio_cache_limit_input: Entity<InputState>,
    progress_slider: Entity<SliderState>,
    volume_slider: Entity<SliderState>,

    audio: Option<AudioPlayer>,
    audio_cache: Option<AudioCache>,
    protocol_client: Option<ProtocolClient>,
    cdn_maintenance: Option<JoinHandle<()>>,
    audio_cache_maintenance: Option<JoinHandle<()>>,
    playback_queue: Option<PlaybackQueue>,
    queue_generation: u64,
    queue_recommendation_loading: bool,
    queue_waiting_for_recommendation: bool,
    current_track: Option<usize>,
    loading_track: Option<usize>,
    loading_autoplay: bool,
    resolving_qualities: bool,
    playback_started: bool,
    playback_location: Option<PlaybackLocation>,
    active_quality: Quality,
    available_qualities: Vec<Quality>,
    quality_menu_open: bool,
    position: Duration,
    seek_preview: Option<Duration>,
    progress_hovered: bool,
    progress_hover_fraction: Option<f32>,
    cover_backdrop_expanded: bool,
    cover_backdrop_fully_expanded: bool,
    backdrop_current_url: Option<String>,
    backdrop_previous_url: Option<String>,
    backdrop_crossfade_phase: bool,
    lyric_disk_cache: Option<LyricDiskCache>,
    lyrics_cache: HashMap<String, MemoryLyrics>,
    pending_lyrics_cache: HashMap<String, MemoryLyrics>,
    lyric_layout_cache: LyricLayoutCache,
    lyric_motion_state: Option<LyricMotionState>,
    pending_lyric_reveal_mid: Option<String>,
    lyric_reveal_frame_pending: bool,
    lyrics_loading: HashSet<String>,
    lyrics_errors: HashMap<String, String>,
    fonts: AppFonts,
    settings: AppSettings,
    library_cache: LibraryCache,
    liked_tracks: HashMap<String, bool>,
    liked_state_loading: HashSet<String>,
    liked_toggle_loading: HashSet<String>,
    shuffle: bool,
    repeat_mode: RepeatMode,
    pending_playback_restore: Option<PersistedPlayback>,
    last_playback_persisted_at: Instant,

    status: StatusMessage,
    login_generation: u64,
    play_generation: u64,
    account_menu_open: bool,
    _subscriptions: Vec<Subscription>,
    _window_subscription: Option<Subscription>,
    window_tick_wake: Option<async_channel::Sender<()>>,
    background_tick_wake: Option<async_channel::Sender<()>>,
    lyric_animation_frame_pending: bool,
    next_lyric_highlight_frame: Option<Instant>,
    next_lyric_scroll_frame: Option<Instant>,
    #[cfg(target_os = "linux")]
    mpris: Option<MprisHandle>,
    #[cfg(target_os = "linux")]
    last_mpris_position_sync: Instant,
}

impl LyruneView {
    pub(crate) fn new(
        window: &mut Window,
        settings: AppSettings,
        fonts: AppFonts,
        cx: &mut Context<Self>,
    ) -> Self {
        let (audio, mut initial_status, mut initial_status_is_error) = match AudioPlayer::new() {
            Ok(player) => {
                player.set_volume(settings.volume);
                (Some(player), "正在读取已保存的登录状态…".to_owned(), false)
            }
            Err(error) => (
                None,
                format!("音频设备初始化失败：{error:#}；仍可浏览 QQ 音乐歌单"),
                true,
            ),
        };
        let audio_cache =
            match AudioCache::new(audio_cache_limit_bytes(settings.audio_cache_limit_gb)) {
                Ok(cache) => Some(cache),
                Err(error) => {
                    initial_status = format!("{initial_status}；音频缓存初始化失败：{error:#}");
                    initial_status_is_error = true;
                    None
                }
            };
        let lyric_disk_cache = LyricDiskCache::new().ok();
        let cdn_cache = match CdnCacheStore::load() {
            Ok(cache) => cache,
            Err(error) => {
                initial_status = format!("{initial_status}；CDN 缓存读取失败：{error:#}");
                initial_status_is_error = true;
                Default::default()
            }
        };
        let protocol_client = match ProtocolClient::new_with_cdn_cache(cdn_cache) {
            Ok(client) => Some(client),
            Err(error) => {
                initial_status = format!("{initial_status}；QQ 音乐客户端初始化失败：{error:#}");
                initial_status_is_error = true;
                None
            }
        };
        let playback_quality = settings.playback_quality;
        let pending_playback_restore = settings.current_playback.clone();
        let library_cache = LibraryCache::default();

        let playlist_list =
            cx.new(|cx| ListState::new(PlaylistListDelegate::new(), window, cx).searchable(false));
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("想播放什么？")
                .context_menu(false)
        });
        let ui_font_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.ui_font_families.join(", "))
                .placeholder("例如：Inter, Noto Sans CJK SC")
        });
        let monospace_font_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.monospace_font_families.join(", "))
                .placeholder("例如：JetBrains Mono, Noto Sans Mono")
        });
        let lyric_font_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.lyric_font_families.join(", "))
                .placeholder("例如：LXGW WenKai, Noto Sans CJK JP")
        });
        let audio_cache_limit_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.audio_cache_limit_gb.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: None,
                })
                .validate(|value, _| value.parse::<u64>().is_ok_and(|value| value > 0))
                .min(1.)
                .step(1.)
        });
        let (load_more_sender, load_more_receiver) = async_channel::bounded(1);
        let (track_event_sender, track_event_receiver) = async_channel::unbounded();
        let track_table = cx.new(|cx| {
            TableState::new(
                TrackTableDelegate::new(load_more_sender, track_event_sender),
                window,
                cx,
            )
            .col_selectable(false)
            .col_movable(false)
            .sortable(false)
        });
        let progress_slider = cx.new(|_| progress_slider_state(0.));
        let volume_slider = cx.new(|_| volume_slider_state(settings.volume));

        let subscriptions = vec![
            cx.subscribe(&playlist_list, |this, _, event: &ListEvent, cx| {
                if let ListEvent::Select(index) | ListEvent::Confirm(index) = event {
                    this.select_playlist(index.row, cx);
                }
            }),
            cx.subscribe(&track_table, |this, _, event: &TableEvent, cx| {
                if let TableEvent::DoubleClickedRow(index) = event {
                    this.select_track(*index, cx);
                }
            }),
            cx.subscribe_in(
                &search_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.submit_search(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &audio_cache_limit_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.apply_audio_cache_limit(window, cx);
                    }
                },
            ),
            cx.subscribe(
                &progress_slider,
                |this, _, event: &SliderEvent, cx| match event {
                    SliderEvent::Change(value) => {
                        this.seek_preview = this
                            .current_duration()
                            .map(|duration| duration.mul_f32(value.end().clamp(0., 1.)));
                        cx.notify();
                    }
                    SliderEvent::Release(value) => {
                        let target = this
                            .current_duration()
                            .map(|duration| duration.mul_f32(value.end().clamp(0., 1.)));
                        this.seek_preview = None;
                        if let Some(target) = target {
                            this.seek_to(target, cx);
                        }
                    }
                },
            ),
            cx.subscribe(
                &volume_slider,
                |this, _, event: &SliderEvent, cx| match event {
                    SliderEvent::Change(value) => this.set_volume(value.end(), cx),
                    SliderEvent::Release(value) => {
                        this.set_volume(value.end(), cx);
                        this.persist_settings();
                    }
                },
            ),
        ];

        cx.spawn(async move |this, cx| {
            while load_more_receiver.recv().await.is_ok() {
                if this
                    .update(cx, |this, cx| this.load_playlist_page(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = track_event_receiver.recv().await {
                if this
                    .update_in(cx, |this, window, cx| match event {
                        TrackTableEvent::Artist(artist) => {
                            this.open_search_artist(artist, window, cx)
                        }
                        TrackTableEvent::Album(album) => {
                            this.open_home_playlist(album.into_playlist(), window, cx)
                        }
                        TrackTableEvent::Unlike(track) => this.unlike_track(track, cx),
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let mut view = Self {
            account_state: AccountState::Restoring,
            credential: None,
            profile: None,
            qr_image: None,
            library_loading: false,
            selected_playlist_index: None,
            selected_playlist: None,
            page_offset: 0,
            page_loading: false,
            pending_playlist_scroll_position: None,
            library_generation: 0,
            playlist_generation: 0,
            playlist_force_refresh: false,
            playlist_cache_revision: 0,
            playlist_page_requests: SingleFlight::default(),
            main_content: MainContent::Home,
            navigation_history: NavigationHistory::default(),
            home_playlists: Vec::new(),
            home_loading: false,
            home_loaded: false,
            home_error: None,
            home_generation: 0,
            home_recommendation_loading: None,
            search_query: String::new(),
            search_results: None,
            search_category: SearchCategory::Songs,
            search_loading: false,
            search_loading_more: false,
            search_error: None,
            search_generation: 0,
            selected_artist: None,
            artist_songs: None,
            artist_track_count: 0,
            artist_songs_loading: false,
            artist_songs_loading_more: false,
            artist_song_error: None,
            artist_albums: None,
            artist_albums_loading: false,
            artist_albums_loading_more: false,
            artist_album_error: None,
            artist_generation: 0,
            playlist_list,
            track_table,
            search_input,
            ui_font_input,
            monospace_font_input,
            lyric_font_input,
            audio_cache_limit_input,
            progress_slider,
            volume_slider,
            audio,
            audio_cache,
            protocol_client,
            cdn_maintenance: None,
            audio_cache_maintenance: None,
            playback_queue: None,
            queue_generation: 0,
            queue_recommendation_loading: false,
            queue_waiting_for_recommendation: false,
            current_track: None,
            loading_track: None,
            loading_autoplay: false,
            resolving_qualities: false,
            playback_started: false,
            playback_location: None,
            active_quality: playback_quality,
            available_qualities: Vec::new(),
            quality_menu_open: false,
            position: Duration::ZERO,
            seek_preview: None,
            progress_hovered: false,
            progress_hover_fraction: None,
            cover_backdrop_expanded: false,
            cover_backdrop_fully_expanded: false,
            backdrop_current_url: None,
            backdrop_previous_url: None,
            backdrop_crossfade_phase: false,
            lyric_disk_cache,
            lyrics_cache: HashMap::new(),
            pending_lyrics_cache: HashMap::new(),
            lyric_layout_cache: LyricLayoutCache::default(),
            lyric_motion_state: None,
            pending_lyric_reveal_mid: None,
            lyric_reveal_frame_pending: false,
            lyrics_loading: HashSet::new(),
            lyrics_errors: HashMap::new(),
            fonts,
            settings,
            library_cache,
            liked_tracks: HashMap::new(),
            liked_state_loading: HashSet::new(),
            liked_toggle_loading: HashSet::new(),
            shuffle: false,
            repeat_mode: RepeatMode::Off,
            pending_playback_restore,
            last_playback_persisted_at: Instant::now(),
            status: if initial_status_is_error {
                StatusMessage::error(initial_status)
            } else {
                StatusMessage::info(initial_status)
            },
            login_generation: 0,
            play_generation: 0,
            account_menu_open: false,
            _subscriptions: subscriptions,
            _window_subscription: None,
            window_tick_wake: None,
            background_tick_wake: None,
            lyric_animation_frame_pending: false,
            next_lyric_highlight_frame: None,
            next_lyric_scroll_frame: None,
            #[cfg(target_os = "linux")]
            mpris: None,
            #[cfg(target_os = "linux")]
            last_mpris_position_sync: Instant::now(),
        };
        view.attach_window(window, cx);
        view.start_audio_cache_maintenance();
        view.start_cdn_maintenance();
        view.restore_credential(cx);
        view
    }

    pub(crate) fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.lyric_animation_frame_pending = false;
        self.next_lyric_highlight_frame = None;
        self.next_lyric_scroll_frame = None;
        window.set_inactive_frame_interval(self.inactive_window_frame_interval());
        self._window_subscription = Some(cx.observe_window_bounds(window, |this, window, _| {
            let size = window.window_bounds().get_bounds().size;
            let width = f32::from(size.width).round() as u32;
            let height = f32::from(size.height).round() as u32;
            if width > 0 && height > 0 {
                this.settings.window_size = Some(PersistedWindowSize { width, height });
            }
        }));
        self.sync_progress_slider(window, cx);
        self.start_window_tick(window, cx);
    }

    fn start_window_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (wake_sender, wake_receiver) = async_channel::bounded(1);
        self.window_tick_wake = Some(wake_sender);
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let playback_advancing =
                    match this.read_with(cx, |this, _| this.playback_is_advancing()) {
                        Ok(playback_advancing) => playback_advancing,
                        Err(_) => break,
                    };
                if !playback_advancing {
                    if this
                        .update_in(cx, |this, window, cx| this.sync_progress_slider(window, cx))
                        .is_err()
                        || wake_receiver.recv().await.is_err()
                    {
                        break;
                    }
                    continue;
                }

                let timer = cx.background_executor().timer(PROGRESS_TICK);
                let wake = wake_receiver.recv();
                pin_mut!(timer, wake);
                match select(timer, wake).await {
                    Either::Left(_) => {
                        if this
                            .update_in(cx, |this, window, cx| this.sync_progress_slider(window, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Either::Right((Ok(()), _)) => {}
                    Either::Right((Err(_), _)) => break,
                }
            }
        })
        .detach();
    }

    pub(crate) fn start_background_tick(&mut self, cx: &mut Context<Self>) {
        let (wake_sender, wake_receiver) = async_channel::bounded(1);
        self.background_tick_wake = Some(wake_sender);
        cx.spawn(async move |this, cx| {
            loop {
                let interval = match this.read_with(cx, |this, _| {
                    if !this.playback_is_advancing() {
                        None
                    } else {
                        Some(PROGRESS_TICK)
                    }
                }) {
                    Ok(interval) => interval,
                    Err(_) => break,
                };
                let Some(interval) = interval else {
                    if wake_receiver.recv().await.is_err() {
                        break;
                    }
                    continue;
                };

                let timer = cx.background_executor().timer(interval);
                let wake = wake_receiver.recv();
                pin_mut!(timer, wake);
                match select(timer, wake).await {
                    Either::Left(_) => {
                        if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                            break;
                        }
                    }
                    Either::Right((Ok(()), _)) => {}
                    Either::Right((Err(_), _)) => break,
                }
            }
        })
        .detach();
    }

    fn playback_is_advancing(&self) -> bool {
        self.playback_started
            && self.loading_track.is_none()
            && self.audio.as_ref().is_some_and(AudioPlayer::is_playing)
    }

    fn wake_playback_ticks(&self) {
        if let Some(wake) = &self.window_tick_wake {
            let _ = wake.try_send(());
        }
        if let Some(wake) = &self.background_tick_wake {
            let _ = wake.try_send(());
        }
    }

    fn inactive_window_frame_interval(&self) -> Option<Duration> {
        if self.cover_backdrop_expanded {
            combined_lyric_frame_interval(
                self.settings.lyric_highlight_frame_rate,
                self.settings.lyric_scroll_frame_rate,
            )
        } else {
            Some(crate::INACTIVE_WINDOW_FRAME_INTERVAL)
        }
    }

    fn reset_lyric_animation_frames(&mut self) {
        self.next_lyric_highlight_frame = None;
        self.next_lyric_scroll_frame = None;
    }

    fn lyric_motion_is_active(&self, now: Instant, reduce_motion: bool) -> bool {
        if reduce_motion {
            return false;
        }
        let Some(state) = &self.lyric_motion_state else {
            return false;
        };
        let elapsed = now.saturating_duration_since(state.started_at);
        (state.scroll_from != state.target && elapsed < LYRIC_SCROLL_DURATION)
            || (state.style_from != state.target && elapsed < LYRIC_STYLE_DURATION)
    }

    fn request_lyric_animation_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.lyric_animation_frame_pending {
            return;
        }

        self.lyric_animation_frame_pending = true;
        cx.on_next_frame(window, |this, window, cx| {
            this.lyric_animation_frame_pending = false;
            if !this.cover_backdrop_expanded || !this.playback_is_advancing() {
                this.reset_lyric_animation_frames();
                return;
            }

            let now = cx.background_executor().now();
            let highlight_frame_due = lyric_frame_is_due(
                now,
                this.settings.lyric_highlight_frame_rate,
                &mut this.next_lyric_highlight_frame,
            );
            let scroll_frame_due = if this.lyric_motion_is_active(now, cx.reduce_motion()) {
                lyric_frame_is_due(
                    now,
                    this.settings.lyric_scroll_frame_rate,
                    &mut this.next_lyric_scroll_frame,
                )
            } else {
                this.next_lyric_scroll_frame = None;
                false
            };
            if highlight_frame_due || scroll_frame_due {
                cx.notify();
            }
            this.request_lyric_animation_frame(window, cx);
        });
    }

    fn lyric_motion_anchors(
        &mut self,
        mid: &str,
        target: usize,
        motion_enabled: bool,
        now: Instant,
    ) -> (f32, f32) {
        let target = target as f32;
        if !motion_enabled {
            self.lyric_motion_state = None;
            return (target, target);
        }

        if self
            .lyric_motion_state
            .as_ref()
            .is_none_or(|state| state.mid != mid)
        {
            self.lyric_motion_state = Some(LyricMotionState {
                mid: mid.to_owned(),
                scroll_from: target,
                style_from: target,
                target,
                started_at: now,
            });
            return (target, target);
        }

        let Some(state) = self.lyric_motion_state.as_mut() else {
            return (target, target);
        };
        if state.target != target {
            state.scroll_from = state.scroll_anchor(now);
            state.style_from = state.style_anchor(now);
            state.target = target;
            state.started_at = now;
        }
        (state.scroll_anchor(now), state.style_anchor(now))
    }

    fn defer_new_lyrics_until_next_frame(
        &mut self,
        mid: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.pending_lyric_reveal_mid.as_deref() != Some(mid) {
            return false;
        }
        if !self.lyric_reveal_frame_pending {
            self.lyric_reveal_frame_pending = true;
            let mid = mid.to_owned();
            cx.on_next_frame(window, move |this, _, cx| {
                this.lyric_reveal_frame_pending = false;
                if this.pending_lyric_reveal_mid.as_deref() == Some(mid.as_str()) {
                    this.pending_lyric_reveal_mid = None;
                }
                cx.notify();
            });
        }
        true
    }

    fn set_cover_backdrop_expanded(
        &mut self,
        expanded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cover_backdrop_expanded = expanded;
        if !expanded {
            self.cover_backdrop_fully_expanded = false;
        }
        self.reset_lyric_animation_frames();
        window.set_inactive_frame_interval(self.inactive_window_frame_interval());
        self.wake_playback_ticks();
        cx.notify();
    }

    pub(crate) fn window_size(&self) -> Option<PersistedWindowSize> {
        self.settings.window_size
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn attach_mpris(&mut self, mpris: MprisHandle) {
        self.mpris = Some(mpris);
        self.sync_mpris(false);
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn handle_mpris_command(&mut self, command: MprisCommand, cx: &mut Context<Self>) {
        match command {
            MprisCommand::Raise | MprisCommand::Quit => {}
            MprisCommand::Next => self.play_next(false, cx),
            MprisCommand::Previous => self.play_previous(cx),
            MprisCommand::Pause => self.pause_playback(cx),
            MprisCommand::PlayPause => self.toggle_playback(cx),
            MprisCommand::Stop => self.stop_playback(cx),
            MprisCommand::Play => self.play(cx),
            MprisCommand::Seek(offset) => self.seek_by(offset, cx),
            MprisCommand::SetPosition { track_id, position } => {
                self.set_mpris_position(&track_id, position, cx);
            }
            MprisCommand::SetLoopStatus(status) => {
                self.repeat_mode = match status {
                    MprisLoopStatus::None => RepeatMode::Off,
                    MprisLoopStatus::Track => RepeatMode::One,
                    MprisLoopStatus::Playlist => RepeatMode::All,
                };
                self.sync_mpris(false);
                cx.notify();
            }
            MprisCommand::SetShuffle(shuffle) => {
                self.shuffle = shuffle;
                self.sync_mpris(false);
                cx.notify();
            }
            MprisCommand::SetVolume(volume) => {
                let volume = volume.clamp(0., 1.) as f32;
                self.volume_slider.update(cx, |slider, cx| {
                    *slider = volume_slider_state(volume);
                    cx.notify();
                });
                self.set_volume(volume, cx);
                self.persist_settings();
            }
        }
    }

    fn restore_credential(&mut self, cx: &mut Context<Self>) {
        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = async {
                let stored = tokio::task::spawn_blocking(CredentialStore::load)
                    .await
                    .context("读取凭据任务异常退出")??;
                match stored {
                    Some(credential) => refresh_credential(credential).await.map(Some),
                    None => Ok(None),
                }
            }
            .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| match result {
                Ok(Some(credential)) => {
                    this.account_state = AccountState::SignedIn;
                    this.credential = Some(credential.clone());
                    this.main_content = MainContent::Home;
                    this.status = StatusMessage::info("已恢复 QQ 音乐登录，正在加载音乐库…");
                    this.persist_credential(credential.clone(), cx);
                    this.load_home(cx);
                    this.load_library(false, cx);
                }
                Ok(None) => {
                    this.account_state = AccountState::SignedOut;
                    this.begin_login(cx);
                }
                Err(error) => {
                    this.account_state = AccountState::SignedOut;
                    this.status = StatusMessage::error(format!(
                        "无法恢复登录：{error:#}；正在加载登录二维码…"
                    ));
                    this.begin_login(cx);
                }
            });
        })
        .detach();
    }

    fn begin_login(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.account_state,
            AccountState::Restoring | AccountState::SigningIn
        ) {
            return;
        }

        self.login_generation = self.login_generation.wrapping_add(1);
        let generation = self.login_generation;
        self.account_state = AccountState::SigningIn;
        self.qr_image = None;
        self.status = StatusMessage::info("正在向 QQ 音乐申请二维码…");
        cx.notify();

        let (sender, receiver) = async_channel::unbounded();
        drop(RUNTIME.spawn(run_qr_login(sender)));
        cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                let completed = matches!(
                    event,
                    LoginEvent::Succeeded(_) | LoginEvent::Expired | LoginEvent::Failed(_)
                );
                let _ = this.update(cx, |this, cx| {
                    if this.login_generation == generation {
                        this.handle_login_event(event, cx);
                    }
                });
                if completed {
                    break;
                }
            }
        })
        .detach();
    }

    fn handle_login_event(&mut self, event: LoginEvent, cx: &mut Context<Self>) {
        match event {
            LoginEvent::QrReady(png) => {
                self.qr_image = Some(Arc::new(Image::from_bytes(ImageFormat::Png, png)));
                self.status = StatusMessage::info("请使用 QQ 音乐 App 扫描二维码");
            }
            LoginEvent::WaitingScan => self.status = StatusMessage::info("等待扫码…"),
            LoginEvent::WaitingConfirm => {
                self.status = StatusMessage::info("已扫码，请在手机上确认登录");
            }
            LoginEvent::Succeeded(credential) => {
                self.account_state = AccountState::SignedIn;
                self.qr_image = None;
                self.credential = Some(credential.clone());
                self.main_content = MainContent::Home;
                self.status = StatusMessage::info("登录成功，正在加载音乐库…");
                self.persist_credential(credential.clone(), cx);
                self.load_home(cx);
                self.load_library(false, cx);
            }
            LoginEvent::Expired => {
                self.account_state = AccountState::SignedOut;
                self.qr_image = None;
                self.begin_login(cx);
            }
            LoginEvent::Failed(error) => {
                self.account_state = AccountState::SignedOut;
                self.qr_image = None;
                self.status =
                    StatusMessage::error(format!("扫码登录失败：{error}；点击二维码区域重试"));
            }
        }
        cx.notify();
    }

    fn persist_credential(&self, credential: QqCredential, cx: &mut Context<Self>) {
        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = tokio::task::spawn_blocking(move || CredentialStore::save(&credential))
                .await
                .context("保存凭据任务异常退出")
                .and_then(|result| result);
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(Err(error)) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.status = StatusMessage::error(format!(
                    "登录成功，但凭据未能保存到系统钥匙串：{error:#}"
                ));
                cx.notify();
            });
        })
        .detach();
    }

    fn start_cdn_maintenance(&mut self) {
        if let Some(task) = self.cdn_maintenance.take() {
            task.abort();
        }
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        self.cdn_maintenance = Some(RUNTIME.spawn(async move {
            loop {
                let delay = client.cdn_refresh_delay().await;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                match client.refresh_cdn().await {
                    Ok(cache) => {
                        let _ =
                            tokio::task::spawn_blocking(move || CdnCacheStore::save(&cache)).await;
                    }
                    Err(_) => tokio::time::sleep(CDN_REFRESH_RETRY).await,
                }
            }
        }));
    }

    fn start_audio_cache_maintenance(&mut self) {
        if let Some(task) = self.audio_cache_maintenance.take() {
            task.abort();
        }
        let Some(cache) = self.audio_cache.clone() else {
            return;
        };
        self.audio_cache_maintenance = Some(RUNTIME.spawn(async move {
            let _ = cache.maintain().await;
        }));
    }

    fn current_navigation_page(&self, cx: &App) -> Option<NavigationPage> {
        match self.main_content {
            MainContent::Home => Some(NavigationPage::Home),
            MainContent::Settings => Some(NavigationPage::Settings),
            MainContent::Search => Some(NavigationPage::Search {
                query: self.search_query.clone(),
                category: self.search_category,
            }),
            MainContent::Artist => self
                .selected_artist
                .clone()
                .map(|artist| NavigationPage::Artist { artist }),
            MainContent::Playlist => self.selected_playlist.clone().map(|playlist| {
                let table = self.track_table.read(cx);
                let scroll_position = PlaylistScrollPosition {
                    row: table.visible_range().rows().start,
                    offset_y: table
                        .vertical_scroll_handle
                        .0
                        .borrow()
                        .base_handle
                        .offset()
                        .y,
                };
                NavigationPage::Playlist {
                    playlist,
                    selected_index: self.selected_playlist_index,
                    scroll_position,
                }
            }),
        }
    }

    fn apply_navigation_page(
        &mut self,
        page: NavigationPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match page {
            NavigationPage::Home => {
                self.main_content = MainContent::Home;
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                if !self.home_loaded && !self.home_loading {
                    self.load_home(cx);
                }
                cx.notify();
            }
            NavigationPage::Settings => {
                self.main_content = MainContent::Settings;
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                cx.notify();
            }
            NavigationPage::Search { query, category } => {
                self.main_content = MainContent::Search;
                self.search_category = category;
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                self.search_input.update(cx, |input, cx| {
                    input.set_value(query.clone(), window, cx);
                });
                if self.search_query != query || self.search_results.is_none() {
                    self.start_search(query, cx);
                } else {
                    cx.notify();
                }
            }
            NavigationPage::Artist { artist } => {
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                self.open_artist(artist, cx);
            }
            NavigationPage::Playlist {
                playlist,
                selected_index,
                scroll_position,
            } => {
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(selected_index.map(IndexPath::new), window, cx);
                });
                self.open_playlist(
                    playlist,
                    selected_index,
                    false,
                    PlaylistCachePolicy::AllowStale,
                    scroll_position,
                    cx,
                );
            }
        }
    }

    fn show_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = NavigationPage::Home;
        let current = self.current_navigation_page(cx);
        self.navigation_history.record(current, &target);
        self.apply_navigation_page(target, window, cx);
    }

    fn show_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = NavigationPage::Settings;
        let current = self.current_navigation_page(cx);
        self.navigation_history.record(current, &target);
        self.account_menu_open = false;
        self.apply_navigation_page(target, window, cx);
    }

    fn submit_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().trim().to_owned();
        if query.is_empty() {
            return;
        }
        window.blur();
        let target = NavigationPage::Search {
            query,
            category: SearchCategory::Songs,
        };
        let current = self.current_navigation_page(cx);
        self.navigation_history.record(current, &target);
        self.apply_navigation_page(target, window, cx);
    }

    fn start_search(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(credential) = self.credential.clone() else {
            self.search_error = Some("请先登录 QQ 音乐".to_owned());
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.search_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        self.search_query = query.clone();
        self.search_results = None;
        self.search_category = SearchCategory::Songs;
        self.search_loading = true;
        self.search_loading_more = false;
        self.search_error = None;
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                client.search(&credential, &query, 20),
            )
            .await
            .context("QQ 音乐搜索等待超过 30 秒")
            .and_then(|result| result);
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.search_generation != generation {
                    return;
                }
                this.search_loading = false;
                match result {
                    Ok(results) => {
                        this.search_results = Some(results);
                        this.search_error = None;
                    }
                    Err(error) => {
                        this.search_results = None;
                        this.search_error = Some(format!("搜索失败：{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_more_search(&mut self, cx: &mut Context<Self>) {
        if self.search_loading || self.search_loading_more {
            return;
        }
        let Some(results) = self.search_results.as_ref() else {
            return;
        };
        let (offset, has_more) = match self.search_category {
            SearchCategory::Songs => (results.songs.next_offset, results.songs.has_more),
            SearchCategory::Artists => (results.artists.next_offset, results.artists.has_more),
            SearchCategory::Albums => (results.albums.next_offset, results.albums.has_more),
            SearchCategory::Playlists => {
                (results.playlists.next_offset, results.playlists.has_more)
            }
        };
        if !has_more {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        let query = self.search_query.clone();
        let category = self.search_category;
        let generation = self.search_generation;
        self.search_loading_more = true;
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = match category {
                SearchCategory::Songs => client
                    .search_songs(&credential, &query, offset, 20)
                    .await
                    .map(SearchMoreResults::Songs),
                SearchCategory::Artists => client
                    .search_artists(&credential, &query, offset, 20)
                    .await
                    .map(SearchMoreResults::Artists),
                SearchCategory::Albums => client
                    .search_albums(&credential, &query, offset, 20)
                    .await
                    .map(SearchMoreResults::Albums),
                SearchCategory::Playlists => client
                    .search_playlists(&credential, &query, offset, 20)
                    .await
                    .map(SearchMoreResults::Playlists),
            };
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.search_generation != generation {
                    return;
                }
                this.search_loading_more = false;
                if this.search_category != category {
                    cx.notify();
                    return;
                }
                match (this.search_results.as_mut(), result) {
                    (Some(results), Ok(SearchMoreResults::Songs(page))) => {
                        append_search_page(&mut results.songs, page)
                    }
                    (Some(results), Ok(SearchMoreResults::Artists(page))) => {
                        append_search_page(&mut results.artists, page)
                    }
                    (Some(results), Ok(SearchMoreResults::Albums(page))) => {
                        append_search_page(&mut results.albums, page)
                    }
                    (Some(results), Ok(SearchMoreResults::Playlists(page))) => {
                        append_search_page(&mut results.playlists, page)
                    }
                    (_, Err(error)) => {
                        this.search_error = Some(format!("继续加载搜索结果失败：{error:#}"));
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_search_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(results) = self.search_results.as_ref() else {
            return;
        };
        let Some(track) = results.songs.items.get(index).cloned() else {
            return;
        };
        if self
            .current_track_data()
            .is_some_and(|current| current.mid == track.mid)
        {
            if self.loading_track.is_none() {
                self.toggle_playback(cx);
            }
            return;
        }

        self.pending_playback_restore = None;
        self.home_recommendation_loading = None;
        let current_index = self.current_track;
        let queue_index = if let Some(queue) = &mut self.playback_queue {
            insert_external_track_after_current(queue, current_index, track)
        } else {
            self.playback_queue = Some(PlaybackQueue {
                playlist_id: UserPlaylistId::Search {
                    query: self.search_query.clone(),
                },
                tracks: vec![track],
                modified: false,
                continuation: None,
            });
            0
        };
        self.start_playback(queue_index, Duration::ZERO, None, true, cx);
    }

    fn navigate_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_navigation_page(cx);
        if let Some(target) = self.navigation_history.go_back(current) {
            self.apply_navigation_page(target, window, cx);
        }
    }

    fn navigate_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_navigation_page(cx);
        if let Some(target) = self.navigation_history.go_forward(current) {
            self.apply_navigation_page(target, window, cx);
        }
    }

    fn load_home(&mut self, cx: &mut Context<Self>) {
        if self.home_loading {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            self.home_error = Some("请先登录 QQ 音乐".to_owned());
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.home_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        self.home_generation = self.home_generation.wrapping_add(1);
        let generation = self.home_generation;
        self.home_loading = true;
        self.home_error = None;
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                client.recommended_playlists(&credential, 0, 20),
            )
            .await
            .context("QQ 音乐主页请求等待超过 30 秒")
            .and_then(|result| result);
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.home_generation != generation {
                    return;
                }
                this.home_loading = false;
                match result {
                    Ok(page) => {
                        this.home_loaded = true;
                        this.home_playlists = page.items;
                        this.home_error = None;
                    }
                    Err(error) => {
                        this.home_error = Some(format!("加载主页推荐失败：{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_home_recommendation(&mut self, kind: RecommendationKind, cx: &mut Context<Self>) {
        let Some(credential) = self.credential.clone() else {
            self.status = StatusMessage::error("请先登录 QQ 音乐");
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.status = StatusMessage::error("QQ 音乐客户端不可用");
            cx.notify();
            return;
        };
        self.home_recommendation_loading = Some(kind);
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(30), async {
                match kind {
                    RecommendationKind::Radar => {
                        let page = client.radar_tracks(&credential, 1).await?;
                        Ok::<_, anyhow::Error>((
                            page.tracks,
                            PersistedQueueContinuation::Radar {
                                next_page: page.next_page,
                                has_more: page.has_more,
                            },
                        ))
                    }
                    RecommendationKind::Guess => Ok((
                        client.guess_tracks(&credential, 5).await?,
                        PersistedQueueContinuation::Guess,
                    )),
                }
            })
            .await
            .context("QQ 音乐个性化推荐请求等待超过 30 秒")
            .and_then(|result| result);
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.home_recommendation_loading != Some(kind) {
                    return;
                }
                this.home_recommendation_loading = None;
                match result {
                    Ok((tracks, continuation)) if !tracks.is_empty() => {
                        this.pending_playback_restore = None;
                        this.queue_generation = this.queue_generation.wrapping_add(1);
                        this.queue_recommendation_loading = false;
                        this.queue_waiting_for_recommendation = false;
                        this.playback_queue = Some(PlaybackQueue {
                            playlist_id: UserPlaylistId::Recommendation { kind },
                            tracks,
                            modified: false,
                            continuation: Some(continuation),
                        });
                        this.start_playback(0, Duration::ZERO, None, true, cx);
                    }
                    Ok(_) => {
                        this.status = StatusMessage::error("QQ 音乐没有返回可播放的推荐歌曲");
                    }
                    Err(error) => {
                        this.status =
                            StatusMessage::error(format!("加载个性化推荐失败：{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn maybe_load_queue_recommendations(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.queue_recommendation_loading {
            return;
        }
        let Some((continuation, remaining)) = self.playback_queue.as_ref().and_then(|queue| {
            let continuation = queue.continuation?;
            let current = self.current_track.unwrap_or_default();
            Some((
                continuation,
                queue.tracks.len().saturating_sub(current.saturating_add(1)),
            ))
        }) else {
            return;
        };
        if !continuation.can_load_more() || (!force && remaining > 2) {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        let generation = self.queue_generation;
        self.queue_recommendation_loading = true;

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(30), async {
                match continuation {
                    PersistedQueueContinuation::Radar { next_page, .. } => {
                        let page = client.radar_tracks(&credential, next_page).await?;
                        Ok::<_, anyhow::Error>((
                            page.tracks,
                            Some(PersistedQueueContinuation::Radar {
                                next_page: page.next_page,
                                has_more: page.has_more,
                            }),
                        ))
                    }
                    PersistedQueueContinuation::Guess => {
                        let tracks = client.guess_tracks(&credential, 5).await?;
                        let next =
                            (!tracks.is_empty()).then_some(PersistedQueueContinuation::Guess);
                        Ok((tracks, next))
                    }
                }
            })
            .await
            .context("QQ 音乐推荐队列请求等待超过 30 秒")
            .and_then(|result| result);
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.queue_generation != generation {
                    return;
                }
                this.queue_recommendation_loading = false;
                match result {
                    Ok((tracks, continuation)) => {
                        let mut first_added = None;
                        if let Some(queue) = &mut this.playback_queue {
                            queue.continuation = continuation;
                            for track in tracks {
                                if !queue.tracks.iter().any(|item| item.mid == track.mid) {
                                    first_added.get_or_insert(queue.tracks.len());
                                    queue.tracks.push(track);
                                }
                            }
                        }
                        this.persist_current_playback();
                        if this.queue_waiting_for_recommendation {
                            this.queue_waiting_for_recommendation = false;
                            if let Some(index) = first_added {
                                this.start_playback(index, Duration::ZERO, None, true, cx);
                            } else {
                                this.status = StatusMessage::info("当前推荐暂时没有更多歌曲");
                            }
                        }
                    }
                    Err(error) => {
                        this.queue_waiting_for_recommendation = false;
                        this.status =
                            StatusMessage::error(format!("继续加载推荐歌曲失败：{error:#}"));
                    }
                }
                #[cfg(target_os = "linux")]
                this.sync_mpris(false);
                cx.notify();
            });
        })
        .detach();
    }

    fn load_library(&mut self, force_refresh: bool, cx: &mut Context<Self>) {
        let Some(credential) = self.credential.clone() else {
            return;
        };
        if !force_refresh
            && let Some((profile, playlists)) = self.library_cache.fresh_directory(
                credential.music_id,
                unix_timestamp_secs(),
                LIBRARY_CACHE_TTL,
            )
        {
            self.library_loading = false;
            self.apply_library(credential.music_id, profile, playlists, false, cx);
            return;
        }
        let Some(client) = self.protocol_client.clone() else {
            self.status = StatusMessage::error("QQ 音乐客户端不可用");
            cx.notify();
            return;
        };
        self.library_generation = self.library_generation.wrapping_add(1);
        let generation = self.library_generation;
        self.library_loading = true;
        self.status = StatusMessage::info("正在加载用户资料和歌单…");
        cx.notify();
        let account_id = credential.music_id;

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = async {
                tokio::time::timeout(Duration::from_secs(30), async {
                    tokio::try_join!(
                        client.user_profile(&credential),
                        client.user_playlists(&credential)
                    )
                })
                .await
                .context("QQ 音乐用户资料和歌单请求等待超过 30 秒")?
            }
            .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.library_generation != generation {
                    return;
                }
                this.library_loading = false;
                match result {
                    Ok((profile, playlists)) => {
                        this.library_cache.replace_directory(
                            account_id,
                            profile.clone(),
                            playlists.clone(),
                            unix_timestamp_secs(),
                        );
                        this.apply_library(account_id, profile, playlists, force_refresh, cx);
                    }
                    Err(error) => {
                        this.status =
                            StatusMessage::error(format!("加载 QQ 音乐资料失败：{error:#}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn apply_library(
        &mut self,
        account_id: u64,
        profile: UserProfile,
        playlists: Vec<UserPlaylist>,
        force_refresh: bool,
        cx: &mut Context<Self>,
    ) {
        self.profile = Some(profile);
        let count = playlists.len();
        let viewed_index = self
            .settings
            .last_library_view
            .as_ref()
            .filter(|view| view.account_id == account_id)
            .and_then(|view| {
                playlists
                    .iter()
                    .position(|playlist| playlist.id == view.playlist_id)
            });
        let playback_restore = self
            .pending_playback_restore
            .clone()
            .filter(|restore| restore.account_id == account_id);
        if self.pending_playback_restore.is_some() && playback_restore.is_none() {
            self.clear_persisted_playback();
        }
        self.playlist_list.update(cx, |list, cx| {
            list.delegate_mut().set_playlists(playlists);
            cx.notify();
        });
        if count > 0 && self.main_content == MainContent::Playlist {
            self.select_playlist_with_refresh(viewed_index.unwrap_or(0), force_refresh, false, cx);
        } else if count == 0 {
            self.status = StatusMessage::info("QQ 音乐账号中没有可显示的歌单");
            cx.notify();
        }
        if let Some(restore) = playback_restore {
            self.restore_playback_queue(restore, cx);
        }
    }

    fn select_playlist(&mut self, index: usize, cx: &mut Context<Self>) {
        self.select_playlist_with_refresh(index, false, true, cx);
    }

    fn select_playlist_with_refresh(
        &mut self,
        index: usize,
        force_refresh: bool,
        record_navigation: bool,
        cx: &mut Context<Self>,
    ) {
        let playlist = self
            .playlist_list
            .read(cx)
            .delegate()
            .playlist(index)
            .cloned();
        let Some(playlist) = playlist else {
            return;
        };

        if let Some(account_id) = self
            .credential
            .as_ref()
            .map(|credential| credential.music_id)
        {
            let view = PersistedLibraryView {
                account_id,
                playlist_id: playlist.id.clone(),
            };
            if self.settings.last_library_view.as_ref() != Some(&view) {
                self.settings.last_library_view = Some(view);
                self.persist_settings();
            }
        }

        if record_navigation {
            let target = NavigationPage::Playlist {
                playlist: playlist.clone(),
                selected_index: Some(index),
                scroll_position: PlaylistScrollPosition::top(),
            };
            let current = self.current_navigation_page(cx);
            self.navigation_history.record(current, &target);
        }
        self.open_playlist(
            playlist,
            Some(index),
            force_refresh,
            PlaylistCachePolicy::Fresh,
            PlaylistScrollPosition::top(),
            cx,
        );
    }

    fn open_search_artist(
        &mut self,
        artist: SearchArtist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = NavigationPage::Artist { artist };
        let current = self.current_navigation_page(cx);
        self.navigation_history.record(current, &target);
        self.apply_navigation_page(target, window, cx);
    }

    fn open_artist(&mut self, artist: SearchArtist, cx: &mut Context<Self>) {
        let artist_changed = self
            .selected_artist
            .as_ref()
            .is_none_or(|selected| selected.mid != artist.mid);
        if artist_changed {
            self.artist_generation = self.artist_generation.wrapping_add(1);
            self.artist_songs = None;
            self.artist_track_count = 0;
            self.artist_songs_loading = false;
            self.artist_songs_loading_more = false;
            self.artist_song_error = None;
            self.artist_albums = None;
            self.artist_albums_loading = false;
            self.artist_albums_loading_more = false;
            self.artist_album_error = None;
        }
        self.selected_artist = Some(artist);
        self.main_content = MainContent::Artist;
        if self.artist_songs.is_none() && !self.artist_songs_loading {
            self.load_artist_songs(false, cx);
        }
        if self.artist_albums.is_none() && !self.artist_albums_loading {
            self.load_artist_albums(false, cx);
        }
        cx.notify();
    }

    fn load_artist_songs(&mut self, append: bool, cx: &mut Context<Self>) {
        if self.artist_songs_loading || self.artist_songs_loading_more {
            return;
        }
        let Some(artist) = self.selected_artist.clone() else {
            return;
        };
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.artist_song_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        let offset = if append {
            let Some(page) = self.artist_songs.as_ref().filter(|page| page.has_more) else {
                return;
            };
            page.next_offset
        } else {
            0
        };
        let generation = self.artist_generation;
        let artist_mid = artist.mid.clone();
        let playlist = artist.into_playlist();
        self.artist_song_error = None;
        if append {
            self.artist_songs_loading_more = true;
        } else {
            self.artist_songs_loading = true;
        }
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = client
                .playlist_page(&credential, &playlist, offset, ARTIST_PAGE_SIZE)
                .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.artist_generation != generation
                    || this
                        .selected_artist
                        .as_ref()
                        .map(|artist| artist.mid.as_str())
                        != Some(artist_mid.as_str())
                {
                    return;
                }
                this.finish_artist_song_load(result, append);
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_artist_song_load(&mut self, result: anyhow::Result<PlaylistPage>, append: bool) {
        self.artist_songs_loading = false;
        self.artist_songs_loading_more = false;
        match result {
            Ok(page) => {
                self.artist_track_count = page.total;
                let page = SearchPage {
                    items: page.tracks,
                    has_more: page.has_more,
                    next_offset: page.next_offset,
                };
                if append {
                    if let Some(songs) = &mut self.artist_songs {
                        append_search_page(songs, page);
                    } else {
                        self.artist_songs = Some(page);
                    }
                } else {
                    self.artist_songs = Some(page);
                }
            }
            Err(error) => {
                self.artist_song_error = Some(format!("加载歌手歌曲失败：{error:#}"));
            }
        }
    }

    fn load_artist_albums(&mut self, append: bool, cx: &mut Context<Self>) {
        if self.artist_albums_loading || self.artist_albums_loading_more {
            return;
        }
        let Some(artist) = self.selected_artist.clone() else {
            return;
        };
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.artist_album_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        let offset = if append {
            let Some(page) = self.artist_albums.as_ref().filter(|page| page.has_more) else {
                return;
            };
            page.next_offset
        } else {
            0
        };
        let generation = self.artist_generation;
        let artist_mid = artist.mid.clone();
        self.artist_album_error = None;
        if append {
            self.artist_albums_loading_more = true;
        } else {
            self.artist_albums_loading = true;
        }
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = client
                .artist_albums(&credential, &artist, offset, ARTIST_PAGE_SIZE)
                .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.artist_generation != generation
                    || this
                        .selected_artist
                        .as_ref()
                        .map(|artist| artist.mid.as_str())
                        != Some(artist_mid.as_str())
                {
                    return;
                }
                this.finish_artist_album_load(result, append);
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_artist_album_load(
        &mut self,
        result: anyhow::Result<SearchPage<SearchAlbum>>,
        append: bool,
    ) {
        self.artist_albums_loading = false;
        self.artist_albums_loading_more = false;
        match result {
            Ok(page) if append => {
                if let Some(albums) = &mut self.artist_albums {
                    append_search_page(albums, page);
                } else {
                    self.artist_albums = Some(page);
                }
            }
            Ok(page) => self.artist_albums = Some(page),
            Err(error) => {
                self.artist_album_error = Some(format!("加载歌手专辑失败：{error:#}"));
            }
        }
    }

    fn select_artist_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(artist) = self.selected_artist.clone() else {
            return;
        };
        let Some(songs) = self.artist_songs.as_ref() else {
            return;
        };
        let Some(selected_track) = songs.items.get(index) else {
            return;
        };
        let mut playlist = artist.into_playlist();
        playlist.track_count = self.artist_track_count;

        if let Some(queue_index) = self
            .playback_queue
            .as_ref()
            .and_then(|queue| canonical_queue_track_index(queue, &playlist.id, &selected_track.mid))
        {
            if self.current_track == Some(queue_index) {
                if self.loading_track.is_none() {
                    self.toggle_playback(cx);
                }
            } else {
                self.start_playback(queue_index, Duration::ZERO, None, true, cx);
            }
            return;
        }

        let tracks = songs.items.clone();
        let has_more = songs.has_more;
        self.pending_playback_restore = None;
        self.replace_playback_queue(playlist, tracks, has_more, cx);
        self.start_playback(index, Duration::ZERO, None, true, cx);
    }

    fn open_home_playlist(
        &mut self,
        playlist: UserPlaylist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = NavigationPage::Playlist {
            playlist: playlist.clone(),
            selected_index: None,
            scroll_position: PlaylistScrollPosition::top(),
        };
        let current = self.current_navigation_page(cx);
        self.navigation_history.record(current, &target);
        self.playlist_list.update(cx, |list, cx| {
            list.set_selected_index(None, window, cx);
        });
        self.open_playlist(
            playlist,
            None,
            false,
            PlaylistCachePolicy::Fresh,
            PlaylistScrollPosition::top(),
            cx,
        );
    }

    fn restore_pending_playlist_scroll(&mut self, cx: &mut Context<Self>) {
        let Some(position) = self.pending_playlist_scroll_position else {
            return;
        };
        let target_row = {
            let table = self.track_table.read(cx);
            resolved_playlist_scroll_row(
                position.row,
                table.delegate().tracks().len(),
                table.delegate().has_more(),
            )
        };
        let Some(target_row) = target_row else {
            return;
        };
        self.pending_playlist_scroll_position = None;
        self.track_table.update(cx, |table, cx| {
            if target_row == position.row {
                let scroll_state = &mut *table.vertical_scroll_handle.0.borrow_mut();
                scroll_state.deferred_scroll_to_item = None;
                let current_offset = scroll_state.base_handle.offset();
                scroll_state
                    .base_handle
                    .set_offset(point(current_offset.x, position.offset_y));
            } else {
                table.scroll_to_row(target_row, cx);
            }
            cx.notify();
        });
    }

    fn open_playlist(
        &mut self,
        playlist: UserPlaylist,
        selected_index: Option<usize>,
        force_refresh: bool,
        cache_policy: PlaylistCachePolicy,
        scroll_position: PlaylistScrollPosition,
        cx: &mut Context<Self>,
    ) {
        self.main_content = MainContent::Playlist;

        self.playlist_generation = self.playlist_generation.wrapping_add(1);
        self.playlist_force_refresh = force_refresh;
        self.playlist_cache_revision = new_cache_revision();
        self.selected_playlist_index = selected_index;
        self.selected_playlist = Some(playlist.clone());
        self.page_offset = 0;
        self.page_loading = false;
        self.pending_playlist_scroll_position = Some(scroll_position);
        if let Some(index) = selected_index {
            self.playlist_list.update(cx, |list, cx| {
                list.delegate_mut().set_selected(index);
                cx.notify();
            });
        }
        self.track_table.update(cx, |table, cx| {
            table.clear_selection(cx);
            table
                .delegate_mut()
                .reset(playlist.id == UserPlaylistId::Liked);
            table.refresh(cx);
            cx.notify();
        });
        let cached_snapshot = if force_refresh {
            None
        } else {
            self.credential
                .as_ref()
                .and_then(|credential| match cache_policy {
                    PlaylistCachePolicy::Fresh => self.library_cache.fresh_playlist(
                        credential.music_id,
                        &playlist.id,
                        unix_timestamp_secs(),
                        LIBRARY_CACHE_TTL,
                    ),
                    PlaylistCachePolicy::AllowStale => self
                        .library_cache
                        .cached_playlist(credential.music_id, &playlist.id),
                })
        };
        if let Some(snapshot) = cached_snapshot {
            self.playlist_cache_revision = snapshot.revision;
            self.page_offset = snapshot.next_offset;
            self.selected_playlist = Some(snapshot.playlist.clone());
            if let Some(index) = self.selected_playlist_index {
                self.playlist_list.update(cx, |list, cx| {
                    list.delegate_mut()
                        .update_playlist(index, snapshot.playlist.clone());
                    cx.notify();
                });
            }
            let track_count = snapshot.tracks.len();
            self.track_table.update(cx, |table, cx| {
                table
                    .delegate_mut()
                    .append(snapshot.tracks, snapshot.has_more);
                cx.notify();
            });
            self.restore_pending_playlist_scroll(cx);
            self.status = StatusMessage::info(if track_count == 0 {
                format!("歌单“{}”中暂时没有歌曲", snapshot.playlist.title)
            } else {
                format!("已打开歌单“{}”", snapshot.playlist.title)
            });
            self.sync_table_playback_state(cx);
            cx.notify();
            if self.pending_playlist_scroll_position.is_some() {
                self.load_playlist_page(cx);
            }
            return;
        }
        self.status = StatusMessage::info(format!("正在加载歌单“{}”…", playlist.title));
        self.sync_table_playback_state(cx);
        self.load_playlist_page(cx);
    }

    fn load_playlist_page(&mut self, cx: &mut Context<Self>) {
        if self.page_loading {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(playlist) = self.selected_playlist.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.status = StatusMessage::error("QQ 音乐客户端不可用");
            cx.notify();
            return;
        };

        let generation = self.playlist_generation;
        let offset = self.page_offset;
        let cache_revision = self.playlist_cache_revision;
        let force_refresh = offset == 0 && self.playlist_force_refresh;
        self.playlist_force_refresh = false;
        let requests = self.playlist_page_requests.clone();
        self.page_loading = true;
        self.track_table.update(cx, |table, cx| {
            table.delegate_mut().set_loading(true);
            cx.notify();
        });

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = request_playlist_page(
                requests,
                client,
                credential,
                playlist,
                offset,
                force_refresh,
            )
            .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.playlist_generation != generation {
                    return;
                }
                this.page_loading = false;
                match result {
                    Ok(page) => {
                        let new_tracks = page.tracks.len();
                        if let Some(credential) = &this.credential {
                            this.library_cache.store_playlist_page(
                                credential.music_id,
                                page.playlist.clone(),
                                page.tracks.clone(),
                                page.has_more,
                                page.next_offset,
                                offset,
                                unix_timestamp_secs(),
                                cache_revision,
                            );
                        }
                        this.page_offset = page.next_offset;
                        this.selected_playlist = Some(page.playlist.clone());
                        if let Some(index) = this.selected_playlist_index {
                            this.playlist_list.update(cx, |list, cx| {
                                list.delegate_mut()
                                    .update_playlist(index, page.playlist.clone());
                                cx.notify();
                            });
                        }
                        this.track_table.update(cx, |table, cx| {
                            table.delegate_mut().append(page.tracks, page.has_more);
                            cx.notify();
                        });
                        if offset == 0 {
                            this.status = StatusMessage::info(if new_tracks == 0 {
                                format!("歌单“{}”中暂时没有歌曲", page.playlist.title)
                            } else {
                                format!("已打开歌单“{}”", page.playlist.title)
                            });
                        }
                        this.restore_pending_playlist_scroll(cx);
                        if this.pending_playlist_scroll_position.is_some() {
                            this.load_playlist_page(cx);
                        }
                    }
                    Err(error) => {
                        this.track_table.update(cx, |table, cx| {
                            table.delegate_mut().set_loading(false);
                            cx.notify();
                        });
                        this.status = StatusMessage::error(format!("加载歌单失败：{error:#}"));
                    }
                }
                this.sync_table_playback_state(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn restore_playback_queue(&mut self, restore: PersistedPlayback, cx: &mut Context<Self>) {
        let index = restore
            .queue_tracks
            .iter()
            .position(|track| track.mid == restore.track_mid);
        if let Some(index) = index {
            let resume_at = restore.resume_position(restore.queue_tracks[index].duration_seconds);
            self.pending_playback_restore = None;
            self.queue_generation = self.queue_generation.wrapping_add(1);
            self.queue_recommendation_loading = false;
            self.queue_waiting_for_recommendation = false;
            self.playback_queue = Some(PlaybackQueue {
                playlist_id: restore.playlist_id,
                tracks: restore.queue_tracks,
                modified: restore.queue_modified,
                continuation: restore.queue_continuation,
            });
            self.start_playback(index, resume_at, None, false, cx);
        } else {
            self.clear_persisted_playback();
        }
    }

    fn replace_playback_queue(
        &mut self,
        playlist: UserPlaylist,
        tracks: Vec<Track>,
        has_more: bool,
        cx: &mut Context<Self>,
    ) {
        self.home_recommendation_loading = None;
        self.queue_generation = self.queue_generation.wrapping_add(1);
        let generation = self.queue_generation;
        let mut offset = tracks.len() as u64;
        let initial_tracks = tracks.clone();
        self.playback_queue = Some(PlaybackQueue {
            playlist_id: playlist.id.clone(),
            tracks,
            modified: false,
            continuation: None,
        });
        self.queue_recommendation_loading = false;
        self.queue_waiting_for_recommendation = false;

        if !has_more {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        let account_id = credential.music_id;
        let cached_playlist = playlist.clone();
        let cache_revision = self.playlist_cache_revision;
        let requests = self.playlist_page_requests.clone();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = async {
                let mut remaining = Vec::new();
                let mut has_more = true;
                while has_more {
                    let page = request_playlist_page(
                        requests.clone(),
                        client.clone(),
                        credential.clone(),
                        playlist.clone(),
                        offset,
                        false,
                    )
                    .await
                    .context("无法补全 QQ 音乐播放队列")?;
                    offset = page.next_offset;
                    has_more = page.has_more;
                    remaining.extend(page.tracks);
                }
                Ok::<_, anyhow::Error>(remaining)
            }
            .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.queue_generation != generation {
                    return;
                }
                if let (Some(queue), Ok(tracks)) = (&mut this.playback_queue, result) {
                    let mut cached_tracks = initial_tracks;
                    cached_tracks.extend(tracks.iter().cloned());
                    for track in tracks {
                        if !queue.tracks.iter().any(|item| item.mid == track.mid) {
                            queue.tracks.push(track);
                        }
                    }
                    let cached_track_count = cached_tracks.len() as u64;
                    this.library_cache.replace_playlist(
                        account_id,
                        cached_playlist,
                        cached_tracks,
                        false,
                        cached_track_count,
                        unix_timestamp_secs(),
                        cache_revision,
                    );
                    this.persist_current_playback();
                    #[cfg(target_os = "linux")]
                    this.sync_mpris(false);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn select_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let (tracks, has_more) = {
            let table = self.track_table.read(cx);
            (
                table.delegate().tracks().to_vec(),
                table.delegate().has_more(),
            )
        };
        if index >= tracks.len() {
            return;
        }
        let Some(playlist) = self.selected_playlist.clone() else {
            return;
        };
        let selected_mid = tracks[index].mid.as_str();
        if let Some(queue_index) = self
            .playback_queue
            .as_ref()
            .and_then(|queue| canonical_queue_track_index(queue, &playlist.id, selected_mid))
        {
            if self.current_track == Some(queue_index) {
                if self.loading_track.is_none() {
                    self.toggle_playback(cx);
                }
            } else {
                self.start_playback(queue_index, Duration::ZERO, None, true, cx);
            }
            return;
        }
        self.pending_playback_restore = None;
        self.replace_playback_queue(playlist, tracks, has_more, cx);
        self.start_playback(index, Duration::ZERO, None, true, cx);
    }

    fn ensure_track_lyrics(
        &mut self,
        client: ProtocolClient,
        credential: QqCredential,
        track: Track,
        cx: &mut Context<Self>,
    ) {
        let mid = track.mid.clone();
        if mid.is_empty() {
            return;
        }

        let current_is_same_track = self
            .current_track_data()
            .is_some_and(|current| current.mid == mid);
        if !current_is_same_track && let Some(pending) = self.pending_lyrics_cache.remove(&mid) {
            self.lyrics_cache.insert(mid.clone(), pending);
            cx.notify();
        }
        let had_cached = self.lyrics_cache.contains_key(&mid);
        if self
            .pending_lyrics_cache
            .get(&mid)
            .is_some_and(|lyrics| current_is_same_track && lyrics.is_fresh(unix_timestamp_secs()))
            || self
                .lyrics_cache
                .get(&mid)
                .is_some_and(|lyrics| lyrics.is_fresh(unix_timestamp_secs()))
            || !self.lyrics_loading.insert(mid.clone())
        {
            return;
        }

        self.lyrics_errors.remove(&mid);
        cx.notify();

        let disk_cache = self.lyric_disk_cache.clone();
        let (sender, receiver) = async_channel::bounded(2);
        let worker_mid = mid.clone();
        drop(RUNTIME.spawn(async move {
            let mut had_cached = had_cached;
            if !had_cached
                && let Some(cache) = disk_cache.as_ref()
                && let Ok(Some(cached)) = cache.load(&worker_mid).await
            {
                let fresh = cached.is_fresh(unix_timestamp_secs(), LYRIC_CACHE_TTL);
                let lyrics = MemoryLyrics {
                    parsed: Arc::new(parse_lyrics(
                        &cached.lyrics.lyric,
                        cached.lyrics.trans_lyric.as_deref(),
                        cached.lyrics.roma_lyric.as_deref(),
                    )),
                    fetched_at_secs: cached.fetched_at_secs,
                };
                if sender
                    .send(LyricLoadEvent::Disk { lyrics, fresh })
                    .await
                    .is_err()
                {
                    return;
                }
                if fresh {
                    return;
                }
                had_cached = true;
            }

            let result = match client.lyrics(&credential, &track).await {
                Ok(result) => {
                    let fetched_at_secs = unix_timestamp_secs();
                    if let Some(cache) = disk_cache.as_ref() {
                        let _ = cache.save(fetched_at_secs, &result).await;
                    }
                    Ok(MemoryLyrics {
                        parsed: Arc::new(parse_lyrics(
                            &result.lyric,
                            result.trans_lyric.as_deref(),
                            result.roma_lyric.as_deref(),
                        )),
                        fetched_at_secs,
                    })
                }
                Err(error) => Err(error),
            };
            let _ = sender
                .send(LyricLoadEvent::Network { result, had_cached })
                .await;
        }));

        cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                if this
                    .update(cx, |this, cx| {
                        match event {
                            LyricLoadEvent::Disk { lyrics, fresh } => {
                                this.lyrics_errors.remove(&mid);
                                this.lyrics_cache.insert(mid.clone(), lyrics);
                                if fresh {
                                    this.lyrics_loading.remove(&mid);
                                }
                            }
                            LyricLoadEvent::Network { result, had_cached } => {
                                this.lyrics_loading.remove(&mid);
                                match result {
                                    Ok(lyrics) => {
                                        this.lyrics_errors.remove(&mid);
                                        let is_current = this
                                            .current_track_data()
                                            .is_some_and(|track| track.mid == mid);
                                        let unchanged = this
                                            .lyrics_cache
                                            .get(&mid)
                                            .is_some_and(|cached| cached.parsed == lyrics.parsed);
                                        if had_cached && is_current {
                                            if unchanged {
                                                if let Some(cached) =
                                                    this.lyrics_cache.get_mut(&mid)
                                                {
                                                    cached.fetched_at_secs = lyrics.fetched_at_secs;
                                                }
                                                this.pending_lyrics_cache.remove(&mid);
                                            } else {
                                                this.pending_lyrics_cache
                                                    .insert(mid.clone(), lyrics);
                                            }
                                        } else {
                                            this.pending_lyrics_cache.remove(&mid);
                                            this.lyrics_cache.insert(mid.clone(), lyrics);
                                        }
                                    }
                                    Err(error) if !had_cached => {
                                        this.lyrics_errors
                                            .insert(mid.clone(), format!("{error:#}"));
                                    }
                                    Err(_) => {
                                        this.lyrics_errors.remove(&mid);
                                    }
                                }
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if !this.lyrics_cache.contains_key(&mid) && this.lyrics_loading.remove(&mid) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn start_playback(
        &mut self,
        index: usize,
        resume_at: Duration,
        requested_quality: Option<Quality>,
        autoplay: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(credential) = self.credential.clone() else {
            self.status = StatusMessage::error("请先登录 QQ 音乐");
            cx.notify();
            return;
        };
        let Some(track) = self
            .playback_queue
            .as_ref()
            .and_then(|queue| queue.tracks.get(index))
            .cloned()
        else {
            return;
        };
        let Some(audio_cache) = self.audio_cache.clone() else {
            self.status = StatusMessage::error("音频缓存不可用，无法创建播放流");
            cx.notify();
            return;
        };
        let Some(audio) = &self.audio else {
            self.status = StatusMessage::error("没有可用的音频输出设备");
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.status = StatusMessage::error("QQ 音乐客户端不可用");
            cx.notify();
            return;
        };

        let desired_quality = requested_quality.unwrap_or(self.settings.playback_quality);
        let same_track = (self.current_track == Some(index) && self.loading_track.is_some())
            || self
                .playback_location
                .as_ref()
                .is_some_and(|location| location.track_mid == track.mid);
        let known_qualities = if same_track {
            self.available_qualities.clone()
        } else {
            self.available_qualities.clear();
            self.quality_menu_open = false;
            Vec::new()
        };
        let reused_urls = self
            .playback_location
            .as_ref()
            .filter(|location| {
                location.track_mid == track.mid && location.quality == desired_quality
            })
            .map(|location| location.urls.clone());

        audio.stop();
        if !same_track {
            self.lyric_motion_state = None;
            self.pending_lyric_reveal_mid = self.cover_backdrop_expanded.then(|| track.mid.clone());
        }
        self.ensure_track_lyrics(client.clone(), credential.clone(), track.clone(), cx);
        self.play_generation = self.play_generation.wrapping_add(1);
        let generation = self.play_generation;
        self.current_track = Some(index);
        self.loading_track = Some(index);
        self.ensure_track_like_state(track.mid.clone(), cx);
        self.loading_autoplay = autoplay;
        self.resolving_qualities = reused_urls.is_none() && known_qualities.is_empty();
        self.playback_started = false;
        self.wake_playback_ticks();
        self.position = resume_at;
        self.progress_hovered = false;
        self.progress_hover_fraction = None;
        let progress = progress_fraction(resume_at, Duration::from_secs(track.duration_seconds));
        self.progress_slider.update(cx, |slider, cx| {
            *slider = progress_slider_state(progress);
            cx.notify();
        });
        self.status = StatusMessage::info(if self.resolving_qualities {
            format!("正在检测“{}”的音质…", track.title)
        } else {
            format!("正在缓冲“{}”…", track.title)
        });
        self.queue_waiting_for_recommendation = false;
        self.persist_current_playback();
        self.sync_table_playback_state(cx);
        #[cfg(target_os = "linux")]
        self.sync_mpris(!same_track && !resume_at.is_zero());
        cx.notify();
        self.maybe_load_queue_recommendations(false, cx);

        let title = track.title.clone();
        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = async {
                let reused_stream = match reused_urls {
                    Some(urls) => audio_cache
                        .prepare_for_seek_with_fallbacks(urls.clone(), &track, desired_quality)
                        .await
                        .ok()
                        .map(|stream| {
                            let qualities = if known_qualities.is_empty() {
                                vec![desired_quality]
                            } else {
                                known_qualities.clone()
                            };
                            (desired_quality, urls, stream, qualities)
                        }),
                    None => None,
                };
                let (quality, urls, stream, available_qualities) = match reused_stream {
                    Some(reused) => reused,
                    None => {
                        let _ = sender.send(PlaybackLoadEvent::ResolvingOptions).await;
                        let options = client.playback_options(&credential, &track).await?;
                        let mut available_qualities = options
                            .iter()
                            .map(|option| option.quality)
                            .collect::<Vec<_>>();
                        let _ = sender
                            .send(PlaybackLoadEvent::Options(available_qualities.clone()))
                            .await;
                        let candidates =
                            Quality::fallback_order(&available_qualities, desired_quality);
                        let mut prepared = None;
                        let mut last_error = None;
                        for quality in candidates {
                            let Some(option) =
                                options.iter().find(|option| option.quality == quality)
                            else {
                                continue;
                            };
                            let urls = option.urls().map(str::to_owned).collect::<Vec<_>>();
                            match audio_cache
                                .prepare_with_fallbacks(urls.clone(), &track, quality)
                                .await
                            {
                                Ok(stream) => {
                                    prepared = Some((quality, urls, stream));
                                    break;
                                }
                                Err(error) => {
                                    available_qualities.retain(|candidate| *candidate != quality);
                                    last_error = Some(error.context(format!(
                                        "“{}”的{}音源不可用",
                                        track.title,
                                        quality.label()
                                    )));
                                }
                            }
                        }
                        let (quality, urls, stream) = prepared.ok_or_else(|| {
                            last_error.unwrap_or_else(|| {
                                anyhow::anyhow!("QQ 音乐没有返回当前账号可播放的音质")
                            })
                        })?;
                        (quality, urls, stream, available_qualities)
                    }
                };
                let playback =
                    tokio::task::spawn_blocking(move || PreparedPlayback::new(stream, resume_at))
                        .await
                        .context("音频解码准备任务异常退出")??;
                Ok::<_, anyhow::Error>((
                    playback,
                    PlaybackLocation {
                        track_mid: track.mid,
                        quality,
                        urls,
                    },
                    available_qualities,
                ))
            }
            .await;
            let _ = sender.send(PlaybackLoadEvent::Finished(result)).await;
        }));

        cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                let finished = matches!(&event, PlaybackLoadEvent::Finished(_));
                let _ = this.update(cx, |this, cx| {
                    if this.play_generation != generation {
                        return;
                    }
                    match event {
                        PlaybackLoadEvent::ResolvingOptions => {
                            this.resolving_qualities = true;
                        }
                        PlaybackLoadEvent::Options(available_qualities) => {
                            this.resolving_qualities = false;
                            let has_available_quality = !available_qualities.is_empty();
                            this.available_qualities = available_qualities;
                            if has_available_quality {
                                this.status = StatusMessage::info(format!("正在缓冲“{title}”…"));
                            } else {
                                this.status =
                                    StatusMessage::info(format!("正在获取“{title}”的可播放音质…"));
                            }
                        }
                        PlaybackLoadEvent::Finished(result) => {
                            this.loading_track = None;
                            this.loading_autoplay = false;
                            this.resolving_qualities = false;
                            match result {
                                Ok((playback, location, available_qualities)) => {
                                    let quality = location.quality;
                                    this.playback_location = Some(location);
                                    this.active_quality = quality;
                                    this.available_qualities = available_qualities;
                                    let result = this
                                        .audio
                                        .as_ref()
                                        .context("音频输出设备不可用")
                                        .and_then(|audio| audio.replace(playback, autoplay));
                                    match result {
                                        Ok(()) => {
                                            if let Some(audio) = &this.audio {
                                                audio.set_volume(this.settings.volume);
                                            }
                                            this.playback_started = true;
                                            this.wake_playback_ticks();
                                            this.status = StatusMessage::info(if autoplay {
                                                format!("正在播放“{title}”")
                                            } else {
                                                format!("已暂停“{title}”")
                                            });
                                        }
                                        Err(error) => {
                                            this.status = StatusMessage::error(format!(
                                                "播放失败：{error:#}"
                                            ));
                                        }
                                    }
                                }
                                Err(error) => {
                                    this.status =
                                        StatusMessage::error(format!("获取歌曲失败：{error:#}"));
                                }
                            }
                        }
                    }
                    this.sync_table_playback_state(cx);
                    #[cfg(target_os = "linux")]
                    this.sync_mpris(false);
                    cx.notify();
                });
                if finished {
                    break;
                }
            }
        })
        .detach();
    }

    fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        if self.loading_track.is_some() {
            return;
        }
        if self.current_track.is_none() {
            if !self.track_table.read(cx).delegate().tracks().is_empty() {
                self.select_track(0, cx);
            }
            return;
        }
        let Some(audio) = &self.audio else {
            return;
        };
        if !self.playback_started || audio.is_empty() {
            let index = self.current_track.expect("current track was checked above");
            self.start_playback(index, Duration::ZERO, None, true, cx);
            return;
        }
        let playing = audio.toggle();
        self.wake_playback_ticks();
        self.status = StatusMessage::info(if playing {
            "继续播放".to_owned()
        } else {
            "已暂停".to_owned()
        });
        self.persist_current_playback();
        self.sync_table_playback_state(cx);
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        cx.notify();
    }

    #[cfg(target_os = "linux")]
    fn play(&mut self, cx: &mut Context<Self>) {
        if self.loading_track.is_some() {
            return;
        }
        let Some(index) = self.current_track else {
            if !self.track_table.read(cx).delegate().tracks().is_empty() {
                self.select_track(0, cx);
            }
            return;
        };
        let Some(audio) = &self.audio else {
            return;
        };
        if !self.playback_started || audio.is_empty() {
            self.start_playback(index, Duration::ZERO, None, true, cx);
        } else if !audio.is_playing() {
            self.toggle_playback(cx);
        }
    }

    #[cfg(target_os = "linux")]
    fn pause_playback(&mut self, cx: &mut Context<Self>) {
        if self.loading_track.is_none() && self.audio.as_ref().is_some_and(AudioPlayer::is_playing)
        {
            self.toggle_playback(cx);
        }
    }

    #[cfg(target_os = "linux")]
    fn stop_playback(&mut self, cx: &mut Context<Self>) {
        if self.current_track.is_none() {
            return;
        }
        self.play_generation = self.play_generation.wrapping_add(1);
        if let Some(audio) = &self.audio {
            audio.stop();
        }
        self.loading_track = None;
        self.loading_autoplay = false;
        self.resolving_qualities = false;
        self.playback_started = false;
        self.wake_playback_ticks();
        self.position = Duration::ZERO;
        self.seek_preview = None;
        self.progress_slider.update(cx, |slider, cx| {
            *slider = progress_slider_state(0.);
            cx.notify();
        });
        self.status = StatusMessage::info("已停止播放");
        self.persist_current_playback();
        self.sync_table_playback_state(cx);
        self.sync_mpris(false);
        cx.notify();
    }

    fn seek_to(&mut self, target: Duration, cx: &mut Context<Self>) {
        let Some(index) = self.current_track else {
            return;
        };
        let target = self
            .current_duration()
            .map_or(target, |duration| target.min(duration));
        let autoplay = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
        self.start_playback(index, target, Some(self.active_quality), autoplay, cx);
        #[cfg(target_os = "linux")]
        self.sync_mpris(true);
    }

    #[cfg(target_os = "linux")]
    fn seek_by(&mut self, offset_micros: i64, cx: &mut Context<Self>) {
        if self.loading_track.is_some() || !self.playback_started {
            return;
        }
        let Some(duration) = self.current_duration() else {
            return;
        };
        let position = self
            .audio
            .as_ref()
            .map(AudioPlayer::position)
            .unwrap_or(self.position);
        let target_micros = duration_micros(position) as i128 + offset_micros as i128;
        let target_micros = target_micros.clamp(0, i64::MAX as i128) as i64;
        if target_micros >= duration_micros(duration) {
            self.play_next(false, cx);
        } else {
            self.seek_to(Duration::from_micros(target_micros as u64), cx);
        }
    }

    #[cfg(target_os = "linux")]
    fn set_mpris_position(&mut self, track_id: &str, position_micros: i64, cx: &mut Context<Self>) {
        if self.loading_track.is_some() || !self.playback_started {
            return;
        }
        let Some(track) = self.current_track_data() else {
            return;
        };
        if track_id != mpris_track_id(&track.mid) || position_micros < 0 {
            return;
        }
        let duration_micros = track
            .duration_seconds
            .saturating_mul(1_000_000)
            .min(i64::MAX as u64) as i64;
        if position_micros >= duration_micros {
            return;
        }
        self.seek_to(Duration::from_micros(position_micros as u64), cx);
    }

    fn play_previous(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.current_track else {
            return;
        };
        let queue_len = self
            .playback_queue
            .as_ref()
            .map_or(0, |queue| queue.tracks.len());
        if self.position >= Duration::from_secs(3) {
            self.seek_to(Duration::ZERO, cx);
            return;
        }
        let previous = if index > 0 {
            Some(index - 1)
        } else if self.repeat_mode == RepeatMode::All && queue_len > 0 {
            Some(queue_len - 1)
        } else {
            None
        };
        if let Some(previous) = previous {
            self.start_playback(previous, Duration::ZERO, None, true, cx);
        }
    }

    fn play_next(&mut self, automatic: bool, cx: &mut Context<Self>) {
        let Some(index) = self.current_track else {
            return;
        };
        let len = self
            .playback_queue
            .as_ref()
            .map_or(0, |queue| queue.tracks.len());
        if len == 0 {
            return;
        }
        let continuation = self
            .playback_queue
            .as_ref()
            .and_then(|queue| queue.continuation);
        let can_extend = continuation.is_some_and(PersistedQueueContinuation::can_load_more);
        let next = if automatic && self.repeat_mode == RepeatMode::One {
            Some(index)
        } else if continuation.is_none() && self.shuffle && len > 1 {
            Some(self.random_track_index(index, len))
        } else if index + 1 < len {
            Some(index + 1)
        } else if !can_extend && self.repeat_mode == RepeatMode::All {
            Some(0)
        } else {
            None
        };
        if let Some(next) = next {
            self.start_playback(next, Duration::ZERO, None, true, cx);
        } else if can_extend {
            self.playback_started = false;
            self.wake_playback_ticks();
            self.queue_waiting_for_recommendation = true;
            self.status = StatusMessage::info("正在获取下一首推荐…");
            self.maybe_load_queue_recommendations(true, cx);
            self.persist_current_playback();
            #[cfg(target_os = "linux")]
            self.sync_mpris(false);
            cx.notify();
        } else {
            self.playback_started = false;
            self.wake_playback_ticks();
            self.position = self.current_duration().unwrap_or_default();
            self.status = StatusMessage::info("当前播放队列已结束");
            self.persist_current_playback();
            #[cfg(target_os = "linux")]
            self.sync_mpris(false);
            cx.notify();
        }
    }

    fn random_track_index(&self, current: usize, len: usize) -> usize {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as usize;
        let candidate = seed % (len - 1);
        if candidate >= current {
            candidate + 1
        } else {
            candidate
        }
    }

    fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.settings.volume = volume.clamp(0., 1.);
        if self.settings.volume > 0. {
            self.settings.last_nonzero_volume = self.settings.volume;
        }
        if let Some(audio) = &self.audio {
            audio.set_volume(self.settings.volume);
        }
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        cx.notify();
    }

    fn set_playback_quality(&mut self, quality: Quality, cx: &mut Context<Self>) {
        if !self.available_qualities.contains(&quality) {
            return;
        }
        self.quality_menu_open = false;
        if self.settings.playback_quality != quality {
            self.settings.playback_quality = quality;
            self.persist_settings();
        }
        if self.active_quality == quality {
            cx.notify();
            return;
        }
        let Some(index) = self.current_track else {
            cx.notify();
            return;
        };
        let loading = self.loading_track.is_some();
        let autoplay = if loading {
            self.loading_autoplay
        } else {
            self.audio.as_ref().is_some_and(AudioPlayer::is_playing)
        };
        let resume_at = if loading {
            self.position
        } else {
            self.audio
                .as_ref()
                .map(AudioPlayer::position)
                .unwrap_or(self.position)
        };
        self.start_playback(index, resume_at, Some(quality), autoplay, cx);
    }

    fn toggle_mute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let volume = if self.settings.volume > 0. {
            0.
        } else {
            self.settings.last_nonzero_volume
        };
        self.volume_slider.update(cx, |slider, cx| {
            slider.set_value(volume, window, cx);
        });
        self.set_volume(volume, cx);
        self.persist_settings();
    }

    fn set_color_theme(
        &mut self,
        color_theme: ColorTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.color_theme == color_theme {
            return;
        }
        self.settings.color_theme = color_theme;
        design::apply(color_theme, &self.fonts, Some(window), cx);
        self.persist_settings();
        cx.notify();
    }

    fn set_preferred_playback_quality(&mut self, quality: Quality, cx: &mut Context<Self>) {
        if self.settings.playback_quality == quality {
            return;
        }
        self.settings.playback_quality = quality;
        self.persist_settings();
        cx.notify();
    }

    fn apply_audio_cache_limit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.settings.audio_cache_limit_gb;
        let value = self
            .audio_cache_limit_input
            .read(cx)
            .value()
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(current);
        self.audio_cache_limit_input.update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
        if value == current {
            return;
        }

        self.settings.audio_cache_limit_gb = value;
        if let Some(cache) = &self.audio_cache {
            cache.set_max_size_bytes(audio_cache_limit_bytes(value));
        }
        self.persist_settings();
        self.start_audio_cache_maintenance();
        cx.notify();
    }

    fn apply_font_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ui = parse_font_families(self.ui_font_input.read(cx).value().as_ref());
        let monospace = parse_font_families(self.monospace_font_input.read(cx).value().as_ref());
        let lyrics = parse_font_families(self.lyric_font_input.read(cx).value().as_ref());
        self.set_font_settings(
            if ui.is_empty() {
                default_ui_font_families()
            } else {
                ui
            },
            if monospace.is_empty() {
                default_monospace_font_families()
            } else {
                monospace
            },
            if lyrics.is_empty() {
                default_lyric_font_families()
            } else {
                lyrics
            },
            window,
            cx,
        );
    }

    fn reset_font_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ui = default_ui_font_families();
        let monospace = default_monospace_font_families();
        let lyrics = default_lyric_font_families();
        self.ui_font_input
            .update(cx, |input, cx| input.set_value(ui.join(", "), window, cx));
        self.monospace_font_input.update(cx, |input, cx| {
            input.set_value(monospace.join(", "), window, cx)
        });
        self.lyric_font_input.update(cx, |input, cx| {
            input.set_value(lyrics.join(", "), window, cx)
        });
    }

    fn set_font_settings(
        &mut self,
        ui: Vec<String>,
        monospace: Vec<String>,
        lyrics: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ui_font_input
            .update(cx, |input, cx| input.set_value(ui.join(", "), window, cx));
        self.monospace_font_input.update(cx, |input, cx| {
            input.set_value(monospace.join(", "), window, cx)
        });
        self.lyric_font_input.update(cx, |input, cx| {
            input.set_value(lyrics.join(", "), window, cx)
        });
        self.settings.ui_font_families = ui;
        self.settings.monospace_font_families = monospace;
        self.settings.lyric_font_families = lyrics;
        self.fonts = design::resolve_fonts(
            &self.settings.ui_font_families,
            &self.settings.monospace_font_families,
            &self.settings.lyric_font_families,
            cx,
        );
        design::apply(self.settings.color_theme, &self.fonts, Some(window), cx);
        self.lyric_layout_cache = LyricLayoutCache::default();
        self.persist_settings();
        cx.notify();
    }

    fn set_lyric_highlight_frame_rate(
        &mut self,
        frame_rate: LyricFrameRate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.lyric_highlight_frame_rate == frame_rate {
            return;
        }
        self.settings.lyric_highlight_frame_rate = frame_rate;
        self.reset_lyric_animation_frames();
        window.set_inactive_frame_interval(self.inactive_window_frame_interval());
        self.persist_settings();
        cx.notify();
    }

    fn set_lyric_scroll_frame_rate(
        &mut self,
        frame_rate: LyricFrameRate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.lyric_scroll_frame_rate == frame_rate {
            return;
        }
        self.settings.lyric_scroll_frame_rate = frame_rate;
        self.reset_lyric_animation_frames();
        window.set_inactive_frame_interval(self.inactive_window_frame_interval());
        self.persist_settings();
        cx.notify();
    }

    fn dismiss_popovers(&mut self, cx: &mut Context<Self>) {
        if self.account_menu_open || self.quality_menu_open {
            self.account_menu_open = false;
            self.quality_menu_open = false;
            cx.notify();
        }
    }

    fn persist_settings(&self) {
        let _ = SettingsStore::save(&self.settings);
    }

    fn persist_current_playback(&mut self) {
        let Some(account_id) = self
            .credential
            .as_ref()
            .map(|credential| credential.music_id)
        else {
            return;
        };
        let Some(playlist_id) = self
            .playback_queue
            .as_ref()
            .map(|queue| queue.playlist_id.clone())
        else {
            return;
        };
        let Some(track_mid) = self.current_track_data().map(|track| track.mid.clone()) else {
            return;
        };
        let position = if self.loading_track.is_some() || !self.playback_started {
            self.position
        } else {
            self.audio
                .as_ref()
                .map(AudioPlayer::position)
                .unwrap_or(self.position)
        };
        self.position = position;
        let current_playback = PersistedPlayback {
            account_id,
            playlist_id,
            track_mid,
            position_ms: position.as_millis().min(u64::MAX as u128) as u64,
            queue_tracks: self
                .playback_queue
                .as_ref()
                .map(|queue| queue.tracks.clone())
                .unwrap_or_default(),
            queue_modified: self
                .playback_queue
                .as_ref()
                .is_some_and(|queue| queue.modified),
            queue_continuation: self
                .playback_queue
                .as_ref()
                .and_then(|queue| queue.continuation),
        };
        if self.settings.current_playback.as_ref() != Some(&current_playback) {
            self.settings.current_playback = Some(current_playback);
            self.persist_settings();
        }
        self.last_playback_persisted_at = Instant::now();
    }

    fn clear_persisted_playback(&mut self) {
        self.pending_playback_restore = None;
        if self.settings.current_playback.take().is_some() {
            self.persist_settings();
        }
        self.last_playback_persisted_at = Instant::now();
    }

    fn sync_table_playback_state(&mut self, cx: &mut Context<Self>) {
        let current_mid = self.current_track_data().map(|track| track.mid.clone());
        let loading_mid = self
            .loading_track
            .and_then(|index| self.playback_queue.as_ref()?.tracks.get(index))
            .map(|track| track.mid.clone());
        let (playing, loading) = {
            let table = self.track_table.read(cx);
            let tracks = table.delegate().tracks();
            (
                current_mid
                    .as_deref()
                    .and_then(|mid| tracks.iter().position(|track| track.mid == mid)),
                loading_mid
                    .as_deref()
                    .and_then(|mid| tracks.iter().position(|track| track.mid == mid)),
            )
        };
        let playback_active = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
        self.track_table.update(cx, |table, cx| {
            table
                .delegate_mut()
                .set_playback_state(playing, loading, playback_active);
            cx.notify();
        });
    }

    #[cfg(target_os = "linux")]
    fn mpris_snapshot(&self) -> MprisSnapshot {
        let audio_available = self.audio.is_some();
        let loading = self.loading_track.is_some();
        let playback_status = if loading || !self.playback_started {
            MprisPlaybackStatus::Stopped
        } else if self.audio.as_ref().is_some_and(AudioPlayer::is_playing) {
            MprisPlaybackStatus::Playing
        } else {
            MprisPlaybackStatus::Paused
        };
        let (queue_len, current_index) = self
            .playback_queue
            .as_ref()
            .map_or((0, None), |queue| (queue.tracks.len(), self.current_track));
        let can_extend = self
            .playback_queue
            .as_ref()
            .and_then(|queue| queue.continuation)
            .is_some_and(PersistedQueueContinuation::can_load_more);
        let can_go_next = audio_available
            && current_index.is_some_and(|index| {
                can_extend
                    || (self.shuffle && queue_len > 1)
                    || index + 1 < queue_len
                    || (self.repeat_mode == RepeatMode::All && queue_len > 0)
            });
        let can_go_previous = audio_available
            && current_index.is_some_and(|index| {
                self.position >= Duration::from_secs(3)
                    || index > 0
                    || (self.repeat_mode == RepeatMode::All && queue_len > 0)
            });
        let track = self.current_track_data().map(|track| MprisTrack {
            id: mpris_track_id(&track.mid),
            title: track.title.clone(),
            artists: if track.artists.trim().is_empty() {
                Vec::new()
            } else {
                vec![track.artists.clone()]
            },
            album: (!track.album.trim().is_empty()).then(|| track.album.clone()),
            art_url: track.cover_url.clone().filter(|url| !url.trim().is_empty()),
            length_micros: track
                .duration_seconds
                .saturating_mul(1_000_000)
                .min(i64::MAX as u64) as i64,
        });
        let has_track = track.is_some();
        MprisSnapshot {
            playback_status,
            loop_status: match self.repeat_mode {
                RepeatMode::Off => MprisLoopStatus::None,
                RepeatMode::All => MprisLoopStatus::Playlist,
                RepeatMode::One => MprisLoopStatus::Track,
            },
            shuffle: self.shuffle,
            volume: self.settings.volume as f64,
            position_micros: duration_micros(self.position),
            track,
            can_go_next,
            can_go_previous,
            can_play: has_track && audio_available && !loading,
            can_pause: has_track && audio_available && !loading && self.playback_started,
            can_seek: has_track && audio_available && !loading && self.playback_started,
        }
    }

    #[cfg(target_os = "linux")]
    fn sync_mpris(&self, seeked: bool) {
        let Some(mpris) = &self.mpris else {
            return;
        };
        let snapshot = self.mpris_snapshot();
        if seeked {
            mpris.seeked(snapshot);
        } else {
            mpris.update(snapshot);
        }
    }

    #[cfg(target_os = "linux")]
    fn sync_mpris_position(&self) {
        if let Some(mpris) = &self.mpris {
            mpris.update_position(duration_micros(self.position));
        }
    }

    fn sync_progress_slider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.seek_preview.is_none() {
            let progress = self
                .current_duration()
                .map_or(0., |duration| progress_fraction(self.position, duration));
            self.progress_slider.update(cx, |slider, cx| {
                if slider.value().end() != progress {
                    slider.set_value(progress, window, cx);
                }
            });
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let previous_position = self.position;
        if self.seek_preview.is_none() && self.loading_track.is_none() && self.playback_started {
            self.position = self
                .audio
                .as_ref()
                .map(AudioPlayer::position)
                .unwrap_or_default();
        }

        let ended = self.playback_started
            && self.loading_track.is_none()
            && self.audio.as_ref().is_some_and(AudioPlayer::is_empty);
        if ended {
            self.playback_started = false;
            self.play_next(true, cx);
        }
        if self.current_track.is_some()
            && self.last_playback_persisted_at.elapsed() >= PLAYBACK_PERSIST_INTERVAL
        {
            self.persist_current_playback();
        }
        #[cfg(target_os = "linux")]
        if self.current_track.is_some() && self.last_mpris_position_sync.elapsed() >= PROGRESS_TICK
        {
            self.sync_mpris_position();
            self.last_mpris_position_sync = Instant::now();
        }

        if self.position != previous_position && self.progress_hovered {
            cx.notify();
        }
    }

    fn current_track_data(&self) -> Option<&Track> {
        self.current_track.and_then(|index| {
            self.playback_queue
                .as_ref()
                .and_then(|queue| queue.tracks.get(index))
        })
    }

    fn ensure_track_like_state(&mut self, mid: String, cx: &mut Context<Self>) {
        if self.liked_tracks.contains_key(&mid) || self.liked_state_loading.contains(&mid) {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        if let Some(liked) = self.library_cache.track_liked(
            credential.music_id,
            &mid,
            unix_timestamp_secs(),
            LIBRARY_CACHE_TTL,
        ) {
            self.liked_tracks.insert(mid, liked);
            cx.notify();
            return;
        }
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        self.liked_state_loading.insert(mid.clone());
        let account_id = credential.music_id;
        let request_mid = mid.clone();
        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = client.track_liked(&credential, &request_mid).await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.liked_state_loading.remove(&mid);
                if this
                    .credential
                    .as_ref()
                    .is_none_or(|credential| credential.music_id != account_id)
                {
                    return;
                }
                match result {
                    Ok(liked) => {
                        this.liked_tracks.insert(mid.clone(), liked);
                    }
                    Err(error) => {
                        if this
                            .current_track_data()
                            .is_some_and(|track| track.mid == mid)
                        {
                            this.status = StatusMessage::error(format!(
                                "读取当前歌曲的喜欢状态失败：{error:#}"
                            ));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_current_track_liked(&mut self, cx: &mut Context<Self>) {
        let Some(track) = self.current_track_data().cloned() else {
            return;
        };
        let Some(liked) = self.liked_tracks.get(&track.mid).copied() else {
            self.ensure_track_like_state(track.mid.clone(), cx);
            return;
        };
        self.set_track_liked(track, !liked, cx);
    }

    fn unlike_track(&mut self, track: Track, cx: &mut Context<Self>) {
        if self.liked_toggle_loading.contains(&track.mid) {
            return;
        }
        self.liked_tracks.insert(track.mid.clone(), true);
        self.set_track_liked(track, false, cx);
    }

    fn set_track_liked(&mut self, track: Track, liked: bool, cx: &mut Context<Self>) {
        let mid = track.mid.clone();
        if self.liked_toggle_loading.contains(&mid) {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.status = StatusMessage::error("QQ 音乐客户端不可用");
            cx.notify();
            return;
        };
        if track.song_id.is_none() {
            self.status = StatusMessage::error(format!(
                "歌曲“{}”缺少数字 ID，暂时无法修改喜欢状态",
                track.title
            ));
            cx.notify();
            return;
        }
        let account_id = credential.music_id;
        let previous = self.liked_tracks.insert(mid.clone(), liked);
        self.liked_toggle_loading.insert(mid.clone());
        cx.notify();

        let request_track = track.clone();
        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = client
                .set_track_liked(&credential, &request_track, liked)
                .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.liked_toggle_loading.remove(&mid);
                if this
                    .credential
                    .as_ref()
                    .is_none_or(|credential| credential.music_id != account_id)
                {
                    return;
                }
                match result {
                    Ok(()) => {
                        this.apply_track_liked(account_id, track.clone(), liked, cx);
                        this.status = StatusMessage::info(if liked {
                            format!("已喜欢“{}”", track.title)
                        } else {
                            format!("已取消喜欢“{}”", track.title)
                        });
                    }
                    Err(error) => {
                        if this.liked_tracks.get(&mid) == Some(&liked) {
                            if let Some(previous) = previous {
                                this.liked_tracks.insert(mid.clone(), previous);
                            } else {
                                this.liked_tracks.remove(&mid);
                            }
                        }
                        this.status = StatusMessage::error(format!(
                            "{}失败：{error:#}",
                            if liked {
                                "喜欢歌曲"
                            } else {
                                "取消喜欢"
                            }
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_track_liked(
        &mut self,
        account_id: u64,
        track: Track,
        liked: bool,
        cx: &mut Context<Self>,
    ) {
        self.library_cache
            .set_track_liked(account_id, track.clone(), liked);
        self.playlist_list.update(cx, |list, cx| {
            list.delegate_mut().update_liked_track_count(liked);
            cx.notify();
        });
        if self
            .selected_playlist
            .as_ref()
            .is_none_or(|playlist| playlist.id != UserPlaylistId::Liked)
        {
            return;
        }
        if let Some(playlist) = &mut self.selected_playlist {
            playlist.track_count = if liked {
                playlist.track_count.saturating_add(1)
            } else {
                playlist.track_count.saturating_sub(1)
            };
        }
        let page_changed = self.track_table.update(cx, |table, cx| {
            let changed = table.delegate_mut().set_track_liked(track, liked);
            if changed {
                table.clear_selection(cx);
            }
            cx.notify();
            changed
        });
        if page_changed {
            self.page_offset = if liked {
                self.page_offset.saturating_add(1)
            } else {
                self.page_offset.saturating_sub(1)
            };
        }
        self.sync_table_playback_state(cx);
    }

    fn current_duration(&self) -> Option<Duration> {
        self.current_track_data()
            .map(|track| Duration::from_secs(track.duration_seconds))
    }

    fn logout(&mut self, cx: &mut Context<Self>) {
        self.login_generation = self.login_generation.wrapping_add(1);
        self.library_generation = self.library_generation.wrapping_add(1);
        self.playlist_generation = self.playlist_generation.wrapping_add(1);
        self.home_generation = self.home_generation.wrapping_add(1);
        self.queue_generation = self.queue_generation.wrapping_add(1);
        self.play_generation = self.play_generation.wrapping_add(1);
        if let Some(audio) = &self.audio {
            audio.stop();
        }
        self.account_state = AccountState::SignedOut;
        self.credential = None;
        self.liked_tracks.clear();
        self.liked_state_loading.clear();
        self.liked_toggle_loading.clear();
        self.profile = None;
        self.qr_image = None;
        self.library_loading = false;
        self.main_content = MainContent::Home;
        self.navigation_history.clear();
        self.home_playlists.clear();
        self.home_loading = false;
        self.home_loaded = false;
        self.home_error = None;
        self.home_recommendation_loading = None;
        self.selected_playlist_index = None;
        self.selected_playlist = None;
        self.page_loading = false;
        self.playback_queue = None;
        self.queue_recommendation_loading = false;
        self.queue_waiting_for_recommendation = false;
        self.current_track = None;
        self.loading_track = None;
        self.loading_autoplay = false;
        self.resolving_qualities = false;
        self.playback_started = false;
        self.wake_playback_ticks();
        self.playback_location = None;
        self.active_quality = self.settings.playback_quality;
        self.available_qualities.clear();
        self.quality_menu_open = false;
        self.position = Duration::ZERO;
        self.account_menu_open = false;
        self.clear_persisted_playback();
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        self.playlist_list.update(cx, |list, cx| {
            list.delegate_mut().clear();
            cx.notify();
        });
        self.track_table.update(cx, |table, cx| {
            table.delegate_mut().clear();
            table.refresh(cx);
            cx.notify();
        });
        self.status = StatusMessage::info("已退出登录");
        self.begin_login(cx);

        drop(RUNTIME.spawn(async move {
            let _ = tokio::task::spawn_blocking(CredentialStore::delete).await;
        }));
    }

    fn render_login(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let qr = match &self.qr_image {
            Some(image) => img(image.clone())
                .size(px(240.))
                .rounded(theme.radius_lg)
                .into_any_element(),
            None => div()
                .size(px(240.))
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .text_color(theme.muted_foreground)
                .flex()
                .items_center()
                .justify_center()
                .child(match self.account_state {
                    AccountState::Restoring => "正在恢复登录…",
                    AccountState::SigningIn => "正在生成二维码…",
                    _ => "使用 QQ 音乐 App 扫码登录",
                })
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .font(self.fonts.ui.clone())
            .items_center()
            .justify_center()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                v_flex()
                    .w(px(380.))
                    .items_center()
                    .gap_5()
                    .p_8()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.group_box)
                    .shadow_lg()
                    .child(lyrune_icon(self.settings.color_theme, px(46.)))
                    .child(div().text_2xl().font_bold().child("登录 Lyrune"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("登录 QQ 音乐以加载你的歌单"),
                    )
                    .child(
                        div()
                            .id("login-qr")
                            .when(self.account_state == AccountState::SignedOut, |this| {
                                this.cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| this.begin_login(cx)))
                            })
                            .child(qr),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_center()
                            .text_color(theme.muted_foreground)
                            .child(self.status.text.clone()),
                    ),
            )
            .into_any_element()
    }

    fn render_account(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let name = self
            .profile
            .as_ref()
            .map(|profile| profile.nickname.clone())
            .unwrap_or_else(|| "QQ 音乐用户".to_owned());
        let mut avatar = Avatar::new().name(name.clone()).with_size(px(38.));
        if let Some(url) = self
            .profile
            .as_ref()
            .and_then(|profile| profile.avatar_url.clone())
        {
            avatar = avatar.src(cached_image_source(url));
        }

        div()
            .relative()
            .child(
                Button::new("account-avatar")
                    .ghost()
                    .rounded(px(999.))
                    .size(px(44.))
                    .p_0()
                    .tooltip(name.clone())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.account_menu_open = !this.account_menu_open;
                        this.quality_menu_open = false;
                        cx.notify();
                    }))
                    .child(avatar),
            )
            .when(self.account_menu_open, |this| {
                this.child(
                    deferred(
                        v_flex()
                            .absolute()
                            .top(px(46.))
                            .right_0()
                            .w(px(220.))
                            .gap_2()
                            .p_3()
                            .rounded(theme.radius_lg)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.popover)
                            .shadow_lg()
                            .occlude()
                            .child(div().truncate().font_medium().child(name))
                            .child(
                                Button::new("open-settings")
                                    .ghost()
                                    .w_full()
                                    .h(px(44.))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .child(media_icon_hsla(
                                                MediaIcon::Settings,
                                                theme.secondary_foreground,
                                                px(18.),
                                            ))
                                            .child("设置"),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_settings(window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .pt_2()
                                    .child(
                                        Button::new("logout")
                                            .label("退出登录")
                                            .outline()
                                            .w_full()
                                            .h(px(44.))
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.logout(cx)),
                                            ),
                                    ),
                            ),
                    )
                    .with_priority(10),
                )
            })
            .into_any_element()
    }

    fn render_settings_page(&mut self, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let font_field = |label: &'static str, detail: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .items_baseline()
                        .gap_2()
                        .child(div().text_sm().font_medium().child(label))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(detail),
                        ),
                )
                .child(Input::new(input).w_full().h(px(40.)).aria_label(label))
        };
        let selected_theme = self.settings.color_theme;
        let theme_rows = ColorTheme::ALL
            .chunks(2)
            .map(|row| {
                h_flex()
                    .w_full()
                    .gap_1()
                    .children(row.iter().copied().map(|color_theme| {
                        Button::new(format!("settings-theme-{}", color_theme.id()))
                            .label(color_theme.label())
                            .ghost()
                            .flex_1()
                            .h(px(38.))
                            .selected(selected_theme == color_theme)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_color_theme(color_theme, window, cx)
                            }))
                    }))
            })
            .collect::<Vec<_>>();
        let preferred_quality = self.settings.playback_quality;
        let quality_rows = Quality::ALL
            .chunks(2)
            .map(|row| {
                h_flex()
                    .w_full()
                    .gap_1()
                    .children(row.iter().copied().map(|quality| {
                        Button::new(format!("settings-quality-{}", quality.cache_id()))
                            .label(quality.label())
                            .ghost()
                            .flex_1()
                            .h(px(38.))
                            .selected(preferred_quality == quality)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_preferred_playback_quality(quality, cx)
                            }))
                    }))
            })
            .collect::<Vec<_>>();
        let selected_highlight_frame_rate = self.settings.lyric_highlight_frame_rate;
        let highlight_frame_rate_buttons = LyricFrameRate::ALL
            .into_iter()
            .map(|frame_rate| {
                Button::new(format!("lyrics-highlight-{}", frame_rate.id()))
                    .label(frame_rate.label())
                    .ghost()
                    .flex_1()
                    .h(px(36.))
                    .selected(selected_highlight_frame_rate == frame_rate)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_lyric_highlight_frame_rate(frame_rate, window, cx)
                    }))
            })
            .collect::<Vec<_>>();
        let selected_scroll_frame_rate = self.settings.lyric_scroll_frame_rate;
        let scroll_frame_rate_buttons = LyricFrameRate::ALL
            .into_iter()
            .map(|frame_rate| {
                Button::new(format!("lyrics-scroll-{}", frame_rate.id()))
                    .label(frame_rate.label())
                    .ghost()
                    .flex_1()
                    .h(px(36.))
                    .selected(selected_scroll_frame_rate == frame_rate)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_lyric_scroll_frame_rate(frame_rate, window, cx)
                    }))
            })
            .collect::<Vec<_>>();
        div()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                h_flex().w_full().items_start().justify_center().child(
                    v_flex()
                        .w_full()
                        .max_w(px(760.))
                        .gap_6()
                        .px_6()
                        .pt_8()
                        .pb_8()
                        .child(
                            div()
                                .text_size(if narrow { px(22.) } else { px(24.) })
                                .font_semibold()
                                .child("设置"),
                        )
                        .child(
                            v_flex()
                                .gap_4()
                                .child(
                                    v_flex()
                                        .gap_2()
                                        .child(div().font_medium().child("主题配色"))
                                        .children(theme_rows),
                                )
                                .child(
                                    v_flex()
                                        .gap_3()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(div().font_medium().child("字体"))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child("按优先级填写字体名称，以逗号分隔"),
                                        )
                                        .child(font_field(
                                            "UI 字体",
                                            "应用于整个界面",
                                            &self.ui_font_input,
                                        ))
                                        .child(font_field(
                                            "等宽字体",
                                            "应用于时间等定宽文本",
                                            &self.monospace_font_input,
                                        ))
                                        .child(font_field(
                                            "歌词字体",
                                            "应用于主歌词、翻译与注音",
                                            &self.lyric_font_input,
                                        ))
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .justify_end()
                                                .gap_2()
                                                .child(
                                                    Button::new("reset-font-settings")
                                                        .label("恢复默认")
                                                        .outline()
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.reset_font_inputs(window, cx)
                                                            },
                                                        )),
                                                )
                                                .child(
                                                    Button::new("apply-font-settings")
                                                        .label("应用字体")
                                                        .primary()
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.apply_font_settings(window, cx)
                                                            },
                                                        )),
                                                ),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .gap_2()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(div().font_medium().child("首选播放音质"))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child("应用于后续加载的歌曲"),
                                        )
                                        .children(quality_rows),
                                )
                                .child(
                                    v_flex()
                                        .gap_2()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(div().font_medium().child("歌曲缓存上限"))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child("超过上限时自动清理最久未播放的歌曲"),
                                        )
                                        .child(
                                            NumberInput::new(&self.audio_cache_limit_input)
                                                .w(px(180.))
                                                .h(px(40.))
                                                .suffix(
                                                    div()
                                                        .pr_2()
                                                        .text_sm()
                                                        .text_color(theme.secondary_foreground)
                                                        .child("GB"),
                                                ),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .gap_2()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(div().font_medium().child("歌词动画帧率"))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child("默认表示跟随显示器刷新率；越高越流畅，也会增加资源消耗"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.secondary_foreground)
                                                .child("逐字高亮与横向跟随"),
                                        )
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_1()
                                                .children(highlight_frame_rate_buttons),
                                        )
                                        .child(
                                            div()
                                                .pt_1()
                                                .text_xs()
                                                .text_color(theme.secondary_foreground)
                                                .child("平滑滚动"),
                                        )
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_1()
                                                .children(scroll_frame_rate_buttons),
                                        ),
                                ),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .w_full()
            .h_full()
            .flex_shrink_0()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .h(px(64.))
                    .mb_2()
                    .px_5()
                    .gap_3()
                    .child(lyrune_icon(self.settings.color_theme, px(42.)))
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().font_semibold().child("Lyrune"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("QQ Music Player"),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .h(px(60.))
                    .px_5()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .size(px(34.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(media_icon_hsla(
                                        MediaIcon::Library,
                                        theme.secondary_foreground,
                                        px(20.),
                                    )),
                            )
                            .child(
                                v_flex()
                                    .gap_0p5()
                                    .child(div().font_semibold().child("音乐库"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("你的 QQ 音乐歌单"),
                                    ),
                            ),
                    )
                    .child(
                        Button::new("reload-library")
                            .ghost()
                            .rounded(px(999.))
                            .size(px(44.))
                            .p_0()
                            .tooltip("重新加载歌单")
                            .disabled(self.library_loading)
                            .loading(self.library_loading)
                            .when(!self.library_loading, |button| {
                                button.child(media_icon_hsla(
                                    MediaIcon::Refresh,
                                    theme.secondary_foreground,
                                    px(18.),
                                ))
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.load_library(true, cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_3()
                    .pb_3()
                    .child(List::new(&self.playlist_list).size_full()),
            )
            .when(self.status.is_error, |sidebar| {
                sidebar.child(
                    div()
                        .mx_3()
                        .mb_3()
                        .px_3()
                        .py_2()
                        .rounded(px(9.))
                        .bg(theme.background.opacity(0.55))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(
                            h_flex()
                                .items_start()
                                .gap_2()
                                .child(
                                    div()
                                        .mt(px(5.))
                                        .size(px(6.))
                                        .flex_shrink_0()
                                        .rounded(px(999.))
                                        .bg(theme.danger),
                                )
                                .child(div().line_clamp(2).child(self.status.text.clone())),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_playlist_header(
        &mut self,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let Some(playlist) = self.selected_playlist.clone() else {
            return div()
                .h(px(220.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("从左侧选择一个歌单")
                .into_any_element();
        };
        let cover_size = if narrow {
            px(112.)
        } else if compact {
            px(142.)
        } else {
            px(176.)
        };
        let cover = div().rounded(px(18.)).shadow_md().child(playlist_cover(
            &playlist,
            cover_size,
            px(18.),
            cx,
        ));
        let owned_by_profile = matches!(
            &playlist.id,
            UserPlaylistId::Liked | UserPlaylistId::Created { .. }
        );
        let owner = (!playlist.owner.is_empty())
            .then(|| playlist.owner.clone())
            .or_else(|| {
                if owned_by_profile {
                    self.profile
                        .as_ref()
                        .map(|profile| profile.nickname.clone())
                } else {
                    None
                }
            });
        let owner_avatar_url = playlist.owner_avatar_url.clone().or_else(|| {
            owned_by_profile
                .then(|| self.profile.as_ref()?.avatar_url.clone())
                .flatten()
        });
        let owner_identity = owner.zip(owner_avatar_url);
        let has_owner = owner_identity.is_some();
        let has_tracks = !self.track_table.read(cx).delegate().tracks().is_empty();
        let long_title = playlist_title_is_long(&playlist.title);
        let title_size = if long_title {
            if narrow {
                px(22.)
            } else if compact {
                px(28.)
            } else {
                px(36.)
            }
        } else if narrow {
            px(28.)
        } else if compact {
            px(34.)
        } else {
            px(44.)
        };
        let title_line_height = if long_title {
            if narrow {
                px(29.)
            } else if compact {
                px(36.)
            } else {
                px(46.)
            }
        } else if narrow {
            px(36.)
        } else if compact {
            px(44.)
        } else {
            px(56.)
        };
        div()
            .min_h(if narrow {
                px(190.)
            } else if compact {
                px(214.)
            } else {
                px(246.)
            })
            .w_full()
            .flex_shrink_0()
            .px_6()
            .pt_4()
            .pb_5()
            .child(
                h_flex()
                    .w_full()
                    .items_end()
                    .gap(if narrow {
                        px(16.)
                    } else if compact {
                        px(20.)
                    } else {
                        px(28.)
                    })
                    .child(cover)
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_2()
                            .child(
                                h_flex().child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded(px(999.))
                                        .bg(theme.muted)
                                        .text_xs()
                                        .font_medium()
                                        .text_color(theme.muted_foreground)
                                        .child(match &playlist.id {
                                            UserPlaylistId::Artist { .. } => "歌手",
                                            UserPlaylistId::Album { .. } => "专辑",
                                            _ => "歌单",
                                        }),
                                ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .w_full()
                                    .when_else(
                                        long_title,
                                        |title| title.line_clamp(3),
                                        |title| title.truncate(),
                                    )
                                    .text_size(title_size)
                                    .line_height(title_line_height)
                                    .font_semibold()
                                    .child(playlist.title),
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_2()
                                    .text_sm()
                                    .font_medium()
                                    .when_some(owner_identity, |this, (owner, url)| {
                                        this.child(
                                            img(cached_image_source(url))
                                                .size(px(18.))
                                                .flex_shrink_0()
                                                .rounded(px(999.)),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .max_w(if narrow { px(120.) } else { px(200.) })
                                                .truncate()
                                                .child(owner),
                                        )
                                    })
                                    .child(
                                        div()
                                            .font_normal()
                                            .text_color(theme.secondary_foreground)
                                            .child(format!(
                                                "{}{} 首歌曲",
                                                if has_owner { "· " } else { "" },
                                                playlist.track_count
                                            )),
                                    ),
                            )
                            .child(
                                h_flex().pt_2().child(
                                    Button::new("play-all")
                                        .primary()
                                        .rounded(px(999.))
                                        .h(px(44.))
                                        .min_w(px(44.))
                                        .px_4()
                                        .tooltip("从第一首开始播放")
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .text_color(theme.primary_foreground)
                                                .child(media_icon(
                                                    MediaIcon::Play,
                                                    self.settings.color_theme.icon_on_accent(),
                                                    px(17.),
                                                ))
                                                .child("播放全部"),
                                        )
                                        .when(!has_tracks, |button| button.bg(theme.button_primary))
                                        .disabled(!has_tracks)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.select_track(0, cx)),
                                        ),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_playlist_content(
        &mut self,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme.background)
            .child(self.render_playlist_header(compact, narrow, cx))
            .child(
                div().flex_1().min_h_0().px_5().pb_4().child(
                    div()
                        .size_full()
                        .overflow_hidden()
                        .bg(theme.background)
                        .child(
                            DataTable::new(&self.track_table)
                                .bordered(false)
                                .stripe(false)
                                .with_size(px(64.)),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_artist_header(
        &mut self,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(artist) = self.selected_artist.clone() else {
            return div().into_any_element();
        };
        let cover_size = if narrow {
            px(112.)
        } else if compact {
            px(142.)
        } else {
            px(176.)
        };
        let cover = div()
            .rounded(px(999.))
            .shadow_md()
            .child(self.render_search_cover(
                artist.cover_url,
                MediaIcon::Artist,
                cover_size,
                px(999.),
                cx,
            ));
        let track_count = self.artist_track_count;
        let has_tracks = self
            .artist_songs
            .as_ref()
            .is_some_and(|songs| !songs.items.is_empty());

        div()
            .h(if narrow {
                px(190.)
            } else if compact {
                px(214.)
            } else {
                px(246.)
            })
            .w_full()
            .px_6()
            .pt_4()
            .pb_5()
            .child(
                h_flex()
                    .size_full()
                    .items_end()
                    .gap(if narrow {
                        px(16.)
                    } else if compact {
                        px(20.)
                    } else {
                        px(28.)
                    })
                    .child(cover)
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_2()
                            .child(
                                h_flex().child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded(px(999.))
                                        .bg(theme.muted)
                                        .text_xs()
                                        .font_medium()
                                        .text_color(theme.muted_foreground)
                                        .child("歌手"),
                                ),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(if narrow {
                                        px(34.)
                                    } else if compact {
                                        px(40.)
                                    } else {
                                        px(52.)
                                    })
                                    .line_height(if narrow {
                                        px(45.)
                                    } else if compact {
                                        px(52.)
                                    } else {
                                        px(68.)
                                    })
                                    .font_semibold()
                                    .child(artist.name),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .text_color(theme.secondary_foreground)
                                    .child(if track_count == 0 {
                                        "歌曲与专辑".to_owned()
                                    } else {
                                        format!("{track_count} 首歌曲")
                                    }),
                            )
                            .child(
                                h_flex().pt_2().child(
                                    Button::new("play-all-artist")
                                        .primary()
                                        .rounded(px(999.))
                                        .h(px(44.))
                                        .min_w(px(44.))
                                        .px_4()
                                        .tooltip("从第一首开始播放")
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .text_color(theme.primary_foreground)
                                                .child(media_icon(
                                                    MediaIcon::Play,
                                                    self.settings.color_theme.icon_on_accent(),
                                                    px(17.),
                                                ))
                                                .child("播放全部"),
                                        )
                                        .when(!has_tracks, |button| button.bg(theme.button_primary))
                                        .disabled(!has_tracks)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.select_artist_track(0, cx)
                                        })),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_artist_content(
        &mut self,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let songs = self.artist_songs.clone();
        let song_has_more = songs.as_ref().is_some_and(|page| page.has_more);
        let song_body = if self.artist_songs_loading {
            h_flex()
                .h(px(92.))
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().with_size(px(22.)).color(theme.primary))
                .child("正在加载歌曲…")
                .into_any_element()
        } else if let Some(error) = self.artist_song_error.clone() {
            h_flex()
                .h(px(92.))
                .items_center()
                .justify_center()
                .gap_4()
                .text_color(theme.muted_foreground)
                .child(error)
                .child(
                    Button::new("retry-artist-songs")
                        .outline()
                        .h(px(40.))
                        .px_4()
                        .label("重新加载")
                        .on_click(cx.listener(|this, _, _, cx| this.load_artist_songs(false, cx))),
                )
                .into_any_element()
        } else if let Some(songs) = songs.filter(|page| !page.items.is_empty()) {
            self.render_song_rows(songs.items, narrow, SongRowSource::Artist, cx)
        } else {
            h_flex()
                .h(px(72.))
                .items_center()
                .text_color(theme.muted_foreground)
                .child("暂无歌曲")
                .into_any_element()
        };

        let albums = self.artist_albums.clone();
        let album_has_more = albums.as_ref().is_some_and(|page| page.has_more);
        let album_body = if self.artist_albums_loading {
            h_flex()
                .h(px(120.))
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().with_size(px(22.)).color(theme.primary))
                .child("正在加载专辑…")
                .into_any_element()
        } else if let Some(error) = self.artist_album_error.clone() {
            h_flex()
                .h(px(120.))
                .items_center()
                .justify_center()
                .gap_4()
                .text_color(theme.muted_foreground)
                .child(error)
                .child(
                    Button::new("retry-artist-albums")
                        .outline()
                        .h(px(40.))
                        .px_4()
                        .label("重新加载")
                        .on_click(cx.listener(|this, _, _, cx| this.load_artist_albums(false, cx))),
                )
                .into_any_element()
        } else if let Some(albums) = albums.filter(|page| !page.items.is_empty()) {
            self.render_search_cards(
                SearchCategory::Albums,
                Vec::new(),
                albums.items,
                Vec::new(),
                compact,
                cx,
            )
        } else {
            h_flex()
                .h(px(72.))
                .items_center()
                .text_color(theme.muted_foreground)
                .child("暂无专辑")
                .into_any_element()
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme.background)
            .child(
                div().flex_1().min_h_0().overflow_y_scrollbar().child(
                    v_flex()
                        .w_full()
                        .child(self.render_artist_header(compact, narrow, cx))
                        .child(
                            v_flex()
                                .w_full()
                                .px(if narrow { px(20.) } else { px(24.) })
                                .pb_10()
                                .gap_10()
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap_3()
                                        .child(
                                            div().text_size(px(24.)).font_semibold().child("歌曲"),
                                        )
                                        .child(song_body)
                                        .when(song_has_more, |this| {
                                            this.child(
                                                h_flex().child(
                                                    Button::new("load-more-artist-songs")
                                                        .outline()
                                                        .h(px(40.))
                                                        .px_4()
                                                        .loading(self.artist_songs_loading_more)
                                                        .disabled(self.artist_songs_loading_more)
                                                        .label(if self.artist_songs_loading_more {
                                                            "正在加载…"
                                                        } else {
                                                            "查看更多"
                                                        })
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.load_artist_songs(true, cx)
                                                        })),
                                                ),
                                            )
                                        }),
                                )
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap_4()
                                        .child(
                                            div().text_size(px(24.)).font_semibold().child("专辑"),
                                        )
                                        .child(album_body)
                                        .when(album_has_more, |this| {
                                            this.child(
                                                h_flex().child(
                                                    Button::new("load-more-artist-albums")
                                                        .outline()
                                                        .h(px(40.))
                                                        .px_4()
                                                        .loading(self.artist_albums_loading_more)
                                                        .disabled(self.artist_albums_loading_more)
                                                        .label(if self.artist_albums_loading_more {
                                                            "正在加载…"
                                                        } else {
                                                            "查看更多"
                                                        })
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.load_artist_albums(true, cx)
                                                        })),
                                                ),
                                            )
                                        }),
                                ),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_home(&mut self, compact: bool, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if self.home_loading && self.home_playlists.is_empty() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().with_size(px(24.)).color(theme.primary))
                .child("正在加载主页推荐…")
                .into_any_element();
        }
        if self.home_playlists.is_empty()
            && let Some(error) = self.home_error.clone()
        {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_4()
                .text_color(theme.muted_foreground)
                .child(error)
                .child(
                    Button::new("retry-home")
                        .outline()
                        .h(px(44.))
                        .px_4()
                        .label("重新加载")
                        .on_click(cx.listener(|this, _, _, cx| this.load_home(cx))),
                )
                .into_any_element();
        }
        if self.home_loaded && self.home_playlists.is_empty() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("QQ 音乐暂时没有返回可显示的歌单推荐")
                .into_any_element();
        }

        let cover_size = if narrow {
            px(132.)
        } else if compact {
            px(148.)
        } else {
            px(168.)
        };
        let card_width = cover_size + px(16.);
        let feature_width = if narrow {
            px(304.)
        } else if compact {
            px(344.)
        } else {
            px(384.)
        };
        let grid_width = if narrow {
            px(640.)
        } else if compact {
            px(704.)
        } else {
            px(784.)
        };
        let cards = self
            .home_playlists
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, playlist)| {
                let cover = playlist_cover(&playlist, cover_size, px(14.), cx);
                let title = playlist.title.clone();
                let subtitle = if playlist.description.is_empty() {
                    "为你推荐".to_owned()
                } else {
                    playlist.description.clone()
                };
                Button::new(format!("home-playlist-{index}"))
                    .ghost()
                    .w(card_width)
                    .h(cover_size + px(74.))
                    .p_2()
                    .rounded(px(12.))
                    .tooltip(title.clone())
                    .child(
                        v_flex()
                            .size_full()
                            .items_start()
                            .gap_2()
                            .child(div().rounded(px(14.)).shadow_sm().child(cover))
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .font_medium()
                                    .text_color(theme.foreground)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(subtitle),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_home_playlist(playlist.clone(), window, cx)
                    }))
            })
            .collect::<Vec<_>>();
        let recommendation_loading = self.home_recommendation_loading.is_some();
        let radar_icon = media_icon_hsla(MediaIcon::Radar, theme.primary, px(25.));
        let guess_icon = media_icon_hsla(MediaIcon::Headphones, theme.primary, px(25.));
        let recommendation_cards = [
            Button::new("home-radar")
                .ghost()
                .w(feature_width)
                .h(px(92.))
                .p_4()
                .rounded(px(12.))
                .bg(theme.muted.opacity(0.7))
                .when(recommendation_loading, |button| {
                    button
                        .bg(theme.muted.opacity(0.7))
                        .text_color(theme.secondary_foreground)
                })
                .disabled(recommendation_loading)
                .child(
                    h_flex()
                        .size_full()
                        .gap_4()
                        .child(
                            div()
                                .size(px(48.))
                                .flex_shrink_0()
                                .rounded(px(10.))
                                .bg(theme.background.opacity(0.55))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(radar_icon),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .items_start()
                                .gap_1()
                                .child(div().font_semibold().child("专属雷达"))
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("不断更新的个性推荐"),
                                ),
                        ),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.start_home_recommendation(RecommendationKind::Radar, cx)
                })),
            Button::new("home-guess")
                .ghost()
                .w(feature_width)
                .h(px(92.))
                .p_4()
                .rounded(px(12.))
                .bg(theme.muted.opacity(0.7))
                .when(recommendation_loading, |button| {
                    button
                        .bg(theme.muted.opacity(0.7))
                        .text_color(theme.secondary_foreground)
                })
                .disabled(recommendation_loading)
                .child(
                    h_flex()
                        .size_full()
                        .gap_4()
                        .child(
                            div()
                                .size(px(48.))
                                .flex_shrink_0()
                                .rounded(px(10.))
                                .bg(theme.background.opacity(0.55))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(guess_icon),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .items_start()
                                .gap_1()
                                .child(div().font_semibold().child("猜你喜欢"))
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("持续生成的个性漫游"),
                                ),
                        ),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.start_home_recommendation(RecommendationKind::Guess, cx)
                })),
        ];

        div()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .w_full()
                    .px(if narrow { px(20.) } else { px(28.) })
                    .pt(if narrow { px(22.) } else { px(32.) })
                    .pb_8()
                    .child(
                        h_flex().w_full().justify_center().child(
                            v_flex()
                                .w_full()
                                .max_w(grid_width)
                                .gap_6()
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap_3()
                                        .child(
                                            div()
                                                .px_2()
                                                .text_size(if narrow { px(22.) } else { px(24.) })
                                                .font_semibold()
                                                .child("智能推荐"),
                                        )
                                        .child(
                                            h_flex()
                                                .items_start()
                                                .flex_wrap()
                                                .gap_4()
                                                .children(recommendation_cards),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .w_full()
                                        .px_2()
                                        .child(
                                            div()
                                                .text_size(if narrow { px(22.) } else { px(24.) })
                                                .font_semibold()
                                                .child("今日歌单"),
                                        )
                                        .when_some(self.home_error.clone(), |header, error| {
                                            header.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(error),
                                            )
                                        }),
                                )
                                .child(h_flex().items_start().flex_wrap().gap_4().children(cards)),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_search_cover(
        &self,
        cover_url: Option<String>,
        icon: MediaIcon,
        size: Pixels,
        radius: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        match cover_url {
            Some(url) => img(cached_image_source(url))
                .size(size)
                .flex_shrink_0()
                .rounded(radius)
                .into_any_element(),
            None => div()
                .size(size)
                .flex_shrink_0()
                .rounded(radius)
                .bg(theme.muted)
                .flex()
                .items_center()
                .justify_center()
                .child(media_icon_hsla(icon, theme.muted_foreground, size * 0.38))
                .into_any_element(),
        }
    }

    fn render_search_songs(
        &mut self,
        songs: Vec<Track>,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_song_rows(songs, narrow, SongRowSource::Search, cx)
    }

    fn render_song_rows(
        &mut self,
        songs: Vec<Track>,
        narrow: bool,
        source: SongRowSource,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let monospace_font = self.fonts.monospace.clone();
        let current_mid = self.current_track_data().map(|track| track.mid.clone());
        let loading_mid = self
            .loading_track
            .and_then(|index| self.playback_queue.as_ref()?.tracks.get(index))
            .map(|track| track.mid.clone());
        let is_playing = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
        let rows = songs
            .into_iter()
            .enumerate()
            .map(|(index, track)| {
                let is_current = current_mid.as_deref() == Some(track.mid.as_str());
                let is_loading = loading_mid.as_deref() == Some(track.mid.as_str());
                let title = track.title.clone();
                let artists = track.artists.clone();
                let album = track.album.clone();
                let duration = format_duration(track.duration_seconds);
                let cover = self.render_search_cover(
                    track.cover_url,
                    MediaIcon::Music,
                    px(48.),
                    px(9.),
                    cx,
                );
                Button::new(format!(
                    "{}-song-{}-{}",
                    match source {
                        SongRowSource::Search => "search",
                        SongRowSource::Artist => "artist",
                    },
                    track.mid,
                    index
                ))
                .ghost()
                .w_full()
                .h(px(68.))
                .px_3()
                .rounded(px(10.))
                .selected(is_current)
                .tooltip(format!("播放 {title}"))
                .child(
                    h_flex()
                        .size_full()
                        .gap_3()
                        .child(
                            div()
                                .w(px(28.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .when_else(
                                    is_loading,
                                    |this| {
                                        this.child(
                                            Spinner::new().with_size(px(17.)).color(theme.primary),
                                        )
                                    },
                                    |this| {
                                        this.child(media_icon_hsla(
                                            if is_current && is_playing {
                                                MediaIcon::Pause
                                            } else {
                                                MediaIcon::Play
                                            },
                                            if is_current {
                                                theme.primary
                                            } else {
                                                theme.muted_foreground
                                            },
                                            px(17.),
                                        ))
                                    },
                                ),
                        )
                        .child(cover)
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap_0p5()
                                .child(
                                    div()
                                        .truncate()
                                        .font_medium()
                                        .text_color(if is_current {
                                            theme.primary
                                        } else {
                                            theme.foreground
                                        })
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(artists),
                                ),
                        )
                        .when(!narrow, |row| {
                            row.child(
                                div()
                                    .w(px(300.))
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .text_color(theme.secondary_foreground)
                                    .child(album),
                            )
                        })
                        .child(
                            div()
                                .w(px(52.))
                                .flex_shrink_0()
                                .text_right()
                                .font(monospace_font.clone())
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(duration),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| match source {
                    SongRowSource::Search => this.select_search_track(index, cx),
                    SongRowSource::Artist => this.select_artist_track(index, cx),
                }))
            })
            .collect::<Vec<_>>();
        v_flex().w_full().gap_1().children(rows).into_any_element()
    }

    fn render_search_cards(
        &mut self,
        category: SearchCategory,
        artists: Vec<SearchArtist>,
        albums: Vec<SearchAlbum>,
        playlists: Vec<UserPlaylist>,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let cover_size = if compact { px(132.) } else { px(148.) };
        let card_width = cover_size + px(16.);
        let cards = match category {
            SearchCategory::Artists => artists
                .into_iter()
                .enumerate()
                .map(|(index, artist)| {
                    let title = artist.name.clone();
                    let cover = self.render_search_cover(
                        artist.cover_url.clone(),
                        MediaIcon::Artist,
                        cover_size,
                        px(999.),
                        cx,
                    );
                    Button::new(format!("search-artist-{index}"))
                        .ghost()
                        .w(card_width)
                        .h(cover_size + px(62.))
                        .p_2()
                        .rounded(px(12.))
                        .tooltip(title.clone())
                        .child(
                            v_flex()
                                .size_full()
                                .items_center()
                                .gap_3()
                                .child(cover)
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .text_center()
                                        .font_medium()
                                        .text_color(theme.foreground)
                                        .child(title),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_search_artist(artist.clone(), window, cx)
                        }))
                        .into_any_element()
                })
                .collect::<Vec<_>>(),
            SearchCategory::Albums => albums
                .into_iter()
                .enumerate()
                .map(|(index, album)| {
                    let title = album.title.clone();
                    let subtitle = album.artist.clone();
                    let cover = self.render_search_cover(
                        album.cover_url.clone(),
                        MediaIcon::Album,
                        cover_size,
                        px(12.),
                        cx,
                    );
                    let playlist = album.into_playlist();
                    Button::new(format!("search-album-{index}"))
                        .ghost()
                        .w(card_width)
                        .h(cover_size + px(74.))
                        .p_2()
                        .rounded(px(12.))
                        .tooltip(title.clone())
                        .child(
                            v_flex()
                                .size_full()
                                .items_start()
                                .gap_2()
                                .child(cover)
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .font_medium()
                                        .text_color(theme.foreground)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(subtitle),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_home_playlist(playlist.clone(), window, cx)
                        }))
                        .into_any_element()
                })
                .collect::<Vec<_>>(),
            SearchCategory::Playlists => playlists
                .into_iter()
                .enumerate()
                .map(|(index, playlist)| {
                    let title = playlist.title.clone();
                    let subtitle = if playlist.owner.is_empty() {
                        "QQ 音乐歌单".to_owned()
                    } else {
                        playlist.owner.clone()
                    };
                    let cover = self.render_search_cover(
                        playlist.cover_url.clone(),
                        MediaIcon::Playlist,
                        cover_size,
                        px(12.),
                        cx,
                    );
                    Button::new(format!("search-playlist-{index}"))
                        .ghost()
                        .w(card_width)
                        .h(cover_size + px(74.))
                        .p_2()
                        .rounded(px(12.))
                        .tooltip(title.clone())
                        .child(
                            v_flex()
                                .size_full()
                                .items_start()
                                .gap_2()
                                .child(cover)
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .font_medium()
                                        .text_color(theme.foreground)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(subtitle),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_home_playlist(playlist.clone(), window, cx)
                        }))
                        .into_any_element()
                })
                .collect::<Vec<_>>(),
            SearchCategory::Songs => Vec::new(),
        };
        h_flex()
            .w_full()
            .items_start()
            .flex_wrap()
            .gap_4()
            .children(cards)
            .into_any_element()
    }

    fn render_search(&mut self, compact: bool, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let active_category = self.search_category;
        let tabs = SearchCategory::ALL
            .into_iter()
            .map(|category| {
                Button::new(format!("search-category-{}", category.label()))
                    .ghost()
                    .h(px(40.))
                    .px_3()
                    .rounded(px(9.))
                    .selected(active_category == category)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(media_icon_hsla(
                                category.icon(),
                                if active_category == category {
                                    theme.primary
                                } else {
                                    theme.secondary_foreground
                                },
                                px(17.),
                            ))
                            .child(category.label()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.search_category = category;
                        this.search_error = None;
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();

        let mut content = v_flex().w_full();
        let mut has_more = false;
        let mut is_empty = false;
        if let Some(results) = self.search_results.clone() {
            match self.search_category {
                SearchCategory::Songs => {
                    has_more = results.songs.has_more;
                    is_empty = results.songs.items.is_empty();
                    content =
                        content.child(self.render_search_songs(results.songs.items, narrow, cx));
                }
                SearchCategory::Artists => {
                    has_more = results.artists.has_more;
                    is_empty = results.artists.items.is_empty();
                    content = content.child(self.render_search_cards(
                        SearchCategory::Artists,
                        results.artists.items,
                        Vec::new(),
                        Vec::new(),
                        compact,
                        cx,
                    ));
                }
                SearchCategory::Albums => {
                    has_more = results.albums.has_more;
                    is_empty = results.albums.items.is_empty();
                    content = content.child(self.render_search_cards(
                        SearchCategory::Albums,
                        Vec::new(),
                        results.albums.items,
                        Vec::new(),
                        compact,
                        cx,
                    ));
                }
                SearchCategory::Playlists => {
                    has_more = results.playlists.has_more;
                    is_empty = results.playlists.items.is_empty();
                    content = content.child(self.render_search_cards(
                        SearchCategory::Playlists,
                        Vec::new(),
                        Vec::new(),
                        results.playlists.items,
                        compact,
                        cx,
                    ));
                }
            }
        }

        v_flex()
            .flex_1()
            .min_h_0()
            .bg(theme.background)
            .child(
                div().flex_1().min_h_0().overflow_y_scrollbar().child(
                    v_flex()
                        .w_full()
                        .max_w(px(1120.))
                        .mx_auto()
                        .px(if narrow { px(20.) } else { px(32.) })
                        .pt(if narrow { px(20.) } else { px(28.) })
                        .pb_8()
                        .gap_5()
                        .child(
                            v_flex().child(
                                div()
                                    .text_size(if narrow { px(22.) } else { px(24.) })
                                    .font_semibold()
                                    .child(format!("搜索“{}”", self.search_query)),
                            ),
                        )
                        .child(h_flex().gap_1().children(tabs))
                        .when(self.search_loading, |this| {
                            this.child(
                                v_flex()
                                    .h(px(260.))
                                    .items_center()
                                    .justify_center()
                                    .gap_3()
                                    .text_color(theme.muted_foreground)
                                    .child(Spinner::new().with_size(px(24.)).color(theme.primary))
                                    .child("正在搜索…"),
                            )
                        })
                        .when(
                            !self.search_loading
                                && self.search_results.is_none()
                                && self.search_error.is_some(),
                            |this| {
                                this.child(
                                    v_flex()
                                        .h(px(260.))
                                        .items_center()
                                        .justify_center()
                                        .gap_4()
                                        .text_color(theme.muted_foreground)
                                        .child(self.search_error.clone().unwrap_or_default())
                                        .child(
                                            Button::new("retry-search")
                                                .outline()
                                                .h(px(44.))
                                                .px_4()
                                                .label("重新搜索")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.start_search(this.search_query.clone(), cx)
                                                })),
                                        ),
                                )
                            },
                        )
                        .when(
                            !self.search_loading && self.search_results.is_some() && is_empty,
                            |this| {
                                this.child(
                                    div()
                                        .h(px(220.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(theme.muted_foreground)
                                        .child(format!(
                                            "没有找到相关{}",
                                            self.search_category.label()
                                        )),
                                )
                            },
                        )
                        .when(
                            !self.search_loading && self.search_results.is_some() && !is_empty,
                            |this| this.child(content),
                        )
                        .when(
                            self.search_results.is_some() && self.search_error.is_some(),
                            |this| {
                                this.child(
                                    div()
                                        .px_3()
                                        .py_2()
                                        .rounded(px(9.))
                                        .bg(theme.danger.opacity(0.1))
                                        .text_sm()
                                        .text_color(theme.danger)
                                        .child(self.search_error.clone().unwrap_or_default()),
                                )
                            },
                        )
                        .when(has_more, |this| {
                            this.child(
                                h_flex().w_full().justify_center().pt_2().child(
                                    Button::new("load-more-search")
                                        .outline()
                                        .h(px(44.))
                                        .px_5()
                                        .label("加载更多")
                                        .loading(self.search_loading_more)
                                        .disabled(self.search_loading_more)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.load_more_search(cx)),
                                        ),
                                ),
                            )
                        }),
                ),
            )
            .into_any_element()
    }

    fn render_quality_selector(&mut self, has_track: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let active_quality = self.active_quality;
        let active_label = active_quality.badge_label();
        let options = self
            .available_qualities
            .iter()
            .copied()
            .map(|quality| {
                Button::new(format!("quality-{}", quality.cache_id()))
                    .label(quality.label())
                    .ghost()
                    .w_full()
                    .h(px(44.))
                    .selected(quality == active_quality)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_playback_quality(quality, cx)),
                    )
            })
            .collect::<Vec<_>>();

        div()
            .relative()
            .child(
                Button::new("quality-selector")
                    .label(active_label)
                    .outline()
                    .w(px(92.))
                    .h(px(34.))
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .rounded(px(7.))
                    .tooltip("切换音质")
                    .when(self.loading_track.is_some(), |button| {
                        button
                            .bg(theme.input_background())
                            .border_color(theme.input)
                            .text_color(theme.button_foreground)
                    })
                    .disabled(
                        !has_track
                            || self.loading_track.is_some()
                            || self.available_qualities.is_empty(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.quality_menu_open = !this.quality_menu_open;
                        this.account_menu_open = false;
                        cx.notify();
                    })),
            )
            .when(self.quality_menu_open, |this| {
                this.child(
                    deferred(
                        v_flex()
                            .absolute()
                            .bottom(px(42.))
                            .right_0()
                            .w(px(220.))
                            .gap_1()
                            .p_2()
                            .rounded(theme.radius_lg)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.popover)
                            .shadow_lg()
                            .occlude()
                            .child(
                                div()
                                    .px_2()
                                    .pb_1()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(theme.muted_foreground)
                                    .child("当前歌曲可用音质"),
                            )
                            .children(options),
                    )
                    .with_priority(20),
                )
            })
            .into_any_element()
    }

    fn lyric_foreground_for_url(
        &self,
        url: &str,
        narrow: bool,
        overlay: Hsla,
        preferred: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Hsla> {
        blurred_cover(url, window, cx)
            .and_then(Result::ok)
            .map(|cover| readable_lyric_color(cover.sampled_rgb(narrow), overlay, preferred))
    }

    fn lyric_foreground_target(
        &self,
        narrow: bool,
        overlay: Hsla,
        preferred: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Hsla> {
        for url in [
            self.backdrop_current_url.as_deref(),
            self.backdrop_previous_url.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(color) =
                self.lyric_foreground_for_url(url, narrow, overlay, preferred, window, cx)
            {
                return Some(color);
            }
        }
        None
    }

    fn render_player_progress(
        &mut self,
        has_track: bool,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let monospace_font = self.fonts.monospace.clone();
        let percentage = self.progress_slider.read(cx).percentage().end;
        let duration = self.current_duration().unwrap_or_default();
        let interactive = has_track && self.playback_started && self.loading_track.is_none();
        let bar_color = theme.slider_bar;
        let thumb_color = theme.slider_thumb;
        let hover_fraction = interactive
            .then_some(self.progress_hover_fraction)
            .flatten();
        let hover_visible = self.progress_hovered && hover_fraction.is_some();
        let hover_opacity = transition(
            ("player-progress", "hover-opacity"),
            if hover_visible { 1. } else { 0. },
            Transition::new(Duration::from_millis(if hover_visible { 120 } else { 170 })),
            window,
            cx,
        );
        let edge_time_color = if self.cover_backdrop_expanded && hover_fraction.is_some() {
            self.lyric_foreground_target(
                narrow,
                theme.background.opacity(LYRIC_BACKGROUND_OVERLAY_OPACITY),
                theme.foreground,
                window,
                cx,
            )
            .unwrap_or(theme.foreground)
        } else {
            theme.secondary_foreground
        };

        let track = SliderTrack::new(&self.progress_slider)
            .disabled(!interactive)
            .h(px(20.))
            .w_full()
            .child(
                SliderIndicator::new(&self.progress_slider)
                    .relative()
                    .h(px(4.))
                    .w_full()
                    .bg(theme.border)
                    .active(|this| this.bg(bar_color.opacity(0.35)))
                    .child(
                        div()
                            .absolute()
                            .h_full()
                            .left_0()
                            .right(relative(1. - percentage))
                            .bg(bar_color),
                    )
                    .when(has_track, |indicator| {
                        indicator.child(
                            SliderThumb::new(&self.progress_slider)
                                .disabled(!interactive)
                                .absolute()
                                .top(px(-6.))
                                .left(relative(percentage))
                                .ml(-px(8.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(bar_color.opacity(0.5))
                                .size_4()
                                .p(px(1.))
                                .child(div().size_full().rounded_full().bg(thumb_color)),
                        )
                    }),
            );
        let control = BaseSlider::new(&self.progress_slider)
            .disabled(!interactive)
            .h(px(20.))
            .w_full()
            .child(track);

        let current_time = format_playback_time(self.position.min(duration));
        let total_time = format_playback_time(duration);
        div()
            .id("player-progress")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(20.))
            .when(interactive, |layer| {
                layer
                    .cursor_pointer()
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        let bounds = this.progress_slider.read(cx).bounds();
                        if bounds.size.width <= px(0.) {
                            return;
                        }
                        let inner =
                            (event.position.x - bounds.left()).clamp(px(0.), bounds.size.width);
                        this.progress_hover_fraction = Some(inner / bounds.size.width);
                        this.progress_hovered = true;
                        cx.notify();
                    }))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !hovered && this.progress_hovered {
                            this.progress_hovered = false;
                            cx.notify();
                        }
                    }))
            })
            .child(control)
            .when_some(hover_fraction, |layer, fraction| {
                let tooltip = div()
                    .absolute()
                    .top(px(-36.))
                    .w(px(64.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_md()
                    .when(hover_visible, |tooltip| tooltip.occlude())
                    .font(monospace_font.clone())
                    .text_xs()
                    .font_medium()
                    .text_color(theme.primary)
                    .child(format_playback_time(duration.mul_f32(fraction)));
                let tooltip = if fraction <= 0.03 {
                    tooltip.left_0()
                } else if fraction >= 0.97 {
                    tooltip.right_0()
                } else {
                    tooltip.left(relative(fraction)).ml(-px(32.))
                };
                layer.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(20.))
                        .opacity(hover_opacity)
                        .child(
                            div()
                                .absolute()
                                .top(px(-27.))
                                .left_0()
                                .w(px(64.))
                                .text_center()
                                .font(monospace_font.clone())
                                .text_xs()
                                .text_color(edge_time_color)
                                .child(current_time),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(px(-27.))
                                .right_0()
                                .w(px(64.))
                                .text_center()
                                .font(monospace_font.clone())
                                .text_xs()
                                .text_color(edge_time_color)
                                .child(total_time),
                        )
                        .child(tooltip),
                )
            })
            .into_any_element()
    }

    fn render_lyrics_panel(
        &mut self,
        mid: &str,
        foreground: Hsla,
        compact: bool,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let message = |title: &str, detail: Option<String>| {
            v_flex()
                .absolute()
                .top(relative(0.44))
                .left_0()
                .right_0()
                .gap_2()
                .text_color(foreground)
                .child(
                    div()
                        .text_size(if narrow { px(24.) } else { px(28.) })
                        .font_semibold()
                        .child(title.to_owned()),
                )
                .when_some(detail, |message, detail| {
                    message.child(
                        div()
                            .line_clamp(2)
                            .text_base()
                            .text_color(foreground.opacity(0.68))
                            .child(detail),
                    )
                })
                .into_any_element()
        };

        let Some(lyrics) = self
            .lyrics_cache
            .get(mid)
            .map(|lyrics| lyrics.parsed.clone())
        else {
            if self.lyrics_loading.contains(mid) {
                return message("正在加载歌词…", None);
            }
            if let Some(error) = self.lyrics_errors.get(mid) {
                return message("歌词加载失败", Some(error.clone()));
            }
            return message("歌词暂未加载", None);
        };
        if lyrics.lines.is_empty() {
            return message("暂无歌词", None);
        }

        let lyric_font = self.fonts.lyrics.clone();
        self.lyric_layout_cache
            .reset_if_needed(&lyrics, mid, compact, narrow, &lyric_font);

        let anchor = lyrics.active_index(self.position).unwrap_or(0);
        let active = Some(anchor);
        let now = cx.background_executor().now();
        let motion_enabled =
            self.cover_backdrop_expanded && self.playback_is_advancing() && !cx.reduce_motion();
        let (scroll_anchor, style_anchor) =
            self.lyric_motion_anchors(mid, anchor, motion_enabled, now);
        let scroll_offset = px(scroll_anchor * LYRIC_ROW_HEIGHT);
        let render_radius = ((f32::from(window.viewport_size().height) * 0.65 / LYRIC_ROW_HEIGHT)
            .ceil() as usize)
            + 2;
        let render_start = anchor.saturating_sub(render_radius);
        let render_end = (anchor + render_radius + 1).min(lyrics.lines.len());
        let highlight_position =
            lyric_position_for_frame_rate(self.position, self.settings.lyric_highlight_frame_rate);
        let track_duration = self.current_duration();
        let rows = lyrics
            .lines
            .iter()
            .enumerate()
            .skip(render_start)
            .take(render_end - render_start)
            .map(|(index, line)| {
                let current = active == Some(index);
                let opacity = interpolated_lyric_line_opacity(style_anchor, index);
                let emphasis = active.map_or(0., |_| {
                    (1. - (index as f32 - style_anchor).abs()).clamp(0., 1.)
                });
                let line_end = lyrics
                    .lines
                    .get(index + 1)
                    .map(|next| next.start)
                    .or(track_duration)
                    .unwrap_or_else(|| line.words.last().map_or(line.start, |word| word.end));
                let estimated_line_progress = (current
                    && line.words.is_empty()
                    && line_end > line.start)
                    .then(|| lyric_highlight_progress(line.start, line_end, highlight_position));
                let normal = self.lyric_layout_cache.line(
                    index,
                    line,
                    line_end,
                    LyricLayoutStyle::Normal,
                    compact,
                    narrow,
                    &lyric_font,
                    window,
                );
                let active_layout = self.lyric_layout_cache.line(
                    index,
                    line,
                    line_end,
                    LyricLayoutStyle::Active,
                    compact,
                    narrow,
                    &lyric_font,
                    window,
                );
                let translation =
                    self.lyric_layout_cache
                        .translation(index, line, narrow, &lyric_font, window);
                PreparedLyricRow {
                    normal,
                    active: active_layout,
                    translation,
                    emphasis,
                    opacity,
                    current,
                    estimated_line_progress,
                }
            })
            .collect::<Vec<_>>();

        div()
            .absolute()
            .top(relative(0.44))
            .left_0()
            .right_0()
            .mt(px(render_start as f32 * LYRIC_ROW_HEIGHT) - scroll_offset - px(38.))
            .child(PreparedLyricsElement {
                rows,
                foreground,
                position: highlight_position,
                translation_line_height: if narrow { px(18.) } else { px(20.) },
            })
            .into_any_element()
    }

    fn render_cover_backdrop(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let available_height = (window.viewport_size().height - px(PLAYER_BAR_HEIGHT)).max(px(0.));
        let track_mid = self
            .current_track_data()
            .map(|track| track.mid.clone())
            .unwrap_or_default();
        let defer_new_lyrics = self.defer_new_lyrics_until_next_frame(&track_mid, window, cx);
        let viewport_width = f32::from(window.viewport_size().width);
        let compact = viewport_width < 1120.;
        let narrow = viewport_width < 900.;
        let cover_size = px((viewport_width * if compact { 0.3 } else { 0.28 })
            .min(f32::from(available_height) * 0.58)
            .clamp(280., 520.));
        let expansion_progress = transition(
            ("cover-backdrop", "expansion-progress"),
            if self.cover_backdrop_expanded { 1. } else { 0. },
            Transition::new(Duration::from_millis(320)),
            window,
            cx,
        );
        if self.cover_backdrop_expanded && expansion_progress >= 1. {
            self.cover_backdrop_fully_expanded = true;
        }
        let height = available_height * expansion_progress;
        let theme = cx.theme().clone();

        match self
            .current_track_data()
            .and_then(|track| track.cover_url.clone())
        {
            Some(url) => {
                if self.backdrop_current_url.as_deref() != Some(url.as_str()) {
                    self.backdrop_previous_url = self.backdrop_current_url.replace(url.clone());
                    self.backdrop_crossfade_phase = !self.backdrop_crossfade_phase;
                }
                let current_backdrop_url = self
                    .backdrop_current_url
                    .clone()
                    .unwrap_or_else(|| url.clone());
                let previous_backdrop_url = self.backdrop_previous_url.clone();
                let track_transition_duration = if self.cover_backdrop_expanded {
                    LYRIC_TRACK_SWITCH_DURATION
                } else {
                    Duration::ZERO
                };
                let crossfade = transition(
                    ("cover-backdrop", "track-crossfade"),
                    if self.backdrop_crossfade_phase {
                        1.
                    } else {
                        0.
                    },
                    Transition::new(track_transition_duration),
                    window,
                    cx,
                );
                let current_backdrop_opacity = if self.backdrop_crossfade_phase {
                    crossfade
                } else {
                    1. - crossfade
                };
                let previous_backdrop_opacity = 1. - current_backdrop_opacity;
                let cover_url = url.clone();
                let overlay = theme.background.opacity(LYRIC_BACKGROUND_OVERLAY_OPACITY);
                let current_lyric_foreground = self.lyric_foreground_for_url(
                    &current_backdrop_url,
                    narrow,
                    overlay,
                    theme.foreground,
                    window,
                    cx,
                );
                let previous_lyric_foreground = previous_backdrop_url.as_deref().and_then(|url| {
                    self.lyric_foreground_for_url(
                        url,
                        narrow,
                        overlay,
                        theme.foreground,
                        window,
                        cx,
                    )
                });
                let lyric_foreground = match (previous_lyric_foreground, current_lyric_foreground) {
                    (Some(previous), Some(current)) => {
                        interpolate_color(previous, current, current_backdrop_opacity)
                    }
                    (Some(previous), None) => previous,
                    (None, Some(current)) => current,
                    (None, None) => theme.foreground,
                };
                let lyrics = if defer_new_lyrics {
                    div().into_any_element()
                } else {
                    self.render_lyrics_panel(
                        &track_mid,
                        lyric_foreground,
                        compact,
                        narrow,
                        window,
                        cx,
                    )
                };
                let backdrop_images = div()
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
                    .when_some(previous_backdrop_url, |images, previous_url| {
                        images.child(
                            div()
                                .absolute()
                                .top(-px(3.))
                                .right(-px(3.))
                                .bottom(-px(3.))
                                .left(-px(3.))
                                .opacity(previous_backdrop_opacity)
                                .child(
                                    img(blurred_image_source(previous_url))
                                        .size_full()
                                        .object_fit(ObjectFit::Cover),
                                ),
                        )
                    })
                    .child(
                        div()
                            .absolute()
                            .top(-px(3.))
                            .right(-px(3.))
                            .bottom(-px(3.))
                            .left(-px(3.))
                            .opacity(current_backdrop_opacity)
                            .child(
                                img(blurred_image_source(current_backdrop_url))
                                    .size_full()
                                    .object_fit(ObjectFit::Cover),
                            ),
                    );
                let content = h_flex()
                    .absolute()
                    .inset_0()
                    .items_center()
                    .justify_center()
                    .px(if narrow {
                        px(48.)
                    } else if compact {
                        px(64.)
                    } else {
                        px(96.)
                    })
                    .pt(px(84.))
                    .pb(px(56.))
                    .child(
                        h_flex()
                            .size_full()
                            .max_w(px(1500.))
                            .items_center()
                            .justify_center()
                            .gap(if compact { px(64.) } else { px(96.) })
                            .when(!narrow, |layout| {
                                layout.child(
                                    div()
                                        .size(cover_size)
                                        .flex_shrink_0()
                                        .rounded(px(20.))
                                        .overflow_hidden()
                                        .shadow_2xl()
                                        .child(
                                            img(cached_image_source(cover_url))
                                                .size_full()
                                                .rounded(px(20.))
                                                .object_fit(ObjectFit::Cover),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .relative()
                                    .h_full()
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(lyrics),
                            ),
                    );

                div()
                    .id("cover-backdrop")
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(PLAYER_BAR_HEIGHT))
                    .h(height)
                    .overflow_hidden()
                    .occlude()
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(available_height)
                            .bg(theme.background)
                            .child(backdrop_images)
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .bg(theme.background.opacity(LYRIC_BACKGROUND_OVERLAY_OPACITY)),
                            )
                            .child(content)
                            .child(
                                div()
                                    .id("collapse-cover-backdrop")
                                    .absolute()
                                    .top(px(24.))
                                    .left(px(24.))
                                    .size(px(54.))
                                    .rounded_full()
                                    .bg(theme.background.opacity(0.58))
                                    .shadow_md()
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|style| style.bg(theme.background.opacity(0.78)))
                                    .child(media_icon_hsla(
                                        MediaIcon::ChevronDown,
                                        theme.foreground,
                                        px(29.),
                                    ))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_cover_backdrop_expanded(false, window, cx);
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    fn render_player_bar(
        &mut self,
        compact: bool,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let track = self.current_track_data().cloned();
        let has_track = track.is_some();
        let quality_selector = self.render_quality_selector(has_track, cx);
        let progress = self.render_player_progress(has_track, narrow, window, cx);
        let theme = cx.theme();
        let is_playing = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
        let loading = self.loading_track.is_some();
        let show_pause = if loading {
            self.loading_autoplay
        } else {
            is_playing
        };
        let icon_foreground = self.settings.color_theme.icon_foreground();
        let icon_accent = self.settings.color_theme.icon_accent();
        let cover_size = if narrow { px(44.) } else { px(52.) };
        let cover = match track.as_ref().and_then(|track| track.cover_url.clone()) {
            Some(url) => div()
                .id("player-cover")
                .relative()
                .size(cover_size)
                .flex_shrink_0()
                .rounded(px(10.))
                .overflow_hidden()
                .child(
                    img(cached_image_source(url))
                        .size_full()
                        .rounded(px(10.))
                        .object_fit(ObjectFit::Cover),
                )
                .child(
                    div()
                        .id("toggle-cover-backdrop")
                        .absolute()
                        .inset_0()
                        .rounded(px(10.))
                        .opacity(0.)
                        .hover(|style| style.opacity(1.))
                        .bg(black().opacity(0.38))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(media_icon(
                            if self.cover_backdrop_expanded {
                                MediaIcon::ChevronDown
                            } else {
                                MediaIcon::ChevronUp
                            },
                            "#ffffff",
                            px(24.),
                        ))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_cover_backdrop_expanded(
                                !this.cover_backdrop_expanded,
                                window,
                                cx,
                            );
                        })),
                )
                .into_any_element(),
            None => div()
                .size(cover_size)
                .flex_shrink_0()
                .rounded(px(10.))
                .bg(theme.muted)
                .text_color(theme.muted_foreground)
                .flex()
                .items_center()
                .justify_center()
                .child(media_icon_hsla(
                    MediaIcon::Play,
                    theme.muted_foreground,
                    px(18.),
                ))
                .into_any_element(),
        };
        let title_text = track
            .as_ref()
            .map(|track| track.title.clone())
            .unwrap_or_else(|| "尚未播放".to_owned());
        let album_link = track.as_ref().and_then(|track| {
            (!track.album_mid.trim().is_empty() && !track.album.trim().is_empty()).then(|| {
                SearchAlbum {
                    mid: track.album_mid.clone(),
                    title: track.album.clone(),
                    cover_url: track.cover_url.clone(),
                    artist: track.artists.clone(),
                }
            })
        });
        let metadata_max_width = if narrow {
            px(100.)
        } else if compact {
            px(138.)
        } else {
            px(202.)
        };
        let title = div()
            .id("player-track-title")
            .self_start()
            .max_w(metadata_max_width)
            .truncate()
            .font_medium()
            .when_some(album_link, |this, album| {
                let hover_color = theme.primary;
                this.cursor_pointer()
                    .hover(move |style| style.text_color(hover_color))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.cover_backdrop_expanded {
                            this.set_cover_backdrop_expanded(false, window, cx);
                        }
                        this.open_home_playlist(album.clone().into_playlist(), window, cx);
                    }))
            })
            .child(title_text);
        let liked = track
            .as_ref()
            .and_then(|track| self.liked_tracks.get(&track.mid))
            .copied();
        let artists = match track.as_ref() {
            Some(track) if !track.artist_details.is_empty() => {
                let mut links = Vec::with_capacity(track.artist_details.len() * 2 - 1);
                for (index, artist) in track.artist_details.iter().cloned().enumerate() {
                    if index > 0 {
                        links.push(
                            div()
                                .flex_shrink_0()
                                .text_color(theme.muted_foreground)
                                .child(" / ")
                                .into_any_element(),
                        );
                    }
                    let name = artist.name.clone();
                    let hover_color = theme.primary;
                    links.push(
                        div()
                            .id(format!("player-artist-{index}"))
                            .flex_shrink_0()
                            .cursor_pointer()
                            .text_color(theme.secondary_foreground)
                            .hover(move |style| style.text_color(hover_color))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if this.cover_backdrop_expanded {
                                    this.set_cover_backdrop_expanded(false, window, cx);
                                }
                                this.open_search_artist(artist.clone(), window, cx);
                            }))
                            .child(name)
                            .into_any_element(),
                    );
                }
                h_flex()
                    .self_start()
                    .max_w(metadata_max_width)
                    .min_w_0()
                    .overflow_hidden()
                    .text_sm()
                    .children(links)
                    .into_any_element()
            }
            Some(track) => div()
                .self_start()
                .max_w(metadata_max_width)
                .truncate()
                .text_sm()
                .text_color(theme.secondary_foreground)
                .child(track.artists.clone())
                .into_any_element(),
            None => div()
                .self_start()
                .max_w(metadata_max_width)
                .truncate()
                .text_sm()
                .text_color(theme.secondary_foreground)
                .child("从播放列表中双击一首歌曲")
                .into_any_element(),
        };

        let content = h_flex()
            .size_full()
            .px_5()
            .gap_4()
            .child(
                h_flex()
                    .w(if narrow {
                        px(190.)
                    } else if compact {
                        px(236.)
                    } else {
                        px(300.)
                    })
                    .min_w_0()
                    .gap_3()
                    .child(cover)
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap(px(18.))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .max_w(metadata_max_width)
                                    .gap_1()
                                    .child(title)
                                    .child(artists),
                            )
                            .when(has_track, |info| {
                                info.child(
                                    Button::new("like-current-track")
                                        .ghost()
                                        .rounded(px(999.))
                                        .size(px(30.))
                                        .p_0()
                                        .tooltip(if liked == Some(true) {
                                            "取消喜欢"
                                        } else {
                                            "喜欢"
                                        })
                                        .child(media_icon_hsla(
                                            if liked == Some(true) {
                                                MediaIcon::HeartFilled
                                            } else {
                                                MediaIcon::Heart
                                            },
                                            if liked == Some(true) {
                                                theme.danger
                                            } else {
                                                theme.secondary_foreground
                                            },
                                            px(18.),
                                        ))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.toggle_current_track_liked(cx)
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(280.))
                    .items_center()
                    .justify_center()
                    .child(
                        h_flex()
                            .h(px(50.))
                            .items_center()
                            .justify_center()
                            .gap_1()
                            .child(
                                div()
                                    .relative()
                                    .child(
                                        Button::new("shuffle")
                                            .ghost()
                                            .rounded(px(999.))
                                            .size(px(44.))
                                            .p_0()
                                            .tooltip("随机播放")
                                            .toggled(self.shuffle)
                                            .selected(self.shuffle)
                                            .disabled(
                                                self.playback_queue
                                                    .as_ref()
                                                    .is_none_or(|queue| queue.tracks.is_empty()),
                                            )
                                            .child(div().w(px(28.)).flex().justify_center().child(
                                                media_icon(
                                                    MediaIcon::Shuffle,
                                                    if self.shuffle {
                                                        icon_accent
                                                    } else {
                                                        icon_foreground
                                                    },
                                                    px(18.),
                                                ),
                                            ))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.shuffle = !this.shuffle;
                                                #[cfg(target_os = "linux")]
                                                this.sync_mpris(false);
                                                cx.notify();
                                            })),
                                    )
                                    .when(self.shuffle, |this| {
                                        this.child(
                                            div()
                                                .absolute()
                                                .bottom(px(1.))
                                                .left(px(21.))
                                                .size(px(3.))
                                                .rounded_full()
                                                .bg(theme.primary),
                                        )
                                    }),
                            )
                            .child(
                                Button::new("previous")
                                    .ghost()
                                    .rounded(px(999.))
                                    .size(px(44.))
                                    .p_0()
                                    .tooltip("上一首")
                                    .disabled(self.current_track.is_none())
                                    .child(div().w(px(28.)).flex().justify_center().child(
                                        media_icon(MediaIcon::SkipBack, icon_foreground, px(20.)),
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| this.play_previous(cx))),
                            )
                            .child(
                                Button::new("play-pause")
                                    .primary()
                                    .rounded(px(999.))
                                    .size(px(48.))
                                    .p_0()
                                    .tooltip(if show_pause { "暂停" } else { "播放" })
                                    .when(loading, |button| button.bg(theme.button_primary))
                                    .disabled(
                                        loading
                                            || (self.current_track.is_none()
                                                && self
                                                    .track_table
                                                    .read(cx)
                                                    .delegate()
                                                    .tracks()
                                                    .is_empty()),
                                    )
                                    .child(media_icon(
                                        if show_pause {
                                            MediaIcon::Pause
                                        } else {
                                            MediaIcon::Play
                                        },
                                        self.settings.color_theme.icon_on_accent(),
                                        px(21.),
                                    ))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_playback(cx)),
                                    ),
                            )
                            .child(
                                Button::new("next")
                                    .ghost()
                                    .rounded(px(999.))
                                    .size(px(44.))
                                    .p_0()
                                    .tooltip("下一首")
                                    .disabled(self.current_track.is_none())
                                    .child(div().w(px(28.)).flex().justify_center().child(
                                        media_icon(
                                            MediaIcon::SkipForward,
                                            icon_foreground,
                                            px(20.),
                                        ),
                                    ))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.play_next(false, cx)),
                                    ),
                            )
                            .child(
                                div()
                                    .relative()
                                    .child(
                                        Button::new("repeat")
                                            .ghost()
                                            .rounded(px(999.))
                                            .size(px(44.))
                                            .p_0()
                                            .tooltip(self.repeat_mode.label())
                                            .toggled(self.repeat_mode != RepeatMode::Off)
                                            .selected(self.repeat_mode != RepeatMode::Off)
                                            .disabled(
                                                self.playback_queue
                                                    .as_ref()
                                                    .is_none_or(|queue| queue.tracks.is_empty()),
                                            )
                                            .child(div().w(px(28.)).flex().justify_center().child(
                                                media_icon(
                                                    if self.repeat_mode == RepeatMode::One {
                                                        MediaIcon::RepeatOne
                                                    } else {
                                                        MediaIcon::Repeat
                                                    },
                                                    if self.repeat_mode != RepeatMode::Off {
                                                        icon_accent
                                                    } else {
                                                        icon_foreground
                                                    },
                                                    px(18.),
                                                ),
                                            ))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.repeat_mode = this.repeat_mode.next();
                                                #[cfg(target_os = "linux")]
                                                this.sync_mpris(false);
                                                cx.notify();
                                            })),
                                    )
                                    .when(self.repeat_mode != RepeatMode::Off, |this| {
                                        this.child(
                                            div()
                                                .absolute()
                                                .bottom(px(1.))
                                                .left(px(21.))
                                                .size(px(3.))
                                                .rounded_full()
                                                .bg(theme.primary),
                                        )
                                    }),
                            ),
                    ),
            )
            .when(!narrow, |bar| {
                bar.child(
                    h_flex()
                        .w(if compact { px(220.) } else { px(300.) })
                        .justify_end()
                        .gap_2()
                        .child(quality_selector)
                        .child(
                            Button::new("mute")
                                .ghost()
                                .rounded(px(999.))
                                .size(px(44.))
                                .p_0()
                                .tooltip(if self.settings.volume > 0. {
                                    "静音"
                                } else {
                                    "取消静音"
                                })
                                .child(div().w(px(28.)).flex().justify_center().child(media_icon(
                                    if self.settings.volume > 0. {
                                        MediaIcon::Volume
                                    } else {
                                        MediaIcon::VolumeMuted
                                    },
                                    icon_foreground,
                                    px(19.),
                                )))
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.toggle_mute(window, cx)),
                                ),
                        )
                        .child(Slider::new(&self.volume_slider).w(if compact {
                            px(82.)
                        } else {
                            px(118.)
                        })),
                )
            });

        div()
            .relative()
            .h(px(PLAYER_BAR_HEIGHT))
            .w_full()
            .flex_shrink_0()
            .bg(theme.group_box)
            .child(progress)
            .child(content)
            .into_any_element()
    }

    fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.cover_backdrop_expanded && self.playback_is_advancing() {
            if self.seek_preview.is_none() {
                self.position = self
                    .audio
                    .as_ref()
                    .map(AudioPlayer::position)
                    .unwrap_or_default();
            }
            self.request_lyric_animation_frame(window, cx);
        }

        let theme = cx.theme().clone();
        let compact = window.viewport_size().width < px(1120.);
        let narrow = window.viewport_size().width < px(900.);
        let popover_open = self.account_menu_open || self.quality_menu_open;
        if self.cover_backdrop_fully_expanded {
            return v_flex()
                .relative()
                .size_full()
                .font(self.fonts.ui.clone())
                .bg(theme.background)
                .text_color(theme.foreground)
                .child(div().flex_1().min_h_0())
                .child(self.render_cover_backdrop(window, cx))
                .child(self.render_player_bar(compact, narrow, window, cx))
                .when(popover_open, |this| {
                    this.child(
                        deferred(
                            div()
                                .id("popover-dismiss-layer")
                                .absolute()
                                .inset_0()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.dismiss_popovers(cx);
                                    }),
                                ),
                        )
                        .with_priority(5),
                    )
                })
                .into_any_element();
        }
        self.track_table.update(cx, |table, cx| {
            if table.delegate_mut().set_compact(compact) {
                table.refresh(cx);
            }
        });
        let (default_sidebar_width, min_sidebar_width, max_sidebar_width) = if narrow {
            (216., 196., 240.)
        } else if compact {
            (248., 220., 300.)
        } else {
            (272., 236., 340.)
        };
        let sidebar_width = px(self
            .settings
            .sidebar_width
            .map(|width| width as f32)
            .unwrap_or(default_sidebar_width)
            .clamp(min_sidebar_width, max_sidebar_width));
        let sidebar_range = px(min_sidebar_width)..px(max_sidebar_width);
        let sidebar = self.render_sidebar(cx);
        let page = match self.main_content {
            MainContent::Home => self.render_home(compact, narrow, cx),
            MainContent::Search => self.render_search(compact, narrow, cx),
            MainContent::Artist => self.render_artist_content(compact, narrow, cx),
            MainContent::Playlist => self.render_playlist_content(compact, narrow, cx),
            MainContent::Settings => self.render_settings_page(narrow, cx),
        };
        let search_width = if narrow {
            px(268.)
        } else if compact {
            px(376.)
        } else {
            px(480.)
        };
        let home_selected = self.main_content == MainContent::Home;
        let can_navigate_back = !self.navigation_history.back.is_empty();
        let can_navigate_forward = !self.navigation_history.forward.is_empty();
        let history_navigation = h_flex()
            .gap_1()
            .child(
                Button::new("navigate-back")
                    .ghost()
                    .rounded(px(999.))
                    .size(px(44.))
                    .p_0()
                    .tooltip("返回")
                    .disabled(!can_navigate_back)
                    .child(media_icon_hsla(
                        MediaIcon::Back,
                        if can_navigate_back {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        },
                        px(22.),
                    ))
                    .on_click(cx.listener(|this, _, window, cx| this.navigate_back(window, cx))),
            )
            .child(
                Button::new("navigate-forward")
                    .ghost()
                    .rounded(px(999.))
                    .size(px(44.))
                    .p_0()
                    .tooltip("前进")
                    .disabled(!can_navigate_forward)
                    .child(media_icon_hsla(
                        MediaIcon::Forward,
                        if can_navigate_forward {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        },
                        px(22.),
                    ))
                    .on_click(cx.listener(|this, _, window, cx| this.navigate_forward(window, cx))),
            );
        let navigation = h_flex()
            .gap_3()
            .child(
                Button::new("home")
                    .ghost()
                    .rounded(px(999.))
                    .size(px(44.))
                    .p_0()
                    .tooltip("主页")
                    .child(media_icon_hsla(
                        MediaIcon::Home,
                        if home_selected {
                            theme.primary
                        } else {
                            theme.secondary_foreground
                        },
                        px(24.),
                    ))
                    .on_click(cx.listener(|this, _, window, cx| this.show_home(window, cx))),
            )
            .child(
                div()
                    .id("search-input")
                    .on_mouse_down_out(|_, window, _| window.blur())
                    .child(
                        Input::new(&self.search_input)
                            .large()
                            .w(search_width)
                            .border_2()
                            .rounded(px(999.))
                            .text_size(theme.font_size)
                            .aria_label("搜索")
                            .prefix(media_icon_hsla(
                                MediaIcon::Search,
                                theme.muted_foreground,
                                px(22.),
                            )),
                    ),
            );
        let account = self.render_account(cx);
        let content = v_flex()
            .h_full()
            .min_w_0()
            .flex_1()
            .child(
                div()
                    .relative()
                    .h(px(72.))
                    .w_full()
                    .flex_shrink_0()
                    .child(
                        h_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .child(navigation),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(14.))
                            .left(px(24.))
                            .child(history_navigation),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(17.))
                            .right(px(24.))
                            .size(px(44.))
                            .child(account),
                    ),
            )
            .child(page);
        v_flex()
            .relative()
            .size_full()
            .font(self.fonts.ui.clone())
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("library-layout")
                        .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                            if let Some(width) = state.read(cx).sizes().first() {
                                this.settings.sidebar_width =
                                    Some(f32::from(*width).round() as u32);
                            }
                        }))
                        .child(
                            resizable_panel()
                                .size(sidebar_width)
                                .size_range(sidebar_range)
                                .flex_none()
                                .child(sidebar),
                        )
                        .child(
                            resizable_panel()
                                .size_range(px(480.)..Pixels::MAX)
                                .child(content),
                        ),
                ),
            )
            .child(self.render_cover_backdrop(window, cx))
            .child(self.render_player_bar(compact, narrow, window, cx))
            .when(popover_open, |this| {
                this.child(
                    deferred(
                        div()
                            .id("popover-dismiss-layer")
                            .absolute()
                            .inset_0()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.dismiss_popovers(cx);
                                }),
                            ),
                    )
                    .with_priority(5),
                )
            })
            .into_any_element()
    }
}

impl Drop for LyruneView {
    fn drop(&mut self) {
        self.persist_current_playback();
        if let Some(audio) = &self.audio {
            audio.stop();
        }
        self.persist_settings();
        if let Some(task) = self.cdn_maintenance.take() {
            task.abort();
        }
        if let Some(task) = self.audio_cache_maintenance.take() {
            task.abort();
        }
    }
}

impl Render for LyruneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.account_state == AccountState::SignedIn {
            self.render_main(window, cx)
        } else {
            self.render_login(cx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LYRIC_MINIMUM_CONTRAST, LyricFrameRate, NavigationHistory, NavigationPage, PlaybackQueue,
        PlaylistScrollPosition, SearchCategory, adjacent_lyric_timing, canonical_queue_track_index,
        combined_lyric_frame_interval, contrast_ratio, extract_qrc_content, format_playback_time,
        insert_external_track_after_current, insert_track_after_current, lyric_edge_opacity,
        lyric_frame_is_due, lyric_horizontal_scroll_offset, lyric_position_for_frame_rate,
        parse_lyrics, playlist_title_is_long, readable_lyric_color, resolved_playlist_scroll_row,
    };
    use gpui::{Pixels, Rgba, black, px, rgb};
    use qqmusic_api::integration::{Track, UserPlaylist, UserPlaylistId};
    use std::time::{Duration, Instant};

    fn track(mid: &str) -> Track {
        Track {
            song_id: None,
            song_type: 0,
            mid: mid.to_owned(),
            media_mid: None,
            standard_size_bytes: None,
            high_size_bytes: None,
            lossless_size_bytes: None,
            hi_res_size_bytes: None,
            atmos_stereo_size_bytes: None,
            atmos_surround_size_bytes: None,
            master_size_bytes: None,
            title: mid.to_owned(),
            artists: String::new(),
            artist_details: Vec::new(),
            album: String::new(),
            album_mid: String::new(),
            cover_url: None,
            duration_seconds: 180,
        }
    }

    fn mids(tracks: &[Track]) -> Vec<&str> {
        tracks.iter().map(|track| track.mid.as_str()).collect()
    }

    #[test]
    fn lyric_foreground_preserves_a_readable_theme_color() {
        let preferred = rgb(0x182030).into();
        let result =
            readable_lyric_color([0.95, 0.95, 0.95], black().opacity(0.28), preferred).to_rgb();
        let preferred = preferred.to_rgb();

        assert!((result.r - preferred.r).abs() < 1e-6);
        assert!((result.g - preferred.g).abs() < 1e-6);
        assert!((result.b - preferred.b).abs() < 1e-6);
    }

    #[test]
    fn lyric_foreground_minimally_corrects_insufficient_theme_contrast() {
        let background = Rgba {
            r: 0.5,
            g: 0.5,
            b: 0.5,
            a: 1.,
        };
        let result = readable_lyric_color(
            [background.r, background.g, background.b],
            black().opacity(0.),
            rgb(0x777777).into(),
        )
        .to_rgb();

        assert!(contrast_ratio(result, background) >= LYRIC_MINIMUM_CONTRAST);
        assert!(result.r > 0. && result.g > 0. && result.b > 0.);
    }

    #[test]
    fn horizontal_lyric_scroll_starts_at_the_anchor_and_stops_at_the_end() {
        assert_eq!(
            lyric_horizontal_scroll_offset(px(500.), px(600.), px(500.)),
            px(0.)
        );
        assert_eq!(
            lyric_horizontal_scroll_offset(px(1000.), px(600.), px(250.)),
            px(0.)
        );
        assert_eq!(
            lyric_horizontal_scroll_offset(px(1000.), px(600.), px(300.)),
            px(48.)
        );
        assert_eq!(
            lyric_horizontal_scroll_offset(px(1000.), px(600.), px(1000.)),
            px(400.)
        );
    }

    #[test]
    fn lyric_rows_fade_before_reaching_the_viewport_edge() {
        assert_eq!(lyric_edge_opacity(px(100.), px(204.), px(0.), px(400.)), 1.);
        assert!((lyric_edge_opacity(px(272.), px(376.), px(0.), px(400.)) - 0.5).abs() < 1e-6);
        assert_eq!(lyric_edge_opacity(px(296.), px(400.), px(0.), px(400.)), 0.);
        assert_eq!(lyric_edge_opacity(px(-10.), px(94.), px(0.), px(400.)), 0.);
    }

    #[test]
    fn lyric_frame_intervals_preserve_the_fastest_animation_requirement() {
        assert_eq!(
            combined_lyric_frame_interval(LyricFrameRate::Fps30, LyricFrameRate::Fps60),
            LyricFrameRate::Fps60.frame_interval()
        );
        assert_eq!(
            combined_lyric_frame_interval(LyricFrameRate::Display, LyricFrameRate::Fps60),
            None
        );
        assert_eq!(
            combined_lyric_frame_interval(LyricFrameRate::Fps30, LyricFrameRate::Display),
            None
        );
    }

    #[test]
    fn lyric_position_is_quantized_only_for_limited_highlight_rates() {
        let position = Duration::from_millis(50);
        assert_eq!(
            lyric_position_for_frame_rate(position, LyricFrameRate::Fps30),
            Duration::from_nanos(1_000_000_000 / 30)
        );
        assert_eq!(
            lyric_position_for_frame_rate(position, LyricFrameRate::Display),
            position
        );
    }

    #[test]
    fn limited_lyric_frames_follow_one_stable_deadline() {
        let now = Instant::now();
        let interval = LyricFrameRate::Fps30.frame_interval().unwrap();
        let mut next_frame = None;

        assert!(!lyric_frame_is_due(
            now,
            LyricFrameRate::Fps30,
            &mut next_frame
        ));
        assert_eq!(next_frame, Some(now + interval));
        assert!(!lyric_frame_is_due(
            now + Duration::from_millis(10),
            LyricFrameRate::Fps30,
            &mut next_frame
        ));
        assert!(lyric_frame_is_due(
            now + interval,
            LyricFrameRate::Fps30,
            &mut next_frame
        ));
        assert!(lyric_frame_is_due(
            now + interval,
            LyricFrameRate::Display,
            &mut next_frame
        ));
        assert_eq!(next_frame, None);
    }

    #[test]
    fn parses_and_aligns_timestamped_lyrics() {
        let lyrics = parse_lyrics(
            "[ti:title]\n[00:01.20]first\n[00:10.000][00:20.000]repeat",
            Some("[00:01.200]第一句\n[00:10.000]重复"),
            None,
        );

        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[0].start, Duration::from_millis(1_200));
        assert_eq!(lyrics.lines[0].text, "first");
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("第一句"));
        assert_eq!(lyrics.lines[2].start, Duration::from_secs(20));
        assert_eq!(lyrics.active_index(Duration::from_secs(5)), Some(0));
        assert_eq!(lyrics.active_index(Duration::from_secs(10)), Some(1));
    }

    #[test]
    fn untimed_qrc_tail_extends_to_the_next_line() {
        let timing = (Duration::from_millis(100), Duration::from_millis(600));

        assert_eq!(
            adjacent_lyric_timing(
                Some(timing),
                None,
                Duration::ZERO,
                Duration::from_millis(900)
            ),
            (Duration::from_millis(600), Duration::from_millis(900))
        );
    }

    #[test]
    fn untimed_qrc_text_between_words_uses_the_timing_gap() {
        let previous = (Duration::from_millis(100), Duration::from_millis(600));
        let next = (Duration::from_millis(900), Duration::from_millis(1_200));

        assert_eq!(
            adjacent_lyric_timing(
                Some(previous),
                Some(next),
                Duration::ZERO,
                Duration::from_millis(1_500)
            ),
            (Duration::from_millis(600), Duration::from_millis(900))
        );
    }

    #[test]
    fn prefers_qrc_word_timing_and_preserves_lyric_parentheses() {
        let lyrics = parse_lyrics(
            r#"<?xml version="1.0" encoding="utf-8"?>
<QrcInfos><LyricInfo LyricCount="1"><Lyric_1 LyricType="1" LyricContent="[1000,700]青(1000,300)い(1300,150)空（そら）(1450,250)&#10;[2000,500]次(2000,500)"/></LyricInfo></QrcInfos>"#,
            Some("[00:01.000]蓝色天空\n[00:02.000]下一句"),
            None,
        );

        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].start, Duration::from_secs(1));
        assert_eq!(lyrics.lines[0].text, "青い空（そら）");
        assert_eq!(lyrics.lines[0].words.len(), 3);
        assert_eq!(
            lyrics.lines[0].words[0].highlight_progress(Duration::from_millis(999)),
            0.
        );
        assert!(
            (lyrics.lines[0].words[0].highlight_progress(Duration::from_millis(1_100)) - 1. / 3.)
                .abs()
                < f32::EPSILON
        );
        assert_eq!(
            lyrics.lines[0].words[2].highlight_progress(Duration::from_millis(1_700)),
            1.
        );
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("蓝色天空"));
        assert_eq!(lyrics.active_index(Duration::from_millis(1_999)), Some(0));
        assert_eq!(lyrics.active_index(Duration::from_secs(2)), Some(1));
    }

    #[test]
    fn aligns_qrc_translation_with_rounded_timestamps() {
        let lyrics = parse_lyrics(
            r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[16013,500]原(16013,500)&#10;[22564,500]文(22564,500)"/></LyricInfo></QrcInfos>"#,
            Some("[00:16.010]第一行\n[00:22.560]第二行"),
            None,
        );

        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("第一行"));
        assert_eq!(lyrics.lines[1].translation.as_deref(), Some("第二行"));
    }

    #[test]
    fn aligns_qrc_romanization_as_kanji_ruby() {
        let lyrics = parse_lyrics(
            "[17499,5352]あ(17499,127)と(17626,211)一(17838,646)匙(18485,739)の(19224,234)憂(19459,297)鬱(19756,389)で(20146,285)",
            None,
            Some(
                "[17499,5352]a (17499,127)to (17626,211)hi (17838,323)to (18161,323)sa (18485,369)ji (18854,370)no (19224,234)yu (19459,148)u (19607,149)u (19756,194)tsu (19950,195)de (20146,285)",
            ),
        );

        let words = &lyrics.lines[0].words;
        assert_eq!(words[0].ruby, None);
        assert_eq!(words[2].ruby.as_deref(), Some("ひと"));
        assert_eq!(words[3].ruby.as_deref(), Some("さじ"));
        assert_eq!(words[5].ruby.as_deref(), Some("ゆう"));
        assert_eq!(words[6].ruby.as_deref(), Some("うつ"));
    }

    #[test]
    fn assigns_qrc_romanization_to_the_word_with_the_largest_overlap() {
        let lyrics = parse_lyrics(
            "[4244,4592]夜(4244,349)の(4593,406)赤(4999,373)と(5372,1039)歩(6411,433)き(6844,756)解(7600,448)く(8048,788)",
            None,
            Some(
                "[4243,4593]yo(4243,182)ru(4426,166)no(4592,406)a(4999,164)ka(5163,208)to(5371,1038)a(6410,236)ru(6646,197)ki(6843,756)ho(7600,236)do(7836,211)ku(8047,788)",
            ),
        );

        let words = &lyrics.lines[0].words;
        assert_eq!(words[0].ruby.as_deref(), Some("よる"));
        assert_eq!(words[1].ruby, None);
        assert_eq!(words[2].ruby.as_deref(), Some("あか"));
        assert_eq!(words[3].ruby, None);
        assert_eq!(words[4].ruby.as_deref(), Some("ある"));
        assert_eq!(words[5].ruby, None);
        assert_eq!(words[6].ruby.as_deref(), Some("ほど"));
        assert_eq!(words[7].ruby, None);
    }

    #[test]
    fn does_not_treat_chinese_romanization_as_japanese_ruby() {
        let lyrics = parse_lyrics(
            "[1000,500]中文(1000,500)",
            None,
            Some("[1000,500]zhong (1000,250)wen (1250,250)"),
        );

        assert_eq!(lyrics.lines[0].words[0].ruby, None);
    }

    #[test]
    fn preserves_literal_line_breaks_in_qrc_attributes() {
        let lyrics = parse_lyrics(
            "<QrcInfos><LyricInfo><Lyric_1 LyricContent=\"[ti:title]\r\n[0,500]中(0,250)文(250,250)\r\n[500,500]歌词(500,500)\"/></LyricInfo></QrcInfos>",
            None,
            None,
        );

        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].text, "中文");
        assert_eq!(lyrics.lines[1].text, "歌词");
    }

    #[test]
    fn extracts_qrc_content_with_malformed_xml_characters() {
        let content = extract_qrc_content(
            r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[0,500]Recording & Mix <live>(0,500)&#10;[500,500]Marcus "MarcLo" Lomax &amp; &#x41;&apos;s(500,500)" Source='qq'/></LyricInfo></QrcInfos>"#,
        )
        .unwrap();

        assert_eq!(
            content,
            "[0,500]Recording & Mix <live>(0,500)\n[500,500]Marcus \"MarcLo\" Lomax & A's(500,500)"
        );
    }

    #[test]
    fn parses_all_lines_after_an_unescaped_qrc_quote() {
        let lyrics = parse_lyrics(
            r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[0,500]Marcus "MarcLo" Lomax(0,500)&#10;[500,500]second(500,500)&#10;[1000,500]third(1000,500)"/></LyricInfo></QrcInfos>"#,
            None,
            None,
        );

        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[0].text, "Marcus \"MarcLo\" Lomax");
        assert_eq!(lyrics.lines[2].text, "third");
    }

    #[test]
    fn falls_back_to_lrc_inside_the_qrc_wrapper() {
        let lyrics = parse_lyrics(
            r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[00:01.000]first&#10;[00:02.000]second"/></LyricInfo></QrcInfos>"#,
            None,
            None,
        );

        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].text, "first");
        assert_eq!(lyrics.lines[1].text, "second");
    }

    #[test]
    fn rejects_a_truncated_qrc_wrapper_instead_of_returning_a_prefix() {
        assert_eq!(
            extract_qrc_content(
                r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[0,500]first(0,500)"#
            ),
            None
        );
    }

    #[test]
    fn omits_provider_translation_placeholders() {
        let lyrics = parse_lyrics(
            "[00:01.000]词：作者\n[00:02.000]//",
            Some("[00:01.000]//\n[00:02.000]实际翻译"),
            None,
        );

        assert_eq!(lyrics.lines[0].translation, None);
        assert_eq!(lyrics.lines[1].text, "//");
        assert_eq!(lyrics.lines[1].translation.as_deref(), Some("实际翻译"));
    }

    fn playlist(diss_id: u64) -> NavigationPage {
        playlist_at_scroll_row(diss_id, 0)
    }

    fn playlist_at_scroll_row(diss_id: u64, scroll_row: usize) -> NavigationPage {
        playlist_at_scroll_position(diss_id, scroll_row, px(0.))
    }

    fn playlist_at_scroll_position(
        diss_id: u64,
        scroll_row: usize,
        offset_y: Pixels,
    ) -> NavigationPage {
        NavigationPage::Playlist {
            playlist: UserPlaylist {
                id: UserPlaylistId::Favorite { diss_id },
                title: format!("playlist-{diss_id}"),
                cover_url: None,
                description: String::new(),
                owner: String::new(),
                owner_avatar_url: None,
                track_count: 0,
            },
            selected_index: None,
            scroll_position: PlaylistScrollPosition {
                row: scroll_row,
                offset_y,
            },
        }
    }

    #[test]
    fn new_navigation_after_back_clears_the_forward_branch() {
        let first = playlist(1);
        let second = playlist(2);
        let home = NavigationPage::Home;
        let mut history = NavigationHistory::default();

        history.record(Some(first.clone()), &home);
        let back = history.go_back(Some(home.clone())).unwrap();
        assert!(back.same_destination(&first));

        let forward = history.go_forward(Some(first.clone())).unwrap();
        assert!(forward.same_destination(&home));

        let back = history.go_back(Some(home)).unwrap();
        assert!(back.same_destination(&first));
        history.record(Some(first), &second);

        assert!(history.forward.is_empty());
        let back = history.go_back(Some(second)).unwrap();
        assert!(back.same_destination(&playlist(1)));
    }

    #[test]
    fn playlist_history_keeps_its_exact_scroll_position() {
        let playlist = playlist_at_scroll_position(1, 37, px(-18.));
        let mut history = NavigationHistory::default();

        history.record(Some(playlist), &NavigationPage::Home);
        let restored = history
            .go_back(Some(NavigationPage::Home))
            .expect("playlist history entry");

        let NavigationPage::Playlist {
            scroll_position, ..
        } = restored
        else {
            panic!("restored playlist history entry");
        };
        assert_eq!(scroll_position.row, 37);
        assert_eq!(scroll_position.offset_y, px(-18.));
    }

    #[test]
    fn playlist_scroll_waits_until_the_requested_row_is_loaded() {
        assert_eq!(resolved_playlist_scroll_row(120, 100, true), None);
        assert_eq!(resolved_playlist_scroll_row(120, 200, true), Some(120));
        assert_eq!(resolved_playlist_scroll_row(250, 200, false), Some(199));
    }

    #[test]
    fn playback_time_uses_compact_player_formatting() {
        assert_eq!(format_playback_time(Duration::from_secs(137)), "2:17");
        assert_eq!(format_playback_time(Duration::from_secs(3_661)), "1:01:01");
    }

    #[test]
    fn playlist_title_length_accounts_for_wide_characters() {
        assert!(!playlist_title_is_long("123456789012345678901234"));
        assert!(playlist_title_is_long("1234567890123456789012345"));
        assert!(!playlist_title_is_long("一二三四五六七八九十一二"));
        assert!(playlist_title_is_long("一二三四五六七八九十一二三"));
    }

    #[test]
    fn search_history_keeps_the_active_category_without_splitting_one_query() {
        let songs = NavigationPage::Search {
            query: "周杰伦".to_owned(),
            category: SearchCategory::Songs,
        };
        let albums = NavigationPage::Search {
            query: "周杰伦".to_owned(),
            category: SearchCategory::Albums,
        };
        assert!(songs.same_destination(&albums));

        let target = playlist(42);
        let mut history = NavigationHistory::default();
        history.record(Some(albums.clone()), &target);
        let restored = history.go_back(Some(target)).expect("search history entry");
        assert!(matches!(
            restored,
            NavigationPage::Search {
                query,
                category: SearchCategory::Albums,
            } if query == "周杰伦"
        ));
    }

    #[test]
    fn search_categories_follow_the_product_order() {
        assert_eq!(
            SearchCategory::ALL.map(SearchCategory::label),
            ["单曲", "歌单", "专辑", "歌手"]
        );
    }

    #[test]
    fn search_track_is_inserted_after_the_current_track() {
        let mut tracks = vec![track("A"), track("B")];

        let inserted = insert_track_after_current(&mut tracks, Some(0), track("C"));

        assert_eq!(inserted, 1);
        assert_eq!(mids(&tracks), ["A", "C", "B"]);
    }

    #[test]
    fn existing_search_track_is_moved_without_duplication() {
        let mut tracks = vec![track("C"), track("A"), track("B")];

        let inserted = insert_track_after_current(&mut tracks, Some(1), track("C"));

        assert_eq!(inserted, 1);
        assert_eq!(mids(&tracks), ["A", "C", "B"]);
    }

    #[test]
    fn search_insertion_prevents_reusing_the_source_playlist_queue() {
        let playlist_id = UserPlaylistId::Liked;
        let mut queue = PlaybackQueue {
            playlist_id: playlist_id.clone(),
            tracks: vec![track("A"), track("B")],
            modified: false,
            continuation: None,
        };

        let inserted = insert_external_track_after_current(&mut queue, Some(0), track("search"));

        assert_eq!(inserted, 1);
        assert_eq!(mids(&queue.tracks), ["A", "search", "B"]);
        assert!(queue.modified);
        assert_eq!(canonical_queue_track_index(&queue, &playlist_id, "B"), None);
    }
}
