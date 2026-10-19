//! 歌曲信息、播放按钮与音量控件；进度条是同级视图，逐帧刷新不重建这里。

use std::sync::Arc;

use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use lyrune_api::{Quality, SearchAlbum, SearchArtist, Track};

use super::progress_view::{PlaybackProgressView, ProgressInput};
use super::{LyruneView, PLAYER_BAR_HEIGHT};
use crate::design::ColorTheme;
use crate::http::cached_image_source;
use crate::icons::{MediaIcon, media_icon, media_icon_hsla};
use crate::player::AudioPlayer;
use crate::settings::RepeatMode;

#[derive(PartialEq)]
pub(super) struct PlayerInput {
    pub(super) liked: Option<bool>,
    pub(super) active_quality: Quality,
    pub(super) available_qualities: Vec<Quality>,
    pub(super) loading: bool,
    pub(super) show_pause: bool,
    pub(super) queue_empty: bool,
    pub(super) browsed_tracks_empty: bool,
    pub(super) shuffle: bool,
    pub(super) repeat_mode: RepeatMode,
    pub(super) volume: f32,
    pub(super) color_theme: ColorTheme,
    pub(super) expanded: bool,
    pub(super) quality_menu_open: bool,
}

pub(super) enum PlayerAction {
    ToggleLyrics,
    Album(SearchAlbum),
    Artist(SearchArtist),
    ToggleLike,
    Shuffle,
    Previous,
    TogglePlayback,
    Next,
    Repeat,
    ToggleMute,
    Quality(Quality),
    ToggleQuality,
    Volume { value: f32, persist: bool },
}

pub(super) struct PlayerControlsView {
    track: Option<Arc<Track>>,
    input: PlayerInput,
    volume_slider: Entity<SliderState>,
    _subscription: Subscription,
    #[cfg(test)]
    render_count: usize,
}

impl EventEmitter<PlayerAction> for PlayerControlsView {}

fn volume_slider_state(value: f32) -> SliderState {
    SliderState::new()
        .min(0.)
        .max(1.)
        .step(0.01)
        .default_value(value)
}

impl PlayerControlsView {
    pub(super) fn new(
        volume: f32,
        color_theme: ColorTheme,
        quality: Quality,
        cx: &mut Context<Self>,
    ) -> Self {
        let volume_slider = cx.new(|_| volume_slider_state(volume));
        let subscription = cx.subscribe(&volume_slider, |_, _, event, cx| {
            let (value, persist) = match event {
                SliderEvent::Change(value) => (value.end(), false),
                SliderEvent::Release(value) => (value.end(), true),
            };
            cx.emit(PlayerAction::Volume { value, persist });
        });
        Self {
            track: None,
            input: PlayerInput {
                liked: None,
                active_quality: quality,
                available_qualities: Vec::new(),
                loading: false,
                show_pause: false,
                queue_empty: true,
                browsed_tracks_empty: true,
                shuffle: false,
                repeat_mode: RepeatMode::Off,
                volume,
                color_theme,
                expanded: false,
                quality_menu_open: false,
            },
            volume_slider,
            _subscription: subscription,
            #[cfg(test)]
            render_count: 0,
        }
    }

    pub(super) fn set_input(
        &mut self,
        track: Option<Arc<Track>>,
        input: PlayerInput,
        cx: &mut Context<Self>,
    ) {
        let same_track = match (&self.track, &track) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        };
        if same_track && self.input == input {
            return;
        }
        self.track = track;
        self.input = input;
        cx.notify();
    }

    pub(super) fn set_volume(&mut self, volume: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.volume_slider
            .update(cx, |slider, cx| slider.set_value(volume, window, cx));
    }

    #[cfg(target_os = "linux")]
    pub(super) fn reset_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.volume_slider.update(cx, |slider, cx| {
            *slider = volume_slider_state(volume);
            cx.notify();
        });
    }

    fn render_quality_selector(&mut self, has_track: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let active_quality = self.input.active_quality;
        let active_label = active_quality.badge_label();
        let options = self
            .input
            .available_qualities
            .iter()
            .rev()
            .copied()
            .map(|quality| {
                Button::new(format!("quality-{}", quality.cache_id()))
                    .label(quality.label())
                    .ghost()
                    .w_full()
                    .h(px(44.))
                    .selected(quality == active_quality)
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(PlayerAction::Quality(quality))),
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
                    .when(self.input.loading, |button| {
                        button
                            .bg(theme.input_background())
                            .border_color(theme.input)
                            .text_color(theme.button_foreground)
                    })
                    .disabled(
                        !has_track
                            || self.input.loading
                            || self.input.available_qualities.is_empty(),
                    )
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PlayerAction::ToggleQuality))),
            )
            .when(self.input.quality_menu_open, |this| {
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
}

impl Render for PlayerControlsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.render_count += 1;
        }
        let compact = window.viewport_size().width < px(1120.);
        let narrow = window.viewport_size().width < px(900.);
        let track = self.track.clone();
        let has_track = track.is_some();
        let quality_selector = self.render_quality_selector(has_track, cx);
        let theme = cx.theme();
        let loading = self.input.loading;
        let show_pause = self.input.show_pause;
        let palette = self.input.color_theme.palette();
        let icon_foreground = palette.foreground;
        let icon_accent = palette.primary;
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
                    img(cached_image_source(url, cover_size, window.scale_factor()))
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
                            if self.input.expanded {
                                MediaIcon::ChevronDown
                            } else {
                                MediaIcon::ChevronUp
                            },
                            "#ffffff",
                            px(24.),
                        ))
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(PlayerAction::ToggleLyrics))),
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
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(PlayerAction::Album(album.clone()))),
                    )
            })
            .child(title_text);
        let liked = self.input.liked;
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
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PlayerAction::Artist(artist.clone()))
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
                                        .on_click(cx.listener(|_, _, _, cx| {
                                            cx.emit(PlayerAction::ToggleLike)
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
                                            .toggled(self.input.shuffle)
                                            .selected(self.input.shuffle)
                                            .disabled(self.input.queue_empty)
                                            .child(div().w(px(28.)).flex().justify_center().child(
                                                media_icon(
                                                    MediaIcon::Shuffle,
                                                    if self.input.shuffle {
                                                        icon_accent
                                                    } else {
                                                        icon_foreground
                                                    },
                                                    px(18.),
                                                ),
                                            ))
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(PlayerAction::Shuffle)
                                            })),
                                    )
                                    .when(self.input.shuffle, |this| {
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
                                    .disabled(self.track.is_none())
                                    .child(div().w(px(28.)).flex().justify_center().child(
                                        media_icon(MediaIcon::SkipBack, icon_foreground, px(20.)),
                                    ))
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(PlayerAction::Previous)),
                                    ),
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
                                            || (self.track.is_none()
                                                && self.input.browsed_tracks_empty),
                                    )
                                    .child(media_icon(
                                        if show_pause {
                                            MediaIcon::Pause
                                        } else {
                                            MediaIcon::Play
                                        },
                                        self.input.color_theme.palette().primary_foreground,
                                        px(21.),
                                    ))
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(PlayerAction::TogglePlayback)
                                    })),
                            )
                            .child(
                                Button::new("next")
                                    .ghost()
                                    .rounded(px(999.))
                                    .size(px(44.))
                                    .p_0()
                                    .tooltip("下一首")
                                    .disabled(self.track.is_none())
                                    .child(div().w(px(28.)).flex().justify_center().child(
                                        media_icon(
                                            MediaIcon::SkipForward,
                                            icon_foreground,
                                            px(20.),
                                        ),
                                    ))
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(PlayerAction::Next)),
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
                                            .tooltip(self.input.repeat_mode.label())
                                            .toggled(self.input.repeat_mode != RepeatMode::Off)
                                            .selected(self.input.repeat_mode != RepeatMode::Off)
                                            .disabled(self.input.queue_empty)
                                            .child(div().w(px(28.)).flex().justify_center().child(
                                                media_icon(
                                                    if self.input.repeat_mode == RepeatMode::One {
                                                        MediaIcon::RepeatOne
                                                    } else {
                                                        MediaIcon::Repeat
                                                    },
                                                    if self.input.repeat_mode != RepeatMode::Off {
                                                        icon_accent
                                                    } else {
                                                        icon_foreground
                                                    },
                                                    px(18.),
                                                ),
                                            ))
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(PlayerAction::Repeat)
                                            })),
                                    )
                                    .when(self.input.repeat_mode != RepeatMode::Off, |this| {
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
                                .tooltip(if self.input.volume > 0. {
                                    "静音"
                                } else {
                                    "取消静音"
                                })
                                .child(div().w(px(28.)).flex().justify_center().child(media_icon(
                                    if self.input.volume > 0. {
                                        MediaIcon::Volume
                                    } else {
                                        MediaIcon::VolumeMuted
                                    },
                                    icon_foreground,
                                    px(19.),
                                )))
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(PlayerAction::ToggleMute)),
                                ),
                        )
                        .child(Slider::new(&self.volume_slider).w(if compact {
                            px(82.)
                        } else {
                            px(118.)
                        })),
                )
            });

        content.into_any_element()
    }
}

impl LyruneView {
    pub(super) fn handle_player_action(
        &mut self,
        action: &PlayerAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            PlayerAction::ToggleLyrics => {
                self.set_cover_backdrop_expanded(!self.cover_backdrop_expanded, window, cx)
            }
            PlayerAction::Album(album) => {
                if self.cover_backdrop_expanded {
                    self.set_cover_backdrop_expanded(false, window, cx);
                }
                self.browser.update(cx, |browser, cx| {
                    browser.open_home_playlist(album.clone().into_playlist(), window, cx)
                });
            }
            PlayerAction::Artist(artist) => {
                if self.cover_backdrop_expanded {
                    self.set_cover_backdrop_expanded(false, window, cx);
                }
                self.browser.update(cx, |browser, cx| {
                    browser.open_search_artist(artist.clone(), window, cx)
                });
            }
            PlayerAction::ToggleLike => self
                .browser
                .update(cx, |browser, cx| browser.toggle_current_track_liked(cx)),
            PlayerAction::Shuffle => self.playback.update(cx, |playback, cx| {
                playback.state.shuffle = !playback.state.shuffle;
                playback.persist_playback_options(cx);
                #[cfg(target_os = "linux")]
                playback.sync_mpris(false, cx);
                cx.notify();
            }),
            PlayerAction::Previous => self
                .playback
                .update(cx, |playback, cx| playback.play_previous(cx)),
            PlayerAction::TogglePlayback => self
                .playback
                .update(cx, |playback, cx| playback.toggle_playback(cx)),
            PlayerAction::Next => self
                .playback
                .update(cx, |playback, cx| playback.play_next(false, cx)),
            PlayerAction::Repeat => self.playback.update(cx, |playback, cx| {
                playback.state.repeat_mode = playback.state.repeat_mode.next();
                playback.persist_playback_options(cx);
                #[cfg(target_os = "linux")]
                playback.sync_mpris(false, cx);
                cx.notify();
            }),
            PlayerAction::ToggleMute => {
                let settings = &self.preferences.read(cx).settings;
                let volume = if settings.volume > 0. {
                    0.
                } else {
                    settings.last_nonzero_volume
                };
                self.player_controls
                    .update(cx, |view, cx| view.set_volume(volume, window, cx));
                self.playback
                    .update(cx, |playback, cx| playback.set_volume(volume, cx));
                self.preferences.read(cx).save();
            }
            PlayerAction::Quality(quality) => self.playback.update(cx, |playback, cx| {
                playback.set_playback_quality(*quality, cx)
            }),
            PlayerAction::ToggleQuality => {
                self.quality_menu_open = !self.quality_menu_open;
                self.account_menu_open = false;
                cx.notify();
            }
            PlayerAction::Volume { value, persist } => {
                self.playback
                    .update(cx, |playback, cx| playback.set_volume(*value, cx));
                if *persist {
                    self.preferences.read(cx).save();
                }
            }
        }
    }

    pub(super) fn render_player_bar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let track = self
            .playback
            .read(cx)
            .state
            .queue
            .as_ref()
            .and_then(|queue| {
                queue
                    .tracks
                    .get(self.playback.read(cx).state.current_track?)
                    .cloned()
            });
        let has_track = track.is_some();
        let foreground = if self.cover_backdrop_expanded {
            cx.theme().foreground
        } else {
            cx.theme().secondary_foreground
        };
        let input = ProgressInput {
            duration: self.playback.read(cx).state.duration(),
            interactive: has_track
                && self.playback.read(cx).state.started
                && self.playback.read(cx).state.load.track.is_none(),
            advancing: self.playback.read(cx).state.is_advancing(),
            frame_rate: self.preferences.read(cx).settings.progress_frame_rate,
            font: self.preferences.read(cx).fonts.monospace.clone(),
            foreground,
        };
        self.progress.update(cx, |progress, cx| {
            progress.set_input(input, self.playback.read(cx).state.position, window, cx);
        });

        let input = PlayerInput {
            liked: track
                .as_ref()
                .and_then(|track| self.browser.read(cx).liked_tracks.get(&track.mid))
                .copied(),
            active_quality: self.playback.read(cx).state.active_quality,
            available_qualities: self.playback.read(cx).state.available_qualities.clone(),
            loading: self.playback.read(cx).state.load.track.is_some(),
            show_pause: if self.playback.read(cx).state.load.track.is_some() {
                self.playback.read(cx).state.load.autoplay
            } else {
                self.playback
                    .read(cx)
                    .state
                    .audio
                    .as_deref()
                    .is_some_and(AudioPlayer::is_playing)
            },
            queue_empty: self
                .playback
                .read(cx)
                .state
                .queue
                .as_ref()
                .is_none_or(|queue| queue.tracks.is_empty()),
            browsed_tracks_empty: self
                .browser
                .read(cx)
                .track_table
                .read(cx)
                .delegate()
                .tracks()
                .is_empty(),
            shuffle: self.playback.read(cx).state.shuffle,
            repeat_mode: self.playback.read(cx).state.repeat_mode,
            volume: self.preferences.read(cx).settings.volume,
            color_theme: self.preferences.read(cx).color_theme,
            expanded: self.cover_backdrop_expanded,
            quality_menu_open: self.quality_menu_open,
        };
        self.player_controls
            .update(cx, |view, cx| view.set_input(track, input, cx));
        player_bar(&self.progress, &self.player_controls, cx.theme().group_box)
    }
}

// 组合边界保持集中，便于用实际 GPUI 布局验证独立刷新不会穿透到控制区。
fn player_bar(
    progress: &Entity<PlaybackProgressView>,
    controls: &Entity<PlayerControlsView>,
    background: Hsla,
) -> AnyElement {
    div()
        .relative()
        .h(px(PLAYER_BAR_HEIGHT))
        .w_full()
        .flex_shrink_0()
        .bg(background)
        .child(progress.clone())
        // 进度条与控制区为同级边界；若缓存共同父视图，进度变脏会连带刷新整个子树。
        .child(
            controls
                .clone()
                .cached(StyleRefinement::default().size_full()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, Window, font,
    };
    use gpui_component::ActiveTheme as _;
    use lyrune_api::Quality;

    use super::{PlayerControlsView, player_bar};
    use crate::app::progress_view::PlaybackProgressView;
    use crate::design::ColorTheme;

    struct TestPlayerBar {
        progress: Entity<PlaybackProgressView>,
        controls: Entity<PlayerControlsView>,
    }

    impl Render for TestPlayerBar {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            player_bar(&self.progress, &self.controls, cx.theme().group_box)
        }
    }

    #[gpui::test]
    fn test_progress_keeps_controls_cached(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let progress = cx.new(|cx| PlaybackProgressView::new(None, font(".SystemUIFont"), cx));
        let controls = cx
            .new(|cx| PlayerControlsView::new(0.5, ColorTheme::default(), Quality::default(), cx));
        let (_, cx) = cx.add_window_view(|_, _| TestPlayerBar {
            progress: progress.clone(),
            controls: controls.clone(),
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let before = controls.read_with(cx, |view, _| view.render_count);
        assert!(before > 0);
        for _ in 0..3 {
            progress.update(cx, |_, cx| cx.notify());
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        assert_eq!(controls.read_with(cx, |view, _| view.render_count), before);

        controls.update(cx, |view, cx| {
            view.input.quality_menu_open = true;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            controls.read_with(cx, |view, _| view.render_count),
            before + 1
        );
    }
}
