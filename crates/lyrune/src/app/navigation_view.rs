//! 导航栏和侧栏拥有交互控件；与页面内容并列，输入框更新不会使整页缓存失效。

use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _,
    avatar::Avatar,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::{List, ListEvent, ListState},
    v_flex,
};

use super::{account::Account, preferences::Preferences};
use crate::{
    http::cached_image_source,
    icons::{MediaIcon, lyrune_icon, media_icon_hsla},
    library::PlaylistListDelegate,
};

#[derive(PartialEq)]
pub(super) struct NavigationInput {
    pub(super) home_selected: bool,
    pub(super) can_navigate_back: bool,
    pub(super) can_navigate_forward: bool,
    pub(super) account_menu_open: bool,
}

pub(super) enum NavigationAction {
    Back,
    Forward,
    Home,
    Settings,
    SubmitSearch,
    ToggleAccount,
    Logout,
}

pub(super) struct NavigationView {
    account: Entity<Account>,
    search_input: Entity<InputState>,
    pub(super) input: NavigationInput,
    _subscriptions: Vec<Subscription>,
    #[cfg(test)]
    pub(super) render_count: usize,
}
impl EventEmitter<NavigationAction> for NavigationView {}
impl NavigationView {
    pub(super) fn new(
        account: Entity<Account>,
        search_input: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&account, |_, _, cx| cx.notify()),
            cx.subscribe(&search_input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    cx.emit(NavigationAction::SubmitSearch);
                }
            }),
        ];
        Self {
            account,
            search_input,
            input: NavigationInput {
                home_selected: true,
                can_navigate_back: false,
                can_navigate_forward: false,
                account_menu_open: false,
            },
            _subscriptions: subscriptions,
            #[cfg(test)]
            render_count: 0,
        }
    }
    pub(super) fn render_account(
        &mut self,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let name = self
            .account
            .read(cx)
            .profile
            .as_ref()
            .map(|profile| profile.nickname.clone())
            .unwrap_or_else(|| "QQ 音乐用户".to_owned());
        let mut avatar = Avatar::new().name(name.clone()).with_size(px(38.));
        if let Some(url) = self
            .account
            .read(cx)
            .profile
            .as_ref()
            .and_then(|profile| profile.avatar_url.clone())
        {
            avatar = avatar.src(cached_image_source(url, px(38.), scale_factor));
        }

        div()
            .relative()
            .child(
                Button::new("account-avatar")
                    .ghost()
                    .rounded(px(999.))
                    .size(px(44.))
                    .p_0()
                    .tooltip(name.clone())
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(NavigationAction::ToggleAccount);
                    }))
                    .child(avatar),
            )
            .when(self.input.account_menu_open, |this| {
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
                                Button::new("open-settings")
                                    .ghost()
                                    .w_full()
                                    .h(px(44.))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .child(media_icon_hsla(
                                                MediaIcon::Settings,
                                                theme.secondary_foreground,
                                                px(18.),
                                            ))
                                            .child("设置"),
                                    )
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(NavigationAction::Settings)
                                    })),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .pt_2()
                                    .child(
                                        Button::new("logout")
                                            .label("退出登录")
                                            .outline()
                                            .w_full()
                                            .h(px(44.))
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(NavigationAction::Logout)
                                            })),
                                    ),
                            ),
                    )
                    .with_priority(10),
                )
            })
            .into_any_element()
    }
}

impl Render for NavigationView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.render_count += 1;
        }
        let theme = cx.theme().clone();
        let compact = window.viewport_size().width < px(1120.);
        let narrow = window.viewport_size().width < px(900.);
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });
        let scale_factor = window.scale_factor();
        let search_width = if narrow {
            px(268.)
        } else if compact {
            px(376.)
        } else {
            px(480.)
        };
        let home_selected = self.input.home_selected;
        let can_navigate_back = self.input.can_navigate_back;
        let can_navigate_forward = self.input.can_navigate_forward;
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
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NavigationAction::Back))),
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
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NavigationAction::Forward))),
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
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NavigationAction::Home))),
            )
            .child(
                div()
                    .id("search-input")
                    .when(client_decorations, |this| {
                        this.flex_1().min_w_0().max_w(search_width)
                    })
                    .on_mouse_down_out(|_, window, cx| window.blur(cx))
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
            })
    }
}

pub(super) struct SelectPlaylist(pub(super) usize);
pub(super) struct SidebarView {
    preferences: Entity<Preferences>,
    playlist_list: Entity<ListState<PlaylistListDelegate>>,
    window_drag_pressed: bool,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<SelectPlaylist> for SidebarView {}
impl SidebarView {
    pub(super) fn new(
        preferences: Entity<Preferences>,
        playlist_list: Entity<ListState<PlaylistListDelegate>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&preferences, |_, _, cx| cx.notify()),
            cx.subscribe(&playlist_list, |_, _, event: &ListEvent, cx| {
                if let ListEvent::Select(index) | ListEvent::Confirm(index) = event {
                    cx.emit(SelectPlaylist(index.row));
                }
            }),
        ];
        Self {
            preferences,
            playlist_list,
            window_drag_pressed: false,
            _subscriptions: subscriptions,
        }
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
}
impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
}
