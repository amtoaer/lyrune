use async_channel::Sender;
use gpui::{
    App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Stateful, Styled as _,
    Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, IndexPath, StyledExt as _, h_flex,
    list::{ListDelegate, ListItem, ListState},
    table::{Column, TableDelegate, TableState},
    v_flex,
};
use qqmusic_api::integration::{Track, UserPlaylist, UserPlaylistId};

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
        self.selected_index = (!self.playlists.is_empty()).then(IndexPath::default);
    }

    pub fn update_playlist(&mut self, index: usize, playlist: UserPlaylist) {
        if let Some(current) = self.playlists.get_mut(index) {
            *current = playlist;
        }
    }

    pub fn set_selected(&mut self, index: usize) {
        self.selected_index = Some(IndexPath::new(index));
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
        let cover = match playlist.cover_url {
            Some(url) => img(url)
                .size(px(48.))
                .flex_shrink_0()
                .rounded(px(6.))
                .into_any_element(),
            None => div()
                .size(px(48.))
                .flex_shrink_0()
                .rounded(px(6.))
                .bg(cx.theme().accent)
                .text_color(cx.theme().accent_foreground)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(match playlist.id {
                    UserPlaylistId::Liked => IconName::Heart,
                    _ => IconName::Folder,
                }))
                .into_any_element(),
        };

        Some(
            ListItem::new(("playlist", index.row))
                .selected(selected)
                .h(px(64.))
                .px_2()
                .rounded(px(8.))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_3()
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
                                        .text_color(cx.theme().muted_foreground)
                                        .child(subtitle),
                                ),
                        ),
                ),
        )
    }
}

pub struct TrackTableDelegate {
    columns: Vec<Column>,
    tracks: Vec<Track>,
    loading: bool,
    has_more: bool,
    playing_index: Option<usize>,
    loading_index: Option<usize>,
    load_more_sender: Sender<()>,
}

impl TrackTableDelegate {
    pub fn new(load_more_sender: Sender<()>) -> Self {
        Self {
            columns: track_columns(false),
            tracks: Vec::new(),
            loading: false,
            has_more: false,
            playing_index: None,
            loading_index: None,
            load_more_sender,
        }
    }

    pub fn reset(&mut self) {
        self.tracks.clear();
        self.loading = true;
        self.has_more = false;
        self.playing_index = None;
        self.loading_index = None;
        self.columns = track_columns(false);
    }

    pub fn append(&mut self, tracks: Vec<Track>, has_more: bool) {
        self.tracks.extend(tracks);
        self.loading = false;
        self.has_more = has_more;
        let show_added_at = self.tracks.iter().any(|track| track.added_at.is_some());
        self.columns = track_columns(show_added_at);
    }

    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    pub fn set_playback_state(
        &mut self,
        playing_index: Option<usize>,
        loading_index: Option<usize>,
    ) {
        self.playing_index = playing_index;
        self.loading_index = loading_index;
    }

    pub fn track(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
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

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<gpui::Div> {
        div()
            .id(("track-row", row_ix))
            .when(self.playing_index == Some(row_ix), |row| {
                row.bg(cx.theme().list_active)
            })
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
            "number" => div()
                .w_full()
                .text_color(cx.theme().muted_foreground)
                .child(if self.loading_index == Some(row_ix) {
                    "…".to_owned()
                } else {
                    (row_ix + 1).to_string()
                })
                .into_any_element(),
            "title" => {
                let cover = match track.cover_url {
                    Some(url) => img(url)
                        .size(px(40.))
                        .flex_shrink_0()
                        .rounded(px(4.))
                        .into_any_element(),
                    None => div()
                        .size(px(40.))
                        .flex_shrink_0()
                        .rounded(px(4.))
                        .bg(cx.theme().muted)
                        .text_color(cx.theme().muted_foreground)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(IconName::Play))
                        .into_any_element(),
                };
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .child(cover)
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .child(div().w_full().truncate().font_medium().child(track.title))
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(track.artists),
                            ),
                    )
                    .into_any_element()
            }
            "album" => div()
                .w_full()
                .truncate()
                .text_color(cx.theme().muted_foreground)
                .child(if track.album.is_empty() {
                    "—".to_owned()
                } else {
                    track.album
                })
                .into_any_element(),
            "added_at" => div()
                .w_full()
                .text_color(cx.theme().muted_foreground)
                .child(track.added_at.map(format_date).unwrap_or_else(|| "—".to_owned()))
                .into_any_element(),
            "duration" => div()
                .w_full()
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

fn track_columns(show_added_at: bool) -> Vec<Column> {
    let mut columns = vec![
        Column::new("number", "#")
            .width(px(52.))
            .resizable(false)
            .movable(false),
        Column::new("title", "标题")
            .width(px(420.))
            .min_width(px(260.)),
        Column::new("album", "专辑")
            .width(px(260.))
            .min_width(px(160.)),
    ];
    if show_added_at {
        columns.push(
            Column::new("added_at", "添加日期")
                .width(px(140.))
                .min_width(px(120.)),
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
        UserPlaylistId::Liked => "我喜欢",
        UserPlaylistId::Created { .. } => "创建的歌单",
        UserPlaylistId::Favorite { .. } => "收藏的歌单",
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
