use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Disableable as _, ResizableState, Selectable as _, Sizable as _,
    StyledExt as _,
    avatar::Avatar,
    button::{Button, ButtonVariants as _},
    h_flex, h_resizable,
    list::{List, ListEvent, ListState},
    resizable_panel,
    slider::{Slider, SliderEvent, SliderState},
    table::{DataTable, TableEvent, TableState},
    v_flex,
};
use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;

use crate::cache::AudioCache;
use crate::credentials::CredentialStore;
use crate::design::{self, ColorTheme};
use crate::http::cached_image_source;
use crate::icons::{MediaIcon, media_icon, media_icon_hsla};
use crate::library::{PlaylistListDelegate, TrackTableDelegate, format_duration, playlist_cover};
use crate::player::{AudioPlayer, PreparedPlayback};
use crate::settings::{
    AppSettings, CdnCacheStore, LibraryCache, LibraryCacheStore, PersistedLibraryView,
    PersistedPlayback, PersistedWindowSize, SettingsStore,
};
use crate::singleflight::SingleFlight;
use qqmusic_api::integration::{
    LoginEvent, PlaylistPage, ProtocolClient, QqCredential, Quality, Track, UserPlaylist,
    UserPlaylistId, UserProfile, refresh_credential, run_qr_login,
};

const PAGE_SIZE: u64 = 100;
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

fn progress_fraction(position: Duration, duration: Duration) -> f32 {
    if duration.is_zero() {
        0.
    } else {
        (position.as_secs_f32() / duration.as_secs_f32()).clamp(0., 1.)
    }
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

    playlist_list: Entity<ListState<PlaylistListDelegate>>,
    track_table: Entity<TableState<TrackTableDelegate>>,
    progress_slider: Entity<SliderState>,
    volume_slider: Entity<SliderState>,

    audio: Option<AudioPlayer>,
    audio_cache: Option<AudioCache>,
    protocol_client: Option<ProtocolClient>,
    cdn_maintenance: Option<JoinHandle<()>>,
    playback_queue: Option<PlaybackQueue>,
    queue_generation: u64,
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
        let (load_more_sender, load_more_receiver) = async_channel::bounded(1);
        let track_table = cx.new(|cx| {
            TableState::new(TrackTableDelegate::new(load_more_sender), window, cx)
                .col_selectable(false)
                .col_movable(false)
                .sortable(false)
        });
        let progress_slider = cx.new(|_| progress_slider_state(0.));
        let volume_slider = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(1.)
                .step(0.01)
                .default_value(settings.volume)
        });

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
            playlist_list,
            track_table,
            progress_slider,
            volume_slider,
            audio,
            audio_cache,
            protocol_client,
            cdn_maintenance: None,
            playback_queue: None,
            queue_generation: 0,
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
    }

    pub(crate) fn start_background_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_TICK).await;
                if this
                    .update_in(cx, |this, window, cx| this.tick(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn window_size(&self) -> Option<PersistedWindowSize> {
        self.settings.window_size
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
        let playback_playlist = playback_restore.as_ref().and_then(|restore| {
            playlists
                .iter()
                .find(|playlist| playlist.id == restore.playlist_id)
                .cloned()
        });
        if self.pending_playback_restore.is_some() && playback_playlist.is_none() {
            self.clear_persisted_playback();
        }
        self.playlist_list.update(cx, |list, cx| {
            list.delegate_mut().set_playlists(playlists);
            cx.notify();
        });
        if count > 0 {
            self.select_playlist_with_refresh(viewed_index.unwrap_or(0), force_refresh, cx);
        } else {
            self.status = StatusMessage::info("QQ 音乐账号中没有可显示的歌单");
            cx.notify();
        }
        if let (Some(playlist), Some(restore)) = (playback_playlist, playback_restore) {
            self.restore_playback_queue(playlist, restore, cx);
        }
    }

    fn select_playlist(&mut self, index: usize, cx: &mut Context<Self>) {
        self.select_playlist_with_refresh(index, false, cx);
    }

    fn select_playlist_with_refresh(
        &mut self,
        index: usize,
        force_refresh: bool,
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

        self.playlist_generation = self.playlist_generation.wrapping_add(1);
        self.playlist_force_refresh = force_refresh;
        self.playlist_cache_revision = new_cache_revision();
        self.selected_playlist_index = Some(index);
        self.selected_playlist = Some(playlist.clone());
        self.page_offset = 0;
        self.page_loading = false;
        self.playlist_list.update(cx, |list, cx| {
            list.delegate_mut().set_selected(index);
            cx.notify();
        });
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

    fn restore_playback_queue(
        &mut self,
        playlist: UserPlaylist,
        restore: PersistedPlayback,
        cx: &mut Context<Self>,
    ) {
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let cached = self.library_cache.fresh_playlist(
            credential.music_id,
            &playlist.id,
            unix_timestamp_secs(),
            LIBRARY_CACHE_TTL,
        );
        let (initial_tracks, initial_offset, initial_has_more, cached_playlist, cache_revision) =
            cached
                .map(|snapshot| {
                    (
                        snapshot.tracks,
                        snapshot.next_offset,
                        snapshot.has_more,
                        snapshot.playlist,
                        snapshot.revision,
                    )
                })
                .unwrap_or_else(|| (Vec::new(), 0, true, playlist.clone(), new_cache_revision()));
        let client = self.protocol_client.clone();
        let requests = self.playlist_page_requests.clone();

        self.queue_generation = self.queue_generation.wrapping_add(1);
        let generation = self.queue_generation;
        let playlist_id = playlist.id.clone();
        let track_mid = restore.track_mid.clone();
        let account_id = credential.music_id;
        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = async {
                let mut tracks = initial_tracks;
                let mut offset = initial_offset;
                let mut has_more = initial_has_more;
                while has_more {
                    let client = client.as_ref().context("QQ 音乐客户端不可用")?;
                    let page = request_playlist_page(
                        requests.clone(),
                        client.clone(),
                        credential.clone(),
                        playlist.clone(),
                        offset,
                        false,
                    )
                    .await
                    .context("无法恢复 QQ 音乐播放队列")?;
                    offset = page.next_offset;
                    has_more = page.has_more;
                    tracks.extend(page.tracks);
                }
                let index = tracks.iter().position(|track| track.mid == track_mid);
                Ok::<_, anyhow::Error>((tracks, index))
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
                match result {
                    Ok((tracks, index)) => {
                        this.library_cache.replace_playlist(
                            account_id,
                            cached_playlist,
                            tracks.clone(),
                            false,
                            tracks.len() as u64,
                            unix_timestamp_secs(),
                            cache_revision,
                        );
                        this.persist_library_cache();
                        if let Some(index) = index {
                            let resume_at = restore.resume_position(tracks[index].duration_seconds);
                            this.pending_playback_restore = None;
                            this.playback_queue = Some(PlaybackQueue {
                                playlist_id,
                                tracks,
                            });
                            this.start_playback(index, resume_at, None, false, cx);
                        } else {
                            this.clear_persisted_playback();
                        }
                    }
                    Err(error) => {
                        this.status = StatusMessage::error(format!("恢复播放队列失败：{error:#}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn replace_playback_queue(
        &mut self,
        playlist: UserPlaylist,
        tracks: Vec<Track>,
        has_more: bool,
        cx: &mut Context<Self>,
    ) {
        self.queue_generation = self.queue_generation.wrapping_add(1);
        let generation = self.queue_generation;
        let mut offset = tracks.len() as u64;
        self.playback_queue = Some(PlaybackQueue {
            playlist_id: playlist.id.clone(),
            tracks,
        });

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
                    queue.tracks.extend(tracks);
                    let cached_tracks = queue.tracks.clone();
                    this.library_cache.replace_playlist(
                        account_id,
                        cached_playlist,
                        cached_tracks,
                        false,
                        queue.tracks.len() as u64,
                        unix_timestamp_secs(),
                        cache_revision,
                    );
                    this.persist_library_cache();
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
        self.persist_current_playback();
        self.sync_table_playback_state(cx);
        cx.notify();

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
        let playing = audio.toggle();
        self.status = StatusMessage::info(if playing {
            "继续播放".to_owned()
        } else {
            "已暂停".to_owned()
        });
        self.persist_current_playback();
        self.sync_table_playback_state(cx);
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
        let next = if automatic && self.repeat_mode == RepeatMode::One {
            Some(index)
        } else if self.shuffle && len > 1 {
            Some(self.random_track_index(index, len))
        } else if index + 1 < len {
            Some(index + 1)
        } else if self.repeat_mode == RepeatMode::All {
            Some(0)
        } else {
            None
        };
        if let Some(next) = next {
            self.start_playback(next, Duration::ZERO, None, true, cx);
        } else {
            self.playback_started = false;
            self.position = self.current_duration().unwrap_or_default();
            self.status = StatusMessage::info("当前播放队列已结束");
            self.persist_current_playback();
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
        let position = if self.loading_track.is_some() {
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

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.seek_preview.is_none() {
            if self.loading_track.is_none() {
                self.position = self
                    .audio
                    .as_ref()
                    .map(AudioPlayer::position)
                    .unwrap_or_default();
            }
            let progress = self
                .current_duration()
                .map_or(0., |duration| progress_fraction(self.position, duration));
            self.progress_slider.update(cx, |slider, cx| {
                slider.set_value(progress, window, cx);
            });
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
        self.selected_playlist_index = None;
        self.selected_playlist = None;
        self.page_loading = false;
        self.playback_queue = None;
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
                    .child(
                        div()
                            .size(px(46.))
                            .rounded(theme.radius_lg)
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xl()
                            .font_bold()
                            .child("L"),
                    )
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
                    .child(
                        div()
                            .size(px(34.))
                            .rounded(px(10.))
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .flex()
                            .items_center()
                            .justify_center()
                            .font_semibold()
                            .child("L"),
                    )
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
        let owner = if playlist.owner.is_empty() {
            self.profile
                .as_ref()
                .map(|profile| profile.nickname.as_str())
                .unwrap_or("QQ 音乐用户")
        } else {
            &playlist.owner
        };
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
                                        .child("歌单"),
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
                                    .child(playlist.title),
                            )
                            .when(
                                playlist.id != UserPlaylistId::Liked
                                    && !playlist.description.is_empty(),
                                |this| {
                                    this.child(
                                        div()
                                            .max_w(px(720.))
                                            .line_clamp(1)
                                            .text_sm()
                                            .text_color(theme.secondary_foreground)
                                            .child(playlist.description),
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
                                    .child(
                                        div()
                                            .min_w_0()
                                            .max_w(if narrow { px(120.) } else { px(200.) })
                                            .truncate()
                                            .child(owner.to_owned()),
                                    )
                                    .child(
                                        div()
                                            .font_normal()
                                            .text_color(theme.secondary_foreground)
                                            .child(format!("· {} 首歌曲", playlist.track_count)),
                                    ),
                            )
                            .child(
                                h_flex().pt_2().gap_2().child(
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
        let content = v_flex()
            .h_full()
            .min_w_0()
            .flex_1()
            .child(
                h_flex()
                    .h(px(52.))
                    .w_full()
                    .flex_shrink_0()
                    .justify_between()
                    .px_6()
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(theme.secondary_foreground)
                            .child("QQ 音乐")
                            .child(div().text_color(theme.muted_foreground).child("/"))
                            .child("音乐库"),
                    )
                    .child(self.render_account(cx)),
            )
            .child(self.render_playlist_content(compact, narrow, cx));
        v_flex()
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
