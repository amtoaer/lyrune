//! 页面历史保存浏览位置和资源强引用，弱引用索引只负责查找可复用的资源。
//! 共享资源可在离开页面后完成加载；是否刷新当前控件由资源身份决定。

use std::sync::{Arc, Mutex, MutexGuard, Weak};

use gpui::{App, Context, Pixels, Window, px};
use gpui_component::IndexPath;
use hashbrown::HashMap;
use lyrune_api::{
    PlaylistPage, SearchAlbum, SearchArtist, SearchPage, SearchResults, Track, UserPlaylist,
    UserPlaylistId,
};

use crate::icons::MediaIcon;
use crate::settings::DEFAULT_NAVIGATION_HISTORY_LIMIT;

use super::browser::BrowserView;
use super::{LIBRARY_CACHE_TTL, SEARCH_PAGE_SIZE, unix_timestamp_secs};

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

fn append_shared_search_page<T>(target: &mut SharedSearchPage<T>, page: SearchPage<T>) {
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

impl SearchResource {
    pub(super) fn apply_results(&mut self, result: anyhow::Result<SearchResults>) {
        self.loading = false;
        match result {
            Ok(results) => {
                self.results = Some(results.into());
                self.error = None;
            }
            Err(error) => {
                self.results = None;
                self.error = Some(format!("搜索失败：{error:#}"));
            }
        }
    }

    pub(super) fn apply_more_results(
        &mut self,
        category: SearchCategory,
        result: anyhow::Result<SearchMoreResults>,
    ) {
        self.loading_more[category.index()] = false;
        // 不同类别可以同时补页；成功回填不能清除另一个类别刚写入的错误。
        match (self.results.as_mut(), result) {
            (Some(results), Ok(SearchMoreResults::Songs(page))) => {
                append_shared_search_page(&mut results.songs, page)
            }
            (Some(results), Ok(SearchMoreResults::Artists(page))) => {
                append_shared_search_page(&mut results.artists, page)
            }
            (Some(results), Ok(SearchMoreResults::Albums(page))) => {
                append_shared_search_page(&mut results.albums, page)
            }
            (Some(results), Ok(SearchMoreResults::Playlists(page))) => {
                append_shared_search_page(&mut results.playlists, page)
            }
            (_, Err(error)) => {
                self.error = Some(format!("继续加载搜索结果失败：{error:#}"));
            }
            _ => {}
        }
    }
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

impl ArtistResource {
    pub(super) fn apply_songs(&mut self, offset: u64, result: anyhow::Result<PlaylistPage>) {
        self.songs_loading = false;
        self.songs_loading_more = false;
        match result {
            Ok(page) => {
                self.track_count = page.total;
                let page = SearchPage {
                    items: page.tracks,
                    has_more: page.has_more,
                    next_offset: page.next_offset,
                };
                if offset == 0 {
                    self.songs = Some(page.into());
                } else if let Some(songs) = &mut self.songs {
                    append_shared_search_page(songs, page);
                }
            }
            Err(error) => {
                self.song_error = Some(format!("加载歌手歌曲失败：{error:#}"));
            }
        }
    }

    pub(super) fn apply_albums(
        &mut self,
        offset: u64,
        result: anyhow::Result<SearchPage<SearchAlbum>>,
    ) {
        self.albums_loading = false;
        self.albums_loading_more = false;
        match result {
            Ok(page) if offset == 0 => self.albums = Some(page.into()),
            Ok(page) => {
                if let Some(albums) = &mut self.albums {
                    append_shared_search_page(albums, page);
                }
            }
            Err(error) => {
                self.album_error = Some(format!("加载歌手专辑失败：{error:#}"));
            }
        }
    }
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
        // 页面和队列可能同时拿到同一页；只有当前期待的 offset 可以追加，防止重复回填。
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

// 歌单补全也会在 Tokio 线程更新资源；锁保护共享数据，GPUI 控件仍由主线程访问。
pub(super) type SharedSearchResource = Arc<Mutex<SearchResource>>;
pub(super) type SharedArtistResource = Arc<Mutex<ArtistResource>>;
pub(super) type SharedPlaylistResource = Arc<Mutex<PlaylistResource>>;

#[derive(Default)]
pub(super) struct PageResourceCache {
    // 强引用来自当前页、历史记录和未结束的请求；索引本身不延长页面资源的寿命。
    searches: HashMap<(u64, String), Weak<Mutex<SearchResource>>>,
    artists: HashMap<(u64, String), Weak<Mutex<ArtistResource>>>,
    playlists: HashMap<(u64, UserPlaylistId), Weak<Mutex<PlaylistResource>>>,
}

impl PageResourceCache {
    pub(super) fn search(&mut self, account_id: u64, query: &str) -> SharedSearchResource {
        let key = (account_id, query.to_owned());
        if let Some(resource) = self.searches.get(&key).and_then(Weak::upgrade) {
            return resource;
        }
        let resource = Arc::new(Mutex::new(SearchResource::default()));
        self.searches.insert(key, Arc::downgrade(&resource));
        resource
    }

    pub(super) fn artist(&mut self, account_id: u64, artist_mid: &str) -> SharedArtistResource {
        let key = (account_id, artist_mid.to_owned());
        if let Some(resource) = self.artists.get(&key).and_then(Weak::upgrade) {
            return resource;
        }
        let resource = Arc::new(Mutex::new(ArtistResource::default()));
        self.artists.insert(key, Arc::downgrade(&resource));
        resource
    }

    pub(super) fn playlist(
        &mut self,
        account_id: u64,
        playlist: UserPlaylist,
        force_refresh: bool,
        cache_policy: PlaylistCachePolicy,
    ) -> SharedPlaylistResource {
        let key = (account_id, playlist.id.clone());
        if !force_refresh
            && let Some(resource) = self.playlists.get(&key).and_then(Weak::upgrade)
            && (matches!(cache_policy, PlaylistCachePolicy::AllowStale)
                || lock_resource(&resource).is_fresh(unix_timestamp_secs()))
        {
            return resource;
        }

        let resource = Arc::new(Mutex::new(PlaylistResource::empty(playlist)));
        self.playlists.insert(key, Arc::downgrade(&resource));
        resource
    }

    pub(super) fn cached_playlist(
        &self,
        account_id: u64,
        id: &UserPlaylistId,
    ) -> Option<SharedPlaylistResource> {
        self.playlists
            .get(&(account_id, id.clone()))
            .and_then(Weak::upgrade)
    }

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
        // 调用方先取得 target 的资源再记录导航；清空 forward 可能释放资源的最后一个旧持有者。
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

impl BrowserView {
    pub(super) fn current_search_scroll_offsets(&self, cx: &App) -> [Pixels; 4] {
        let mut offsets = self.search.scroll_offsets;
        if !self.search.pending_scroll {
            offsets[self.search.category.index()] = match self.search.category {
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
                query: self.search.query.clone(),
                category: self.search.category,
                visible_counts: self.search.visible_counts,
                scroll_offsets: self.current_search_scroll_offsets(cx),
                resource: self.search.resource.clone(),
            }),
            MainContent::Artist => {
                self.artist
                    .selected
                    .clone()
                    .map(|artist| NavigationPage::Artist {
                        artist,
                        visible_song_count: self.artist.visible_song_count,
                        visible_album_count: self.artist.visible_album_count,
                        resource: self.artist.resource.clone(),
                    })
            }
            MainContent::Playlist => self.playlist.selected.clone().map(|playlist| {
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
                    selected_index: self.playlist.selected_index,
                    scroll_position,
                    resource: self.playlist.resource.clone(),
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
                self.search.resource = None;
                self.artist.resource = None;
                self.playlist.resource = None;
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                if !self.home.loaded && !self.home.loading {
                    self.load_home(cx);
                }
                cx.notify();
            }
            NavigationPage::Settings => {
                self.main_content = MainContent::Settings;
                self.search.resource = None;
                self.artist.resource = None;
                self.playlist.resource = None;
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
                self.search.category = category;
                self.search.visible_counts = visible_counts;
                self.search.scroll_offsets = scroll_offsets;
                self.search.pending_scroll = true;
                self.artist.resource = None;
                self.playlist.resource = None;
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                self.search_input.update(cx, |input, cx| {
                    input.set_value(query.clone(), window, cx);
                });
                self.search.query = query.clone();
                let resource = resource.unwrap_or_else(|| {
                    self.page_resource_cache
                        .search(self.account.read(cx).id().unwrap_or(0), &query)
                });
                self.search.resource = Some(resource.clone());
                self.search.pending_results_sync = true;
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
                self.search.resource = None;
                self.playlist.resource = None;
                self.artist.visible_song_count = visible_song_count;
                self.artist.visible_album_count = visible_album_count;
                let resource = resource.unwrap_or_else(|| {
                    self.page_resource_cache
                        .artist(self.account.read(cx).id().unwrap_or(0), artist.mid.as_str())
                });
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
        self.page_resource_cache.prune();
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
        cx.emit(super::browser::BrowserEvent::CloseAccountMenu);
        self.apply_navigation_page(target, window, cx);
    }

    pub(super) fn submit_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().trim().to_owned();
        if query.is_empty() {
            return;
        }
        window.blur(cx);
        let resource = self
            .page_resource_cache
            .search(self.account.read(cx).id().unwrap_or(0), &query);
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
