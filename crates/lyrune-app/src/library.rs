use std::sync::Arc;

use crate::http::cached_image_source;
use crate::icons::{MediaIcon, media_icon_hsla};
use gpui::{
    AnyElement, App, Context, Image, ImageFormat, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Pixels, Stateful, StatefulInteractiveElement as _,
    Styled as _, TextAlign, Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, IndexPath, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    list::{ListDelegate, ListItem, ListState},
    spinner::Spinner,
    table::{Column, TableDelegate, TableState},
    v_flex,
};
use qqmusic_api::integration::{SearchAlbum, SearchArtist, Track, UserPlaylist, UserPlaylistId};
use tokio::sync::mpsc;

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub enum TrackTableEvent {
    Artist(SearchArtist),
    Album(SearchAlbum),
    Unlike(Track),
    Playlist(UserPlaylist),
}

pub enum SearchCard {
    Artist(Arc<SearchArtist>),
    Album(Arc<SearchAlbum>),
    Playlist(Arc<UserPlaylist>),
}

pub struct SearchCardGridDelegate {
    cards: Vec<SearchCard>,
    columns: usize,
    card_width: Pixels,
    card_height: Pixels,
    cover_size: Pixels,
    scale_factor: f32,
    has_more: bool,
    loading_more: bool,
    event_sender: mpsc::UnboundedSender<TrackTableEvent>,
    load_more_sender: mpsc::Sender<()>,
}

impl SearchCardGridDelegate {
    pub fn new(
        event_sender: mpsc::UnboundedSender<TrackTableEvent>,
        load_more_sender: mpsc::Sender<()>,
    ) -> Self {
        Self {
            cards: Vec::new(),
            columns: 1,
            card_width: px(0.),
            card_height: px(0.),
            cover_size: px(0.),
            scale_factor: 1.,
            has_more: false,
            loading_more: false,
            event_sender,
            load_more_sender,
        }
    }

    pub fn set_cards(
        &mut self,
        cards: Vec<SearchCard>,
        columns: usize,
        card_width: Pixels,
        card_height: Pixels,
        cover_size: Pixels,
        scale_factor: f32,
        has_more: bool,
        loading_more: bool,
    ) -> bool {
        let cards_changed = self.cards.len() != cards.len()
            || self
                .cards
                .iter()
                .zip(&cards)
                .any(|(current, next)| match (current, next) {
                    (SearchCard::Artist(current), SearchCard::Artist(next)) => {
                        !Arc::ptr_eq(current, next)
                    }
                    (SearchCard::Album(current), SearchCard::Album(next)) => {
                        !Arc::ptr_eq(current, next)
                    }
                    (SearchCard::Playlist(current), SearchCard::Playlist(next)) => {
                        !Arc::ptr_eq(current, next)
                    }
                    _ => true,
                });
        let changed = cards_changed
            || self.columns != columns
            || self.card_width != card_width
            || self.card_height != card_height
            || self.cover_size != cover_size
            || self.scale_factor != scale_factor
            || self.has_more != has_more
            || self.loading_more != loading_more;
        self.cards = cards;
        self.columns = columns.max(1);
        self.card_width = card_width;
        self.card_height = card_height;
        self.cover_size = cover_size;
        self.scale_factor = scale_factor;
        self.has_more = has_more;
        self.loading_more = loading_more;
        changed
    }

    fn render_cover(
        &self,
        url: Option<String>,
        icon: MediaIcon,
        radius: Pixels,
        window: &Window,
        cx: &Context<ListState<Self>>,
    ) -> AnyElement {
        match url {
            Some(url) => img(cached_image_source(
                url,
                self.cover_size,
                window.scale_factor(),
            ))
            .size(self.cover_size)
            .flex_shrink_0()
            .rounded(radius)
            .into_any_element(),
            None => div()
                .size(self.cover_size)
                .flex_shrink_0()
                .rounded(radius)
                .bg(cx.theme().muted)
                .flex()
                .items_center()
                .justify_center()
                .child(media_icon_hsla(
                    icon,
                    cx.theme().muted_foreground,
                    self.cover_size * 0.38,
                ))
                .into_any_element(),
        }
    }

    fn render_card(
        &self,
        card: &SearchCard,
        index: usize,
        window: &Window,
        cx: &Context<ListState<Self>>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let event_sender = self.event_sender.clone();
        match card {
            SearchCard::Artist(artist) => {
                let artist = artist.clone();
                let title = artist.name.clone();
                let cover = self.render_cover(
                    artist.cover_url.clone(),
                    MediaIcon::Artist,
                    px(999.),
                    window,
                    cx,
                );
                Button::new(format!("search-artist-{index}"))
                    .ghost()
                    .w(self.card_width)
                    .h(self.card_height)
                    .p_2()
                    .rounded(px(12.))
                    .tooltip(title.clone())
                    .child(
                        v_flex()
                            .size_full()
                            .items_center()
                            .gap_3()
                            .child(cover)
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .text_center()
                                    .font_medium()
                                    .text_color(theme.foreground)
                                    .child(title),
                            ),
                    )
                    .on_click(move |_, _, cx| {
                        let _ = event_sender.send(TrackTableEvent::Artist(artist.as_ref().clone()));
                        cx.stop_propagation();
                    })
                    .into_any_element()
            }
            SearchCard::Album(album) => {
                let album = album.clone();
                let title = album.title.clone();
                let subtitle = album.artist.clone();
                let cover = self.render_cover(
                    album.cover_url.clone(),
                    MediaIcon::Album,
                    px(12.),
                    window,
                    cx,
                );
                Button::new(format!("search-album-{index}"))
                    .ghost()
                    .w(self.card_width)
                    .h(self.card_height)
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
                    .on_click(move |_, _, cx| {
                        let _ = event_sender.send(TrackTableEvent::Album(album.as_ref().clone()));
                        cx.stop_propagation();
                    })
                    .into_any_element()
            }
            SearchCard::Playlist(playlist) => {
                let playlist = playlist.clone();
                let title = playlist.title.clone();
                let subtitle = if playlist.owner.is_empty() {
                    "QQ 音乐歌单".to_owned()
                } else {
                    playlist.owner.clone()
                };
                let cover = self.render_cover(
                    playlist.cover_url.clone(),
                    MediaIcon::Playlist,
                    px(12.),
                    window,
                    cx,
                );
                Button::new(format!("search-playlist-{index}"))
                    .ghost()
                    .w(self.card_width)
                    .h(self.card_height)
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
                    .on_click(move |_, _, cx| {
                        let _ =
                            event_sender.send(TrackTableEvent::Playlist(playlist.as_ref().clone()));
                        cx.stop_propagation();
                    })
                    .into_any_element()
            }
        }
    }
}

impl ListDelegate for SearchCardGridDelegate {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.cards.len().div_ceil(self.columns.max(1))
    }

    fn render_section_header(
        &mut self,
        _: usize,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        Some(div().w_full().h(px(24.)))
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let start = index.row * self.columns.max(1);
        let cards = self.cards[start..self.cards.len().min(start + self.columns)]
            .iter()
            .enumerate()
            .map(|(offset, card)| {
                self.render_card(card, start + offset, window, cx)
                    .into_any_element()
            });
        Some(
            ListItem::new(("search-card-row", index.row))
                .disabled(true)
                .h(self.card_height + px(16.))
                .p_0()
                .child(h_flex().w_full().items_start().gap_4().children(cards)),
        )
    }

    fn set_selected_index(
        &mut self,
        _: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
    }

    fn has_more(&self, _: &App) -> bool {
        self.has_more && !self.loading_more
    }

    fn load_more_threshold(&self) -> usize {
        2
    }

    fn load_more(&mut self, _: &mut Window, _: &mut Context<ListState<Self>>) {
        if self.has_more && !self.loading_more && self.load_more_sender.try_send(()).is_ok() {
            self.loading_more = true;
        }
    }

    fn render_section_footer(
        &mut self,
        _: usize,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        Some(
            h_flex()
                .w_full()
                .h(px(52.))
                .justify_center()
                .when(self.loading_more, |this| {
                    this.child(Spinner::new().with_size(px(18.)))
                }),
        )
    }
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

    pub fn update_liked_track_count(&mut self, liked: bool) {
        if let Some(playlist) = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == UserPlaylistId::Liked)
        {
            playlist.track_count = if liked {
                playlist.track_count.saturating_add(1)
            } else {
                playlist.track_count.saturating_sub(1)
            };
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
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let playlist = self.playlists.get(index.row)?.clone();
        let selected = self.selected_index == Some(index);
        let subtitle = playlist_subtitle(&playlist);
        let cover = playlist_cover(&playlist, px(44.), px(9.), window.scale_factor(), cx);

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
    scale_factor: f32,
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
        return img(cached_image_source(url, size, scale_factor))
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
    header_height: Pixels,
    header_text_padding: Pixels,
    tracks: Vec<Arc<Track>>,
    loading: bool,
    has_more: bool,
    playing_index: Option<usize>,
    loading_index: Option<usize>,
    playback_active: bool,
    show_liked_actions: bool,
    compact: bool,
    load_more_sender: mpsc::Sender<()>,
    event_sender: mpsc::UnboundedSender<TrackTableEvent>,
}

impl TrackTableDelegate {
    pub fn new(
        load_more_sender: mpsc::Sender<()>,
        event_sender: mpsc::UnboundedSender<TrackTableEvent>,
    ) -> Self {
        Self::new_with_header_style(load_more_sender, event_sender, px(48.), px(6.))
    }

    pub fn new_with_header_style(
        load_more_sender: mpsc::Sender<()>,
        event_sender: mpsc::UnboundedSender<TrackTableEvent>,
        header_height: Pixels,
        header_text_padding: Pixels,
    ) -> Self {
        Self {
            columns: track_columns(false),
            header_height,
            header_text_padding,
            tracks: Vec::new(),
            loading: false,
            has_more: false,
            playing_index: None,
            loading_index: None,
            playback_active: false,
            show_liked_actions: false,
            compact: false,
            load_more_sender,
            event_sender,
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
            let sender = self.event_sender.clone();
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
                        let _ = sender.send(TrackTableEvent::Artist(artist.clone()));
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

    pub fn reset(&mut self, show_liked_actions: bool) {
        self.tracks.clear();
        self.loading = true;
        self.has_more = false;
        self.playing_index = None;
        self.loading_index = None;
        self.playback_active = false;
        self.show_liked_actions = show_liked_actions;
        self.columns = track_columns(self.compact);
    }

    pub fn set_tracks(&mut self, tracks: Vec<Arc<Track>>, has_more: bool, loading: bool) {
        self.tracks = tracks;
        self.loading = loading;
        self.has_more = has_more;
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
        self.columns = track_columns(compact);
        true
    }

    pub fn tracks(&self) -> &[Arc<Track>] {
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
        self.show_liked_actions = false;
        self.columns = track_columns(self.compact);
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
            .h(self.header_height)
            .mb(px(4.))
            .overflow_hidden()
            .border_b_1()
            .border_color(cx.theme().border)
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let is_duration = self.columns[col_ix].key.as_ref() == "duration";
        let content = if is_duration {
            media_icon_hsla(MediaIcon::Clock, cx.theme().muted_foreground, px(18.))
        } else {
            self.columns[col_ix].name.clone().into_any_element()
        };
        div()
            .size_full()
            .when(self.columns[col_ix].align == TextAlign::Right, |this| {
                this.flex().justify_end().text_right()
            })
            .pt(self.header_text_padding)
            .when(is_duration, |this| this.pr(px(8.)))
            .child(content)
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
        window: &mut Window,
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
                    Some(url) => img(cached_image_source(url, px(44.), window.scale_factor()))
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
                        let sender = self.event_sender.clone();
                        let hover_color = cx.theme().primary;
                        this.cursor_pointer()
                            .hover(move |style| style.text_color(hover_color))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                let _ = sender.send(TrackTableEvent::Album(album.clone()));
                            })
                    })
                    .child(album)
                    .into_any_element()
            }
            "duration" => {
                let group = format!("track-row-{row_ix}");
                let sender = self.event_sender.clone();
                let hover_background = cx.theme().muted;
                let duration = format_duration(track.duration_seconds);
                h_flex()
                    .w_full()
                    .h_full()
                    .justify_end()
                    .gap_1()
                    .text_right()
                    .text_color(cx.theme().muted_foreground)
                    .when(self.show_liked_actions, |cell| {
                        cell.child(
                            div()
                                .id(("unlike-track", row_ix))
                                .size(px(28.))
                                .flex_shrink_0()
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(0.)
                                .group_hover(group, |style| style.opacity(1.))
                                .cursor_pointer()
                                .hover(move |style| style.bg(hover_background))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    let _ = sender
                                        .send(TrackTableEvent::Unlike(track.as_ref().clone()));
                                })
                                .child(media_icon_hsla(
                                    MediaIcon::HeartFilled,
                                    cx.theme().danger,
                                    px(16.),
                                )),
                        )
                    })
                    .child(duration)
                    .into_any_element()
            }
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

fn track_columns(compact: bool) -> Vec<Column> {
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
