//! 普通页面、导航历史、共享页面资源及其控件；歌词和播放控件是窗口中的独立兄弟视图。

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{prelude::*, *};
use gpui_component::{
    ResizableState, h_resizable,
    input::InputState,
    list::ListState,
    resizable_panel,
    table::{TableEvent, TableState},
    v_flex,
};
use hashbrown::HashMap;
use lyrune_api::{PlaylistPage, ProtocolClient};
use tokio::sync::mpsc;

use super::{
    account::Account,
    artist::ArtistPageState,
    home::HomePageState,
    navigation::{self, MainContent, NavigationHistory, PageResourceCache},
    navigation_view::{
        NavigationAction, NavigationInput, NavigationView, SelectPlaylist, SidebarView,
    },
    playback::{Playback, PlaybackEvent},
    playlists::{PlaylistPageKey, PlaylistPageState},
    preferences::Preferences,
    search::SearchPageState,
    settings::SettingsPage,
};
use crate::{
    library::{PlaylistListDelegate, SearchCardGridDelegate, TrackTableDelegate, TrackTableEvent},
    settings::LibraryCache,
    singleflight::SingleFlight,
};

pub(super) enum BrowserEvent {
    ToggleAccountMenu,
    CloseAccountMenu,
    Logout,
    Error(String),
}

pub(super) struct BrowserView {
    pub(super) library_loading: bool,
    pub(super) playlist: PlaylistPageState,
    pub(super) library_generation: u64,
    pub(super) playlist_page_requests: SingleFlight<PlaylistPageKey, PlaylistPage>,
    pub(super) main_content: MainContent,
    pub(super) navigation_history: NavigationHistory,
    pub(super) home: HomePageState,
    pub(super) search: SearchPageState,
    pub(super) artist: ArtistPageState,
    pub(super) playlist_list: Entity<ListState<PlaylistListDelegate>>,
    pub(super) track_table: Entity<TableState<TrackTableDelegate>>,
    pub(super) search_track_table: Entity<TableState<TrackTableDelegate>>,
    pub(super) search_card_grid: Entity<ListState<SearchCardGridDelegate>>,
    pub(super) search_input: Entity<InputState>,
    pub(super) library_cache: LibraryCache,
    pub(super) page_resource_cache: PageResourceCache,
    pub(super) liked_tracks: HashMap<String, bool>,
    pub(super) liked_state_loading: HashSet<String>,
    pub(super) liked_toggle_loading: HashSet<String>,
    pub(super) account: Entity<Account>,
    pub(super) preferences: Entity<Preferences>,
    pub(super) playback: Entity<Playback>,
    pub(super) protocol_client: Option<ProtocolClient>,
    settings_page: Entity<SettingsPage>,
    navigation_view: Entity<NavigationView>,
    sidebar_view: Entity<SidebarView>,
    content_width: Pixels,
    pub(super) account_menu_open: bool,
    window_handle: AnyWindowHandle,
    _subscriptions: Vec<Subscription>,
    _window_subscriptions: Vec<Subscription>,
    #[cfg(test)]
    render_count: usize,
}

impl EventEmitter<BrowserEvent> for BrowserView {}

impl BrowserView {
    pub(super) fn new(
        window: &mut Window,
        account: Entity<Account>,
        preferences: Entity<Preferences>,
        playback: Entity<Playback>,
        settings_page: Entity<SettingsPage>,
        cx: &mut Context<Self>,
    ) -> Self {
        let history_limit = preferences.read(cx).settings.navigation_history_limit;
        let protocol_client = playback.read(cx).protocol_client.clone();
        let playlist_page_requests = playback.read(cx).playlist_page_requests.clone();
        let playlist_list =
            cx.new(|cx| ListState::new(PlaylistListDelegate::new(), window, cx).searchable(false));
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("想播放什么？")
                .context_menu(false)
        });
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
        let navigation_view =
            cx.new(|cx| NavigationView::new(account.clone(), search_input.clone(), cx));
        let sidebar_view =
            cx.new(|cx| SidebarView::new(preferences.clone(), playlist_list.clone(), cx));
        let subscriptions = vec![
            cx.observe(&preferences, |_, _, cx| cx.notify()),
            cx.observe(&account, |_, _, cx| cx.notify()),
            cx.observe(&playback, |this, _, cx| {
                this.sync_table_playback_state(cx);
                cx.notify();
            }),
            cx.subscribe(
                &playback,
                |this, _, event: &PlaybackEvent, cx| match event {
                    PlaybackEvent::TrackStarted { mid, .. } => {
                        this.ensure_track_like_state(mid.clone(), cx)
                    }
                    PlaybackEvent::PlayBrowsedTrack => {
                        if !this.track_table.read(cx).delegate().tracks().is_empty() {
                            this.select_track(0, cx);
                        }
                    }
                    PlaybackEvent::PlaylistLoaded(resource) => {
                        if this
                            .playlist
                            .resource
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, resource))
                        {
                            let state = navigation::lock_resource(resource);
                            let tracks = state.tracks.clone();
                            let has_more = state.has_more;
                            drop(state);
                            this.track_table.update(cx, |table, cx| {
                                table.delegate_mut().set_tracks(tracks, has_more, false);
                                cx.notify();
                            });
                            this.restore_pending_playlist_scroll(cx);
                            this.sync_table_playback_state(cx);
                        }
                    }
                    _ => {}
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
        ];
        Self::listen_for_load_more(load_more_receiver, Self::load_playlist_page, cx);
        Self::listen_for_load_more(search_load_more_receiver, Self::load_more_search, cx);
        Self::listen_for_load_more(search_card_load_more_receiver, Self::load_more_search, cx);

        cx.spawn(async move |this, cx| {
            while let Some(event) = track_event_receiver.recv().await {
                let Ok(window) = this.read_with(cx, |this, _| this.window_handle) else {
                    break;
                };
                // 监听按应用寿命运行；窗口关闭时忽略残留的 UI 事件，重开后使用新窗口。
                let _ = window.update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| match event {
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
                });
            }
        })
        .detach();

        let mut view = Self {
            account,
            preferences,
            playback,
            protocol_client,
            settings_page,
            navigation_view,
            sidebar_view,
            content_width: px(0.),
            library_loading: false,
            playlist: PlaylistPageState::default(),
            library_generation: 0,
            playlist_page_requests,
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
            library_cache: LibraryCache::default(),
            page_resource_cache: PageResourceCache::default(),
            liked_tracks: HashMap::new(),
            liked_state_loading: HashSet::new(),
            liked_toggle_loading: HashSet::new(),
            account_menu_open: false,
            window_handle: window.window_handle(),
            _subscriptions: subscriptions,
            _window_subscriptions: Vec::new(),
            #[cfg(test)]
            render_count: 0,
        };
        view.attach_window(window, cx);
        view
    }

    pub(super) fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_handle = window.window_handle();
        self._window_subscriptions = vec![
            cx.subscribe_in(
                &self.sidebar_view,
                window,
                |this, _, event: &SelectPlaylist, window, cx| {
                    this.select_playlist(event.0, window, cx)
                },
            ),
            cx.subscribe_in(
                &self.navigation_view,
                window,
                |this, _, event: &NavigationAction, window, cx| match event {
                    NavigationAction::Back => this.navigate_back(window, cx),
                    NavigationAction::Forward => this.navigate_forward(window, cx),
                    NavigationAction::Home => this.show_home(window, cx),
                    NavigationAction::Settings => this.show_settings(window, cx),
                    NavigationAction::SubmitSearch => this.submit_search(window, cx),
                    NavigationAction::ToggleAccount => cx.emit(BrowserEvent::ToggleAccountMenu),
                    NavigationAction::Logout => cx.emit(BrowserEvent::Logout),
                },
            ),
        ];
        cx.notify();
    }

    pub(super) fn notify_error(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        cx.emit(BrowserEvent::Error(message.into()));
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
}

pub(super) fn browsing_area(
    browser: &Entity<BrowserView>,
    window: &Window,
    cx: &mut App,
) -> AnyElement {
    let view = browser.read(cx);
    let preferences = view.preferences.clone();
    let navigation = view.navigation_view.clone();
    let sidebar = view.sidebar_view.clone();
    let input = NavigationInput {
        home_selected: view.main_content == MainContent::Home,
        can_navigate_back: !view.navigation_history.back.is_empty(),
        can_navigate_forward: !view.navigation_history.forward.is_empty(),
        account_menu_open: view.account_menu_open,
    };
    navigation.update(cx, |navigation, cx| {
        if navigation.input != input {
            navigation.input = input;
            cx.notify();
        }
    });
    let compact = window.viewport_size().width < px(1120.);
    let narrow = window.viewport_size().width < px(900.);
    let (default_sidebar_width, min_sidebar_width, max_sidebar_width) = if narrow {
        (216., 196., 240.)
    } else if compact {
        (248., 220., 300.)
    } else {
        (272., 236., 340.)
    };
    let sidebar_width = px(preferences
        .read(cx)
        .settings
        .sidebar_width
        .map(|width| width as f32)
        .unwrap_or(default_sidebar_width)
        .clamp(min_sidebar_width, max_sidebar_width));
    let content_width = window.viewport_size().width - sidebar_width;
    browser.update(cx, |view, cx| {
        if view.content_width != content_width {
            view.content_width = content_width;
            cx.notify();
        }
    });
    // 输入框的 paint 会通知自身；导航区与页面必须是缓存边界外的兄弟，不能缓存共同父级。
    div()
        .flex_1()
        .min_h_0()
        .child(
            h_resizable("library-layout")
                .on_resize(move |state: &Entity<ResizableState>, _, cx| {
                    if let Some(width) = state.read(cx).sizes().first() {
                        let value = Some(f32::from(*width).round() as u32);
                        preferences.update(cx, |preferences, _| {
                            preferences.settings.sidebar_width = value
                        });
                    }
                })
                .child(
                    resizable_panel()
                        .size(sidebar_width)
                        .size_range(px(min_sidebar_width)..px(max_sidebar_width))
                        .flex_none()
                        .child(sidebar.cached(StyleRefinement::default().size_full())),
                )
                .child(
                    resizable_panel().size_range(px(480.)..Pixels::MAX).child(
                        v_flex()
                            .h_full()
                            .min_w_0()
                            .flex_1()
                            .child(navigation)
                            .child(
                                browser
                                    .clone()
                                    .cached(StyleRefinement::default().flex_1().w_full().min_h_0()),
                            ),
                    ),
                ),
        )
        .into_any_element()
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.render_count += 1;
        }
        let compact = window.viewport_size().width < px(1120.);
        let narrow = window.viewport_size().width < px(900.);
        let scale_factor = window.scale_factor();
        self.track_table.update(cx, |table, cx| {
            if table.delegate_mut().set_compact(compact) {
                table.refresh(cx);
            }
        });
        let page = match self.main_content {
            MainContent::Home => self.render_home(compact, narrow, scale_factor, cx),
            MainContent::Search => {
                self.render_search(compact, narrow, scale_factor, self.content_width, cx)
            }
            MainContent::Artist => self.render_artist_content(compact, narrow, scale_factor, cx),
            MainContent::Playlist => {
                self.render_playlist_content(compact, narrow, scale_factor, cx)
            }
            MainContent::Settings => self
                .settings_page
                .clone()
                .cached(StyleRefinement::default().flex_1().w_full().min_h_0())
                .into_any_element(),
        };
        div().size_full().flex().flex_col().child(page)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
        Render, Styled as _, TestAppContext, Window, div, font, px,
    };
    use gpui_component::{input::InputEvent, v_flex};

    use super::{BrowserView, MainContent};
    use crate::app::{
        account::Account,
        lyrics_loading::LyricsCache,
        playback::{Playback, PlaybackState},
        preferences::Preferences,
        settings::SettingsPage,
    };
    use crate::design::{AppFonts, ColorTheme};
    use crate::settings::AppSettings;
    use crate::singleflight::SingleFlight;

    struct TestBrowser {
        browser: Entity<BrowserView>,
        reveal: f32,
        hidden: bool,
    }

    impl Render for TestBrowser {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .relative()
                .size_full()
                .child(if self.hidden {
                    div().flex_1().min_h_0().into_any_element()
                } else {
                    super::browsing_area(&self.browser, window, cx)
                })
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px(112.))
                        .h(px(self.reveal))
                        .occlude(),
                )
                .child(div().h(px(112.)).flex_shrink_0())
        }
    }

    #[gpui::test]
    fn test_browser_cache_and_reopen(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let preferences = cx.new(|_| Preferences {
            settings: AppSettings::default(),
            fonts: AppFonts {
                ui: font(".SystemUIFont"),
                monospace: font(".SystemUIFont"),
                lyrics: font(".SystemUIFont"),
            },
            color_theme: ColorTheme::default(),
            writer: None,
        });
        let account = cx.new(|_| Account::new(None));
        let lyrics = cx.new(|_| LyricsCache::new(None));
        let playback = cx.new(|_| {
            Playback::new(
                PlaybackState::new(None, Default::default(), None),
                account.clone(),
                preferences.clone(),
                lyrics,
                None,
                None,
                SingleFlight::default(),
            )
        });
        let (host, cx) = cx.add_window_view(|window, cx| {
            let settings = cx.new(|cx| SettingsPage::new(preferences.clone(), window, cx));
            let browser =
                cx.new(|cx| BrowserView::new(window, account, preferences, playback, settings, cx));
            TestBrowser {
                browser,
                reveal: 0.,
                hidden: false,
            }
        });
        let browser = host.read_with(cx, |host, _| host.browser.clone());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let before = browser.read_with(cx, |browser, _| browser.render_count);
        assert!(before > 0);
        for step in 1..=30 {
            host.update(cx, |host, cx| {
                host.reveal = step as f32 * 8.;
                cx.notify();
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        assert_eq!(
            browser.read_with(cx, |browser, _| browser.render_count),
            before
        );

        cx.update(|window, cx| browser.update(cx, |browser, cx| browser.show_settings(window, cx)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(browser.read_with(cx, |browser, _| browser.render_count) > before);
        let history = browser.read_with(cx, |browser, _| browser.navigation_history.back.len());
        host.update(cx, |host, cx| {
            host.hidden = true;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        host.update(cx, |host, cx| {
            host.hidden = false;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        browser.read_with(cx, |browser, _| {
            assert!(browser.main_content == MainContent::Settings);
            assert_eq!(browser.navigation_history.back.len(), history);
        });

        cx.update(|window, _| window.remove_window());
        let (_, cx) = cx.cx.add_window_view(|window, cx| {
            browser.update(cx, |browser, cx| browser.attach_window(window, cx));
            TestBrowser {
                browser: browser.clone(),
                reveal: 0.,
                hidden: false,
            }
        });
        let input = browser.read_with(cx, |browser, _| browser.search_input.clone());
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.set_value("reopened", window, cx);
                cx.emit(InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                });
            })
        });
        cx.run_until_parked();
        browser.read_with(cx, |browser, _| {
            assert!(browser.main_content == MainContent::Search);
            assert_eq!(browser.search.query, "reopened");
        });
    }
}
