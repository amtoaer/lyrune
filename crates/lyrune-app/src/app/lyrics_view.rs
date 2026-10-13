use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{prelude::*, *};
use gpui_base::motion::{Transition, transition};
use gpui_component::{
    ActiveTheme as _, StyledExt as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex, v_flex,
};

#[cfg(test)]
use super::lyrics;
use super::lyrics::{LyricLine, ParsedLyrics};
use super::{LyruneView, PLAYER_BAR_HEIGHT};
use crate::http::{BlurredCover, blurred_cover, blurred_image_source, cached_image_source};
use crate::icons::{MediaIcon, media_icon_hsla};
use crate::settings::LyricFrameRate;

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
    highlight: LyricFrameRate,
    scroll: LyricFrameRate,
) -> Option<Duration> {
    match (highlight.frame_interval(), scroll.frame_interval()) {
        (Some(highlight), Some(scroll)) => Some(highlight.min(scroll)),
        _ => None,
    }
}

pub(super) fn lyric_position_for_frame_rate(
    position: Duration,
    frame_rate: LyricFrameRate,
) -> Duration {
    let Some(interval) = frame_rate.frame_interval() else {
        return position;
    };
    let interval_nanos = interval.as_nanos();
    let quantized_nanos = position.as_nanos() / interval_nanos * interval_nanos;
    Duration::from_nanos(quantized_nanos.min(u64::MAX as u128) as u64)
}

pub(super) fn lyric_frame_is_due(
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

    // 正常帧沿用原来的截止时间，避免每次从 now 计时导致累计漂移；落后整帧才重置。
    let delay = now.saturating_duration_since(deadline);
    *next_frame = Some(if delay < interval {
        deadline + interval
    } else {
        now + interval
    });
    true
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

impl LyruneView {
    pub(super) fn inactive_window_frame_interval(&self) -> Option<Duration> {
        if self.cover_backdrop_visible {
            combined_lyric_frame_interval(
                self.settings.lyric_highlight_frame_rate,
                self.settings.lyric_scroll_frame_rate,
            )
        } else {
            Some(crate::INACTIVE_WINDOW_FRAME_INTERVAL)
        }
    }

    pub(super) fn reset_lyric_animation_frames(&mut self) {
        self.next_lyric_highlight_frame = None;
        self.next_lyric_scroll_frame = None;
        self.lyric_panel_frame_pending = true;
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

    pub(super) fn request_lyric_animation_frame(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.lyric_animation_frame_pending {
            return;
        }

        self.lyric_animation_frame_pending = true;
        cx.on_next_frame(window, |this, window, cx| {
            this.lyric_animation_frame_pending = false;
            if !this.cover_backdrop_visible || !this.playback_is_advancing() {
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
                this.lyric_panel_frame_pending = true;
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
            // 切歌后延迟一帧显示歌词；回调比对 mid，避免连续切歌解除后一首歌的等待。
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

    pub(super) fn set_cover_backdrop_expanded(
        &mut self,
        expanded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cover_backdrop_expanded = expanded;
        if !expanded {
            self.cover_backdrop_fully_expanded = false;
        }
        if expanded && !self.cover_backdrop_visible {
            self.cover_backdrop_visible = true;
            self.reset_lyric_animation_frames();
        }
        window.set_inactive_frame_interval(self.inactive_window_frame_interval());
        self.wake_playback_ticks();
        cx.notify();
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
        let scale_percent = self.settings.lyric_font_scale;
        let scale = scale_percent as f32 / 100.;
        let row_height =
            px(LYRIC_ROW_HEIGHT * scale * self.settings.lyric_line_spacing as f32 / 100.);
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
        let cache_needs_update = self.lyric_panel_frame_pending
            || self.cached_lyrics_panel.as_ref().is_none_or(|cache| {
                cache.lyrics != Arc::as_ptr(&lyrics) as usize
                    || cache.mid != mid
                    || cache.compact != compact
                    || cache.narrow != narrow
                    || cache.scale != scale_percent
                    || cache.row_height != row_height
                    || cache.font != lyric_font
                    || cache.render_radius != render_radius
            });
        if cache_needs_update {
            let active = Some(anchor);
            let now = cx.background_executor().now();
            let motion_enabled =
                self.cover_backdrop_visible && self.playback_is_advancing() && !cx.reduce_motion();
            let (scroll_anchor, style_anchor) =
                self.lyric_motion_anchors(mid, anchor, motion_enabled, now);
            let scroll_offset = row_height * scroll_anchor;
            let render_start = anchor.saturating_sub(render_radius);
            let render_end = (anchor + render_radius + 1).min(lyrics.lines.len());
            let highlight_position = lyric_position_for_frame_rate(
                self.position,
                self.settings.lyric_highlight_frame_rate,
            );
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
                highlight_position,
                translation_line_height: translation_line_height(narrow, scale),
                rows,
            });
            self.lyric_panel_frame_pending = false;
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
            Transition::new(LYRIC_EXPANSION_DURATION).ease(|progress| 1. - (1. - progress).powi(4)),
            window,
            cx,
        );
        if self.cover_backdrop_expanded && expansion_progress >= 1. {
            self.cover_backdrop_fully_expanded = true;
        }
        let visible = self.cover_backdrop_expanded || expansion_progress > 0.;
        if self.cover_backdrop_visible != visible {
            self.cover_backdrop_visible = visible;
            self.reset_lyric_animation_frames();
            window.set_inactive_frame_interval(self.inactive_window_frame_interval());
        }
        let height = available_height * expansion_progress;
        let theme = cx.theme().clone();
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });

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
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_cover_backdrop_expanded(false, window, cx);
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
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_cover_backdrop_expanded(false, window, cx);
                        }))
                        .into_any_element()
                };
                let collapse_top = if client_decorations { px(18.) } else { px(24.) };
                let collapse_left = if client_decorations { px(16.) } else { px(24.) };
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
                    .id("cover-backdrop")
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(PLAYER_BAR_HEIGHT))
                    .h(available_height)
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(height)
                            .overflow_hidden()
                            .occlude()
                            .child(
                                div()
                                    .id("lyrics-drag-region")
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .bottom_0()
                                    .h(available_height)
                                    .bg(theme.background)
                                    .child(backdrop_images)
                                    .child(div().absolute().inset_0().bg(
                                        theme.background.opacity(LYRIC_BACKGROUND_OVERLAY_OPACITY),
                                    ))
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
                                    }),
                            ),
                    )
                    .into_any_element()
            }
            None => div().into_any_element(),
        }
    }
}
