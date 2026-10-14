//! 播放栏、进度预览和音质菜单。音频加载与控制动作在 playback。

use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    slider::Slider,
    v_flex,
};
use lyrune_api::SearchAlbum;

use super::lyrics_view::LYRIC_BACKGROUND_OVERLAY_OPACITY;
use super::progress_view::ProgressInput;
use super::queue::RepeatMode;
use super::{LyruneView, PLAYER_BAR_HEIGHT};
use crate::http::cached_image_source;
use crate::icons::{MediaIcon, media_icon, media_icon_hsla};
use crate::player::AudioPlayer;

impl LyruneView {
    fn render_quality_selector(&mut self, has_track: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let active_quality = self.playback.active_quality;
        let active_label = active_quality.badge_label();
        let options = self
            .playback
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
                    .when(self.playback.load.track.is_some(), |button| {
                        button
                            .bg(theme.input_background())
                            .border_color(theme.input)
                            .text_color(theme.button_foreground)
                    })
                    .disabled(
                        !has_track
                            || self.playback.load.track.is_some()
                            || self.playback.available_qualities.is_empty(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.quality_menu_open = !this.quality_menu_open;
                        this.account.menu_open = false;
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

    pub(super) fn render_player_bar(
        &mut self,
        compact: bool,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let track = self
            .playback
            .queue
            .as_ref()
            .and_then(|queue| queue.tracks.get(self.playback.current_track?).cloned());
        let has_track = track.is_some();
        let quality_selector = self.render_quality_selector(has_track, cx);
        let foreground = if self.cover_backdrop_expanded {
            self.foreground_target(
                cx.theme()
                    .background
                    .opacity(LYRIC_BACKGROUND_OVERLAY_OPACITY),
                cx.theme().foreground,
                |cover| cover.sampled_rgb(narrow),
                window,
                cx,
            )
            .unwrap_or(cx.theme().foreground)
        } else {
            cx.theme().secondary_foreground
        };
        let input = ProgressInput {
            duration: self.playback.duration(),
            interactive: has_track && self.playback.started && self.playback.load.track.is_none(),
            advancing: self.playback.is_advancing(),
            frame_rate: self.settings.progress_frame_rate,
            font: self.fonts.monospace.clone(),
            foreground,
        };
        self.progress.update(cx, |progress, cx| {
            progress.set_input(input, self.playback.position, window, cx);
        });
        let progress = self.progress.clone();
        let theme = cx.theme();
        let is_playing = self
            .playback
            .audio
            .as_deref()
            .is_some_and(AudioPlayer::is_playing);
        let loading = self.playback.load.track.is_some();
        let show_pause = if loading {
            self.playback.load.autoplay
        } else {
            is_playing
        };
        let palette = self.color_theme.palette();
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
                                            .toggled(self.playback.shuffle)
                                            .selected(self.playback.shuffle)
                                            .disabled(
                                                self.playback
                                                    .queue
                                                    .as_ref()
                                                    .is_none_or(|queue| queue.tracks.is_empty()),
                                            )
                                            .child(div().w(px(28.)).flex().justify_center().child(
                                                media_icon(
                                                    MediaIcon::Shuffle,
                                                    if self.playback.shuffle {
                                                        icon_accent
                                                    } else {
                                                        icon_foreground
                                                    },
                                                    px(18.),
                                                ),
                                            ))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.playback.shuffle = !this.playback.shuffle;
                                                #[cfg(target_os = "linux")]
                                                this.sync_mpris(false);
                                                cx.notify();
                                            })),
                                    )
                                    .when(self.playback.shuffle, |this| {
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
                                    .disabled(self.playback.current_track.is_none())
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
                                            || (self.playback.current_track.is_none()
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
                                        self.color_theme.palette().primary_foreground,
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
                                    .disabled(self.playback.current_track.is_none())
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
                                            .tooltip(self.playback.repeat_mode.label())
                                            .toggled(self.playback.repeat_mode != RepeatMode::Off)
                                            .selected(self.playback.repeat_mode != RepeatMode::Off)
                                            .disabled(
                                                self.playback
                                                    .queue
                                                    .as_ref()
                                                    .is_none_or(|queue| queue.tracks.is_empty()),
                                            )
                                            .child(div().w(px(28.)).flex().justify_center().child(
                                                media_icon(
                                                    if self.playback.repeat_mode == RepeatMode::One
                                                    {
                                                        MediaIcon::RepeatOne
                                                    } else {
                                                        MediaIcon::Repeat
                                                    },
                                                    if self.playback.repeat_mode != RepeatMode::Off
                                                    {
                                                        icon_accent
                                                    } else {
                                                        icon_foreground
                                                    },
                                                    px(18.),
                                                ),
                                            ))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.playback.repeat_mode =
                                                    this.playback.repeat_mode.next();
                                                #[cfg(target_os = "linux")]
                                                this.sync_mpris(false);
                                                cx.notify();
                                            })),
                                    )
                                    .when(self.playback.repeat_mode != RepeatMode::Off, |this| {
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
}
