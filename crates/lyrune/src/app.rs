//! 主窗口组合与跨域操作顺序。页面、歌词、播放控制和进度分别由独立实体持有。
//! 阅读顺序和跨模块约束见 docs/architecture.md。

mod account;
mod animation;
mod artist;
mod browser;
mod charts;
mod home;
mod lyrics;
mod lyrics_loading;
mod lyrics_view;
mod navigation;
mod navigation_view;
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

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{prelude::*, *};
use gpui_base::motion::{Transition, transition};
use gpui_component::{
    ActiveTheme as _, Theme,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex,
    notification::{Notification, NotificationList},
    v_flex,
};
use lyrune_api::ProtocolClient;

use self::{
    account::{Account, AccountEvent, AccountState},
    browser::{BrowserEvent, BrowserView},
    lyrics_loading::LyricsCache,
    lyrics_view::{
        CollapseLyrics, LYRIC_EXPANSION_DURATION, LyricsContent, LyricsInput, LyricsView,
        combined_lyric_frame_interval,
    },
    playback::{Playback, PlaybackEvent, PlaybackState},
    player_view::{PlayerAction, PlayerControlsView},
    preferences::Preferences,
    progress_view::{PlaybackProgressView, SeekTo},
    settings::{SettingsAction, SettingsPage},
};
use crate::cache::{AudioCache, audio_cache_limit_bytes};
use crate::design::AppFonts;
use crate::http::CachedImageCache;
use crate::icons::{MediaIcon, media_icon_hsla};
use crate::lyrics_cache::LyricDiskCache;
use crate::player::AudioPlayer;
use crate::runtime::RUNTIME;
use crate::settings::{AppSettings, CdnCacheStore, PersistedWindowSize, WindowDecoration};
use crate::singleflight::SingleFlight;
use crate::update::AvailableUpdate;

const PAGE_SIZE: u64 = 100;
const ARTIST_PAGE_SIZE: u64 = 5;
const ARTIST_ALBUM_PAGE_SIZE: u64 = 10;
const SEARCH_PAGE_SIZE: usize = 20;
const LIBRARY_CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const PLAYER_BAR_HEIGHT: f32 = 112.;

fn page_horizontal_padding(narrow: bool) -> Pixels {
    if narrow { px(20.) } else { px(24.) }
}

fn home_content_width(compact: bool, narrow: bool) -> Pixels {
    if narrow {
        px(640.)
    } else if compact {
        px(704.)
    } else {
        px(784.)
    }
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
    account: Entity<Account>,
    account_menu_open: bool,
    browser: Entity<BrowserView>,

    settings_page: Entity<SettingsPage>,
    progress: Entity<PlaybackProgressView>,
    player_controls: Entity<PlayerControlsView>,
    image_cache: Entity<CachedImageCache>,
    notifications: Entity<NotificationList>,

    pub(crate) playback: Entity<Playback>,
    protocol_client: Option<ProtocolClient>,
    quality_menu_open: bool,
    // expanded 是用户目标，visible 包含过渡动画，fully_expanded 用于跳过被遮挡的主页面渲染。
    cover_backdrop_expanded: bool,
    cover_backdrop_visible: bool,
    cover_backdrop_fully_expanded: bool,
    lyrics: Entity<LyricsCache>,
    lyrics_view: Entity<LyricsView>,
    preferences: Entity<Preferences>,
    update_check_in_progress: bool,

    window_handle: AnyWindowHandle,
    _subscriptions: Vec<Subscription>,
    // 窗口关闭不销毁应用实体；所有 subscribe_in 订阅都在重开时重新绑定。
    _window_subscriptions: Vec<Subscription>,
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
        let account =
            cx.new(|_| Account::new(protocol_client.clone(), settings.client_guid.clone()));

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
        let preferences = cx.new(|cx| Preferences::new(settings, fonts, cx));
        let settings_page = cx.new(|cx| SettingsPage::new(preferences.clone(), window, cx));
        let lyrics = cx.new(|_| LyricsCache::new(lyric_disk_cache));
        let playlist_page_requests = SingleFlight::default();
        let playback = cx.new(|cx| {
            Playback::new(
                PlaybackState::new(audio, &preferences.read(cx).settings),
                account.clone(),
                preferences.clone(),
                lyrics.clone(),
                audio_cache.clone(),
                protocol_client.clone(),
                playlist_page_requests.clone(),
            )
        });
        let browser = cx.new(|cx| {
            BrowserView::new(
                window,
                account.clone(),
                preferences.clone(),
                playback.clone(),
                settings_page.clone(),
                cx,
            )
        });
        let subscriptions = vec![
            cx.observe(&browser, |_, _, cx| cx.notify()),
            cx.subscribe(&browser, |this, _, event: &BrowserEvent, cx| {
                match event {
                    BrowserEvent::ToggleAccountMenu => {
                        this.account_menu_open = !this.account_menu_open;
                        this.quality_menu_open = false;
                    }
                    BrowserEvent::CloseAccountMenu => this.account_menu_open = false,
                    BrowserEvent::Logout => this.logout(cx),
                    BrowserEvent::Error(message) => this.notify_error(message.clone(), cx),
                }
                cx.notify();
            }),
            cx.observe(&playback, |_, _, cx| cx.notify()),
            cx.observe(&lyrics, |_, _, cx| cx.notify()),
            cx.subscribe(&playback, |this, _, event: &PlaybackEvent, cx| {
                this.handle_playback_event(event, cx)
            }),
            cx.observe(&preferences, |_, _, cx| cx.notify()),
            cx.observe(&account, |_, _, cx| cx.notify()),
            cx.subscribe(&account, |this, _, event: &AccountEvent, cx| {
                this.handle_account_event(event, cx)
            }),
            cx.subscribe(&progress, |this, _, event: &SeekTo, cx| {
                this.playback
                    .update(cx, |playback, cx| playback.seek_to(event.0, cx));
            }),
        ];

        let mut view = Self {
            account,
            account_menu_open: false,
            browser,
            settings_page,
            progress,
            player_controls,
            image_cache,
            notifications: cx.new(|cx| NotificationList::new(window, cx)),
            playback,
            protocol_client,
            quality_menu_open: false,
            cover_backdrop_expanded: false,
            cover_backdrop_visible: false,
            cover_backdrop_fully_expanded: false,
            lyrics,
            lyrics_view,
            preferences,
            update_check_in_progress: false,
            window_handle: window.window_handle(),
            _subscriptions: subscriptions,
            _window_subscriptions: Vec::new(),
        };
        view.attach_window(window, cx);
        if !initial_errors.is_empty() {
            view.notify_error(initial_errors.join("；"), cx);
        }
        view.playback.update(cx, |playback, _| {
            playback.start_audio_cache_maintenance();
            playback.start_cdn_maintenance();
        });
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

    pub(crate) fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.set_window_title("Lyrune");
        if self.window_handle != window.window_handle() {
            self.notifications = cx.new(|cx| NotificationList::new(window, cx));
        }
        self.window_handle = window.window_handle();
        self.browser
            .update(cx, |browser, cx| browser.attach_window(window, cx));
        Theme::global_mut(cx).notification.margins.bottom = px(PLAYER_BAR_HEIGHT + 16.);
        self.lyrics_view
            .update(cx, |view, cx| view.attach_window(cx));
        window.set_inactive_frame_interval(self.inactive_window_frame_interval(cx));
        self.settings_page
            .update(cx, |page, cx| page.attach_window(window, cx));
        self._window_subscriptions = vec![
            cx.subscribe_in(
                &self.lyrics_view,
                window,
                |this, _, _: &CollapseLyrics, window, cx| {
                    this.set_cover_backdrop_expanded(false, window, cx);
                },
            ),
            cx.observe_window_appearance(window, |this, window, cx| {
                this.preferences
                    .update(cx, |preferences, cx| preferences.apply_theme(window, cx))
            }),
            cx.observe_window_bounds(window, |this, window, cx| {
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
            }),
            cx.subscribe_in(
                &self.player_controls,
                window,
                |this, _, action: &PlayerAction, window, cx| {
                    this.handle_player_action(action, window, cx)
                },
            ),
            cx.subscribe_in(
                &self.settings_page,
                window,
                |this, _, action: &SettingsAction, window, cx| {
                    this.handle_settings_action(action, window, cx)
                },
            ),
        ];
        self.progress
            .update(cx, |view, cx| view.attach_window(window, cx));
    }

    fn handle_playback_event(&mut self, event: &PlaybackEvent, cx: &mut Context<Self>) {
        match event {
            PlaybackEvent::TrackStarted {
                mid,
                duration,
                position,
                same_track,
            } => {
                if !same_track {
                    self.lyrics_view.update(cx, |view, cx| {
                        view.start_track(mid, self.cover_backdrop_expanded, cx)
                    });
                }
                self.progress
                    .update(cx, |view, cx| view.start_track(*position, *duration, cx));
            }
            #[cfg(target_os = "linux")]
            PlaybackEvent::Stopped => self.progress.update(cx, |view, cx| view.stop(cx)),
            PlaybackEvent::CloseQualityMenu => self.quality_menu_open = false,
            #[cfg(target_os = "linux")]
            PlaybackEvent::VolumeChanged(volume) => self
                .player_controls
                .update(cx, |view, cx| view.reset_volume(*volume, cx)),
            PlaybackEvent::Error(message) => self.notify_error(message.clone(), cx),
            PlaybackEvent::PlayBrowsedTrack | PlaybackEvent::PlaylistLoaded(_) => {}
        }
        cx.notify();
    }

    fn notify_error(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.notify_status(
            Notification::new().message(message.into()),
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
        self.settings_page.update(cx, |page, cx| {
            page.update_check_in_progress = true;
            cx.notify();
        });
        let task = RUNTIME.spawn(crate::update::check());
        cx.spawn(async move |this, cx| {
            let result = task
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
            this.update(cx, |this, cx| {
                this.update_check_in_progress = false;
                this.settings_page.update(cx, |page, cx| {
                    page.update_check_in_progress = false;
                    cx.notify();
                });
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
        let url = format!(
            "https://github.com/amtoaer/lyrune/releases/tag/v{}",
            update.version
        );
        self.notify_status(
            Notification::new()
                .message(format!("发现 Lyrune 新版本 v{}", update.version))
                .w(px(320.))
                .mx_auto()
                .action(move |_, _, cx| {
                    let url = url.clone();
                    let foreground = cx.theme().info_foreground;
                    Button::new("open-update-release")
                        .label("前往更新")
                        .custom(
                            ButtonCustomVariant::new(cx)
                                .foreground(foreground)
                                .hover(foreground.opacity(0.12))
                                .active(foreground.opacity(0.18)),
                        )
                        .role(Role::Link)
                        .cursor_pointer()
                        .underline()
                        .on_click(move |_, _, cx| cx.open_url(&url))
                })
                .autohide(true),
            cx.theme().info,
            cx.theme().info_foreground,
            cx,
        );
    }

    fn notify_success(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.notify_status(
            Notification::new().message(message.into()),
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
            .px_8()
            .py_3();
        let window_handle = self.window_handle;
        let notifications = self.notifications.clone();
        cx.defer(move |cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                notifications.update(cx, |list, cx| list.push(notification, window, cx));
            });
        });
    }

    pub(crate) fn window_size(&self, cx: &App) -> Option<PersistedWindowSize> {
        self.preferences.read(cx).settings.window_size
    }

    pub(crate) fn window_decoration(&self, cx: &App) -> WindowDecoration {
        self.preferences.read(cx).settings.window_decoration
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
            self.playback
                .update(cx, |playback, cx| playback.persist_current_playback(cx));
        }
        self.playback.update(cx, |playback, _| {
            if let Some(audio) = playback.state.audio.take() {
                audio.stop();
            }
        });
        self.preferences
            .update(cx, |preferences, _| preferences.finish());
        self.playback
            .update(cx, |playback, _| playback.stop_maintenance());
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
        self.playback
            .update(cx, |playback, _| playback.wake_playback_tick());
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

    fn sync_lyrics_view(&mut self, cx: &mut Context<Self>) {
        let track = self.playback.read(cx).state.track();
        let mid = track.map(|track| track.mid.clone()).unwrap_or_default();
        let content = if let Some(lyrics) = self.lyrics.read(cx).adopted.get(&mid) {
            LyricsContent::Ready(lyrics.parsed.clone())
        } else if self.lyrics.read(cx).loading.contains(&mid) {
            LyricsContent::Loading
        } else if let Some(error) = self.lyrics.read(cx).errors.get(&mid) {
            LyricsContent::Error(error.clone())
        } else {
            LyricsContent::Unloaded
        };
        let input = LyricsInput {
            mid,
            cover_url: track.and_then(|track| track.cover_url.clone()),
            duration: self.playback.read(cx).state.duration(),
            content,
            font: self.preferences.read(cx).fonts.lyrics.clone(),
            font_scale: self.preferences.read(cx).settings.lyric_font_scale,
            line_spacing: self.preferences.read(cx).settings.lyric_line_spacing,
            background_min_overlay_percent: self
                .preferences
                .read(cx)
                .settings
                .lyric_background_min_overlay_percent,
            background_chroma: self.preferences.read(cx).settings.lyric_background_chroma,
            background_contrast: self.preferences.read(cx).settings.lyric_background_contrast,
            highlight_frame_rate: self
                .preferences
                .read(cx)
                .settings
                .lyric_highlight_frame_rate,
            scroll_frame_rate: self.preferences.read(cx).settings.lyric_scroll_frame_rate,
            advancing: self.playback.read(cx).state.is_advancing(),
            expanded: self.cover_backdrop_expanded,
            visible: self.cover_backdrop_visible,
        };
        self.lyrics_view.update(cx, |view, cx| {
            view.set_input(input, self.playback.read(cx).state.position, cx)
        });
    }

    fn render_lyrics_layer(
        &self,
        expansion_progress: f32,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        if self
            .playback
            .read(cx)
            .state
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
                            // 零高度裁剪仍会执行子视图布局；隐藏时缓存，保留资源预热与状态失效。
                            .child(if self.cover_backdrop_visible {
                                self.lyrics_view.clone().into_any_element()
                            } else {
                                self.lyrics_view
                                    .clone()
                                    .cached(StyleRefinement::default().size_full())
                                    .into_any_element()
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_window_controls(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let lyrics_controls = self.cover_backdrop_expanded;
        let control_hover_background = theme.background.opacity(0.58);
        let control_variant = ButtonCustomVariant::new(cx)
            .foreground(theme.foreground)
            .hover(control_hover_background)
            .active(control_hover_background);
        h_flex()
            .absolute()
            .top(px(18.))
            .right(px(16.))
            .children([
                div()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("window-minimize")
                            .when(lyrics_controls, |button| button.custom(control_variant))
                            .when(!lyrics_controls, |button| button.ghost())
                            .size(px(36.))
                            .p_0()
                            .child(media_icon_hsla(
                                MediaIcon::WindowMinimize,
                                theme.foreground,
                                px(16.),
                            ))
                            .on_click(|_, window, _| window.minimize_window()),
                    ),
                div()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("window-maximize")
                            .when(lyrics_controls, |button| button.custom(control_variant))
                            .when(!lyrics_controls, |button| button.ghost())
                            .size(px(36.))
                            .p_0()
                            .child(media_icon_hsla(
                                if window.is_maximized() {
                                    MediaIcon::WindowRestore
                                } else {
                                    MediaIcon::WindowMaximize
                                },
                                theme.foreground,
                                px(16.),
                            ))
                            .on_click(|_, window, _| window.zoom_window()),
                    ),
                div()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("window-close")
                            .when(lyrics_controls, |button| button.custom(control_variant))
                            .when(!lyrics_controls, |button| button.ghost())
                            .size(px(36.))
                            .p_0()
                            .child(media_icon_hsla(
                                MediaIcon::WindowClose,
                                theme.foreground,
                                px(16.),
                            ))
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            ])
            .into_any_element()
    }

    fn render_main(
        &mut self,
        expansion_progress: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });
        let popover_open = self.account_menu_open || self.quality_menu_open;
        self.browser.update(cx, |browser, cx| {
            if browser.account_menu_open != self.account_menu_open {
                browser.account_menu_open = self.account_menu_open;
                cx.notify();
            }
        });
        v_flex()
            .relative()
            .size_full()
            .image_cache(self.image_cache.clone())
            .font(self.preferences.read(cx).fonts.ui.clone())
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(if self.cover_backdrop_fully_expanded {
                div().flex_1().min_h_0().into_any_element()
            } else {
                browser::browsing_area(&self.browser, window, cx)
            })
            .child(self.render_lyrics_layer(expansion_progress, window, cx))
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
                this.child(deferred(self.render_window_controls(window, cx)).with_priority(20))
            })
            .into_any_element()
    }
}

impl Render for LyruneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.cover_backdrop_visible && self.playback.read(cx).state.is_advancing() {
            self.playback.update(cx, |playback, _| {
                playback.state.position = playback
                    .state
                    .audio
                    .as_deref()
                    .map(AudioPlayer::position)
                    .unwrap_or_default();
            });
        }

        let expansion_progress = self.lyrics_expansion_progress(window, cx);
        self.sync_lyrics_view(cx);
        // 自己持有通知列表，使其继续位于歌词之上、延后绘制的菜单之下。
        let notification_layer = div()
            .absolute()
            .top_0()
            .left(px(-42.))
            .size_full()
            .child(self.notifications.clone());
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
                .child(self.render_login(window, cx))
                .child(notification_layer)
                .when(client_decorations, |this| {
                    this.child(deferred(self.render_window_controls(window, cx)).with_priority(20))
                })
                .into_any_element()
        }
    }
}
