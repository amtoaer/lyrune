//! GPUI 应用实体：集中创建控件、连接事件和组合主窗口，各职责的实现放在子模块。
//! 子模块中的 impl LyruneView 共享这个实体，保持现有订阅、任务及退出保存的寿命。
//! 阅读顺序和跨模块约束见 docs/architecture.md。

mod account;
mod animation;
mod artist;
mod home;
mod lyrics;
mod lyrics_loading;
mod lyrics_view;
mod navigation;
mod playback;
mod playback_sync;
mod player_view;
mod playlists;
mod preferences;
mod progress_view;
mod queue;
mod search;
mod settings;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{prelude::*, *};
use gpui_base::motion::{Transition, transition};
use gpui_component::{
    ActiveTheme as _, Disableable as _, ResizableState, Root, Sizable as _, StyledExt as _, Theme,
    WindowExt as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex, h_resizable,
    input::{Input, InputEvent, InputState},
    list::{List, ListEvent, ListState},
    notification::Notification,
    resizable_panel,
    table::{TableEvent, TableState},
    v_flex,
};
use hashbrown::HashMap;
use lyrune_api::{PlaylistPage, ProtocolClient};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use self::{
    account::{Account, AccountEvent, AccountState},
    artist::ArtistPageState,
    home::HomePageState,
    lyrics_loading::LyricsCache,
    lyrics_view::{
        CollapseLyrics, LYRIC_BACKGROUND_OVERLAY_OPACITY, LYRIC_EXPANSION_DURATION, LyricsContent,
        LyricsInput, LyricsView, combined_lyric_frame_interval, interpolate_color,
    },
    navigation::{MainContent, NavigationHistory, PageResourceCache},
    playback::Playback,
    player_view::{PlayerAction, PlayerControlsView},
    playlists::{PlaylistPageKey, PlaylistPageState},
    preferences::Preferences,
    progress_view::{PlaybackProgressView, SeekTo},
    search::SearchPageState,
    settings::SettingsInputs,
};
use crate::cache::{AudioCache, audio_cache_limit_bytes};
use crate::design::AppFonts;
use crate::http::CachedImageCache;
use crate::icons::{MediaIcon, lyrune_icon, media_icon_hsla};
#[cfg(target_os = "linux")]
use crate::inhibit::InhibitHandle;
use crate::library::{
    PlaylistListDelegate, SearchCardGridDelegate, TrackTableDelegate, TrackTableEvent,
};
use crate::lyrics_cache::LyricDiskCache;
#[cfg(target_os = "linux")]
use crate::mpris::MprisHandle;
use crate::player::AudioPlayer;
use crate::runtime::RUNTIME;
use crate::settings::{
    AppSettings, CdnCacheStore, LibraryCache, PersistedWindowSize, WindowDecoration,
};
use crate::singleflight::SingleFlight;
use crate::update::AvailableUpdate;

const PAGE_SIZE: u64 = 100;
const ARTIST_PAGE_SIZE: u64 = 5;
const ARTIST_ALBUM_PAGE_SIZE: u64 = 10;
const SEARCH_PAGE_SIZE: usize = 20;
const CDN_REFRESH_RETRY: Duration = Duration::from_secs(60);
const LIBRARY_CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const PLAYER_BAR_HEIGHT: f32 = 112.;

fn page_horizontal_padding(narrow: bool) -> Pixels {
    if narrow { px(20.) } else { px(24.) }
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

pub struct LyruneView {
    // 账号和浏览状态；当前浏览的歌单可以与下面的播放队列不同。
    account: Entity<Account>,
    account_menu_open: bool,
    library_loading: bool,
    playlist: PlaylistPageState,
    library_generation: u64,
    playlist_page_requests: SingleFlight<PlaylistPageKey, PlaylistPage>,
    main_content: MainContent,
    navigation_history: NavigationHistory,
    home: HomePageState,
    search: SearchPageState,
    artist: ArtistPageState,

    // GPUI 控件由实体持有，子模块更新它们；订阅句柄保存在下方，不能创建后立即丢弃。
    playlist_list: Entity<ListState<PlaylistListDelegate>>,
    track_table: Entity<TableState<TrackTableDelegate>>,
    search_track_table: Entity<TableState<TrackTableDelegate>>,
    search_card_grid: Entity<ListState<SearchCardGridDelegate>>,
    search_input: Entity<InputState>,
    settings_inputs: SettingsInputs,
    settings_scroll_handle: ScrollHandle,
    progress: Entity<PlaybackProgressView>,
    player_controls: Entity<PlayerControlsView>,
    _player_subscription: Option<Subscription>,
    image_cache: Entity<CachedImageCache>,
    window_drag_pressed: bool,

    // 音频、网络服务及维护任务。队列版本与单次音源加载版本分别失效。
    playback: Playback,
    audio_cache: Option<AudioCache>,
    protocol_client: Option<ProtocolClient>,
    cdn_maintenance: Option<JoinHandle<()>>,
    audio_cache_maintenance: Option<JoinHandle<()>>,
    quality_menu_open: bool,
    // expanded 是用户目标，visible 包含过渡动画，fully_expanded 用于跳过被遮挡的主页面渲染。
    cover_backdrop_expanded: bool,
    cover_backdrop_visible: bool,
    cover_backdrop_fully_expanded: bool,
    lyrics: LyricsCache,
    lyrics_view: Entity<LyricsView>,
    _lyrics_subscription: Option<Subscription>,
    preferences: Entity<Preferences>,
    library_cache: LibraryCache,
    page_resource_cache: PageResourceCache,
    liked_tracks: HashMap<String, bool>,
    liked_state_loading: HashSet<String>,
    liked_toggle_loading: HashSet<String>,
    update_check_in_progress: bool,

    window_handle: AnyWindowHandle,
    // 保存控件订阅句柄；下方两个窗口观察句柄由 attach_window 在窗口重开时替换。
    _subscriptions: Vec<Subscription>,
    _window_subscription: Option<Subscription>,
    _appearance_subscription: Option<Subscription>,
    background_tick_wake: Option<mpsc::Sender<()>>,
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
                Some(Arc::new(player))
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
        let account = cx.new(|_| Account::new(protocol_client.clone()));

        let playlist_list =
            cx.new(|cx| ListState::new(PlaylistListDelegate::new(), window, cx).searchable(false));
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("想播放什么？")
                .context_menu(false)
        });
        let settings_inputs = SettingsInputs::new(&settings, window, cx);
        // Delegate 只发出交互/补页信号；这里把信号接回应用实体，避免控件直接管理网络状态。
        let (load_more_sender, load_more_receiver) = mpsc::channel(1);
        let (search_load_more_sender, search_load_more_receiver) = mpsc::channel(1);
        let (search_card_load_more_sender, search_card_load_more_receiver) = mpsc::channel(1);
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
                TrackTableDelegate::new(search_load_more_sender, track_event_sender.clone()),
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
        let progress =
            cx.new(|cx| PlaybackProgressView::new(audio.clone(), fonts.monospace.clone(), cx));
        let lyrics_view = cx.new(|_| LyricsView::new(audio.clone(), fonts.lyrics.clone()));
        let player_controls = cx.new(|cx| {
            PlayerControlsView::new(
                settings.volume,
                settings.active_color_theme(cx.window_appearance()),
                settings.playback_quality,
                cx,
            )
        });
        let image_cache = CachedImageCache::new(settings.image_cache_capacity, cx);
        let history_limit = settings.navigation_history_limit;
        let preferences = cx.new(|cx| Preferences::new(settings, fonts, cx));
        let subscriptions = vec![
            cx.observe(&preferences, |_, _, cx| cx.notify()),
            cx.observe(&account, |_, _, cx| cx.notify()),
            cx.subscribe(&account, |this, _, event: &AccountEvent, cx| {
                this.handle_account_event(event, cx)
            }),
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
                &settings_inputs.audio_cache_limit,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.apply_audio_cache_limit(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &settings_inputs.image_cache_capacity,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.apply_image_cache_capacity(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &settings_inputs.navigation_history_limit,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.apply_navigation_history_limit(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &settings_inputs.lyric_font_scale,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.apply_scale_settings(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &settings_inputs.lyric_line_spacing,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.apply_scale_settings(window, cx);
                    }
                },
            ),
            cx.subscribe(&progress, |this, _, event: &SeekTo, cx| {
                this.seek_to(event.0, cx);
            }),
        ];

        Self::listen_for_load_more(load_more_receiver, Self::load_playlist_page, cx);
        Self::listen_for_load_more(search_load_more_receiver, Self::load_more_search, cx);
        Self::listen_for_load_more(search_card_load_more_receiver, Self::load_more_search, cx);

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

        let mut view = Self {
            account,
            account_menu_open: false,
            library_loading: false,
            playlist: PlaylistPageState::default(),
            library_generation: 0,
            playlist_page_requests: SingleFlight::default(),
            main_content: MainContent::Home,
            navigation_history: NavigationHistory::new(history_limit),
            home: HomePageState::default(),
            search: SearchPageState::default(),
            artist: ArtistPageState::default(),
            playlist_list,
            track_table,
            search_track_table,
            search_card_grid,
            search_input,
            settings_inputs,
            settings_scroll_handle: ScrollHandle::new(),
            progress,
            player_controls,
            _player_subscription: None,
            image_cache,
            window_drag_pressed: false,
            playback: Playback::new(audio, playback_quality, pending_playback_restore),
            audio_cache,
            protocol_client,
            cdn_maintenance: None,
            audio_cache_maintenance: None,
            quality_menu_open: false,
            cover_backdrop_expanded: false,
            cover_backdrop_visible: false,
            cover_backdrop_fully_expanded: false,
            lyrics: LyricsCache::new(lyric_disk_cache),
            lyrics_view,
            _lyrics_subscription: None,
            preferences,
            library_cache,
            page_resource_cache: PageResourceCache::default(),
            liked_tracks: HashMap::new(),
            liked_state_loading: HashSet::new(),
            liked_toggle_loading: HashSet::new(),
            update_check_in_progress: false,
            window_handle: window.window_handle(),
            _subscriptions: subscriptions,
            _window_subscription: None,
            _appearance_subscription: None,
            background_tick_wake: None,
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
        view.account
            .update(cx, |account, cx| account.restore_credential(cx));
        if view.preferences.read(cx).settings.check_updates_on_startup {
            view.schedule_update_check(cx);
        }
        // release 仍可读取账号实体；Rust Drop 阶段已没有 GPUI App 可供生成最后的快照。
        cx.on_release(|this, cx| this.shutdown(cx)).detach();
        cx.on_app_quit(|this, cx| {
            // GPUI 只等待退出 future 200ms，macOS 终止后也不保证返回 main。
            // 在回调返回前排空写入；后台任务不访问 GPUI，退出时同步等待不会形成互相等待。
            this.shutdown(cx);
            async {}
        })
        .detach();
        view
    }

    fn listen_for_load_more(
        mut receiver: mpsc::Receiver<()>,
        load_more: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while receiver.recv().await.is_some() {
                // 控件通道关闭或应用实体销毁后结束监听；窗口关闭本身不会结束实体的寿命。
                if this.update(cx, load_more).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.set_window_title("Lyrune");
        self.window_handle = window.window_handle();
        Theme::global_mut(cx).notification.margins.bottom = px(PLAYER_BAR_HEIGHT + 16.);
        self.lyrics_view
            .update(cx, |view, cx| view.attach_window(cx));
        self._lyrics_subscription = Some(cx.subscribe_in(
            &self.lyrics_view,
            window,
            |this, _, _: &CollapseLyrics, window, cx| {
                this.set_cover_backdrop_expanded(false, window, cx);
            },
        ));
        window.set_inactive_frame_interval(self.inactive_window_frame_interval(cx));
        self._appearance_subscription =
            Some(cx.observe_window_appearance(window, |this, window, cx| {
                this.preferences
                    .update(cx, |preferences, cx| preferences.apply_theme(window, cx))
            }));
        self._window_subscription = Some(cx.observe_window_bounds(window, |this, window, cx| {
            let size = window.window_bounds().get_bounds().size;
            let width = f32::from(size.width).round() as u32;
            let height = f32::from(size.height).round() as u32;
            if width > 0 && height > 0 {
                {
                    let value = Some(PersistedWindowSize { width, height });
                    this.preferences.update(cx, |preferences, _| {
                        preferences.settings.window_size = value;
                    });
                }
            }
            cx.notify();
        }));
        self._player_subscription = Some(cx.subscribe_in(
            &self.player_controls,
            window,
            |this, _, action: &PlayerAction, window, cx| {
                this.handle_player_action(action, window, cx)
            },
        ));
        self.progress
            .update(cx, |view, cx| view.attach_window(window, cx));
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
                .read_with(cx, |this, cx| {
                    this.preferences.read(cx).settings.check_updates_on_startup
                        && !this.update_check_in_progress
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

    pub(crate) fn window_size(&self, cx: &App) -> Option<PersistedWindowSize> {
        self.preferences.read(cx).settings.window_size
    }

    pub(crate) fn window_decoration(&self, cx: &App) -> WindowDecoration {
        self.preferences.read(cx).settings.window_decoration
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

    fn shutdown(&mut self, cx: &mut App) {
        if self.preferences.read(cx).writer.is_some() {
            self.persist_current_playback(cx);
        }
        if let Some(audio) = self.playback.audio.take() {
            audio.stop();
        }
        self.preferences
            .update(cx, |preferences, _| preferences.finish());
        if let Some(task) = self.cdn_maintenance.take() {
            task.abort();
        }
        if let Some(task) = self.audio_cache_maintenance.take() {
            task.abort();
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
            .child(lyrune_icon(self.preferences.read(cx).color_theme, px(42.)))
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

    pub(super) fn inactive_window_frame_interval(&self, cx: &App) -> Option<Duration> {
        let lyrics = if self.cover_backdrop_visible {
            combined_lyric_frame_interval(
                self.preferences
                    .read(cx)
                    .settings
                    .lyric_highlight_frame_rate,
                self.preferences.read(cx).settings.lyric_scroll_frame_rate,
            )
        } else {
            Some(crate::INACTIVE_WINDOW_FRAME_INTERVAL)
        };
        match (
            lyrics,
            self.preferences
                .read(cx)
                .settings
                .progress_frame_rate
                .frame_interval(),
        ) {
            (Some(lyrics), Some(progress)) => Some(lyrics.min(progress)),
            _ => None,
        }
    }

    pub(super) fn set_cover_backdrop_expanded(
        &mut self,
        expanded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cover_backdrop_expanded = expanded;
        if !expanded {
            self.cover_backdrop_fully_expanded = false;
        }
        if expanded && !self.cover_backdrop_visible {
            self.cover_backdrop_visible = true;
            self.lyrics_view
                .update(cx, |view, _| view.reset_lyric_animation_frames());
        }
        window.set_inactive_frame_interval(self.inactive_window_frame_interval(cx));
        self.wake_playback_tick();
        cx.notify();
    }

    fn lyrics_expansion_progress(&mut self, window: &mut Window, cx: &mut Context<Self>) -> f32 {
        // 裁剪与 CSD 图标取同一条动画；子视图中的同名 transition 会形成独立状态。
        let progress = transition(
            ("cover-backdrop", "expansion-progress"),
            if self.cover_backdrop_expanded { 1. } else { 0. },
            Transition::new(LYRIC_EXPANSION_DURATION).ease(|progress| 1. - (1. - progress).powi(4)),
            window,
            cx,
        );
        if self.cover_backdrop_expanded && progress >= 1. {
            self.cover_backdrop_fully_expanded = true;
        }
        let visible = self.cover_backdrop_expanded || progress > 0.;
        if self.cover_backdrop_visible != visible {
            self.cover_backdrop_visible = visible;
            window.set_inactive_frame_interval(self.inactive_window_frame_interval(cx));
        }
        progress
    }

    fn sync_lyrics_view(&mut self, expansion_progress: f32, cx: &mut Context<Self>) {
        let track = self.playback.track();
        let mid = track.map(|track| track.mid.clone()).unwrap_or_default();
        let content = if let Some(lyrics) = self.lyrics.adopted.get(&mid) {
            LyricsContent::Ready(lyrics.parsed.clone())
        } else if self.lyrics.loading.contains(&mid) {
            LyricsContent::Loading
        } else if let Some(error) = self.lyrics.errors.get(&mid) {
            LyricsContent::Error(error.clone())
        } else {
            LyricsContent::Unloaded
        };
        let input = LyricsInput {
            mid,
            cover_url: track.and_then(|track| track.cover_url.clone()),
            duration: self.playback.duration(),
            content,
            font: self.preferences.read(cx).fonts.lyrics.clone(),
            font_scale: self.preferences.read(cx).settings.lyric_font_scale,
            line_spacing: self.preferences.read(cx).settings.lyric_line_spacing,
            highlight_frame_rate: self
                .preferences
                .read(cx)
                .settings
                .lyric_highlight_frame_rate,
            scroll_frame_rate: self.preferences.read(cx).settings.lyric_scroll_frame_rate,
            advancing: self.playback.is_advancing(),
            seeking: self.progress.read(cx).seek_preview.is_some(),
            expanded: self.cover_backdrop_expanded,
            visible: self.cover_backdrop_visible,
            expansion_progress,
        };
        self.lyrics_view.update(cx, |view, cx| {
            view.set_input(input, self.playback.position, cx)
        });
    }

    fn render_lyrics_layer(&self, expansion_progress: f32, window: &Window) -> AnyElement {
        if self
            .playback
            .track()
            .and_then(|track| track.cover_url.as_ref())
            .is_none()
        {
            return div().into_any_element();
        }
        let available_height = (window.viewport_size().height - px(PLAYER_BAR_HEIGHT)).max(px(0.));
        // 只有已展开的裁剪区域遮挡输入，透明的未展开区域仍交给普通页面。
        div()
            .id("cover-backdrop")
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(PLAYER_BAR_HEIGHT))
            .h(available_height)
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(available_height * expansion_progress)
                    .overflow_hidden()
                    .occlude()
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(available_height)
                            .child(self.lyrics_view.clone()),
                    ),
            )
            .into_any_element()
    }

    fn foreground_target(
        &self,
        overlay: Hsla,
        preferred: Hsla,
        sample: impl Fn(&crate::http::BlurredCover) -> [f32; 3],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Hsla> {
        self.lyrics_view.update(cx, |view, cx| {
            view.foreground_target(overlay, preferred, sample, window, cx)
        })
    }

    fn render_window_controls(
        &self,
        expansion_progress: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let available_height = (window.viewport_size().height - px(PLAYER_BAR_HEIGHT)).max(px(0.));
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

    fn render_main(
        &mut self,
        expansion_progress: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                .font(self.preferences.read(cx).fonts.ui.clone())
                .bg(theme.background)
                .text_color(theme.foreground)
                .child(div().flex_1().min_h_0())
                .child(self.render_lyrics_layer(expansion_progress, window))
                .child(self.render_player_bar(window, cx))
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
                    this.child(
                        deferred(self.render_window_controls(expansion_progress, window, cx))
                            .with_priority(20),
                    )
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
            .preferences
            .read(cx)
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
            .font(self.preferences.read(cx).fonts.ui.clone())
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("library-layout")
                        .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                            if let Some(width) = state.read(cx).sizes().first() {
                                {
                                    let value = Some(f32::from(*width).round() as u32);
                                    this.preferences.update(cx, |preferences, _| {
                                        preferences.settings.sidebar_width = value;
                                    });
                                }
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
            .child(self.render_lyrics_layer(expansion_progress, window))
            .child(self.render_player_bar(window, cx))
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
                this.child(
                    deferred(self.render_window_controls(expansion_progress, window, cx))
                        .with_priority(20),
                )
            })
            .into_any_element()
    }
}

impl Render for LyruneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.cover_backdrop_visible && self.playback.is_advancing() {
            if self.progress.read(cx).seek_preview.is_none() {
                self.playback.position = self
                    .playback
                    .audio
                    .as_deref()
                    .map(AudioPlayer::position)
                    .unwrap_or_default();
            }
        }

        let expansion_progress = self.lyrics_expansion_progress(window, cx);
        self.sync_lyrics_view(expansion_progress, cx);
        let notification_list = Root::read(window, cx).notification.clone();
        let notification_layer = div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(PLAYER_BAR_HEIGHT + 16.))
            .flex()
            .justify_center()
            .child(div().relative().left(px(-42.)).child(notification_list));
        if self.account.read(cx).state == AccountState::SignedIn {
            div()
                .relative()
                .size_full()
                .child(self.render_main(expansion_progress, window, cx))
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
                    this.child(
                        deferred(self.render_window_controls(expansion_progress, window, cx))
                            .with_priority(20),
                    )
                })
                .into_any_element()
        }
    }
}
