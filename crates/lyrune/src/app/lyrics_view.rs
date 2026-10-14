use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{prelude::*, *};
use gpui_base::motion::{Transition, transition};
use gpui_component::{
    ActiveTheme as _, StyledExt as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex, v_flex,
};

use super::PLAYER_BAR_HEIGHT;
use super::animation::frame_is_due;
#[cfg(test)]
use super::lyrics;
use super::lyrics::{LyricLine, ParsedLyrics};
use crate::http::{
    BLURRED_COVER_SIZE, BlurredCover, blurred_cover, blurred_image_source, cached_image_source,
};
use crate::icons::{MediaIcon, media_icon_hsla};
use crate::player::AudioPlayer;
use crate::settings::FrameRate;

const LYRIC_ROW_HEIGHT: f32 = 104.;
const LYRIC_ANCHOR_OFFSET: f32 = 38.;
const LYRIC_EDGE_FADE_DISTANCE: f32 = 48.;
const LYRIC_SCROLL_DURATION: Duration = Duration::from_millis(360);
const LYRIC_STYLE_DURATION: Duration = Duration::from_millis(240);
pub(super) const LYRIC_EXPANSION_DURATION: Duration = Duration::from_millis(480);
const LYRIC_TRACK_SWITCH_DURATION: Duration = Duration::from_millis(420);
pub(super) const LYRIC_BACKGROUND_OVERLAY_OPACITY: f32 = 0.4;
pub(super) const LYRIC_MINIMUM_CONTRAST: f32 = 3.5;
const LYRIC_DARK_TEXT_LUMINANCE: f32 = 0.25;
const LYRIC_HORIZONTAL_ANCHOR: f32 = 0.42;
const LYRIC_HORIZONTAL_STEP: f32 = 0.5;

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

pub(super) fn contrast_ratio(first: Rgba, second: Rgba) -> f32 {
    let first = relative_luminance(first);
    let second = relative_luminance(second);
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

pub(super) fn interpolate_color(from: Hsla, to: Hsla, progress: f32) -> Hsla {
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

pub(super) fn readable_lyric_color(sampled_rgb: [f32; 3], overlay: Hsla, preferred: Hsla) -> Hsla {
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
    let neutral = if relative_luminance(background) >= LYRIC_DARK_TEXT_LUMINANCE {
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

fn foreground_for_cover(
    url: &str,
    overlay: Hsla,
    preferred: Hsla,
    sample: impl Fn(&BlurredCover) -> [f32; 3],
    window: &mut Window,
    cx: &mut App,
) -> Option<Hsla> {
    blurred_cover(url, window, cx)
        .and_then(Result::ok)
        .map(|cover| readable_lyric_color(sample(&cover), overlay, preferred))
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
    // 仅作为缓存身份，不解引用；同一首歌刷新歌词后也可能得到新的 ParsedLyrics。
    lyrics: usize,
    mid: String,
    compact: bool,
    narrow: bool,
    scale: u32,
    font: Font,
}

struct CachedLyricRow {
    normal: Option<Arc<PreparedLyricLine>>,
    active: Option<Arc<PreparedLyricLine>>,
    translation: Option<Arc<PreparedLyricTranslation>>,
    translation_prepared: bool,
}

#[derive(Default)]
pub(super) struct LyricLayoutCache {
    source: Option<LyricLayoutCacheSource>,
    rows: Vec<CachedLyricRow>,
}

#[derive(Clone)]
struct PreparedLyricRow {
    normal: Arc<PreparedLyricLine>,
    active: Arc<PreparedLyricLine>,
    translation: Option<Arc<PreparedLyricTranslation>>,
    emphasis: f32,
    opacity: f32,
    current: bool,
    estimated_line_progress: Option<f32>,
}

pub(super) struct CachedLyricsPanel {
    lyrics: usize,
    mid: String,
    compact: bool,
    narrow: bool,
    scale: u32,
    row_height: Pixels,
    font: Font,
    render_radius: usize,
    render_start: usize,
    scroll_offset: Pixels,
    style_anchor: f32,
    highlight_position: Duration,
    translation_line_height: Pixels,
    rows: Vec<PreparedLyricRow>,
}

struct PreparedLyricsElement {
    rows: Vec<PreparedLyricRow>,
    foreground: Hsla,
    position: Duration,
    translation_line_height: Pixels,
    row_height: Pixels,
}

pub(super) struct LyricMotionState {
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
impl lyrics::LyricWord {
    pub(super) fn highlight_progress(&self, position: Duration) -> f32 {
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

pub(super) fn adjacent_lyric_timing(
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

pub(super) fn lyric_horizontal_scroll_offset(
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

pub(super) fn combined_lyric_frame_interval(
    highlight: FrameRate,
    scroll: FrameRate,
) -> Option<Duration> {
    match (highlight.frame_interval(), scroll.frame_interval()) {
        (Some(highlight), Some(scroll)) => Some(highlight.min(scroll)),
        _ => None,
    }
}

pub(super) fn lyric_position_for_frame_rate(position: Duration, frame_rate: FrameRate) -> Duration {
    let Some(interval) = frame_rate.frame_interval() else {
        return position;
    };
    let interval_nanos = interval.as_nanos();
    let quantized_nanos = position.as_nanos() / interval_nanos * interval_nanos;
    Duration::from_nanos(quantized_nanos.min(u64::MAX as u128) as u64)
}

fn translation_line_height(narrow: bool, scale: f32) -> Pixels {
    (if narrow { px(18.) } else { px(20.) }) * scale
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

pub(super) fn lyric_edge_opacity(
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
    #[allow(clippy::too_many_arguments)]
    fn new(
        line: &LyricLine,
        line_end: Duration,
        style: LyricLayoutStyle,
        compact: bool,
        narrow: bool,
        scale: f32,
        configured_font: &Font,
        window: &Window,
    ) -> Self {
        let active = style == LyricLayoutStyle::Active;
        let font_size = scale
            * match (active, compact) {
                (true, true) => px(24.),
                (true, false) => px(28.),
                (false, true) => px(17.),
                (false, false) => px(19.),
            };
        let ruby_font_size = (if narrow { px(11.) } else { px(12.) }) * scale;
        let ruby_line_height = (if narrow { px(13.) } else { px(15.) }) * scale;
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
            // QRC 的字级时间戳可能漏掉标点或尾部文字，保留原文并借用相邻时间段高亮。
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
    fn new(text: &str, narrow: bool, scale: f32, configured_font: &Font, window: &Window) -> Self {
        let font_size = (if narrow { px(13.) } else { px(14.) }) * scale;
        let line_height = translation_line_height(narrow, scale);
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
        scale: u32,
        font: &Font,
    ) {
        let lyrics_identity = Arc::as_ptr(lyrics) as usize;
        let matches = self.source.as_ref().is_some_and(|source| {
            source.lyrics == lyrics_identity
                && source.mid == mid
                && source.compact == compact
                && source.narrow == narrow
                && source.scale == scale
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
            scale,
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

    #[allow(clippy::too_many_arguments)]
    fn line(
        &mut self,
        index: usize,
        lyric: &LyricLine,
        line_end: Duration,
        style: LyricLayoutStyle,
        compact: bool,
        narrow: bool,
        scale: f32,
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
                    lyric, line_end, style, compact, narrow, scale, font, window,
                ))
            })
            .clone()
    }

    fn translation(
        &mut self,
        index: usize,
        lyric: &LyricLine,
        narrow: bool,
        scale: f32,
        font: &Font,
        window: &Window,
    ) -> Option<Arc<PreparedLyricTranslation>> {
        let row = &mut self.rows[index];
        if !row.translation_prepared {
            row.translation = lyric.translation.as_deref().map(|translation| {
                Arc::new(PreparedLyricTranslation::new(
                    translation,
                    narrow,
                    scale,
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
    // 动画直接缩放缓存中的字形，避免字号插值时每帧重新排版。
    // Emoji 走 GPUI 的独立绘制接口，因此需要显式缩放位置和字号。
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

    #[allow(clippy::too_many_arguments)]
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
        style.size.height = (self.row_height * self.rows.len() as f32).into();
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
                        bounds.origin.y + self.row_height * index as f32,
                    ),
                    size(bounds.size.width, self.row_height),
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

#[derive(Clone)]
pub(super) enum LyricsContent {
    Ready(Arc<ParsedLyrics>),
    Loading,
    Error(String),
    Unloaded,
}

impl PartialEq for LyricsContent {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ready(left), Self::Ready(right)) => Arc::ptr_eq(left, right),
            (Self::Error(left), Self::Error(right)) => left == right,
            (Self::Loading, Self::Loading) | (Self::Unloaded, Self::Unloaded) => true,
            _ => false,
        }
    }
}

#[derive(PartialEq)]
pub(super) struct LyricsInput {
    pub(super) mid: String,
    pub(super) cover_url: Option<String>,
    pub(super) duration: Option<Duration>,
    pub(super) content: LyricsContent,
    pub(super) font: Font,
    pub(super) font_scale: u32,
    pub(super) line_spacing: u32,
    pub(super) highlight_frame_rate: FrameRate,
    pub(super) scroll_frame_rate: FrameRate,
    pub(super) advancing: bool,
    pub(super) expanded: bool,
    pub(super) visible: bool,
    pub(super) expansion_progress: f32,
}

pub(super) struct CollapseLyrics;

pub(super) struct LyricsView {
    input: LyricsInput,
    audio: Option<Arc<AudioPlayer>>,
    position: Duration,
    backdrop_current_url: Option<String>,
    backdrop_previous_url: Option<String>,
    backdrop_crossfade_phase: bool,
    lyric_layout_cache: LyricLayoutCache,
    lyric_motion_state: Option<LyricMotionState>,
    cached_lyrics_panel: Option<CachedLyricsPanel>,
    lyric_highlight_frame_pending: bool,
    lyric_scroll_frame_pending: bool,
    pending_lyric_reveal_mid: Option<String>,
    lyric_reveal_frame_pending: bool,
    lyric_animation_frame_pending: bool,
    next_lyric_highlight_frame: Option<Instant>,
    next_lyric_scroll_frame: Option<Instant>,
    window_generation: u64,
    window_drag_pressed: bool,
}

impl EventEmitter<CollapseLyrics> for LyricsView {}

impl LyricsView {
    pub(super) fn new(audio: Option<Arc<AudioPlayer>>, font: Font) -> Self {
        Self {
            input: LyricsInput {
                mid: String::new(),
                cover_url: None,
                duration: None,
                content: LyricsContent::Unloaded,
                font,
                font_scale: 100,
                line_spacing: 100,
                highlight_frame_rate: FrameRate::Fps60,
                scroll_frame_rate: FrameRate::Fps60,
                advancing: false,
                expanded: false,
                visible: false,
                expansion_progress: 0.,
            },
            audio,
            position: Duration::ZERO,
            backdrop_current_url: None,
            backdrop_previous_url: None,
            backdrop_crossfade_phase: false,
            lyric_layout_cache: LyricLayoutCache::default(),
            lyric_motion_state: None,
            cached_lyrics_panel: None,
            lyric_highlight_frame_pending: true,
            lyric_scroll_frame_pending: true,
            pending_lyric_reveal_mid: None,
            lyric_reveal_frame_pending: false,
            lyric_animation_frame_pending: false,
            next_lyric_highlight_frame: None,
            next_lyric_scroll_frame: None,
            window_generation: 0,
            window_drag_pressed: false,
        }
    }

    pub(super) fn set_input(
        &mut self,
        input: LyricsInput,
        position: Duration,
        cx: &mut Context<Self>,
    ) {
        if let Some(url) = &input.cover_url
            && self.backdrop_current_url.as_ref() != Some(url)
        {
            self.backdrop_previous_url = self.backdrop_current_url.replace(url.clone());
            self.backdrop_crossfade_phase = !self.backdrop_crossfade_phase;
        }
        let position_changed = self.position != position && !input.advancing;
        if position_changed
            || self.input.visible != input.visible
            || self.input.highlight_frame_rate != input.highlight_frame_rate
            || self.input.scroll_frame_rate != input.scroll_frame_rate
        {
            self.reset_lyric_animation_frames();
        }
        let changed = self.input != input;
        self.input = input;
        self.position = position;
        if changed || position_changed {
            cx.notify();
        }
    }

    pub(super) fn attach_window(&mut self, cx: &mut Context<Self>) {
        self.window_generation = self.window_generation.wrapping_add(1);
        self.lyric_animation_frame_pending = false;
        self.lyric_reveal_frame_pending = false;
        self.reset_lyric_animation_frames();
        cx.notify();
    }

    pub(super) fn start_track(&mut self, mid: &str, expanded: bool, cx: &mut Context<Self>) {
        self.lyric_motion_state = None;
        self.pending_lyric_reveal_mid = expanded.then(|| mid.to_owned());
        cx.notify();
    }

    pub(super) fn reset_layout(&mut self, cx: &mut Context<Self>) {
        self.lyric_layout_cache = LyricLayoutCache::default();
        cx.notify();
    }

    pub(super) fn reset_lyric_animation_frames(&mut self) {
        self.next_lyric_highlight_frame = None;
        self.next_lyric_scroll_frame = None;
        self.lyric_highlight_frame_pending = true;
        self.lyric_scroll_frame_pending = true;
    }

    fn lyric_motion_is_active(&self, reduce_motion: bool) -> bool {
        if reduce_motion {
            return false;
        }
        let Some(state) = &self.lyric_motion_state else {
            return false;
        };
        // 动画时间结束后仍需采样最终位置，不能丢掉下一个限帧截止点的末帧。
        self.cached_lyrics_panel.as_ref().is_none_or(|cache| {
            cache.scroll_offset != cache.row_height * state.target
                || cache.style_anchor != state.target
        })
    }

    pub(super) fn request_lyric_animation_frame(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.lyric_animation_frame_pending {
            return;
        }

        self.lyric_animation_frame_pending = true;
        let generation = self.window_generation;
        cx.on_next_frame(window, move |this, window, cx| {
            if this.window_generation != generation {
                return;
            }
            this.lyric_animation_frame_pending = false;
            if !this.input.visible || !this.input.advancing {
                this.reset_lyric_animation_frames();
                return;
            }

            if this.advance_lyric_frames(cx.background_executor().now(), cx.reduce_motion()) {
                cx.notify();
            }
            this.request_lyric_animation_frame(window, cx);
        });
    }

    fn advance_lyric_frames(&mut self, now: Instant, reduce_motion: bool) -> bool {
        let highlight_due = frame_is_due(
            now,
            self.input.highlight_frame_rate,
            &mut self.next_lyric_highlight_frame,
        );
        let scroll_due = if self.lyric_motion_is_active(reduce_motion) {
            frame_is_due(
                now,
                self.input.scroll_frame_rate,
                &mut self.next_lyric_scroll_frame,
            )
        } else {
            self.next_lyric_scroll_frame = None;
            false
        };
        self.lyric_highlight_frame_pending |= highlight_due;
        self.lyric_scroll_frame_pending |= scroll_due;
        highlight_due || scroll_due
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
            // 切歌后延迟一帧显示歌词；回调比对 mid，避免连续切歌解除后一首歌的等待。
            self.lyric_reveal_frame_pending = true;
            let mid = mid.to_owned();
            let generation = self.window_generation;
            cx.on_next_frame(window, move |this, _, cx| {
                if this.window_generation != generation {
                    return;
                }
                this.lyric_reveal_frame_pending = false;
                if this.pending_lyric_reveal_mid.as_deref() == Some(mid.as_str()) {
                    this.pending_lyric_reveal_mid = None;
                }
                cx.notify();
            });
        }
        true
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
        foreground_for_cover(
            url,
            overlay,
            preferred,
            |cover| cover.sampled_rgb(narrow),
            window,
            cx,
        )
    }

    pub(super) fn foreground_target(
        &self,
        overlay: Hsla,
        preferred: Hsla,
        sample: impl Fn(&BlurredCover) -> [f32; 3],
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
            if let Some(color) = foreground_for_cover(url, overlay, preferred, &sample, window, cx)
            {
                return Some(color);
            }
        }
        None
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

        let lyrics = match &self.input.content {
            LyricsContent::Ready(lyrics) => lyrics.clone(),
            LyricsContent::Loading => return message("正在加载歌词…", None),
            LyricsContent::Error(error) => return message("歌词加载失败", Some(error.clone())),
            LyricsContent::Unloaded => return message("歌词暂未加载", None),
        };
        if lyrics.lines.is_empty() {
            return message("暂无歌词", None);
        }

        let lyric_font = self.input.font.clone();
        let scale_percent = self.input.font_scale;
        let scale = scale_percent as f32 / 100.;
        let row_height = px(LYRIC_ROW_HEIGHT * scale * self.input.line_spacing as f32 / 100.);
        self.lyric_layout_cache.reset_if_needed(
            &lyrics,
            mid,
            compact,
            narrow,
            scale_percent,
            &lyric_font,
        );

        let render_radius =
            ((window.viewport_size().height * 0.65 / row_height).ceil() as usize) + 2;
        let anchor = lyrics.active_index(self.position).unwrap_or(0);
        // 排版缓存复用字形；面板缓存则固定本帧的行、滚动位置和高亮时间，
        // 避免其他控件触发重绘时绕过用户设置的歌词帧率。
        let cache_invalid = self.cached_lyrics_panel.as_ref().is_none_or(|cache| {
            cache.lyrics != Arc::as_ptr(&lyrics) as usize
                || cache.mid != mid
                || cache.compact != compact
                || cache.narrow != narrow
                || cache.scale != scale_percent
                || cache.row_height != row_height
                || cache.font != lyric_font
                || cache.render_radius != render_radius
        });
        if cache_invalid || self.lyric_highlight_frame_pending || self.lyric_scroll_frame_pending {
            let active = Some(anchor);
            let now = cx.background_executor().now();
            let motion_enabled = self.input.visible && self.input.advancing && !cx.reduce_motion();
            let (scroll_anchor, style_anchor) =
                self.lyric_motion_anchors(mid, anchor, motion_enabled, now);
            // 一个通道到期只更新自己的采样；不能因另一通道更快而越过其帧率上限。
            let (scroll_offset, style_anchor) =
                if let Some(cache) = self.cached_lyrics_panel.as_ref().filter(|_| {
                    !cache_invalid && !self.lyric_scroll_frame_pending && motion_enabled
                }) {
                    (cache.scroll_offset, cache.style_anchor)
                } else {
                    (row_height * scroll_anchor, style_anchor)
                };
            let render_start = anchor.saturating_sub(render_radius);
            let render_end = (anchor + render_radius + 1).min(lyrics.lines.len());
            let highlight_position = if let Some(cache) = self
                .cached_lyrics_panel
                .as_ref()
                .filter(|_| !cache_invalid && !self.lyric_highlight_frame_pending)
            {
                cache.highlight_position
            } else {
                lyric_position_for_frame_rate(self.position, self.input.highlight_frame_rate)
            };
            let track_duration = self.input.duration;
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
                    let estimated_line_progress =
                        (current && line.words.is_empty() && line_end > line.start).then(|| {
                            lyric_highlight_progress(line.start, line_end, highlight_position)
                        });
                    let normal = self.lyric_layout_cache.line(
                        index,
                        line,
                        line_end,
                        LyricLayoutStyle::Normal,
                        compact,
                        narrow,
                        scale,
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
                        scale,
                        &lyric_font,
                        window,
                    );
                    let translation = self.lyric_layout_cache.translation(
                        index,
                        line,
                        narrow,
                        scale,
                        &lyric_font,
                        window,
                    );
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
            self.cached_lyrics_panel = Some(CachedLyricsPanel {
                lyrics: Arc::as_ptr(&lyrics) as usize,
                mid: mid.to_owned(),
                compact,
                narrow,
                scale: scale_percent,
                row_height,
                font: lyric_font.clone(),
                render_radius,
                render_start,
                scroll_offset,
                style_anchor,
                highlight_position,
                translation_line_height: translation_line_height(narrow, scale),
                rows,
            });
            self.lyric_highlight_frame_pending = false;
            self.lyric_scroll_frame_pending = false;
        }
        let cached = self
            .cached_lyrics_panel
            .as_ref()
            .expect("lyrics panel cache initialized");

        div()
            .absolute()
            .top(relative(0.44))
            .left_0()
            .right_0()
            .mt(cached.row_height * cached.render_start as f32
                - cached.scroll_offset
                - cached.row_height * (LYRIC_ANCHOR_OFFSET / LYRIC_ROW_HEIGHT))
            .child(PreparedLyricsElement {
                rows: cached.rows.clone(),
                foreground,
                position: cached.highlight_position,
                translation_line_height: cached.translation_line_height,
                row_height: cached.row_height,
            })
            .into_any_element()
    }

    pub(super) fn render_cover_backdrop(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let available_height = (window.viewport_size().height - px(PLAYER_BAR_HEIGHT)).max(px(0.));
        let track_mid = self.input.mid.clone();
        let defer_new_lyrics = self.defer_new_lyrics_until_next_frame(&track_mid, window, cx);
        let viewport_width = f32::from(window.viewport_size().width);
        let compact = viewport_width < 1120.;
        let narrow = viewport_width < 900.;
        let cover_size = px((viewport_width * if compact { 0.3 } else { 0.28 })
            .min(f32::from(available_height) * 0.58)
            .clamp(280., 520.));
        let expansion_progress = self.input.expansion_progress;
        let theme = cx.theme().clone();
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });

        match self.input.cover_url.clone() {
            Some(url) => {
                let current_backdrop_url = self
                    .backdrop_current_url
                    .clone()
                    .unwrap_or_else(|| url.clone());
                let previous_backdrop_url = self.backdrop_previous_url.clone();
                let track_transition_duration = if self.input.expanded {
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
                let collapse_foreground_target = self
                    .foreground_target(
                        overlay,
                        theme.foreground,
                        |cover| cover.collapse_rgb(),
                        window,
                        cx,
                    )
                    .unwrap_or(theme.foreground);
                let reveal_start = (1. - 54. / f32::from(available_height).max(54.)).clamp(0., 1.);
                let control_color_progress =
                    ((expansion_progress - reveal_start) / (1. - reveal_start)).clamp(0., 1.);
                let collapse_foreground = interpolate_color(
                    theme.foreground,
                    collapse_foreground_target,
                    control_color_progress,
                );
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
                let collapse_button = if client_decorations {
                    div()
                        .group("collapse-cover-backdrop")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            Button::new("collapse-cover-backdrop")
                                .custom(
                                    ButtonCustomVariant::new(cx)
                                        .foreground(collapse_foreground)
                                        .hover(theme.background.opacity(0.58))
                                        .active(theme.background.opacity(0.58)),
                                )
                                .size(px(36.))
                                .p_0()
                                .child(
                                    div()
                                        .relative()
                                        .size(px(16.))
                                        .child(
                                            div()
                                                .absolute()
                                                .inset_0()
                                                .group_hover("collapse-cover-backdrop", |style| {
                                                    style.opacity(0.)
                                                })
                                                .child(media_icon_hsla(
                                                    MediaIcon::ChevronDown,
                                                    collapse_foreground,
                                                    px(16.),
                                                )),
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .inset_0()
                                                .opacity(0.)
                                                .group_hover("collapse-cover-backdrop", |style| {
                                                    style.opacity(1.)
                                                })
                                                .child(media_icon_hsla(
                                                    MediaIcon::ChevronDown,
                                                    theme.foreground,
                                                    px(16.),
                                                )),
                                        ),
                                )
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(CollapseLyrics);
                                })),
                        )
                        .into_any_element()
                } else {
                    div()
                        .id("collapse-cover-backdrop")
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
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(CollapseLyrics);
                        }))
                        .into_any_element()
                };
                let collapse_top = if client_decorations { px(18.) } else { px(24.) };
                let collapse_left = if client_decorations { px(16.) } else { px(24.) };
                // 放大后裁掉至少一个源像素，避免线性采样混入纹理外的透明像素。
                // 外扩会同时增大图片宽度，分母扣除两侧各一个源像素。
                let backdrop_padding =
                    px((viewport_width / (BLURRED_COVER_SIZE - 2) as f32).ceil());
                let backdrop_images = div()
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
                    .when_some(previous_backdrop_url, |images, previous_url| {
                        images.child(
                            div()
                                .absolute()
                                .top(-backdrop_padding)
                                .right(-backdrop_padding)
                                .bottom(-backdrop_padding)
                                .left(-backdrop_padding)
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
                            .top(-backdrop_padding)
                            .right(-backdrop_padding)
                            .bottom(-backdrop_padding)
                            .left(-backdrop_padding)
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
                                            img(cached_image_source(
                                                cover_url,
                                                cover_size,
                                                window.scale_factor(),
                                            ))
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
                    .id("lyrics-drag-region")
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
                            .absolute()
                            .top(collapse_top)
                            .left(collapse_left)
                            .child(collapse_button),
                    )
                    .when(client_decorations, |region| {
                        self.window_drag_region(region, cx)
                    })
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }

    fn window_drag_region(&self, region: Stateful<Div>, cx: &Context<Self>) -> Stateful<Div> {
        region
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.window_drag_pressed = false))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.window_drag_pressed = true),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.window_drag_pressed = false),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.window_drag_pressed {
                    this.window_drag_pressed = false;
                    window.start_window_move();
                }
            }))
    }
}

impl Render for LyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.input.visible && self.input.advancing {
            self.position = self
                .audio
                .as_deref()
                .map(AudioPlayer::position)
                .unwrap_or(self.position);
            self.request_lyric_animation_frame(window, cx);
        }
        self.render_cover_backdrop(window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::{LYRIC_ROW_HEIGHT, LyricsContent, LyricsInput, LyricsView, lyrics};
    use crate::settings::FrameRate;
    use gpui::{TestAppContext, VisualTestContext, font, px, white};
    use std::sync::Arc;
    use std::time::Duration;

    #[gpui::test]
    fn test_lyric_frame_rates_are_independent(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for (highlight, scroll) in [
            (FrameRate::Display, FrameRate::Fps30),
            (FrameRate::Fps30, FrameRate::Display),
        ] {
            let (view, cx) =
                cx.add_window_view(|_, _| LyricsView::new(None, font(".SystemUIFont")));
            view.update(cx, |view, _| {
                view.input.mid = "test".into();
                view.input.content = LyricsContent::Ready(Arc::new(lyrics::parse_lyrics(
                    "[00:00.00]first\n[00:01.00]second",
                    None,
                    None,
                )));
                view.input.duration = Some(Duration::from_secs(10));
                view.input.visible = true;
                view.input.advancing = true;
                view.input.highlight_frame_rate = highlight;
                view.input.scroll_frame_rate = scroll;
            });
            let sample = |position, cx: &mut VisualTestContext| {
                cx.update(|window, cx| {
                    view.update(cx, |view, cx| {
                        view.position = position;
                        view.advance_lyric_frames(cx.background_executor().now(), false);
                        let _ = view.render_lyrics_panel("test", white(), true, false, window, cx);
                        let cache = view.cached_lyrics_panel.as_ref().unwrap();
                        (
                            cache.scroll_offset,
                            cache.style_anchor,
                            cache.highlight_position,
                        )
                    })
                })
            };
            sample(Duration::ZERO, cx);
            cx.cx.executor().advance_clock(Duration::from_millis(40));
            sample(Duration::from_millis(1100), cx);
            cx.cx.executor().advance_clock(Duration::from_millis(8));
            let before = sample(Duration::from_millis(1108), cx);
            cx.cx.executor().advance_clock(Duration::from_millis(8));
            let after = sample(Duration::from_millis(1116), cx);
            if scroll == FrameRate::Fps30 {
                assert_eq!((before.0, before.1), (after.0, after.1));
                assert!(after.2 > before.2);
            } else {
                assert_eq!(before.2, after.2);
                assert!(after.0 > before.0);
            }
            cx.cx.executor().advance_clock(Duration::from_millis(34));
            let next = sample(Duration::from_millis(1150), cx);
            assert!(next.0 > after.0);
            assert!(next.2 > after.2);
            cx.cx.executor().advance_clock(Duration::from_millis(400));
            let settled = sample(Duration::from_millis(1550), cx);
            assert_eq!(settled.0, px(LYRIC_ROW_HEIGHT));
            assert_eq!(settled.1, 1.);
        }
    }

    #[gpui::test]
    fn test_paused_lyrics_invalidate_on_seek_and_settings(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| LyricsView::new(None, font(".SystemUIFont")));
        let lyrics = Arc::new(lyrics::parse_lyrics(
            "[00:00.00]first\n[00:01.00]second",
            None,
            None,
        ));
        let input = || LyricsInput {
            mid: "test".into(),
            cover_url: None,
            duration: Some(Duration::from_secs(10)),
            content: LyricsContent::Ready(lyrics.clone()),
            font: font(".SystemUIFont"),
            font_scale: 100,
            line_spacing: 100,
            highlight_frame_rate: FrameRate::Display,
            scroll_frame_rate: FrameRate::Display,
            advancing: false,
            expanded: true,
            visible: true,
            expansion_progress: 1.,
        };
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.set_input(input(), Duration::ZERO, cx);
                let _ = view.render_lyrics_panel("test", white(), true, false, window, cx);
                view.set_input(input(), Duration::from_millis(1500), cx);
                let _ = view.render_lyrics_panel("test", white(), true, false, window, cx);
                let cache = view.cached_lyrics_panel.as_ref().unwrap();
                assert_eq!(cache.highlight_position, Duration::from_millis(1500));
                assert_eq!(cache.scroll_offset, cache.row_height);
                let old_height = cache.row_height;
                let mut next = input();
                next.line_spacing = 120;
                view.set_input(next, Duration::from_millis(1500), cx);
                let _ = view.render_lyrics_panel("test", white(), true, false, window, cx);
                assert!(view.cached_lyrics_panel.as_ref().unwrap().row_height > old_height);
            })
        });
    }
}
