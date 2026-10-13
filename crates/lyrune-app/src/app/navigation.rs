use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use gpui::{App, Context, Pixels, Window, px};
use gpui_component::IndexPath;
use qqmusic_api::integration::{
    SearchAlbum, SearchArtist, SearchPage, SearchResults, Track, UserPlaylist, UserPlaylistId,
};

use crate::icons::MediaIcon;
use crate::settings::DEFAULT_NAVIGATION_HISTORY_LIMIT;

use super::{LIBRARY_CACHE_TTL, LyruneView, SEARCH_PAGE_SIZE, unix_timestamp_secs};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MainContent {
    Home,
    Search,
    Artist,
    Playlist,
    Settings,
}

#[derive(Clone, Copy)]
pub(super) enum PlaylistCachePolicy {
    Fresh,
    AllowStale,
}

#[derive(Clone, Copy)]
pub(super) struct PlaylistScrollPosition {
    pub(super) row: usize,
    pub(super) offset_y: Pixels,
}

impl PlaylistScrollPosition {
    pub(super) fn top() -> Self {
        Self {
            row: 0,
            offset_y: px(0.),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchCategory {
    Songs,
    Playlists,
    Albums,
    Artists,
}

impl SearchCategory {
    pub(super) const ALL: [Self; 4] = [Self::Songs, Self::Playlists, Self::Albums, Self::Artists];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Songs => "单曲",
            Self::Playlists => "歌单",
            Self::Albums => "专辑",
            Self::Artists => "歌手",
        }
    }

    pub(super) fn icon(self) -> MediaIcon {
        match self {
            Self::Songs => MediaIcon::Music,
            Self::Artists => MediaIcon::Artist,
            Self::Albums => MediaIcon::Album,
            Self::Playlists => MediaIcon::Playlist,
        }
    }

    pub(super) fn index(self) -> usize {
        match self {
            Self::Songs => 0,
            Self::Playlists => 1,
            Self::Albums => 2,
            Self::Artists => 3,
        }
    }
}

#[derive(Clone)]
pub(super) enum SearchMoreResults {
    Songs(SearchPage<Track>),
    Artists(SearchPage<SearchArtist>),
    Albums(SearchPage<SearchAlbum>),
    Playlists(SearchPage<UserPlaylist>),
}

pub(super) fn lock_resource<T>(resource: &Mutex<T>) -> MutexGuard<'_, T> {
    resource.lock().unwrap_or_else(|error| error.into_inner())
}

pub(super) fn share_items<T>(items: Vec<T>) -> Vec<Arc<T>> {
    items.into_iter().map(Arc::new).collect()
}

#[derive(Clone)]
pub(super) struct SharedSearchPage<T> {
    pub(super) items: Vec<Arc<T>>,
    pub(super) has_more: bool,
    pub(super) next_offset: u64,
}

impl<T> From<SearchPage<T>> for SharedSearchPage<T> {
    fn from(page: SearchPage<T>) -> Self {
        Self {
            items: share_items(page.items),
            has_more: page.has_more,
            next_offset: page.next_offset,
        }
    }
}

pub(super) fn append_shared_search_page<T>(target: &mut SharedSearchPage<T>, page: SearchPage<T>) {
    target.items.extend(share_items(page.items));
    target.has_more = page.has_more;
    target.next_offset = page.next_offset;
}

#[derive(Clone)]
pub(super) struct SharedSearchResults {
    pub(super) songs: SharedSearchPage<Track>,
    pub(super) artists: SharedSearchPage<SearchArtist>,
    pub(super) albums: SharedSearchPage<SearchAlbum>,
    pub(super) playlists: SharedSearchPage<UserPlaylist>,
}

impl From<SearchResults> for SharedSearchResults {
    fn from(results: SearchResults) -> Self {
        Self {
            songs: results.songs.into(),
            artists: results.artists.into(),
            albums: results.albums.into(),
            playlists: results.playlists.into(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct SearchVisibleCounts {
    pub(super) songs: usize,
    pub(super) artists: usize,
    pub(super) albums: usize,
    pub(super) playlists: usize,
}

impl Default for SearchVisibleCounts {
    fn default() -> Self {
        Self {
            songs: SEARCH_PAGE_SIZE,
            artists: SEARCH_PAGE_SIZE,
            albums: SEARCH_PAGE_SIZE,
            playlists: SEARCH_PAGE_SIZE,
        }
    }
}

impl SearchVisibleCounts {
    pub(super) fn get(self, category: SearchCategory) -> usize {
        match category {
            SearchCategory::Songs => self.songs,
            SearchCategory::Artists => self.artists,
            SearchCategory::Albums => self.albums,
            SearchCategory::Playlists => self.playlists,
        }
    }

    pub(super) fn get_mut(&mut self, category: SearchCategory) -> &mut usize {
        match category {
            SearchCategory::Songs => &mut self.songs,
            SearchCategory::Artists => &mut self.artists,
            SearchCategory::Albums => &mut self.albums,
            SearchCategory::Playlists => &mut self.playlists,
        }
    }
}

#[derive(Default)]
pub(super) struct SearchResource {
    pub(super) results: Option<SharedSearchResults>,
    pub(super) loading: bool,
    pub(super) loading_more: [bool; 4],
    pub(super) error: Option<String>,
}

#[derive(Default)]
pub(super) struct ArtistResource {
    pub(super) songs: Option<SharedSearchPage<Track>>,
    pub(super) track_count: u64,
    pub(super) songs_loading: bool,
    pub(super) songs_loading_more: bool,
    pub(super) song_error: Option<String>,
    pub(super) albums: Option<SharedSearchPage<SearchAlbum>>,
    pub(super) albums_loading: bool,
    pub(super) albums_loading_more: bool,
    pub(super) album_error: Option<String>,
}

pub(super) struct PlaylistResource {
    pub(super) playlist: UserPlaylist,
    pub(super) tracks: Vec<Arc<Track>>,
    pub(super) has_more: bool,
    pub(super) next_offset: u64,
    pub(super) fetched_at_secs: u64,
    pub(super) loading: bool,
}

impl PlaylistResource {
    pub(super) fn empty(playlist: UserPlaylist) -> Self {
        Self {
            playlist,
            tracks: Vec::new(),
            has_more: true,
            next_offset: 0,
            fetched_at_secs: 0,
            loading: false,
        }
    }

    pub(super) fn is_fresh(&self, now_secs: u64) -> bool {
        matches!(self.playlist.id, UserPlaylistId::Search { .. })
            || now_secs.saturating_sub(self.fetched_at_secs) < LIBRARY_CACHE_TTL.as_secs()
    }

    pub(super) fn apply_page(
        &mut self,
        playlist: UserPlaylist,
        tracks: Vec<Arc<Track>>,
        has_more: bool,
        next_offset: u64,
        offset: u64,
    ) {
        if offset == 0 {
            self.tracks = tracks;
        } else if offset == self.next_offset {
            self.tracks.extend(tracks);
        } else {
            return;
        }
        self.playlist = playlist;
        self.has_more = has_more;
        self.next_offset = next_offset;
        self.fetched_at_secs = unix_timestamp_secs();
    }
}

pub(super) fn update_liked_playlist_resource(
    resource: &SharedPlaylistResource,
    track: &Track,
    liked: bool,
) -> Option<(UserPlaylist, Vec<Arc<Track>>, bool)> {
    let mut state = lock_resource(resource);
    if state.fetched_at_secs == 0 {
        return None;
    }
    let index = state.tracks.iter().position(|item| item.mid == track.mid);
    match (liked, index) {
        (true, None) => state.tracks.insert(0, Arc::new(track.clone())),
        (false, Some(index)) => {
            state.tracks.remove(index);
        }
        _ => return None,
    }
    state.playlist.track_count = if liked {
        state.playlist.track_count.saturating_add(1)
    } else {
        state.playlist.track_count.saturating_sub(1)
    };
    state.next_offset = if liked {
        state.next_offset.saturating_add(1)
    } else {
        state.next_offset.saturating_sub(1)
    };
    Some((state.playlist.clone(), state.tracks.clone(), state.has_more))
}

pub(super) type SharedSearchResource = Arc<Mutex<SearchResource>>;
pub(super) type SharedArtistResource = Arc<Mutex<ArtistResource>>;
pub(super) type SharedPlaylistResource = Arc<Mutex<PlaylistResource>>;

#[derive(Default)]
pub(super) struct PageResourceCache {
    pub(super) searches: HashMap<(u64, String), Weak<Mutex<SearchResource>>>,
    pub(super) artists: HashMap<(u64, String), Weak<Mutex<ArtistResource>>>,
    pub(super) playlists: HashMap<(u64, UserPlaylistId), Weak<Mutex<PlaylistResource>>>,
}

impl PageResourceCache {
    pub(super) fn prune(&mut self) {
        self.searches
            .retain(|_, resource| resource.strong_count() > 0);
        self.artists
            .retain(|_, resource| resource.strong_count() > 0);
        self.playlists
            .retain(|_, resource| resource.strong_count() > 0);
    }
}

pub(super) fn resolved_playlist_scroll_row(
    requested_row: usize,
    track_count: usize,
    has_more: bool,
) -> Option<usize> {
    if requested_row >= track_count && has_more {
        None
    } else {
        Some(requested_row.min(track_count.saturating_sub(1)))
    }
}

#[derive(Clone)]
pub(super) enum NavigationPage {
    Home,
    Settings,
    Search {
        query: String,
        category: SearchCategory,
        visible_counts: SearchVisibleCounts,
        scroll_offsets: [Pixels; 4],
        resource: Option<SharedSearchResource>,
    },
    Artist {
        artist: SearchArtist,
        visible_song_count: usize,
        visible_album_count: usize,
        resource: Option<SharedArtistResource>,
    },
    Playlist {
        playlist: UserPlaylist,
        selected_index: Option<usize>,
        scroll_position: PlaylistScrollPosition,
        resource: Option<SharedPlaylistResource>,
    },
}

impl NavigationPage {
    pub(super) fn same_destination(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Home, Self::Home) => true,
            (Self::Settings, Self::Settings) => true,
            (Self::Search { query, .. }, Self::Search { query: other, .. }) => query == other,
            (Self::Artist { artist, .. }, Self::Artist { artist: other, .. }) => {
                artist.mid == other.mid
            }
            (
                Self::Playlist { playlist, .. },
                Self::Playlist {
                    playlist: other, ..
                },
            ) => playlist.id == other.id,
            _ => false,
        }
    }

    pub(super) fn playlist_resource(
        &self,
        playlist_id: &UserPlaylistId,
    ) -> Option<SharedPlaylistResource> {
        match self {
            Self::Playlist {
                playlist,
                resource: Some(resource),
                ..
            } if playlist.id == *playlist_id => Some(resource.clone()),
            _ => None,
        }
    }
}

pub(super) struct NavigationHistory {
    pub(super) limit: usize,
    pub(super) back: Vec<NavigationPage>,
    pub(super) forward: Vec<NavigationPage>,
}

impl Default for NavigationHistory {
    fn default() -> Self {
        Self::new(DEFAULT_NAVIGATION_HISTORY_LIMIT)
    }
}

impl NavigationHistory {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            back: Vec::new(),
            forward: Vec::new(),
        }
    }

    pub(super) fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
        self.trim();
    }

    pub(super) fn record(&mut self, current: Option<NavigationPage>, target: &NavigationPage) {
        if current
            .as_ref()
            .is_some_and(|current| current.same_destination(target))
        {
            return;
        }
        if let Some(current) = current {
            self.back.push(current);
        }
        self.forward.clear();
        self.trim();
    }

    pub(super) fn go_back(&mut self, current: Option<NavigationPage>) -> Option<NavigationPage> {
        let target = self.back.pop()?;
        if let Some(current) = current {
            self.forward.push(current);
        }
        Some(target)
    }

    pub(super) fn go_forward(&mut self, current: Option<NavigationPage>) -> Option<NavigationPage> {
        let target = self.forward.pop()?;
        if let Some(current) = current {
            self.back.push(current);
        }
        Some(target)
    }

    pub(super) fn clear(&mut self) {
        self.back.clear();
        self.forward.clear();
    }

    pub(super) fn trim(&mut self) {
        while self.back.len() + self.forward.len() + 1 > self.limit {
            if !self.back.is_empty() {
                self.back.remove(0);
            } else if !self.forward.is_empty() {
                self.forward.remove(0);
            }
        }
    }

    pub(super) fn playlist_resources(
        &self,
        playlist_id: &UserPlaylistId,
    ) -> Vec<SharedPlaylistResource> {
        let mut resources = Vec::new();
        for resource in self
            .back
            .iter()
            .chain(&self.forward)
            .filter_map(|page| page.playlist_resource(playlist_id))
        {
            if !resources
                .iter()
                .any(|current| Arc::ptr_eq(current, &resource))
            {
                resources.push(resource);
            }
        }
        resources
    }
}

impl LyruneView {
    pub(super) fn shared_search_resource(&mut self, query: &str) -> SharedSearchResource {
        let account_id = self.account_id().unwrap_or(0);
        let key = (account_id, query.to_owned());
        if let Some(resource) = self
            .page_resource_cache
            .searches
            .get(&key)
            .and_then(Weak::upgrade)
        {
            return resource;
        }
        let resource = Arc::new(Mutex::new(SearchResource::default()));
        self.page_resource_cache
            .searches
            .insert(key, Arc::downgrade(&resource));
        resource
    }

    pub(super) fn shared_artist_resource(&mut self, artist_mid: &str) -> SharedArtistResource {
        let account_id = self.account_id().unwrap_or(0);
        let key = (account_id, artist_mid.to_owned());
        if let Some(resource) = self
            .page_resource_cache
            .artists
            .get(&key)
            .and_then(Weak::upgrade)
        {
            return resource;
        }
        let resource = Arc::new(Mutex::new(ArtistResource::default()));
        self.page_resource_cache
            .artists
            .insert(key, Arc::downgrade(&resource));
        resource
    }

    pub(super) fn shared_playlist_resource(
        &mut self,
        playlist: UserPlaylist,
        force_refresh: bool,
        cache_policy: PlaylistCachePolicy,
    ) -> SharedPlaylistResource {
        let account_id = self.account_id().unwrap_or(0);
        let key = (account_id, playlist.id.clone());
        if !force_refresh
            && let Some(resource) = self
                .page_resource_cache
                .playlists
                .get(&key)
                .and_then(Weak::upgrade)
            && (matches!(cache_policy, PlaylistCachePolicy::AllowStale)
                || lock_resource(&resource).is_fresh(unix_timestamp_secs()))
        {
            return resource;
        }

        let resource = Arc::new(Mutex::new(PlaylistResource::empty(playlist)));
        self.page_resource_cache
            .playlists
            .insert(key, Arc::downgrade(&resource));
        resource
    }

    pub(super) fn prune_page_resources(&mut self) {
        self.page_resource_cache.prune();
    }

    pub(super) fn current_search_scroll_offsets(&self, cx: &App) -> [Pixels; 4] {
        let mut offsets = self.search_scroll_offsets;
        if !self.pending_search_scroll {
            offsets[self.search_category.index()] = match self.search_category {
                SearchCategory::Songs => {
                    self.search_track_table
                        .read(cx)
                        .vertical_scroll_handle
                        .0
                        .borrow()
                        .base_handle
                        .offset()
                        .y
                }
                _ => self.search_card_grid.read(cx).scroll_handle().offset().y,
            };
        }
        offsets
    }

    pub(super) fn current_navigation_page(&self, cx: &App) -> Option<NavigationPage> {
        match self.main_content {
            MainContent::Home => Some(NavigationPage::Home),
            MainContent::Settings => Some(NavigationPage::Settings),
            MainContent::Search => Some(NavigationPage::Search {
                query: self.search_query.clone(),
                category: self.search_category,
                visible_counts: self.search_visible_counts,
                scroll_offsets: self.current_search_scroll_offsets(cx),
                resource: self.search_resource.clone(),
            }),
            MainContent::Artist => {
                self.selected_artist
                    .clone()
                    .map(|artist| NavigationPage::Artist {
                        artist,
                        visible_song_count: self.artist_visible_song_count,
                        visible_album_count: self.artist_visible_album_count,
                        resource: self.artist_resource.clone(),
                    })
            }
            MainContent::Playlist => self.selected_playlist.clone().map(|playlist| {
                let table = self.track_table.read(cx);
                let scroll_position = PlaylistScrollPosition {
                    row: table.visible_range().rows().start,
                    offset_y: table
                        .vertical_scroll_handle
                        .0
                        .borrow()
                        .base_handle
                        .offset()
                        .y,
                };
                NavigationPage::Playlist {
                    playlist,
                    selected_index: self.selected_playlist_index,
                    scroll_position,
                    resource: self.selected_playlist_resource.clone(),
                }
            }),
        }
    }

    pub(super) fn apply_navigation_page(
        &mut self,
        page: NavigationPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(&page, NavigationPage::Search { .. })
            && !self.search_input.read(cx).value().is_empty()
        {
            self.search_input.update(cx, |input, cx| {
                input.set_value(String::new(), window, cx);
            });
        }
        match page {
            NavigationPage::Home => {
                self.main_content = MainContent::Home;
                self.search_resource = None;
                self.artist_resource = None;
                self.selected_playlist_resource = None;
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                if !self.home_loaded && !self.home_loading {
                    self.load_home(cx);
                }
                cx.notify();
            }
            NavigationPage::Settings => {
                self.main_content = MainContent::Settings;
                self.search_resource = None;
                self.artist_resource = None;
                self.selected_playlist_resource = None;
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                cx.notify();
            }
            NavigationPage::Search {
                query,
                category,
                visible_counts,
                scroll_offsets,
                resource,
            } => {
                self.main_content = MainContent::Search;
                self.search_category = category;
                self.search_visible_counts = visible_counts;
                self.search_scroll_offsets = scroll_offsets;
                self.pending_search_scroll = true;
                self.artist_resource = None;
                self.selected_playlist_resource = None;
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                self.search_input.update(cx, |input, cx| {
                    input.set_value(query.clone(), window, cx);
                });
                self.search_query = query.clone();
                let resource = resource.unwrap_or_else(|| self.shared_search_resource(&query));
                self.search_resource = Some(resource.clone());
                self.start_search(resource, query, cx);
            }
            NavigationPage::Artist {
                artist,
                visible_song_count,
                visible_album_count,
                resource,
            } => {
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                self.search_resource = None;
                self.selected_playlist_resource = None;
                self.artist_visible_song_count = visible_song_count;
                self.artist_visible_album_count = visible_album_count;
                let resource =
                    resource.unwrap_or_else(|| self.shared_artist_resource(artist.mid.as_str()));
                self.open_artist(artist, resource, cx);
            }
            NavigationPage::Playlist {
                playlist,
                selected_index,
                scroll_position,
                resource,
            } => {
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(selected_index.map(IndexPath::new), window, cx);
                });
                self.open_playlist(
                    playlist,
                    selected_index,
                    false,
                    PlaylistCachePolicy::AllowStale,
                    scroll_position,
                    resource,
                    cx,
                );
            }
        }
        self.prune_page_resources();
    }

    pub(super) fn record_navigation(
        &mut self,
        current: Option<NavigationPage>,
        target: &NavigationPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let leaving_search = current
            .as_ref()
            .is_some_and(|page| matches!(page, NavigationPage::Search { .. }))
            && !matches!(target, NavigationPage::Search { .. });
        self.navigation_history.record(current, target);
        if leaving_search {
            self.search_input.update(cx, |input, cx| {
                input.set_value(String::new(), window, cx);
            });
        }
    }

    pub(super) fn show_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = NavigationPage::Home;
        let current = self.current_navigation_page(cx);
        self.record_navigation(current, &target, window, cx);
        self.apply_navigation_page(target, window, cx);
    }

    pub(super) fn show_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = NavigationPage::Settings;
        let current = self.current_navigation_page(cx);
        self.record_navigation(current, &target, window, cx);
        self.account_menu_open = false;
        self.apply_navigation_page(target, window, cx);
    }

    pub(super) fn submit_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().trim().to_owned();
        if query.is_empty() {
            return;
        }
        window.blur();
        let resource = self.shared_search_resource(&query);
        let target = NavigationPage::Search {
            query,
            category: SearchCategory::Songs,
            visible_counts: SearchVisibleCounts::default(),
            scroll_offsets: [px(0.); 4],
            resource: Some(resource),
        };
        let current = self.current_navigation_page(cx);
        self.record_navigation(current, &target, window, cx);
        self.apply_navigation_page(target, window, cx);
    }

    pub(super) fn navigate_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_navigation_page(cx);
        if let Some(target) = self.navigation_history.go_back(current) {
            self.apply_navigation_page(target, window, cx);
        }
    }

    pub(super) fn navigate_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_navigation_page(cx);
        if let Some(target) = self.navigation_history.go_forward(current) {
            self.apply_navigation_page(target, window, cx);
        }
    }
}
