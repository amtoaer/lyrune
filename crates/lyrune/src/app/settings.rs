//! 设置页面及偏好的即时生效；序列化格式、默认值和磁盘保存由 crate::settings 管理。

use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState, MaskPattern, NumberInput, NumberInputEvent},
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    v_flex,
};
use lyrune_api::Quality;

use super::preferences::Preferences;
use super::{LyruneView, page_horizontal_padding};
use crate::cache::audio_cache_limit_bytes;
use crate::design::{self, ColorTheme};
use crate::settings::{
    AppSettings, ColorThemeMode, FrameRate, MAX_IMAGE_CACHE_CAPACITY, MAX_LYRIC_SCALE_PERCENT,
    MAX_NAVIGATION_HISTORY_LIMIT, MIN_SCALE_PERCENT, TrayIconStyle, WindowDecoration,
    default_lyric_font_families, default_monospace_font_families, default_ui_font_families,
    parse_font_families,
};

struct SettingsInputs {
    ui_font: Entity<InputState>,
    monospace_font: Entity<InputState>,
    lyric_font: Entity<InputState>,
    audio_cache_limit: Entity<InputState>,
    image_cache_capacity: Entity<InputState>,
    navigation_history_limit: Entity<InputState>,
    lyric_font_scale: Entity<InputState>,
    lyric_line_spacing: Entity<InputState>,
    lyric_background_chroma: Entity<InputState>,
    lyric_background_contrast: Entity<InputState>,
}

impl SettingsInputs {
    fn new(settings: &AppSettings, window: &mut Window, cx: &mut App) -> Self {
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
        });
        let image_cache_capacity_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.image_cache_capacity.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: None,
                })
                .validate(|value, _| value.parse::<usize>().is_ok_and(|value| value > 0))
                .min(1.)
        });
        let navigation_history_limit_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.navigation_history_limit.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: None,
                })
                .validate(|value, _| value.parse::<usize>().is_ok_and(|value| value > 0))
                .min(1.)
        });
        let scale_input = |value: u32, window: &mut Window, cx: &mut App| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value.to_string())
                    .mask_pattern(MaskPattern::Number {
                        separator: None,
                        fraction: None,
                    })
                    .validate(|value, _| value.parse::<u32>().is_ok())
                    .min(MIN_SCALE_PERCENT as f64)
            })
        };
        let lyric_font_scale_input = scale_input(settings.lyric_font_scale, window, cx);
        let lyric_line_spacing_input = scale_input(settings.lyric_line_spacing, window, cx);
        let lyric_background_chroma_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.lyric_background_chroma.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: Some(2),
                })
                .validate(|value, _| value.parse::<f32>().is_ok_and(f32::is_finite))
                .min(0.)
                .max(0.4)
        });
        let lyric_background_contrast_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.lyric_background_contrast.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: Some(1),
                })
                .validate(|value, _| value.parse::<f32>().is_ok_and(f32::is_finite))
                .min(1.)
                .max(21.)
        });
        Self {
            ui_font: ui_font_input,
            monospace_font: monospace_font_input,
            lyric_font: lyric_font_input,
            audio_cache_limit: audio_cache_limit_input,
            image_cache_capacity: image_cache_capacity_input,
            navigation_history_limit: navigation_history_limit_input,
            lyric_font_scale: lyric_font_scale_input,
            lyric_line_spacing: lyric_line_spacing_input,
            lyric_background_chroma: lyric_background_chroma_input,
            lyric_background_contrast: lyric_background_contrast_input,
        }
    }

    fn set_fonts(
        &self,
        ui: &[String],
        monospace: &[String],
        lyrics: &[String],
        window: &mut Window,
        cx: &mut App,
    ) {
        for (input, families) in [
            (&self.ui_font, ui),
            (&self.monospace_font, monospace),
            (&self.lyric_font, lyrics),
        ] {
            input.update(cx, |input, cx| {
                input.set_value(families.join(", "), window, cx)
            });
        }
    }
}

pub(super) enum SettingsAction {
    ColorTheme {
        theme: ColorTheme,
        light: bool,
    },
    ThemeMode(ColorThemeMode),
    TrayIcon(TrayIconStyle),
    CheckOnStartup(bool),
    WindowDecoration(WindowDecoration),
    Quality(Quality),
    ProgressRate(FrameRate),
    HighlightRate(FrameRate),
    ScrollRate(FrameRate),
    AudioCacheLimit(u64),
    ImageCacheCapacity(usize),
    HistoryLimit(usize),
    Scale {
        lyric: u32,
        spacing: u32,
    },
    LyricBackground {
        chroma: f32,
        contrast: f32,
    },
    Fonts {
        ui: Vec<String>,
        monospace: Vec<String>,
        lyrics: Vec<String>,
    },
    CheckUpdates,
}

pub(super) struct SettingsPage {
    preferences: Entity<Preferences>,
    settings_inputs: SettingsInputs,
    settings_scroll_handle: ScrollHandle,
    pub(super) update_check_in_progress: bool,
    _preferences_subscription: Subscription,
    _input_subscriptions: Vec<Subscription>,
    #[cfg(test)]
    render_count: usize,
}

impl EventEmitter<SettingsAction> for SettingsPage {}

impl SettingsPage {
    pub(super) fn new(
        preferences: Entity<Preferences>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings_inputs = preferences.update(cx, |preferences, cx| {
            SettingsInputs::new(&preferences.settings, window, cx)
        });
        let subscription = cx.observe(&preferences, |_, _, cx| cx.notify());
        Self {
            preferences,
            settings_inputs,
            settings_scroll_handle: ScrollHandle::new(),
            update_check_in_progress: false,
            _preferences_subscription: subscription,
            _input_subscriptions: Vec::new(),
            #[cfg(test)]
            render_count: 0,
        }
    }

    pub(super) fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 窗口重开后替换窗口绑定；输入内容和滚动位置仍由同一个页面实体持有。
        self._input_subscriptions.clear();
        type ApplyNumberSetting = fn(&mut SettingsPage, &mut Window, &mut Context<SettingsPage>);
        let numeric_inputs: [(_, _, _, ApplyNumberSetting); 7] = [
            (
                &self.settings_inputs.audio_cache_limit,
                1.,
                1.,
                Self::apply_audio_cache_limit,
            ),
            (
                &self.settings_inputs.image_cache_capacity,
                1.,
                1.,
                Self::apply_image_cache_capacity,
            ),
            (
                &self.settings_inputs.navigation_history_limit,
                1.,
                1.,
                Self::apply_navigation_history_limit,
            ),
            (
                &self.settings_inputs.lyric_font_scale,
                10.,
                MIN_SCALE_PERCENT as f64,
                Self::apply_scale_settings,
            ),
            (
                &self.settings_inputs.lyric_line_spacing,
                10.,
                MIN_SCALE_PERCENT as f64,
                Self::apply_scale_settings,
            ),
            (
                &self.settings_inputs.lyric_background_chroma,
                0.01,
                0.,
                Self::apply_lyric_background_settings,
            ),
            (
                &self.settings_inputs.lyric_background_contrast,
                0.1,
                1.,
                Self::apply_lyric_background_settings,
            ),
        ];
        for (input, step, min, apply) in numeric_inputs {
            // 内建 step 只发 Change，无法区别于输入草稿；接管 Step 后立即提交。
            input.update(cx, |input, cx| input.set_step(None, window, cx));
            self._input_subscriptions.push(cx.subscribe_in(
                input,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        apply(this, window, cx);
                    }
                },
            ));
            self._input_subscriptions.push(cx.subscribe_in(
                input,
                window,
                move |this, input, event: &NumberInputEvent, window, cx| {
                    let NumberInputEvent::Step(action) = event;
                    if let Some(value) = gpui_base::step_value(
                        input.read(cx).value().as_ref(),
                        *action,
                        step,
                        Some(min),
                        None,
                    ) {
                        input.update(cx, |input, cx| input.set_value(value, window, cx));
                        apply(this, window, cx);
                    }
                },
            ));
        }
    }

    fn apply_audio_cache_limit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.preferences.read(cx).settings.audio_cache_limit_gb;
        let value = self
            .settings_inputs
            .audio_cache_limit
            .read(cx)
            .value()
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(current);
        self.settings_inputs
            .audio_cache_limit
            .update(cx, |input, cx| {
                input.set_value(value.to_string(), window, cx)
            });
        cx.emit(SettingsAction::AudioCacheLimit(value));
    }

    fn apply_image_cache_capacity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.preferences.read(cx).settings.image_cache_capacity;
        let value = self
            .settings_inputs
            .image_cache_capacity
            .read(cx)
            .value()
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(current)
            .min(MAX_IMAGE_CACHE_CAPACITY);
        self.settings_inputs
            .image_cache_capacity
            .update(cx, |input, cx| {
                input.set_value(value.to_string(), window, cx)
            });
        cx.emit(SettingsAction::ImageCacheCapacity(value));
    }

    fn apply_navigation_history_limit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.preferences.read(cx).settings.navigation_history_limit;
        let value = self
            .settings_inputs
            .navigation_history_limit
            .read(cx)
            .value()
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(current)
            .min(MAX_NAVIGATION_HISTORY_LIMIT);
        self.settings_inputs
            .navigation_history_limit
            .update(cx, |input, cx| {
                input.set_value(value.to_string(), window, cx)
            });
        cx.emit(SettingsAction::HistoryLimit(value));
    }

    fn apply_scale_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let read = |input: &Entity<InputState>, current: u32, max: u32, cx: &mut Context<Self>| {
            input
                .read(cx)
                .value()
                .parse::<u32>()
                .map_or(current, |value| value.clamp(MIN_SCALE_PERCENT, max))
        };
        let lyric = read(
            &self.settings_inputs.lyric_font_scale,
            self.preferences.read(cx).settings.lyric_font_scale,
            MAX_LYRIC_SCALE_PERCENT,
            cx,
        );
        let spacing = read(
            &self.settings_inputs.lyric_line_spacing,
            self.preferences.read(cx).settings.lyric_line_spacing,
            MAX_LYRIC_SCALE_PERCENT,
            cx,
        );
        for (input, value) in [
            (&self.settings_inputs.lyric_font_scale, lyric),
            (&self.settings_inputs.lyric_line_spacing, spacing),
        ] {
            input.update(cx, |input, cx| {
                input.set_value(value.to_string(), window, cx)
            });
        }
        cx.emit(SettingsAction::Scale { lyric, spacing });
    }

    fn apply_lyric_background_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = &self.preferences.read(cx).settings;
        let chroma = self
            .settings_inputs
            .lyric_background_chroma
            .read(cx)
            .value()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .unwrap_or(settings.lyric_background_chroma)
            .clamp(0., 0.4);
        let contrast = self
            .settings_inputs
            .lyric_background_contrast
            .read(cx)
            .value()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .unwrap_or(settings.lyric_background_contrast)
            .clamp(1., 21.);
        for (input, value) in [
            (
                &self.settings_inputs.lyric_background_chroma,
                chroma.to_string(),
            ),
            (
                &self.settings_inputs.lyric_background_contrast,
                contrast.to_string(),
            ),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        cx.emit(SettingsAction::LyricBackground { chroma, contrast });
    }

    fn apply_font_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ui = parse_font_families(self.settings_inputs.ui_font.read(cx).value().as_ref());
        let monospace = parse_font_families(
            self.settings_inputs
                .monospace_font
                .read(cx)
                .value()
                .as_ref(),
        );
        let lyrics = parse_font_families(self.settings_inputs.lyric_font.read(cx).value().as_ref());
        let ui = if ui.is_empty() {
            default_ui_font_families()
        } else {
            ui
        };
        let monospace = if monospace.is_empty() {
            default_monospace_font_families()
        } else {
            monospace
        };
        let lyrics = if lyrics.is_empty() {
            default_lyric_font_families()
        } else {
            lyrics
        };
        self.settings_inputs
            .set_fonts(&ui, &monospace, &lyrics, window, cx);
        cx.emit(SettingsAction::Fonts {
            ui,
            monospace,
            lyrics,
        });
    }

    fn reset_font_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 重置按钮只回填输入框；应用/保存仍由用户点击应用字体触发。
        let ui = default_ui_font_families();
        let monospace = default_monospace_font_families();
        let lyrics = default_lyric_font_families();
        self.settings_inputs
            .set_fonts(&ui, &monospace, &lyrics, window, cx);
    }
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.render_count += 1;
        }
        let narrow = window.viewport_size().width < px(900.);
        let theme = cx.theme().clone();
        let settings = &self.preferences.read(cx).settings;
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
        let number_setting = |label: &'static str,
                              detail: &'static str,
                              input: &Entity<InputState>,
                              suffix: &'static str,
                              divided: bool| {
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_6()
                .when(divided, |this| {
                    this.pt_4().border_t_1().border_color(theme.border)
                })
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().font_medium().child(label))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(detail),
                        ),
                )
                .child(
                    NumberInput::new(input)
                        .flex_none()
                        .w(px(140.))
                        .h(px(40.))
                        .suffix(
                            div()
                                .pr_2()
                                .text_sm()
                                .text_color(theme.secondary_foreground)
                                .child(suffix),
                        ),
                )
        };
        let theme_picker = |prefix: &'static str, selected: ColorTheme, light: bool| {
            let view = cx.entity().clone();
            Button::new(format!("settings-{prefix}-{}", selected.id()))
                .label(selected.label())
                .outline()
                .w_full()
                .h(px(38.))
                .dropdown_menu(move |menu, window, _| {
                    ColorTheme::ALL.into_iter().fold(menu, |menu, color_theme| {
                        menu.item(
                            PopupMenuItem::new(color_theme.label())
                                .checked(selected == color_theme)
                                .on_click(window.listener_for(&view, move |_, _, _, cx| {
                                    cx.emit(SettingsAction::ColorTheme {
                                        theme: color_theme,
                                        light,
                                    })
                                })),
                        )
                    })
                })
        };
        let light_theme_picker = theme_picker("light", settings.light_color_theme, true);
        let dark_theme_picker = theme_picker("dark", settings.dark_color_theme, false);
        let selected_theme_mode = settings.color_theme_mode;
        let theme_mode_buttons = ColorThemeMode::ALL
            .into_iter()
            .map(|mode| {
                Button::new(mode.id())
                    .label(mode.label())
                    .ghost()
                    .flex_1()
                    .h(px(38.))
                    .selected(selected_theme_mode == mode)
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(SettingsAction::ThemeMode(mode))),
                    )
            })
            .collect::<Vec<_>>();
        let selected_tray_icon_style = settings.tray_icon_style;
        let tray_icon_buttons = TrayIconStyle::ALL
            .into_iter()
            .map(|style| {
                Button::new(style.id())
                    .label(style.label())
                    .ghost()
                    .flex_1()
                    .h(px(38.))
                    .selected(selected_tray_icon_style == style)
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(SettingsAction::TrayIcon(style))),
                    )
            })
            .collect::<Vec<_>>();
        let check_updates_on_startup = settings.check_updates_on_startup;
        let selected_window_decoration = settings.window_decoration;
        let window_decoration_buttons = WindowDecoration::ALL
            .into_iter()
            .map(|decoration| {
                Button::new(decoration.id())
                    .label(decoration.label())
                    .ghost()
                    .flex_1()
                    .h(px(38.))
                    .selected(selected_window_decoration == decoration)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SettingsAction::WindowDecoration(decoration))
                    }))
            })
            .collect::<Vec<_>>();
        let preferred_quality = settings.playback_quality;
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
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(SettingsAction::Quality(quality))
                            }))
                    }))
            })
            .collect::<Vec<_>>();
        let selected_progress_frame_rate = settings.progress_frame_rate;
        let progress_frame_rate_buttons = FrameRate::ALL
            .into_iter()
            .map(|frame_rate| {
                Button::new(format!("progress-{}", frame_rate.id()))
                    .label(frame_rate.label())
                    .ghost()
                    .flex_1()
                    .h(px(36.))
                    .selected(selected_progress_frame_rate == frame_rate)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SettingsAction::ProgressRate(frame_rate))
                    }))
            })
            .collect::<Vec<_>>();
        let selected_highlight_frame_rate = self
            .preferences
            .read(cx)
            .settings
            .lyric_highlight_frame_rate;
        let highlight_frame_rate_buttons = FrameRate::ALL
            .into_iter()
            .map(|frame_rate| {
                Button::new(format!("lyrics-highlight-{}", frame_rate.id()))
                    .label(frame_rate.label())
                    .ghost()
                    .flex_1()
                    .h(px(36.))
                    .selected(selected_highlight_frame_rate == frame_rate)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SettingsAction::HighlightRate(frame_rate))
                    }))
            })
            .collect::<Vec<_>>();
        let selected_scroll_frame_rate = settings.lyric_scroll_frame_rate;
        let scroll_frame_rate_buttons = FrameRate::ALL
            .into_iter()
            .map(|frame_rate| {
                Button::new(format!("lyrics-scroll-{}", frame_rate.id()))
                    .label(frame_rate.label())
                    .ghost()
                    .flex_1()
                    .h(px(36.))
                    .selected(selected_scroll_frame_rate == frame_rate)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SettingsAction::ScrollRate(frame_rate))
                    }))
            })
            .collect::<Vec<_>>();
        div()
            .relative()
            .size_full()
            .min_h_0()
            .child(
                div()
                    .id("settings-scroll")
                    .size_full()
                    .track_scroll(&self.settings_scroll_handle)
                    .overflow_y_scroll()
                    .child(h_flex().w_full().items_start().justify_center().child(
                        v_flex()
                        .w_full()
                        .max_w(px(760.))
                        .gap_6()
                        .px(page_horizontal_padding(narrow))
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
                                        .child(div().font_medium().child("主题偏好"))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child("选择固定的亮色、暗色，或跟随系统外观"),
                                        )
                                        .child(h_flex().w_full().gap_1().children(theme_mode_buttons))
                                        .child(div().font_medium().child("主题配色"))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child("分别设置亮色和暗色模式使用的主题"),
                                        )
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_2()
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .gap_1()
                                                        .pl_4()
                                                        .child(div().child("亮色"))
                                                        .child(light_theme_picker),
                                                )
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .gap_1()
                                                        .pl_4()
                                                        .child(div().child("暗色"))
                                                        .child(dark_theme_picker),
                                                ),
                                        )
                                )
                                .child(
                                    v_flex()
                                        .gap_2()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(div().font_medium().child("托盘图标"))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child("选择系统托盘图标的显示样式"),
                                        )
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_1()
                                                .children(tray_icon_buttons),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .gap_2()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(div().font_medium().child("更新"))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child("启动时在后台检查新版本，不会影响正常使用"),
                                        )
                                        .child(
                                            Button::new("check-updates-on-startup")
                                                .label(if check_updates_on_startup {
                                                    "启动时检查更新：开启"
                                                } else {
                                                    "启动时检查更新：关闭"
                                                })
                                                .ghost()
                                                .selected(check_updates_on_startup)
                                                .w_full()
                                                .h(px(38.))
                                                .on_click(cx.listener(move |_, _, _, cx| {
                                                    cx.emit(SettingsAction::CheckOnStartup(!check_updates_on_startup))
                                                })),
                                        )
                                        .child(
                                            Button::new("check-updates-now")
                                            .label(if self.update_check_in_progress {
                                                "正在检查更新…"
                                            } else {
                                                "立即检查更新"
                                            })
                                            .outline()
                                            .w_full()
                                            .h(px(38.))
                                            .disabled(self.update_check_in_progress)
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(SettingsAction::CheckUpdates)
                                            })),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .gap_2()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(div().font_medium().child("窗口装饰"))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child("SSD 使用系统绘制标题栏；CSD 由 Lyrune 绘制窗口按钮和边框。切换后立即生效"),
                                        )
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_1()
                                                .children(window_decoration_buttons),
                                        ),
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
                                            &self.settings_inputs.ui_font,
                                        ))
                                        .child(font_field(
                                            "等宽字体",
                                            "应用于时间等定宽文本",
                                            &self.settings_inputs.monospace_font,
                                        ))
                                        .child(font_field(
                                            "歌词字体",
                                            "应用于主歌词、翻译与注音",
                                            &self.settings_inputs.lyric_font,
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
                                        .gap_4()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(div().font_medium().child("缓存与历史"))
                                        .child(number_setting(
                                            "歌曲缓存上限",
                                            "超过上限时自动清理最久未播放的歌曲",
                                            &self.settings_inputs.audio_cache_limit,
                                            "GB",
                                            false,
                                        ))
                                        .child(number_setting(
                                            "图片内存缓存",
                                            "图片最大缓存数，缩小该数值可以减少内存占用，但同屏显示图片超过该数值会出现渲染问题",
                                            &self.settings_inputs.image_cache_capacity,
                                            "张",
                                            true,
                                        ))
                                        .child(number_setting(
                                            "页面历史上限",
                                            "最多保留的页面数量（包含当前页），缩小该数值会立即清理最早的历史页面",
                                            &self.settings_inputs.navigation_history_limit,
                                            "页",
                                            true,
                                        )),
                                )
                                .child(
                                    v_flex()
                                        .gap_4()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(div().font_medium().child("歌词页样式"))
                                        .child(number_setting(
                                            "歌词字号缩放",
                                            "缩放主歌词、翻译与注音的文字大小，行高随之等比例调整",
                                            &self.settings_inputs.lyric_font_scale,
                                            "%",
                                            false,
                                        ))
                                        .child(number_setting(
                                            "歌词行间距",
                                            "调整相邻歌词之间的留白",
                                            &self.settings_inputs.lyric_line_spacing,
                                            "%",
                                            true,
                                        ))
                                        .child(number_setting(
                                            "背景彩度上限",
                                            "自动混入主题背景色以降低鲜艳程度，实际下限受主题背景彩度限制",
                                            &self.settings_inputs.lyric_background_chroma,
                                            "",
                                            true,
                                        ))
                                        .child(number_setting(
                                            "最低对比度",
                                            "仅在对比度不足时调整背景明度，实际可达范围受主题颜色限制",
                                            &self.settings_inputs.lyric_background_contrast,
                                            "∶1",
                                            true,
                                        )),
                                )
                                .child(
                                    v_flex()
                                        .gap_2()
                                        .pt_4()
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .child(div().font_medium().child("动画帧率"))
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
                                                .child("歌词逐字高亮与横向跟随"),
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
                                                .child("歌词平滑滚动"),
                                        )
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_1()
                                                .children(scroll_frame_rate_buttons),
                                        )
                                        .child(
                                            div()
                                                .pt_1()
                                                .text_xs()
                                                .text_color(theme.secondary_foreground)
                                                .child("播放进度条"),
                                        )
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_1()
                                                .children(progress_frame_rate_buttons),
                                        ),
                                ),
                        ),
                    )),
            )
            .vertical_scrollbar(&self.settings_scroll_handle)
            .into_any_element()
    }
}

impl LyruneView {
    pub(super) fn handle_settings_action(
        &mut self,
        action: &SettingsAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            SettingsAction::ColorTheme { theme, light } => {
                self.set_color_theme(*theme, *light, window, cx)
            }
            SettingsAction::ThemeMode(mode) => self.set_color_theme_mode(*mode, window, cx),
            SettingsAction::TrayIcon(style) => self.set_tray_icon_style(*style, cx),
            SettingsAction::CheckOnStartup(enabled) => {
                self.set_check_updates_on_startup(*enabled, cx)
            }
            SettingsAction::WindowDecoration(decoration) => {
                self.set_window_decoration(*decoration, window, cx)
            }
            SettingsAction::Quality(quality) => self.set_preferred_playback_quality(*quality, cx),
            SettingsAction::ProgressRate(rate) => self.set_progress_frame_rate(*rate, window, cx),
            SettingsAction::HighlightRate(rate) => {
                self.set_lyric_highlight_frame_rate(*rate, window, cx)
            }
            SettingsAction::ScrollRate(rate) => self.set_lyric_scroll_frame_rate(*rate, window, cx),
            SettingsAction::AudioCacheLimit(value) => self.set_audio_cache_limit(*value, cx),
            SettingsAction::ImageCacheCapacity(value) => self.set_image_cache_capacity(*value, cx),
            SettingsAction::HistoryLimit(value) => self.set_navigation_history_limit(*value, cx),
            SettingsAction::Scale { lyric, spacing } => {
                self.set_scale_settings(*lyric, *spacing, cx)
            }
            SettingsAction::LyricBackground { chroma, contrast } => {
                let settings = &self.preferences.read(cx).settings;
                if settings.lyric_background_chroma == *chroma
                    && settings.lyric_background_contrast == *contrast
                {
                    return;
                }
                self.preferences.update(cx, |preferences, cx| {
                    preferences.settings.lyric_background_chroma = *chroma;
                    preferences.settings.lyric_background_contrast = *contrast;
                    cx.notify();
                });
                self.preferences.read(cx).save();
            }
            SettingsAction::Fonts {
                ui,
                monospace,
                lyrics,
            } => self.set_font_settings(ui.clone(), monospace.clone(), lyrics.clone(), window, cx),
            SettingsAction::CheckUpdates => self.check_updates(true, cx),
        }
    }

    fn set_color_theme(
        &mut self,
        color_theme: ColorTheme,
        light: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.preferences.update(cx, |preferences, cx| {
            let selected = if light {
                &mut preferences.settings.light_color_theme
            } else {
                &mut preferences.settings.dark_color_theme
            };
            if *selected == color_theme {
                return false;
            }
            *selected = color_theme;
            cx.notify();
            true
        });
        if !changed {
            return;
        }
        self.preferences
            .update(cx, |preferences, cx| preferences.apply_theme(window, cx));
        self.preferences.read(cx).save();
    }

    fn set_color_theme_mode(
        &mut self,
        mode: ColorThemeMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preferences.read(cx).settings.color_theme_mode == mode {
            return;
        }
        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.color_theme_mode = mode;
            cx.notify();
        });
        self.preferences
            .update(cx, |preferences, cx| preferences.apply_theme(window, cx));
        self.preferences.read(cx).save();
    }

    fn set_tray_icon_style(&mut self, style: TrayIconStyle, cx: &mut Context<Self>) {
        if self.preferences.read(cx).settings.tray_icon_style == style {
            return;
        }
        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.tray_icon_style = style;
            cx.notify();
        });
        crate::set_tray_icon_style(style.resolve(cx.window_appearance()), cx);
        self.preferences.read(cx).save();
        cx.notify();
    }

    fn set_check_updates_on_startup(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.preferences.read(cx).settings.check_updates_on_startup == enabled {
            return;
        }
        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.check_updates_on_startup = enabled;
            cx.notify();
        });
        self.preferences.read(cx).save();
        cx.notify();
    }

    fn set_window_decoration(
        &mut self,
        decoration: WindowDecoration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preferences.read(cx).settings.window_decoration == decoration {
            return;
        }
        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.window_decoration = decoration;
            cx.notify();
        });
        window.request_decorations(match decoration {
            WindowDecoration::Ssd => WindowDecorations::Server,
            WindowDecoration::Csd => WindowDecorations::Client,
        });
        self.preferences.read(cx).save();
        window.set_background_appearance(match window.window_decorations() {
            Decorations::Client { .. } => WindowBackgroundAppearance::Transparent,
            Decorations::Server => WindowBackgroundAppearance::Opaque,
        });
        window.refresh();
        cx.notify();
    }

    fn set_preferred_playback_quality(&mut self, quality: Quality, cx: &mut Context<Self>) {
        if self.preferences.read(cx).settings.playback_quality == quality {
            return;
        }
        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.playback_quality = quality;
            cx.notify();
        });
        self.preferences.read(cx).save();
        cx.notify();
    }

    fn set_audio_cache_limit(&mut self, value: u64, cx: &mut Context<Self>) {
        let current = self.preferences.read(cx).settings.audio_cache_limit_gb;
        if value == current {
            return;
        }

        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.audio_cache_limit_gb = value;
            cx.notify();
        });
        if let Some(cache) = &self.playback.read(cx).audio_cache {
            cache.set_max_size_bytes(audio_cache_limit_bytes(value));
        }
        self.preferences.read(cx).save();
        self.playback
            .update(cx, |playback, _| playback.start_audio_cache_maintenance());
        cx.notify();
    }

    fn set_image_cache_capacity(&mut self, value: usize, cx: &mut Context<Self>) {
        let current = self.preferences.read(cx).settings.image_cache_capacity;
        if value == current {
            return;
        }

        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.image_cache_capacity = value;
            cx.notify();
        });
        self.image_cache
            .update(cx, |cache, _| cache.set_capacity(value));
        self.preferences.read(cx).save();
        cx.notify();
    }

    fn set_navigation_history_limit(&mut self, value: usize, cx: &mut Context<Self>) {
        let current = self.preferences.read(cx).settings.navigation_history_limit;
        if value == current {
            return;
        }

        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.navigation_history_limit = value;
            cx.notify();
        });
        self.browser.update(cx, |browser, cx| {
            browser.navigation_history.set_limit(value);
            browser.page_resource_cache.prune();
            cx.notify();
        });
        self.preferences.read(cx).save();
        cx.notify();
    }

    fn set_scale_settings(&mut self, lyric: u32, spacing: u32, cx: &mut Context<Self>) {
        if lyric == self.preferences.read(cx).settings.lyric_font_scale
            && spacing == self.preferences.read(cx).settings.lyric_line_spacing
        {
            return;
        }

        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.lyric_font_scale = lyric;
            cx.notify();
        });
        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.lyric_line_spacing = spacing;
            cx.notify();
        });
        self.preferences.read(cx).save();
        cx.notify();
    }

    fn set_font_settings(
        &mut self,
        ui: Vec<String>,
        monospace: Vec<String>,
        lyrics: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.ui_font_families = ui;
            preferences.settings.monospace_font_families = monospace;
            preferences.settings.lyric_font_families = lyrics;
            preferences.fonts = design::resolve_fonts(
                &preferences.settings.ui_font_families,
                &preferences.settings.monospace_font_families,
                &preferences.settings.lyric_font_families,
                cx,
            );
            cx.notify();
        });
        self.preferences
            .update(cx, |preferences, cx| preferences.apply_theme(window, cx));
        self.lyrics_view
            .update(cx, |view, cx| view.reset_layout(cx));
        self.preferences.read(cx).save();
        cx.notify();
    }

    fn set_progress_frame_rate(
        &mut self,
        frame_rate: FrameRate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preferences.read(cx).settings.progress_frame_rate == frame_rate {
            return;
        }
        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.progress_frame_rate = frame_rate;
            cx.notify();
        });
        window.set_inactive_frame_interval(self.inactive_window_frame_interval(cx));
        self.preferences.read(cx).save();
        cx.notify();
    }

    fn set_lyric_highlight_frame_rate(
        &mut self,
        frame_rate: FrameRate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .preferences
            .read(cx)
            .settings
            .lyric_highlight_frame_rate
            == frame_rate
        {
            return;
        }
        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.lyric_highlight_frame_rate = frame_rate;
            cx.notify();
        });
        self.lyrics_view
            .update(cx, |view, _| view.reset_lyric_animation_frames());
        window.set_inactive_frame_interval(self.inactive_window_frame_interval(cx));
        self.preferences.read(cx).save();
        cx.notify();
    }

    fn set_lyric_scroll_frame_rate(
        &mut self,
        frame_rate: FrameRate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preferences.read(cx).settings.lyric_scroll_frame_rate == frame_rate {
            return;
        }
        self.preferences.update(cx, |preferences, cx| {
            preferences.settings.lyric_scroll_frame_rate = frame_rate;
            cx.notify();
        });
        self.lyrics_view
            .update(cx, |view, _| view.reset_lyric_animation_frames());
        window.set_inactive_frame_interval(self.inactive_window_frame_interval(cx));
        self.preferences.read(cx).save();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, StyleRefinement,
        Styled as _, TestAppContext, Window, div, font, px,
    };
    use gpui_component::{input::InputEvent, v_flex};

    use super::{Preferences, SettingsAction, SettingsPage};
    use crate::design::{AppFonts, ColorTheme};
    use crate::settings::AppSettings;

    struct TestSettingsPage {
        page: Entity<SettingsPage>,
    }

    impl Render for TestSettingsPage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .size_full()
                .child(div().h(px(72.)).flex_shrink_0())
                .child(
                    self.page
                        .clone()
                        .cached(StyleRefinement::default().flex_1().w_full().min_h_0()),
                )
                .child(div().h(px(112.)).flex_shrink_0())
        }
    }

    #[gpui::test]
    fn test_number_settings_commit_on_step(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let preferences = cx.new(|_| Preferences {
            settings: AppSettings::default(),
            fonts: AppFonts {
                ui: font(".SystemUIFont"),
                monospace: font(".SystemUIFont"),
                lyrics: font(".SystemUIFont"),
            },
            color_theme: ColorTheme::default(),
            writer: None,
        });
        let (page, cx) = cx.add_window_view(|window, cx| {
            let mut page = SettingsPage::new(preferences, window, cx);
            page.attach_window(window, cx);
            page
        });
        cx.simulate_resize(gpui::size(px(1080.), px(2400.)));
        let actions = Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = actions.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&page, move |_, event: &SettingsAction, _| {
                let value = match event {
                    SettingsAction::AudioCacheLimit(value) => *value,
                    SettingsAction::ImageCacheCapacity(value)
                    | SettingsAction::HistoryLimit(value) => *value as u64,
                    SettingsAction::Scale { lyric, spacing } => u64::from((*lyric).max(*spacing)),
                    _ => return,
                };
                received.borrow_mut().push(value);
            })
        });
        let inputs = page.read_with(cx, |page, _| {
            vec![
                (page.settings_inputs.audio_cache_limit.clone(), 1),
                (page.settings_inputs.image_cache_capacity.clone(), 1),
                (page.settings_inputs.navigation_history_limit.clone(), 1),
                (page.settings_inputs.lyric_font_scale.clone(), 10),
                (page.settings_inputs.lyric_line_spacing.clone(), 10),
            ]
        });
        for (input, step) in inputs {
            let initial = if step == 10 { 100 } else { 90 };
            cx.update(|window, cx| {
                input.update(cx, |input, cx| {
                    input.set_value(initial.to_string(), window, cx)
                })
            });
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            actions.borrow_mut().clear();
            let bounds = input.read_with(cx, |input, _| input.input_bounds());
            let increment = gpui::point(bounds.origin.x + px(92.), bounds.center().y);
            cx.simulate_click(increment, gpui::Modifiers::none());
            cx.run_until_parked();
            assert_eq!(
                input.read_with(cx, |input, _| input.value().to_string()),
                (initial + step).to_string()
            );
            assert_eq!(*actions.borrow(), vec![initial + step]);
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.simulate_click(
                gpui::point(bounds.origin.x - px(16.), bounds.center().y),
                gpui::Modifiers::none(),
            );
            cx.run_until_parked();
            assert_eq!(*actions.borrow(), vec![initial + step, initial]);
        }
        actions.borrow_mut().clear();
        cx.simulate_keystrokes("ctrl-a");
        cx.simulate_input("150");
        assert!(actions.borrow().is_empty());
        cx.simulate_keystrokes("enter");
        assert_eq!(*actions.borrow(), vec![150]);
    }

    #[gpui::test]
    fn test_lyric_background_settings(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let preferences = cx.new(|_| Preferences {
            settings: AppSettings::default(),
            fonts: AppFonts {
                ui: font(".SystemUIFont"),
                monospace: font(".SystemUIFont"),
                lyrics: font(".SystemUIFont"),
            },
            color_theme: ColorTheme::default(),
            writer: None,
        });
        let (page, cx) = cx.add_window_view(|window, cx| {
            let mut page = SettingsPage::new(preferences, window, cx);
            page.attach_window(window, cx);
            page
        });
        cx.simulate_resize(gpui::size(px(1080.), px(3000.)));
        cx.update(|window, _| window.activate_window());
        let actions = Rc::new(std::cell::RefCell::new(Vec::new()));
        let received = actions.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&page, move |_, event: &SettingsAction, _| {
                if let SettingsAction::LyricBackground { chroma, contrast } = event {
                    received.borrow_mut().push((*chroma, *contrast));
                }
            })
        });
        let inputs = page.read_with(cx, |page, _| {
            [
                (
                    page.settings_inputs.lyric_background_chroma.clone(),
                    (0.09, 4.5),
                ),
                (
                    page.settings_inputs.lyric_background_contrast.clone(),
                    (0.09, 4.6),
                ),
            ]
        });
        for (input, expected) in inputs {
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let bounds = input.read_with(cx, |input, _| input.input_bounds());
            cx.simulate_click(
                gpui::point(bounds.origin.x + px(92.), bounds.center().y),
                gpui::Modifiers::none(),
            );
            cx.run_until_parked();
            assert_eq!(actions.borrow().last(), Some(&expected));
        }
        actions.borrow_mut().clear();
        cx.simulate_keystrokes("ctrl-a");
        cx.simulate_input("3.2");
        assert!(actions.borrow().is_empty());
        cx.simulate_keystrokes("enter");
        assert_eq!(actions.borrow().last(), Some(&(0.09, 3.2)));
        cx.simulate_keystrokes("ctrl-a");
        cx.simulate_input("6.5");
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let chroma = page.read_with(cx, |page, cx| {
            assert_eq!(
                page.settings_inputs
                    .lyric_background_contrast
                    .read(cx)
                    .value(),
                "6.5"
            );
            page.settings_inputs.lyric_background_chroma.clone()
        });
        cx.update(|window, cx| chroma.update(cx, |input, cx| input.focus(window, cx)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
        assert_eq!(actions.borrow().last(), Some(&(0.09, 6.5)));
    }

    #[gpui::test]
    fn test_settings_cache_and_window_binding(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let preferences = cx.new(|_| Preferences {
            settings: AppSettings::default(),
            fonts: AppFonts {
                ui: font(".SystemUIFont"),
                monospace: font(".SystemUIFont"),
                lyrics: font(".SystemUIFont"),
            },
            color_theme: ColorTheme::default(),
            writer: None,
        });
        let (host, cx) = cx.add_window_view(|window, cx| TestSettingsPage {
            page: cx.new(|cx| SettingsPage::new(preferences.clone(), window, cx)),
        });
        let page = host.read_with(cx, |host, _| host.page.clone());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let before = page.read_with(cx, |page, _| page.render_count);
        let expected_height = cx.update(|window, _| window.viewport_size().height - px(184.));
        page.read_with(cx, |page, _| {
            assert_eq!(
                page.settings_scroll_handle.bounds().size.height,
                expected_height
            )
        });
        for _ in 0..3 {
            host.update(cx, |_, cx| cx.notify());
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        assert_eq!(page.read_with(cx, |page, _| page.render_count), before);
        preferences.update(cx, |preferences, cx| {
            preferences.settings.progress_frame_rate = crate::settings::FrameRate::Fps90;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(page.read_with(cx, |page, _| page.render_count) > before);

        page.update(cx, |page, cx| {
            page.settings_scroll_handle
                .set_offset(gpui::point(px(0.), px(-650.)));
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let scrolled = page.read_with(cx, |page, _| page.render_count);
        for _ in 0..3 {
            host.update(cx, |_, cx| cx.notify());
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        assert_eq!(page.read_with(cx, |page, _| page.render_count), scrolled);

        let events = Rc::new(Cell::new(0));
        let received = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&page, move |_, event: &SettingsAction, _| {
                if matches!(event, SettingsAction::AudioCacheLimit(12)) {
                    received.set(received.get() + 1);
                }
            })
        });
        cx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.attach_window(window, cx);
                page.attach_window(window, cx);
                page.settings_inputs
                    .audio_cache_limit
                    .update(cx, |input, cx| {
                        input.set_value("12", window, cx);
                        cx.emit(InputEvent::Blur);
                    });
            })
        });
        cx.run_until_parked();
        assert_eq!(events.get(), 1);
        // 页面只发出提交意图，草稿输入不会自行覆盖共享配置。
        assert_ne!(
            preferences.read_with(cx, |preferences, _| preferences
                .settings
                .audio_cache_limit_gb),
            12
        );
    }
}
