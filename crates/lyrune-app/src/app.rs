mod account;
mod artist;
mod home;
mod lyrics;
mod lyrics_loading;
mod lyrics_view;
mod navigation;
mod playback;
mod playlists;
mod queue;
mod search;
mod settings;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use account::AccountState;
use futures_util::{
    future::{Either, select},
    pin_mut,
};
use gpui::{prelude::*, *};
use gpui_base::{
    Slider as BaseSlider, SliderIndicator, SliderTrack,
    motion::{Transition, transition},
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, ResizableState, Root, Selectable as _, Sizable as _,
    StyledExt as _, Theme, WindowExt as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex, h_resizable,
    input::{Input, InputEvent, InputState, MaskPattern},
    list::{List, ListEvent, ListState},
    notification::Notification,
    resizable_panel,
    slider::{Slider, SliderEvent, SliderState},
    table::{TableEvent, TableState},
    v_flex,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::cache::{AudioCache, audio_cache_limit_bytes};
use crate::design::{AppFonts, ColorTheme};
use crate::http::{CachedImageCache, cached_image_source};
use crate::icons::{MediaIcon, lyrune_icon, media_icon, media_icon_hsla};
#[cfg(target_os = "linux")]
use crate::inhibit::InhibitHandle;
use crate::library::{
    PlaylistListDelegate, SearchCardGridDelegate, TrackTableDelegate, TrackTableEvent,
};
use crate::lyrics_cache::LyricDiskCache;
#[cfg(target_os = "linux")]
use crate::mpris::{
    MprisCommand, MprisHandle, MprisLoopStatus, MprisPlaybackStatus, MprisSnapshot, MprisTrack,
};
use crate::player::AudioPlayer;
use crate::runtime::RUNTIME;
use crate::settings::{
    AppSettings, CdnCacheStore, LibraryCache, MIN_SCALE_PERCENT, PersistedPlayback,
    PersistedQueueContinuation, PersistedWindowSize, SettingsStore, WindowDecoration,
};
use crate::singleflight::SingleFlight;
use crate::update::AvailableUpdate;
use lyrics_loading::MemoryLyrics;
use lyrics_view::{
    CachedLyricsPanel, LYRIC_BACKGROUND_OVERLAY_OPACITY, LYRIC_EXPANSION_DURATION,
    LyricLayoutCache, LyricMotionState, interpolate_color,
};
use navigation::{
    MainContent, NavigationHistory, PageResourceCache, PlaylistScrollPosition, SearchCategory,
    SearchVisibleCounts, SharedArtistResource, SharedPlaylistResource, SharedSearchResource,
};
use playback::PlaybackLocation;
use playlists::PlaylistPageKey;
use qqmusic_api::integration::{
    CredentialSession, PlaylistPage, ProtocolClient, Quality, RecommendationKind, SearchAlbum,
    SearchArtist, UserPlaylist, UserProfile,
};
use queue::{PlaybackQueue, RepeatMode};
#[cfg(target_os = "linux")]
use xxhash_rust::xxh3::xxh3_128;

const PAGE_SIZE: u64 = 100;
const ARTIST_PAGE_SIZE: u64 = 5;
const ARTIST_ALBUM_PAGE_SIZE: u64 = 10;
const SEARCH_PAGE_SIZE: usize = 20;
const PROGRESS_TICK: Duration = Duration::from_millis(250);
const PROGRESS_THUMB_RADIUS: Pixels = px(8.);
const PLAYBACK_PERSIST_INTERVAL: Duration = Duration::from_secs(5);
const CDN_REFRESH_RETRY: Duration = Duration::from_secs(60);
const LIBRARY_CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const PLAYER_BAR_HEIGHT: f32 = 112.;

fn page_horizontal_padding(narrow: bool) -> Pixels {
    if narrow { px(20.) } else { px(24.) }
}

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

fn format_playback_time(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 60 * 60 {
        format!(
            "{}:{:02}:{:02}",
            seconds / (60 * 60),
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
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

pub struct LyruneView {
    account_state: AccountState,
    credential: Option<CredentialSession>,
    profile: Option<UserProfile>,
    qr_image: Option<Arc<Image>>,
    library_loading: bool,
    selected_playlist_index: Option<usize>,
    selected_playlist: Option<UserPlaylist>,
    selected_playlist_resource: Option<SharedPlaylistResource>,
    pending_playlist_scroll_position: Option<PlaylistScrollPosition>,
    library_generation: u64,
    playlist_force_refresh: bool,
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
    search_resource: Option<SharedSearchResource>,
    search_visible_counts: SearchVisibleCounts,
    search_category: SearchCategory,
    search_scroll_offsets: [Pixels; 4],
    pending_search_scroll: bool,
    selected_artist: Option<SearchArtist>,
    artist_resource: Option<SharedArtistResource>,
    artist_visible_song_count: usize,
    artist_visible_album_count: usize,

    playlist_list: Entity<ListState<PlaylistListDelegate>>,
    track_table: Entity<TableState<TrackTableDelegate>>,
    search_track_table: Entity<TableState<TrackTableDelegate>>,
    search_card_grid: Entity<ListState<SearchCardGridDelegate>>,
    search_input: Entity<InputState>,
    ui_font_input: Entity<InputState>,
    monospace_font_input: Entity<InputState>,
    lyric_font_input: Entity<InputState>,
    audio_cache_limit_input: Entity<InputState>,
    image_cache_capacity_input: Entity<InputState>,
    navigation_history_limit_input: Entity<InputState>,
    lyric_font_scale_input: Entity<InputState>,
    lyric_line_spacing_input: Entity<InputState>,
    settings_scroll_handle: ScrollHandle,
    progress_slider: Entity<SliderState>,
    volume_slider: Entity<SliderState>,
    image_cache: Entity<CachedImageCache>,
    window_drag_pressed: bool,

    audio: Option<AudioPlayer>,
    audio_cache: Option<AudioCache>,
    protocol_client: Option<ProtocolClient>,
    cdn_maintenance: Option<JoinHandle<()>>,
    audio_cache_maintenance: Option<JoinHandle<()>>,
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
    progress_hovered: bool,
    progress_hover_fraction: Option<f32>,
    cover_backdrop_expanded: bool,
    cover_backdrop_visible: bool,
    cover_backdrop_fully_expanded: bool,
    backdrop_current_url: Option<String>,
    backdrop_previous_url: Option<String>,
    backdrop_crossfade_phase: bool,
    lyric_disk_cache: Option<LyricDiskCache>,
    lyrics_cache: HashMap<String, MemoryLyrics>,
    pending_lyrics_cache: HashMap<String, MemoryLyrics>,
    lyric_layout_cache: LyricLayoutCache,
    lyric_motion_state: Option<LyricMotionState>,
    cached_lyrics_panel: Option<CachedLyricsPanel>,
    lyric_panel_frame_pending: bool,
    pending_lyric_reveal_mid: Option<String>,
    lyric_reveal_frame_pending: bool,
    lyrics_loading: HashSet<String>,
    lyrics_errors: HashMap<String, String>,
    fonts: AppFonts,
    color_theme: ColorTheme,
    settings: AppSettings,
    library_cache: LibraryCache,
    page_resource_cache: PageResourceCache,
    liked_tracks: HashMap<String, bool>,
    liked_state_loading: HashSet<String>,
    liked_toggle_loading: HashSet<String>,
    update_check_in_progress: bool,
    shuffle: bool,
    repeat_mode: RepeatMode,
    pending_playback_restore: Option<PersistedPlayback>,
    last_playback_persisted_at: Instant,

    login_message: String,
    window_handle: AnyWindowHandle,
    login_generation: u64,
    play_generation: u64,
    account_menu_open: bool,
    _subscriptions: Vec<Subscription>,
    _window_subscription: Option<Subscription>,
    _appearance_subscription: Option<Subscription>,
    window_tick_wake: Option<mpsc::Sender<()>>,
    background_tick_wake: Option<mpsc::Sender<()>>,
    lyric_animation_frame_pending: bool,
    next_lyric_highlight_frame: Option<Instant>,
    next_lyric_scroll_frame: Option<Instant>,
    #[cfg(target_os = "linux")]
    mpris: Option<MprisHandle>,
    #[cfg(target_os = "linux")]
    inhibit: Option<InhibitHandle>,
    #[cfg(target_os = "linux")]
    last_mpris_position_sync: Instant,
}

impl LyruneView {
    pub(crate) fn new(
        window: &mut Window,
        settings: AppSettings,
        fonts: AppFonts,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut initial_errors = Vec::new();
        let audio = match AudioPlayer::new() {
            Ok(player) => {
                player.set_volume(settings.volume);
                Some(player)
            }
            Err(error) => {
                initial_errors.push(format!(
                    "音频设备初始化失败：{error:#}；仍可浏览 QQ 音乐歌单"
                ));
                None
            }
        };
        let audio_cache =
            match AudioCache::new(audio_cache_limit_bytes(settings.audio_cache_limit_gb)) {
                Ok(cache) => Some(cache),
                Err(error) => {
                    initial_errors.push(format!("音频缓存初始化失败：{error:#}"));
                    None
                }
            };
        let lyric_disk_cache = LyricDiskCache::new().ok();
        let cdn_cache = match CdnCacheStore::load() {
            Ok(cache) => cache,
            Err(error) => {
                initial_errors.push(format!("CDN 缓存读取失败：{error:#}"));
                Default::default()
            }
        };
        let protocol_client = match ProtocolClient::new_with_cdn_cache(cdn_cache) {
            Ok(client) => Some(client),
            Err(error) => {
                initial_errors.push(format!("QQ 音乐客户端初始化失败：{error:#}"));
                None
            }
        };
        let playback_quality = settings.playback_quality;
        let pending_playback_restore = settings.current_playback.clone();
        let library_cache = LibraryCache::default();

        let playlist_list =
            cx.new(|cx| ListState::new(PlaylistListDelegate::new(), window, cx).searchable(false));
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("想播放什么？")
                .context_menu(false)
        });
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
        let scale_input = |value: u32, window: &mut Window, cx: &mut Context<Self>| {
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
        let (load_more_sender, mut load_more_receiver) = mpsc::channel(1);
        let (search_load_more_sender, mut search_load_more_receiver) = mpsc::channel(1);
        let (search_card_load_more_sender, mut search_card_load_more_receiver) = mpsc::channel(1);
        let (track_event_sender, mut track_event_receiver) = mpsc::unbounded_channel();
        let track_table = cx.new(|cx| {
            TableState::new(
                TrackTableDelegate::new(load_more_sender, track_event_sender.clone()),
                window,
                cx,
            )
            .col_selectable(false)
            .col_movable(false)
            .sortable(false)
        });
        let search_track_table = cx.new(|cx| {
            TableState::new(
                TrackTableDelegate::new_with_header_style(
                    search_load_more_sender,
                    track_event_sender.clone(),
                    px(32.),
                    px(14.),
                ),
                window,
                cx,
            )
            .col_selectable(false)
            .col_movable(false)
            .sortable(false)
        });
        let search_card_grid = cx.new(|cx| {
            ListState::new(
                SearchCardGridDelegate::new(
                    track_event_sender.clone(),
                    search_card_load_more_sender,
                ),
                window,
                cx,
            )
            .searchable(false)
            .selectable(false)
        });
        let progress_slider = cx.new(|_| progress_slider_state(0.));
        let volume_slider = cx.new(|_| volume_slider_state(settings.volume));
        let image_cache = CachedImageCache::new(settings.image_cache_capacity, cx);
        let subscriptions = vec![
            cx.subscribe_in(
                &playlist_list,
                window,
                |this, _, event: &ListEvent, window, cx| {
                    if let ListEvent::Select(index) | ListEvent::Confirm(index) = event {
                        this.select_playlist(index.row, window, cx);
                    }
                },
            ),
            cx.subscribe(&track_table, |this, _, event: &TableEvent, cx| {
                if let TableEvent::DoubleClickedRow(index) = event {
                    this.select_track(*index, cx);
                }
            }),
            cx.subscribe(&search_track_table, |this, _, event: &TableEvent, cx| {
                if let TableEvent::DoubleClickedRow(index) = event {
                    this.select_search_track(*index, cx);
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
            cx.subscribe_in(
                &audio_cache_limit_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.apply_audio_cache_limit(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &image_cache_capacity_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.apply_image_cache_capacity(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &navigation_history_limit_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.apply_navigation_history_limit(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &lyric_font_scale_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.apply_scale_settings(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &lyric_line_spacing_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.apply_scale_settings(window, cx);
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
            while load_more_receiver.recv().await.is_some() {
                if this
                    .update(cx, |this, cx| this.load_playlist_page(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            while search_load_more_receiver.recv().await.is_some() {
                if this
                    .update(cx, |this, cx| this.load_more_search(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            while search_card_load_more_receiver.recv().await.is_some() {
                if this
                    .update(cx, |this, cx| this.load_more_search(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = track_event_receiver.recv().await {
                if this
                    .update_in(cx, |this, window, cx| match event {
                        TrackTableEvent::Artist(artist) => {
                            this.open_search_artist(artist, window, cx)
                        }
                        TrackTableEvent::Album(album) => {
                            this.open_home_playlist(album.into_playlist(), window, cx)
                        }
                        TrackTableEvent::Unlike(track) => this.unlike_track(track, cx),
                        TrackTableEvent::Playlist(playlist) => {
                            this.open_home_playlist(playlist, window, cx)
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let color_theme = settings.active_color_theme(cx.window_appearance());
        let mut view = Self {
            account_state: AccountState::Restoring,
            credential: None,
            profile: None,
            qr_image: None,
            library_loading: false,
            selected_playlist_index: None,
            selected_playlist: None,
            selected_playlist_resource: None,
            pending_playlist_scroll_position: None,
            library_generation: 0,
            playlist_force_refresh: false,
            playlist_page_requests: SingleFlight::default(),
            main_content: MainContent::Home,
            navigation_history: NavigationHistory::new(settings.navigation_history_limit),
            home_playlists: Vec::new(),
            home_loading: false,
            home_loaded: false,
            home_error: None,
            home_generation: 0,
            home_recommendation_loading: None,
            search_query: String::new(),
            search_resource: None,
            search_visible_counts: SearchVisibleCounts::default(),
            search_category: SearchCategory::Songs,
            search_scroll_offsets: [px(0.); 4],
            pending_search_scroll: false,
            selected_artist: None,
            artist_resource: None,
            artist_visible_song_count: ARTIST_PAGE_SIZE as usize,
            artist_visible_album_count: ARTIST_ALBUM_PAGE_SIZE as usize,
            playlist_list,
            track_table,
            search_track_table,
            search_card_grid,
            search_input,
            ui_font_input,
            monospace_font_input,
            lyric_font_input,
            audio_cache_limit_input,
            image_cache_capacity_input,
            navigation_history_limit_input,
            lyric_font_scale_input,
            lyric_line_spacing_input,
            settings_scroll_handle: ScrollHandle::new(),
            progress_slider,
            volume_slider,
            image_cache,
            window_drag_pressed: false,
            audio,
            audio_cache,
            protocol_client,
            cdn_maintenance: None,
            audio_cache_maintenance: None,
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
            progress_hovered: false,
            progress_hover_fraction: None,
            cover_backdrop_expanded: false,
            cover_backdrop_visible: false,
            cover_backdrop_fully_expanded: false,
            backdrop_current_url: None,
            backdrop_previous_url: None,
            backdrop_crossfade_phase: false,
            lyric_disk_cache,
            lyrics_cache: HashMap::new(),
            pending_lyrics_cache: HashMap::new(),
            lyric_layout_cache: LyricLayoutCache::default(),
            lyric_motion_state: None,
            cached_lyrics_panel: None,
            lyric_panel_frame_pending: true,
            pending_lyric_reveal_mid: None,
            lyric_reveal_frame_pending: false,
            lyrics_loading: HashSet::new(),
            lyrics_errors: HashMap::new(),
            fonts,
            color_theme,
            settings,
            library_cache,
            page_resource_cache: PageResourceCache::default(),
            liked_tracks: HashMap::new(),
            liked_state_loading: HashSet::new(),
            liked_toggle_loading: HashSet::new(),
            update_check_in_progress: false,
            shuffle: false,
            repeat_mode: RepeatMode::Off,
            pending_playback_restore,
            last_playback_persisted_at: Instant::now(),
            login_message: "正在读取已保存的登录状态…".to_owned(),
            window_handle: window.window_handle(),
            login_generation: 0,
            play_generation: 0,
            account_menu_open: false,
            _subscriptions: subscriptions,
            _window_subscription: None,
            _appearance_subscription: None,
            window_tick_wake: None,
            background_tick_wake: None,
            lyric_animation_frame_pending: false,
            next_lyric_highlight_frame: None,
            next_lyric_scroll_frame: None,
            #[cfg(target_os = "linux")]
            mpris: None,
            #[cfg(target_os = "linux")]
            inhibit: None,
            #[cfg(target_os = "linux")]
            last_mpris_position_sync: Instant::now(),
        };
        view.attach_window(window, cx);
        if !initial_errors.is_empty() {
            view.notify_error(initial_errors.join("；"), cx);
        }
        view.start_audio_cache_maintenance();
        view.start_cdn_maintenance();
        view.restore_credential(cx);
        if view.settings.check_updates_on_startup {
            view.schedule_update_check(cx);
        }
        view
    }

    pub(crate) fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.set_window_title("Lyrune");
        self.window_handle = window.window_handle();
        Theme::global_mut(cx).notification.margins.bottom = px(PLAYER_BAR_HEIGHT + 16.);
        self.lyric_animation_frame_pending = false;
        self.lyric_panel_frame_pending = true;
        self.next_lyric_highlight_frame = None;
        self.next_lyric_scroll_frame = None;
        window.set_inactive_frame_interval(self.inactive_window_frame_interval());
        self._appearance_subscription =
            Some(cx.observe_window_appearance(window, |this, window, cx| {
                this.apply_theme(window, cx)
            }));
        self._window_subscription = Some(cx.observe_window_bounds(window, |this, window, cx| {
            let size = window.window_bounds().get_bounds().size;
            let width = f32::from(size.width).round() as u32;
            let height = f32::from(size.height).round() as u32;
            if width > 0 && height > 0 {
                this.settings.window_size = Some(PersistedWindowSize { width, height });
            }
            cx.notify();
        }));
        self.sync_progress_slider(window, cx);
        self.start_window_tick(window, cx);
    }

    fn notify_error(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.notify_status(
            Notification::error(message.into()),
            cx.theme().danger,
            cx.theme().danger_foreground,
            cx,
        );
    }

    fn schedule_update_check(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            if this
                .read_with(cx, |this, _| {
                    this.settings.check_updates_on_startup && !this.update_check_in_progress
                })
                .unwrap_or(false)
            {
                this.update(cx, |this, cx| this.check_updates(false, cx))?;
            }
            anyhow::Ok(())
        })
        .detach();
    }

    fn check_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.update_check_in_progress {
            return;
        }
        self.update_check_in_progress = true;
        let task = RUNTIME.spawn(crate::update::check());
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
            this.update(cx, |this, cx| {
                this.update_check_in_progress = false;
                match result {
                    Ok(Some(update)) => this.notify_update(update, cx),
                    Ok(None) if manual => this.notify_success("当前已是最新版本", cx),
                    Err(error) => this.notify_error(format!("检查更新失败：{error:#}"), cx),
                    _ => {}
                }
                cx.notify();
            })
        })
        .detach();
    }

    fn notify_update(&self, update: AvailableUpdate, cx: &mut Context<Self>) {
        self.notify_info(
            format!(
                "发现 Lyrune {}，请前往 GitHub Release 页面更新",
                update.version
            ),
            cx,
        );
    }

    fn notify_info(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.notify_status(
            Notification::info(message.into()),
            cx.theme().info,
            cx.theme().info_foreground,
            cx,
        );
    }

    fn notify_success(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.notify_status(
            Notification::success(message.into()),
            cx.theme().success,
            cx.theme().success_foreground,
            cx,
        );
    }

    fn notify_status(
        &self,
        notification: Notification,
        background: Hsla,
        foreground: Hsla,
        cx: &mut Context<Self>,
    ) {
        let notification = notification
            .bg(background)
            .text_color(foreground)
            .text_center()
            .border_0()
            .rounded(px(9.))
            .py_3();
        let window_handle = self.window_handle;
        cx.defer(move |cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                window.push_notification(notification, cx);
            });
        });
    }

    fn start_window_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (wake_sender, mut wake_receiver) = mpsc::channel(1);
        self.window_tick_wake = Some(wake_sender);
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let playback_advancing =
                    match this.read_with(cx, |this, _| this.playback_is_advancing()) {
                        Ok(playback_advancing) => playback_advancing,
                        Err(_) => break,
                    };
                if !playback_advancing {
                    if this
                        .update_in(cx, |this, window, cx| this.sync_progress_slider(window, cx))
                        .is_err()
                        || wake_receiver.recv().await.is_none()
                    {
                        break;
                    }
                    continue;
                }

                let timer = cx.background_executor().timer(PROGRESS_TICK);
                let wake = wake_receiver.recv();
                pin_mut!(timer, wake);
                match select(timer, wake).await {
                    Either::Left(_) => {
                        if this
                            .update_in(cx, |this, window, cx| this.sync_progress_slider(window, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Either::Right((Some(()), _)) => {}
                    Either::Right((None, _)) => break,
                }
            }
        })
        .detach();
    }

    pub(crate) fn start_background_tick(&mut self, cx: &mut Context<Self>) {
        let (wake_sender, mut wake_receiver) = mpsc::channel(1);
        self.background_tick_wake = Some(wake_sender);
        cx.spawn(async move |this, cx| {
            loop {
                let interval = match this.read_with(cx, |this, _| {
                    if !this.playback_is_advancing() {
                        None
                    } else {
                        Some(PROGRESS_TICK)
                    }
                }) {
                    Ok(interval) => interval,
                    Err(_) => break,
                };
                let Some(interval) = interval else {
                    if wake_receiver.recv().await.is_none() {
                        break;
                    }
                    continue;
                };

                let timer = cx.background_executor().timer(interval);
                let wake = wake_receiver.recv();
                pin_mut!(timer, wake);
                match select(timer, wake).await {
                    Either::Left(_) => {
                        if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                            break;
                        }
                    }
                    Either::Right((Some(()), _)) => {}
                    Either::Right((None, _)) => break,
                }
            }
        })
        .detach();
    }

    fn playback_is_advancing(&self) -> bool {
        self.playback_started
            && self.loading_track.is_none()
            && self.audio.as_ref().is_some_and(AudioPlayer::is_playing)
    }

    fn wake_playback_ticks(&self) {
        if let Some(wake) = &self.window_tick_wake {
            let _ = wake.try_send(());
        }
        if let Some(wake) = &self.background_tick_wake {
            let _ = wake.try_send(());
        }
    }

    pub(crate) fn window_size(&self) -> Option<PersistedWindowSize> {
        self.settings.window_size
    }

    pub(crate) fn window_decoration(&self) -> WindowDecoration {
        self.settings.window_decoration
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn attach_mpris(&mut self, mpris: MprisHandle) {
        self.mpris = Some(mpris);
        self.sync_mpris(false);
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn attach_inhibit(&mut self, inhibit: InhibitHandle) {
        self.inhibit = Some(inhibit);
        self.sync_inhibit();
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

    fn start_audio_cache_maintenance(&mut self) {
        if let Some(task) = self.audio_cache_maintenance.take() {
            task.abort();
        }
        let Some(cache) = self.audio_cache.clone() else {
            return;
        };
        self.audio_cache_maintenance = Some(RUNTIME.spawn(async move {
            let _ = cache.maintain().await;
        }));
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

    #[cfg(target_os = "linux")]
    fn mpris_snapshot(&self) -> MprisSnapshot {
        let audio_available = self.audio.is_some();
        let loading = self.loading_track.is_some();
        let playback_status = if loading {
            MprisPlaybackStatus::Playing
        } else if !self.playback_started {
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
        if let Some(mpris) = &self.mpris {
            let snapshot = self.mpris_snapshot();
            if seeked {
                mpris.seeked(snapshot);
            } else {
                mpris.update(snapshot);
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn sync_inhibit(&self) {
        if let Some(inhibit) = &self.inhibit {
            inhibit.set_active(self.playback_is_advancing());
        }
    }

    #[cfg(target_os = "linux")]
    fn sync_mpris_position(&self) {
        if let Some(mpris) = &self.mpris {
            mpris.update_position(duration_micros(self.position));
        }
    }

    fn sync_progress_slider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.seek_preview.is_none() {
            let progress = self
                .current_duration()
                .map_or(0., |duration| progress_fraction(self.position, duration));
            self.progress_slider.update(cx, |slider, cx| {
                if slider.value().end() != progress {
                    slider.set_value(progress, window, cx);
                }
            });
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let previous_position = self.position;
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
        if self.current_track.is_some()
            && self.loading_track.is_none()
            && self.last_mpris_position_sync.elapsed() >= PROGRESS_TICK
        {
            self.sync_mpris_position();
            self.last_mpris_position_sync = Instant::now();
        }

        if self.position != previous_position && self.progress_hovered {
            cx.notify();
        }
    }

    fn render_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });
        let logo = h_flex()
            .id("window-drag-logo")
            .h(px(64.))
            .mb_2()
            .px_5()
            .gap_3()
            .child(lyrune_icon(self.color_theme, px(42.)))
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
            )
            .when(client_decorations, |logo| self.window_drag_region(logo, cx));
        v_flex()
            .w_full()
            .h_full()
            .flex_shrink_0()
            .bg(theme.sidebar)
            .child(logo)
            .child(
                h_flex().h(px(60.)).px_5().justify_between().child(
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
            .into_any_element()
    }

    fn render_quality_selector(&mut self, has_track: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let active_quality = self.active_quality;
        let active_label = active_quality.badge_label();
        let options = self
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
                    .when(self.loading_track.is_some(), |button| {
                        button
                            .bg(theme.input_background())
                            .border_color(theme.input)
                            .text_color(theme.button_foreground)
                    })
                    .disabled(
                        !has_track
                            || self.loading_track.is_some()
                            || self.available_qualities.is_empty(),
                    )
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

    fn render_player_progress(
        &mut self,
        has_track: bool,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let monospace_font = self.fonts.monospace.clone();
        let duration = self.current_duration().unwrap_or_default();
        let percentage =
            if self.cover_backdrop_visible && self.seek_preview.is_none() && !duration.is_zero() {
                progress_fraction(self.position, duration)
            } else {
                self.progress_slider.read(cx).percentage().end
            };
        let interactive = has_track && self.playback_started && self.loading_track.is_none();
        let bar_color = theme.slider_bar;
        let thumb_color = theme.slider_thumb;
        let hover_fraction = interactive
            .then_some(self.progress_hover_fraction)
            .flatten();
        let hover_visible = self.progress_hovered && hover_fraction.is_some();
        let hover_opacity = transition(
            ("player-progress", "hover-opacity"),
            if hover_visible { 1. } else { 0. },
            Transition::new(Duration::from_millis(if hover_visible { 120 } else { 170 })),
            window,
            cx,
        );
        let edge_time_color = if self.cover_backdrop_expanded && hover_fraction.is_some() {
            self.foreground_target(
                theme.background.opacity(LYRIC_BACKGROUND_OVERLAY_OPACITY),
                theme.foreground,
                |cover| cover.sampled_rgb(narrow),
                window,
                cx,
            )
            .unwrap_or(theme.foreground)
        } else {
            theme.secondary_foreground
        };

        let track = SliderTrack::new(&self.progress_slider)
            .disabled(!interactive)
            .h(px(20.))
            .w_full()
            .child(
                SliderIndicator::new(&self.progress_slider)
                    .relative()
                    .h(px(4.))
                    .w_full()
                    .bg(theme.border)
                    .active(|this| this.bg(bar_color.opacity(0.35)))
                    .child(
                        // Painted as paths because quads and layout snap to device pixels,
                        // which makes slow progress advance in visible one-pixel steps.
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                let x = bounds.left() + bounds.size.width * percentage;
                                if x > bounds.left() {
                                    let mut fill = PathBuilder::fill();
                                    fill.add_polygon(
                                        &[
                                            bounds.origin,
                                            point(x, bounds.top()),
                                            point(x, bounds.bottom()),
                                            bounds.bottom_left(),
                                        ],
                                        true,
                                    );
                                    if let Ok(path) = fill.build() {
                                        window.paint_path(path, bar_color);
                                    }
                                }
                                if has_track {
                                    let center = point(x, bounds.center().y);
                                    for (radius, color) in [
                                        (PROGRESS_THUMB_RADIUS, bar_color.opacity(0.5)),
                                        (PROGRESS_THUMB_RADIUS - px(1.), thumb_color),
                                    ] {
                                        let mut circle = PathBuilder::fill();
                                        circle.move_to(point(center.x - radius, center.y));
                                        for to in [center.x + radius, center.x - radius] {
                                            circle.arc_to(
                                                point(radius, radius),
                                                px(0.),
                                                false,
                                                true,
                                                point(to, center.y),
                                            );
                                        }
                                        circle.close();
                                        if let Ok(path) = circle.build() {
                                            window.paint_path(path, color);
                                        }
                                    }
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    ),
            );
        let control = BaseSlider::new(&self.progress_slider)
            .disabled(!interactive)
            .h(px(20.))
            .w_full()
            .child(track);

        let current_time = format_playback_time(self.position.min(duration));
        let total_time = format_playback_time(duration);
        div()
            .id("player-progress")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(20.))
            .when(interactive, |layer| {
                layer
                    .cursor_pointer()
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        let bounds = this.progress_slider.read(cx).bounds();
                        if bounds.size.width <= px(0.) {
                            return;
                        }
                        let inner =
                            (event.position.x - bounds.left()).clamp(px(0.), bounds.size.width);
                        this.progress_hover_fraction = Some(inner / bounds.size.width);
                        this.progress_hovered = true;
                        cx.notify();
                    }))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !hovered && this.progress_hovered {
                            this.progress_hovered = false;
                            cx.notify();
                        }
                    }))
            })
            .child(control)
            .when_some(hover_fraction, |layer, fraction| {
                let tooltip = div()
                    .absolute()
                    .top(px(-36.))
                    .w(px(64.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_md()
                    .when(hover_visible, |tooltip| tooltip.occlude())
                    .font(monospace_font.clone())
                    .text_xs()
                    .font_medium()
                    .text_color(theme.primary)
                    .child(format_playback_time(duration.mul_f32(fraction)));
                let tooltip = if fraction <= 0.03 {
                    tooltip.left_0()
                } else if fraction >= 0.97 {
                    tooltip.right_0()
                } else {
                    tooltip.left(relative(fraction)).ml(-px(32.))
                };
                layer.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(20.))
                        .opacity(hover_opacity)
                        .child(
                            div()
                                .absolute()
                                .top(px(-27.))
                                .left_0()
                                .w(px(64.))
                                .text_center()
                                .font(monospace_font.clone())
                                .text_xs()
                                .text_color(edge_time_color)
                                .child(current_time),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(px(-27.))
                                .right_0()
                                .w(px(64.))
                                .text_center()
                                .font(monospace_font.clone())
                                .text_xs()
                                .text_color(edge_time_color)
                                .child(total_time),
                        )
                        .child(tooltip),
                )
            })
            .into_any_element()
    }

    fn render_player_bar(
        &mut self,
        compact: bool,
        narrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let track = self.current_track_data().cloned();
        let has_track = track.is_some();
        let quality_selector = self.render_quality_selector(has_track, cx);
        let progress = self.render_player_progress(has_track, narrow, window, cx);
        let theme = cx.theme();
        let is_playing = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
        let loading = self.loading_track.is_some();
        let show_pause = if loading {
            self.loading_autoplay
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
                                                #[cfg(target_os = "linux")]
                                                this.sync_mpris(false);
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
                                    .tooltip(if show_pause { "暂停" } else { "播放" })
                                    .when(loading, |button| button.bg(theme.button_primary))
                                    .disabled(
                                        loading
                                            || (self.current_track.is_none()
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
                                                #[cfg(target_os = "linux")]
                                                this.sync_mpris(false);
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

    fn render_window_controls(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let available_height = (window.viewport_size().height - px(PLAYER_BAR_HEIGHT)).max(px(0.));
        let expansion_progress = transition(
            ("cover-backdrop", "expansion-progress"),
            if self.cover_backdrop_expanded { 1. } else { 0. },
            Transition::new(LYRIC_EXPANSION_DURATION).ease(|progress| 1. - (1. - progress).powi(4)),
            window,
            cx,
        );
        let lyrics_controls = self.cover_backdrop_expanded;
        let reveal_start = (1. - 54. / f32::from(available_height).max(54.)).clamp(0., 1.);
        let control_color_progress =
            ((expansion_progress - reveal_start) / (1. - reveal_start)).clamp(0., 1.);
        let control_foreground = if control_color_progress > 0. {
            let control_foreground = self
                .foreground_target(
                    theme.background.opacity(LYRIC_BACKGROUND_OVERLAY_OPACITY),
                    theme.foreground,
                    |cover| cover.controls_rgb(),
                    window,
                    cx,
                )
                .unwrap_or(theme.foreground);
            interpolate_color(theme.foreground, control_foreground, control_color_progress)
        } else {
            theme.foreground
        };
        let control_hover_background = theme.background.opacity(0.58);
        let control_variant = ButtonCustomVariant::new(cx)
            .foreground(control_foreground)
            .hover(control_hover_background)
            .active(control_hover_background);
        let icon_foreground = if lyrics_controls {
            control_foreground
        } else {
            theme.foreground
        };
        let icon = |group: &'static str, icon: MediaIcon| {
            div()
                .relative()
                .size(px(16.))
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .group_hover(group, |style| style.opacity(0.))
                        .child(media_icon_hsla(icon, icon_foreground, px(16.))),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .opacity(0.)
                        .group_hover(group, |style| style.opacity(1.))
                        .child(media_icon_hsla(icon, theme.foreground, px(16.))),
                )
        };
        h_flex()
            .absolute()
            .top(px(18.))
            .right(px(16.))
            .children([
                div()
                    .group("window-minimize")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("window-minimize")
                            .when(lyrics_controls, |button| button.custom(control_variant))
                            .when(!lyrics_controls, |button| button.ghost())
                            .size(px(36.))
                            .p_0()
                            .child(icon("window-minimize", MediaIcon::WindowMinimize))
                            .on_click(|_, window, _| window.minimize_window()),
                    ),
                div()
                    .group("window-maximize")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("window-maximize")
                            .when(lyrics_controls, |button| button.custom(control_variant))
                            .when(!lyrics_controls, |button| button.ghost())
                            .size(px(36.))
                            .p_0()
                            .child(icon(
                                "window-maximize",
                                if window.is_maximized() {
                                    MediaIcon::WindowRestore
                                } else {
                                    MediaIcon::WindowMaximize
                                },
                            ))
                            .on_click(|_, window, _| window.zoom_window()),
                    ),
                div()
                    .group("window-close")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("window-close")
                            .when(lyrics_controls, |button| button.custom(control_variant))
                            .when(!lyrics_controls, |button| button.ghost())
                            .size(px(36.))
                            .p_0()
                            .child(icon("window-close", MediaIcon::WindowClose))
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            ])
            .into_any_element()
    }

    fn window_drag_region(&self, region: Stateful<Div>, cx: &Context<Self>) -> Stateful<Div> {
        region
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.window_drag_pressed = false))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.window_drag_pressed = true),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.window_drag_pressed = false),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.window_drag_pressed {
                    this.window_drag_pressed = false;
                    window.start_window_move();
                }
            }))
    }

    fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.cover_backdrop_visible && self.playback_is_advancing() {
            if self.seek_preview.is_none() {
                self.position = self
                    .audio
                    .as_ref()
                    .map(AudioPlayer::position)
                    .unwrap_or_default();
            }
            self.request_lyric_animation_frame(window, cx);
        }

        let theme = cx.theme().clone();
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });
        let compact = window.viewport_size().width < px(1120.);
        let narrow = window.viewport_size().width < px(900.);
        let scale_factor = window.scale_factor();
        let popover_open = self.account_menu_open || self.quality_menu_open;
        if self.cover_backdrop_fully_expanded {
            return v_flex()
                .relative()
                .size_full()
                .image_cache(self.image_cache.clone())
                .font(self.fonts.ui.clone())
                .bg(theme.background)
                .text_color(theme.foreground)
                .child(div().flex_1().min_h_0())
                .child(self.render_cover_backdrop(window, cx))
                .child(self.render_player_bar(compact, narrow, window, cx))
                .when(popover_open, |this| {
                    this.child(
                        deferred(
                            div()
                                .id("popover-dismiss-layer")
                                .absolute()
                                .inset_0()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.dismiss_popovers(cx);
                                    }),
                                ),
                        )
                        .with_priority(5),
                    )
                })
                .when(client_decorations, |this| {
                    this.child(deferred(self.render_window_controls(window, cx)).with_priority(20))
                })
                .into_any_element();
        }
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
        let sidebar = self.render_sidebar(window, cx);
        let page = match self.main_content {
            MainContent::Home => self.render_home(compact, narrow, scale_factor, cx),
            MainContent::Search => self.render_search(
                compact,
                narrow,
                scale_factor,
                window.viewport_size().width - sidebar_width,
                cx,
            ),
            MainContent::Artist => self.render_artist_content(compact, narrow, scale_factor, cx),
            MainContent::Playlist => {
                self.render_playlist_content(compact, narrow, scale_factor, cx)
            }
            MainContent::Settings => self.render_settings_page(narrow, cx),
        };
        let search_width = if narrow {
            px(268.)
        } else if compact {
            px(376.)
        } else {
            px(480.)
        };
        let home_selected = self.main_content == MainContent::Home;
        let can_navigate_back = !self.navigation_history.back.is_empty();
        let can_navigate_forward = !self.navigation_history.forward.is_empty();
        let history_navigation = h_flex()
            .gap_1()
            .child(
                Button::new("navigate-back")
                    .ghost()
                    .rounded(px(999.))
                    .size(px(44.))
                    .p_0()
                    .tooltip("返回")
                    .disabled(!can_navigate_back)
                    .child(media_icon_hsla(
                        MediaIcon::Back,
                        if can_navigate_back {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        },
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
                    .disabled(!can_navigate_forward)
                    .child(media_icon_hsla(
                        MediaIcon::Forward,
                        if can_navigate_forward {
                            theme.foreground
                        } else {
                            theme.muted_foreground
                        },
                        px(22.),
                    ))
                    .on_click(cx.listener(|this, _, window, cx| this.navigate_forward(window, cx))),
            );
        let navigation = h_flex()
            .gap_3()
            .when(client_decorations, |this| {
                this.flex_1().min_w_0().justify_center()
            })
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
                    .when(client_decorations, |this| {
                        this.flex_1().min_w_0().max_w(search_width)
                    })
                    .on_mouse_down_out(|_, window, _| window.blur())
                    .child(
                        Input::new(&self.search_input)
                            .large()
                            .w(search_width)
                            .when(client_decorations, |this| this.w_full())
                            .border_2()
                            .rounded(px(999.))
                            .text_size(theme.font_size)
                            .aria_label("搜索")
                            .prefix(media_icon_hsla(
                                MediaIcon::Search,
                                theme.muted_foreground,
                                px(22.),
                            )),
                    ),
            );
        let account = self.render_account(scale_factor, cx);
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
                    .map(|header| {
                        if client_decorations {
                            header.child(
                                h_flex()
                                    .size_full()
                                    .px_4()
                                    .gap_2()
                                    .child(history_navigation.flex_shrink_0())
                                    .child(navigation)
                                    .child(div().size(px(44.)).flex_shrink_0().child(account))
                                    .child(div().w(px(108.)).flex_shrink_0()),
                            )
                        } else {
                            header
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
                                .child(
                                    div()
                                        .absolute()
                                        .top(px(17.))
                                        .right(px(24.))
                                        .size(px(44.))
                                        .child(account),
                                )
                        }
                    }),
            )
            .child(page);
        v_flex()
            .relative()
            .size_full()
            .image_cache(self.image_cache.clone())
            .font(self.fonts.ui.clone())
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
            .child(self.render_cover_backdrop(window, cx))
            .child(self.render_player_bar(compact, narrow, window, cx))
            .when(popover_open, |this| {
                this.child(
                    deferred(
                        div()
                            .id("popover-dismiss-layer")
                            .absolute()
                            .inset_0()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.dismiss_popovers(cx);
                                }),
                            ),
                    )
                    .with_priority(5),
                )
            })
            .when(client_decorations, |this| {
                this.child(deferred(self.render_window_controls(window, cx)).with_priority(20))
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
        if let Some(task) = self.cdn_maintenance.take() {
            task.abort();
        }
        if let Some(task) = self.audio_cache_maintenance.take() {
            task.abort();
        }
    }
}

impl Render for LyruneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let notification_list = Root::read(window, cx).notification.clone();
        let notification_layer = div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(PLAYER_BAR_HEIGHT + 16.))
            .flex()
            .justify_center()
            .child(div().relative().left(px(-42.)).child(notification_list));
        if self.account_state == AccountState::SignedIn {
            div()
                .relative()
                .size_full()
                .child(self.render_main(window, cx))
                .child(notification_layer)
                .into_any_element()
        } else {
            let client_decorations =
                matches!(window.window_decorations(), Decorations::Client { .. });
            div()
                .relative()
                .size_full()
                .child(self.render_login(cx))
                .child(notification_layer)
                .when(client_decorations, |this| {
                    this.child(deferred(self.render_window_controls(window, cx)).with_priority(20))
                })
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::lyrics::parse_lyrics;
    use super::lyrics_view::{
        LYRIC_MINIMUM_CONTRAST, adjacent_lyric_timing, combined_lyric_frame_interval,
        contrast_ratio, lyric_edge_opacity, lyric_frame_is_due, lyric_horizontal_scroll_offset,
        lyric_position_for_frame_rate, readable_lyric_color,
    };
    use super::navigation::{
        NavigationPage, PlaylistResource, SearchResource, resolved_playlist_scroll_row,
    };
    use super::playlists::playlist_title_is_long;
    use super::queue::{
        canonical_queue_track_index, insert_external_track_after_current,
        insert_track_after_current,
    };
    use super::{
        NavigationHistory, PlaybackQueue, PlaylistScrollPosition, SearchCategory,
        SearchVisibleCounts, format_playback_time,
    };
    use crate::settings::{DEFAULT_NAVIGATION_HISTORY_LIMIT, LyricFrameRate};
    use gpui::{Pixels, Rgba, black, px, rgb};
    use qqmusic_api::integration::{Track, UserPlaylist, UserPlaylistId};
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    fn track(mid: &str) -> Track {
        Track {
            song_id: None,
            song_type: 0,
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
            artist_details: Vec::new(),
            album: String::new(),
            album_mid: String::new(),
            cover_url: None,
            duration_seconds: 180,
        }
    }

    fn mids(tracks: &[Arc<Track>]) -> Vec<&str> {
        tracks.iter().map(|track| track.mid.as_str()).collect()
    }

    #[test]
    fn lyric_foreground_preserves_a_readable_theme_color() {
        let preferred = rgb(0x182030).into();
        let result =
            readable_lyric_color([0.95, 0.95, 0.95], black().opacity(0.28), preferred).to_rgb();
        let preferred = preferred.to_rgb();

        assert!((result.r - preferred.r).abs() < 1e-6);
        assert!((result.g - preferred.g).abs() < 1e-6);
        assert!((result.b - preferred.b).abs() < 1e-6);
    }

    #[test]
    fn lyric_foreground_minimally_corrects_insufficient_theme_contrast() {
        let background = Rgba {
            r: 0.5,
            g: 0.5,
            b: 0.5,
            a: 1.,
        };
        let result = readable_lyric_color(
            [background.r, background.g, background.b],
            black().opacity(0.),
            rgb(0x777777).into(),
        )
        .to_rgb();

        assert!(contrast_ratio(result, background) >= LYRIC_MINIMUM_CONTRAST);
        assert!(result.r > 0. && result.g > 0. && result.b > 0.);
    }

    #[test]
    fn horizontal_lyric_scroll_starts_at_the_anchor_and_stops_at_the_end() {
        assert_eq!(
            lyric_horizontal_scroll_offset(px(500.), px(600.), px(500.)),
            px(0.)
        );
        assert_eq!(
            lyric_horizontal_scroll_offset(px(1000.), px(600.), px(250.)),
            px(0.)
        );
        assert_eq!(
            lyric_horizontal_scroll_offset(px(1000.), px(600.), px(300.)),
            px(48.)
        );
        assert_eq!(
            lyric_horizontal_scroll_offset(px(1000.), px(600.), px(1000.)),
            px(400.)
        );
    }

    #[test]
    fn lyric_rows_fade_before_reaching_the_viewport_edge() {
        assert_eq!(lyric_edge_opacity(px(100.), px(204.), px(0.), px(400.)), 1.);
        assert!((lyric_edge_opacity(px(272.), px(376.), px(0.), px(400.)) - 0.5).abs() < 1e-6);
        assert_eq!(lyric_edge_opacity(px(296.), px(400.), px(0.), px(400.)), 0.);
        assert_eq!(lyric_edge_opacity(px(-10.), px(94.), px(0.), px(400.)), 0.);
    }

    #[test]
    fn lyric_frame_intervals_preserve_the_fastest_animation_requirement() {
        assert_eq!(
            combined_lyric_frame_interval(LyricFrameRate::Fps30, LyricFrameRate::Fps60),
            LyricFrameRate::Fps60.frame_interval()
        );
        assert_eq!(
            combined_lyric_frame_interval(LyricFrameRate::Display, LyricFrameRate::Fps60),
            None
        );
        assert_eq!(
            combined_lyric_frame_interval(LyricFrameRate::Fps30, LyricFrameRate::Display),
            None
        );
    }

    #[test]
    fn lyric_position_is_quantized_only_for_limited_highlight_rates() {
        let position = Duration::from_millis(50);
        assert_eq!(
            lyric_position_for_frame_rate(position, LyricFrameRate::Fps30),
            Duration::from_nanos(1_000_000_000 / 30)
        );
        assert_eq!(
            lyric_position_for_frame_rate(position, LyricFrameRate::Display),
            position
        );
    }

    #[test]
    fn limited_lyric_frames_follow_one_stable_deadline() {
        let now = Instant::now();
        let interval = LyricFrameRate::Fps30.frame_interval().unwrap();
        let mut next_frame = None;

        assert!(!lyric_frame_is_due(
            now,
            LyricFrameRate::Fps30,
            &mut next_frame
        ));
        assert_eq!(next_frame, Some(now + interval));
        assert!(!lyric_frame_is_due(
            now + Duration::from_millis(10),
            LyricFrameRate::Fps30,
            &mut next_frame
        ));
        assert!(lyric_frame_is_due(
            now + interval,
            LyricFrameRate::Fps30,
            &mut next_frame
        ));
        assert!(lyric_frame_is_due(
            now + interval,
            LyricFrameRate::Display,
            &mut next_frame
        ));
        assert_eq!(next_frame, None);
    }

    #[test]
    fn untimed_qrc_tail_extends_to_the_next_line() {
        let timing = (Duration::from_millis(100), Duration::from_millis(600));

        assert_eq!(
            adjacent_lyric_timing(
                Some(timing),
                None,
                Duration::ZERO,
                Duration::from_millis(900)
            ),
            (Duration::from_millis(600), Duration::from_millis(900))
        );
    }

    #[test]
    fn untimed_qrc_text_between_words_uses_the_timing_gap() {
        let previous = (Duration::from_millis(100), Duration::from_millis(600));
        let next = (Duration::from_millis(900), Duration::from_millis(1_200));

        assert_eq!(
            adjacent_lyric_timing(
                Some(previous),
                Some(next),
                Duration::ZERO,
                Duration::from_millis(1_500)
            ),
            (Duration::from_millis(600), Duration::from_millis(900))
        );
    }

    #[test]
    fn prefers_qrc_word_timing_and_preserves_lyric_parentheses() {
        let lyrics = parse_lyrics(
            r#"<?xml version="1.0" encoding="utf-8"?>
<QrcInfos><LyricInfo LyricCount="1"><Lyric_1 LyricType="1" LyricContent="[1000,700]青(1000,300)い(1300,150)空（そら）(1450,250)&#10;[2000,500]次(2000,500)"/></LyricInfo></QrcInfos>"#,
            Some("[00:01.000]蓝色天空\n[00:02.000]下一句"),
            None,
        );

        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].start, Duration::from_secs(1));
        assert_eq!(lyrics.lines[0].text, "青い空（そら）");
        assert_eq!(lyrics.lines[0].words.len(), 3);
        assert_eq!(
            lyrics.lines[0].words[0].highlight_progress(Duration::from_millis(999)),
            0.
        );
        assert!(
            (lyrics.lines[0].words[0].highlight_progress(Duration::from_millis(1_100)) - 1. / 3.)
                .abs()
                < f32::EPSILON
        );
        assert_eq!(
            lyrics.lines[0].words[2].highlight_progress(Duration::from_millis(1_700)),
            1.
        );
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("蓝色天空"));
        assert_eq!(lyrics.active_index(Duration::from_millis(1_999)), Some(0));
        assert_eq!(lyrics.active_index(Duration::from_secs(2)), Some(1));
    }

    fn playlist(diss_id: u64) -> NavigationPage {
        playlist_at_scroll_row(diss_id, 0)
    }

    fn playlist_at_scroll_row(diss_id: u64, scroll_row: usize) -> NavigationPage {
        playlist_at_scroll_position(diss_id, scroll_row, px(0.))
    }

    fn playlist_at_scroll_position(
        diss_id: u64,
        scroll_row: usize,
        offset_y: Pixels,
    ) -> NavigationPage {
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
            scroll_position: PlaylistScrollPosition {
                row: scroll_row,
                offset_y,
            },
            resource: None,
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
    fn new_navigation_can_claim_a_forward_resource_before_clearing_it() {
        let NavigationPage::Playlist { playlist, .. } = playlist(1) else {
            unreachable!();
        };
        let resource = Arc::new(std::sync::Mutex::new(PlaylistResource::empty(
            playlist.clone(),
        )));
        let weak = Arc::downgrade(&resource);
        let mut history = NavigationHistory {
            limit: DEFAULT_NAVIGATION_HISTORY_LIMIT,
            back: Vec::new(),
            forward: vec![NavigationPage::Playlist {
                playlist: playlist.clone(),
                selected_index: None,
                scroll_position: PlaylistScrollPosition::top(),
                resource: Some(resource),
            }],
        };

        let claimed = weak.upgrade().expect("forward resource remains available");
        let target = NavigationPage::Playlist {
            playlist,
            selected_index: None,
            scroll_position: PlaylistScrollPosition::top(),
            resource: Some(claimed),
        };
        history.record(Some(NavigationPage::Home), &target);

        assert!(history.forward.is_empty());
        assert!(weak.upgrade().is_some());
    }

    #[test]
    fn navigation_history_keeps_at_most_ten_pages_including_the_current_page() {
        let mut history = NavigationHistory::default();
        let mut current = NavigationPage::Home;
        for diss_id in 1..=DEFAULT_NAVIGATION_HISTORY_LIMIT as u64 + 2 {
            let target = playlist(diss_id);
            history.record(Some(current), &target);
            current = target;
        }

        assert_eq!(history.back.len() + 1, DEFAULT_NAVIGATION_HISTORY_LIMIT);
        assert!(history.back[0].same_destination(&playlist(3)));
    }

    #[test]
    fn shrinking_navigation_history_limit_evicts_the_oldest_pages_immediately() {
        let mut history = NavigationHistory::new(4);
        let mut current = NavigationPage::Home;
        for diss_id in 1..=3 {
            let target = playlist(diss_id);
            history.record(Some(current), &target);
            current = target;
        }

        history.set_limit(2);

        assert_eq!(history.back.len() + history.forward.len() + 1, 2);
        assert!(history.back[0].same_destination(&playlist(2)));
    }

    #[test]
    fn evicting_history_releases_its_shared_page_resource() {
        let resource = Arc::new(std::sync::Mutex::new(SearchResource::default()));
        let weak = Arc::downgrade(&resource);
        let mut current = NavigationPage::Search {
            query: "resource lifetime".to_owned(),
            category: SearchCategory::Songs,
            visible_counts: SearchVisibleCounts::default(),
            scroll_offsets: [px(0.); 4],
            resource: Some(resource.clone()),
        };
        drop(resource);

        let mut history = NavigationHistory::default();
        for diss_id in 1..=DEFAULT_NAVIGATION_HISTORY_LIMIT as u64 {
            let target = playlist(diss_id);
            history.record(Some(current), &target);
            current = target;
        }

        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn playlist_history_keeps_its_exact_scroll_position() {
        let playlist = playlist_at_scroll_position(1, 37, px(-18.));
        let mut history = NavigationHistory::default();

        history.record(Some(playlist), &NavigationPage::Home);
        let restored = history
            .go_back(Some(NavigationPage::Home))
            .expect("playlist history entry");

        let NavigationPage::Playlist {
            scroll_position, ..
        } = restored
        else {
            panic!("restored playlist history entry");
        };
        assert_eq!(scroll_position.row, 37);
        assert_eq!(scroll_position.offset_y, px(-18.));
    }

    #[test]
    fn playlist_scroll_waits_until_the_requested_row_is_loaded() {
        assert_eq!(resolved_playlist_scroll_row(120, 100, true), None);
        assert_eq!(resolved_playlist_scroll_row(120, 200, true), Some(120));
        assert_eq!(resolved_playlist_scroll_row(250, 200, false), Some(199));
    }

    #[test]
    fn shared_playlist_ignores_a_late_duplicate_page() {
        let NavigationPage::Playlist { playlist, .. } = playlist(1) else {
            unreachable!();
        };
        let mut resource = PlaylistResource::empty(playlist.clone());
        resource.apply_page(
            playlist.clone(),
            vec![Arc::new(track("first"))],
            true,
            20,
            0,
        );
        resource.apply_page(
            playlist.clone(),
            vec![Arc::new(track("second"))],
            true,
            40,
            20,
        );
        resource.apply_page(playlist, vec![Arc::new(track("duplicate"))], false, 40, 20);

        assert_eq!(mids(&resource.tracks), ["first", "second"]);
        assert!(resource.has_more);
        assert_eq!(resource.next_offset, 40);
    }

    #[test]
    fn playback_time_uses_compact_player_formatting() {
        assert_eq!(format_playback_time(Duration::from_secs(137)), "2:17");
        assert_eq!(format_playback_time(Duration::from_secs(3_661)), "1:01:01");
    }

    #[test]
    fn playlist_title_length_accounts_for_wide_characters() {
        assert!(!playlist_title_is_long("123456789012345678901234"));
        assert!(playlist_title_is_long("1234567890123456789012345"));
        assert!(!playlist_title_is_long("一二三四五六七八九十一二"));
        assert!(playlist_title_is_long("一二三四五六七八九十一二三"));
    }

    #[test]
    fn search_history_keeps_the_active_category_without_splitting_one_query() {
        let visible_counts = SearchVisibleCounts {
            albums: 60,
            ..Default::default()
        };
        let songs = NavigationPage::Search {
            query: "周杰伦".to_owned(),
            category: SearchCategory::Songs,
            visible_counts: SearchVisibleCounts::default(),
            scroll_offsets: [px(0.); 4],
            resource: None,
        };
        let albums = NavigationPage::Search {
            query: "周杰伦".to_owned(),
            category: SearchCategory::Albums,
            visible_counts,
            scroll_offsets: [px(-120.), px(-360.), px(-720.), px(-180.)],
            resource: None,
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
                visible_counts,
                scroll_offsets,
                ..
            } if query == "周杰伦"
                && visible_counts.albums == 60
                && scroll_offsets == [px(-120.), px(-360.), px(-720.), px(-180.)]
        ));
    }

    #[test]
    fn search_history_keeps_scroll_offsets_between_queries() {
        let previous = NavigationPage::Search {
            query: "milet".to_owned(),
            category: SearchCategory::Playlists,
            visible_counts: SearchVisibleCounts::default(),
            scroll_offsets: [px(-128.), px(-480.), px(-256.), px(-64.)],
            resource: None,
        };
        let next = NavigationPage::Search {
            query: "许嵩".to_owned(),
            category: SearchCategory::Songs,
            visible_counts: SearchVisibleCounts::default(),
            scroll_offsets: [px(0.); 4],
            resource: None,
        };
        let mut history = NavigationHistory::default();
        history.record(Some(previous), &next);
        let restored = history.go_back(Some(next)).expect("previous search");
        assert!(matches!(
            &restored,
            NavigationPage::Search { category: SearchCategory::Playlists, scroll_offsets, .. }
                if *scroll_offsets == [px(-128.), px(-480.), px(-256.), px(-64.)]
        ));
        let restored = history.go_forward(Some(restored)).expect("new search");
        assert!(matches!(
            restored,
            NavigationPage::Search { query, scroll_offsets, .. }
                if query == "许嵩" && scroll_offsets == [px(0.); 4]
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
        let mut tracks = vec![Arc::new(track("A")), Arc::new(track("B"))];

        let inserted = insert_track_after_current(&mut tracks, Some(0), Arc::new(track("C")));

        assert_eq!(inserted, 1);
        assert_eq!(mids(&tracks), ["A", "C", "B"]);
    }

    #[test]
    fn existing_search_track_is_moved_without_duplication() {
        let mut tracks = vec![
            Arc::new(track("C")),
            Arc::new(track("A")),
            Arc::new(track("B")),
        ];

        let inserted = insert_track_after_current(&mut tracks, Some(1), Arc::new(track("C")));

        assert_eq!(inserted, 1);
        assert_eq!(mids(&tracks), ["A", "C", "B"]);
    }

    #[test]
    fn search_insertion_prevents_reusing_the_source_playlist_queue() {
        let playlist_id = UserPlaylistId::Liked;
        let mut queue = PlaybackQueue {
            playlist_id: playlist_id.clone(),
            tracks: vec![Arc::new(track("A")), Arc::new(track("B"))],
            modified: false,
            continuation: None,
        };

        let inserted =
            insert_external_track_after_current(&mut queue, Some(0), Arc::new(track("search")));

        assert_eq!(inserted, 1);
        assert_eq!(mids(&queue.tracks), ["A", "search", "B"]);
        assert!(queue.modified);
        assert_eq!(canonical_queue_track_index(&queue, &playlist_id, "B"), None);
    }
}
