//! 扫码登录、凭据恢复与会话失效。清理账号同时使旧账号的异步结果失效。

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, avatar::Avatar, h_flex,
    v_flex,
};
use lyrune_api::{
    CredentialSession, LoginEvent, ProtocolClient, QqCredential, QqQuickLogin, UserProfile,
    run_qq_login, run_qr_login, run_wechat_login,
};
use tokio::sync::mpsc;

use crate::credentials::CredentialStore;
use crate::http::cached_image_source;
use crate::icons::{MediaIcon, lyrune_icon, media_icon_hsla};
use crate::runtime::RUNTIME;

use super::LyruneView;
use super::navigation::{MainContent, PageResourceCache};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AccountState {
    Restoring,
    SignedOut,
    SigningIn,
    SignedIn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoginMethod {
    MusicApp,
    Wechat,
    Qq,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QqLoginMode {
    Quick,
    Qr,
}

// 实体随应用存活；关闭窗口不会中断凭据监听或把登录任务交给页面视图。
pub(super) struct Account {
    pub(super) state: AccountState,
    pub(super) credential: Option<CredentialSession>,
    pub(super) profile: Option<UserProfile>,
    qr_image: Option<Arc<Image>>,
    // 退出和重试时继续递增，不能在清空会话时用 Default 重置。
    generation: u64,
    message: String,
    client: Option<ProtocolClient>,
    client_guid: String,
    login_method: LoginMethod,
    login_task: Option<tokio::task::AbortHandle>,
    qq_login: Option<Arc<QqQuickLogin>>,
    qq_mode: QqLoginMode,
    selected_qq: Option<u64>,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            state: AccountState::Restoring,
            credential: None,
            profile: None,
            qr_image: None,
            generation: 0,
            message: "正在读取已保存的登录状态…".to_owned(),
            client: None,
            client_guid: String::new(),
            login_method: LoginMethod::MusicApp,
            login_task: None,
            qq_login: None,
            qq_mode: QqLoginMode::Quick,
            selected_qq: None,
        }
    }
}

impl Account {
    fn begin_login(&mut self) -> Option<u64> {
        if matches!(
            self.state,
            AccountState::Restoring | AccountState::SigningIn
        ) {
            return None;
        }

        let generation = self.invalidate_login();
        self.state = AccountState::SigningIn;
        self.qr_image = None;
        self.message.clear();
        Some(generation)
    }

    fn invalidate_login(&mut self) -> u64 {
        if let Some(task) = self.login_task.take() {
            task.abort();
        }
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    fn is_current_session(&self, generation: u64, credential: &CredentialSession) -> bool {
        // 账号相同不代表会话相同。这里不检查 snapshot：当前会话被撤销时，
        // 凭据监听仍要处理该会话的通知，才能清理界面并提示重新登录。
        self.generation == generation
            && self
                .credential
                .as_ref()
                .is_some_and(|current| current.ptr_eq(credential))
    }

    fn snapshot(&self) -> Option<Arc<QqCredential>> {
        self.credential.as_ref()?.snapshot()
    }

    pub(super) fn id(&self) -> Option<u64> {
        self.snapshot().map(|credential| credential.music_id)
    }

    fn revoke(&mut self) {
        // 版本失效和音频停止由跨领域退出流程先完成；这里不重置版本或删除钥匙串。
        self.state = AccountState::SignedOut;
        if let Some(credential) = self.credential.take() {
            credential.revoke();
        }
    }
}

pub(super) enum AccountEvent {
    SignedIn,
    Rejected {
        generation: u64,
        credential: CredentialSession,
    },
    Error(String),
}

impl EventEmitter<AccountEvent> for Account {}

impl Account {
    pub(super) fn new(client: Option<ProtocolClient>, client_guid: String) -> Self {
        Self {
            client,
            client_guid,
            ..Self::default()
        }
    }

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
                    this.state = AccountState::SignedIn;
                    this.install_credential_session(CredentialSession::new(credential), cx);
                    cx.emit(AccountEvent::SignedIn);
                    cx.notify();
                }
                Ok(None) => {
                    this.state = AccountState::SignedOut;
                    this.start_login(cx);
                }
                Err(error) => {
                    this.state = AccountState::SignedOut;
                    cx.emit(AccountEvent::Error(format!(
                        "无法恢复登录：{error:#}；正在加载登录二维码…"
                    )));
                    this.start_login(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn start_login(&mut self, cx: &mut Context<Self>) {
        let Some(generation) = self.begin_login() else {
            return;
        };
        if self.login_method == LoginMethod::Qq && self.qq_mode == QqLoginMode::Quick {
            self.qq_login = None;
            self.message = "正在检测本机已登录账号…".to_owned();
            let task = RUNTIME.spawn(QqQuickLogin::discover(self.client_guid.clone()));
            self.login_task = Some(task.abort_handle());
            cx.spawn(async move |this, cx| {
                let Ok(result) = task.await else {
                    return;
                };
                let _ = this.update(cx, |this, cx| {
                    this.complete_qq_discovery(generation, result, cx);
                });
            })
            .detach();
            cx.notify();
            return;
        }
        cx.notify();

        let (sender, mut receiver) = mpsc::unbounded_channel();
        let guid = self.client_guid.clone();
        let method = self.login_method;
        let task = RUNTIME.spawn(async move {
            match method {
                LoginMethod::MusicApp => run_qr_login(sender, guid).await,
                LoginMethod::Wechat => run_wechat_login(sender, guid).await,
                LoginMethod::Qq => run_qq_login(sender, guid).await,
            }
        });
        self.login_task = Some(task.abort_handle());
        cx.spawn(async move |this, cx| {
            while let Some(event) = receiver.recv().await {
                let event = match event {
                    LoginEvent::QrReady(bytes) => match RUNTIME
                        .spawn_blocking(move || normalize_login_qr(&bytes))
                        .await
                        .context("生成登录二维码任务异常退出")
                        .and_then(|result| result)
                    {
                        Ok(bytes) => LoginEvent::QrReady(bytes),
                        Err(error) => {
                            task.abort();
                            LoginEvent::Failed(format!("{error:#}"))
                        }
                    },
                    event => event,
                };
                let completed = matches!(
                    event,
                    LoginEvent::Succeeded(_) | LoginEvent::Expired | LoginEvent::Failed(_)
                );
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
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

    fn complete_qq_discovery(
        &mut self,
        generation: u64,
        result: anyhow::Result<QqQuickLogin>,
        cx: &mut Context<Self>,
    ) {
        if self.generation != generation {
            return;
        }
        self.login_task = None;
        self.state = AccountState::SignedOut;
        match result {
            Ok(login) if !login.accounts().is_empty() => {
                if !login
                    .accounts()
                    .iter()
                    .any(|account| Some(account.uin) == self.selected_qq)
                {
                    self.selected_qq = Some(login.accounts()[0].uin);
                }
                self.message.clear();
                self.qq_login = Some(Arc::new(login));
            }
            _ => {
                self.qq_mode = QqLoginMode::Qr;
                self.start_login(cx);
            }
        }
        cx.notify();
    }

    fn select_login_method(&mut self, method: LoginMethod, cx: &mut Context<Self>) {
        if self.login_method == method || self.state == AccountState::Restoring {
            return;
        }
        self.invalidate_login();
        self.state = AccountState::SignedOut;
        self.login_method = method;
        self.qr_image = None;
        self.message.clear();
        if method == LoginMethod::Qq
            && self.qq_mode == QqLoginMode::Quick
            && self.qq_login.is_some()
        {
            cx.notify();
        } else {
            self.start_login(cx);
        }
    }

    fn select_qq_mode(&mut self, mode: QqLoginMode, cx: &mut Context<Self>) {
        self.invalidate_login();
        self.state = AccountState::SignedOut;
        self.qq_mode = mode;
        self.start_login(cx);
    }

    fn login_with_qq(&mut self, uin: u64, cx: &mut Context<Self>) {
        let Some(login) = self.qq_login.clone() else {
            return;
        };
        let Some(generation) = self.begin_login() else {
            return;
        };
        self.message = "正在授权登录 QQ 音乐…".to_owned();
        let task = RUNTIME.spawn(async move { login.login(uin).await });
        self.login_task = Some(task.abort_handle());
        cx.spawn(async move |this, cx| {
            let Ok(result) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let event = match result {
                    Ok(credential) => LoginEvent::Succeeded(credential),
                    Err(error) => LoginEvent::Failed(format!("{error:#}")),
                };
                this.handle_login_event(event, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn handle_login_event(&mut self, event: LoginEvent, cx: &mut Context<Self>) {
        match event {
            LoginEvent::QrReady(bytes) => {
                self.qr_image = Some(Arc::new(Image::from_bytes(ImageFormat::Png, bytes)));
                self.message.clear();
            }
            LoginEvent::WaitingScan => self.message.clear(),
            LoginEvent::WaitingConfirm => {
                self.message = "已扫码，请在手机上确认登录".to_owned();
            }
            LoginEvent::Succeeded(credential) => {
                self.login_task = None;
                self.qq_login = None;
                self.selected_qq = None;
                self.state = AccountState::SignedIn;
                self.qr_image = None;
                self.install_credential_session(CredentialSession::new(credential), cx);
                cx.emit(AccountEvent::SignedIn);
            }
            LoginEvent::Expired => {
                self.state = AccountState::SignedOut;
                self.qr_image = None;
                self.start_login(cx);
            }
            LoginEvent::Failed(error) => {
                self.login_task = None;
                self.state = AccountState::SignedOut;
                self.qr_image = None;
                self.message =
                    if self.login_method == LoginMethod::Qq && self.qq_mode == QqLoginMode::Quick {
                        "登录失败，请重试或重新检测账号"
                    } else {
                        "扫码登录失败，点击二维码区域重试"
                    }
                    .to_owned();
                cx.emit(AccountEvent::Error(format!("登录失败：{error}")));
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
            let _ = this.update(cx, |_, cx| {
                cx.emit(AccountEvent::Error(format!(
                    "登录成功，但凭据未能保存到系统钥匙串：{error:#}"
                )));
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
        let generation = self.generation;
        let mut updates = credential.subscribe();
        self.credential = Some(credential.clone());
        if let Some(current) = credential.snapshot() {
            self.persist_credential(current.as_ref().clone(), cx);
        }

        cx.spawn(async move |this, cx| {
            while updates.changed().await.is_ok() {
                let keep_watching = this
                    .update(cx, |this, cx| {
                        if !this.is_current_session(generation, &credential) {
                            return false;
                        }
                        let Some(current) = credential.snapshot() else {
                            cx.emit(AccountEvent::Rejected {
                                generation,
                                credential: credential.clone(),
                            });
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
        let Some(client) = self.client.clone() else {
            return;
        };
        let generation = self.generation;
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
                if !this.is_current_session(generation, &credential)
                    || credential.snapshot().is_none()
                {
                    return;
                }
                cx.emit(AccountEvent::Error(format!(
                    "暂时无法验证 QQ 音乐登录凭据：{error:#}"
                )));
                cx.notify();
            });
        })
        .detach();
    }
}

impl LyruneView {
    pub(super) fn handle_account_event(&mut self, event: &AccountEvent, cx: &mut Context<Self>) {
        match event {
            AccountEvent::SignedIn => {
                self.browser.update(cx, |browser, cx| {
                    browser.main_content = MainContent::Home;
                    browser.load_home(cx);
                    browser.load_library(false, cx);
                });
            }
            AccountEvent::Rejected {
                generation,
                credential,
            } => {
                // 事件经过实体边界后再次核对身份，不能清理期间新安装的会话。
                if self
                    .account
                    .read(cx)
                    .is_current_session(*generation, credential)
                {
                    self.handle_credential_rejected(cx);
                }
            }
            AccountEvent::Error(message) => self.notify_error(message.clone(), cx),
        }
    }

    pub(super) fn logout(&mut self, cx: &mut Context<Self>) {
        if let (Some(client), Some(credential)) = (
            self.protocol_client.clone(),
            self.account.read(cx).snapshot(),
        ) {
            drop(RUNTIME.spawn(async move {
                let _ = client.logout(&credential).await;
            }));
        }
        self.clear_account_session(true, cx);
    }

    fn handle_credential_rejected(&mut self, cx: &mut Context<Self>) {
        self.clear_account_session(false, cx);
        let message = "QQ 音乐登录凭据已失效，请重新登录".to_owned();
        self.account.update(cx, |account, cx| {
            account.message = message.clone();
            cx.notify();
        });
        self.notify_error(message, cx);
    }

    fn clear_account_session(&mut self, restart_login: bool, cx: &mut Context<Self>) {
        // 这些计数器阻止晚到结果覆盖新状态，并不取消已发出的网络请求。
        self.account
            .update(cx, |account, _| account.invalidate_login());
        self.browser.update(cx, |browser, cx| {
            browser.library_generation = browser.library_generation.wrapping_add(1);
            browser.home.generation = browser.home.generation.wrapping_add(1);
            browser.charts.generation = browser.charts.generation.wrapping_add(1);
            browser.charts.loading = false;
            cx.notify();
        });
        self.playback
            .update(cx, |playback, _| playback.invalidate_session());
        self.account.update(cx, |account, cx| {
            account.revoke();
            cx.notify();
        });
        self.browser.update(cx, |browser, cx| {
            browser.liked_tracks.clear();
            browser.liked_state_loading.clear();
            browser.liked_toggle_loading.clear();
            cx.notify();
        });
        self.account.update(cx, |account, _| {
            account.profile = None;
            account.qr_image = None;
            account.qq_login = None;
            account.selected_qq = None;
            account.qq_mode = QqLoginMode::Quick;
            account.login_method = LoginMethod::MusicApp;
        });
        self.browser.update(cx, |browser, cx| {
            browser.library_loading = false;
            browser.main_content = MainContent::Home;
            browser.navigation_history.clear();
            browser.home.playlists.clear();
            browser.home.loading = false;
            browser.home.loaded = false;
            browser.home.error = None;
            browser.home.recommendation_loading = None;
            browser.search.resource = None;
            browser.artist.selected = None;
            browser.artist.resource = None;
            browser.playlist.selected_index = None;
            browser.playlist.selected = None;
            browser.playlist.resource = None;
            browser.page_resource_cache = PageResourceCache::default();
            cx.notify();
        });
        self.quality_menu_open = false;
        self.account_menu_open = false;
        self.playback
            .update(cx, |playback, cx| playback.clear_session(cx));
        self.browser.update(cx, |browser, cx| {
            browser.playlist_list.update(cx, |list, cx| {
                list.delegate_mut().clear();
                cx.notify();
            });
            browser.track_table.update(cx, |table, cx| {
                table.delegate_mut().clear();
                table.refresh(cx);
                cx.notify();
            });
            cx.notify();
        });
        self.account
            .update(cx, |account, _| account.message.clear());
        if restart_login {
            self.account
                .update(cx, |account, cx| account.start_login(cx));
        } else {
            cx.notify();
        }

        drop(RUNTIME.spawn(async move {
            let _ = tokio::task::spawn_blocking(CredentialStore::delete).await;
        }));
    }

    pub(super) fn render_login(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let account = self.account.read(cx);
        let busy = account.state == AccountState::SigningIn;
        let quick =
            account.login_method == LoginMethod::Qq && account.qq_mode == QqLoginMode::Quick;
        let login_content = if quick {
            let accounts = account
                .qq_login
                .as_ref()
                .map(|login| login.accounts())
                .unwrap_or_default();
            let single = accounts.len() == 1;
            v_flex()
                .size_full()
                .gap_2()
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("本机账号")
                        .child(
                            Button::new("qq-detect")
                                .label("重新检测")
                                .ghost()
                                .small()
                                .disabled(busy)
                                .child(media_icon_hsla(
                                    MediaIcon::Loading,
                                    theme.muted_foreground,
                                    px(14.),
                                ))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.account
                                        .update(cx, |account, cx| account.start_login(cx));
                                })),
                        ),
                )
                .child(
                    v_flex()
                        .id("qq-accounts")
                        .w_full()
                        .flex_1()
                        .min_h_0()
                        .gap_2()
                        .when(single || accounts.is_empty(), |this| {
                            this.items_center().justify_center()
                        })
                        .when(!single, |this| this.overflow_y_scroll())
                        .when(accounts.is_empty(), |this| {
                            this.text_sm()
                                .text_color(theme.muted_foreground)
                                .child("正在检测本机已登录账号…")
                        })
                        .children(accounts.iter().map(|item| {
                            let uin = item.uin;
                            let selected = account.selected_qq == Some(uin);
                            let size = px(if single { 80. } else { 42. });
                            let avatar = Avatar::new()
                                .name(item.nickname.clone())
                                .with_size(size)
                                .src(cached_image_source(
                                    format!("https://q1.qlogo.cn/g?b=qq&nk={uin}&s=100"),
                                    size,
                                    window.scale_factor(),
                                ));
                            let details = v_flex()
                                .w_full()
                                .min_w_0()
                                .gap_1()
                                .when(single, |this| this.items_center())
                                .child(
                                    div()
                                        .max_w_full()
                                        .truncate()
                                        .font_semibold()
                                        .child(item.nickname.clone()),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("QQ {uin}")),
                                );
                            if single {
                                v_flex()
                                    .w_full()
                                    .gap_3()
                                    .items_center()
                                    .child(avatar)
                                    .child(details)
                                    .into_any_element()
                            } else {
                                Button::new(("qq-account", uin))
                                    .w_full()
                                    .h(px(60.))
                                    .flex_shrink_0()
                                    .px_3()
                                    .disabled(busy)
                                    .when(selected, |this| {
                                        this.bg(theme.accent).border_color(theme.primary)
                                    })
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_3()
                                            .child(avatar)
                                            .child(div().flex_1().min_w_0().child(details))
                                            .child(
                                                div()
                                                    .size(px(20.))
                                                    .flex_shrink_0()
                                                    .rounded_full()
                                                    .border_1()
                                                    .border_color(if selected {
                                                        theme.primary
                                                    } else {
                                                        theme.border
                                                    })
                                                    .when(selected, |this| {
                                                        this.bg(theme.primary)
                                                            .text_color(theme.primary_foreground)
                                                            .text_center()
                                                            .child("✓")
                                                    }),
                                            ),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.account.update(cx, |account, cx| {
                                            account.selected_qq = Some(uin);
                                            cx.notify();
                                        });
                                    }))
                                    .into_any_element()
                            }
                        })),
                )
                .when(
                    !account.message.is_empty() && !accounts.is_empty(),
                    |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_center()
                                .text_color(theme.muted_foreground)
                                .child(account.message.clone()),
                        )
                    },
                )
                .child(
                    Button::new("qq-authorize")
                        .label("登录")
                        .primary()
                        .w_full()
                        .h(px(40.))
                        .loading(busy && !accounts.is_empty())
                        .disabled(busy || account.selected_qq.is_none() || accounts.is_empty())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.account.update(cx, |account, cx| {
                                if let Some(uin) = account.selected_qq {
                                    account.login_with_qq(uin, cx);
                                }
                            });
                        })),
                )
                .child(
                    Button::new("qq-use-qr")
                        .label("使用 QQ 扫码")
                        .ghost()
                        .w_full()
                        .h(px(36.))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.account.update(cx, |account, cx| {
                                account.select_qq_mode(QqLoginMode::Qr, cx)
                            });
                        })),
                )
                .into_any_element()
        } else {
            let qr = match &account.qr_image {
                Some(image) => div()
                    .size(px(208.))
                    .rounded(theme.radius_lg)
                    .overflow_hidden()
                    .bg(white())
                    .child(img(image.clone()).size_full())
                    .into_any_element(),
                None => div()
                    .size(px(208.))
                    .rounded(theme.radius_lg)
                    .bg(theme.muted)
                    .text_color(theme.muted_foreground)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(match account.state {
                        AccountState::Restoring => "正在恢复登录…",
                        AccountState::SigningIn => "正在生成二维码…",
                        _ => "点击重试登录",
                    })
                    .into_any_element(),
            };
            v_flex()
                .size_full()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("login-qr")
                        .flex_shrink_0()
                        .when(account.state == AccountState::SignedOut, |this| {
                            this.cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.account
                                        .update(cx, |account, cx| account.start_login(cx));
                                }))
                        })
                        .child(qr),
                )
                .child(
                    div()
                        .w_full()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_center()
                        .when(!account.message.is_empty(), |this| {
                            this.text_sm().text_color(theme.muted_foreground)
                        })
                        .child(if account.message.is_empty() {
                            match account.login_method {
                                LoginMethod::MusicApp => "使用手机 QQ 音乐扫码",
                                LoginMethod::Qq => "使用手机 QQ 扫码",
                                LoginMethod::Wechat => "使用手机微信扫码",
                            }
                            .to_owned()
                        } else {
                            account.message.clone()
                        }),
                )
                .child(div().w_full().h(px(36.)).flex_shrink_0().when(
                    account.login_method == LoginMethod::Qq,
                    |this| {
                        this.child(
                            Button::new("qq-use-quick")
                                .label("检测本机已登录账号")
                                .ghost()
                                .w_full()
                                .h(px(36.))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.account.update(cx, |account, cx| {
                                        account.select_qq_mode(QqLoginMode::Quick, cx)
                                    });
                                })),
                        )
                    },
                ))
                .into_any_element()
        };

        let alternatives = [
            ("music-app-login", "QQ 音乐", LoginMethod::MusicApp),
            ("qq-login", "QQ", LoginMethod::Qq),
            ("wechat-login", "微信", LoginMethod::Wechat),
        ]
        .into_iter()
        .filter(|(_, _, method)| *method != account.login_method);
        v_flex()
            .size_full()
            .image_cache(self.image_cache.clone())
            .font(self.preferences.read(cx).fonts.ui.clone())
            .items_center()
            .justify_center()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                v_flex()
                    .w(px(400.))
                    .items_center()
                    .gap_5()
                    .p_7()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.group_box)
                    .shadow_lg()
                    .child(
                        v_flex()
                            .items_center()
                            .gap_2()
                            .child(lyrune_icon(self.preferences.read(cx).color_theme, px(44.)))
                            .child(div().mt_2().text_2xl().font_bold().child("登录 Lyrune"))
                            .child(
                                div()
                                    .text_base()
                                    .text_color(theme.foreground.opacity(0.8))
                                    .child(match account.login_method {
                                        LoginMethod::MusicApp => "使用 QQ 音乐登录",
                                        LoginMethod::Qq => "使用 QQ 登录",
                                        LoginMethod::Wechat => "使用微信登录",
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(288.))
                            .flex_shrink_0()
                            .child(login_content),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .justify_center()
                            .gap_2()
                            .pt_3()
                            .border_t_1()
                            .border_color(theme.border)
                            .children(alternatives.enumerate().flat_map(
                                |(index, (id, label, method))| {
                                    let button = Button::new(id)
                                        .label(label)
                                        .ghost()
                                        .small()
                                        .disabled(account.state == AccountState::Restoring)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.account.update(cx, |account, cx| {
                                                account.select_login_method(method, cx)
                                            });
                                        }))
                                        .into_any_element();
                                    (index > 0)
                                        .then(|| {
                                            div()
                                                .text_color(theme.muted_foreground)
                                                .child("·")
                                                .into_any_element()
                                        })
                                        .into_iter()
                                        .chain(std::iter::once(button))
                                },
                            )),
                    ),
            )
            .into_any_element()
    }
}

fn normalize_login_qr(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let image = image::load_from_memory(bytes)
        .context("无法读取登录二维码图像")?
        .to_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(image);
    let grids = prepared.detect_grids();
    let [grid] = grids.as_slice() else {
        anyhow::bail!("登录图像应包含一个二维码，实际检测到 {} 个", grids.len());
    };
    let mut payload = Vec::new();
    grid.decode_to(&mut payload).context("无法解码登录二维码")?;
    let code = qrcode::QrCode::new(payload).context("无法重新生成登录二维码")?;
    let image = code
        .render::<image::Luma<u8>>()
        .quiet_zone(true)
        .min_dimensions(624, 624)
        .build();
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, image::ImageFormat::Png)
        .context("无法编码登录二维码图像")?;
    Ok(output.into_inner())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use gpui::{AppContext as _, TestAppContext};

    use super::{Account, AccountState, LoginMethod, QqLoginMode, normalize_login_qr};
    use lyrune_api::{CredentialSession, QqCredential};

    #[test]
    fn test_normalize_login_qr() -> anyhow::Result<()> {
        let payload = b"https://example.com/login?token=A%2Bb%2Fc%3D&state=AbC123\x00\xff";
        let code = qrcode::QrCode::new(payload)?;
        let mut outputs = Vec::new();
        for (quiet_zone, format) in [
            (false, image::ImageFormat::Png),
            (true, image::ImageFormat::Jpeg),
        ] {
            let original = code
                .render::<image::Luma<u8>>()
                .quiet_zone(quiet_zone)
                .build();
            let mut input = Cursor::new(Vec::new());
            original.write_to(&mut input, format)?;
            outputs.push(normalize_login_qr(input.get_ref())?);
        }
        assert_eq!(outputs[0], outputs[1]);
        let image = image::load_from_memory(&outputs[0])?.to_luma8();
        assert_eq!(image.width(), image.height());
        let margin = image.width() / (code.width() as u32 + 8) * 4;
        for (x, y, pixel) in image.enumerate_pixels() {
            assert!(matches!(pixel[0], 0 | 255));
            if x < margin
                || y < margin
                || x >= image.width() - margin
                || y >= image.height() - margin
            {
                assert_eq!(pixel[0], 255);
            }
        }
        let mut prepared = rqrr::PreparedImage::prepare(image);
        let grids = prepared.detect_grids();
        assert_eq!(grids.len(), 1);
        let mut decoded = Vec::new();
        grids[0].decode_to(&mut decoded)?;
        assert_eq!(decoded, payload);
        assert!(normalize_login_qr(b"invalid image").is_err());
        let mut blank = Cursor::new(Vec::new());
        image::GrayImage::from_pixel(100, 100, image::Luma([255]))
            .write_to(&mut blank, image::ImageFormat::Png)?;
        assert!(normalize_login_qr(blank.get_ref()).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_login_cancellation() {
        let task = tokio::spawn(std::future::pending::<()>());
        let mut account = Account {
            login_task: Some(task.abort_handle()),
            ..Account::default()
        };
        account.invalidate_login();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(account.login_task.is_none());
    }

    #[gpui::test]
    fn test_qq_login_switching(cx: &mut TestAppContext) {
        // 空 GUID 使测试任务在网络请求前退出；仍经过真实的切换、取消和回调路径。
        let account = cx.new(|_| Account::new(None, String::new()));
        account.update(cx, |account, cx| {
            assert_eq!(account.login_method, LoginMethod::MusicApp);
            account.state = AccountState::SignedOut;
            account.select_login_method(LoginMethod::Qq, cx);
            let discovery = account.generation;
            assert_eq!(account.qq_mode, QqLoginMode::Quick);
            account.complete_qq_discovery(discovery, Err(anyhow::anyhow!("no local QQ")), cx);
            assert_eq!(account.qq_mode, QqLoginMode::Qr);
            assert_eq!(account.state, AccountState::SigningIn);

            account.select_qq_mode(QqLoginMode::Quick, cx);
            let discovery = account.generation;
            account.select_qq_mode(QqLoginMode::Qr, cx);
            let scan = account.generation;
            account.complete_qq_discovery(discovery, Err(anyhow::anyhow!("late result")), cx);
            assert_eq!(account.generation, scan);
            assert_eq!(account.qq_mode, QqLoginMode::Qr);

            account.select_login_method(LoginMethod::Wechat, cx);
            account.select_login_method(LoginMethod::Qq, cx);
            assert_eq!(account.qq_mode, QqLoginMode::Qr);
            account.handle_login_event(lyrune_api::LoginEvent::Expired, cx);
            assert_eq!(account.qq_mode, QqLoginMode::Qr);
            assert_eq!(account.state, AccountState::SigningIn);
            account.invalidate_login();
        });
    }

    #[test]
    fn test_login_attempts() {
        let mut account = Account::default();
        assert_eq!(account.begin_login(), None);

        account.state = AccountState::SignedOut;
        let first = account.begin_login().expect("start QR login");
        assert_eq!(account.state, AccountState::SigningIn);
        assert_eq!(account.begin_login(), None);
        assert_eq!(account.generation, first);

        // 失败或过期后可再次发起请求；退出后的请求也不能复用之前的版本。
        account.state = AccountState::SignedOut;
        let retry = account.begin_login().expect("retry QR login");
        assert_ne!(retry, first);
        account.invalidate_login();
        account.revoke();
        let next = account.begin_login().expect("login after logout");
        assert_ne!(next, retry);
    }

    #[test]
    fn test_session_identity() {
        let credential: QqCredential = serde_json::from_value(serde_json::json!({
            "music_id": 123,
            "music_key": "music-key",
            "refresh_token": "refresh-token",
            "refresh_key": "refresh-key",
            "login_type": 2,
            "expires_at": null,
            "client_guid": "client-guid"
        }))
        .expect("credential fixture");
        let first = CredentialSession::new(credential.clone());
        let replacement = CredentialSession::new(credential);
        let mut account = Account {
            state: AccountState::SignedIn,
            credential: Some(first.clone()),
            ..Account::default()
        };
        let generation = account.generation;
        assert!(account.is_current_session(generation, &first));
        assert_eq!(account.id(), Some(123));

        account.credential = Some(replacement.clone());
        assert!(!account.is_current_session(generation, &first));
        assert!(account.is_current_session(generation, &replacement));
        replacement.revoke();
        assert_eq!(account.id(), None);
        assert!(account.is_current_session(generation, &replacement));

        account.invalidate_login();
        assert!(!account.is_current_session(generation, &replacement));
        account.credential = Some(first.clone());
        account.revoke();
        assert_eq!(account.state, AccountState::SignedOut);
        assert!(account.credential.is_none());
        assert!(first.snapshot().is_none());
    }
}
