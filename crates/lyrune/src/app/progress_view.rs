//! 播放进度控件拥有滑块、拖动预览和悬停状态；松手后才向播放端发出 seek。

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{prelude::*, *};
use gpui_base::{
    Slider as BaseSlider, SliderIndicator, SliderTrack,
    motion::{Transition, transition},
};
use gpui_component::{
    ActiveTheme as _, StyledExt as _,
    slider::{SliderEvent, SliderState},
};

use super::animation::frame_is_due;
use super::progress_fraction;
use crate::player::AudioPlayer;
use crate::settings::FrameRate;

#[derive(PartialEq)]
pub(super) struct ProgressInput {
    pub(super) duration: Option<Duration>,
    pub(super) interactive: bool,
    pub(super) advancing: bool,
    pub(super) frame_rate: FrameRate,
    pub(super) font: Font,
    pub(super) foreground: Hsla,
}

pub(super) struct SeekTo(pub(super) Duration);

pub(super) struct PlaybackProgressView {
    slider: Entity<SliderState>,
    // 与播放端共享设备时钟，显示采样不修改音频或后台保存的节奏。
    audio: Option<Arc<AudioPlayer>>,
    position: Duration,
    next_frame: Option<Instant>,
    frame_pending: bool,
    window_generation: u64,
    input: ProgressInput,
    pub(super) seek_preview: Option<Duration>,
    hovered: bool,
    hover_fraction: Option<f32>,
    _subscription: Subscription,
}

impl EventEmitter<SeekTo> for PlaybackProgressView {}

impl PlaybackProgressView {
    pub(super) fn new(audio: Option<Arc<AudioPlayer>>, font: Font, cx: &mut Context<Self>) -> Self {
        let slider = cx.new(|_| progress_slider_state(0.));
        let subscription = cx.subscribe(&slider, |this, _, event, cx| {
            match event {
                SliderEvent::Change(value) => {
                    // 音频进度同步不能覆盖拖动预览，Release 才真正 seek。
                    this.seek_preview = this
                        .input
                        .duration
                        .map(|duration| duration.mul_f32(value.end().clamp(0., 1.)));
                }
                SliderEvent::Release(value) => {
                    this.seek_preview = None;
                    if let Some(duration) = this.input.duration {
                        cx.emit(SeekTo(duration.mul_f32(value.end().clamp(0., 1.))));
                    }
                }
            }
            cx.notify();
        });
        Self {
            slider,
            audio,
            position: Duration::ZERO,
            next_frame: None,
            frame_pending: false,
            window_generation: 0,
            input: ProgressInput {
                duration: None,
                interactive: false,
                advancing: false,
                frame_rate: FrameRate::Fps60,
                font,
                foreground: cx.theme().secondary_foreground,
            },
            seek_preview: None,
            hovered: false,
            hover_fraction: None,
            _subscription: subscription,
        }
    }

    pub(super) fn set_input(
        &mut self,
        input: ProgressInput,
        position: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.input != input;
        if self.input.frame_rate != input.frame_rate || self.input.advancing != input.advancing {
            self.next_frame = None;
        }
        self.input = input;
        // 播放中的位置仅由本控件的帧调度采样，其他视图重绘不能绕过帧率限制。
        let position_changed = !self.input.advancing && self.position != position;
        if position_changed {
            self.position = position;
            self.sync_slider(window, cx);
        }
        if changed || position_changed {
            cx.notify();
        }
    }

    pub(super) fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 旧窗口的回调不能清除新窗口的 pending 标记，也不能建立第二条帧链。
        self.window_generation = self.window_generation.wrapping_add(1);
        self.frame_pending = false;
        self.next_frame = None;
        self.sync_slider(window, cx);
        cx.notify();
    }

    fn request_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.frame_pending
            || !self.input.advancing
            || self.seek_preview.is_some()
            || !self.audio.as_deref().is_some_and(AudioPlayer::is_playing)
        {
            return;
        }
        self.frame_pending = true;
        let generation = self.window_generation;
        cx.on_next_frame(window, move |this, window, cx| {
            if this.window_generation != generation {
                return;
            }
            this.frame_pending = false;
            if !this.input.advancing
                || this.seek_preview.is_some()
                || !this.audio.as_deref().is_some_and(AudioPlayer::is_playing)
            {
                this.next_frame = None;
                return;
            }
            if frame_is_due(
                cx.background_executor().now(),
                this.input.frame_rate,
                &mut this.next_frame,
            ) {
                this.position = this
                    .audio
                    .as_deref()
                    .map(AudioPlayer::position)
                    .unwrap_or(this.position);
                this.sync_slider(window, cx);
                cx.notify();
            }
            this.request_frame(window, cx);
        });
    }

    pub(super) fn start_track(
        &mut self,
        position: Duration,
        duration: Duration,
        cx: &mut Context<Self>,
    ) {
        self.position = position;
        self.input.duration = Some(duration);
        self.input.advancing = false;
        self.next_frame = None;
        self.hovered = false;
        self.hover_fraction = None;
        self.slider.update(cx, |slider, cx| {
            *slider = progress_slider_state(progress_fraction(position, duration));
            cx.notify();
        });
        cx.notify();
    }

    pub(super) fn stop(&mut self, cx: &mut Context<Self>) {
        self.seek_preview = None;
        self.position = Duration::ZERO;
        self.input.advancing = false;
        self.next_frame = None;
        self.slider.update(cx, |slider, cx| {
            *slider = progress_slider_state(0.);
            cx.notify();
        });
        cx.notify();
    }

    fn sync_slider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.seek_preview.is_none() {
            let progress = self
                .input
                .duration
                .map_or(0., |duration| progress_fraction(self.position, duration));
            self.slider.update(cx, |slider, cx| {
                if slider.value().end() != progress {
                    slider.set_value(progress, window, cx);
                }
            });
        }
    }
}

fn progress_slider_state(value: f32) -> SliderState {
    SliderState::new()
        .min(0.)
        .max(1.)
        .step(0.001)
        .default_value(value)
}

const PROGRESS_THUMB_RADIUS: Pixels = px(8.);

pub(super) fn format_playback_time(duration: Duration) -> String {
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

impl Render for PlaybackProgressView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.request_frame(window, cx);
        let theme = cx.theme().clone();
        let monospace_font = self.input.font.clone();
        let duration = self.input.duration.unwrap_or_default();
        let percentage = if self.seek_preview.is_none() && !duration.is_zero() {
            progress_fraction(self.position, duration)
        } else {
            self.slider.read(cx).percentage().end
        };
        let has_track = self.input.duration.is_some();
        let interactive = self.input.interactive;
        let bar_color = theme.slider_bar;
        let thumb_color = theme.slider_thumb;
        let hover_fraction = interactive.then_some(self.hover_fraction).flatten();
        let hover_visible = self.hovered && hover_fraction.is_some();
        let hover_opacity = transition(
            ("player-progress", "hover-opacity"),
            if hover_visible { 1. } else { 0. },
            Transition::new(Duration::from_millis(if hover_visible { 120 } else { 170 })),
            window,
            cx,
        );
        let edge_time_color = self.input.foreground;

        let track = SliderTrack::new(&self.slider)
            .disabled(!interactive)
            .h(px(20.))
            .w_full()
            .child(
                SliderIndicator::new(&self.slider)
                    .relative()
                    .h(px(4.))
                    .w_full()
                    .bg(theme.border)
                    .active(|this| this.bg(bar_color.opacity(0.35)))
                    .child(
                        // Painted as paths because quads and layout snap to device pixels,
                        // which makes slow progress advance in visible one-pixel steps.
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                let x = bounds.left() + bounds.size.width * percentage;
                                if x > bounds.left() {
                                    let mut fill = PathBuilder::fill();
                                    fill.add_polygon(
                                        &[
                                            bounds.origin,
                                            point(x, bounds.top()),
                                            point(x, bounds.bottom()),
                                            bounds.bottom_left(),
                                        ],
                                        true,
                                    );
                                    if let Ok(path) = fill.build() {
                                        window.paint_path(path, bar_color);
                                    }
                                }
                                if has_track {
                                    let center = point(x, bounds.center().y);
                                    for (radius, color) in [
                                        (PROGRESS_THUMB_RADIUS, bar_color.opacity(0.5)),
                                        (PROGRESS_THUMB_RADIUS - px(1.), thumb_color),
                                    ] {
                                        let mut circle = PathBuilder::fill();
                                        circle.move_to(point(center.x - radius, center.y));
                                        for to in [center.x + radius, center.x - radius] {
                                            circle.arc_to(
                                                point(radius, radius),
                                                px(0.),
                                                false,
                                                true,
                                                point(to, center.y),
                                            );
                                        }
                                        circle.close();
                                        if let Ok(path) = circle.build() {
                                            window.paint_path(path, color);
                                        }
                                    }
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    ),
            );
        let control = BaseSlider::new(&self.slider)
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
                        let bounds = this.slider.read(cx).bounds();
                        if bounds.size.width <= px(0.) {
                            return;
                        }
                        let inner =
                            (event.position.x - bounds.left()).clamp(px(0.), bounds.size.width);
                        this.hover_fraction = Some(inner / bounds.size.width);
                        this.hovered = true;
                        cx.notify();
                    }))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !hovered && this.hovered {
                            this.hovered = false;
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
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use gpui::{AppContext as _, TestAppContext, font};
    use gpui_component::{ActiveTheme as _, slider::SliderEvent};

    use super::{PlaybackProgressView, ProgressInput, SeekTo};
    use crate::settings::FrameRate;

    #[gpui::test]
    fn test_seek_on_release(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let progress = cx.new(|cx| PlaybackProgressView::new(None, font(".SystemUIFont"), cx));
        progress.update(cx, |view, cx| {
            view.start_track(Duration::from_secs(10), Duration::from_secs(100), cx);
        });
        let seeks = Rc::new(RefCell::new(Vec::new()));
        let received = seeks.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&progress, move |_, event: &SeekTo, _| {
                received.borrow_mut().push(event.0)
            })
        });
        let slider = progress.read_with(cx, |view, _| view.slider.clone());
        slider.update(cx, |_, cx| cx.emit(SliderEvent::Change(0.5.into())));
        cx.run_until_parked();
        assert!(seeks.borrow().is_empty());
        progress.read_with(cx, |view, _| {
            assert_eq!(view.seek_preview, Some(Duration::from_secs(50)));
            assert_eq!(view.position, Duration::from_secs(10));
        });

        slider.update(cx, |_, cx| cx.emit(SliderEvent::Release(0.5.into())));
        cx.run_until_parked();
        assert_eq!(*seeks.borrow(), [Duration::from_secs(50)]);
        progress.read_with(cx, |view, _| assert_eq!(view.seek_preview, None));
    }

    #[gpui::test]
    fn test_external_updates_keep_frame_position(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (progress, cx) =
            cx.add_window_view(|_, cx| PlaybackProgressView::new(None, font(".SystemUIFont"), cx));
        cx.update(|window, cx| {
            progress.update(cx, |view, cx| {
                let duration = Duration::from_secs(100);
                view.start_track(Duration::from_secs(10), duration, cx);
                for advancing in [true, false] {
                    view.set_input(
                        ProgressInput {
                            duration: Some(duration),
                            interactive: true,
                            advancing,
                            frame_rate: FrameRate::Fps60,
                            font: font(".SystemUIFont"),
                            foreground: cx.theme().foreground,
                        },
                        Duration::from_secs(20),
                        window,
                        cx,
                    );
                    // 播放时其他视图更新不能推进本帧；暂停时立即应用最终位置。
                    assert_eq!(
                        view.position,
                        Duration::from_secs(if advancing { 10 } else { 20 })
                    );
                }
            });
        });
    }
}
