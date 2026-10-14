//! 歌手页面的歌曲、专辑分页及其展示；播放时将选中的页面数据交给独立队列。

use std::sync::Arc;

use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    list::List,
    scroll::ScrollableElement as _,
    spinner::Spinner,
    v_flex,
};
use lyrune_api::{SearchAlbum, SearchArtist, Track};

use super::browser::BrowserView;
use super::navigation::{MainContent, NavigationPage, SharedArtistResource, lock_resource};
use super::{ARTIST_ALBUM_PAGE_SIZE, ARTIST_PAGE_SIZE, page_horizontal_padding};
use crate::http::cached_image_source;
use crate::icons::{MediaIcon, media_icon, media_icon_hsla};
use crate::library::{SearchCard, card_grid_dimensions, format_duration};
use crate::player::AudioPlayer;
use crate::runtime::RUNTIME;

pub(super) struct ArtistPageState {
    pub(super) selected: Option<SearchArtist>,
    pub(super) resource: Option<SharedArtistResource>,
    pub(super) album_scroll_offset: Pixels,
    pub(super) pending_album_scroll: bool,
    pub(super) pending_albums_sync: bool,
}

impl Default for ArtistPageState {
    fn default() -> Self {
        Self {
            selected: None,
            resource: None,
            album_scroll_offset: px(0.),
            pending_album_scroll: false,
            pending_albums_sync: true,
        }
    }
}

impl BrowserView {
    pub(super) fn open_search_artist(
        &mut self,
        artist: SearchArtist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let resource = self
            .page_resource_cache
            .artist(self.account.read(cx).id().unwrap_or(0), &artist.mid);
        let target = NavigationPage::Artist {
            artist,
            resource: Some(resource),
        };
        let current = self.current_navigation_page(cx);
        self.record_navigation(current, &target, window, cx);
        self.apply_navigation_page(target, window, cx);
    }

    pub(super) fn open_artist(
        &mut self,
        artist: SearchArtist,
        resource: SharedArtistResource,
        cx: &mut Context<Self>,
    ) {
        self.artist.selected = Some(artist);
        self.artist.resource = Some(resource.clone());
        self.main_content = MainContent::Artist;
        let (load_songs, load_albums) = {
            let state = lock_resource(&resource);
            (
                state.songs.is_none() && !state.songs_loading,
                state.albums.is_none() && !state.albums_loading,
            )
        };
        if load_songs {
            self.load_artist_songs(cx);
        }
        if load_albums {
            self.load_artist_albums(false, cx);
        }
        cx.notify();
    }

    fn load_artist_songs(&mut self, cx: &mut Context<Self>) {
        let Some(resource) = self.artist.resource.clone() else {
            return;
        };
        let Some(artist) = self.artist.selected.clone() else {
            return;
        };
        let Some(credential) = self.account.read(cx).credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            lock_resource(&resource).song_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        {
            let mut state = lock_resource(&resource);
            if state.songs_loading || state.songs.is_some() {
                return;
            }
            state.song_error = None;
            state.songs_loading = true;
        }
        let playlist = artist.into_playlist();
        cx.notify();

        let task = RUNTIME.spawn(async move {
            client
                .playlist_page(&credential, &playlist, 0, ARTIST_PAGE_SIZE)
                .await
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                lock_resource(&resource).apply_songs(result);
                if this
                    .artist
                    .resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource))
                {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn load_artist_albums(&mut self, append: bool, cx: &mut Context<Self>) {
        let Some(resource) = self.artist.resource.clone() else {
            return;
        };
        let Some(artist) = self.artist.selected.clone() else {
            return;
        };
        let Some(credential) = self.account.read(cx).credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            lock_resource(&resource).album_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        let offset = {
            let mut state = lock_resource(&resource);
            if state.albums_loading || state.albums_loading_more {
                return;
            }
            if let Some(page) = &state.albums {
                if !append || !page.has_more {
                    return;
                }
            } else if append {
                return;
            }
            let offset = state.albums.as_ref().map_or(0, |page| page.next_offset);
            state.album_error = None;
            if append {
                state.albums_loading_more = true;
            } else {
                state.albums_loading = true;
            }
            offset
        };
        cx.notify();

        let task = RUNTIME.spawn(async move {
            client
                .artist_albums(&credential, &artist, offset, ARTIST_ALBUM_PAGE_SIZE)
                .await
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                lock_resource(&resource).apply_albums(offset, result);
                if this
                    .artist
                    .resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource))
                {
                    this.artist.pending_albums_sync = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn open_artist_albums(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(artist) = self.artist.selected.clone() else {
            return;
        };
        let target = NavigationPage::ArtistAlbums {
            artist,
            scroll_offset: px(0.),
            resource: self.artist.resource.clone(),
        };
        let current = self.current_navigation_page(cx);
        self.record_navigation(current, &target, window, cx);
        self.apply_navigation_page(target, window, cx);
    }

    fn select_artist_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(artist) = self.artist.selected.clone() else {
            return;
        };
        let Some(resource) = self.artist.resource.as_ref() else {
            return;
        };
        let (tracks, has_more, track_count) = {
            let state = lock_resource(resource);
            let Some(songs) = state.songs.as_ref() else {
                return;
            };
            (songs.items.clone(), songs.has_more, state.track_count)
        };
        let mut playlist = artist.into_playlist();
        playlist.track_count = track_count;

        self.select_playlist_track(playlist, tracks, has_more, index, cx);
    }

    fn render_artist_header(
        &mut self,
        compact: bool,
        narrow: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(artist) = self.artist.selected.clone() else {
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
            .child(self.render_artist_cover(
                artist.cover_url,
                MediaIcon::Artist,
                cover_size,
                px(999.),
                scale_factor,
                cx,
            ));
        let (track_count, has_tracks) =
            self.artist
                .resource
                .as_ref()
                .map_or((0, false), |resource| {
                    let state = lock_resource(resource);
                    (
                        state.track_count,
                        state
                            .songs
                            .as_ref()
                            .is_some_and(|songs| !songs.items.is_empty()),
                    )
                });

        div()
            .min_h(if narrow {
                px(182.)
            } else if compact {
                px(206.)
            } else {
                px(238.)
            })
            .w_full()
            .px(page_horizontal_padding(narrow))
            .pt_4()
            .pb_3()
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
                                        .child("歌手"),
                                ),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(if narrow {
                                        px(28.)
                                    } else if compact {
                                        px(34.)
                                    } else {
                                        px(44.)
                                    })
                                    .line_height(if narrow {
                                        px(36.)
                                    } else if compact {
                                        px(44.)
                                    } else {
                                        px(56.)
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
                                                    self.preferences
                                                        .read(cx)
                                                        .color_theme
                                                        .palette()
                                                        .primary_foreground,
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

    pub(super) fn render_artist_content(
        &mut self,
        compact: bool,
        narrow: bool,
        scale_factor: f32,
        content_width: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (songs, songs_loading, song_error, albums, albums_loading, album_error) =
            self.artist.resource.as_ref().map_or_else(
                || (None, false, None, None, false, None),
                |resource| {
                    let state = lock_resource(resource);
                    (
                        state.songs.clone(),
                        state.songs_loading,
                        state.song_error.clone(),
                        state.albums.as_ref().map(|page| {
                            page.items
                                .iter()
                                .take(ARTIST_ALBUM_PAGE_SIZE as usize)
                                .cloned()
                                .collect::<Vec<_>>()
                        }),
                        state.albums_loading,
                        state.album_error.clone(),
                    )
                },
            );
        let song_body = if songs_loading && songs.is_none() {
            h_flex()
                .h(px(92.))
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().with_size(px(22.)).color(theme.primary))
                .child("正在加载歌曲…")
                .into_any_element()
        } else if let Some(error) = song_error {
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
                        .on_click(cx.listener(|this, _, _, cx| this.load_artist_songs(cx))),
                )
                .into_any_element()
        } else if let Some(songs) = songs.filter(|page| !page.items.is_empty()) {
            let visible = (ARTIST_PAGE_SIZE as usize).min(songs.items.len());
            self.render_artist_song_rows(songs.items[..visible].to_vec(), narrow, scale_factor, cx)
        } else {
            h_flex()
                .h(px(72.))
                .items_center()
                .text_color(theme.muted_foreground)
                .child("暂无歌曲")
                .into_any_element()
        };

        let album_body = if albums_loading && albums.is_none() {
            h_flex()
                .h(px(120.))
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().with_size(px(22.)).color(theme.primary))
                .child("正在加载专辑…")
                .into_any_element()
        } else if let Some(error) = album_error.filter(|_| albums.is_none()) {
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
        } else if let Some(albums) = albums.filter(|albums| !albums.is_empty()) {
            let available_width = content_width - page_horizontal_padding(narrow) * 2.;
            self.render_artist_albums(albums, available_width, compact, scale_factor, cx)
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
                        .child(self.render_artist_header(compact, narrow, scale_factor, cx))
                        .child(
                            v_flex()
                                .w_full()
                                .px(page_horizontal_padding(narrow))
                                .pb_10()
                                .gap_10()
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap_3()
                                        .child(
                                            h_flex()
                                                .justify_between()
                                                .child(
                                                    div()
                                                        .text_size(px(24.))
                                                        .font_semibold()
                                                        .child("歌曲"),
                                                )
                                                .child(
                                                    Button::new("view-all-artist-songs")
                                                        .debug_selector(|| {
                                                            "view-all-artist-songs".into()
                                                        })
                                                        .ghost()
                                                        .label("查看全部")
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                if let Some(artist) =
                                                                    this.artist.selected.clone()
                                                                {
                                                                    this.open_home_playlist(
                                                                        artist.into_playlist(),
                                                                        window,
                                                                        cx,
                                                                    );
                                                                }
                                                            },
                                                        )),
                                                ),
                                        )
                                        .child(song_body),
                                )
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap_4()
                                        .child(
                                            h_flex()
                                                .justify_between()
                                                .child(
                                                    div()
                                                        .text_size(px(24.))
                                                        .font_semibold()
                                                        .child("专辑"),
                                                )
                                                .child(
                                                    Button::new("view-all-artist-albums")
                                                        .debug_selector(|| {
                                                            "view-all-artist-albums".into()
                                                        })
                                                        .ghost()
                                                        .label("查看全部")
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.open_artist_albums(window, cx)
                                                            },
                                                        )),
                                                ),
                                        )
                                        .child(album_body),
                                ),
                        ),
                ),
            )
            .into_any_element()
    }

    pub(super) fn render_artist_album_page(
        &mut self,
        narrow: bool,
        content_width: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let sync_results = std::mem::take(&mut self.artist.pending_albums_sync);
        let (cards, has_results, is_empty, has_more, loading, loading_more, error) =
            self.artist.resource.as_ref().map_or_else(
                || (None, false, true, false, false, false, None),
                |resource| {
                    let state = lock_resource(resource);
                    let albums = state.albums.as_ref();
                    (
                        sync_results.then(|| {
                            albums.map_or_else(Vec::new, |page| {
                                page.items.iter().cloned().map(SearchCard::Album).collect()
                            })
                        }),
                        albums.is_some(),
                        albums.is_none_or(|page| page.items.is_empty()),
                        albums.is_some_and(|page| page.has_more),
                        state.albums_loading,
                        state.albums_loading_more,
                        state.album_error.clone(),
                    )
                },
            );
        self.artist_album_grid.update(cx, |list, cx| {
            if list.delegate_mut().set_cards(
                cards,
                content_width - page_horizontal_padding(narrow) * 2.,
                px(74.),
                has_more && error.is_none(),
                loading_more,
                window,
            ) {
                cx.notify();
            }
            if has_results && !is_empty && self.artist.pending_album_scroll {
                list.scroll_handle()
                    .set_offset(point(px(0.), self.artist.album_scroll_offset));
                self.artist.pending_album_scroll = false;
                cx.notify();
            }
        });

        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .h_full()
            .bg(theme.background)
            .px(page_horizontal_padding(narrow))
            .child(
                self.render_artist_collection_header(
                    self.artist
                        .selected
                        .as_ref()
                        .map(|artist| artist.name.as_str()),
                    self.artist
                        .selected
                        .as_ref()
                        .and_then(|artist| artist.cover_url.clone()),
                    "专辑",
                    window.scale_factor(),
                    cx,
                ),
            )
            .when(loading && !has_results, |this| {
                this.child(
                    h_flex()
                        .h(px(260.))
                        .justify_center()
                        .gap_3()
                        .text_color(theme.muted_foreground)
                        .child(Spinner::new().with_size(px(24.)).color(theme.primary))
                        .child("正在加载专辑…"),
                )
            })
            .when(!loading && is_empty && error.is_none(), |this| {
                this.child(
                    h_flex()
                        .h(px(220.))
                        .justify_center()
                        .text_color(theme.muted_foreground)
                        .child("暂无专辑"),
                )
            })
            .when(has_results && !is_empty, |this| {
                this.child(
                    div().flex_1().min_h_0().child(
                        List::new(&self.artist_album_grid)
                            .size_full()
                            .scrollbar_visible(false),
                    ),
                )
            })
            .when_some(error, |this, error| {
                this.child(
                    h_flex()
                        .flex_shrink_0()
                        .py_2()
                        .gap_3()
                        .text_color(theme.danger)
                        .child(error)
                        .child(
                            Button::new("retry-all-artist-albums")
                                .outline()
                                .label("重新加载")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.load_artist_albums(has_results, cx);
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_artist_collection_header(
        &self,
        name: Option<&str>,
        cover_url: Option<String>,
        kind: &'static str,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let mut title = h_flex().h(px(36.)).min_w_0().gap_3().text_size(px(24.));
        if let Some(name) = name {
            title = title
                .child(self.render_artist_cover(
                    cover_url,
                    MediaIcon::Artist,
                    px(32.),
                    px(999.),
                    scale_factor,
                    cx,
                ))
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .child(div().truncate().font_semibold().child(name.to_owned()))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(theme.secondary_foreground)
                                .child(format!("的{kind}")),
                        ),
                );
        } else {
            title = title.child(kind);
        }
        div()
            .debug_selector(|| "artist-collection-header".into())
            .pt_2()
            .pb_3()
            .flex_shrink_0()
            .child(title)
            .into_any_element()
    }

    fn render_artist_cover(
        &self,
        cover_url: Option<String>,
        icon: MediaIcon,
        size: Pixels,
        radius: Pixels,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        match cover_url {
            Some(url) => img(cached_image_source(url, size, scale_factor))
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

    fn render_artist_song_rows(
        &mut self,
        songs: Vec<Arc<Track>>,
        narrow: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let monospace_font = self.preferences.read(cx).fonts.monospace.clone();
        let current_mid = self
            .playback
            .read(cx)
            .state
            .track()
            .map(|track| track.mid.clone());
        let loading_mid = self
            .playback
            .read(cx)
            .state
            .load
            .track
            .and_then(|index| {
                self.playback
                    .read(cx)
                    .state
                    .queue
                    .as_ref()?
                    .tracks
                    .get(index)
            })
            .map(|track| track.mid.clone());
        let is_playing = self
            .playback
            .read(cx)
            .state
            .audio
            .as_deref()
            .is_some_and(AudioPlayer::is_playing);
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
                let cover = self.render_artist_cover(
                    track.cover_url.clone(),
                    MediaIcon::Music,
                    px(48.),
                    px(9.),
                    scale_factor,
                    cx,
                );
                Button::new(format!("artist-song-{}-{}", track.mid, index))
                    .debug_selector(move || format!("artist-preview-song-{index}"))
                    .ghost()
                    .w_full()
                    .h(px(68.))
                    .px_3()
                    .rounded(px(10.))
                    .selected(is_current)
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
                                                Spinner::new()
                                                    .with_size(px(17.))
                                                    .color(theme.primary),
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
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_artist_track(index, cx)),
                    )
            })
            .collect::<Vec<_>>();
        v_flex().w_full().gap_1().children(rows).into_any_element()
    }

    fn render_artist_albums(
        &mut self,
        albums: Vec<Arc<SearchAlbum>>,
        available_width: Pixels,
        compact: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (columns, card_width) = card_grid_dimensions(available_width, compact);
        let cover_size = card_width - px(16.);
        let cards = albums
            .into_iter()
            .enumerate()
            .map(|(index, album)| {
                let title = album.title.clone();
                let subtitle = album.artist.clone();
                let cover = self.render_artist_cover(
                    album.cover_url.clone(),
                    MediaIcon::Album,
                    cover_size,
                    px(12.),
                    scale_factor,
                    cx,
                );
                let playlist = album.as_ref().clone().into_playlist();
                Button::new(format!("search-album-{index}"))
                    .debug_selector(move || format!("artist-preview-album-{index}"))
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
            .collect::<Vec<_>>();
        div()
            .grid()
            .grid_cols(columns as u16)
            .w_full()
            .gap_4()
            .children(cards)
            .into_any_element()
    }
}
