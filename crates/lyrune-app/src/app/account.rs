//! 扫码登录、凭据恢复与会话失效。清理账号同时使旧账号的异步结果失效。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Sizable as _, StyledExt as _,
    avatar::Avatar,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use qqmusic_api::integration::{CredentialSession, LoginEvent, QqCredential, run_qr_login};
use tokio::sync::mpsc;

use crate::credentials::CredentialStore;
use crate::http::cached_image_source;
use crate::icons::{MediaIcon, lyrune_icon, media_icon_hsla};
use crate::runtime::RUNTIME;

use super::LyruneView;
use super::navigation::{MainContent, PageResourceCache};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AccountState {
    Restoring,
    SignedOut,
    SigningIn,
    SignedIn,
}

impl LyruneView {
    pub(super) fn restore_credential(&mut self, cx: &mut Context<Self>) {
        let task = RUNTIME.spawn(async move {
            tokio::task::spawn_blocking(CredentialStore::load)
                .await
                .context("读取凭据任务异常退出")?
        });

        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| match result {
                Ok(Some(credential)) => {
                    this.account_state = AccountState::SignedIn;
                    this.main_content = MainContent::Home;
                    this.install_credential_session(CredentialSession::new(credential), cx);
                    this.load_home(cx);
                    this.load_library(false, cx);
                }
                Ok(None) => {
                    this.account_state = AccountState::SignedOut;
                    this.begin_login(cx);
                }
                Err(error) => {
                    this.account_state = AccountState::SignedOut;
                    this.notify_error(format!("无法恢复登录：{error:#}；正在加载登录二维码…"), cx);
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
        self.login_message = "正在向 QQ 音乐申请二维码…".to_owned();
        cx.notify();

        let (sender, mut receiver) = mpsc::unbounded_channel();
        drop(RUNTIME.spawn(run_qr_login(sender)));
        cx.spawn(async move |this, cx| {
            while let Some(event) = receiver.recv().await {
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
                self.login_message = "请使用 QQ 音乐 App 扫描二维码".to_owned();
            }
            LoginEvent::WaitingScan => self.login_message = "等待扫码…".to_owned(),
            LoginEvent::WaitingConfirm => {
                self.login_message = "已扫码，请在手机上确认登录".to_owned();
            }
            LoginEvent::Succeeded(credential) => {
                self.account_state = AccountState::SignedIn;
                self.qr_image = None;
                self.main_content = MainContent::Home;
                self.install_credential_session(CredentialSession::new(credential), cx);
                self.load_home(cx);
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
                self.login_message = "扫码登录失败，点击二维码区域重试".to_owned();
                self.notify_error(format!("扫码登录失败：{error}"), cx);
            }
        }
        cx.notify();
    }

    fn persist_credential(&self, credential: QqCredential, cx: &mut Context<Self>) {
        let task = RUNTIME.spawn(async move {
            tokio::task::spawn_blocking(move || CredentialStore::save(&credential))
                .await
                .context("保存凭据任务异常退出")
                .and_then(|result| result)
        });

        cx.spawn(async move |this, cx| {
            let Ok(Err(error)) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.notify_error(
                    format!("登录成功，但凭据未能保存到系统钥匙串：{error:#}"),
                    cx,
                );
                cx.notify();
            });
        })
        .detach();
    }

    fn install_credential_session(
        &mut self,
        credential: CredentialSession,
        cx: &mut Context<Self>,
    ) {
        let generation = self.login_generation;
        let mut updates = credential.subscribe();
        self.credential = Some(credential.clone());
        if let Some(current) = credential.snapshot() {
            self.persist_credential(current.as_ref().clone(), cx);
        }

        cx.spawn(async move |this, cx| {
            while updates.changed().await.is_ok() {
                let keep_watching = this
                    .update(cx, |this, cx| {
                        // 同一账号也可能建立新会话；版本和会话身份都匹配时才能处理凭据刷新。
                        if this.login_generation != generation
                            || this
                                .credential
                                .as_ref()
                                .is_none_or(|current| !current.ptr_eq(&credential))
                        {
                            return false;
                        }
                        let Some(current) = credential.snapshot() else {
                            this.handle_credential_rejected(cx);
                            return false;
                        };
                        this.persist_credential(current.as_ref().clone(), cx);
                        true
                    })
                    .unwrap_or(false);
                if !keep_watching {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn validate_cached_credential(&mut self, cx: &mut Context<Self>) {
        let Some(credential) = self.credential.clone() else {
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            return;
        };
        let generation = self.login_generation;
        let task_credential = credential.clone();
        let task = RUNTIME.spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(30),
                client.validate_credential(&task_credential),
            )
            .await
            .context("QQ 音乐登录凭据验证等待超过 30 秒")
            .and_then(|result| result)
        });

        cx.spawn(async move |this, cx| {
            let Ok(Err(error)) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.login_generation != generation
                    || credential.snapshot().is_none()
                    || this
                        .credential
                        .as_ref()
                        .is_none_or(|current| !current.ptr_eq(&credential))
                {
                    return;
                }
                this.notify_error(format!("暂时无法验证 QQ 音乐登录凭据：{error:#}"), cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn credential_snapshot(&self) -> Option<Arc<QqCredential>> {
        self.credential.as_ref()?.snapshot()
    }

    pub(super) fn account_id(&self) -> Option<u64> {
        self.credential_snapshot()
            .map(|credential| credential.music_id)
    }

    fn logout(&mut self, cx: &mut Context<Self>) {
        if let (Some(client), Some(credential)) =
            (self.protocol_client.clone(), self.credential_snapshot())
        {
            drop(RUNTIME.spawn(async move {
                let _ = client.logout(&credential).await;
            }));
        }
        self.clear_account_session(true, cx);
    }

    fn handle_credential_rejected(&mut self, cx: &mut Context<Self>) {
        self.clear_account_session(false, cx);
        self.login_message = "QQ 音乐登录凭据已失效，请重新登录".to_owned();
        self.notify_error(self.login_message.clone(), cx);
    }

    fn clear_account_session(&mut self, restart_login: bool, cx: &mut Context<Self>) {
        // 这些计数器阻止晚到结果覆盖新状态，并不取消已发出的网络请求。
        self.login_generation = self.login_generation.wrapping_add(1);
        self.library_generation = self.library_generation.wrapping_add(1);
        self.home_generation = self.home_generation.wrapping_add(1);
        self.queue_generation = self.queue_generation.wrapping_add(1);
        self.playback_load.generation = self.playback_load.generation.wrapping_add(1);
        if let Some(audio) = &self.audio {
            audio.stop();
        }
        self.account_state = AccountState::SignedOut;
        if let Some(credential) = self.credential.take() {
            credential.revoke();
        }
        self.liked_tracks.clear();
        self.liked_state_loading.clear();
        self.liked_toggle_loading.clear();
        self.profile = None;
        self.qr_image = None;
        self.library_loading = false;
        self.main_content = MainContent::Home;
        self.navigation_history.clear();
        self.home_playlists.clear();
        self.home_loading = false;
        self.home_loaded = false;
        self.home_error = None;
        self.home_recommendation_loading = None;
        self.search_resource = None;
        self.selected_artist = None;
        self.artist_resource = None;
        self.selected_playlist_index = None;
        self.selected_playlist = None;
        self.selected_playlist_resource = None;
        self.page_resource_cache = PageResourceCache::default();
        self.playback_queue = None;
        self.queue_recommendation_loading = false;
        self.queue_waiting_for_recommendation = false;
        self.current_track = None;
        self.playback_load.track = None;
        self.playback_load.autoplay = false;
        self.playback_started = false;
        self.wake_playback_ticks();
        self.playback_location = None;
        self.active_quality = self.settings.playback_quality;
        self.available_qualities.clear();
        self.quality_menu_open = false;
        self.position = Duration::ZERO;
        self.account_menu_open = false;
        self.clear_persisted_playback();
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        #[cfg(target_os = "linux")]
        self.sync_inhibit();
        self.playlist_list.update(cx, |list, cx| {
            list.delegate_mut().clear();
            cx.notify();
        });
        self.track_table.update(cx, |table, cx| {
            table.delegate_mut().clear();
            table.refresh(cx);
            cx.notify();
        });
        self.login_message = "使用 QQ 音乐 App 扫码登录".to_owned();
        if restart_login {
            self.begin_login(cx);
        } else {
            cx.notify();
        }

        drop(RUNTIME.spawn(async move {
            let _ = tokio::task::spawn_blocking(CredentialStore::delete).await;
        }));
    }

    pub(super) fn render_login(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
            .font(self.fonts.ui.clone())
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
                    .child(lyrune_icon(self.color_theme, px(46.)))
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
                            .child(self.login_message.clone()),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_account(
        &mut self,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_settings(window, cx)
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
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.logout(cx)),
                                            ),
                                    ),
                            ),
                    )
                    .with_priority(10),
                )
            })
            .into_any_element()
    }
}
