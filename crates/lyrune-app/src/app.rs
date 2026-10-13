mod account;
mod lyrics;
mod lyrics_view;
mod navigation;
mod playback;
mod settings;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use account::AccountState;
use anyhow::Context as _;
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
    scroll::ScrollableElement as _,
    slider::{Slider, SliderEvent, SliderState},
    spinner::Spinner,
    table::{DataTable, TableEvent, TableState},
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
    PlaylistListDelegate, SearchCard, SearchCardGridDelegate, TrackTableDelegate, TrackTableEvent,
    format_duration, playlist_cover,
};
use crate::lyrics_cache::LyricDiskCache;
#[cfg(target_os = "linux")]
use crate::mpris::{
    MprisCommand, MprisHandle, MprisLoopStatus, MprisPlaybackStatus, MprisSnapshot, MprisTrack,
};
use crate::player::AudioPlayer;
use crate::runtime::RUNTIME;
use crate::settings::{
    AppSettings, CdnCacheStore, LibraryCache, MIN_SCALE_PERCENT, PersistedLibraryView,
    PersistedPlayback, PersistedQueueContinuation, PersistedWindowSize, SettingsStore,
    WindowDecoration,
};
use crate::singleflight::SingleFlight;
use crate::update::AvailableUpdate;
use lyrics::{ParsedLyrics, parse_lyrics};
use lyrics_view::{
    CachedLyricsPanel, LYRIC_BACKGROUND_OVERLAY_OPACITY, LYRIC_EXPANSION_DURATION,
    LyricLayoutCache, LyricMotionState, interpolate_color,
};
use navigation::{
    MainContent, NavigationHistory, NavigationPage, PageResourceCache, PlaylistCachePolicy,
    PlaylistScrollPosition, SearchCategory, SearchMoreResults, SearchVisibleCounts,
    SharedArtistResource, SharedPlaylistResource, SharedSearchResource, append_shared_search_page,
    lock_resource, resolved_playlist_scroll_row, share_items, update_liked_playlist_resource,
};
use playback::PlaybackLocation;
use qqmusic_api::integration::{
    CredentialSession, PlaylistPage, ProtocolClient, Quality, RecommendationKind, SearchAlbum,
    SearchArtist, SearchPage, Track, UserPlaylist, UserPlaylistId, UserProfile,
};
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
const LYRIC_CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const PLAYER_BAR_HEIGHT: f32 = 112.;

fn page_horizontal_padding(narrow: bool) -> Pixels {
    if narrow { px(20.) } else { px(24.) }
}

#[derive(Clone)]
struct MemoryLyrics {
    parsed: Arc<ParsedLyrics>,
    fetched_at_secs: u64,
}

impl MemoryLyrics {
    fn is_fresh(&self, now_secs: u64) -> bool {
        now_secs.saturating_sub(self.fetched_at_secs) < LYRIC_CACHE_TTL.as_secs()
    }
}

enum LyricLoadEvent {
    Disk {
        lyrics: MemoryLyrics,
        fresh: bool,
    },
    Network {
        result: anyhow::Result<MemoryLyrics>,
        had_cached: bool,
    },
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

fn playlist_title_is_long(title: &str) -> bool {
    title
        .chars()
        .map(|character| if character.is_ascii() { 1 } else { 2 })
        .sum::<usize>()
        > 24
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

fn insert_track_after_current(
    tracks: &mut Vec<Arc<Track>>,
    current_index: Option<usize>,
    track: Arc<Track>,
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

fn insert_external_track_after_current(
    queue: &mut PlaybackQueue,
    current_index: Option<usize>,
    track: Arc<Track>,
) -> usize {
    let index = insert_track_after_current(&mut queue.tracks, current_index, track);
    queue.modified = true;
    index
}

fn canonical_queue_track_index(
    queue: &PlaybackQueue,
    playlist_id: &UserPlaylistId,
    track_mid: &str,
) -> Option<usize> {
    (!queue.modified && queue.playlist_id == *playlist_id)
        .then(|| queue.tracks.iter().position(|track| track.mid == track_mid))
        .flatten()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RepeatMode {
    Off,
    All,
    One,
}

struct PlaybackQueue {
    playlist_id: UserPlaylistId,
    tracks: Vec<Arc<Track>>,
    modified: bool,
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
    credential: CredentialSession,
    playlist: UserPlaylist,
    offset: u64,
    force: bool,
) -> anyhow::Result<PlaylistPage> {
    let account_id = credential
        .snapshot()
        .context("QQ 音乐登录凭据已注销")?
        .music_id;
    let key = PlaylistPageKey {
        account_id,
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

    fn start_search(
        &mut self,
        resource: SharedSearchResource,
        query: String,
        cx: &mut Context<Self>,
    ) {
        let Some(credential) = self.credential.clone() else {
            lock_resource(&resource).error = Some("请先登录 QQ 音乐".to_owned());
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            lock_resource(&resource).error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        {
            let mut state = lock_resource(&resource);
            if state.results.is_some() || state.loading {
                cx.notify();
                return;
            }
            state.loading = true;
            state.error = None;
        }
        cx.notify();

        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(30),
                client.search(&credential, &query, SEARCH_PAGE_SIZE as u64),
            )
            .await
            .context("QQ 音乐搜索等待超过 30 秒")
            .and_then(|result| result)
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let mut state = lock_resource(&resource);
                state.loading = false;
                match result {
                    Ok(results) => {
                        state.results = Some(results.into());
                        state.error = None;
                    }
                    Err(error) => {
                        state.results = None;
                        state.error = Some(format!("搜索失败：{error:#}"));
                    }
                }
                drop(state);
                if this
                    .search_resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource))
                {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn load_more_search(&mut self, cx: &mut Context<Self>) {
        let Some(resource) = self.search_resource.clone() else {
            return;
        };
        let category = self.search_category;
        let target_count = self.search_visible_counts.get(category) + SEARCH_PAGE_SIZE;
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            lock_resource(&resource).error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        let offset = {
            let mut state = lock_resource(&resource);
            if state.loading || state.loading_more[category.index()] {
                return;
            }
            let Some(results) = state.results.as_ref() else {
                return;
            };
            let (len, offset, has_more) = match category {
                SearchCategory::Songs => (
                    results.songs.items.len(),
                    results.songs.next_offset,
                    results.songs.has_more,
                ),
                SearchCategory::Artists => (
                    results.artists.items.len(),
                    results.artists.next_offset,
                    results.artists.has_more,
                ),
                SearchCategory::Albums => (
                    results.albums.items.len(),
                    results.albums.next_offset,
                    results.albums.has_more,
                ),
                SearchCategory::Playlists => (
                    results.playlists.items.len(),
                    results.playlists.next_offset,
                    results.playlists.has_more,
                ),
            };
            if len >= target_count || !has_more {
                *self.search_visible_counts.get_mut(category) = target_count;
                cx.notify();
                return;
            }
            state.loading_more[category.index()] = true;
            state.error = None;
            offset
        };
        *self.search_visible_counts.get_mut(category) = target_count;
        let query = self.search_query.clone();
        cx.notify();

        let task = RUNTIME.spawn(async move {
            match category {
                SearchCategory::Songs => client
                    .search_songs(&credential, &query, offset, SEARCH_PAGE_SIZE as u64)
                    .await
                    .map(SearchMoreResults::Songs),
                SearchCategory::Artists => client
                    .search_artists(&credential, &query, offset, SEARCH_PAGE_SIZE as u64)
                    .await
                    .map(SearchMoreResults::Artists),
                SearchCategory::Albums => client
                    .search_albums(&credential, &query, offset, SEARCH_PAGE_SIZE as u64)
                    .await
                    .map(SearchMoreResults::Albums),
                SearchCategory::Playlists => client
                    .search_playlists(&credential, &query, offset, SEARCH_PAGE_SIZE as u64)
                    .await
                    .map(SearchMoreResults::Playlists),
            }
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let mut state = lock_resource(&resource);
                state.loading_more[category.index()] = false;
                match (state.results.as_mut(), result) {
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
                        state.error = Some(format!("继续加载搜索结果失败：{error:#}"));
                    }
                    _ => {}
                }
                drop(state);
                if this
                    .search_resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource))
                {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn select_search_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(resource) = self.search_resource.as_ref() else {
            return;
        };
        let Some(track) = lock_resource(resource)
            .results
            .as_ref()
            .and_then(|results| results.songs.items.get(index))
            .cloned()
        else {
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
            insert_external_track_after_current(queue, current_index, track)
        } else {
            self.playback_queue = Some(PlaybackQueue {
                playlist_id: UserPlaylistId::Search {
                    query: self.search_query.clone(),
                },
                tracks: vec![track],
                modified: false,
                continuation: None,
            });
            0
        };
        self.start_playback(queue_index, Duration::ZERO, None, true, cx);
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

        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(30),
                client.recommended_playlists(&credential, 0, 20),
            )
            .await
            .context("QQ 音乐主页请求等待超过 30 秒")
            .and_then(|result| result)
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
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
            self.notify_error("请先登录 QQ 音乐", cx);
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.notify_error("QQ 音乐客户端不可用", cx);
            cx.notify();
            return;
        };
        self.home_recommendation_loading = Some(kind);
        cx.notify();

        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(Duration::from_secs(30), async {
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
            .and_then(|result| result)
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
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
                            tracks: share_items(tracks),
                            modified: false,
                            continuation: Some(continuation),
                        });
                        this.start_playback(0, Duration::ZERO, None, true, cx);
                    }
                    Ok(_) => {
                        this.notify_error("QQ 音乐没有返回可播放的推荐歌曲", cx);
                    }
                    Err(error) => {
                        this.notify_error(format!("加载个性化推荐失败：{error:#}"), cx);
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

        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(Duration::from_secs(30), async {
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
            .and_then(|result| result)
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
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
                                    queue.tracks.push(Arc::new(track));
                                }
                            }
                        }
                        this.persist_current_playback();
                        if this.queue_waiting_for_recommendation {
                            this.queue_waiting_for_recommendation = false;
                            if let Some(index) = first_added {
                                this.start_playback(index, Duration::ZERO, None, true, cx);
                            }
                        }
                    }
                    Err(error) => {
                        this.queue_waiting_for_recommendation = false;
                        this.notify_error(format!("继续加载推荐歌曲失败：{error:#}"), cx);
                    }
                }
                #[cfg(target_os = "linux")]
                this.sync_mpris(false);
                cx.notify();
            });
        })
        .detach();
    }

    fn load_library(&mut self, force_refresh: bool, cx: &mut Context<Self>) {
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(account_id) = credential.snapshot().map(|credential| credential.music_id) else {
            return;
        };
        if !force_refresh
            && let Some((profile, playlists)) = self.library_cache.fresh_directory(
                account_id,
                unix_timestamp_secs(),
                LIBRARY_CACHE_TTL,
            )
        {
            self.library_loading = false;
            self.apply_library(account_id, profile, playlists, false, cx);
            self.validate_cached_credential(cx);
            return;
        }
        let Some(client) = self.protocol_client.clone() else {
            self.notify_error("QQ 音乐客户端不可用", cx);
            cx.notify();
            return;
        };
        self.library_generation = self.library_generation.wrapping_add(1);
        let generation = self.library_generation;
        self.library_loading = true;
        cx.notify();
        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(Duration::from_secs(30), async {
                tokio::try_join!(
                    client.user_profile(&credential),
                    client.user_playlists(&credential)
                )
            })
            .await
            .context("QQ 音乐用户资料和歌单请求等待超过 30 秒")?
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
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
                        this.apply_library(account_id, profile, playlists, force_refresh, cx);
                    }
                    Err(error) => {
                        this.notify_error(format!("加载 QQ 音乐资料失败：{error:#}"), cx);
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
        if count > 0 && self.main_content == MainContent::Playlist {
            self.select_playlist_with_refresh(
                viewed_index.unwrap_or(0),
                force_refresh,
                false,
                None,
                cx,
            );
        } else if count == 0 {
            cx.notify();
        }
        if let Some(restore) = playback_restore {
            self.restore_playback_queue(restore, cx);
        }
    }

    fn select_playlist(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.select_playlist_with_refresh(index, false, true, Some(window), cx);
    }

    fn select_playlist_with_refresh(
        &mut self,
        index: usize,
        force_refresh: bool,
        record_navigation: bool,
        window: Option<&mut Window>,
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

        if let Some(account_id) = self.account_id() {
            let view = PersistedLibraryView {
                account_id,
                playlist_id: playlist.id.clone(),
            };
            if self.settings.last_library_view.as_ref() != Some(&view) {
                self.settings.last_library_view = Some(view);
                self.persist_settings();
            }
        }

        let resource = self.shared_playlist_resource(
            playlist.clone(),
            force_refresh,
            PlaylistCachePolicy::Fresh,
        );
        if record_navigation {
            let target = NavigationPage::Playlist {
                playlist: playlist.clone(),
                selected_index: Some(index),
                scroll_position: PlaylistScrollPosition::top(),
                resource: Some(resource.clone()),
            };
            let current = self.current_navigation_page(cx);
            if let Some(window) = window {
                self.record_navigation(current, &target, window, cx);
            } else {
                self.navigation_history.record(current, &target);
            }
        }
        self.open_playlist(
            playlist,
            Some(index),
            force_refresh,
            PlaylistCachePolicy::Fresh,
            PlaylistScrollPosition::top(),
            Some(resource),
            cx,
        );
    }

    fn open_search_artist(
        &mut self,
        artist: SearchArtist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let resource = self.shared_artist_resource(&artist.mid);
        let target = NavigationPage::Artist {
            artist,
            visible_song_count: ARTIST_PAGE_SIZE as usize,
            visible_album_count: ARTIST_ALBUM_PAGE_SIZE as usize,
            resource: Some(resource),
        };
        let current = self.current_navigation_page(cx);
        self.record_navigation(current, &target, window, cx);
        self.apply_navigation_page(target, window, cx);
    }

    fn open_artist(
        &mut self,
        artist: SearchArtist,
        resource: SharedArtistResource,
        cx: &mut Context<Self>,
    ) {
        self.selected_artist = Some(artist);
        self.artist_resource = Some(resource.clone());
        self.main_content = MainContent::Artist;
        let (load_songs, load_albums) = {
            let state = lock_resource(&resource);
            (
                state.songs.is_none() && !state.songs_loading,
                state.albums.is_none() && !state.albums_loading,
            )
        };
        if load_songs {
            self.load_artist_songs(false, cx);
        }
        if load_albums {
            self.load_artist_albums(false, cx);
        }
        cx.notify();
    }

    fn load_artist_songs(&mut self, append: bool, cx: &mut Context<Self>) {
        let Some(resource) = self.artist_resource.clone() else {
            return;
        };
        let Some(artist) = self.selected_artist.clone() else {
            return;
        };
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            lock_resource(&resource).song_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        if append {
            self.artist_visible_song_count = self
                .artist_visible_song_count
                .saturating_add(ARTIST_PAGE_SIZE as usize);
        }
        let target_count = self.artist_visible_song_count;
        let offset = {
            let mut state = lock_resource(&resource);
            if state.songs_loading || state.songs_loading_more {
                return;
            }
            if let Some(page) = &state.songs {
                if page.items.len() >= target_count || !page.has_more {
                    cx.notify();
                    return;
                }
            } else if append {
                return;
            }
            let offset = state.songs.as_ref().map_or(0, |page| page.next_offset);
            state.song_error = None;
            if append {
                state.songs_loading_more = true;
            } else {
                state.songs_loading = true;
            }
            offset
        };
        let playlist = artist.into_playlist();
        cx.notify();

        let task = RUNTIME.spawn(async move {
            client
                .playlist_page(&credential, &playlist, offset, ARTIST_PAGE_SIZE)
                .await
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let mut state = lock_resource(&resource);
                state.songs_loading = false;
                state.songs_loading_more = false;
                match result {
                    Ok(page) => {
                        state.track_count = page.total;
                        let page = SearchPage {
                            items: page.tracks,
                            has_more: page.has_more,
                            next_offset: page.next_offset,
                        };
                        if offset == 0 {
                            state.songs = Some(page.into());
                        } else if let Some(songs) = &mut state.songs {
                            append_shared_search_page(songs, page);
                        }
                    }
                    Err(error) => {
                        state.song_error = Some(format!("加载歌手歌曲失败：{error:#}"));
                    }
                }
                drop(state);
                if this
                    .artist_resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource))
                {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn load_artist_albums(&mut self, append: bool, cx: &mut Context<Self>) {
        let Some(resource) = self.artist_resource.clone() else {
            return;
        };
        let Some(artist) = self.selected_artist.clone() else {
            return;
        };
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            lock_resource(&resource).album_error = Some("QQ 音乐客户端不可用".to_owned());
            cx.notify();
            return;
        };
        if append {
            self.artist_visible_album_count = self
                .artist_visible_album_count
                .saturating_add(ARTIST_ALBUM_PAGE_SIZE as usize);
        }
        let target_count = self.artist_visible_album_count;
        let offset = {
            let mut state = lock_resource(&resource);
            if state.albums_loading || state.albums_loading_more {
                return;
            }
            if let Some(page) = &state.albums {
                if page.items.len() >= target_count || !page.has_more {
                    cx.notify();
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
                let mut state = lock_resource(&resource);
                state.albums_loading = false;
                state.albums_loading_more = false;
                match result {
                    Ok(page) if offset == 0 => state.albums = Some(page.into()),
                    Ok(page) => {
                        if let Some(albums) = &mut state.albums {
                            append_shared_search_page(albums, page);
                        }
                    }
                    Err(error) => {
                        state.album_error = Some(format!("加载歌手专辑失败：{error:#}"));
                    }
                }
                drop(state);
                if this
                    .artist_resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource))
                {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn select_artist_track(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(artist) = self.selected_artist.clone() else {
            return;
        };
        let Some(resource) = self.artist_resource.as_ref() else {
            return;
        };
        let (selected_track, tracks, has_more, track_count) = {
            let state = lock_resource(resource);
            let Some(songs) = state.songs.as_ref() else {
                return;
            };
            let Some(selected_track) = songs.items.get(index).cloned() else {
                return;
            };
            (
                selected_track,
                songs.items.clone(),
                songs.has_more,
                state.track_count,
            )
        };
        let mut playlist = artist.into_playlist();
        playlist.track_count = track_count;

        if let Some(queue_index) = self
            .playback_queue
            .as_ref()
            .and_then(|queue| canonical_queue_track_index(queue, &playlist.id, &selected_track.mid))
        {
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

    fn open_home_playlist(
        &mut self,
        playlist: UserPlaylist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let resource =
            self.shared_playlist_resource(playlist.clone(), false, PlaylistCachePolicy::Fresh);
        let target = NavigationPage::Playlist {
            playlist: playlist.clone(),
            selected_index: None,
            scroll_position: PlaylistScrollPosition::top(),
            resource: Some(resource.clone()),
        };
        let current = self.current_navigation_page(cx);
        self.record_navigation(current, &target, window, cx);
        self.playlist_list.update(cx, |list, cx| {
            list.set_selected_index(None, window, cx);
        });
        self.open_playlist(
            playlist,
            None,
            false,
            PlaylistCachePolicy::Fresh,
            PlaylistScrollPosition::top(),
            Some(resource),
            cx,
        );
    }

    fn restore_pending_playlist_scroll(&mut self, cx: &mut Context<Self>) {
        let Some(position) = self.pending_playlist_scroll_position else {
            return;
        };
        let target_row = {
            let table = self.track_table.read(cx);
            resolved_playlist_scroll_row(
                position.row,
                table.delegate().tracks().len(),
                table.delegate().has_more(),
            )
        };
        let Some(target_row) = target_row else {
            return;
        };
        self.pending_playlist_scroll_position = None;
        self.track_table.update(cx, |table, cx| {
            if target_row == position.row {
                let scroll_state = &mut *table.vertical_scroll_handle.0.borrow_mut();
                scroll_state.deferred_scroll_to_item = None;
                let current_offset = scroll_state.base_handle.offset();
                scroll_state
                    .base_handle
                    .set_offset(point(current_offset.x, position.offset_y));
            } else {
                table.scroll_to_row(target_row, cx);
            }
            cx.notify();
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn open_playlist(
        &mut self,
        playlist: UserPlaylist,
        selected_index: Option<usize>,
        force_refresh: bool,
        cache_policy: PlaylistCachePolicy,
        scroll_position: PlaylistScrollPosition,
        resource: Option<SharedPlaylistResource>,
        cx: &mut Context<Self>,
    ) {
        self.main_content = MainContent::Playlist;
        self.search_resource = None;
        self.artist_resource = None;

        self.playlist_force_refresh = force_refresh;
        self.selected_playlist_index = selected_index;
        self.pending_playlist_scroll_position = Some(scroll_position);
        if let Some(index) = selected_index {
            self.playlist_list.update(cx, |list, cx| {
                list.delegate_mut().set_selected(index);
                cx.notify();
            });
        }
        self.track_table.update(cx, |table, cx| {
            table.clear_selection(cx);
            table
                .delegate_mut()
                .reset(playlist.id == UserPlaylistId::Liked);
            table.refresh(cx);
            cx.notify();
        });
        let resource = resource.unwrap_or_else(|| {
            self.shared_playlist_resource(playlist.clone(), force_refresh, cache_policy)
        });
        let (playlist, tracks, has_more, loading, loaded) = {
            let state = lock_resource(&resource);
            (
                state.playlist.clone(),
                state.tracks.clone(),
                state.has_more,
                state.loading,
                state.fetched_at_secs != 0,
            )
        };
        self.selected_playlist_resource = Some(resource);
        self.selected_playlist = Some(playlist.clone());
        if let Some(index) = self.selected_playlist_index {
            self.playlist_list.update(cx, |list, cx| {
                list.delegate_mut().update_playlist(index, playlist.clone());
                cx.notify();
            });
        }
        self.track_table.update(cx, |table, cx| {
            table
                .delegate_mut()
                .set_tracks(tracks.clone(), has_more, loading);
            cx.notify();
        });
        self.restore_pending_playlist_scroll(cx);
        self.sync_table_playback_state(cx);
        cx.notify();
        self.prune_page_resources();
        if !loaded || self.pending_playlist_scroll_position.is_some() {
            self.load_playlist_page(cx);
        }
    }

    fn load_playlist_page(&mut self, cx: &mut Context<Self>) {
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(resource) = self.selected_playlist_resource.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.notify_error("QQ 音乐客户端不可用", cx);
            cx.notify();
            return;
        };

        let (playlist, offset) = {
            let mut state = lock_resource(&resource);
            if state.loading || !state.has_more {
                return;
            }
            state.loading = true;
            (state.playlist.clone(), state.next_offset)
        };
        let force_refresh = offset == 0 && self.playlist_force_refresh;
        self.playlist_force_refresh = false;
        let requests = self.playlist_page_requests.clone();
        self.track_table.update(cx, |table, cx| {
            table.delegate_mut().set_loading(true);
            cx.notify();
        });

        let task = RUNTIME.spawn(request_playlist_page(
            requests,
            client,
            credential,
            playlist,
            offset,
            force_refresh,
        ));

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let active = this
                    .selected_playlist_resource
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &resource));
                let mut state = lock_resource(&resource);
                state.loading = false;
                let error = match result {
                    Ok(page) => {
                        state.apply_page(
                            page.playlist,
                            share_items(page.tracks),
                            page.has_more,
                            page.next_offset,
                            offset,
                        );
                        None
                    }
                    Err(error) => Some(format!("加载歌单失败：{error:#}")),
                };
                let snapshot =
                    active.then(|| (state.playlist.clone(), state.tracks.clone(), state.has_more));
                drop(state);
                if let Some((playlist, tracks, has_more)) = snapshot {
                    this.selected_playlist = Some(playlist.clone());
                    if let Some(index) = this.selected_playlist_index {
                        this.playlist_list.update(cx, |list, cx| {
                            list.delegate_mut().update_playlist(index, playlist.clone());
                            cx.notify();
                        });
                    }
                    this.track_table.update(cx, |table, cx| {
                        table
                            .delegate_mut()
                            .set_tracks(tracks.clone(), has_more, false);
                        cx.notify();
                    });
                    if let Some(error) = error {
                        this.notify_error(error, cx);
                    }
                    this.restore_pending_playlist_scroll(cx);
                    if this.pending_playlist_scroll_position.is_some() {
                        this.load_playlist_page(cx);
                    }
                    this.sync_table_playback_state(cx);
                    cx.notify();
                }
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
                modified: restore.queue_modified,
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
        tracks: Vec<Arc<Track>>,
        has_more: bool,
        cx: &mut Context<Self>,
    ) {
        self.home_recommendation_loading = None;
        self.queue_generation = self.queue_generation.wrapping_add(1);
        let generation = self.queue_generation;
        let playlist_resource = self
            .selected_playlist_resource
            .as_ref()
            .filter(|resource| lock_resource(resource).playlist.id == playlist.id)
            .cloned();
        let mut offset = playlist_resource
            .as_ref()
            .map_or(tracks.len() as u64, |resource| {
                lock_resource(resource).next_offset
            });
        self.playback_queue = Some(PlaybackQueue {
            playlist_id: playlist.id.clone(),
            tracks,
            modified: false,
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
        let requests = self.playlist_page_requests.clone();
        let queue_resource = playlist_resource.clone();

        let task = RUNTIME.spawn(async move {
            let mut remaining = Vec::new();
            let mut has_more = true;
            while has_more {
                let page_offset = offset;
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
                let tracks = share_items(page.tracks);
                if let Some(resource) = &queue_resource {
                    lock_resource(resource).apply_page(
                        page.playlist,
                        tracks.clone(),
                        page.has_more,
                        page.next_offset,
                        page_offset,
                    );
                }
                remaining.extend(tracks);
            }
            Ok::<_, anyhow::Error>(remaining)
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.queue_generation != generation {
                    return;
                }
                if let (Some(queue), Ok(tracks)) = (&mut this.playback_queue, result) {
                    for track in &tracks {
                        if !queue.tracks.iter().any(|item| item.mid == track.mid) {
                            queue.tracks.push(track.clone());
                        }
                    }
                    if let Some(resource) = playlist_resource {
                        let state = lock_resource(&resource);
                        let active = this
                            .selected_playlist_resource
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &resource));
                        let snapshot = active.then(|| (state.tracks.clone(), state.has_more));
                        drop(state);
                        if let Some((tracks, has_more)) = snapshot {
                            this.track_table.update(cx, |table, cx| {
                                table.delegate_mut().set_tracks(tracks, has_more, false);
                                cx.notify();
                            });
                            this.restore_pending_playlist_scroll(cx);
                            this.sync_table_playback_state(cx);
                        }
                    }
                    this.persist_current_playback();
                    #[cfg(target_os = "linux")]
                    this.sync_mpris(false);
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
        if let Some(queue_index) = self
            .playback_queue
            .as_ref()
            .and_then(|queue| canonical_queue_track_index(queue, &playlist.id, selected_mid))
        {
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

    fn ensure_track_lyrics(
        &mut self,
        client: ProtocolClient,
        credential: CredentialSession,
        track: Track,
        cx: &mut Context<Self>,
    ) {
        let mid = track.mid.clone();
        if mid.is_empty() {
            return;
        }

        let current_is_same_track = self
            .current_track_data()
            .is_some_and(|current| current.mid == mid);
        if !current_is_same_track && let Some(pending) = self.pending_lyrics_cache.remove(&mid) {
            self.lyrics_cache.insert(mid.clone(), pending);
            cx.notify();
        }
        let had_cached = self.lyrics_cache.contains_key(&mid);
        if self
            .pending_lyrics_cache
            .get(&mid)
            .is_some_and(|lyrics| current_is_same_track && lyrics.is_fresh(unix_timestamp_secs()))
            || self
                .lyrics_cache
                .get(&mid)
                .is_some_and(|lyrics| lyrics.is_fresh(unix_timestamp_secs()))
            || !self.lyrics_loading.insert(mid.clone())
        {
            return;
        }

        self.lyrics_errors.remove(&mid);
        cx.notify();

        let disk_cache = self.lyric_disk_cache.clone();
        let (sender, mut receiver) = mpsc::channel(2);
        let worker_mid = mid.clone();
        drop(RUNTIME.spawn(async move {
            let mut had_cached = had_cached;
            if !had_cached
                && let Some(cache) = disk_cache.as_ref()
                && let Ok(Some(cached)) = cache.load(&worker_mid).await
            {
                let fresh = cached.is_fresh(unix_timestamp_secs(), LYRIC_CACHE_TTL);
                let lyrics = MemoryLyrics {
                    parsed: Arc::new(parse_lyrics(
                        &cached.lyrics.lyric,
                        cached.lyrics.trans_lyric.as_deref(),
                        cached.lyrics.roma_lyric.as_deref(),
                    )),
                    fetched_at_secs: cached.fetched_at_secs,
                };
                if sender
                    .send(LyricLoadEvent::Disk { lyrics, fresh })
                    .await
                    .is_err()
                {
                    return;
                }
                if fresh {
                    return;
                }
                had_cached = true;
            }

            let result = match client.lyrics(&credential, &track).await {
                Ok(result) => {
                    let fetched_at_secs = unix_timestamp_secs();
                    if let Some(cache) = disk_cache.as_ref() {
                        let _ = cache.save(fetched_at_secs, &result).await;
                    }
                    Ok(MemoryLyrics {
                        parsed: Arc::new(parse_lyrics(
                            &result.lyric,
                            result.trans_lyric.as_deref(),
                            result.roma_lyric.as_deref(),
                        )),
                        fetched_at_secs,
                    })
                }
                Err(error) => Err(error),
            };
            let _ = sender
                .send(LyricLoadEvent::Network { result, had_cached })
                .await;
        }));

        cx.spawn(async move |this, cx| {
            while let Some(event) = receiver.recv().await {
                if this
                    .update(cx, |this, cx| {
                        match event {
                            LyricLoadEvent::Disk { lyrics, fresh } => {
                                this.lyrics_errors.remove(&mid);
                                this.lyrics_cache.insert(mid.clone(), lyrics);
                                if fresh {
                                    this.lyrics_loading.remove(&mid);
                                }
                            }
                            LyricLoadEvent::Network { result, had_cached } => {
                                this.lyrics_loading.remove(&mid);
                                match result {
                                    Ok(lyrics) => {
                                        this.lyrics_errors.remove(&mid);
                                        let is_current = this
                                            .current_track_data()
                                            .is_some_and(|track| track.mid == mid);
                                        let unchanged = this
                                            .lyrics_cache
                                            .get(&mid)
                                            .is_some_and(|cached| cached.parsed == lyrics.parsed);
                                        if had_cached && is_current {
                                            if unchanged {
                                                if let Some(cached) =
                                                    this.lyrics_cache.get_mut(&mid)
                                                {
                                                    cached.fetched_at_secs = lyrics.fetched_at_secs;
                                                }
                                                this.pending_lyrics_cache.remove(&mid);
                                            } else {
                                                this.pending_lyrics_cache
                                                    .insert(mid.clone(), lyrics);
                                            }
                                        } else {
                                            this.pending_lyrics_cache.remove(&mid);
                                            this.lyrics_cache.insert(mid.clone(), lyrics);
                                        }
                                    }
                                    Err(error) if !had_cached => {
                                        this.lyrics_errors
                                            .insert(mid.clone(), format!("{error:#}"));
                                    }
                                    Err(_) => {
                                        this.lyrics_errors.remove(&mid);
                                    }
                                }
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if !this.lyrics_cache.contains_key(&mid) && this.lyrics_loading.remove(&mid) {
                    cx.notify();
                }
            });
        })
        .detach();
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

    fn persist_current_playback(&mut self) {
        let Some(account_id) = self.account_id() else {
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
            queue_modified: self
                .playback_queue
                .as_ref()
                .is_some_and(|queue| queue.modified),
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
        let current_mid = self.current_track_data().map(|track| track.mid.clone());
        let loading_mid = self
            .loading_track
            .and_then(|index| self.playback_queue.as_ref()?.tracks.get(index))
            .map(|track| track.mid.clone());
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

    fn current_track_data(&self) -> Option<&Track> {
        self.current_track
            .and_then(|index| {
                self.playback_queue
                    .as_ref()
                    .and_then(|queue| queue.tracks.get(index))
            })
            .map(Arc::as_ref)
    }

    fn ensure_track_like_state(&mut self, mid: String, cx: &mut Context<Self>) {
        if self.liked_tracks.contains_key(&mid) || self.liked_state_loading.contains(&mid) {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(account_id) = credential.snapshot().map(|credential| credential.music_id) else {
            return;
        };
        let liked = self
            .page_resource_cache
            .playlists
            .get(&(account_id, UserPlaylistId::Liked))
            .and_then(Weak::upgrade)
            .and_then(|resource| {
                let state = lock_resource(&resource);
                if !state.is_fresh(unix_timestamp_secs()) || state.fetched_at_secs == 0 {
                    None
                } else if state.tracks.iter().any(|track| track.mid == mid) {
                    Some(true)
                } else if state.has_more {
                    None
                } else {
                    Some(false)
                }
            });
        if let Some(liked) = liked {
            self.liked_tracks.insert(mid, liked);
            cx.notify();
            return;
        }
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        self.liked_state_loading.insert(mid.clone());
        let request_mid = mid.clone();
        let task =
            RUNTIME.spawn(async move { client.track_liked(&credential, &request_mid).await });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.liked_state_loading.remove(&mid);
                if this.account_id().is_none_or(|id| id != account_id) {
                    return;
                }
                match result {
                    Ok(liked) => {
                        this.liked_tracks.insert(mid.clone(), liked);
                    }
                    Err(error) => {
                        if this
                            .current_track_data()
                            .is_some_and(|track| track.mid == mid)
                        {
                            this.notify_error(format!("读取当前歌曲的喜欢状态失败：{error:#}"), cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_current_track_liked(&mut self, cx: &mut Context<Self>) {
        let Some(track) = self.current_track_data().cloned() else {
            return;
        };
        let Some(liked) = self.liked_tracks.get(&track.mid).copied() else {
            self.ensure_track_like_state(track.mid.clone(), cx);
            return;
        };
        self.set_track_liked(track, !liked, cx);
    }

    fn unlike_track(&mut self, track: Track, cx: &mut Context<Self>) {
        if self.liked_toggle_loading.contains(&track.mid) {
            return;
        }
        self.liked_tracks.insert(track.mid.clone(), true);
        self.set_track_liked(track, false, cx);
    }

    fn set_track_liked(&mut self, track: Track, liked: bool, cx: &mut Context<Self>) {
        let mid = track.mid.clone();
        if self.liked_toggle_loading.contains(&mid) {
            return;
        }
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(account_id) = credential.snapshot().map(|credential| credential.music_id) else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.notify_error("QQ 音乐客户端不可用", cx);
            cx.notify();
            return;
        };
        if track.song_id.is_none() {
            self.notify_error(
                format!("歌曲“{}”缺少数字 ID，暂时无法修改喜欢状态", track.title),
                cx,
            );
            cx.notify();
            return;
        }
        let previous = self.liked_tracks.insert(mid.clone(), liked);
        self.liked_toggle_loading.insert(mid.clone());
        cx.notify();

        let request_track = track.clone();
        let task = RUNTIME.spawn(async move {
            client
                .set_track_liked(&credential, &request_track, liked)
                .await
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.liked_toggle_loading.remove(&mid);
                if this.account_id().is_none_or(|id| id != account_id) {
                    return;
                }
                match result {
                    Ok(()) => {
                        this.apply_track_liked(account_id, track.clone(), liked, cx);
                    }
                    Err(error) => {
                        if this.liked_tracks.get(&mid) == Some(&liked) {
                            if let Some(previous) = previous {
                                this.liked_tracks.insert(mid.clone(), previous);
                            } else {
                                this.liked_tracks.remove(&mid);
                            }
                        }
                        this.notify_error(
                            format!(
                                "{}失败：{error:#}",
                                if liked {
                                    "喜欢歌曲"
                                } else {
                                    "取消喜欢"
                                }
                            ),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_track_liked(
        &mut self,
        account_id: u64,
        track: Track,
        liked: bool,
        cx: &mut Context<Self>,
    ) {
        self.library_cache
            .update_liked_track_count(account_id, liked);
        self.playlist_list.update(cx, |list, cx| {
            list.delegate_mut().update_liked_track_count(liked);
            cx.notify();
        });
        let cached_resource = self
            .page_resource_cache
            .playlists
            .get(&(account_id, UserPlaylistId::Liked))
            .and_then(Weak::upgrade);
        let active_resource = self
            .selected_playlist_resource
            .as_ref()
            .and_then(|resource| {
                (lock_resource(resource).playlist.id == UserPlaylistId::Liked)
                    .then(|| resource.clone())
            });
        let mut resources = self
            .navigation_history
            .playlist_resources(&UserPlaylistId::Liked);
        for resource in active_resource.iter().chain(cached_resource.iter()) {
            if !resources
                .iter()
                .any(|current| Arc::ptr_eq(current, resource))
            {
                resources.push(resource.clone());
            }
        }
        let mut active_snapshot = None;
        for resource in resources {
            let snapshot = update_liked_playlist_resource(&resource, &track, liked);
            if active_resource
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &resource))
            {
                active_snapshot = snapshot;
            }
        }
        let Some((playlist, tracks, has_more)) = active_snapshot else {
            return;
        };
        self.selected_playlist = Some(playlist);
        self.track_table.update(cx, |table, cx| {
            table.delegate_mut().set_tracks(tracks, has_more, false);
            table.clear_selection(cx);
            cx.notify();
        });
        self.sync_table_playback_state(cx);
    }

    fn current_duration(&self) -> Option<Duration> {
        self.current_track_data()
            .map(|track| Duration::from_secs(track.duration_seconds))
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

    fn render_playlist_header(
        &mut self,
        compact: bool,
        narrow: bool,
        scale_factor: f32,
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
            scale_factor,
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
        let has_tracks = !self.track_table.read(cx).delegate().tracks().is_empty();
        let long_title = playlist_title_is_long(&playlist.title);
        let title_size = if long_title {
            if narrow {
                px(22.)
            } else if compact {
                px(28.)
            } else {
                px(36.)
            }
        } else if narrow {
            px(28.)
        } else if compact {
            px(34.)
        } else {
            px(44.)
        };
        let title_line_height = if long_title {
            if narrow {
                px(29.)
            } else if compact {
                px(36.)
            } else {
                px(46.)
            }
        } else if narrow {
            px(36.)
        } else if compact {
            px(44.)
        } else {
            px(56.)
        };
        div()
            .min_h(if narrow {
                px(182.)
            } else if compact {
                px(206.)
            } else {
                px(238.)
            })
            .w_full()
            .flex_shrink_0()
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
                                    .when_else(
                                        long_title,
                                        |title| title.line_clamp(3),
                                        |title| title.truncate(),
                                    )
                                    .text_size(title_size)
                                    .line_height(title_line_height)
                                    .font_semibold()
                                    .child(playlist.title),
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
                                            img(cached_image_source(url, px(18.), scale_factor))
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
                                                    self.color_theme.palette().primary_foreground,
                                                    px(17.),
                                                ))
                                                .child("播放全部"),
                                        )
                                        .when(!has_tracks, |button| button.bg(theme.button_primary))
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
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme.background)
            .child(self.render_playlist_header(compact, narrow, scale_factor, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(page_horizontal_padding(narrow))
                    .pb_4()
                    .child(
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
        scale_factor: f32,
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
                scale_factor,
                cx,
            ));
        let (track_count, has_tracks) =
            self.artist_resource
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
                                                    self.color_theme.palette().primary_foreground,
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

    fn render_artist_content(
        &mut self,
        compact: bool,
        narrow: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (
            songs,
            songs_loading,
            songs_loading_more,
            song_error,
            albums,
            albums_loading,
            albums_loading_more,
            album_error,
        ) = self.artist_resource.as_ref().map_or_else(
            || (None, false, false, None, None, false, false, None),
            |resource| {
                let state = lock_resource(resource);
                (
                    state.songs.clone(),
                    state.songs_loading,
                    state.songs_loading_more,
                    state.song_error.clone(),
                    state.albums.clone(),
                    state.albums_loading,
                    state.albums_loading_more,
                    state.album_error.clone(),
                )
            },
        );
        let song_has_more = songs
            .as_ref()
            .is_some_and(|page| page.items.len() > self.artist_visible_song_count || page.has_more);
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
                        .on_click(cx.listener(|this, _, _, cx| this.load_artist_songs(false, cx))),
                )
                .into_any_element()
        } else if let Some(songs) = songs.filter(|page| !page.items.is_empty()) {
            let visible = self.artist_visible_song_count.min(songs.items.len());
            self.render_song_rows(songs.items[..visible].to_vec(), narrow, scale_factor, cx)
        } else {
            h_flex()
                .h(px(72.))
                .items_center()
                .text_color(theme.muted_foreground)
                .child("暂无歌曲")
                .into_any_element()
        };

        let album_has_more = albums.as_ref().is_some_and(|page| {
            page.items.len() > self.artist_visible_album_count || page.has_more
        });
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
        } else if let Some(error) = album_error {
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
            let visible = self.artist_visible_album_count.min(albums.items.len());
            self.render_search_cards(
                SearchCategory::Albums,
                Vec::new(),
                albums.items[..visible].to_vec(),
                Vec::new(),
                compact,
                scale_factor,
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
                                                        .loading(songs_loading_more)
                                                        .disabled(songs_loading_more)
                                                        .label(if songs_loading_more {
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
                                                        .loading(albums_loading_more)
                                                        .disabled(albums_loading_more)
                                                        .label(if albums_loading_more {
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

    fn render_home(
        &mut self,
        compact: bool,
        narrow: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                let cover = playlist_cover(&playlist, cover_size, px(14.), scale_factor, cx);
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
        let radar_icon = media_icon_hsla(MediaIcon::Radar, theme.primary, px(25.));
        let guess_icon = media_icon_hsla(MediaIcon::Headphones, theme.primary, px(25.));
        let recommendation_cards = [
            Button::new("home-radar")
                .ghost()
                .w(feature_width)
                .h(px(92.))
                .p_4()
                .rounded(px(12.))
                .bg(theme.muted.opacity(0.7))
                .when(recommendation_loading, |button| {
                    button
                        .bg(theme.muted.opacity(0.7))
                        .text_color(theme.secondary_foreground)
                })
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
                .when(recommendation_loading, |button| {
                    button
                        .bg(theme.muted.opacity(0.7))
                        .text_color(theme.secondary_foreground)
                })
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
                    .px(page_horizontal_padding(narrow))
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
                                                .child("智能推荐"),
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
                                                .child("今日歌单"),
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

    fn render_song_rows(
        &mut self,
        songs: Vec<Arc<Track>>,
        narrow: bool,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let monospace_font = self.fonts.monospace.clone();
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
                    track.cover_url.clone(),
                    MediaIcon::Music,
                    px(48.),
                    px(9.),
                    scale_factor,
                    cx,
                );
                Button::new(format!("artist-song-{}-{}", track.mid, index))
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

    #[allow(clippy::too_many_arguments)]
    fn render_search_cards(
        &mut self,
        category: SearchCategory,
        artists: Vec<Arc<SearchArtist>>,
        albums: Vec<Arc<SearchAlbum>>,
        playlists: Vec<Arc<UserPlaylist>>,
        compact: bool,
        scale_factor: f32,
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
                        scale_factor,
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
                            this.open_search_artist(artist.as_ref().clone(), window, cx)
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
                        scale_factor,
                        cx,
                    );
                    let playlist = album.as_ref().clone().into_playlist();
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
                        scale_factor,
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
                            this.open_home_playlist(playlist.as_ref().clone(), window, cx)
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

    fn render_search(
        &mut self,
        compact: bool,
        narrow: bool,
        scale_factor: f32,
        content_width: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let active_category = self.search_category;
        let tabs = SearchCategory::ALL
            .into_iter()
            .map(|category| {
                let selected = active_category == category;
                Button::new(format!("search-category-{}", category.label()))
                    .ghost()
                    .h(px(36.))
                    .px_3()
                    .rounded(px(999.))
                    .selected(selected)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(media_icon_hsla(
                                category.icon(),
                                if selected {
                                    theme.primary
                                } else {
                                    theme.secondary_foreground
                                },
                                px(17.),
                            ))
                            .child(category.label()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.search_scroll_offsets = this.current_search_scroll_offsets(cx);
                        this.search_category = category;
                        this.pending_search_scroll = true;
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();

        let (results, search_loading, search_loading_more, search_error) =
            self.search_resource.as_ref().map_or_else(
                || (None, false, false, None),
                |resource| {
                    let state = lock_resource(resource);
                    (
                        state.results.clone(),
                        state.loading,
                        state.loading_more[active_category.index()],
                        state.error.clone(),
                    )
                },
            );
        let has_results = results.is_some();
        let mut content = v_flex().w_full();
        let mut is_empty = false;
        if let Some(results) = results {
            match self.search_category {
                SearchCategory::Songs => {
                    is_empty = results.songs.items.is_empty();
                    let playing_mid = self.current_track_data().map(|track| track.mid.as_str());
                    let loading_mid = self
                        .loading_track
                        .and_then(|index| self.playback_queue.as_ref()?.tracks.get(index))
                        .map(|track| track.mid.as_str());
                    let playing_index = results
                        .songs
                        .items
                        .iter()
                        .position(|track| Some(track.mid.as_str()) == playing_mid);
                    let loading_index = results
                        .songs
                        .items
                        .iter()
                        .position(|track| Some(track.mid.as_str()) == loading_mid);
                    let playback_active = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
                    self.search_track_table.update(cx, |table, cx| {
                        let delegate = table.delegate_mut();
                        delegate.set_tracks(
                            results.songs.items.clone(),
                            results.songs.has_more,
                            search_loading_more,
                        );
                        delegate.set_playback_state(playing_index, loading_index, playback_active);
                        cx.notify();
                    });
                    content = v_flex().w_full().flex_1().min_h_0().child(
                        DataTable::new(&self.search_track_table)
                            .bordered(false)
                            .stripe(false)
                            .with_size(px(64.)),
                    );
                }
                SearchCategory::Artists => {
                    is_empty = results.artists.items.is_empty();
                    let cover_size = if compact { px(132.) } else { px(148.) };
                    let card_width = cover_size + px(16.);
                    let available_width =
                        (content_width - if narrow { px(40.) } else { px(64.) }).max(px(1.));
                    let columns = ((f32::from(available_width) + 16.)
                        / (f32::from(card_width) + 16.))
                        .floor() as usize;
                    self.search_card_grid.update(cx, |list, cx| {
                        if list.delegate_mut().set_cards(
                            results
                                .artists
                                .items
                                .iter()
                                .cloned()
                                .map(SearchCard::Artist)
                                .collect(),
                            columns,
                            card_width,
                            cover_size + px(62.),
                            cover_size,
                            scale_factor,
                            results.artists.has_more,
                            search_loading_more,
                        ) {
                            cx.notify();
                        }
                    });
                    content = v_flex().w_full().flex_1().min_h_0().child(
                        List::new(&self.search_card_grid)
                            .size_full()
                            .scrollbar_visible(false),
                    );
                }
                SearchCategory::Albums => {
                    is_empty = results.albums.items.is_empty();
                    let cover_size = if compact { px(132.) } else { px(148.) };
                    let card_width = cover_size + px(16.);
                    let available_width =
                        (content_width - if narrow { px(40.) } else { px(64.) }).max(px(1.));
                    let columns = ((f32::from(available_width) + 16.)
                        / (f32::from(card_width) + 16.))
                        .floor() as usize;
                    self.search_card_grid.update(cx, |list, cx| {
                        if list.delegate_mut().set_cards(
                            results
                                .albums
                                .items
                                .iter()
                                .cloned()
                                .map(SearchCard::Album)
                                .collect(),
                            columns,
                            card_width,
                            cover_size + px(74.),
                            cover_size,
                            scale_factor,
                            results.albums.has_more,
                            search_loading_more,
                        ) {
                            cx.notify();
                        }
                    });
                    content = v_flex().w_full().flex_1().min_h_0().child(
                        List::new(&self.search_card_grid)
                            .size_full()
                            .scrollbar_visible(false),
                    );
                }
                SearchCategory::Playlists => {
                    is_empty = results.playlists.items.is_empty();
                    let cover_size = if compact { px(132.) } else { px(148.) };
                    let card_width = cover_size + px(16.);
                    let available_width =
                        (content_width - if narrow { px(40.) } else { px(64.) }).max(px(1.));
                    let columns = ((f32::from(available_width) + 16.)
                        / (f32::from(card_width) + 16.))
                        .floor() as usize;
                    self.search_card_grid.update(cx, |list, cx| {
                        if list.delegate_mut().set_cards(
                            results
                                .playlists
                                .items
                                .iter()
                                .cloned()
                                .map(SearchCard::Playlist)
                                .collect(),
                            columns,
                            card_width,
                            cover_size + px(74.),
                            cover_size,
                            scale_factor,
                            results.playlists.has_more,
                            search_loading_more,
                        ) {
                            cx.notify();
                        }
                    });
                    content = v_flex().w_full().flex_1().min_h_0().child(
                        List::new(&self.search_card_grid)
                            .size_full()
                            .scrollbar_visible(false),
                    );
                }
            }
        }
        let song_list_active = active_category == SearchCategory::Songs
            && !search_loading
            && has_results
            && !is_empty
            && search_error.is_none();
        let card_results_active = active_category != SearchCategory::Songs
            && !search_loading
            && has_results
            && !is_empty
            && search_error.is_none();
        let virtual_list_active = song_list_active || card_results_active;

        if virtual_list_active && self.pending_search_scroll {
            let offset = point(px(0.), self.search_scroll_offsets[active_category.index()]);
            if song_list_active {
                self.search_track_table.update(cx, |table, cx| {
                    let scroll = &mut *table.vertical_scroll_handle.0.borrow_mut();
                    scroll.deferred_scroll_to_item = None;
                    scroll.base_handle.set_offset(offset);
                    cx.notify();
                });
            } else {
                self.search_card_grid.update(cx, |list, cx| {
                    list.scroll_handle().set_offset(offset);
                    cx.notify();
                });
            }
            self.pending_search_scroll = false;
        }

        let body_content = v_flex()
            .w_full()
            .mx_auto()
            .px(page_horizontal_padding(narrow))
            .when(virtual_list_active, |this| this.h_full().min_h_0())
            .when(!virtual_list_active, |this| this.pb_8())
            .gap_5()
            .when(search_loading, |this| {
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
                !search_loading && !has_results && search_error.is_some(),
                |this| {
                    this.child(
                        v_flex()
                            .h(px(260.))
                            .items_center()
                            .justify_center()
                            .gap_4()
                            .text_color(theme.muted_foreground)
                            .child(search_error.clone().unwrap_or_default())
                            .child(
                                Button::new("retry-search")
                                    .outline()
                                    .h(px(44.))
                                    .px_4()
                                    .label("重新搜索")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let Some(resource) = this.search_resource.clone() else {
                                            return;
                                        };
                                        this.start_search(resource, this.search_query.clone(), cx)
                                    })),
                            ),
                    )
                },
            )
            .when(!search_loading && has_results && is_empty, |this| {
                this.child(
                    div()
                        .h(px(220.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.muted_foreground)
                        .child(format!("没有找到相关{}", self.search_category.label())),
                )
            })
            .when(!search_loading && has_results && !is_empty, |this| {
                this.child(content)
            })
            .when(has_results && search_error.is_some(), |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded(px(9.))
                        .bg(theme.danger.opacity(0.1))
                        .text_sm()
                        .text_color(theme.danger)
                        .child(search_error.clone().unwrap_or_default()),
                )
            });
        let body = if virtual_list_active {
            div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(body_content)
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(body_content)
                .into_any_element()
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .bg(theme.background)
            .child(
                v_flex()
                    .w_full()
                    .mx_auto()
                    .px(page_horizontal_padding(narrow))
                    .pt_2()
                    .pb_3()
                    .flex_shrink_0()
                    .child(h_flex().gap_1().children(tabs)),
            )
            .child(body)
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
    use super::lyrics_view::{
        LYRIC_MINIMUM_CONTRAST, adjacent_lyric_timing, combined_lyric_frame_interval,
        contrast_ratio, lyric_edge_opacity, lyric_frame_is_due, lyric_horizontal_scroll_offset,
        lyric_position_for_frame_rate, readable_lyric_color,
    };
    use super::navigation::{PlaylistResource, SearchResource};
    use super::{
        NavigationHistory, NavigationPage, PlaybackQueue, PlaylistScrollPosition, SearchCategory,
        SearchVisibleCounts, canonical_queue_track_index, format_playback_time,
        insert_external_track_after_current, insert_track_after_current, parse_lyrics,
        playlist_title_is_long, resolved_playlist_scroll_row,
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
