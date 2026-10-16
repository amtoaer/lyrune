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
    charts::ChartsPageState,
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
    pub(super) charts: ChartsPageState,
    pub(super) search: SearchPageState,
    pub(super) artist: ArtistPageState,
    pub(super) playlist_list: Entity<ListState<PlaylistListDelegate>>,
    pub(super) track_table: Entity<TableState<TrackTableDelegate>>,
    pub(super) search_track_table: Entity<TableState<TrackTableDelegate>>,
    pub(super) artist_song_table: Entity<TableState<TrackTableDelegate>>,
    pub(super) search_card_grid: Entity<ListState<SearchCardGridDelegate>>,
    pub(super) artist_album_grid: Entity<ListState<SearchCardGridDelegate>>,
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
        let (artist_album_load_more_sender, artist_album_load_more_receiver) = mpsc::channel(1);
        let (artist_song_load_more_sender, _) = mpsc::channel(1);
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
        let artist_song_table = cx.new(|cx| {
            TableState::new(
                TrackTableDelegate::new(artist_song_load_more_sender, track_event_sender.clone())
                    .without_header(),
                window,
                cx,
            )
            .col_selectable(false)
            .col_movable(false)
            .sortable(false)
        });
        let artist_album_grid = cx.new(|cx| {
            ListState::new(
                SearchCardGridDelegate::new(
                    track_event_sender.clone(),
                    artist_album_load_more_sender,
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
                    PlaybackEvent::PlaylistLoaded(resource)
                        if this
                            .playlist
                            .resource
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, resource)) =>
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
            cx.subscribe(&artist_song_table, |this, _, event: &TableEvent, cx| {
                if let TableEvent::DoubleClickedRow(index) = event {
                    this.select_artist_track(*index, cx);
                }
            }),
        ];
        Self::listen_for_load_more(load_more_receiver, Self::load_playlist_page, cx);
        Self::listen_for_load_more(search_load_more_receiver, Self::load_more_search, cx);
        Self::listen_for_load_more(search_card_load_more_receiver, Self::load_more_search, cx);
        Self::listen_for_load_more(
            artist_album_load_more_receiver,
            |this, cx| {
                if this.main_content == MainContent::ArtistAlbums {
                    this.load_artist_albums(true, cx);
                }
            },
            cx,
        );

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
            charts: ChartsPageState::default(),
            search: SearchPageState::default(),
            artist: ArtistPageState::default(),
            playlist_list,
            track_table,
            search_track_table,
            artist_song_table,
            search_card_grid,
            artist_album_grid,
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
        // 首帧先估算，后续以实际布局宽度为准；侧栏跨尺寸断点时未必采用默认宽度。
        if view.content_width == px(0.) {
            view.content_width = content_width;
            cx.notify();
        }
    });
    // 导航输入的焦点、光标与页体各自刷新；可调整面板只负责它们共有的布局。
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
                            .child(
                                navigation.cached(
                                    StyleRefinement::default()
                                        .w_full()
                                        .h(px(72.))
                                        .flex_shrink_0(),
                                ),
                            )
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
        let track_table = match self.main_content {
            MainContent::Playlist => Some(&self.track_table),
            MainContent::Search => Some(&self.search_track_table),
            MainContent::Artist => Some(&self.artist_song_table),
            _ => None,
        };
        if let Some(track_table) = track_table {
            let available_width = self.content_width - super::page_horizontal_padding(narrow) * 2.;
            track_table.update(cx, |table, cx| {
                if table.delegate_mut().set_width(available_width) {
                    table.refresh(cx);
                    cx.notify();
                }
            });
        }
        let page = match self.main_content {
            MainContent::Home => self.render_home(compact, narrow, scale_factor, cx),
            MainContent::Charts => {
                self.render_charts(compact, narrow, self.content_width, scale_factor, cx)
            }
            MainContent::Search => self.render_search(narrow, self.content_width, window, cx),
            MainContent::Artist => {
                self.render_artist_content(compact, narrow, scale_factor, self.content_width, cx)
            }
            MainContent::ArtistAlbums => {
                self.render_artist_album_page(narrow, self.content_width, window, cx)
            }
            MainContent::Playlist => {
                self.render_playlist_content(compact, narrow, scale_factor, cx)
            }
            MainContent::Settings => self
                .settings_page
                .clone()
                .cached(StyleRefinement::default().flex_1().w_full().min_h_0())
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(page)
            .on_children_prepainted(cx.processor(|this, bounds: Vec<Bounds<Pixels>>, _, cx| {
                let width = bounds[0].size.width;
                if this.content_width != width {
                    this.content_width = width;
                    cx.notify();
                }
            }))
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

    fn new_browser(window: &mut Window, cx: &mut gpui::App) -> Entity<BrowserView> {
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
        let account = cx.new(|_| Account::new(None, String::new()));
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
        let settings = cx.new(|cx| SettingsPage::new(preferences.clone(), window, cx));
        cx.new(|cx| BrowserView::new(window, account, preferences, playback, settings, cx))
    }

    #[gpui::test]
    fn test_browser_cache_and_reopen(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (host, cx) = cx.add_window_view(|window, cx| {
            let browser = new_browser(window, cx);
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
        let navigation = browser.read_with(cx, |browser, _| browser.navigation_view.clone());
        let navigation_before = navigation.read_with(cx, |view, _| view.render_count);
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
        assert_eq!(
            navigation.read_with(cx, |view, _| view.render_count),
            navigation_before
        );

        let input = browser.read_with(cx, |browser, _| browser.search_input.clone());
        cx.update(|window, cx| {
            input.update(cx, |input, cx| input.set_value("draft", window, cx));
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(navigation.read_with(cx, |view, _| view.render_count) > navigation_before);
        assert_eq!(browser.read_with(cx, |view, _| view.render_count), before);
        let bounds = input.read_with(cx, |input, _| input.input_bounds());
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        cx.simulate_keystrokes("ctrl-a");
        cx.simulate_input("typed");
        assert_eq!(input.read_with(cx, |input, _| input.value()), "typed");

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

    #[gpui::test]
    fn test_chart_navigation_and_periods(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use gpui::{Modifiers, size};
        use lyrune_api::{Chart, ChartGroup};

        use crate::app::navigation::{PlaylistCachePolicy, lock_resource};
        use crate::app::playback::PlaybackEvent;

        cx.update(gpui_component::init);
        let (host, cx) = cx.add_window_view(|window, cx| TestBrowser {
            browser: new_browser(window, cx),
            reveal: 0.,
            hidden: false,
        });
        let browser = host.read_with(cx, |host, _| host.browser.clone());
        let chart = |id| {
            serde_json::from_value::<Chart>(serde_json::json!({
                "topId": id, "title": format!("榜单 {id}"), "period": "2026_42",
                "frontPicUrl": "", "updateTips": "每周更新", "totalNum": 1,
                "song": [{"title": "歌曲", "singerName": "歌手"}],
                "history": {"year": [2026, 2025], "subPeriod": [[42, 41], [52, 50]]}
            }))
            .unwrap()
        };
        let weekly = chart(5);
        let resources = browser.update(cx, |browser, cx| {
            browser.charts.groups = Arc::new(vec![
                ChartGroup { group_id: 0, group_name: "巅峰榜".into(), toplist: vec![chart(62), chart(26), chart(27), chart(4)] },
                ChartGroup { group_id: 1, group_name: "地区榜".into(), toplist: vec![weekly.clone()] },
            ]);
            let resources = ["2026_42", "2025_50"].map(|period| {
                let mut playlist = weekly.playlist(period.into());
                playlist.cover_url = None;
                let resource = browser.page_resource_cache.playlist(0, playlist.clone(), false, PlaylistCachePolicy::Fresh);
                let track = serde_json::from_value(serde_json::json!({
                    "mid": period, "title": period, "artists": "歌手", "album": "专辑", "album_mid": "", "duration_seconds": 180
                })).unwrap();
                lock_resource(&resource).apply_page(playlist, vec![Arc::new(track)], false, 1, 0);
                resource
            });
            cx.notify();
            resources
        });
        cx.update(|window, cx| browser.update(cx, |browser, cx| browser.show_charts(window, cx)));
        for width in [850., 1440.] {
            cx.simulate_resize(size(px(width), px(1500.)));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let first = cx.debug_bounds("chart-62").unwrap();
            let second = cx.debug_bounds("chart-26").unwrap();
            let third = cx.debug_bounds("chart-27").unwrap();
            if width < 900. {
                assert!(second.top() >= first.bottom());
            } else {
                assert_eq!(first.top(), second.top());
                assert_eq!(first.left(), third.left());
                assert_eq!(first.size.width, second.size.width);
                assert!(third.top() >= first.bottom());
            }
        }
        let card = cx.debug_bounds("chart-5").unwrap();
        cx.simulate_click(card.center(), Modifiers::none());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        browser.update(cx, |browser, cx| browser.select_track(0, cx));
        let playing_id = weekly.playlist("2026_42".into()).id;
        let picker = cx.debug_bounds("chart-period").unwrap();
        cx.simulate_click(picker.center(), Modifiers::none());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let year = cx.debug_bounds("chart-year-2025").unwrap();
        cx.simulate_click(year.center(), Modifiers::none());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("chart-week-2025_51").is_none());
        let week = cx.debug_bounds("chart-week-2025_50").unwrap();
        cx.simulate_click(week.center(), Modifiers::none());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let playback = browser.read_with(cx, |browser, cx| {
            assert_eq!(
                browser.playlist.selected.as_ref().unwrap().id,
                weekly.playlist("2025_50".into()).id
            );
            assert_eq!(
                browser.playlist.selected.as_ref().unwrap().cover_url,
                Some(weekly.front_pic_url.clone())
            );
            assert_eq!(
                browser.track_table.read(cx).delegate().tracks()[0].mid,
                "2025_50"
            );
            assert_eq!(
                browser
                    .playback
                    .read(cx)
                    .state
                    .queue
                    .as_ref()
                    .unwrap()
                    .playlist_id,
                playing_id
            );
            browser.playback.clone()
        });
        playback.update(cx, |_, cx| {
            cx.emit(PlaybackEvent::PlaylistLoaded(resources[0].clone()))
        });
        cx.run_until_parked();
        browser.read_with(cx, |browser, cx| {
            assert_eq!(
                browser.track_table.read(cx).delegate().tracks()[0].mid,
                "2025_50"
            );
        });
        cx.update(|window, cx| {
            browser.update(cx, |browser, cx| {
                browser.show_home(window, cx);
                browser.navigate_back(window, cx);
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        browser.read_with(cx, |browser, _| {
            assert!(Arc::ptr_eq(
                browser.playlist.resource.as_ref().unwrap(),
                &resources[1]
            ));
        });
        browser.update(cx, |browser, cx| browser.select_track(0, cx));
        browser.read_with(cx, |browser, cx| {
            assert_eq!(
                browser
                    .playback
                    .read(cx)
                    .state
                    .queue
                    .as_ref()
                    .unwrap()
                    .playlist_id,
                weekly.playlist("2025_50".into()).id
            );
        });
        let previous_cover = browser.read_with(cx, |browser, _| {
            browser
                .playlist
                .selected
                .as_ref()
                .unwrap()
                .cover_url
                .clone()
        });
        let picker = cx.debug_bounds("chart-period").unwrap();
        cx.simulate_click(picker.center(), Modifiers::none());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let week = cx.debug_bounds("chart-week-2025_52").unwrap();
        cx.simulate_click(week.center(), Modifiers::none());
        browser.read_with(cx, |browser, _| {
            let playlist = browser.playlist.selected.as_ref().unwrap();
            assert_eq!(playlist.id, weekly.playlist("2025_52".into()).id);
            assert_eq!(playlist.cover_url, previous_cover);
            assert_eq!(
                lock_resource(browser.playlist.resource.as_ref().unwrap()).fetched_at_secs,
                0
            );
        });
        cx.update(|window, cx| browser.update(cx, |browser, cx| browser.navigate_back(window, cx)));
        browser.read_with(cx, |browser, _| {
            assert!(browser.main_content == MainContent::Charts)
        });
    }

    #[gpui::test]
    fn test_chart_period_scroll(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use gpui::{Modifiers, ScrollDelta, ScrollWheelEvent, TouchPhase, point, size};
        use lyrune_api::{Chart, ChartGroup};

        use crate::app::navigation::{PlaylistCachePolicy, PlaylistScrollPosition};

        cx.update(gpui_component::init);
        let (host, cx) = cx.add_window_view(|window, cx| TestBrowser {
            browser: new_browser(window, cx),
            reveal: 0.,
            hidden: false,
        });
        cx.simulate_resize(size(px(1440.), px(1000.)));
        let browser = host.read_with(cx, |host, _| host.browser.clone());
        let chart: Chart = serde_json::from_value(serde_json::json!({
            "topId": 5, "title": "内地榜", "period": "2026_42",
            "frontPicUrl": "", "updateTips": "每周更新", "totalNum": 0, "song": [],
            "history": {
                "year": (2010..=2026).rev().collect::<Vec<_>>(),
                "subPeriod": vec![(1..=52).rev().collect::<Vec<_>>(); 17]
            }
        }))
        .unwrap();
        browser.update(cx, |browser, cx| {
            let mut playlist = chart.playlist("2014_3".into());
            playlist.cover_url = None;
            browser.charts.groups = Arc::new(vec![ChartGroup {
                group_id: 1,
                group_name: "地区榜".into(),
                toplist: vec![chart],
            }]);
            browser.open_playlist(
                playlist,
                None,
                false,
                PlaylistCachePolicy::Fresh,
                PlaylistScrollPosition::top(),
                None,
                cx,
            );
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        for pass in 0..2 {
            let picker = cx.debug_bounds("chart-period").unwrap();
            cx.simulate_click(picker.center(), Modifiers::none());
            cx.update(|window, cx| window.draw(cx).clear(cx));
            for (list, selected) in [
                ("chart-years", "chart-year-2014"),
                ("chart-weeks", "chart-week-2014_3"),
            ] {
                let viewport = cx.debug_bounds(list).unwrap();
                let item = cx.debug_bounds(selected).unwrap();
                assert!(item.top() >= viewport.top());
                assert!(
                    item.bottom() <= viewport.bottom(),
                    "{selected}, pass {pass}: item {item:?}, viewport {viewport:?}"
                );
            }
            if pass == 0 {
                let viewport = cx.debug_bounds("chart-weeks").unwrap();
                cx.simulate_event(ScrollWheelEvent {
                    position: viewport.center(),
                    delta: ScrollDelta::Pixels(point(px(0.), px(2000.))),
                    modifiers: Modifiers::none(),
                    touch_phase: TouchPhase::Moved,
                });
                browser.update(cx, |_, cx| cx.notify());
                cx.update(|window, cx| window.draw(cx).clear(cx));
                let viewport = cx.debug_bounds("chart-weeks").unwrap();
                let latest = cx.debug_bounds("chart-week-2014_52").unwrap();
                assert!(latest.top() >= viewport.top());
                assert!(latest.bottom() <= viewport.bottom());
                cx.simulate_click(picker.center(), Modifiers::none());
                cx.update(|window, cx| window.draw(cx).clear(cx));
            }
        }
    }

    #[gpui::test]
    fn test_artist_album_navigation(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use gpui::{Modifiers, ScrollDelta, ScrollWheelEvent, TouchPhase, point, size};
        use gpui_component::{list::ListDelegate, scroll::Scrollbar, table::TableEvent};
        use lyrune_api::{PlaylistPage, SearchAlbum, SearchArtist, SearchPage};

        use crate::app::navigation::{NavigationPage, PlaylistCachePolicy, lock_resource};

        cx.update(gpui_component::init);
        let (host, cx) = cx.add_window_view(|window, cx| TestBrowser {
            browser: new_browser(window, cx),
            reveal: 0.,
            hidden: false,
        });
        cx.simulate_resize(size(px(1000.), px(1900.)));
        let browser = host.read_with(cx, |host, _| host.browser.clone());
        browser.update(cx, |browser, cx| {
            browser.main_content = MainContent::Search;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let search_header = cx.debug_bounds("search-category-header").unwrap();
        let artist = SearchArtist {
            mid: "artist".into(),
            name: "Artist".into(),
            cover_url: None,
        };
        let albums = (0..80)
            .map(|i| SearchAlbum {
                mid: format!("album-{i}"),
                title: format!("Album {i}"),
                artist: artist.name.clone(),
                cover_url: None,
            })
            .collect::<Vec<_>>();
        let resource = cx.update(|window, cx| {
            browser.update(cx, |browser, cx| {
                let resource = browser.page_resource_cache.artist(0, &artist.mid);
                let mut state = lock_resource(&resource);
                state.apply_songs(Ok(PlaylistPage {
                    playlist: artist.clone().into_playlist(),
                    tracks: (0..20)
                        .map(|i| {
                            serde_json::from_value(serde_json::json!({
                                "mid": format!("song-{i}"),
                                "title": format!("Song {i}"),
                                "artists": "Artist",
                                "album": "Album",
                                "album_mid": "album",
                                "duration_seconds": 180,
                            }))
                            .unwrap()
                        })
                        .collect(),
                    total: 20,
                    has_more: false,
                    next_offset: 20,
                }));
                state.apply_albums(
                    0,
                    Ok(SearchPage {
                        items: albums[..40].to_vec(),
                        has_more: false,
                        next_offset: 40,
                    }),
                );
                drop(state);
                browser.open_search_artist(artist, window, cx);
                resource
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("track-row-4").is_some());
        assert!(cx.debug_bounds("track-row-5").is_none());
        assert_eq!(
            cx.debug_bounds("track-table-header").unwrap().size.height,
            px(0.)
        );
        assert_eq!(cx.debug_bounds("track-row-0").unwrap().size.height, px(64.));
        cx.simulate_resize(size(px(1000.), px(850.)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let row = cx.debug_bounds("track-row-2").unwrap();
        cx.simulate_event(ScrollWheelEvent {
            position: row.center(),
            delta: ScrollDelta::Pixels(point(px(0.), -px(100.))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("track-row-2").unwrap().top() < row.top());
        let preview = browser.read_with(cx, |browser, _| browser.artist_song_table.clone());
        preview.read_with(cx, |table, _| {
            assert_eq!(table.delegate().tracks().len(), 5);
            assert!(!table.delegate().has_more());
            assert_eq!(
                table
                    .vertical_scroll_handle
                    .0
                    .borrow()
                    .base_handle
                    .offset()
                    .y,
                px(0.)
            );
        });
        let row = cx.debug_bounds("track-row-2").unwrap();
        cx.simulate_event(ScrollWheelEvent {
            position: row.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(100.))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.simulate_resize(size(px(1000.), px(1900.)));
        preview.update(cx, |_, cx| cx.emit(TableEvent::DoubleClickedRow(3)));
        cx.run_until_parked();
        browser.read_with(cx, |browser, cx| {
            let playback = browser.playback.read(cx);
            let queue = playback.state.queue.as_ref().unwrap();
            assert_eq!(queue.tracks.len(), 20);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("artist-preview-album-9").is_some());
        assert!(cx.debug_bounds("artist-preview-album-10").is_none());
        for (width, row_end) in [
            (850., "artist-preview-album-2"),
            (1000., "artist-preview-album-3"),
            (1250., "artist-preview-album-4"),
            (1440., "artist-preview-album-5"),
        ] {
            cx.simulate_resize(size(px(width), px(1900.)));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let first = cx.debug_bounds("artist-preview-album-0").unwrap();
            let last = cx.debug_bounds(row_end).unwrap();
            assert_eq!(first.origin.y, last.origin.y);
            let row = cx.debug_bounds("track-row-0").unwrap();
            let duration = cx.debug_bounds("track-duration-0").unwrap();
            assert!(
                (row.right() - duration.right() - px(8.) - Scrollbar::width()).abs() <= px(1.),
                "duration must align with the row at width {width}: {row:?}, {duration:?}"
            );
            preview.read_with(cx, |table, _| {
                assert_eq!(table.horizontal_scroll_handle.max_offset().x, px(0.));
            });
            let view_all = cx.debug_bounds("view-all-artist-albums").unwrap();
            assert!(
                (last.right() - view_all.right() - px(8.)).abs() <= px(1.),
                "view-all button must be inset from the album row at width {width}: {last:?}, {view_all:?}"
            );
            assert_eq!(
                first.size.width,
                cx.debug_bounds("artist-preview-album-9")
                    .unwrap()
                    .size
                    .width
            );
        }
        cx.simulate_resize(size(px(1000.), px(1900.)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let song_resource = browser.update(cx, |browser, _| {
            let playlist = browser.artist.selected.clone().unwrap().into_playlist();
            let songs = lock_resource(&resource)
                .songs
                .as_ref()
                .unwrap()
                .items
                .clone();
            let result = browser.page_resource_cache.playlist(
                0,
                playlist.clone(),
                false,
                PlaylistCachePolicy::Fresh,
            );
            lock_resource(&result).apply_page(playlist, songs, false, 20, 0);
            result
        });
        let view_songs = cx.debug_bounds("view-all-artist-songs").unwrap();
        cx.simulate_click(view_songs.center(), Modifiers::none());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let song_header = cx.debug_bounds("artist-collection-header").unwrap();
        assert_eq!(song_header.top(), search_header.top());
        assert_eq!(song_header.size.height, search_header.size.height);
        let table_header = cx.debug_bounds("track-table-header").unwrap();
        assert_eq!(table_header.size.height, px(36.));
        assert_eq!(
            cx.debug_bounds("track-row-0").unwrap().top(),
            table_header.bottom()
        );
        for selector in [
            "track-table-heading-0",
            "track-table-heading-1",
            "track-table-heading-2",
        ] {
            let heading = cx.debug_bounds(selector).unwrap();
            assert!(
                heading.top() >= table_header.top() && heading.bottom() < table_header.bottom(),
                "heading must fit above the header border: {heading:?}, {table_header:?}"
            );
        }
        browser.read_with(cx, |browser, cx| {
            assert!(browser.main_content == MainContent::Playlist);
            assert!(Arc::ptr_eq(
                browser.playlist.resource.as_ref().unwrap(),
                &song_resource
            ));
            assert_eq!(browser.track_table.read(cx).delegate().tracks().len(), 20);
        });
        let mut previous_title_width = px(0.);
        for width in [1440., 1920.] {
            cx.simulate_resize(size(px(width), px(850.)));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let row = cx.debug_bounds("track-row-0").unwrap();
            let duration = cx.debug_bounds("track-duration-0").unwrap();
            assert!((row.right() - duration.right() - px(8.) - Scrollbar::width()).abs() <= px(1.));
            let table_header = cx.debug_bounds("track-table-header").unwrap();
            let album = cx.debug_bounds("track-table-heading-2").unwrap();
            assert!(
                (album.left() - table_header.left() - table_header.size.width * 0.5 - px(8.)).abs()
                    <= px(1.)
            );
            let table_right = table_header.right();
            assert!(
                duration.right() <= table_right - Scrollbar::width(),
                "duration must not overlap the vertical scrollbar: {duration:?}, {table_right:?}"
            );
            let title_width = cx.debug_bounds("track-title-0").unwrap().size.width;
            assert!(title_width > previous_title_width);
            previous_title_width = title_width;
            browser.read_with(cx, |browser, cx| {
                let table = browser.track_table.read(cx);
                assert_eq!(table.horizontal_scroll_handle.max_offset().x, px(0.));
                let scroll = table.vertical_scroll_handle.0.borrow();
                assert!(scroll.base_handle.max_offset().y > px(0.));
            });
        }
        cx.simulate_resize(size(px(1000.), px(1900.)));
        cx.update(|window, cx| browser.update(cx, |browser, cx| browser.navigate_back(window, cx)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("track-row-4").is_some());
        assert!(cx.debug_bounds("track-row-5").is_none());
        let view_all = cx.debug_bounds("view-all-artist-albums").unwrap();
        cx.simulate_click(view_all.center(), Modifiers::none());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let grid = browser.read_with(cx, |browser, _| {
            assert!(browser.main_content == MainContent::ArtistAlbums);
            browser.artist_album_grid.clone()
        });
        let album_header = cx.debug_bounds("artist-collection-header").unwrap();
        assert_eq!(album_header.top(), search_header.top());
        assert_eq!(album_header.size.height, search_header.size.height);
        for (width, row_end) in [
            (850., "search-album-2"),
            (1000., "search-album-3"),
            (1250., "search-album-4"),
            (1440., "search-album-5"),
        ] {
            cx.simulate_resize(size(px(width), px(1900.)));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let first = cx.debug_bounds("search-album-0").unwrap();
            let last = cx.debug_bounds(row_end).unwrap();
            assert_eq!(first.origin.y, last.origin.y);
            let padding = super::super::page_horizontal_padding(width < 900.);
            assert!((last.right() - (px(width) - padding)).abs() <= px(1.));
        }
        cx.simulate_resize(size(px(1000.), px(1900.)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let rows_before = grid.read_with(cx, |list, cx| list.delegate().items_count(0, cx));
        assert!(rows_before > 0);
        lock_resource(&resource).apply_albums(40, Err(anyhow::anyhow!("offline")));
        browser.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        grid.read_with(cx, |list, cx| {
            assert_eq!(list.delegate().items_count(0, cx), rows_before);
            assert!(!list.delegate().has_more(cx));
        });
        lock_resource(&resource).album_error = None;
        lock_resource(&resource).apply_albums(
            40,
            Ok(SearchPage {
                items: albums[40..].to_vec(),
                has_more: false,
                next_offset: 80,
            }),
        );
        browser.update(cx, |browser, cx| {
            browser.artist.pending_albums_sync = true;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(grid.read_with(cx, |list, cx| list.delegate().items_count(0, cx)) > rows_before);
        grid.update(cx, |list, cx| {
            list.scroll_handle().set_offset(point(px(0.), -px(300.)));
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| {
            browser.update(cx, |browser, cx| {
                let Some(NavigationPage::ArtistAlbums { scroll_offset, .. }) =
                    browser.current_navigation_page(cx)
                else {
                    panic!("artist albums must have their own history entry");
                };
                assert_eq!(scroll_offset, -px(300.));
                browser.open_home_playlist(albums[0].clone().into_playlist(), window, cx);
                browser.navigate_back(window, cx);
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            grid.read_with(cx, |list, _| list.scroll_handle().offset().y),
            -px(300.)
        );
        cx.update(|window, cx| browser.update(cx, |browser, cx| browser.navigate_back(window, cx)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        browser.read_with(cx, |browser, _| {
            assert!(browser.main_content == MainContent::Artist);
            assert!(Arc::ptr_eq(
                browser.artist.resource.as_ref().unwrap(),
                &resource
            ));
        });
        assert!(cx.debug_bounds("artist-preview-album-9").is_some());
        assert!(cx.debug_bounds("artist-preview-album-10").is_none());
        cx.update(|window, cx| {
            browser.update(cx, |browser, cx| browser.navigate_forward(window, cx))
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            grid.read_with(cx, |list, _| list.scroll_handle().offset().y),
            -px(300.)
        );
    }
}
