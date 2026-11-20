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
use crate::http::{BLURRED_COVER_SIZE, BlurredCoverStyle, blurred_cover_image};
use crate::icons::{MediaIcon, tinted_media_icon};
use crate::player::AudioPlayer;
use crate::settings::{
    DEFAULT_LYRIC_BACKGROUND_BLUR_PERCENT, DEFAULT_LYRIC_BACKGROUND_CHROMA,
    DEFAULT_LYRIC_BACKGROUND_CONTRAST, DEFAULT_LYRIC_BACKGROUND_MAX_DELTA_E,
    DEFAULT_LYRIC_TINT_PERCENT, FrameRate,
};

const LYRIC_ANCHOR_SPACING_EM: f32 = 3.;
const LYRIC_EDGE_FADE_DISTANCE: f32 = 48.;
const LYRIC_SCROLL_DURATION: Duration = Duration::from_millis(360);
const LYRIC_STYLE_DURATION: Duration = Duration::from_millis(240);
// 跑得太远的跳转会快到分不清方向：动画起点最多离目标这么多行，再按正常速度滚过去。
const LYRIC_MAX_MOTION_LINES: f32 = 8.;
pub(super) const LYRIC_EXPANSION_DURATION: Duration = Duration::from_millis(480);
const LYRIC_TRACK_SWITCH_DURATION: Duration = Duration::from_millis(420);
const LYRIC_COVER_SWITCH_DURATION: Duration = Duration::from_millis(300);

pub(super) fn lyrics_cover_size(window: &Window) -> Option<Pixels> {
    let viewport = window.viewport_size();
    let width = f32::from(viewport.width);
    let height = (f32::from(viewport.height) - PLAYER_BAR_HEIGHT).max(0.);
    (width >= 900.).then(|| {
        px((width * if width < 1120. { 0.3 } else { 0.28 })
            .min(height * 0.58)
            .clamp(280., 520.))
    })
}
const LYRIC_HORIZONTAL_ANCHOR: f32 = 0.42;
const LYRIC_HORIZONTAL_STEP: f32 = 0.5;

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
    spacing: u32,
    font: Font,
}

struct CachedLyricRow {
    top: Pixels,
    height: Pixels,
    anchor: Pixels,
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
    top: Pixels,
    height: Pixels,
    normal: Arc<PreparedLyricLine>,
    active: Arc<PreparedLyricLine>,
    translation: Option<Arc<PreparedLyricTranslation>>,
    emphasis: f32,
    opacity: f32,
    current: bool,
    estimated_line_progress: Option<f32>,
}

pub(super) struct CachedLyricsPanel {
    render_distance: Pixels,
    render_offset: Pixels,
    scroll_anchor: f32,
    scroll_offset: Pixels,
    style_anchor: f32,
    highlight_position: Duration,
    rows: Vec<PreparedLyricRow>,
}

struct PreparedLyricsElement {
    rows: Vec<PreparedLyricRow>,
    foreground: Hsla,
    position: Duration,
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

fn translation_line_height(narrow: bool, scale: f32) -> Pixels {
    (if narrow { px(18.) } else { px(20.) }) * scale
}

fn lyric_font_size(style: LyricLayoutStyle, compact: bool, scale: f32) -> Pixels {
    scale
        * match (style, compact) {
            (LyricLayoutStyle::Active, true) => px(24.),
            (LyricLayoutStyle::Active, false) => px(28.),
            (LyricLayoutStyle::Normal, true) => px(17.),
            (LyricLayoutStyle::Normal, false) => px(19.),
        }
}

fn ruby_line_height(line: &LyricLine, narrow: bool, scale: f32) -> Pixels {
    if line.words.iter().any(|word| word.ruby.is_some()) {
        (if narrow { px(13.) } else { px(15.) }) * scale
    } else {
        px(0.)
    }
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
        let font_size = lyric_font_size(style, compact, scale);
        // ruby 尺寸以当前行为基准，随正文字号等比缩放。
        let ruby_scale = scale * f32::from(font_size)
            / f32::from(lyric_font_size(LyricLayoutStyle::Active, compact, scale));
        let ruby_font_size = (if narrow { px(11.) } else { px(12.) }) * ruby_scale;
        let ruby_line_height = ruby_line_height(line, narrow, ruby_scale);
        let mut font = configured_font.clone();
        font.weight = if active {
            FontWeight::BOLD
        } else {
            FontWeight::MEDIUM
        };
        let has_word_timing = !line.words.is_empty();
        let has_ruby = ruby_line_height > px(0.);
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
            .map_or(text_width, |ruby| text_width.max(ruby.width * scale))
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
    #[allow(clippy::too_many_arguments)]
    fn reset_if_needed(
        &mut self,
        lyrics: &Arc<ParsedLyrics>,
        mid: &str,
        compact: bool,
        narrow: bool,
        scale: u32,
        spacing: u32,
        font: &Font,
    ) -> bool {
        let lyrics_identity = Arc::as_ptr(lyrics) as usize;
        let matches = self.source.as_ref().is_some_and(|source| {
            source.lyrics == lyrics_identity
                && source.mid == mid
                && source.compact == compact
                && source.narrow == narrow
                && source.scale == scale
                && source.spacing == spacing
                && source.font == *font
        });
        if matches {
            return false;
        }

        self.source = Some(LyricLayoutCacheSource {
            lyrics: lyrics_identity,
            mid: mid.to_owned(),
            compact,
            narrow,
            scale,
            spacing,
            font: font.clone(),
        });
        let scale = scale as f32 / 100.;
        let spacing = spacing as f32 / 100.;
        let font_size = lyric_font_size(LyricLayoutStyle::Active, compact, scale);
        let line_height = font_size * 1.3;
        let gap = font_size * spacing;
        let anchor_spacing =
            line_height + (font_size * LYRIC_ANCHOR_SPACING_EM - line_height) * spacing;
        self.rows = Vec::with_capacity(lyrics.lines.len());
        for line in &lyrics.lines {
            let ruby_height = ruby_line_height(line, narrow, scale);
            let translation_height = line
                .translation
                .as_ref()
                .map_or(px(0.), |_| px(4.) + translation_line_height(narrow, scale));
            let height = ruby_height + line_height + translation_height;
            let anchor_offset = ruby_height + line_height / 2.;
            let top = self.rows.last().map_or(px(0.), |previous| {
                (previous.anchor + anchor_spacing - anchor_offset)
                    .max(previous.top + previous.height + gap)
            });
            self.rows.push(CachedLyricRow {
                top,
                height,
                anchor: top + anchor_offset,
                normal: None,
                active: None,
                translation: None,
                translation_prepared: false,
            });
        }
        true
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
        let ruby_line_height = line.ruby_line_height * scale;
        let height = ruby_line_height + line_height;
        let origin = point(origin.x, origin.y + (bounds.size.height - height) / 2.);
        let base_origin_y = origin.y + ruby_line_height;
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
            let fragment_width = PreparedLyricLine::fragment_width(fragment, scale);
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
                    point(x + (fragment_width - ruby.width * scale) / 2., origin.y),
                    ruby_line_height,
                    ruby_color,
                    scale,
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
        style.size.height = self
            .rows
            .last()
            .map_or(px(0.), |row| row.top + row.height)
            .into();
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
            for row in &self.rows {
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
                    point(bounds.origin.x, bounds.origin.y + row.top),
                    size(bounds.size.width, row.height),
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
                let lyric_bounds =
                    Bounds::new(row_bounds.origin, size(row_bounds.size.width, lyric_height));
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
    pub(super) cover_image: Option<Arc<RenderImage>>,
    pub(super) preload_cover_url: Option<String>,
    pub(super) duration: Option<Duration>,
    pub(super) content: LyricsContent,
    pub(super) font: Font,
    pub(super) font_scale: u32,
    pub(super) line_spacing: u32,
    pub(super) background_max_delta_e: f32,
    pub(super) background_chroma: f32,
    pub(super) tint_percent: u32,
    pub(super) background_contrast: f32,
    pub(super) background_blur_percent: u32,
    pub(super) highlight_frame_rate: FrameRate,
    pub(super) scroll_frame_rate: FrameRate,
    pub(super) advancing: bool,
    pub(super) expanded: bool,
    pub(super) visible: bool,
}

pub(super) struct CollapseLyrics;

#[derive(Default)]
struct BackdropSwitch {
    current: Option<String>,
    previous: Option<String>,
    pending: Option<String>,
    pending_animate: bool,
    watching: bool,
    crossfade_phase: bool,
}

impl BackdropSwitch {
    fn hide(&mut self) {
        self.watching = false;
        self.pending_animate = false;
    }

    /// 不可见时不会同步封面，只有前后两次同步都可见时，切换才是用户看着发生的。
    fn sync(&mut self, watching: bool, url: Option<&String>) {
        if let Some(url) = url {
            self.request(url, watching && self.watching);
        } else {
            self.current = None;
            self.previous = None;
            self.pending = None;
        }
        if !watching {
            self.hide();
        }
        self.watching = watching;
    }

    fn request(&mut self, url: &String, animate: bool) {
        if self.pending.as_ref() == Some(url) {
            return;
        }
        if self.current.as_ref() == Some(url) {
            self.pending = None;
            return;
        }
        self.pending_animate = animate;
        self.pending = Some(url.clone());
    }

    fn commit(&mut self) -> Option<bool> {
        let url = self.pending.take()?;
        let animate = self.current.is_some() && self.pending_animate;
        self.previous = self.current.replace(url).filter(|_| animate);
        if animate {
            self.crossfade_phase = !self.crossfade_phase;
        }
        Some(animate)
    }
}

#[derive(Clone, PartialEq)]
struct BackdropFrame {
    current: Option<(Arc<RenderImage>, f32)>,
    previous: Option<Arc<RenderImage>>,
    foreground: Hsla,
}

pub(super) struct LyricsView {
    input: LyricsInput,
    audio: Option<Arc<AudioPlayer>>,
    position: Duration,
    backdrop: BackdropSwitch,
    backdrop_frame: Option<BackdropFrame>,
    window_visible: bool,
    lyric_layout_cache: LyricLayoutCache,
    lyric_motion_state: Option<LyricMotionState>,
    cached_lyrics_panel: Option<CachedLyricsPanel>,
    lyric_highlight_frame_pending: bool,
    lyric_scroll_frame_pending: bool,
    previous_cover: Option<Arc<RenderImage>>,
    cover_fade_phase: bool,
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
                cover_image: None,
                preload_cover_url: None,
                duration: None,
                content: LyricsContent::Unloaded,
                font,
                font_scale: 100,
                line_spacing: 100,
                background_max_delta_e: DEFAULT_LYRIC_BACKGROUND_MAX_DELTA_E,
                background_chroma: DEFAULT_LYRIC_BACKGROUND_CHROMA,
                tint_percent: DEFAULT_LYRIC_TINT_PERCENT,
                background_contrast: DEFAULT_LYRIC_BACKGROUND_CONTRAST,
                background_blur_percent: DEFAULT_LYRIC_BACKGROUND_BLUR_PERCENT,
                highlight_frame_rate: FrameRate::Fps60,
                scroll_frame_rate: FrameRate::Fps60,
                advancing: false,
                expanded: false,
                visible: false,
            },
            audio,
            position: Duration::ZERO,
            backdrop: BackdropSwitch::default(),
            backdrop_frame: None,
            window_visible: true,
            lyric_layout_cache: LyricLayoutCache::default(),
            lyric_motion_state: None,
            cached_lyrics_panel: None,
            lyric_highlight_frame_pending: true,
            lyric_scroll_frame_pending: true,
            previous_cover: None,
            cover_fade_phase: false,
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
        self.backdrop.sync(
            input.visible && self.window_visible,
            input.cover_url.as_ref(),
        );
        if self.input.mid != input.mid {
            self.previous_cover = None;
            if self.input.expanded
                && input.expanded
                && self.window_visible
                && !cx.reduce_motion()
                && input.cover_image.is_some()
                && self.input.cover_image != input.cover_image
            {
                self.previous_cover = self.input.cover_image.clone();
                if self.previous_cover.is_some() {
                    self.cover_fade_phase = !self.cover_fade_phase;
                }
            }
        } else if !input.visible || input.cover_image.is_none() {
            self.previous_cover = None;
        }
        // 与背景一致：初次可见时直接定位，持续可见时位置变化一律滚动。
        if (input.visible && !self.input.visible) || self.input.mid != input.mid {
            self.lyric_motion_state = None;
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

    pub(super) fn set_window_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.window_visible = visible;
        if visible {
            self.lyric_motion_state = None;
            cx.notify();
        } else {
            self.backdrop.hide();
            self.previous_cover = None;
        }
    }

    pub(super) fn attach_window(&mut self, cx: &mut Context<Self>) {
        self.window_generation = self.window_generation.wrapping_add(1);
        self.backdrop.hide();
        self.lyric_animation_frame_pending = false;
        self.previous_cover = None;
        self.reset_lyric_animation_frames();
        self.lyric_motion_state = None;
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
            cache.scroll_anchor != state.target || cache.style_anchor != state.target
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
            if !this.input.visible
                || !this.input.advancing && !this.lyric_motion_is_active(cx.reduce_motion())
            {
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
            let range = target - LYRIC_MAX_MOTION_LINES..=target + LYRIC_MAX_MOTION_LINES;
            state.scroll_from = state.scroll_anchor(now).clamp(*range.start(), *range.end());
            state.style_from = state.style_anchor(now).clamp(*range.start(), *range.end());
            state.target = target;
            state.started_at = now;
        }
        (state.scroll_anchor(now), state.style_anchor(now))
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
        let layout_changed = self.lyric_layout_cache.reset_if_needed(
            &lyrics,
            mid,
            compact,
            narrow,
            scale_percent,
            self.input.line_spacing,
            &lyric_font,
        );
        if layout_changed {
            self.lyric_motion_state = None;
        }

        let render_distance = window.viewport_size().height * 0.65;
        let anchor = lyrics.active_index(self.position).unwrap_or(0);
        // 排版缓存复用字形；面板缓存则固定本帧的行、滚动位置和高亮时间，
        // 避免其他控件触发重绘时绕过用户设置的歌词帧率。
        let cache_invalid = layout_changed
            || self
                .cached_lyrics_panel
                .as_ref()
                .is_none_or(|cache| cache.render_distance != render_distance);
        if cache_invalid || self.lyric_highlight_frame_pending || self.lyric_scroll_frame_pending {
            let active = Some(anchor);
            let now = cx.background_executor().now();
            // 暂停或拖动后音频加载期间也要滚动，不以 advancing 为条件。
            let motion_enabled = self.input.visible && !cx.reduce_motion();
            let (scroll_anchor, style_anchor) =
                self.lyric_motion_anchors(mid, anchor, motion_enabled, now);
            // 一个通道到期只更新自己的采样；不能因另一通道更快而越过其帧率上限。
            let (scroll_anchor, style_anchor) =
                if let Some(cache) = self.cached_lyrics_panel.as_ref().filter(|_| {
                    !cache_invalid && !self.lyric_scroll_frame_pending && motion_enabled
                }) {
                    (cache.scroll_anchor, cache.style_anchor)
                } else {
                    (scroll_anchor, style_anchor)
                };
            let previous_anchor =
                self.lyric_layout_cache.rows[scroll_anchor.floor() as usize].anchor;
            let next_anchor = self.lyric_layout_cache.rows[scroll_anchor.ceil() as usize].anchor;
            let scroll_offset =
                previous_anchor + (next_anchor - previous_anchor) * scroll_anchor.fract();
            let render_start = self
                .lyric_layout_cache
                .rows
                .partition_point(|row| row.top + row.height < scroll_offset - render_distance);
            let render_end = self
                .lyric_layout_cache
                .rows
                .partition_point(|row| row.top <= scroll_offset + render_distance);
            let render_offset = self.lyric_layout_cache.rows[render_start].top;
            let highlight_position = if let Some(cache) = self
                .cached_lyrics_panel
                .as_ref()
                .filter(|_| !cache_invalid && !self.lyric_highlight_frame_pending)
            {
                cache.highlight_position
            } else {
                self.position
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
                        top: self.lyric_layout_cache.rows[index].top - render_offset,
                        height: self.lyric_layout_cache.rows[index].height,
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
                render_distance,
                render_offset,
                scroll_anchor,
                scroll_offset,
                style_anchor,
                highlight_position,
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
            .mt(cached.render_offset - cached.scroll_offset)
            .child(PreparedLyricsElement {
                rows: cached.rows.clone(),
                foreground,
                position: cached.highlight_position,
            })
            .into_any_element()
    }

    pub(super) fn prepare_backdrop(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Hsla> {
        let backdrop_style = BlurredCoverStyle {
            background: cx.theme().background,
            foreground: cx.theme().foreground,
            max_delta_e: self.input.background_max_delta_e,
            chroma: self.input.background_chroma,
            tint_percent: self.input.tint_percent,
            contrast: self.input.background_contrast,
            blur_percent: self.input.background_blur_percent,
        };

        if self.input.visible
            && self.window_visible
            && let Some(url) = self.input.preload_cover_url.as_deref()
            && self.input.cover_url.as_deref() != Some(url)
        {
            let _ = blurred_cover_image(url, backdrop_style, window, cx);
        }
        // 就绪后才切换，避免中途褪到主题底色。
        if let Some(url) = self.backdrop.pending.clone()
            && (!self.backdrop.watching
                || blurred_cover_image(&url, backdrop_style, window, cx).is_some())
        {
            self.backdrop.commit();
        }

        let frame = (!self.input.mid.is_empty()).then(|| {
            let current_backdrop_url = self
                .input
                .cover_url
                .as_ref()
                .map(|url| self.backdrop.current.as_ref().unwrap_or(url).clone());
            let track_transition_duration = if self.input.expanded {
                LYRIC_TRACK_SWITCH_DURATION
            } else {
                Duration::ZERO
            };
            let crossfade = transition(
                ("cover-backdrop", "track-crossfade"),
                if self.backdrop.crossfade_phase {
                    1.
                } else {
                    0.
                },
                Transition::new(track_transition_duration),
                window,
                cx,
            );
            let current_backdrop_opacity = if self.backdrop.crossfade_phase {
                crossfade
            } else {
                1. - crossfade
            };
            if current_backdrop_opacity == 1. {
                self.backdrop.previous = None;
            }
            // 与上方就绪判断在同一层取图，共用同一份生成结果。
            let current_backdrop = current_backdrop_url.and_then(|url| {
                blurred_cover_image(&url, backdrop_style, window, cx).and_then(Result::ok)
            });
            let previous_backdrop = self.backdrop.previous.clone().and_then(|url| {
                blurred_cover_image(&url, backdrop_style, window, cx).and_then(Result::ok)
            });
            let mut lyric_foreground = previous_backdrop
                .as_ref()
                .map_or(backdrop_style.foreground, |cover| cover.foreground);
            if let Some(cover) = &current_backdrop {
                lyric_foreground = lyric_foreground
                    .to_rgb()
                    .blend(cover.foreground.to_rgb().opacity(current_backdrop_opacity))
                    .into();
            }
            BackdropFrame {
                current: current_backdrop.map(|cover| (cover.image, current_backdrop_opacity)),
                previous: previous_backdrop.map(|cover| cover.image),
                foreground: lyric_foreground,
            }
        });
        let foreground = frame.as_ref().map(|frame| frame.foreground);
        if self.backdrop_frame != frame {
            self.backdrop_frame = frame;
            cx.notify();
        }
        foreground
    }

    fn render_cover_backdrop(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let available_height = (window.viewport_size().height - px(PLAYER_BAR_HEIGHT)).max(px(0.));
        let viewport_width = f32::from(window.viewport_size().width);
        let compact = viewport_width < 1120.;
        let narrow = viewport_width < 900.;
        let cover_size = lyrics_cover_size(window);
        let theme = cx.theme().clone();
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });

        match (!self.input.mid.is_empty()).then(|| self.input.mid.clone()) {
            Some(track_mid) => {
                let frame = self.backdrop_frame.clone();
                let lyric_foreground = frame
                    .as_ref()
                    .map_or(theme.foreground, |frame| frame.foreground);
                let lyrics = self.render_lyrics_panel(
                    &track_mid,
                    lyric_foreground,
                    compact,
                    narrow,
                    window,
                    cx,
                );
                let cover_fade = transition(
                    ("lyrics-cover", "track-fade"),
                    if self.cover_fade_phase { 1. } else { 0. },
                    Transition::new(if self.previous_cover.is_some() && self.input.expanded {
                        LYRIC_COVER_SWITCH_DURATION
                    } else {
                        Duration::ZERO
                    }),
                    window,
                    cx,
                );
                let cover_opacity = if self.cover_fade_phase {
                    cover_fade
                } else {
                    1. - cover_fade
                };
                if cover_opacity == 1. {
                    self.previous_cover = None;
                }
                let collapse_button = if client_decorations {
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            Button::new("collapse-cover-backdrop")
                                .custom(
                                    ButtonCustomVariant::new(cx)
                                        .foreground(lyric_foreground)
                                        .hover(theme.background.opacity(0.58))
                                        .active(theme.background.opacity(0.58)),
                                )
                                .size(px(36.))
                                .p_0()
                                .child(tinted_media_icon(
                                    MediaIcon::ChevronDown,
                                    lyric_foreground,
                                    px(16.),
                                ))
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
                        .child(tinted_media_icon(
                            MediaIcon::ChevronDown,
                            lyric_foreground,
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
                    .when_some(
                        frame.as_ref().and_then(|frame| frame.previous.clone()),
                        |images, image| {
                            images.child(
                                div()
                                    .absolute()
                                    .top(-backdrop_padding)
                                    .right(-backdrop_padding)
                                    .bottom(-backdrop_padding)
                                    .left(-backdrop_padding)
                                    .child(img(image).size_full().object_fit(ObjectFit::Cover)),
                            )
                        },
                    )
                    .when_some(
                        frame.and_then(|frame| frame.current),
                        |images, (image, opacity)| {
                            images.child(
                                div()
                                    .absolute()
                                    .top(-backdrop_padding)
                                    .right(-backdrop_padding)
                                    .bottom(-backdrop_padding)
                                    .left(-backdrop_padding)
                                    .opacity(opacity)
                                    .child(img(image).size_full().object_fit(ObjectFit::Cover)),
                            )
                        },
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
                            .when_some(cover_size, |layout, cover_size| {
                                let cover_radius = px(20.);
                                layout.child(
                                    div()
                                        .relative()
                                        .size(cover_size)
                                        .flex_shrink_0()
                                        .rounded(cover_radius)
                                        .overflow_hidden()
                                        .shadow_2xl()
                                        .bg(theme.muted)
                                        .when_some(self.previous_cover.clone(), |cover, image| {
                                            cover.child(
                                                img(image)
                                                    .absolute()
                                                    .inset_0()
                                                    .size_full()
                                                    .rounded(cover_radius)
                                                    .object_fit(ObjectFit::Cover),
                                            )
                                        })
                                        .when_some(
                                            self.input.cover_image.clone(),
                                            |cover, image| {
                                                cover.child(
                                                    img(image)
                                                        .absolute()
                                                        .inset_0()
                                                        .size_full()
                                                        .rounded(cover_radius)
                                                        .object_fit(ObjectFit::Cover)
                                                        .opacity(
                                                            if self.previous_cover.is_some() {
                                                                cover_opacity
                                                            } else {
                                                                1.
                                                            },
                                                        ),
                                                )
                                            },
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
        }
        let element = self.render_cover_backdrop(window, cx);
        // 滚动目标在本帧绘制时才确定，暂停时也需要继续请求帧直到滚动结束。
        if self.input.visible
            && (self.input.advancing || self.lyric_motion_is_active(cx.reduce_motion()))
        {
            self.request_lyric_animation_frame(window, cx);
        }
        element
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BackdropSwitch, LYRIC_COVER_SWITCH_DURATION, LYRIC_MAX_MOTION_LINES, LYRIC_SCROLL_DURATION,
        LYRIC_TRACK_SWITCH_DURATION, LyricsContent, LyricsInput, LyricsView, lyrics,
    };
    use crate::settings::{
        DEFAULT_LYRIC_BACKGROUND_BLUR_PERCENT, DEFAULT_LYRIC_BACKGROUND_CHROMA,
        DEFAULT_LYRIC_BACKGROUND_CONTRAST, DEFAULT_LYRIC_BACKGROUND_MAX_DELTA_E,
        DEFAULT_LYRIC_TINT_PERCENT, FrameRate,
    };
    use gpui::{
        AppContext as _, Context, Entity, IntoElement, Render, RenderImage, TestAppContext,
        VisualTestContext, Window, font, px, white,
    };
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn test_backdrop_switch() {
        let url = |url: &str| Some(url.to_owned());
        let mut backdrop = BackdropSwitch::default();
        backdrop.sync(true, url("first").as_ref());
        assert_eq!(backdrop.commit(), Some(false));

        backdrop.sync(true, url("second").as_ref());
        assert_eq!(backdrop.commit(), Some(true));
        assert_eq!(backdrop.previous.as_deref(), Some("first"));
        assert_eq!(backdrop.current.as_deref(), Some("second"));

        backdrop.sync(false, url("third").as_ref());
        backdrop.sync(true, url("third").as_ref());
        assert_eq!(backdrop.commit(), Some(false));
        assert_eq!(backdrop.previous, None);

        // 窗口关闭期间不再同步，重新打开后的第一次同步就带着新封面。
        backdrop.hide();
        backdrop.sync(true, url("fourth").as_ref());
        assert_eq!(backdrop.commit(), Some(false));

        backdrop.sync(true, url("fifth").as_ref());
        assert_eq!(backdrop.commit(), Some(true));
        assert_eq!(backdrop.previous.as_deref(), Some("fourth"));

        backdrop.sync(true, url("fifth").as_ref());
        assert_eq!(backdrop.commit(), None);

        backdrop.sync(true, url("sixth").as_ref());
        backdrop.sync(true, url("fifth").as_ref());
        assert_eq!(backdrop.commit(), None);
        assert_eq!(backdrop.current.as_deref(), Some("fifth"));

        backdrop.sync(true, None);
        backdrop.sync(true, url("sixth").as_ref());
        assert_eq!(backdrop.commit(), Some(false));
        assert_eq!(backdrop.previous, None);
    }

    #[gpui::test]
    fn test_backdrop_crossfade(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.executor().allow_parking();

        struct BackdropTestRoot(Entity<LyricsView>);
        impl Render for BackdropTestRoot {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                self.0
                    .update(cx, |view, cx| view.prepare_backdrop(window, cx));
                self.0.clone()
            }
        }

        let view = cx.new(|_| LyricsView::new(None, font(".SystemUIFont")));
        let (_, cx) = cx.add_window_view(|_, _| BackdropTestRoot(view.clone()));
        let render = |url: &str, expanded, cx: &mut VisualTestContext| {
            view.update(cx, |view, cx| {
                let input = LyricsInput {
                    mid: url.into(),
                    cover_url: Some(url.into()),
                    expanded,
                    visible: true,
                    ..LyricsView::new(None, font(".SystemUIFont")).input
                };
                view.set_input(input, Duration::ZERO, cx);
            });
            // 测试 HTTP 客户端一律返回 404，封面以失败告终，仍会触发切换。
            while view.read_with(cx, |view, _| view.backdrop.pending.is_some()) {
                cx.update(|window, _| window.refresh());
                cx.run_until_parked();
            }
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            view.read_with(cx, |view, _| view.backdrop.previous.clone())
        };
        assert_eq!(render("first", true, cx), None);
        assert_eq!(render("second", true, cx).as_deref(), Some("first"));
        cx.cx
            .executor()
            .advance_clock(LYRIC_TRACK_SWITCH_DURATION / 2);
        assert_eq!(render("second", true, cx).as_deref(), Some("first"));
        cx.cx
            .executor()
            .advance_clock(LYRIC_TRACK_SWITCH_DURATION / 2);
        assert_eq!(render("second", true, cx), None);

        assert_eq!(render("third", true, cx).as_deref(), Some("second"));
        cx.cx
            .executor()
            .advance_clock(LYRIC_TRACK_SWITCH_DURATION / 2);
        assert_eq!(render("third", false, cx), None);
    }

    #[gpui::test]
    fn test_large_cover_keeps_old_image_until_fade_ends(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| LyricsView::new(None, font(".SystemUIFont")));
        let cover = |gray| {
            let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([gray, gray, gray, 255]));
            Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
                image
            )]))
        };
        let first = cover(64);
        let second = cover(128);
        let display = |mid: &str, cover_image, cx: &mut VisualTestContext| {
            view.update(cx, |view, cx| {
                let input = LyricsInput {
                    mid: mid.into(),
                    cover_image: Some(cover_image),
                    expanded: true,
                    visible: true,
                    ..LyricsView::new(None, font(".SystemUIFont")).input
                };
                view.set_input(input, Duration::ZERO, cx);
            });
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        };
        display("first", first.clone(), cx);
        display("second", second.clone(), cx);
        assert!(view.read_with(cx, |view, _| view.previous_cover.as_ref() == Some(&first)));

        cx.cx.executor().advance_clock(LYRIC_COVER_SWITCH_DURATION);
        display("second", second, cx);
        assert!(view.read_with(cx, |view, _| view.previous_cover.is_none()));
    }

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
                    Some("[00:01.00]第二行"),
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
            assert_eq!(next.2, Duration::from_millis(1150));
            cx.cx.executor().advance_clock(Duration::from_millis(400));
            let settled = sample(Duration::from_millis(1550), cx);
            view.update(cx, |view, _| {
                assert_eq!(settled.0, view.lyric_layout_cache.rows[1].anchor);
                assert!(!view.lyric_motion_is_active(false));
            });
            assert_eq!(settled.1, 1.);
        }
    }

    #[gpui::test]
    fn test_long_lyric_jump_starts_near_target(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| LyricsView::new(None, font(".SystemUIFont")));
        view.update(cx, |view, cx| {
            let now = cx.background_executor().now();
            view.lyric_motion_anchors("test", 0, true, now);
            for (target, expected_start) in [(2, 0.), (100, 100. - LYRIC_MAX_MOTION_LINES)] {
                let (scroll, style) = view.lyric_motion_anchors("test", target, true, now);
                // 起点最多离目标 N 行；近距离跳转仍从原位置开始。
                assert_eq!((scroll, style), (expected_start, expected_start));
                // 动画结束后落在目标行。
                let later = now + LYRIC_SCROLL_DURATION;
                assert_eq!(
                    view.lyric_motion_anchors("test", target, true, later).0,
                    target as f32
                );
            }
        });
    }

    #[gpui::test]
    fn test_lyric_seek_motion(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| LyricsView::new(None, font(".SystemUIFont")));
        let lyrics = Arc::new(lyrics::parse_lyrics(
            "[00:00.00]first\n[00:01.00]second",
            None,
            None,
        ));
        let input = |visible| LyricsInput {
            mid: "test".into(),
            content: LyricsContent::Ready(lyrics.clone()),
            visible,
            ..LyricsView::new(None, font(".SystemUIFont")).input
        };
        // 拖动后音频仍在加载（advancing 为 false），可见时也要从旧行滚动过去。
        let seek = |visible: bool, cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.set_input(input(true), Duration::ZERO, cx);
                    let _ = view.render_lyrics_panel("test", white(), true, false, window, cx);
                    if !visible {
                        view.set_input(input(false), Duration::ZERO, cx);
                    }
                    view.set_input(input(true), Duration::from_millis(1100), cx);
                    let _ = view.render_lyrics_panel("test", white(), true, false, window, cx);
                    view.cached_lyrics_panel.as_ref().unwrap().style_anchor
                })
            })
        };
        assert!(seek(true, cx) < 1.);
        assert_eq!(seek(false, cx), 1.);
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
            cover_image: None,
            preload_cover_url: None,
            duration: Some(Duration::from_secs(10)),
            content: LyricsContent::Ready(lyrics.clone()),
            font: font(".SystemUIFont"),
            font_scale: 100,
            line_spacing: 100,
            background_max_delta_e: DEFAULT_LYRIC_BACKGROUND_MAX_DELTA_E,
            background_chroma: DEFAULT_LYRIC_BACKGROUND_CHROMA,
            tint_percent: DEFAULT_LYRIC_TINT_PERCENT,
            background_contrast: DEFAULT_LYRIC_BACKGROUND_CONTRAST,
            background_blur_percent: DEFAULT_LYRIC_BACKGROUND_BLUR_PERCENT,
            highlight_frame_rate: FrameRate::Display,
            scroll_frame_rate: FrameRate::Display,
            advancing: false,
            expanded: true,
            visible: true,
        };
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.set_input(input(), Duration::ZERO, cx);
                let _ = view.render_lyrics_panel("test", white(), true, false, window, cx);
                view.set_input(input(), Duration::from_millis(1500), cx);
                let _ = view.render_lyrics_panel("test", white(), true, false, window, cx);
                let cache = view.cached_lyrics_panel.as_ref().unwrap();
                assert_eq!(cache.highlight_position, Duration::from_millis(1500));
                let old_offset = cache.scroll_offset;
                let mut next = input();
                next.line_spacing = 120;
                view.set_input(next, Duration::from_millis(1500), cx);
                let _ = view.render_lyrics_panel("test", white(), true, false, window, cx);
                let cache = view.cached_lyrics_panel.as_ref().unwrap();
                assert!(cache.scroll_offset > old_offset);
                assert_eq!(cache.scroll_offset, view.lyric_layout_cache.rows[1].anchor);
            })
        });
    }

    #[gpui::test]
    fn test_lyric_content_spacing(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| LyricsView::new(None, font(".SystemUIFont")));
        let lyrics = Arc::new(lyrics::ParsedLyrics {
            lines: (0..100)
                .map(|index| lyrics::LyricLine {
                    start: Duration::from_secs(index),
                    text: "歌".into(),
                    words: if index / 2 % 4 >= 2 {
                        vec![lyrics::LyricWord {
                            range: 0.."歌".len(),
                            start: Duration::from_secs(index),
                            end: Duration::from_secs(index + 1),
                            ruby: Some("うた".into()),
                        }]
                    } else {
                        Vec::new()
                    },
                    translation: matches!(index / 2 % 4, 1 | 2).then(|| "歌曲".into()),
                })
                .collect(),
        });
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.input.content = LyricsContent::Ready(lyrics.clone());
                for (spacing, distances) in [
                    (100, [84., 88.4, 103.4, 84.]),
                    (50, [60.2, 74.4, 89.4, 65.4]),
                ] {
                    view.input.line_spacing = spacing;
                    let _ = view.render_lyrics_panel("test", white(), false, false, window, cx);
                    for (rows, expected) in view.lyric_layout_cache.rows[..8]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .zip(distances)
                    {
                        assert!(
                            (f32::from(rows[1].anchor - rows[0].anchor) - expected).abs() < 0.01
                        );
                    }
                    for rows in view.lyric_layout_cache.rows.windows(2) {
                        let gap = rows[1].top - rows[0].top - rows[0].height;
                        assert!(f32::from(gap) >= 28. * spacing as f32 / 100. - 0.01);
                    }
                    let cache = view.cached_lyrics_panel.as_ref().unwrap();
                    assert_eq!(cache.rows[0].active.ruby_line_height, px(0.));
                    assert!(cache.rows[0].translation.is_none());
                    for row in &cache.rows {
                        let content_height = row.active.height()
                            + row
                                .translation
                                .as_ref()
                                .map_or(px(0.), |translation| px(4.) + translation.line_height);
                        assert!((f32::from(row.height - content_height)).abs() < 0.01);
                    }
                }

                view.position = Duration::from_secs(50);
                view.input.visible = true;
                view.input.advancing = true;
                view.reset_lyric_animation_frames();
                let _ = view.render_lyrics_panel("test", white(), false, false, window, cx);
                let cache = view.cached_lyrics_panel.as_ref().unwrap();
                assert_eq!(cache.scroll_offset, view.lyric_layout_cache.rows[50].anchor);
                assert!(cache.rows.len() < lyrics.lines.len());
                let visible = view
                    .lyric_layout_cache
                    .rows
                    .iter()
                    .filter(|row| {
                        row.top + row.height >= cache.scroll_offset - cache.render_distance
                            && row.top <= cache.scroll_offset + cache.render_distance
                    })
                    .collect::<Vec<_>>();
                assert_eq!(cache.rows.len(), visible.len());
                assert_eq!(cache.render_offset, visible[0].top);
                assert_eq!(cache.rows[0].top, px(0.));
                for (row, visible) in cache.rows.iter().zip(visible) {
                    assert!((f32::from(cache.render_offset + row.top - visible.top)).abs() < 0.01);
                }

                view.input.content = LyricsContent::Ready(Arc::new(lyrics::parse_lyrics(
                    "[00:00.00]更新后的歌词",
                    None,
                    None,
                )));
                let _ = view.render_lyrics_panel("test", white(), false, false, window, cx);
                let cache = view.cached_lyrics_panel.as_ref().unwrap();
                assert_eq!(cache.rows.len(), 1);
                assert_eq!(cache.scroll_anchor, 0.);
            });
        });
    }
}
