use std::sync::Arc;

use crate::http::cached_image_source;
use crate::icons::{MediaIcon, media_icon_hsla};
use async_channel::Sender;
use gpui::{
    AnyElement, App, Context, Image, ImageFormat, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Pixels, Stateful, StatefulInteractiveElement as _,
    Styled as _, Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, IndexPath, StyledExt as _, h_flex,
    list::{ListDelegate, ListItem, ListState},
    table::{Column, TableDelegate, TableState},
    v_flex,
};
use qqmusic_api::integration::{SearchAlbum, SearchArtist, Track, UserPlaylist, UserPlaylistId};

#[derive(Clone)]
pub enum TrackTableNavigation {
    Artist(SearchArtist),
    Album(SearchAlbum),
}

pub struct PlaylistListDelegate {
    playlists: Vec<UserPlaylist>,
    selected_index: Option<IndexPath>,
}

impl PlaylistListDelegate {
    pub fn new() -> Self {
        Self {
            playlists: Vec::new(),
            selected_index: None,
        }
    }

    pub fn set_playlists(&mut self, playlists: Vec<UserPlaylist>) {
        self.playlists = playlists;
        self.selected_index = None;
    }

    pub fn update_playlist(&mut self, index: usize, playlist: UserPlaylist) {
        if let Some(current) = self.playlists.get_mut(index) {
            *current = playlist;
        }
    }

    pub fn set_selected(&mut self, index: usize) {
        self.selected_index = Some(IndexPath::new(index));
    }

    pub fn playlist(&self, index: usize) -> Option<&UserPlaylist> {
        self.playlists.get(index)
    }

    pub fn clear(&mut self) {
        self.playlists.clear();
        self.selected_index = None;
    }
}

impl ListDelegate for PlaylistListDelegate {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.playlists.len()
    }

    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = index;
        cx.notify();
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let playlist = self.playlists.get(index.row)?.clone();
        let selected = self.selected_index == Some(index);
        let subtitle = playlist_subtitle(&playlist);
        let cover = playlist_cover(&playlist, px(44.), px(9.), cx);

        Some(
            ListItem::new(("playlist", index.row))
                .selected(selected)
                .h(px(64.))
                .px_3()
                .rounded(px(9.))
                .child(
                    h_flex()
                        .w_full()
                        .h(px(56.))
                        .min_w_0()
                        .gap_3()
                        .px_2()
                        .rounded(px(9.))
                        .when(selected, |row| row.bg(cx.theme().muted))
                        .child(cover)
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap_0p5()
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .font_medium()
                                        .child(playlist.title),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .text_xs()
                                        .text_color(cx.theme().secondary_foreground)
                                        .child(subtitle),
                                ),
                        )
                        .when(selected, |row| {
                            row.child(
                                div()
                                    .w(px(3.))
                                    .h(px(24.))
                                    .rounded_full()
                                    .bg(cx.theme().primary),
                            )
                        }),
                ),
        )
    }
}

pub fn playlist_cover(
    playlist: &UserPlaylist,
    size: Pixels,
    radius: Pixels,
    cx: &App,
) -> AnyElement {
    if playlist.id == UserPlaylistId::Liked {
        let radius_percent = (f32::from(radius) / f32::from(size) * 100.).clamp(0., 50.);
        let color = |color: gpui::Hsla| {
            let rgba: u32 = color.to_rgb().into();
            format!("#{rgba:08x}")
        };
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
<defs><clipPath id="cover"><rect width="100" height="100" rx="{radius_percent}"/></clipPath></defs>
<g clip-path="url(#cover)">
<rect width="100" height="100" fill="{}"/>
<circle cx="82" cy="12" r="36" fill="{}" fill-opacity="0.28"/>
<circle cx="14" cy="104" r="34" fill="{}" fill-opacity="0.22"/>
</g>
<path d="M50 73 27 52C10 37 20 19 36 22c7 1 11 7 14 12 3-5 7-11 14-12 16-3 26 15 9 30Z" transform="translate(32.5 33.9) scale(.35)" vector-effect="non-scaling-stroke" fill="none" stroke="{}" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"/>
</svg>"#,
            color(cx.theme().ring),
            color(cx.theme().primary),
            color(cx.theme().danger),
            color(cx.theme().foreground),
        );
        return img(Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            svg.into_bytes(),
        )))
        .size(size)
        .flex_shrink_0()
        .rounded(radius)
        .into_any_element();
    }

    if let Some(url) = playlist.cover_url.clone() {
        return img(cached_image_source(url))
            .size(size)
            .flex_shrink_0()
            .rounded(radius)
            .into_any_element();
    }

    div()
        .size(size)
        .flex_shrink_0()
        .rounded(radius)
        .bg(cx.theme().muted)
        .text_color(cx.theme().secondary_foreground)
        .flex()
        .items_center()
        .justify_center()
        .child(media_icon_hsla(
            MediaIcon::Folder,
            cx.theme().secondary_foreground,
            size * 0.38,
        ))
        .into_any_element()
}

pub struct TrackTableDelegate {
    columns: Vec<Column>,
    tracks: Vec<Track>,
    loading: bool,
    has_more: bool,
    playing_index: Option<usize>,
    loading_index: Option<usize>,
    playback_active: bool,
    show_added_at: bool,
    compact: bool,
    load_more_sender: Sender<()>,
    navigation_sender: Sender<TrackTableNavigation>,
}

impl TrackTableDelegate {
    pub fn new(
        load_more_sender: Sender<()>,
        navigation_sender: Sender<TrackTableNavigation>,
    ) -> Self {
        Self {
            columns: track_columns(false, false),
            tracks: Vec::new(),
            loading: false,
            has_more: false,
            playing_index: None,
            loading_index: None,
            playback_active: false,
            show_added_at: false,
            compact: false,
            load_more_sender,
            navigation_sender,
        }
    }

    fn render_artists(&self, row_ix: usize, track: &Track, cx: &App) -> AnyElement {
        if track.artist_details.is_empty() {
            return div()
                .w_full()
                .truncate()
                .text_xs()
                .text_color(cx.theme().secondary_foreground)
                .child(track.artists.clone())
                .into_any_element();
        }

        let mut links = Vec::with_capacity(track.artist_details.len() * 2 - 1);
        for (index, artist) in track.artist_details.iter().cloned().enumerate() {
            if index > 0 {
                links.push(
                    div()
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(" / ")
                        .into_any_element(),
                );
            }
            let sender = self.navigation_sender.clone();
            let name = artist.name.clone();
            let hover_color = cx.theme().primary;
            links.push(
                div()
                    .id(format!("track-artist-{row_ix}-{index}"))
                    .flex_shrink_0()
                    .cursor_pointer()
                    .text_color(cx.theme().secondary_foreground)
                    .hover(move |style| style.text_color(hover_color))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        let _ = sender.try_send(TrackTableNavigation::Artist(artist.clone()));
                    })
                    .child(name)
                    .into_any_element(),
            );
        }

        h_flex()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .text_xs()
            .children(links)
            .into_any_element()
    }

    pub fn reset(&mut self) {
        self.tracks.clear();
        self.loading = true;
        self.has_more = false;
        self.playing_index = None;
        self.loading_index = None;
        self.playback_active = false;
        self.show_added_at = false;
        self.columns = track_columns(false, self.compact);
    }

    pub fn append(&mut self, tracks: Vec<Track>, has_more: bool) {
        self.tracks.extend(tracks);
        self.loading = false;
        self.has_more = has_more;
        self.show_added_at = self.tracks.iter().any(|track| track.added_at.is_some());
        self.columns = track_columns(self.show_added_at, self.compact);
    }

    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    pub fn set_playback_state(
        &mut self,
        playing_index: Option<usize>,
        loading_index: Option<usize>,
        playback_active: bool,
    ) {
        self.playing_index = playing_index;
        self.loading_index = loading_index;
        self.playback_active = playback_active;
    }

    pub fn set_compact(&mut self, compact: bool) -> bool {
        if self.compact == compact {
            return false;
        }
        self.compact = compact;
        self.columns = track_columns(self.show_added_at, compact);
        true
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub fn has_more(&self) -> bool {
        self.has_more
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
        self.loading = false;
        self.has_more = false;
        self.playing_index = None;
        self.loading_index = None;
        self.playback_active = false;
        self.show_added_at = false;
        self.columns = track_columns(false, self.compact);
    }
}

impl TableDelegate for TrackTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.tracks.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_header(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<gpui::Div> {
        div()
            .id("track-table-header")
            .h(px(48.))
            .mb(px(4.))
            .overflow_hidden()
            .border_b_1()
            .border_color(cx.theme().border)
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .pt(px(6.))
            .child(self.columns[col_ix].name.clone())
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> Stateful<gpui::Div> {
        div()
            .id(("track-row", row_ix))
            .group(format!("track-row-{row_ix}"))
            .mx_1()
            .rounded(px(9.))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(track) = self.tracks.get(row_ix).cloned() else {
            return div().into_any_element();
        };
        let key = self.columns[col_ix].key.as_ref();
        match key {
            "number" => {
                if self.loading_index == Some(row_ix) {
                    h_flex()
                        .w_full()
                        .h_full()
                        .text_color(cx.theme().primary)
                        .child(media_icon_hsla(
                            MediaIcon::Loading,
                            cx.theme().primary,
                            px(16.),
                        ))
                        .into_any_element()
                } else if self.playing_index == Some(row_ix) {
                    h_flex()
                        .w_full()
                        .h_full()
                        .text_color(cx.theme().primary)
                        .child(media_icon_hsla(
                            if self.playback_active {
                                MediaIcon::Pause
                            } else {
                                MediaIcon::Play
                            },
                            cx.theme().primary,
                            px(17.),
                        ))
                        .into_any_element()
                } else {
                    let group = format!("track-row-{row_ix}");
                    h_flex()
                        .relative()
                        .w_full()
                        .h_full()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            div()
                                .group_hover(group.clone(), |style| style.opacity(0.))
                                .child((row_ix + 1).to_string()),
                        )
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .opacity(0.)
                                .group_hover(group, |style| style.opacity(1.))
                                .child(media_icon_hsla(
                                    MediaIcon::Play,
                                    cx.theme().foreground,
                                    px(16.),
                                )),
                        )
                        .into_any_element()
                }
            }
            "title" => {
                let cover = match track.cover_url.clone() {
                    Some(url) => img(cached_image_source(url))
                        .size(px(44.))
                        .flex_shrink_0()
                        .rounded(px(9.))
                        .into_any_element(),
                    None => div()
                        .size(px(44.))
                        .flex_shrink_0()
                        .rounded(px(9.))
                        .bg(cx.theme().muted)
                        .text_color(cx.theme().muted_foreground)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(media_icon_hsla(
                            MediaIcon::Play,
                            cx.theme().muted_foreground,
                            px(17.),
                        ))
                        .into_any_element(),
                };
                h_flex()
                    .w_full()
                    .h_full()
                    .min_w_0()
                    .gap_3()
                    .child(cover)
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .font_medium()
                                    .text_color(if self.playing_index == Some(row_ix) {
                                        cx.theme().primary
                                    } else {
                                        cx.theme().foreground
                                    })
                                    .child(track.title.clone()),
                            )
                            .child(self.render_artists(row_ix, &track, cx)),
                    )
                    .into_any_element()
            }
            "album" => {
                let album = if track.album.is_empty() {
                    "—".to_owned()
                } else {
                    track.album.clone()
                };
                let album_link = (!track.album_mid.trim().is_empty()
                    && !track.album.trim().is_empty())
                .then(|| SearchAlbum {
                    mid: track.album_mid.clone(),
                    title: track.album.clone(),
                    cover_url: track.cover_url.clone(),
                    artist: track.artists.clone(),
                });
                h_flex()
                    .id(("track-album", row_ix))
                    .w_full()
                    .h_full()
                    .truncate()
                    .text_color(cx.theme().secondary_foreground)
                    .when_some(album_link, |this, album| {
                        let sender = self.navigation_sender.clone();
                        let hover_color = cx.theme().primary;
                        this.cursor_pointer()
                            .hover(move |style| style.text_color(hover_color))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                let _ = sender.try_send(TrackTableNavigation::Album(album.clone()));
                            })
                    })
                    .child(album)
                    .into_any_element()
            }
            "added_at" => h_flex()
                .w_full()
                .h_full()
                .text_color(cx.theme().muted_foreground)
                .child(
                    track
                        .added_at
                        .map(format_date)
                        .unwrap_or_else(|| "—".to_owned()),
                )
                .into_any_element(),
            "duration" => h_flex()
                .w_full()
                .h_full()
                .justify_end()
                .text_right()
                .text_color(cx.theme().muted_foreground)
                .child(format_duration(track.duration_seconds))
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    fn loading(&self, _: &App) -> bool {
        self.loading && self.tracks.is_empty()
    }

    fn has_more(&self, _: &App) -> bool {
        self.has_more && !self.loading
    }

    fn load_more(&mut self, _: &mut Window, _: &mut Context<TableState<Self>>) {
        if self.has_more && !self.loading && self.load_more_sender.try_send(()).is_ok() {
            self.loading = true;
        }
    }
}

fn track_columns(show_added_at: bool, compact: bool) -> Vec<Column> {
    let mut columns = vec![
        Column::new("number", "#")
            .width(px(48.))
            .resizable(false)
            .movable(false),
        Column::new("title", "标题")
            .width(px(420.))
            .min_width(px(240.)),
    ];
    if !compact {
        columns.push(
            Column::new("album", "专辑")
                .width(px(240.))
                .min_width(px(136.)),
        );
    }
    if show_added_at && !compact {
        columns.push(
            Column::new("added_at", "添加日期")
                .width(px(124.))
                .min_width(px(104.)),
        );
    }
    columns.push(
        Column::new("duration", "时长")
            .width(px(84.))
            .min_width(px(72.))
            .text_right()
            .resizable(false)
            .movable(false),
    );
    columns
}

fn playlist_subtitle(playlist: &UserPlaylist) -> String {
    let kind = match playlist.id {
        UserPlaylistId::Liked => "已点赞的歌曲",
        UserPlaylistId::Created { .. } => "创建的歌单",
        UserPlaylistId::Favorite { .. } => "收藏的歌单",
        UserPlaylistId::Recommended { .. } => "推荐歌单",
        UserPlaylistId::Artist { .. } => "歌手",
        UserPlaylistId::Album { .. } => "专辑",
        UserPlaylistId::Search { .. } => "搜索结果",
        UserPlaylistId::Recommendation { .. } => "个性化推荐",
    };
    if playlist.owner.is_empty() {
        format!("{kind} · {} 首", playlist.track_count)
    } else {
        format!("{kind} · {}", playlist.owner)
    }
}

pub fn format_duration(seconds: u64) -> String {
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn format_date(timestamp: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(timestamp)
        .map(|date| {
            let (year, month, day) = date.to_calendar_date();
            format!("{year:04}-{:02}-{day:02}", month as u8)
        })
        .unwrap_or_else(|_| "—".to_owned())
}
