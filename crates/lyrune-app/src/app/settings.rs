//! 设置页面及偏好的即时生效；序列化格式、默认值和磁盘保存由 crate::settings 管理。

use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState, MaskPattern, NumberInput},
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    v_flex,
};
use qqmusic_api::integration::Quality;

use super::{LyricLayoutCache, LyruneView, page_horizontal_padding};
use crate::cache::audio_cache_limit_bytes;
use crate::design::{self, ColorTheme};
use crate::settings::{
    AppSettings, ColorThemeMode, LyricFrameRate, MAX_IMAGE_CACHE_CAPACITY, MAX_LYRIC_SCALE_PERCENT,
    MAX_NAVIGATION_HISTORY_LIMIT, MIN_SCALE_PERCENT, TrayIconStyle, WindowDecoration,
    default_lyric_font_families, default_monospace_font_families, default_ui_font_families,
    parse_font_families,
};

pub(super) struct SettingsInputs {
    pub(super) ui_font: Entity<InputState>,
    pub(super) monospace_font: Entity<InputState>,
    pub(super) lyric_font: Entity<InputState>,
    pub(super) audio_cache_limit: Entity<InputState>,
    pub(super) image_cache_capacity: Entity<InputState>,
    pub(super) navigation_history_limit: Entity<InputState>,
    pub(super) lyric_font_scale: Entity<InputState>,
    pub(super) lyric_line_spacing: Entity<InputState>,
}

impl SettingsInputs {
    pub(super) fn new(
        settings: &AppSettings,
        window: &mut Window,
        cx: &mut Context<LyruneView>,
    ) -> Self {
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
        let image_cache_capacity_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.image_cache_capacity.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: None,
                })
                .validate(|value, _| value.parse::<usize>().is_ok_and(|value| value > 0))
                .min(1.)
                .step(1.)
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
                .step(1.)
        });
        let scale_input = |value: u32, window: &mut Window, cx: &mut Context<LyruneView>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value.to_string())
                    .mask_pattern(MaskPattern::Number {
                        separator: None,
                        fraction: None,
                    })
                    .validate(|value, _| value.parse::<u32>().is_ok())
                    .min(MIN_SCALE_PERCENT as f64)
                    .step(10.)
            })
        };
        let lyric_font_scale_input = scale_input(settings.lyric_font_scale, window, cx);
        let lyric_line_spacing_input = scale_input(settings.lyric_line_spacing, window, cx);
        Self {
            ui_font: ui_font_input,
            monospace_font: monospace_font_input,
            lyric_font: lyric_font_input,
            audio_cache_limit: audio_cache_limit_input,
            image_cache_capacity: image_cache_capacity_input,
            navigation_history_limit: navigation_history_limit_input,
            lyric_font_scale: lyric_font_scale_input,
            lyric_line_spacing: lyric_line_spacing_input,
        }
    }
}

impl LyruneView {
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

    fn set_color_theme(
        &mut self,
        color_theme: ColorTheme,
        light: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = if light {
            &mut self.settings.light_color_theme
        } else {
            &mut self.settings.dark_color_theme
        };
        if *selected == color_theme {
            return;
        }
        *selected = color_theme;
        self.apply_theme(window, cx);
        self.persist_settings();
    }

    fn set_color_theme_mode(
        &mut self,
        mode: ColorThemeMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.color_theme_mode == mode {
            return;
        }
        self.settings.color_theme_mode = mode;
        self.apply_theme(window, cx);
        self.persist_settings();
    }

    fn set_tray_icon_style(&mut self, style: TrayIconStyle, cx: &mut Context<Self>) {
        if self.settings.tray_icon_style == style {
            return;
        }
        self.settings.tray_icon_style = style;
        crate::set_tray_icon_style(style.resolve(cx.window_appearance()), cx);
        self.persist_settings();
        cx.notify();
    }

    fn set_check_updates_on_startup(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.settings.check_updates_on_startup == enabled {
            return;
        }
        self.settings.check_updates_on_startup = enabled;
        self.persist_settings();
        cx.notify();
    }

    fn set_window_decoration(
        &mut self,
        decoration: WindowDecoration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.window_decoration == decoration {
            return;
        }
        self.settings.window_decoration = decoration;
        window.request_decorations(match decoration {
            WindowDecoration::Ssd => WindowDecorations::Server,
            WindowDecoration::Csd => WindowDecorations::Client,
        });
        self.persist_settings();
        window.set_background_appearance(match window.window_decorations() {
            Decorations::Client { .. } => WindowBackgroundAppearance::Transparent,
            Decorations::Server => WindowBackgroundAppearance::Opaque,
        });
        window.refresh();
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

    pub(super) fn apply_audio_cache_limit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.settings.audio_cache_limit_gb;
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

    pub(super) fn apply_image_cache_capacity(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.settings.image_cache_capacity;
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
        if value == current {
            return;
        }

        self.settings.image_cache_capacity = value;
        self.image_cache
            .update(cx, |cache, cx| cache.set_capacity(value, window, cx));
        self.persist_settings();
        cx.notify();
    }

    pub(super) fn apply_navigation_history_limit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.settings.navigation_history_limit;
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
        if value == current {
            return;
        }

        self.settings.navigation_history_limit = value;
        self.navigation_history.set_limit(value);
        self.page_resource_cache.prune();
        self.persist_settings();
        cx.notify();
    }

    pub(super) fn apply_scale_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let read = |input: &Entity<InputState>, current: u32, max: u32, cx: &mut Context<Self>| {
            input
                .read(cx)
                .value()
                .parse::<u32>()
                .map_or(current, |value| value.clamp(MIN_SCALE_PERCENT, max))
        };
        let lyric = read(
            &self.settings_inputs.lyric_font_scale,
            self.settings.lyric_font_scale,
            MAX_LYRIC_SCALE_PERCENT,
            cx,
        );
        let spacing = read(
            &self.settings_inputs.lyric_line_spacing,
            self.settings.lyric_line_spacing,
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
        if lyric == self.settings.lyric_font_scale && spacing == self.settings.lyric_line_spacing {
            return;
        }

        self.settings.lyric_font_scale = lyric;
        self.settings.lyric_line_spacing = spacing;
        self.persist_settings();
        cx.notify();
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
        self.settings_inputs
            .ui_font
            .update(cx, |input, cx| input.set_value(ui.join(", "), window, cx));
        self.settings_inputs.monospace_font.update(cx, |input, cx| {
            input.set_value(monospace.join(", "), window, cx)
        });
        self.settings_inputs.lyric_font.update(cx, |input, cx| {
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
        self.settings_inputs
            .ui_font
            .update(cx, |input, cx| input.set_value(ui.join(", "), window, cx));
        self.settings_inputs.monospace_font.update(cx, |input, cx| {
            input.set_value(monospace.join(", "), window, cx)
        });
        self.settings_inputs.lyric_font.update(cx, |input, cx| {
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
        self.apply_theme(window, cx);
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

    pub(super) fn render_settings_page(
        &mut self,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                                .on_click(window.listener_for(
                                    &view,
                                    move |this, _, window, cx| {
                                        this.set_color_theme(color_theme, light, window, cx)
                                    },
                                )),
                        )
                    })
                })
        };
        let light_theme_picker = theme_picker("light", self.settings.light_color_theme, true);
        let dark_theme_picker = theme_picker("dark", self.settings.dark_color_theme, false);
        let selected_theme_mode = self.settings.color_theme_mode;
        let theme_mode_buttons = ColorThemeMode::ALL
            .into_iter()
            .map(|mode| {
                Button::new(mode.id())
                    .label(mode.label())
                    .ghost()
                    .flex_1()
                    .h(px(38.))
                    .selected(selected_theme_mode == mode)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_color_theme_mode(mode, window, cx)
                    }))
            })
            .collect::<Vec<_>>();
        let selected_tray_icon_style = self.settings.tray_icon_style;
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
                        cx.listener(move |this, _, _, cx| this.set_tray_icon_style(style, cx)),
                    )
            })
            .collect::<Vec<_>>();
        let check_updates_on_startup = self.settings.check_updates_on_startup;
        let selected_window_decoration = self.settings.window_decoration;
        let window_decoration_buttons = WindowDecoration::ALL
            .into_iter()
            .map(|decoration| {
                Button::new(decoration.id())
                    .label(decoration.label())
                    .ghost()
                    .flex_1()
                    .h(px(38.))
                    .selected(selected_window_decoration == decoration)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_window_decoration(decoration, window, cx)
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
            .relative()
            .flex_1()
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
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.set_check_updates_on_startup(
                                                        !check_updates_on_startup,
                                                        cx,
                                                    )
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
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.check_updates(true, cx)
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
                                        .child(div().font_medium().child("歌词字号与间距"))
                                        .child(number_setting(
                                            "歌词字号缩放",
                                            "缩放主歌词、翻译与注音的文字大小，行高随之等比例调整",
                                            &self.settings_inputs.lyric_font_scale,
                                            "%",
                                            false,
                                        ))
                                        .child(number_setting(
                                            "歌词行间距",
                                            "调整歌词页相邻两行之间的距离",
                                            &self.settings_inputs.lyric_line_spacing,
                                            "%",
                                            true,
                                        )),
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
                    )),
            )
            .vertical_scrollbar(&self.settings_scroll_handle)
            .into_any_element()
    }
}
