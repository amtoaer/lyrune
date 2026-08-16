use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IndexPath, ResizableState, Selectable as _, Sizable as _,
    StyledExt as _,
    avatar::Avatar,
    button::{Button, ButtonVariants as _},
    h_flex, h_resizable,
    input::{Input, InputEvent, InputState},
    list::{List, ListEvent, ListState},
    resizable_panel,
    scroll::ScrollableElement as _,
    slider::{Slider, SliderEvent, SliderState},
    spinner::Spinner,
    table::{DataTable, TableEvent, TableState},
    v_flex,
};
use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;

use crate::cache::AudioCache;
use crate::credentials::CredentialStore;
use crate::design::{self, ColorTheme};
use crate::http::cached_image_source;
use crate::icons::{MediaIcon, lyrune_icon, media_icon, media_icon_hsla};
use crate::library::{PlaylistListDelegate, TrackTableDelegate, format_duration, playlist_cover};
#[cfg(target_os = "linux")]
use crate::mpris::{
    MprisCommand, MprisHandle, MprisLoopStatus, MprisPlaybackStatus, MprisSnapshot, MprisTrack,
};
use crate::player::{AudioPlayer, PreparedPlayback};
use crate::settings::{
    AppSettings, CdnCacheStore, LibraryCache, LibraryCacheStore, PersistedLibraryView,
    PersistedPlayback, PersistedQueueContinuation, PersistedWindowSize, SettingsStore,
};
use crate::singleflight::SingleFlight;
use qqmusic_api::integration::{
    LoginEvent, PlaylistPage, ProtocolClient, QqCredential, Quality, RecommendationKind,
    SearchAlbum, SearchArtist, SearchPage, SearchResults, Track, UserPlaylist, UserPlaylistId,
    UserProfile, refresh_credential, run_qr_login,
};
#[cfg(target_os = "linux")]
use xxhash_rust::xxh3::xxh3_128;

const PAGE_SIZE: u64 = 100;
const ARTIST_PAGE_SIZE: u64 = 5;
const PROGRESS_TICK: Duration = Duration::from_millis(250);
const PLAYBACK_PERSIST_INTERVAL: Duration = Duration::from_secs(5);
const CDN_REFRESH_RETRY: Duration = Duration::from_secs(60);
const LIBRARY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

fn progress_slider_state(value: f32) -> SliderState {
    SliderState::new()
        .min(0.)
        .max(1.)
        .step(0.001)
        .default_value(value)
}

fn volume_slider_state(value: f32) -> SliderState {
    SliderState::new()
        .min(0.)
        .max(1.)
        .step(0.01)
        .default_value(value)
}

fn progress_fraction(position: Duration, duration: Duration) -> f32 {
    if duration.is_zero() {
        0.
    } else {
        (position.as_secs_f32() / duration.as_secs_f32()).clamp(0., 1.)
    }
}

fn single_line_summary(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(target_os = "linux")]
fn duration_micros(duration: Duration) -> i64 {
    duration.as_micros().min(i64::MAX as u128) as i64
}

#[cfg(target_os = "linux")]
fn mpris_track_id(track_mid: &str) -> String {
    format!(
        "/dev/lyrune/track/id_{:032x}",
        xxh3_128(track_mid.as_bytes())
    )
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn new_cache_revision() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

pub(crate) static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .thread_keep_alive(Duration::from_secs(2))
        .enable_all()
        .thread_name("lyrune-worker")
        .build()
        .expect("create Lyrune Tokio runtime")
});

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccountState {
    Restoring,
    SignedOut,
    SigningIn,
    SignedIn,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MainContent {
    Home,
    Search,
    Artist,
    Playlist,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchCategory {
    Songs,
    Playlists,
    Albums,
    Artists,
}

impl SearchCategory {
    const ALL: [Self; 4] = [Self::Songs, Self::Playlists, Self::Albums, Self::Artists];

    fn label(self) -> &'static str {
        match self {
            Self::Songs => "单曲",
            Self::Playlists => "歌单",
            Self::Albums => "专辑",
            Self::Artists => "歌手",
        }
    }

    fn icon(self) -> MediaIcon {
        match self {
            Self::Songs => MediaIcon::Music,
            Self::Artists => MediaIcon::Artist,
            Self::Albums => MediaIcon::Album,
            Self::Playlists => MediaIcon::Playlist,
        }
    }
}

enum SearchMoreResults {
    Songs(SearchPage<Track>),
    Artists(SearchPage<SearchArtist>),
    Albums(SearchPage<SearchAlbum>),
    Playlists(SearchPage<UserPlaylist>),
}

#[derive(Clone, Copy)]
enum SongRowSource {
    Search,
    Artist,
}

fn append_search_page<T>(target: &mut SearchPage<T>, mut page: SearchPage<T>) {
    target.items.append(&mut page.items);
    target.has_more = page.has_more;
    target.next_offset = page.next_offset;
}

fn insert_track_after_current(
    tracks: &mut Vec<Track>,
    current_index: Option<usize>,
    track: Track,
) -> usize {
    let mut current_index = current_index.filter(|index| *index < tracks.len());
    if let Some(existing_index) = tracks.iter().position(|item| item.mid == track.mid) {
        if current_index == Some(existing_index) {
            return existing_index;
        }
        tracks.remove(existing_index);
        if let Some(index) = &mut current_index
            && existing_index < *index
        {
            *index -= 1;
        }
    }
    let insert_index = current_index.map_or(tracks.len(), |index| index + 1);
    tracks.insert(insert_index, track);
    insert_index
}

#[derive(Clone)]
enum NavigationPage {
    Home,
    Search {
        query: String,
        category: SearchCategory,
    },
    Artist {
        artist: SearchArtist,
    },
    Playlist {
        playlist: UserPlaylist,
        selected_index: Option<usize>,
    },
}

impl NavigationPage {
    fn same_destination(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Home, Self::Home) => true,
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
}

#[derive(Default)]
struct NavigationHistory {
    back: Vec<NavigationPage>,
    forward: Vec<NavigationPage>,
}

impl NavigationHistory {
    fn record(&mut self, current: Option<NavigationPage>, target: &NavigationPage) {
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
    }

    fn go_back(&mut self, current: Option<NavigationPage>) -> Option<NavigationPage> {
        let target = self.back.pop()?;
        if let Some(current) = current {
            self.forward.push(current);
        }
        Some(target)
    }

    fn go_forward(&mut self, current: Option<NavigationPage>) -> Option<NavigationPage> {
        let target = self.forward.pop()?;
        if let Some(current) = current {
            self.back.push(current);
        }
        Some(target)
    }

    fn clear(&mut self) {
        self.back.clear();
        self.forward.clear();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RepeatMode {
    Off,
    All,
    One,
}

#[derive(Clone)]
struct PlaybackLocation {
    track_mid: String,
    quality: Quality,
    urls: Vec<String>,
}

struct PlaybackQueue {
    playlist_id: UserPlaylistId,
    tracks: Vec<Track>,
    continuation: Option<PersistedQueueContinuation>,
}

impl PersistedQueueContinuation {
    fn can_load_more(self) -> bool {
        match self {
            Self::Radar { has_more, .. } => has_more,
            Self::Guess => true,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PlaylistPageKey {
    account_id: u64,
    playlist_id: UserPlaylistId,
    offset: u64,
}

async fn request_playlist_page(
    requests: SingleFlight<PlaylistPageKey, PlaylistPage>,
    client: ProtocolClient,
    credential: QqCredential,
    playlist: UserPlaylist,
    offset: u64,
    force: bool,
) -> anyhow::Result<PlaylistPage> {
    let key = PlaylistPageKey {
        account_id: credential.music_id,
        playlist_id: playlist.id.clone(),
        offset,
    };
    requests
        .run(key, force, move || async move {
            tokio::time::timeout(
                Duration::from_secs(30),
                client.playlist_page(&credential, &playlist, offset, PAGE_SIZE),
            )
            .await
            .context("QQ 音乐歌单分页请求等待超过 30 秒")?
        })
        .await
}

enum PlaybackLoadEvent {
    ResolvingOptions,
    Options(Vec<Quality>),
    Finished(anyhow::Result<(PreparedPlayback, PlaybackLocation, Vec<Quality>)>),
}

struct StatusMessage {
    text: String,
    is_error: bool,
}

impl StatusMessage {
    fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

impl RepeatMode {
    fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Off => "循环",
            Self::All => "列表循环",
            Self::One => "单曲循环",
        }
    }
}

pub struct LyruneView {
    account_state: AccountState,
    credential: Option<QqCredential>,
    profile: Option<UserProfile>,
    qr_image: Option<Arc<Image>>,
    library_loading: bool,
    selected_playlist_index: Option<usize>,
    selected_playlist: Option<UserPlaylist>,
    page_offset: u64,
    page_loading: bool,
    library_generation: u64,
    playlist_generation: u64,
    playlist_force_refresh: bool,
    playlist_cache_revision: u64,
    playlist_page_requests: SingleFlight<PlaylistPageKey, PlaylistPage>,
    main_content: MainContent,
    navigation_history: NavigationHistory,
    home_playlists: Vec<UserPlaylist>,
    home_loading: bool,
    home_loaded: bool,
    home_error: Option<String>,
    home_generation: u64,
    home_recommendation_loading: Option<RecommendationKind>,
    search_query: String,
    search_results: Option<SearchResults>,
    search_category: SearchCategory,
    search_loading: bool,
    search_loading_more: bool,
    search_error: Option<String>,
    search_generation: u64,
    selected_artist: Option<SearchArtist>,
    artist_songs: Option<SearchPage<Track>>,
    artist_track_count: u64,
    artist_songs_loading: bool,
    artist_songs_loading_more: bool,
    artist_song_error: Option<String>,
    artist_albums: Option<SearchPage<SearchAlbum>>,
    artist_albums_loading: bool,
    artist_albums_loading_more: bool,
    artist_album_error: Option<String>,
    artist_generation: u64,

    playlist_list: Entity<ListState<PlaylistListDelegate>>,
    track_table: Entity<TableState<TrackTableDelegate>>,
    search_input: Entity<InputState>,
    progress_slider: Entity<SliderState>,
    volume_slider: Entity<SliderState>,

    audio: Option<AudioPlayer>,
    audio_cache: Option<AudioCache>,
    protocol_client: Option<ProtocolClient>,
    cdn_maintenance: Option<JoinHandle<()>>,
    playback_queue: Option<PlaybackQueue>,
    queue_generation: u64,
    queue_recommendation_loading: bool,
    queue_waiting_for_recommendation: bool,
    current_track: Option<usize>,
    loading_track: Option<usize>,
    loading_autoplay: bool,
    resolving_qualities: bool,
    playback_started: bool,
    playback_location: Option<PlaybackLocation>,
    active_quality: Quality,
    available_qualities: Vec<Quality>,
    quality_menu_open: bool,
    position: Duration,
    seek_preview: Option<Duration>,
    settings: AppSettings,
    library_cache: LibraryCache,
    library_cache_saves: async_channel::Sender<LibraryCache>,
    library_cache_writer: Option<JoinHandle<()>>,
    shuffle: bool,
    repeat_mode: RepeatMode,
    pending_playback_restore: Option<PersistedPlayback>,
    last_playback_persisted_at: Instant,

    status: StatusMessage,
    login_generation: u64,
    play_generation: u64,
    account_menu_open: bool,
    _subscriptions: Vec<Subscription>,
    _window_subscription: Option<Subscription>,
    #[cfg(target_os = "linux")]
    mpris: Option<MprisHandle>,
}

impl LyruneView {
    pub fn new(window: &mut Window, settings: AppSettings, cx: &mut Context<Self>) -> Self {
        let (audio, mut initial_status, mut initial_status_is_error) = match AudioPlayer::new() {
            Ok(player) => {
                player.set_volume(settings.volume);
                (Some(player), "正在读取已保存的登录状态…".to_owned(), false)
            }
            Err(error) => (
                None,
                format!("音频设备初始化失败：{error:#}；仍可浏览 QQ 音乐歌单"),
                true,
            ),
        };
        let audio_cache = match AudioCache::new() {
            Ok(cache) => Some(cache),
            Err(error) => {
                initial_status = format!("{initial_status}；音频缓存初始化失败：{error:#}");
                initial_status_is_error = true;
                None
            }
        };
        let cdn_cache = match CdnCacheStore::load() {
            Ok(cache) => cache,
            Err(error) => {
                initial_status = format!("{initial_status}；CDN 缓存读取失败：{error:#}");
                initial_status_is_error = true;
                Default::default()
            }
        };
        let protocol_client = match ProtocolClient::new_with_cdn_cache(cdn_cache) {
            Ok(client) => Some(client),
            Err(error) => {
                initial_status = format!("{initial_status}；QQ 音乐客户端初始化失败：{error:#}");
                initial_status_is_error = true;
                None
            }
        };
        let playback_quality = settings.playback_quality;
        let pending_playback_restore = settings.current_playback.clone();
        let library_cache = LibraryCacheStore::load().unwrap_or_default();
        let (library_cache_saves, library_cache_save_receiver) = async_channel::unbounded();
        let library_cache_writer = RUNTIME.spawn(async move {
            while let Ok(mut cache) = library_cache_save_receiver.recv().await {
                while let Ok(newer) = library_cache_save_receiver.try_recv() {
                    cache = newer;
                }
                let _ = tokio::task::spawn_blocking(move || LibraryCacheStore::save(&cache)).await;
            }
        });

        let playlist_list =
            cx.new(|cx| ListState::new(PlaylistListDelegate::new(), window, cx).searchable(false));
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("想播放什么？")
                .context_menu(false)
        });
        let (load_more_sender, load_more_receiver) = async_channel::bounded(1);
        let track_table = cx.new(|cx| {
            TableState::new(TrackTableDelegate::new(load_more_sender), window, cx)
                .col_selectable(false)
                .col_movable(false)
                .sortable(false)
        });
        let progress_slider = cx.new(|_| progress_slider_state(0.));
        let volume_slider = cx.new(|_| volume_slider_state(settings.volume));

        let subscriptions = vec![
            cx.subscribe(&playlist_list, |this, _, event: &ListEvent, cx| {
                if let ListEvent::Select(index) | ListEvent::Confirm(index) = event {
                    this.select_playlist(index.row, cx);
                }
            }),
            cx.subscribe(&track_table, |this, _, event: &TableEvent, cx| {
                if let TableEvent::DoubleClickedRow(index) = event {
                    this.select_track(*index, cx);
                }
            }),
            cx.subscribe_in(
                &search_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.submit_search(window, cx);
                    }
                },
            ),
            cx.subscribe(
                &progress_slider,
                |this, _, event: &SliderEvent, cx| match event {
                    SliderEvent::Change(value) => {
                        this.seek_preview = this
                            .current_duration()
                            .map(|duration| duration.mul_f32(value.end().clamp(0., 1.)));
                        cx.notify();
                    }
                    SliderEvent::Release(value) => {
                        let target = this
                            .current_duration()
                            .map(|duration| duration.mul_f32(value.end().clamp(0., 1.)));
                        this.seek_preview = None;
                        if let Some(target) = target {
                            this.seek_to(target, cx);
                        }
                    }
                },
            ),
            cx.subscribe(
                &volume_slider,
                |this, _, event: &SliderEvent, cx| match event {
                    SliderEvent::Change(value) => this.set_volume(value.end(), cx),
                    SliderEvent::Release(value) => {
                        this.set_volume(value.end(), cx);
                        this.persist_settings();
                    }
                },
            ),
        ];

        cx.spawn(async move |this, cx| {
            while load_more_receiver.recv().await.is_ok() {
                if this
                    .update(cx, |this, cx| this.load_playlist_page(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let mut view = Self {
            account_state: AccountState::Restoring,
            credential: None,
            profile: None,
            qr_image: None,
            library_loading: false,
            selected_playlist_index: None,
            selected_playlist: None,
            page_offset: 0,
            page_loading: false,
            library_generation: 0,
            playlist_generation: 0,
            playlist_force_refresh: false,
            playlist_cache_revision: 0,
            playlist_page_requests: SingleFlight::default(),
            main_content: MainContent::Playlist,
            navigation_history: NavigationHistory::default(),
            home_playlists: Vec::new(),
            home_loading: false,
            home_loaded: false,
            home_error: None,
            home_generation: 0,
            home_recommendation_loading: None,
            search_query: String::new(),
            search_results: None,
            search_category: SearchCategory::Songs,
            search_loading: false,
            search_loading_more: false,
            search_error: None,
            search_generation: 0,
            selected_artist: None,
            artist_songs: None,
            artist_track_count: 0,
            artist_songs_loading: false,
            artist_songs_loading_more: false,
            artist_song_error: None,
            artist_albums: None,
            artist_albums_loading: false,
            artist_albums_loading_more: false,
            artist_album_error: None,
            artist_generation: 0,
            playlist_list,
            track_table,
            search_input,
            progress_slider,
            volume_slider,
            audio,
            audio_cache,
            protocol_client,
            cdn_maintenance: None,
            playback_queue: None,
            queue_generation: 0,
            queue_recommendation_loading: false,
            queue_waiting_for_recommendation: false,
            current_track: None,
            loading_track: None,
            loading_autoplay: false,
            resolving_qualities: false,
            playback_started: false,
            playback_location: None,
            active_quality: playback_quality,
            available_qualities: Vec::new(),
            quality_menu_open: false,
            position: Duration::ZERO,
            seek_preview: None,
            settings,
            library_cache,
            library_cache_saves,
            library_cache_writer: Some(library_cache_writer),
            shuffle: false,
            repeat_mode: RepeatMode::Off,
            pending_playback_restore,
            last_playback_persisted_at: Instant::now(),
            status: if initial_status_is_error {
                StatusMessage::error(initial_status)
            } else {
                StatusMessage::info(initial_status)
            },
            login_generation: 0,
            play_generation: 0,
            account_menu_open: false,
            _subscriptions: subscriptions,
            _window_subscription: None,
            #[cfg(target_os = "linux")]
            mpris: None,
        };
        view.attach_window(window, cx);
        view.start_cdn_maintenance();
        view.restore_credential(cx);
        view
    }

    pub(crate) fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._window_subscription = Some(cx.observe_window_bounds(window, |this, window, _| {
            let size = window.window_bounds().get_bounds().size;
            let width = f32::from(size.width).round() as u32;
            let height = f32::from(size.height).round() as u32;
            if width > 0 && height > 0 {
                this.settings.window_size = Some(PersistedWindowSize { width, height });
            }
        }));
        self.sync_progress_slider(window, cx);
        self.start_window_tick(window, cx);
    }

    fn start_window_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_TICK).await;
                if this
                    .update_in(cx, |this, window, cx| this.sync_progress_slider(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn start_background_tick(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_TICK).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn window_size(&self) -> Option<PersistedWindowSize> {
        self.settings.window_size
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn attach_mpris(&mut self, mpris: MprisHandle) {
        self.mpris = Some(mpris);
        self.sync_mpris(false);
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn handle_mpris_command(&mut self, command: MprisCommand, cx: &mut Context<Self>) {
        match command {
            MprisCommand::Raise | MprisCommand::Quit => {}
            MprisCommand::Next => self.play_next(false, cx),
            MprisCommand::Previous => self.play_previous(cx),
            MprisCommand::Pause => self.pause_playback(cx),
            MprisCommand::PlayPause => self.toggle_playback(cx),
            MprisCommand::Stop => self.stop_playback(cx),
            MprisCommand::Play => self.play(cx),
            MprisCommand::Seek(offset) => self.seek_by(offset, cx),
            MprisCommand::SetPosition { track_id, position } => {
                self.set_mpris_position(&track_id, position, cx);
            }
            MprisCommand::SetLoopStatus(status) => {
                self.repeat_mode = match status {
                    MprisLoopStatus::None => RepeatMode::Off,
                    MprisLoopStatus::Track => RepeatMode::One,
                    MprisLoopStatus::Playlist => RepeatMode::All,
                };
                self.sync_mpris(false);
                cx.notify();
            }
            MprisCommand::SetShuffle(shuffle) => {
                self.shuffle = shuffle;
                self.sync_mpris(false);
                cx.notify();
            }
            MprisCommand::SetVolume(volume) => {
                let volume = volume.clamp(0., 1.) as f32;
                self.volume_slider.update(cx, |slider, cx| {
                    *slider = volume_slider_state(volume);
                    cx.notify();
                });
                self.set_volume(volume, cx);
                self.persist_settings();
            }
        }
    }

    fn restore_credential(&mut self, cx: &mut Context<Self>) {
        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = async {
                let stored = tokio::task::spawn_blocking(CredentialStore::load)
                    .await
                    .context("读取凭据任务异常退出")??;
                match stored {
                    Some(credential) => refresh_credential(credential).await.map(Some),
                    None => Ok(None),
                }
            }
            .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| match result {
                Ok(Some(credential)) => {
                    this.account_state = AccountState::SignedIn;
                    this.credential = Some(credential.clone());
                    this.status = StatusMessage::info("已恢复 QQ 音乐登录，正在加载音乐库…");
                    this.persist_credential(credential.clone(), cx);
                    this.load_library(false, cx);
                }
                Ok(None) => {
                    this.account_state = AccountState::SignedOut;
                    this.begin_login(cx);
                }
                Err(error) => {
                    this.account_state = AccountState::SignedOut;
                    this.status = StatusMessage::error(format!(
                        "无法恢复登录：{error:#}；正在加载登录二维码…"
                    ));
                    this.begin_login(cx);
                }
            });
        })
        .detach();
    }

    fn begin_login(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.account_state,
            AccountState::Restoring | AccountState::SigningIn
        ) {
            return;
        }

        self.login_generation = self.login_generation.wrapping_add(1);
        let generation = self.login_generation;
        self.account_state = AccountState::SigningIn;
        self.qr_image = None;
        self.status = StatusMessage::info("正在向 QQ 音乐申请二维码…");
        cx.notify();

        let (sender, receiver) = async_channel::unbounded();
        drop(RUNTIME.spawn(run_qr_login(sender)));
        cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                let completed = matches!(
                    event,
                    LoginEvent::Succeeded(_) | LoginEvent::Expired | LoginEvent::Failed(_)
                );
                let _ = this.update(cx, |this, cx| {
                    if this.login_generation == generation {
                        this.handle_login_event(event, cx);
                    }
                });
                if completed {
                    break;
                }
            }
        })
        .detach();
    }

    fn handle_login_event(&mut self, event: LoginEvent, cx: &mut Context<Self>) {
        match event {
            LoginEvent::QrReady(png) => {
                self.qr_image = Some(Arc::new(Image::from_bytes(ImageFormat::Png, png)));
                self.status = StatusMessage::info("请使用 QQ 音乐 App 扫描二维码");
            }
            LoginEvent::WaitingScan => self.status = StatusMessage::info("等待扫码…"),
            LoginEvent::WaitingConfirm => {
                self.status = StatusMessage::info("已扫码，请在手机上确认登录");
            }
            LoginEvent::Succeeded(credential) => {
                self.account_state = AccountState::SignedIn;
                self.qr_image = None;
                self.credential = Some(credential.clone());
                self.status = StatusMessage::info("登录成功，正在加载音乐库…");
                self.persist_credential(credential.clone(), cx);
                self.load_library(false, cx);
            }
            LoginEvent::Expired => {
                self.account_state = AccountState::SignedOut;
                self.qr_image = None;
                self.begin_login(cx);
            }
            LoginEvent::Failed(error) => {
                self.account_state = AccountState::SignedOut;
                self.qr_image = None;
                self.status =
                    StatusMessage::error(format!("扫码登录失败：{error}；点击二维码区域重试"));
            }
        }
        cx.notify();
    }

    fn persist_credential(&self, credential: QqCredential, cx: &mut Context<Self>) {
        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = tokio::task::spawn_blocking(move || CredentialStore::save(&credential))
                .await
                .context("保存凭据任务异常退出")
                .and_then(|result| result);
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(Err(error)) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.status = StatusMessage::error(format!(
                    "登录成功，但凭据未能保存到系统钥匙串：{error:#}"
                ));
                cx.notify();
            });
        })
        .detach();
    }

    fn start_cdn_maintenance(&mut self) {
        if let Some(task) = self.cdn_maintenance.take() {
            task.abort();
        }
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        self.cdn_maintenance = Some(RUNTIME.spawn(async move {
            loop {
                let delay = client.cdn_refresh_delay().await;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                match client.refresh_cdn().await {
                    Ok(cache) => {
                        let _ =
                            tokio::task::spawn_blocking(move || CdnCacheStore::save(&cache)).await;
                    }
                    Err(_) => tokio::time::sleep(CDN_REFRESH_RETRY).await,
                }
            }
        }));
    }

    fn current_navigation_page(&self) -> Option<NavigationPage> {
        match self.main_content {
            MainContent::Home => Some(NavigationPage::Home),
            MainContent::Search => Some(NavigationPage::Search {
                query: self.search_query.clone(),
                category: self.search_category,
            }),
            MainContent::Artist => self
                .selected_artist
                .clone()
                .map(|artist| NavigationPage::Artist { artist }),
            MainContent::Playlist => {
                self.selected_playlist
                    .clone()
                    .map(|playlist| NavigationPage::Playlist {
                        playlist,
                        selected_index: self.selected_playlist_index,
                    })
            }
        }
    }

    fn apply_navigation_page(
        &mut self,
        page: NavigationPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match page {
            NavigationPage::Home => {
                self.main_content = MainContent::Home;
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                if !self.home_loaded && !self.home_loading {
                    self.load_home(cx);
                }
                cx.notify();
            }
            NavigationPage::Search { query, category } => {
                self.main_content = MainContent::Search;
                self.search_category = category;
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                self.search_input.update(cx, |input, cx| {
                    input.set_value(query.clone(), window, cx);
                });
                if self.search_query != query || self.search_results.is_none() {
                    self.start_search(query, cx);
                } else {
                    cx.notify();
                }
            }
            NavigationPage::Artist { artist } => {
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                });
                self.open_artist(artist, cx);
            }
            NavigationPage::Playlist {
                playlist,
                selected_index,
            } => {
                self.playlist_list.update(cx, |list, cx| {
                    list.set_selected_index(selected_index.map(IndexPath::new), window, cx);
                });
                self.open_playlist(playlist, selected_index, false, cx);
            }
        }
    }

    fn show_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = NavigationPage::Home;
        let current = self.current_navigation_page();
        self.navigation_history.record(current, &target);
        self.apply_navigation_page(target, window, cx);
    }

    fn submit_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().trim().to_owned();
        if query.is_empty() {
            return;
        }
        window.blur();
        let target = NavigationPage::Search {
            query,
            category: SearchCategory::Songs,
        };
        let current = self.current_navigation_page();
        self.navigation_history.record(current, &target);
        self.apply_navigation_page(target, window, cx);
    }

    fn start_search(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(credential) = self.credential.clone() else {
            self.search_error = Some("请先登录 QQ 音乐".to_owned());
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.search_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        self.search_query = query.clone();
        self.search_results = None;
        self.search_category = SearchCategory::Songs;
        self.search_loading = true;
        self.search_loading_more = false;
        self.search_error = None;
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                client.search(&credential, &query, 20),
            )
            .await
            .context("QQ 音乐搜索等待超过 30 秒")
            .and_then(|result| result);
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.search_generation != generation {
                    return;
                }
                this.search_loading = false;
                match result {
                    Ok(results) => {
                        this.search_results = Some(results);
                        this.search_error = None;
                    }
                    Err(error) => {
                        this.search_results = None;
                        this.search_error = Some(format!("搜索失败：{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_more_search(&mut self, cx: &mut Context<Self>) {
        if self.search_loading || self.search_loading_more {
            return;
        }
        let Some(results) = self.search_results.as_ref() else {
            return;
        };
        let (offset, has_more) = match self.search_category {
            SearchCategory::Songs => (results.songs.next_offset, results.songs.has_more),
            SearchCategory::Artists => (results.artists.next_offset, results.artists.has_more),
            SearchCategory::Albums => (results.albums.next_offset, results.albums.has_more),
            SearchCategory::Playlists => {
                (results.playlists.next_offset, results.playlists.has_more)
            }
        };
        if !has_more {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        let query = self.search_query.clone();
        let category = self.search_category;
        let generation = self.search_generation;
        self.search_loading_more = true;
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = match category {
                SearchCategory::Songs => client
                    .search_songs(&credential, &query, offset, 20)
                    .await
                    .map(SearchMoreResults::Songs),
                SearchCategory::Artists => client
                    .search_artists(&credential, &query, offset, 20)
                    .await
                    .map(SearchMoreResults::Artists),
                SearchCategory::Albums => client
                    .search_albums(&credential, &query, offset, 20)
                    .await
                    .map(SearchMoreResults::Albums),
                SearchCategory::Playlists => client
                    .search_playlists(&credential, &query, offset, 20)
                    .await
                    .map(SearchMoreResults::Playlists),
            };
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.search_generation != generation {
                    return;
                }
                this.search_loading_more = false;
                if this.search_category != category {
                    cx.notify();
                    return;
                }
                match (this.search_results.as_mut(), result) {
                    (Some(results), Ok(SearchMoreResults::Songs(page))) => {
                        append_search_page(&mut results.songs, page)
                    }
                    (Some(results), Ok(SearchMoreResults::Artists(page))) => {
                        append_search_page(&mut results.artists, page)
                    }
                    (Some(results), Ok(SearchMoreResults::Albums(page))) => {
                        append_search_page(&mut results.albums, page)
                    }
                    (Some(results), Ok(SearchMoreResults::Playlists(page))) => {
                        append_search_page(&mut results.playlists, page)
                    }
                    (_, Err(error)) => {
                        this.search_error = Some(format!("继续加载搜索结果失败：{error:#}"));
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_search_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(results) = self.search_results.as_ref() else {
            return;
        };
        let Some(track) = results.songs.items.get(index).cloned() else {
            return;
        };
        if self
            .current_track_data()
            .is_some_and(|current| current.mid == track.mid)
        {
            if self.loading_track.is_none() {
                self.toggle_playback(cx);
            }
            return;
        }

        self.pending_playback_restore = None;
        self.home_recommendation_loading = None;
        let current_index = self.current_track;
        let queue_index = if let Some(queue) = &mut self.playback_queue {
            insert_track_after_current(&mut queue.tracks, current_index, track)
        } else {
            self.playback_queue = Some(PlaybackQueue {
                playlist_id: UserPlaylistId::Search {
                    query: self.search_query.clone(),
                },
                tracks: vec![track],
                continuation: None,
            });
            0
        };
        self.start_playback(queue_index, Duration::ZERO, None, true, cx);
    }

    fn navigate_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_navigation_page();
        if let Some(target) = self.navigation_history.go_back(current) {
            self.apply_navigation_page(target, window, cx);
        }
    }

    fn navigate_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_navigation_page();
        if let Some(target) = self.navigation_history.go_forward(current) {
            self.apply_navigation_page(target, window, cx);
        }
    }

    fn load_home(&mut self, cx: &mut Context<Self>) {
        if self.home_loading {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            self.home_error = Some("请先登录 QQ 音乐".to_owned());
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.home_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        self.home_generation = self.home_generation.wrapping_add(1);
        let generation = self.home_generation;
        self.home_loading = true;
        self.home_error = None;
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                client.recommended_playlists(&credential, 0, 20),
            )
            .await
            .context("QQ 音乐主页请求等待超过 30 秒")
            .and_then(|result| result);
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.home_generation != generation {
                    return;
                }
                this.home_loading = false;
                match result {
                    Ok(page) => {
                        this.home_loaded = true;
                        this.home_playlists = page.items;
                        this.home_error = None;
                    }
                    Err(error) => {
                        this.home_error = Some(format!("加载主页推荐失败：{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_home_recommendation(&mut self, kind: RecommendationKind, cx: &mut Context<Self>) {
        let Some(credential) = self.credential.clone() else {
            self.status = StatusMessage::error("请先登录 QQ 音乐");
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.status = StatusMessage::error("QQ 音乐客户端不可用");
            cx.notify();
            return;
        };
        self.home_recommendation_loading = Some(kind);
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(30), async {
                match kind {
                    RecommendationKind::Radar => {
                        let page = client.radar_tracks(&credential, 1).await?;
                        Ok::<_, anyhow::Error>((
                            page.tracks,
                            PersistedQueueContinuation::Radar {
                                next_page: page.next_page,
                                has_more: page.has_more,
                            },
                        ))
                    }
                    RecommendationKind::Guess => Ok((
                        client.guess_tracks(&credential, 5).await?,
                        PersistedQueueContinuation::Guess,
                    )),
                }
            })
            .await
            .context("QQ 音乐个性化推荐请求等待超过 30 秒")
            .and_then(|result| result);
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.home_recommendation_loading != Some(kind) {
                    return;
                }
                this.home_recommendation_loading = None;
                match result {
                    Ok((tracks, continuation)) if !tracks.is_empty() => {
                        this.pending_playback_restore = None;
                        this.queue_generation = this.queue_generation.wrapping_add(1);
                        this.queue_recommendation_loading = false;
                        this.queue_waiting_for_recommendation = false;
                        this.playback_queue = Some(PlaybackQueue {
                            playlist_id: UserPlaylistId::Recommendation { kind },
                            tracks,
                            continuation: Some(continuation),
                        });
                        this.start_playback(0, Duration::ZERO, None, true, cx);
                    }
                    Ok(_) => {
                        this.status = StatusMessage::error("QQ 音乐没有返回可播放的推荐歌曲");
                    }
                    Err(error) => {
                        this.status =
                            StatusMessage::error(format!("加载个性化推荐失败：{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn maybe_load_queue_recommendations(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.queue_recommendation_loading {
            return;
        }
        let Some((continuation, remaining)) = self.playback_queue.as_ref().and_then(|queue| {
            let continuation = queue.continuation?;
            let current = self.current_track.unwrap_or_default();
            Some((
                continuation,
                queue.tracks.len().saturating_sub(current.saturating_add(1)),
            ))
        }) else {
            return;
        };
        if !continuation.can_load_more() || (!force && remaining > 2) {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        let generation = self.queue_generation;
        self.queue_recommendation_loading = true;

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(30), async {
                match continuation {
                    PersistedQueueContinuation::Radar { next_page, .. } => {
                        let page = client.radar_tracks(&credential, next_page).await?;
                        Ok::<_, anyhow::Error>((
                            page.tracks,
                            Some(PersistedQueueContinuation::Radar {
                                next_page: page.next_page,
                                has_more: page.has_more,
                            }),
                        ))
                    }
                    PersistedQueueContinuation::Guess => {
                        let tracks = client.guess_tracks(&credential, 5).await?;
                        let next =
                            (!tracks.is_empty()).then_some(PersistedQueueContinuation::Guess);
                        Ok((tracks, next))
                    }
                }
            })
            .await
            .context("QQ 音乐推荐队列请求等待超过 30 秒")
            .and_then(|result| result);
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.queue_generation != generation {
                    return;
                }
                this.queue_recommendation_loading = false;
                match result {
                    Ok((tracks, continuation)) => {
                        let mut first_added = None;
                        if let Some(queue) = &mut this.playback_queue {
                            queue.continuation = continuation;
                            for track in tracks {
                                if !queue.tracks.iter().any(|item| item.mid == track.mid) {
                                    first_added.get_or_insert(queue.tracks.len());
                                    queue.tracks.push(track);
                                }
                            }
                        }
                        this.persist_current_playback();
                        if this.queue_waiting_for_recommendation {
                            this.queue_waiting_for_recommendation = false;
                            if let Some(index) = first_added {
                                this.start_playback(index, Duration::ZERO, None, true, cx);
                            } else {
                                this.status = StatusMessage::info("当前推荐暂时没有更多歌曲");
                            }
                        }
                    }
                    Err(error) => {
                        this.queue_waiting_for_recommendation = false;
                        this.status =
                            StatusMessage::error(format!("继续加载推荐歌曲失败：{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_library(&mut self, force_refresh: bool, cx: &mut Context<Self>) {
        let Some(credential) = self.credential.clone() else {
            return;
        };
        if !force_refresh
            && let Some((profile, playlists)) = self.library_cache.fresh_directory(
                credential.music_id,
                unix_timestamp_secs(),
                LIBRARY_CACHE_TTL,
            )
        {
            self.library_loading = false;
            self.apply_library(credential.music_id, profile, playlists, false, cx);
            return;
        }
        let Some(client) = self.protocol_client.clone() else {
            self.status = StatusMessage::error("QQ 音乐客户端不可用");
            cx.notify();
            return;
        };
        self.library_generation = self.library_generation.wrapping_add(1);
        let generation = self.library_generation;
        self.library_loading = true;
        self.status = StatusMessage::info("正在加载用户资料和歌单…");
        cx.notify();
        let account_id = credential.music_id;

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = async {
                tokio::time::timeout(Duration::from_secs(30), async {
                    tokio::try_join!(
                        client.user_profile(&credential),
                        client.user_playlists(&credential)
                    )
                })
                .await
                .context("QQ 音乐用户资料和歌单请求等待超过 30 秒")?
            }
            .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.library_generation != generation {
                    return;
                }
                this.library_loading = false;
                match result {
                    Ok((profile, playlists)) => {
                        this.library_cache.replace_directory(
                            account_id,
                            profile.clone(),
                            playlists.clone(),
                            unix_timestamp_secs(),
                        );
                        this.persist_library_cache();
                        this.apply_library(account_id, profile, playlists, force_refresh, cx);
                    }
                    Err(error) => {
                        this.status =
                            StatusMessage::error(format!("加载 QQ 音乐资料失败：{error:#}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn apply_library(
        &mut self,
        account_id: u64,
        profile: UserProfile,
        playlists: Vec<UserPlaylist>,
        force_refresh: bool,
        cx: &mut Context<Self>,
    ) {
        self.profile = Some(profile);
        let count = playlists.len();
        let viewed_index = self
            .settings
            .last_library_view
            .as_ref()
            .filter(|view| view.account_id == account_id)
            .and_then(|view| {
                playlists
                    .iter()
                    .position(|playlist| playlist.id == view.playlist_id)
            });
        let playback_restore = self
            .pending_playback_restore
            .clone()
            .filter(|restore| restore.account_id == account_id);
        if self.pending_playback_restore.is_some() && playback_restore.is_none() {
            self.clear_persisted_playback();
        }
        self.playlist_list.update(cx, |list, cx| {
            list.delegate_mut().set_playlists(playlists);
            cx.notify();
        });
        if count > 0 {
            self.select_playlist_with_refresh(viewed_index.unwrap_or(0), force_refresh, false, cx);
        } else {
            self.status = StatusMessage::info("QQ 音乐账号中没有可显示的歌单");
            cx.notify();
        }
        if let Some(restore) = playback_restore {
            self.restore_playback_queue(restore, cx);
        }
    }

    fn select_playlist(&mut self, index: usize, cx: &mut Context<Self>) {
        self.select_playlist_with_refresh(index, false, true, cx);
    }

    fn select_playlist_with_refresh(
        &mut self,
        index: usize,
        force_refresh: bool,
        record_navigation: bool,
        cx: &mut Context<Self>,
    ) {
        let playlist = self
            .playlist_list
            .read(cx)
            .delegate()
            .playlist(index)
            .cloned();
        let Some(playlist) = playlist else {
            return;
        };

        if let Some(account_id) = self
            .credential
            .as_ref()
            .map(|credential| credential.music_id)
        {
            let view = PersistedLibraryView {
                account_id,
                playlist_id: playlist.id.clone(),
            };
            if self.settings.last_library_view.as_ref() != Some(&view) {
                self.settings.last_library_view = Some(view);
                self.persist_settings();
            }
        }

        if record_navigation {
            let target = NavigationPage::Playlist {
                playlist: playlist.clone(),
                selected_index: Some(index),
            };
            let current = self.current_navigation_page();
            self.navigation_history.record(current, &target);
        }
        self.open_playlist(playlist, Some(index), force_refresh, cx);
    }

    fn open_search_artist(
        &mut self,
        artist: SearchArtist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = NavigationPage::Artist { artist };
        let current = self.current_navigation_page();
        self.navigation_history.record(current, &target);
        self.apply_navigation_page(target, window, cx);
    }

    fn open_artist(&mut self, artist: SearchArtist, cx: &mut Context<Self>) {
        let artist_changed = self
            .selected_artist
            .as_ref()
            .is_none_or(|selected| selected.mid != artist.mid);
        if artist_changed {
            self.artist_generation = self.artist_generation.wrapping_add(1);
            self.artist_songs = None;
            self.artist_track_count = 0;
            self.artist_songs_loading = false;
            self.artist_songs_loading_more = false;
            self.artist_song_error = None;
            self.artist_albums = None;
            self.artist_albums_loading = false;
            self.artist_albums_loading_more = false;
            self.artist_album_error = None;
        }
        self.selected_artist = Some(artist);
        self.main_content = MainContent::Artist;
        if self.artist_songs.is_none() && !self.artist_songs_loading {
            self.load_artist_songs(false, cx);
        }
        if self.artist_albums.is_none() && !self.artist_albums_loading {
            self.load_artist_albums(false, cx);
        }
        cx.notify();
    }

    fn load_artist_songs(&mut self, append: bool, cx: &mut Context<Self>) {
        if self.artist_songs_loading || self.artist_songs_loading_more {
            return;
        }
        let Some(artist) = self.selected_artist.clone() else {
            return;
        };
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.artist_song_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        let offset = if append {
            let Some(page) = self.artist_songs.as_ref().filter(|page| page.has_more) else {
                return;
            };
            page.next_offset
        } else {
            0
        };
        let generation = self.artist_generation;
        let artist_mid = artist.mid.clone();
        let playlist = artist.into_playlist();
        self.artist_song_error = None;
        if append {
            self.artist_songs_loading_more = true;
        } else {
            self.artist_songs_loading = true;
        }
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = client
                .playlist_page(&credential, &playlist, offset, ARTIST_PAGE_SIZE)
                .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.artist_generation != generation
                    || this
                        .selected_artist
                        .as_ref()
                        .map(|artist| artist.mid.as_str())
                        != Some(artist_mid.as_str())
                {
                    return;
                }
                this.finish_artist_song_load(result, append);
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_artist_song_load(&mut self, result: anyhow::Result<PlaylistPage>, append: bool) {
        self.artist_songs_loading = false;
        self.artist_songs_loading_more = false;
        match result {
            Ok(page) => {
                self.artist_track_count = page.total;
                let page = SearchPage {
                    items: page.tracks,
                    has_more: page.has_more,
                    next_offset: page.next_offset,
                };
                if append {
                    if let Some(songs) = &mut self.artist_songs {
                        append_search_page(songs, page);
                    } else {
                        self.artist_songs = Some(page);
                    }
                } else {
                    self.artist_songs = Some(page);
                }
            }
            Err(error) => {
                self.artist_song_error = Some(format!("加载歌手歌曲失败：{error:#}"));
            }
        }
    }

    fn load_artist_albums(&mut self, append: bool, cx: &mut Context<Self>) {
        if self.artist_albums_loading || self.artist_albums_loading_more {
            return;
        }
        let Some(artist) = self.selected_artist.clone() else {
            return;
        };
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.artist_album_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        let offset = if append {
            let Some(page) = self.artist_albums.as_ref().filter(|page| page.has_more) else {
                return;
            };
            page.next_offset
        } else {
            0
        };
        let generation = self.artist_generation;
        let artist_mid = artist.mid.clone();
        self.artist_album_error = None;
        if append {
            self.artist_albums_loading_more = true;
        } else {
            self.artist_albums_loading = true;
        }
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = client
                .artist_albums(&credential, &artist, offset, ARTIST_PAGE_SIZE)
                .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.artist_generation != generation
                    || this
                        .selected_artist
                        .as_ref()
                        .map(|artist| artist.mid.as_str())
                        != Some(artist_mid.as_str())
                {
                    return;
                }
                this.finish_artist_album_load(result, append);
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_artist_album_load(
        &mut self,
        result: anyhow::Result<SearchPage<SearchAlbum>>,
        append: bool,
    ) {
        self.artist_albums_loading = false;
        self.artist_albums_loading_more = false;
        match result {
            Ok(page) if append => {
                if let Some(albums) = &mut self.artist_albums {
                    append_search_page(albums, page);
                } else {
                    self.artist_albums = Some(page);
                }
            }
            Ok(page) => self.artist_albums = Some(page),
            Err(error) => {
                self.artist_album_error = Some(format!("加载歌手专辑失败：{error:#}"));
            }
        }
    }

    fn select_artist_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(artist) = self.selected_artist.clone() else {
            return;
        };
        let Some(songs) = self.artist_songs.as_ref() else {
            return;
        };
        let Some(selected_track) = songs.items.get(index) else {
            return;
        };
        let mut playlist = artist.into_playlist();
        playlist.track_count = self.artist_track_count;

        if let Some(queue_index) = self.playback_queue.as_ref().and_then(|queue| {
            (queue.playlist_id == playlist.id)
                .then(|| {
                    queue
                        .tracks
                        .iter()
                        .position(|track| track.mid == selected_track.mid)
                })
                .flatten()
        }) {
            if self.current_track == Some(queue_index) {
                if self.loading_track.is_none() {
                    self.toggle_playback(cx);
                }
            } else {
                self.start_playback(queue_index, Duration::ZERO, None, true, cx);
            }
            return;
        }

        let tracks = songs.items.clone();
        let has_more = songs.has_more;
        self.pending_playback_restore = None;
        self.replace_playback_queue(playlist, tracks, has_more, cx);
        self.start_playback(index, Duration::ZERO, None, true, cx);
    }

    fn open_home_playlist(
        &mut self,
        playlist: UserPlaylist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = NavigationPage::Playlist {
            playlist: playlist.clone(),
            selected_index: None,
        };
        let current = self.current_navigation_page();
        self.navigation_history.record(current, &target);
        self.playlist_list.update(cx, |list, cx| {
            list.set_selected_index(None, window, cx);
        });
        self.open_playlist(playlist, None, false, cx);
    }

    fn open_playlist(
        &mut self,
        playlist: UserPlaylist,
        selected_index: Option<usize>,
        force_refresh: bool,
        cx: &mut Context<Self>,
    ) {
        self.main_content = MainContent::Playlist;

        self.playlist_generation = self.playlist_generation.wrapping_add(1);
        self.playlist_force_refresh = force_refresh;
        self.playlist_cache_revision = new_cache_revision();
        self.selected_playlist_index = selected_index;
        self.selected_playlist = Some(playlist.clone());
        self.page_offset = 0;
        self.page_loading = false;
        if let Some(index) = selected_index {
            self.playlist_list.update(cx, |list, cx| {
                list.delegate_mut().set_selected(index);
                cx.notify();
            });
        }
        self.track_table.update(cx, |table, cx| {
            table.delegate_mut().reset();
            table.refresh(cx);
            table.scroll_to_row(0, cx);
            cx.notify();
        });
        if !force_refresh
            && let Some(account_id) = self
                .credential
                .as_ref()
                .map(|credential| credential.music_id)
            && let Some(snapshot) = self.library_cache.fresh_playlist(
                account_id,
                &playlist.id,
                unix_timestamp_secs(),
                LIBRARY_CACHE_TTL,
            )
        {
            self.playlist_cache_revision = snapshot.revision;
            self.page_offset = snapshot.next_offset;
            self.selected_playlist = Some(snapshot.playlist.clone());
            if let Some(index) = self.selected_playlist_index {
                self.playlist_list.update(cx, |list, cx| {
                    list.delegate_mut()
                        .update_playlist(index, snapshot.playlist.clone());
                    cx.notify();
                });
            }
            let track_count = snapshot.tracks.len();
            self.track_table.update(cx, |table, cx| {
                table
                    .delegate_mut()
                    .append(snapshot.tracks, snapshot.has_more);
                table.refresh(cx);
                cx.notify();
            });
            self.status = StatusMessage::info(if track_count == 0 {
                format!("歌单“{}”中暂时没有歌曲", snapshot.playlist.title)
            } else {
                format!("已打开歌单“{}”", snapshot.playlist.title)
            });
            self.sync_table_playback_state(cx);
            cx.notify();
            return;
        }
        self.status = StatusMessage::info(format!("正在加载歌单“{}”…", playlist.title));
        self.sync_table_playback_state(cx);
        self.load_playlist_page(cx);
    }

    fn load_playlist_page(&mut self, cx: &mut Context<Self>) {
        if self.page_loading {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(playlist) = self.selected_playlist.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.status = StatusMessage::error("QQ 音乐客户端不可用");
            cx.notify();
            return;
        };

        let generation = self.playlist_generation;
        let offset = self.page_offset;
        let cache_revision = self.playlist_cache_revision;
        let force_refresh = offset == 0 && self.playlist_force_refresh;
        self.playlist_force_refresh = false;
        let requests = self.playlist_page_requests.clone();
        self.page_loading = true;
        self.track_table.update(cx, |table, cx| {
            table.delegate_mut().set_loading(true);
            cx.notify();
        });

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = request_playlist_page(
                requests,
                client,
                credential,
                playlist,
                offset,
                force_refresh,
            )
            .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.playlist_generation != generation {
                    return;
                }
                this.page_loading = false;
                match result {
                    Ok(page) => {
                        let new_tracks = page.tracks.len();
                        let cache_changed = this.credential.as_ref().is_some_and(|credential| {
                            this.library_cache.store_playlist_page(
                                credential.music_id,
                                page.playlist.clone(),
                                page.tracks.clone(),
                                page.has_more,
                                page.next_offset,
                                offset,
                                unix_timestamp_secs(),
                                cache_revision,
                            )
                        });
                        if cache_changed {
                            this.persist_library_cache();
                        }
                        this.page_offset = page.next_offset;
                        this.selected_playlist = Some(page.playlist.clone());
                        if let Some(index) = this.selected_playlist_index {
                            this.playlist_list.update(cx, |list, cx| {
                                list.delegate_mut()
                                    .update_playlist(index, page.playlist.clone());
                                cx.notify();
                            });
                        }
                        this.track_table.update(cx, |table, cx| {
                            table.delegate_mut().append(page.tracks, page.has_more);
                            table.refresh(cx);
                            cx.notify();
                        });
                        if offset == 0 {
                            this.status = StatusMessage::info(if new_tracks == 0 {
                                format!("歌单“{}”中暂时没有歌曲", page.playlist.title)
                            } else {
                                format!("已打开歌单“{}”", page.playlist.title)
                            });
                        }
                    }
                    Err(error) => {
                        this.track_table.update(cx, |table, cx| {
                            table.delegate_mut().set_loading(false);
                            cx.notify();
                        });
                        this.status = StatusMessage::error(format!("加载歌单失败：{error:#}"));
                    }
                }
                this.sync_table_playback_state(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn restore_playback_queue(&mut self, restore: PersistedPlayback, cx: &mut Context<Self>) {
        let index = restore
            .queue_tracks
            .iter()
            .position(|track| track.mid == restore.track_mid);
        if let Some(index) = index {
            let resume_at = restore.resume_position(restore.queue_tracks[index].duration_seconds);
            self.pending_playback_restore = None;
            self.queue_generation = self.queue_generation.wrapping_add(1);
            self.queue_recommendation_loading = false;
            self.queue_waiting_for_recommendation = false;
            self.playback_queue = Some(PlaybackQueue {
                playlist_id: restore.playlist_id,
                tracks: restore.queue_tracks,
                continuation: restore.queue_continuation,
            });
            self.start_playback(index, resume_at, None, false, cx);
        } else {
            self.clear_persisted_playback();
        }
    }

    fn replace_playback_queue(
        &mut self,
        playlist: UserPlaylist,
        tracks: Vec<Track>,
        has_more: bool,
        cx: &mut Context<Self>,
    ) {
        self.home_recommendation_loading = None;
        self.queue_generation = self.queue_generation.wrapping_add(1);
        let generation = self.queue_generation;
        let mut offset = tracks.len() as u64;
        let initial_tracks = tracks.clone();
        self.playback_queue = Some(PlaybackQueue {
            playlist_id: playlist.id.clone(),
            tracks,
            continuation: None,
        });
        self.queue_recommendation_loading = false;
        self.queue_waiting_for_recommendation = false;

        if !has_more {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        let account_id = credential.music_id;
        let cached_playlist = playlist.clone();
        let cache_revision = self.playlist_cache_revision;
        let requests = self.playlist_page_requests.clone();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = async {
                let mut remaining = Vec::new();
                let mut has_more = true;
                while has_more {
                    let page = request_playlist_page(
                        requests.clone(),
                        client.clone(),
                        credential.clone(),
                        playlist.clone(),
                        offset,
                        false,
                    )
                    .await
                    .context("无法补全 QQ 音乐播放队列")?;
                    offset = page.next_offset;
                    has_more = page.has_more;
                    remaining.extend(page.tracks);
                }
                Ok::<_, anyhow::Error>(remaining)
            }
            .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.queue_generation != generation {
                    return;
                }
                if let (Some(queue), Ok(tracks)) = (&mut this.playback_queue, result) {
                    let mut cached_tracks = initial_tracks;
                    cached_tracks.extend(tracks.iter().cloned());
                    for track in tracks {
                        if !queue.tracks.iter().any(|item| item.mid == track.mid) {
                            queue.tracks.push(track);
                        }
                    }
                    let cached_track_count = cached_tracks.len() as u64;
                    this.library_cache.replace_playlist(
                        account_id,
                        cached_playlist,
                        cached_tracks,
                        false,
                        cached_track_count,
                        unix_timestamp_secs(),
                        cache_revision,
                    );
                    this.persist_library_cache();
                    this.persist_current_playback();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn select_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let (tracks, has_more) = {
            let table = self.track_table.read(cx);
            (
                table.delegate().tracks().to_vec(),
                table.delegate().has_more(),
            )
        };
        if index >= tracks.len() {
            return;
        }
        let Some(playlist) = self.selected_playlist.clone() else {
            return;
        };
        let selected_mid = tracks[index].mid.as_str();
        if let Some(queue_index) = self.playback_queue.as_ref().and_then(|queue| {
            (queue.playlist_id == playlist.id)
                .then(|| {
                    queue
                        .tracks
                        .iter()
                        .position(|track| track.mid == selected_mid)
                })
                .flatten()
        }) {
            if self.current_track == Some(queue_index) {
                if self.loading_track.is_none() {
                    self.toggle_playback(cx);
                }
            } else {
                self.start_playback(queue_index, Duration::ZERO, None, true, cx);
            }
            return;
        }
        self.pending_playback_restore = None;
        self.replace_playback_queue(playlist, tracks, has_more, cx);
        self.start_playback(index, Duration::ZERO, None, true, cx);
    }

    fn start_playback(
        &mut self,
        index: usize,
        resume_at: Duration,
        requested_quality: Option<Quality>,
        autoplay: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(credential) = self.credential.clone() else {
            self.status = StatusMessage::error("请先登录 QQ 音乐");
            cx.notify();
            return;
        };
        let Some(track) = self
            .playback_queue
            .as_ref()
            .and_then(|queue| queue.tracks.get(index))
            .cloned()
        else {
            return;
        };
        let Some(audio_cache) = self.audio_cache.clone() else {
            self.status = StatusMessage::error("音频缓存不可用，无法创建播放流");
            cx.notify();
            return;
        };
        let Some(audio) = &self.audio else {
            self.status = StatusMessage::error("没有可用的音频输出设备");
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.status = StatusMessage::error("QQ 音乐客户端不可用");
            cx.notify();
            return;
        };

        let desired_quality = requested_quality.unwrap_or(self.settings.playback_quality);
        let same_track = (self.current_track == Some(index) && self.loading_track.is_some())
            || self
                .playback_location
                .as_ref()
                .is_some_and(|location| location.track_mid == track.mid);
        let known_qualities = if same_track {
            self.available_qualities.clone()
        } else {
            self.available_qualities.clear();
            self.quality_menu_open = false;
            Vec::new()
        };
        let reused_urls = self
            .playback_location
            .as_ref()
            .filter(|location| {
                location.track_mid == track.mid && location.quality == desired_quality
            })
            .map(|location| location.urls.clone());

        audio.stop();
        self.play_generation = self.play_generation.wrapping_add(1);
        let generation = self.play_generation;
        self.current_track = Some(index);
        self.loading_track = Some(index);
        self.loading_autoplay = autoplay;
        self.resolving_qualities = reused_urls.is_none() && known_qualities.is_empty();
        self.playback_started = false;
        self.active_quality = desired_quality;
        self.position = resume_at;
        let progress = progress_fraction(resume_at, Duration::from_secs(track.duration_seconds));
        self.progress_slider.update(cx, |slider, cx| {
            *slider = progress_slider_state(progress);
            cx.notify();
        });
        self.status = StatusMessage::info(if self.resolving_qualities {
            format!("正在检测“{}”的音质…", track.title)
        } else {
            format!("正在缓冲“{}”…", track.title)
        });
        self.queue_waiting_for_recommendation = false;
        self.persist_current_playback();
        self.sync_table_playback_state(cx);
        #[cfg(target_os = "linux")]
        self.sync_mpris(!same_track && !resume_at.is_zero());
        cx.notify();
        self.maybe_load_queue_recommendations(false, cx);

        let title = track.title.clone();
        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = async {
                let reused_stream = match reused_urls {
                    Some(urls) => audio_cache
                        .prepare_for_seek_with_fallbacks(urls.clone(), &track, desired_quality)
                        .await
                        .ok()
                        .map(|stream| {
                            let qualities = if known_qualities.is_empty() {
                                vec![desired_quality]
                            } else {
                                known_qualities.clone()
                            };
                            (desired_quality, urls, stream, qualities)
                        }),
                    None => None,
                };
                let (quality, urls, stream, available_qualities) = match reused_stream {
                    Some(reused) => reused,
                    None => {
                        let _ = sender.send(PlaybackLoadEvent::ResolvingOptions).await;
                        let options = client.playback_options(&credential, &track).await?;
                        let mut available_qualities = options
                            .iter()
                            .map(|option| option.quality)
                            .collect::<Vec<_>>();
                        let _ = sender
                            .send(PlaybackLoadEvent::Options(available_qualities.clone()))
                            .await;
                        let candidates =
                            Quality::fallback_order(&available_qualities, desired_quality);
                        let mut prepared = None;
                        let mut last_error = None;
                        for quality in candidates {
                            let Some(option) =
                                options.iter().find(|option| option.quality == quality)
                            else {
                                continue;
                            };
                            let urls = option.urls().map(str::to_owned).collect::<Vec<_>>();
                            match audio_cache
                                .prepare_with_fallbacks(urls.clone(), &track, quality)
                                .await
                            {
                                Ok(stream) => {
                                    prepared = Some((quality, urls, stream));
                                    break;
                                }
                                Err(error) => {
                                    available_qualities.retain(|candidate| *candidate != quality);
                                    last_error = Some(error.context(format!(
                                        "“{}”的{}音源不可用",
                                        track.title,
                                        quality.label()
                                    )));
                                }
                            }
                        }
                        let (quality, urls, stream) = prepared.ok_or_else(|| {
                            last_error.unwrap_or_else(|| {
                                anyhow::anyhow!("QQ 音乐没有返回当前账号可播放的音质")
                            })
                        })?;
                        (quality, urls, stream, available_qualities)
                    }
                };
                let playback =
                    tokio::task::spawn_blocking(move || PreparedPlayback::new(stream, resume_at))
                        .await
                        .context("音频解码准备任务异常退出")??;
                Ok::<_, anyhow::Error>((
                    playback,
                    PlaybackLocation {
                        track_mid: track.mid,
                        quality,
                        urls,
                    },
                    available_qualities,
                ))
            }
            .await;
            let _ = sender.send(PlaybackLoadEvent::Finished(result)).await;
        }));

        cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                let finished = matches!(&event, PlaybackLoadEvent::Finished(_));
                let _ = this.update(cx, |this, cx| {
                    if this.play_generation != generation {
                        return;
                    }
                    match event {
                        PlaybackLoadEvent::ResolvingOptions => {
                            this.resolving_qualities = true;
                        }
                        PlaybackLoadEvent::Options(available_qualities) => {
                            this.resolving_qualities = false;
                            let loading_quality =
                                Quality::best_available(&available_qualities, desired_quality);
                            this.available_qualities = available_qualities;
                            if let Some(quality) = loading_quality {
                                this.active_quality = quality;
                                this.status = StatusMessage::info(format!("正在缓冲“{title}”…"));
                            } else {
                                this.status =
                                    StatusMessage::info(format!("正在获取“{title}”的可播放音质…"));
                            }
                        }
                        PlaybackLoadEvent::Finished(result) => {
                            this.loading_track = None;
                            this.loading_autoplay = false;
                            this.resolving_qualities = false;
                            match result {
                                Ok((playback, location, available_qualities)) => {
                                    let quality = location.quality;
                                    this.playback_location = Some(location);
                                    this.active_quality = quality;
                                    this.available_qualities = available_qualities;
                                    let result = this
                                        .audio
                                        .as_ref()
                                        .context("音频输出设备不可用")
                                        .and_then(|audio| audio.replace(playback, autoplay));
                                    match result {
                                        Ok(()) => {
                                            if let Some(audio) = &this.audio {
                                                audio.set_volume(this.settings.volume);
                                            }
                                            this.playback_started = true;
                                            this.status = StatusMessage::info(if autoplay {
                                                format!("正在播放“{title}”")
                                            } else {
                                                format!("已暂停“{title}”")
                                            });
                                        }
                                        Err(error) => {
                                            this.status = StatusMessage::error(format!(
                                                "播放失败：{error:#}"
                                            ));
                                        }
                                    }
                                }
                                Err(error) => {
                                    this.status =
                                        StatusMessage::error(format!("获取歌曲失败：{error:#}"));
                                }
                            }
                        }
                    }
                    this.sync_table_playback_state(cx);
                    #[cfg(target_os = "linux")]
                    this.sync_mpris(false);
                    cx.notify();
                });
                if finished {
                    break;
                }
            }
        })
        .detach();
    }

    fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        if self.loading_track.is_some() {
            return;
        }
        if self.current_track.is_none() {
            if !self.track_table.read(cx).delegate().tracks().is_empty() {
                self.select_track(0, cx);
            }
            return;
        }
        let Some(audio) = &self.audio else {
            return;
        };
        if !self.playback_started || audio.is_empty() {
            let index = self.current_track.expect("current track was checked above");
            self.start_playback(index, Duration::ZERO, None, true, cx);
            return;
        }
        let playing = audio.toggle();
        self.status = StatusMessage::info(if playing {
            "继续播放".to_owned()
        } else {
            "已暂停".to_owned()
        });
        self.persist_current_playback();
        self.sync_table_playback_state(cx);
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        cx.notify();
    }

    #[cfg(target_os = "linux")]
    fn play(&mut self, cx: &mut Context<Self>) {
        if self.loading_track.is_some() {
            return;
        }
        let Some(index) = self.current_track else {
            if !self.track_table.read(cx).delegate().tracks().is_empty() {
                self.select_track(0, cx);
            }
            return;
        };
        let Some(audio) = &self.audio else {
            return;
        };
        if !self.playback_started || audio.is_empty() {
            self.start_playback(index, Duration::ZERO, None, true, cx);
        } else if !audio.is_playing() {
            self.toggle_playback(cx);
        }
    }

    #[cfg(target_os = "linux")]
    fn pause_playback(&mut self, cx: &mut Context<Self>) {
        if self.loading_track.is_none() && self.audio.as_ref().is_some_and(AudioPlayer::is_playing)
        {
            self.toggle_playback(cx);
        }
    }

    #[cfg(target_os = "linux")]
    fn stop_playback(&mut self, cx: &mut Context<Self>) {
        if self.current_track.is_none() {
            return;
        }
        self.play_generation = self.play_generation.wrapping_add(1);
        if let Some(audio) = &self.audio {
            audio.stop();
        }
        self.loading_track = None;
        self.loading_autoplay = false;
        self.resolving_qualities = false;
        self.playback_started = false;
        self.position = Duration::ZERO;
        self.seek_preview = None;
        self.progress_slider.update(cx, |slider, cx| {
            *slider = progress_slider_state(0.);
            cx.notify();
        });
        self.status = StatusMessage::info("已停止播放");
        self.persist_current_playback();
        self.sync_table_playback_state(cx);
        self.sync_mpris(false);
        cx.notify();
    }

    fn seek_to(&mut self, target: Duration, cx: &mut Context<Self>) {
        let Some(index) = self.current_track else {
            return;
        };
        let target = self
            .current_duration()
            .map_or(target, |duration| target.min(duration));
        let autoplay = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
        self.start_playback(index, target, Some(self.active_quality), autoplay, cx);
        #[cfg(target_os = "linux")]
        self.sync_mpris(true);
    }

    #[cfg(target_os = "linux")]
    fn seek_by(&mut self, offset_micros: i64, cx: &mut Context<Self>) {
        if self.loading_track.is_some() || !self.playback_started {
            return;
        }
        let Some(duration) = self.current_duration() else {
            return;
        };
        let position = self
            .audio
            .as_ref()
            .map(AudioPlayer::position)
            .unwrap_or(self.position);
        let target_micros = duration_micros(position) as i128 + offset_micros as i128;
        let target_micros = target_micros.clamp(0, i64::MAX as i128) as i64;
        if target_micros >= duration_micros(duration) {
            self.play_next(false, cx);
        } else {
            self.seek_to(Duration::from_micros(target_micros as u64), cx);
        }
    }

    #[cfg(target_os = "linux")]
    fn set_mpris_position(&mut self, track_id: &str, position_micros: i64, cx: &mut Context<Self>) {
        if self.loading_track.is_some() || !self.playback_started {
            return;
        }
        let Some(track) = self.current_track_data() else {
            return;
        };
        if track_id != mpris_track_id(&track.mid) || position_micros < 0 {
            return;
        }
        let duration_micros = track
            .duration_seconds
            .saturating_mul(1_000_000)
            .min(i64::MAX as u64) as i64;
        if position_micros >= duration_micros {
            return;
        }
        self.seek_to(Duration::from_micros(position_micros as u64), cx);
    }

    fn play_previous(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.current_track else {
            return;
        };
        let queue_len = self
            .playback_queue
            .as_ref()
            .map_or(0, |queue| queue.tracks.len());
        if self.position >= Duration::from_secs(3) {
            self.seek_to(Duration::ZERO, cx);
            return;
        }
        let previous = if index > 0 {
            Some(index - 1)
        } else if self.repeat_mode == RepeatMode::All && queue_len > 0 {
            Some(queue_len - 1)
        } else {
            None
        };
        if let Some(previous) = previous {
            self.start_playback(previous, Duration::ZERO, None, true, cx);
        }
    }

    fn play_next(&mut self, automatic: bool, cx: &mut Context<Self>) {
        let Some(index) = self.current_track else {
            return;
        };
        let len = self
            .playback_queue
            .as_ref()
            .map_or(0, |queue| queue.tracks.len());
        if len == 0 {
            return;
        }
        let continuation = self
            .playback_queue
            .as_ref()
            .and_then(|queue| queue.continuation);
        let can_extend = continuation.is_some_and(PersistedQueueContinuation::can_load_more);
        let next = if automatic && self.repeat_mode == RepeatMode::One {
            Some(index)
        } else if continuation.is_none() && self.shuffle && len > 1 {
            Some(self.random_track_index(index, len))
        } else if index + 1 < len {
            Some(index + 1)
        } else if !can_extend && self.repeat_mode == RepeatMode::All {
            Some(0)
        } else {
            None
        };
        if let Some(next) = next {
            self.start_playback(next, Duration::ZERO, None, true, cx);
        } else if can_extend {
            self.playback_started = false;
            self.queue_waiting_for_recommendation = true;
            self.status = StatusMessage::info("正在获取下一首推荐…");
            self.maybe_load_queue_recommendations(true, cx);
            self.persist_current_playback();
            #[cfg(target_os = "linux")]
            self.sync_mpris(false);
            cx.notify();
        } else {
            self.playback_started = false;
            self.position = self.current_duration().unwrap_or_default();
            self.status = StatusMessage::info("当前播放队列已结束");
            self.persist_current_playback();
            #[cfg(target_os = "linux")]
            self.sync_mpris(false);
            cx.notify();
        }
    }

    fn random_track_index(&self, current: usize, len: usize) -> usize {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as usize;
        let candidate = seed % (len - 1);
        if candidate >= current {
            candidate + 1
        } else {
            candidate
        }
    }

    fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.settings.volume = volume.clamp(0., 1.);
        if self.settings.volume > 0. {
            self.settings.last_nonzero_volume = self.settings.volume;
        }
        if let Some(audio) = &self.audio {
            audio.set_volume(self.settings.volume);
        }
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        cx.notify();
    }

    fn set_playback_quality(&mut self, quality: Quality, cx: &mut Context<Self>) {
        if !self.available_qualities.contains(&quality) {
            return;
        }
        self.quality_menu_open = false;
        if self.settings.playback_quality != quality {
            self.settings.playback_quality = quality;
            self.persist_settings();
        }
        if self.active_quality == quality {
            cx.notify();
            return;
        }
        let Some(index) = self.current_track else {
            cx.notify();
            return;
        };
        let loading = self.loading_track.is_some();
        let autoplay = if loading {
            self.loading_autoplay
        } else {
            self.audio.as_ref().is_some_and(AudioPlayer::is_playing)
        };
        let resume_at = if loading {
            self.position
        } else {
            self.audio
                .as_ref()
                .map(AudioPlayer::position)
                .unwrap_or(self.position)
        };
        self.start_playback(index, resume_at, Some(quality), autoplay, cx);
    }

    fn toggle_mute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let volume = if self.settings.volume > 0. {
            0.
        } else {
            self.settings.last_nonzero_volume
        };
        self.volume_slider.update(cx, |slider, cx| {
            slider.set_value(volume, window, cx);
        });
        self.set_volume(volume, cx);
        self.persist_settings();
    }

    fn set_color_theme(
        &mut self,
        color_theme: ColorTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.color_theme == color_theme {
            return;
        }
        self.settings.color_theme = color_theme;
        design::apply(color_theme, Some(window), cx);
        self.persist_settings();
        cx.notify();
    }

    fn dismiss_popovers(&mut self, cx: &mut Context<Self>) {
        if self.account_menu_open || self.quality_menu_open {
            self.account_menu_open = false;
            self.quality_menu_open = false;
            cx.notify();
        }
    }

    fn persist_settings(&self) {
        let _ = SettingsStore::save(&self.settings);
    }

    fn persist_library_cache(&self) {
        let _ = self
            .library_cache_saves
            .send_blocking(self.library_cache.clone());
    }

    fn persist_current_playback(&mut self) {
        let Some(account_id) = self
            .credential
            .as_ref()
            .map(|credential| credential.music_id)
        else {
            return;
        };
        let Some(playlist_id) = self
            .playback_queue
            .as_ref()
            .map(|queue| queue.playlist_id.clone())
        else {
            return;
        };
        let Some(track_mid) = self.current_track_data().map(|track| track.mid.clone()) else {
            return;
        };
        let position = if self.loading_track.is_some() || !self.playback_started {
            self.position
        } else {
            self.audio
                .as_ref()
                .map(AudioPlayer::position)
                .unwrap_or(self.position)
        };
        self.position = position;
        let current_playback = PersistedPlayback {
            account_id,
            playlist_id,
            track_mid,
            position_ms: position.as_millis().min(u64::MAX as u128) as u64,
            queue_tracks: self
                .playback_queue
                .as_ref()
                .map(|queue| queue.tracks.clone())
                .unwrap_or_default(),
            queue_continuation: self
                .playback_queue
                .as_ref()
                .and_then(|queue| queue.continuation),
        };
        if self.settings.current_playback.as_ref() != Some(&current_playback) {
            self.settings.current_playback = Some(current_playback);
            self.persist_settings();
        }
        self.last_playback_persisted_at = Instant::now();
    }

    fn clear_persisted_playback(&mut self) {
        self.pending_playback_restore = None;
        if self.settings.current_playback.take().is_some() {
            self.persist_settings();
        }
        self.last_playback_persisted_at = Instant::now();
    }

    fn sync_table_playback_state(&mut self, cx: &mut Context<Self>) {
        let visible = self.playback_queue.as_ref().is_some_and(|queue| {
            self.selected_playlist
                .as_ref()
                .is_some_and(|playlist| playlist.id == queue.playlist_id)
        });
        let current_mid = visible
            .then(|| self.current_track_data().map(|track| track.mid.clone()))
            .flatten();
        let loading_mid = visible
            .then(|| {
                self.loading_track
                    .and_then(|index| self.playback_queue.as_ref()?.tracks.get(index))
                    .map(|track| track.mid.clone())
            })
            .flatten();
        let (playing, loading) = {
            let table = self.track_table.read(cx);
            let tracks = table.delegate().tracks();
            (
                current_mid
                    .as_deref()
                    .and_then(|mid| tracks.iter().position(|track| track.mid == mid)),
                loading_mid
                    .as_deref()
                    .and_then(|mid| tracks.iter().position(|track| track.mid == mid)),
            )
        };
        let playback_active = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
        self.track_table.update(cx, |table, cx| {
            table
                .delegate_mut()
                .set_playback_state(playing, loading, playback_active);
            cx.notify();
        });
    }

    #[cfg(target_os = "linux")]
    fn mpris_snapshot(&self) -> MprisSnapshot {
        let audio_available = self.audio.is_some();
        let loading = self.loading_track.is_some();
        let playback_status = if loading || !self.playback_started {
            MprisPlaybackStatus::Stopped
        } else if self.audio.as_ref().is_some_and(AudioPlayer::is_playing) {
            MprisPlaybackStatus::Playing
        } else {
            MprisPlaybackStatus::Paused
        };
        let (queue_len, current_index) = self
            .playback_queue
            .as_ref()
            .map_or((0, None), |queue| (queue.tracks.len(), self.current_track));
        let can_extend = self
            .playback_queue
            .as_ref()
            .and_then(|queue| queue.continuation)
            .is_some_and(PersistedQueueContinuation::can_load_more);
        let can_go_next = audio_available
            && current_index.is_some_and(|index| {
                can_extend
                    || (self.shuffle && queue_len > 1)
                    || index + 1 < queue_len
                    || (self.repeat_mode == RepeatMode::All && queue_len > 0)
            });
        let can_go_previous = audio_available
            && current_index.is_some_and(|index| {
                self.position >= Duration::from_secs(3)
                    || index > 0
                    || (self.repeat_mode == RepeatMode::All && queue_len > 0)
            });
        let track = self.current_track_data().map(|track| MprisTrack {
            id: mpris_track_id(&track.mid),
            title: track.title.clone(),
            artists: if track.artists.trim().is_empty() {
                Vec::new()
            } else {
                vec![track.artists.clone()]
            },
            album: (!track.album.trim().is_empty()).then(|| track.album.clone()),
            art_url: track.cover_url.clone().filter(|url| !url.trim().is_empty()),
            length_micros: track
                .duration_seconds
                .saturating_mul(1_000_000)
                .min(i64::MAX as u64) as i64,
        });
        let has_track = track.is_some();
        MprisSnapshot {
            playback_status,
            loop_status: match self.repeat_mode {
                RepeatMode::Off => MprisLoopStatus::None,
                RepeatMode::All => MprisLoopStatus::Playlist,
                RepeatMode::One => MprisLoopStatus::Track,
            },
            shuffle: self.shuffle,
            volume: self.settings.volume as f64,
            position_micros: duration_micros(self.position),
            track,
            can_go_next,
            can_go_previous,
            can_play: has_track && audio_available && !loading,
            can_pause: has_track && audio_available && !loading && self.playback_started,
            can_seek: has_track && audio_available && !loading && self.playback_started,
        }
    }

    #[cfg(target_os = "linux")]
    fn sync_mpris(&self, seeked: bool) {
        let Some(mpris) = &self.mpris else {
            return;
        };
        let snapshot = self.mpris_snapshot();
        if seeked {
            mpris.seeked(snapshot);
        } else {
            mpris.update(snapshot);
        }
    }

    fn sync_progress_slider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.seek_preview.is_none() {
            let progress = self
                .current_duration()
                .map_or(0., |duration| progress_fraction(self.position, duration));
            self.progress_slider.update(cx, |slider, cx| {
                slider.set_value(progress, window, cx);
            });
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.seek_preview.is_none() && self.loading_track.is_none() && self.playback_started {
            self.position = self
                .audio
                .as_ref()
                .map(AudioPlayer::position)
                .unwrap_or_default();
        }

        let ended = self.playback_started
            && self.loading_track.is_none()
            && self.audio.as_ref().is_some_and(AudioPlayer::is_empty);
        if ended {
            self.playback_started = false;
            self.play_next(true, cx);
        }
        if self.current_track.is_some()
            && self.last_playback_persisted_at.elapsed() >= PLAYBACK_PERSIST_INTERVAL
        {
            self.persist_current_playback();
        }
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        cx.notify();
    }

    fn current_track_data(&self) -> Option<&Track> {
        self.current_track.and_then(|index| {
            self.playback_queue
                .as_ref()
                .and_then(|queue| queue.tracks.get(index))
        })
    }

    fn current_duration(&self) -> Option<Duration> {
        self.current_track_data()
            .map(|track| Duration::from_secs(track.duration_seconds))
    }

    fn logout(&mut self, cx: &mut Context<Self>) {
        self.login_generation = self.login_generation.wrapping_add(1);
        self.library_generation = self.library_generation.wrapping_add(1);
        self.playlist_generation = self.playlist_generation.wrapping_add(1);
        self.home_generation = self.home_generation.wrapping_add(1);
        self.queue_generation = self.queue_generation.wrapping_add(1);
        self.play_generation = self.play_generation.wrapping_add(1);
        if let Some(audio) = &self.audio {
            audio.stop();
        }
        self.account_state = AccountState::SignedOut;
        self.credential = None;
        self.profile = None;
        self.qr_image = None;
        self.library_loading = false;
        self.main_content = MainContent::Playlist;
        self.navigation_history.clear();
        self.home_playlists.clear();
        self.home_loading = false;
        self.home_loaded = false;
        self.home_error = None;
        self.home_recommendation_loading = None;
        self.selected_playlist_index = None;
        self.selected_playlist = None;
        self.page_loading = false;
        self.playback_queue = None;
        self.queue_recommendation_loading = false;
        self.queue_waiting_for_recommendation = false;
        self.current_track = None;
        self.loading_track = None;
        self.loading_autoplay = false;
        self.resolving_qualities = false;
        self.playback_started = false;
        self.playback_location = None;
        self.active_quality = self.settings.playback_quality;
        self.available_qualities.clear();
        self.quality_menu_open = false;
        self.position = Duration::ZERO;
        self.account_menu_open = false;
        self.clear_persisted_playback();
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        self.playlist_list.update(cx, |list, cx| {
            list.delegate_mut().clear();
            cx.notify();
        });
        self.track_table.update(cx, |table, cx| {
            table.delegate_mut().clear();
            table.refresh(cx);
            cx.notify();
        });
        self.status = StatusMessage::info("已退出登录");
        self.begin_login(cx);

        drop(RUNTIME.spawn(async move {
            let _ = tokio::task::spawn_blocking(CredentialStore::delete).await;
        }));
    }

    fn render_login(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let qr = match &self.qr_image {
            Some(image) => img(image.clone())
                .size(px(240.))
                .rounded(theme.radius_lg)
                .into_any_element(),
            None => div()
                .size(px(240.))
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .text_color(theme.muted_foreground)
                .flex()
                .items_center()
                .justify_center()
                .child(match self.account_state {
                    AccountState::Restoring => "正在恢复登录…",
                    AccountState::SigningIn => "正在生成二维码…",
                    _ => "使用 QQ 音乐 App 扫码登录",
                })
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                v_flex()
                    .w(px(380.))
                    .items_center()
                    .gap_5()
                    .p_8()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.group_box)
                    .shadow_lg()
                    .child(lyrune_icon(px(46.)))
                    .child(div().text_2xl().font_bold().child("登录 Lyrune"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("登录 QQ 音乐以加载你的歌单"),
                    )
                    .child(
                        div()
                            .id("login-qr")
                            .when(self.account_state == AccountState::SignedOut, |this| {
                                this.cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| this.begin_login(cx)))
                            })
                            .child(qr),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_center()
                            .text_color(theme.muted_foreground)
                            .child(self.status.text.clone()),
                    ),
            )
            .into_any_element()
    }

    fn render_account(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let name = self
            .profile
            .as_ref()
            .map(|profile| profile.nickname.clone())
            .unwrap_or_else(|| "QQ 音乐用户".to_owned());
        let mut avatar = Avatar::new().name(name.clone()).with_size(px(38.));
        if let Some(url) = self
            .profile
            .as_ref()
            .and_then(|profile| profile.avatar_url.clone())
        {
            avatar = avatar.src(cached_image_source(url));
        }
        let selected_theme = self.settings.color_theme;
        let theme_buttons = ColorTheme::ALL
            .into_iter()
            .map(|color_theme| {
                Button::new(color_theme.id())
                    .label(color_theme.label())
                    .ghost()
                    .w_full()
                    .h(px(44.))
                    .selected(selected_theme == color_theme)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_color_theme(color_theme, window, cx)
                    }))
            })
            .collect::<Vec<_>>();

        div()
            .relative()
            .child(
                Button::new("account-avatar")
                    .ghost()
                    .rounded(px(999.))
                    .size(px(44.))
                    .p_0()
                    .tooltip(name.clone())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.account_menu_open = !this.account_menu_open;
                        this.quality_menu_open = false;
                        cx.notify();
                    }))
                    .child(avatar),
            )
            .when(self.account_menu_open, |this| {
                this.child(
                    deferred(
                        v_flex()
                            .absolute()
                            .top(px(46.))
                            .right_0()
                            .w(px(220.))
                            .gap_2()
                            .p_3()
                            .rounded(theme.radius_lg)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.popover)
                            .shadow_lg()
                            .occlude()
                            .child(div().truncate().font_medium().child(name))
                            .child(
                                div()
                                    .pt_1()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(theme.muted_foreground)
                                    .child("主题配色"),
                            )
                            .children(theme_buttons)
                            .child(
                                div().border_t_1().border_color(theme.border).pt_2().child(
                                    Button::new("logout")
                                        .label("退出登录")
                                        .outline()
                                        .w_full()
                                        .h(px(44.))
                                        .on_click(cx.listener(|this, _, _, cx| this.logout(cx))),
                                ),
                            ),
                    )
                    .with_priority(10),
                )
            })
            .into_any_element()
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .w_full()
            .h_full()
            .flex_shrink_0()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .h(px(64.))
                    .px_5()
                    .gap_3()
                    .child(lyrune_icon(px(34.)))
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().font_semibold().child("Lyrune"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("QQ Music Player"),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .h(px(60.))
                    .px_5()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .size(px(34.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(media_icon_hsla(
                                        MediaIcon::Library,
                                        theme.secondary_foreground,
                                        px(20.),
                                    )),
                            )
                            .child(
                                v_flex()
                                    .gap_0p5()
                                    .child(div().font_semibold().child("音乐库"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("你的 QQ 音乐歌单"),
                                    ),
                            ),
                    )
                    .child(
                        Button::new("reload-library")
                            .ghost()
                            .rounded(px(999.))
                            .size(px(44.))
                            .p_0()
                            .tooltip("重新加载歌单")
                            .disabled(self.library_loading)
                            .loading(self.library_loading)
                            .when(!self.library_loading, |button| {
                                button.child(media_icon_hsla(
                                    MediaIcon::Refresh,
                                    theme.secondary_foreground,
                                    px(18.),
                                ))
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.load_library(true, cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_3()
                    .pb_3()
                    .child(List::new(&self.playlist_list).size_full()),
            )
            .when(self.status.is_error, |sidebar| {
                sidebar.child(
                    div()
                        .mx_3()
                        .mb_3()
                        .px_3()
                        .py_2()
                        .rounded(px(9.))
                        .bg(theme.background.opacity(0.55))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(
                            h_flex()
                                .items_start()
                                .gap_2()
                                .child(
                                    div()
                                        .mt(px(5.))
                                        .size(px(6.))
                                        .flex_shrink_0()
                                        .rounded(px(999.))
                                        .bg(theme.danger),
                                )
                                .child(div().line_clamp(2).child(self.status.text.clone())),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_playlist_header(
        &mut self,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let Some(playlist) = self.selected_playlist.clone() else {
            return div()
                .h(px(220.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("从左侧选择一个歌单")
                .into_any_element();
        };
        let cover_size = if narrow {
            px(112.)
        } else if compact {
            px(142.)
        } else {
            px(176.)
        };
        let cover = div().rounded(px(18.)).shadow_md().child(playlist_cover(
            &playlist,
            cover_size,
            px(18.),
            cx,
        ));
        let owned_by_profile = matches!(
            &playlist.id,
            UserPlaylistId::Liked | UserPlaylistId::Created { .. }
        );
        let owner = (!playlist.owner.is_empty())
            .then(|| playlist.owner.clone())
            .or_else(|| {
                if owned_by_profile {
                    self.profile
                        .as_ref()
                        .map(|profile| profile.nickname.clone())
                } else {
                    None
                }
            });
        let owner_avatar_url = playlist.owner_avatar_url.clone().or_else(|| {
            owned_by_profile
                .then(|| self.profile.as_ref()?.avatar_url.clone())
                .flatten()
        });
        let owner_identity = owner.zip(owner_avatar_url);
        let has_owner = owner_identity.is_some();
        let description = single_line_summary(&playlist.description);
        let has_tracks = !self.track_table.read(cx).delegate().tracks().is_empty();
        div()
            .h(if narrow {
                px(190.)
            } else if compact {
                px(214.)
            } else {
                px(246.)
            })
            .w_full()
            .px_6()
            .pt_4()
            .pb_5()
            .child(
                h_flex()
                    .size_full()
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
                                        .child(match &playlist.id {
                                            UserPlaylistId::Artist { .. } => "歌手",
                                            UserPlaylistId::Album { .. } => "专辑",
                                            _ => "歌单",
                                        }),
                                ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .w_full()
                                    .truncate()
                                    .text_size(if narrow {
                                        px(34.)
                                    } else if compact {
                                        px(40.)
                                    } else {
                                        px(52.)
                                    })
                                    .line_height(if narrow {
                                        px(45.)
                                    } else if compact {
                                        px(52.)
                                    } else {
                                        px(68.)
                                    })
                                    .font_semibold()
                                    .child(playlist.title),
                            )
                            .when(
                                playlist.id != UserPlaylistId::Liked && !description.is_empty(),
                                |this| {
                                    this.child(
                                        div()
                                            .w_full()
                                            .max_w(px(720.))
                                            .truncate()
                                            .text_sm()
                                            .text_color(theme.secondary_foreground)
                                            .child(description),
                                    )
                                },
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_2()
                                    .text_sm()
                                    .font_medium()
                                    .when_some(owner_identity, |this, (owner, url)| {
                                        this.child(
                                            img(cached_image_source(url))
                                                .size(px(18.))
                                                .flex_shrink_0()
                                                .rounded(px(999.)),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .max_w(if narrow { px(120.) } else { px(200.) })
                                                .truncate()
                                                .child(owner),
                                        )
                                    })
                                    .child(
                                        div()
                                            .font_normal()
                                            .text_color(theme.secondary_foreground)
                                            .child(format!(
                                                "{}{} 首歌曲",
                                                if has_owner { "· " } else { "" },
                                                playlist.track_count
                                            )),
                                    ),
                            )
                            .child(
                                h_flex().pt_2().child(
                                    Button::new("play-all")
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
                                                    self.settings.color_theme.icon_on_accent(),
                                                    px(17.),
                                                ))
                                                .child("播放全部"),
                                        )
                                        .disabled(!has_tracks)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.select_track(0, cx)),
                                        ),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_playlist_content(
        &mut self,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme.background)
            .child(self.render_playlist_header(compact, narrow, cx))
            .child(
                div().flex_1().min_h_0().px_5().pb_4().child(
                    div()
                        .size_full()
                        .overflow_hidden()
                        .bg(theme.background)
                        .child(
                            DataTable::new(&self.track_table)
                                .bordered(false)
                                .stripe(false)
                                .with_size(px(64.)),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_artist_header(
        &mut self,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(artist) = self.selected_artist.clone() else {
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
            .child(self.render_search_cover(
                artist.cover_url,
                MediaIcon::Artist,
                cover_size,
                px(999.),
                cx,
            ));
        let track_count = self.artist_track_count;
        let has_tracks = self
            .artist_songs
            .as_ref()
            .is_some_and(|songs| !songs.items.is_empty());

        div()
            .h(if narrow {
                px(190.)
            } else if compact {
                px(214.)
            } else {
                px(246.)
            })
            .w_full()
            .px_6()
            .pt_4()
            .pb_5()
            .child(
                h_flex()
                    .size_full()
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
                                        px(34.)
                                    } else if compact {
                                        px(40.)
                                    } else {
                                        px(52.)
                                    })
                                    .line_height(if narrow {
                                        px(45.)
                                    } else if compact {
                                        px(52.)
                                    } else {
                                        px(68.)
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
                                                    self.settings.color_theme.icon_on_accent(),
                                                    px(17.),
                                                ))
                                                .child("播放全部"),
                                        )
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

    fn render_artist_content(
        &mut self,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let songs = self.artist_songs.clone();
        let song_has_more = songs.as_ref().is_some_and(|page| page.has_more);
        let song_body = if self.artist_songs_loading {
            h_flex()
                .h(px(92.))
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().with_size(px(22.)).color(theme.primary))
                .child("正在加载歌曲…")
                .into_any_element()
        } else if let Some(error) = self.artist_song_error.clone() {
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
                        .on_click(cx.listener(|this, _, _, cx| this.load_artist_songs(false, cx))),
                )
                .into_any_element()
        } else if let Some(songs) = songs.filter(|page| !page.items.is_empty()) {
            self.render_song_rows(songs.items, narrow, SongRowSource::Artist, cx)
        } else {
            h_flex()
                .h(px(72.))
                .items_center()
                .text_color(theme.muted_foreground)
                .child("暂无歌曲")
                .into_any_element()
        };

        let albums = self.artist_albums.clone();
        let album_has_more = albums.as_ref().is_some_and(|page| page.has_more);
        let album_body = if self.artist_albums_loading {
            h_flex()
                .h(px(120.))
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().with_size(px(22.)).color(theme.primary))
                .child("正在加载专辑…")
                .into_any_element()
        } else if let Some(error) = self.artist_album_error.clone() {
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
        } else if let Some(albums) = albums.filter(|page| !page.items.is_empty()) {
            self.render_search_cards(
                SearchCategory::Albums,
                Vec::new(),
                albums.items,
                Vec::new(),
                compact,
                cx,
            )
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
                        .child(self.render_artist_header(compact, narrow, cx))
                        .child(
                            v_flex()
                                .w_full()
                                .px(if narrow { px(20.) } else { px(24.) })
                                .pb_10()
                                .gap_10()
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap_3()
                                        .child(
                                            div().text_size(px(24.)).font_semibold().child("歌曲"),
                                        )
                                        .child(song_body)
                                        .when(song_has_more, |this| {
                                            this.child(
                                                h_flex().child(
                                                    Button::new("load-more-artist-songs")
                                                        .outline()
                                                        .h(px(40.))
                                                        .px_4()
                                                        .loading(self.artist_songs_loading_more)
                                                        .disabled(self.artist_songs_loading_more)
                                                        .label(if self.artist_songs_loading_more {
                                                            "正在加载…"
                                                        } else {
                                                            "查看更多"
                                                        })
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.load_artist_songs(true, cx)
                                                        })),
                                                ),
                                            )
                                        }),
                                )
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap_4()
                                        .child(
                                            div().text_size(px(24.)).font_semibold().child("专辑"),
                                        )
                                        .child(album_body)
                                        .when(album_has_more, |this| {
                                            this.child(
                                                h_flex().child(
                                                    Button::new("load-more-artist-albums")
                                                        .outline()
                                                        .h(px(40.))
                                                        .px_4()
                                                        .loading(self.artist_albums_loading_more)
                                                        .disabled(self.artist_albums_loading_more)
                                                        .label(if self.artist_albums_loading_more {
                                                            "正在加载…"
                                                        } else {
                                                            "查看更多"
                                                        })
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.load_artist_albums(true, cx)
                                                        })),
                                                ),
                                            )
                                        }),
                                ),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_home(&mut self, compact: bool, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if self.home_loading && self.home_playlists.is_empty() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().with_size(px(24.)).color(theme.primary))
                .child("正在加载主页推荐…")
                .into_any_element();
        }
        if self.home_playlists.is_empty()
            && let Some(error) = self.home_error.clone()
        {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_4()
                .text_color(theme.muted_foreground)
                .child(error)
                .child(
                    Button::new("retry-home")
                        .outline()
                        .h(px(44.))
                        .px_4()
                        .label("重新加载")
                        .on_click(cx.listener(|this, _, _, cx| this.load_home(cx))),
                )
                .into_any_element();
        }
        if self.home_loaded && self.home_playlists.is_empty() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("QQ 音乐暂时没有返回可显示的歌单推荐")
                .into_any_element();
        }

        let cover_size = if narrow {
            px(132.)
        } else if compact {
            px(148.)
        } else {
            px(168.)
        };
        let card_width = cover_size + px(16.);
        let feature_width = if narrow {
            px(304.)
        } else if compact {
            px(344.)
        } else {
            px(384.)
        };
        let grid_width = if narrow {
            px(640.)
        } else if compact {
            px(704.)
        } else {
            px(784.)
        };
        let cards = self
            .home_playlists
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, playlist)| {
                let cover = playlist_cover(&playlist, cover_size, px(14.), cx);
                let title = playlist.title.clone();
                let subtitle = if playlist.description.is_empty() {
                    "为你推荐".to_owned()
                } else {
                    playlist.description.clone()
                };
                Button::new(format!("home-playlist-{index}"))
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
                            .child(div().rounded(px(14.)).shadow_sm().child(cover))
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
            })
            .collect::<Vec<_>>();
        let recommendation_loading = self.home_recommendation_loading.is_some();
        let radar_loading = self.home_recommendation_loading == Some(RecommendationKind::Radar);
        let guess_loading = self.home_recommendation_loading == Some(RecommendationKind::Guess);
        let radar_icon = if radar_loading {
            Spinner::new()
                .with_size(px(24.))
                .color(theme.primary)
                .into_any_element()
        } else {
            media_icon_hsla(MediaIcon::Radar, theme.primary, px(25.))
        };
        let guess_icon = if guess_loading {
            Spinner::new()
                .with_size(px(24.))
                .color(theme.primary)
                .into_any_element()
        } else {
            media_icon_hsla(MediaIcon::Headphones, theme.primary, px(25.))
        };
        let recommendation_cards = [
            Button::new("home-radar")
                .ghost()
                .w(feature_width)
                .h(px(92.))
                .p_4()
                .rounded(px(12.))
                .bg(theme.muted.opacity(0.7))
                .tooltip("播放专属雷达")
                .disabled(recommendation_loading)
                .child(
                    h_flex()
                        .size_full()
                        .gap_4()
                        .child(
                            div()
                                .size(px(48.))
                                .flex_shrink_0()
                                .rounded(px(10.))
                                .bg(theme.background.opacity(0.55))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(radar_icon),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .items_start()
                                .gap_1()
                                .child(div().font_semibold().child("专属雷达"))
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("不断更新的个性推荐"),
                                ),
                        ),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.start_home_recommendation(RecommendationKind::Radar, cx)
                })),
            Button::new("home-guess")
                .ghost()
                .w(feature_width)
                .h(px(92.))
                .p_4()
                .rounded(px(12.))
                .bg(theme.muted.opacity(0.7))
                .tooltip("播放猜你喜欢")
                .disabled(recommendation_loading)
                .child(
                    h_flex()
                        .size_full()
                        .gap_4()
                        .child(
                            div()
                                .size(px(48.))
                                .flex_shrink_0()
                                .rounded(px(10.))
                                .bg(theme.background.opacity(0.55))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(guess_icon),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .items_start()
                                .gap_1()
                                .child(div().font_semibold().child("猜你喜欢"))
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("持续生成的个性漫游"),
                                ),
                        ),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.start_home_recommendation(RecommendationKind::Guess, cx)
                })),
        ];

        div()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .w_full()
                    .px(if narrow { px(20.) } else { px(28.) })
                    .pt(if narrow { px(22.) } else { px(32.) })
                    .pb_8()
                    .child(
                        h_flex().w_full().justify_center().child(
                            v_flex()
                                .w_full()
                                .max_w(grid_width)
                                .gap_6()
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap_3()
                                        .child(
                                            div()
                                                .px_2()
                                                .text_size(if narrow { px(22.) } else { px(24.) })
                                                .font_semibold()
                                                .child("专属推荐"),
                                        )
                                        .child(
                                            h_flex()
                                                .items_start()
                                                .flex_wrap()
                                                .gap_4()
                                                .children(recommendation_cards),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .w_full()
                                        .px_2()
                                        .child(
                                            div()
                                                .text_size(if narrow { px(22.) } else { px(24.) })
                                                .font_semibold()
                                                .child("推荐歌单"),
                                        )
                                        .when_some(self.home_error.clone(), |header, error| {
                                            header.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(error),
                                            )
                                        }),
                                )
                                .child(h_flex().items_start().flex_wrap().gap_4().children(cards)),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_search_cover(
        &self,
        cover_url: Option<String>,
        icon: MediaIcon,
        size: Pixels,
        radius: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        match cover_url {
            Some(url) => img(cached_image_source(url))
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

    fn render_search_songs(
        &mut self,
        songs: Vec<Track>,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_song_rows(songs, narrow, SongRowSource::Search, cx)
    }

    fn render_song_rows(
        &mut self,
        songs: Vec<Track>,
        narrow: bool,
        source: SongRowSource,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let current_mid = self.current_track_data().map(|track| track.mid.clone());
        let loading_mid = self
            .loading_track
            .and_then(|index| self.playback_queue.as_ref()?.tracks.get(index))
            .map(|track| track.mid.clone());
        let is_playing = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
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
                let cover = self.render_search_cover(
                    track.cover_url,
                    MediaIcon::Music,
                    px(48.),
                    px(9.),
                    cx,
                );
                Button::new(format!(
                    "{}-song-{}-{}",
                    match source {
                        SongRowSource::Search => "search",
                        SongRowSource::Artist => "artist",
                    },
                    track.mid,
                    index
                ))
                .ghost()
                .w_full()
                .h(px(68.))
                .px_3()
                .rounded(px(10.))
                .selected(is_current)
                .tooltip(format!("播放 {title}"))
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
                                            Spinner::new().with_size(px(17.)).color(theme.primary),
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
                                .font_family("monospace")
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(duration),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| match source {
                    SongRowSource::Search => this.select_search_track(index, cx),
                    SongRowSource::Artist => this.select_artist_track(index, cx),
                }))
            })
            .collect::<Vec<_>>();
        v_flex().w_full().gap_1().children(rows).into_any_element()
    }

    fn render_search_cards(
        &mut self,
        category: SearchCategory,
        artists: Vec<SearchArtist>,
        albums: Vec<SearchAlbum>,
        playlists: Vec<UserPlaylist>,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let cover_size = if compact { px(132.) } else { px(148.) };
        let card_width = cover_size + px(16.);
        let cards = match category {
            SearchCategory::Artists => artists
                .into_iter()
                .enumerate()
                .map(|(index, artist)| {
                    let title = artist.name.clone();
                    let cover = self.render_search_cover(
                        artist.cover_url.clone(),
                        MediaIcon::Artist,
                        cover_size,
                        px(999.),
                        cx,
                    );
                    Button::new(format!("search-artist-{index}"))
                        .ghost()
                        .w(card_width)
                        .h(cover_size + px(62.))
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
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_search_artist(artist.clone(), window, cx)
                        }))
                        .into_any_element()
                })
                .collect::<Vec<_>>(),
            SearchCategory::Albums => albums
                .into_iter()
                .enumerate()
                .map(|(index, album)| {
                    let title = album.title.clone();
                    let subtitle = album.artist.clone();
                    let cover = self.render_search_cover(
                        album.cover_url.clone(),
                        MediaIcon::Album,
                        cover_size,
                        px(12.),
                        cx,
                    );
                    let playlist = album.into_playlist();
                    Button::new(format!("search-album-{index}"))
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
                .collect::<Vec<_>>(),
            SearchCategory::Playlists => playlists
                .into_iter()
                .enumerate()
                .map(|(index, playlist)| {
                    let title = playlist.title.clone();
                    let subtitle = if playlist.owner.is_empty() {
                        "QQ 音乐歌单".to_owned()
                    } else {
                        playlist.owner.clone()
                    };
                    let cover = self.render_search_cover(
                        playlist.cover_url.clone(),
                        MediaIcon::Playlist,
                        cover_size,
                        px(12.),
                        cx,
                    );
                    Button::new(format!("search-playlist-{index}"))
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
                .collect::<Vec<_>>(),
            SearchCategory::Songs => Vec::new(),
        };
        h_flex()
            .w_full()
            .items_start()
            .flex_wrap()
            .gap_4()
            .children(cards)
            .into_any_element()
    }

    fn render_search(&mut self, compact: bool, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let active_category = self.search_category;
        let tabs = SearchCategory::ALL
            .into_iter()
            .map(|category| {
                Button::new(format!("search-category-{}", category.label()))
                    .ghost()
                    .h(px(40.))
                    .px_3()
                    .rounded(px(9.))
                    .selected(active_category == category)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(media_icon_hsla(
                                category.icon(),
                                if active_category == category {
                                    theme.primary
                                } else {
                                    theme.secondary_foreground
                                },
                                px(17.),
                            ))
                            .child(category.label()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.search_category = category;
                        this.search_error = None;
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();

        let mut content = v_flex().w_full();
        let mut has_more = false;
        let mut is_empty = false;
        if let Some(results) = self.search_results.clone() {
            match self.search_category {
                SearchCategory::Songs => {
                    has_more = results.songs.has_more;
                    is_empty = results.songs.items.is_empty();
                    content =
                        content.child(self.render_search_songs(results.songs.items, narrow, cx));
                }
                SearchCategory::Artists => {
                    has_more = results.artists.has_more;
                    is_empty = results.artists.items.is_empty();
                    content = content.child(self.render_search_cards(
                        SearchCategory::Artists,
                        results.artists.items,
                        Vec::new(),
                        Vec::new(),
                        compact,
                        cx,
                    ));
                }
                SearchCategory::Albums => {
                    has_more = results.albums.has_more;
                    is_empty = results.albums.items.is_empty();
                    content = content.child(self.render_search_cards(
                        SearchCategory::Albums,
                        Vec::new(),
                        results.albums.items,
                        Vec::new(),
                        compact,
                        cx,
                    ));
                }
                SearchCategory::Playlists => {
                    has_more = results.playlists.has_more;
                    is_empty = results.playlists.items.is_empty();
                    content = content.child(self.render_search_cards(
                        SearchCategory::Playlists,
                        Vec::new(),
                        Vec::new(),
                        results.playlists.items,
                        compact,
                        cx,
                    ));
                }
            }
        }

        v_flex()
            .flex_1()
            .min_h_0()
            .bg(theme.background)
            .child(
                div().flex_1().min_h_0().overflow_y_scrollbar().child(
                    v_flex()
                        .w_full()
                        .max_w(px(1120.))
                        .mx_auto()
                        .px(if narrow { px(20.) } else { px(32.) })
                        .pt(if narrow { px(20.) } else { px(28.) })
                        .pb_8()
                        .gap_5()
                        .child(
                            v_flex().child(
                                div()
                                    .text_size(if narrow { px(22.) } else { px(24.) })
                                    .font_semibold()
                                    .child(format!("搜索“{}”", self.search_query)),
                            ),
                        )
                        .child(h_flex().gap_1().children(tabs))
                        .when(self.search_loading, |this| {
                            this.child(
                                v_flex()
                                    .h(px(260.))
                                    .items_center()
                                    .justify_center()
                                    .gap_3()
                                    .text_color(theme.muted_foreground)
                                    .child(Spinner::new().with_size(px(24.)).color(theme.primary))
                                    .child("正在搜索…"),
                            )
                        })
                        .when(
                            !self.search_loading
                                && self.search_results.is_none()
                                && self.search_error.is_some(),
                            |this| {
                                this.child(
                                    v_flex()
                                        .h(px(260.))
                                        .items_center()
                                        .justify_center()
                                        .gap_4()
                                        .text_color(theme.muted_foreground)
                                        .child(self.search_error.clone().unwrap_or_default())
                                        .child(
                                            Button::new("retry-search")
                                                .outline()
                                                .h(px(44.))
                                                .px_4()
                                                .label("重新搜索")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.start_search(this.search_query.clone(), cx)
                                                })),
                                        ),
                                )
                            },
                        )
                        .when(
                            !self.search_loading && self.search_results.is_some() && is_empty,
                            |this| {
                                this.child(
                                    div()
                                        .h(px(220.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_color(theme.muted_foreground)
                                        .child(format!(
                                            "没有找到相关{}",
                                            self.search_category.label()
                                        )),
                                )
                            },
                        )
                        .when(
                            !self.search_loading && self.search_results.is_some() && !is_empty,
                            |this| this.child(content),
                        )
                        .when(
                            self.search_results.is_some() && self.search_error.is_some(),
                            |this| {
                                this.child(
                                    div()
                                        .px_3()
                                        .py_2()
                                        .rounded(px(9.))
                                        .bg(theme.danger.opacity(0.1))
                                        .text_sm()
                                        .text_color(theme.danger)
                                        .child(self.search_error.clone().unwrap_or_default()),
                                )
                            },
                        )
                        .when(has_more, |this| {
                            this.child(
                                h_flex().w_full().justify_center().pt_2().child(
                                    Button::new("load-more-search")
                                        .outline()
                                        .h(px(44.))
                                        .px_5()
                                        .label("加载更多")
                                        .loading(self.search_loading_more)
                                        .disabled(self.search_loading_more)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.load_more_search(cx)),
                                        ),
                                ),
                            )
                        }),
                ),
            )
            .into_any_element()
    }

    fn render_quality_selector(&mut self, has_track: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let active_quality = self.active_quality;
        let active_label = active_quality.badge_label();
        let options = self
            .available_qualities
            .iter()
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
                    .disabled(!has_track || self.available_qualities.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.quality_menu_open = !this.quality_menu_open;
                        this.account_menu_open = false;
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

    fn render_player_bar(
        &mut self,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let track = self.current_track_data().cloned();
        let has_track = track.is_some();
        let quality_selector = self.render_quality_selector(has_track, cx);
        let theme = cx.theme();
        let is_playing = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
        let display_position = self.seek_preview.unwrap_or(self.position);
        let duration = self.current_duration().unwrap_or_default();
        let icon_foreground = self.settings.color_theme.icon_foreground();
        let icon_accent = self.settings.color_theme.icon_accent();
        let cover_size = if narrow { px(44.) } else { px(52.) };
        let cover = match track.as_ref().and_then(|track| track.cover_url.clone()) {
            Some(url) => img(cached_image_source(url))
                .size(cover_size)
                .flex_shrink_0()
                .rounded(px(10.))
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
        let (title, album, artists) = track
            .map(|track| (track.title, track.album, track.artists))
            .unwrap_or_else(|| {
                (
                    "尚未播放".to_owned(),
                    "从播放列表中双击一首歌曲".to_owned(),
                    String::new(),
                )
            });

        h_flex()
            .h(px(112.))
            .w_full()
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.group_box)
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
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_1()
                            .child(div().truncate().font_medium().child(title))
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(theme.secondary_foreground)
                                    .child(if artists.is_empty() {
                                        album
                                    } else if compact || album.is_empty() {
                                        artists
                                    } else {
                                        format!("{artists} · {album}")
                                    }),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(280.))
                    .items_center()
                    .justify_center()
                    .gap_1()
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
                                            .toggled(self.shuffle)
                                            .selected(self.shuffle)
                                            .disabled(
                                                self.playback_queue
                                                    .as_ref()
                                                    .is_none_or(|queue| queue.tracks.is_empty()),
                                            )
                                            .child(div().w(px(28.)).flex().justify_center().child(
                                                media_icon(
                                                    MediaIcon::Shuffle,
                                                    if self.shuffle {
                                                        icon_accent
                                                    } else {
                                                        icon_foreground
                                                    },
                                                    px(18.),
                                                ),
                                            ))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.shuffle = !this.shuffle;
                                                cx.notify();
                                            })),
                                    )
                                    .when(self.shuffle, |this| {
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
                                    .disabled(self.current_track.is_none())
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
                                    .tooltip(if self.loading_track.is_some() {
                                        "正在加载"
                                    } else if is_playing {
                                        "暂停"
                                    } else {
                                        "播放"
                                    })
                                    .disabled(
                                        self.loading_track.is_some()
                                            || (self.current_track.is_none()
                                                && self
                                                    .track_table
                                                    .read(cx)
                                                    .delegate()
                                                    .tracks()
                                                    .is_empty()),
                                    )
                                    .child(media_icon(
                                        if self.loading_track.is_some() {
                                            MediaIcon::Loading
                                        } else if is_playing {
                                            MediaIcon::Pause
                                        } else {
                                            MediaIcon::Play
                                        },
                                        self.settings.color_theme.icon_on_accent(),
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
                                    .disabled(self.current_track.is_none())
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
                                            .tooltip(self.repeat_mode.label())
                                            .toggled(self.repeat_mode != RepeatMode::Off)
                                            .selected(self.repeat_mode != RepeatMode::Off)
                                            .disabled(
                                                self.playback_queue
                                                    .as_ref()
                                                    .is_none_or(|queue| queue.tracks.is_empty()),
                                            )
                                            .child(div().w(px(28.)).flex().justify_center().child(
                                                media_icon(
                                                    if self.repeat_mode == RepeatMode::One {
                                                        MediaIcon::RepeatOne
                                                    } else {
                                                        MediaIcon::Repeat
                                                    },
                                                    if self.repeat_mode != RepeatMode::Off {
                                                        icon_accent
                                                    } else {
                                                        icon_foreground
                                                    },
                                                    px(18.),
                                                ),
                                            ))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.repeat_mode = this.repeat_mode.next();
                                                cx.notify();
                                            })),
                                    )
                                    .when(self.repeat_mode != RepeatMode::Off, |this| {
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
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .max_w(px(620.))
                            .gap_2()
                            .child(
                                div()
                                    .w(px(44.))
                                    .text_right()
                                    .font_family("monospace")
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format_duration(display_position.as_secs())),
                            )
                            .child(
                                Slider::new(&self.progress_slider)
                                    .flex_1()
                                    .disabled(!has_track || self.loading_track.is_some()),
                            )
                            .child(
                                div()
                                    .w(px(44.))
                                    .font_family("monospace")
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format_duration(duration.as_secs())),
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
            })
            .into_any_element()
    }

    fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let compact = window.viewport_size().width < px(1120.);
        let narrow = window.viewport_size().width < px(900.);
        let popover_open = self.account_menu_open || self.quality_menu_open;
        self.track_table.update(cx, |table, cx| {
            if table.delegate_mut().set_compact(compact) {
                table.refresh(cx);
            }
        });
        let (default_sidebar_width, min_sidebar_width, max_sidebar_width) = if narrow {
            (216., 196., 240.)
        } else if compact {
            (248., 220., 300.)
        } else {
            (272., 236., 340.)
        };
        let sidebar_width = px(self
            .settings
            .sidebar_width
            .map(|width| width as f32)
            .unwrap_or(default_sidebar_width)
            .clamp(min_sidebar_width, max_sidebar_width));
        let sidebar_range = px(min_sidebar_width)..px(max_sidebar_width);
        let sidebar = self.render_sidebar(cx);
        let page = match self.main_content {
            MainContent::Home => self.render_home(compact, narrow, cx),
            MainContent::Search => self.render_search(compact, narrow, cx),
            MainContent::Artist => self.render_artist_content(compact, narrow, cx),
            MainContent::Playlist => self.render_playlist_content(compact, narrow, cx),
        };
        let search_width = if narrow {
            px(268.)
        } else if compact {
            px(376.)
        } else {
            px(480.)
        };
        let home_selected = self.main_content == MainContent::Home;
        let history_navigation = h_flex()
            .gap_1()
            .child(
                Button::new("navigate-back")
                    .ghost()
                    .rounded(px(999.))
                    .size(px(44.))
                    .p_0()
                    .tooltip("返回")
                    .disabled(self.navigation_history.back.is_empty())
                    .child(media_icon_hsla(
                        MediaIcon::Back,
                        theme.secondary_foreground,
                        px(22.),
                    ))
                    .on_click(cx.listener(|this, _, window, cx| this.navigate_back(window, cx))),
            )
            .child(
                Button::new("navigate-forward")
                    .ghost()
                    .rounded(px(999.))
                    .size(px(44.))
                    .p_0()
                    .tooltip("前进")
                    .disabled(self.navigation_history.forward.is_empty())
                    .child(media_icon_hsla(
                        MediaIcon::Forward,
                        theme.secondary_foreground,
                        px(22.),
                    ))
                    .on_click(cx.listener(|this, _, window, cx| this.navigate_forward(window, cx))),
            );
        let navigation = h_flex()
            .gap_3()
            .child(
                Button::new("home")
                    .ghost()
                    .rounded(px(999.))
                    .size(px(44.))
                    .p_0()
                    .tooltip("主页")
                    .child(media_icon_hsla(
                        MediaIcon::Home,
                        if home_selected {
                            theme.primary
                        } else {
                            theme.secondary_foreground
                        },
                        px(24.),
                    ))
                    .on_click(cx.listener(|this, _, window, cx| this.show_home(window, cx))),
            )
            .child(
                div()
                    .id("search-input")
                    .on_mouse_down_out(|_, window, _| window.blur())
                    .child(
                        Input::new(&self.search_input)
                            .large()
                            .w(search_width)
                            .border_2()
                            .rounded(px(999.))
                            .text_size(px(16.))
                            .aria_label("搜索")
                            .prefix(media_icon_hsla(
                                MediaIcon::Search,
                                theme.muted_foreground,
                                px(22.),
                            )),
                    ),
            );
        let account = self.render_account(cx);
        let content = v_flex()
            .h_full()
            .min_w_0()
            .flex_1()
            .child(
                div()
                    .relative()
                    .h(px(72.))
                    .w_full()
                    .flex_shrink_0()
                    .child(
                        h_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .child(navigation),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(14.))
                            .left(px(24.))
                            .child(history_navigation),
                    )
                    .child(div().absolute().top(px(17.)).right(px(24.)).child(account)),
            )
            .child(page);
        v_flex()
            .relative()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("library-layout")
                        .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                            if let Some(width) = state.read(cx).sizes().first() {
                                this.settings.sidebar_width =
                                    Some(f32::from(*width).round() as u32);
                            }
                        }))
                        .child(
                            resizable_panel()
                                .size(sidebar_width)
                                .size_range(sidebar_range)
                                .flex_none()
                                .child(sidebar),
                        )
                        .child(
                            resizable_panel()
                                .size_range(px(480.)..Pixels::MAX)
                                .child(content),
                        ),
                ),
            )
            .child(self.render_player_bar(compact, narrow, cx))
            .when(popover_open, |this| {
                this.child(
                    deferred(
                        div()
                            .id("popover-dismiss-layer")
                            .absolute()
                            .inset_0()
                            .on_click(cx.listener(|this, _, _, cx| this.dismiss_popovers(cx))),
                    )
                    .with_priority(5),
                )
            })
            .into_any_element()
    }
}

impl Drop for LyruneView {
    fn drop(&mut self) {
        self.persist_current_playback();
        if let Some(audio) = &self.audio {
            audio.stop();
        }
        self.persist_settings();
        self.persist_library_cache();
        self.library_cache_saves.close();
        if let Some(task) = self.library_cache_writer.take() {
            let _ = RUNTIME.block_on(task);
        }
        if let Some(task) = self.cdn_maintenance.take() {
            task.abort();
        }
    }
}

impl Render for LyruneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.account_state == AccountState::SignedIn {
            self.render_main(window, cx)
        } else {
            self.render_login(cx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NavigationHistory, NavigationPage, SearchCategory, insert_track_after_current};
    use qqmusic_api::integration::{Track, UserPlaylist, UserPlaylistId};

    fn track(mid: &str) -> Track {
        Track {
            song_id: None,
            mid: mid.to_owned(),
            media_mid: None,
            standard_size_bytes: None,
            high_size_bytes: None,
            lossless_size_bytes: None,
            hi_res_size_bytes: None,
            atmos_stereo_size_bytes: None,
            atmos_surround_size_bytes: None,
            master_size_bytes: None,
            title: mid.to_owned(),
            artists: String::new(),
            album: String::new(),
            album_mid: String::new(),
            cover_url: None,
            duration_seconds: 180,
            added_at: None,
        }
    }

    fn mids(tracks: &[Track]) -> Vec<&str> {
        tracks.iter().map(|track| track.mid.as_str()).collect()
    }

    fn playlist(diss_id: u64) -> NavigationPage {
        NavigationPage::Playlist {
            playlist: UserPlaylist {
                id: UserPlaylistId::Favorite { diss_id },
                title: format!("playlist-{diss_id}"),
                cover_url: None,
                description: String::new(),
                owner: String::new(),
                owner_avatar_url: None,
                track_count: 0,
            },
            selected_index: None,
        }
    }

    #[test]
    fn new_navigation_after_back_clears_the_forward_branch() {
        let first = playlist(1);
        let second = playlist(2);
        let home = NavigationPage::Home;
        let mut history = NavigationHistory::default();

        history.record(Some(first.clone()), &home);
        let back = history.go_back(Some(home.clone())).unwrap();
        assert!(back.same_destination(&first));

        let forward = history.go_forward(Some(first.clone())).unwrap();
        assert!(forward.same_destination(&home));

        let back = history.go_back(Some(home)).unwrap();
        assert!(back.same_destination(&first));
        history.record(Some(first), &second);

        assert!(history.forward.is_empty());
        let back = history.go_back(Some(second)).unwrap();
        assert!(back.same_destination(&playlist(1)));
    }

    #[test]
    fn playlist_description_is_collapsed_to_one_line() {
        assert_eq!(
            super::single_line_summary("first line\n second\tline\r\nthird"),
            "first line second line third"
        );
    }

    #[test]
    fn search_history_keeps_the_active_category_without_splitting_one_query() {
        let songs = NavigationPage::Search {
            query: "周杰伦".to_owned(),
            category: SearchCategory::Songs,
        };
        let albums = NavigationPage::Search {
            query: "周杰伦".to_owned(),
            category: SearchCategory::Albums,
        };
        assert!(songs.same_destination(&albums));

        let target = playlist(42);
        let mut history = NavigationHistory::default();
        history.record(Some(albums.clone()), &target);
        let restored = history.go_back(Some(target)).expect("search history entry");
        assert!(matches!(
            restored,
            NavigationPage::Search {
                query,
                category: SearchCategory::Albums,
            } if query == "周杰伦"
        ));
    }

    #[test]
    fn search_categories_follow_the_product_order() {
        assert_eq!(
            SearchCategory::ALL.map(SearchCategory::label),
            ["单曲", "歌单", "专辑", "歌手"]
        );
    }

    #[test]
    fn search_track_is_inserted_after_the_current_track() {
        let mut tracks = vec![track("A"), track("B")];

        let inserted = insert_track_after_current(&mut tracks, Some(0), track("C"));

        assert_eq!(inserted, 1);
        assert_eq!(mids(&tracks), ["A", "C", "B"]);
    }

    #[test]
    fn existing_search_track_is_moved_without_duplication() {
        let mut tracks = vec![track("C"), track("A"), track("B")];

        let inserted = insert_track_after_current(&mut tracks, Some(1), track("C"));

        assert_eq!(inserted, 1);
        assert_eq!(mids(&tracks), ["A", "C", "B"]);
    }
}
