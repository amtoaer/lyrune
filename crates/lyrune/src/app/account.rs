//! 扫码登录、凭据恢复与会话失效。清理账号同时使旧账号的异步结果失效。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use gpui::{prelude::*, *};
use gpui_component::{ActiveTheme as _, StyledExt as _, v_flex};
use lyrune_api::{
    CredentialSession, LoginEvent, ProtocolClient, QqCredential, UserProfile, run_qr_login,
};
use tokio::sync::mpsc;

use crate::credentials::CredentialStore;
use crate::icons::lyrune_icon;
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
        self.message = "正在向 QQ 音乐申请二维码…".to_owned();
        Some(generation)
    }

    fn invalidate_login(&mut self) -> u64 {
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
    pub(super) fn new(client: Option<ProtocolClient>) -> Self {
        Self {
            client,
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

    fn handle_login_event(&mut self, event: LoginEvent, cx: &mut Context<Self>) {
        match event {
            LoginEvent::QrReady(png) => {
                self.qr_image = Some(Arc::new(Image::from_bytes(ImageFormat::Png, png)));
                self.message = "请使用 QQ 音乐 App 扫描二维码".to_owned();
            }
            LoginEvent::WaitingScan => self.message = "等待扫码…".to_owned(),
            LoginEvent::WaitingConfirm => {
                self.message = "已扫码，请在手机上确认登录".to_owned();
            }
            LoginEvent::Succeeded(credential) => {
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
                self.state = AccountState::SignedOut;
                self.qr_image = None;
                self.message = "扫码登录失败，点击二维码区域重试".to_owned();
                cx.emit(AccountEvent::Error(format!("扫码登录失败：{error}")));
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
        self.account.update(cx, |account, _| {
            account.message = "使用 QQ 音乐 App 扫码登录".to_owned()
        });
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

    pub(super) fn render_login(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let qr = match &self.account.read(cx).qr_image {
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
                .child(match self.account.read(cx).state {
                    AccountState::Restoring => "正在恢复登录…",
                    AccountState::SigningIn => "正在生成二维码…",
                    _ => "使用 QQ 音乐 App 扫码登录",
                })
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .font(self.preferences.read(cx).fonts.ui.clone())
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
                    .child(lyrune_icon(self.preferences.read(cx).color_theme, px(46.)))
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
                            .when(
                                self.account.read(cx).state == AccountState::SignedOut,
                                |this| {
                                    this.cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.account
                                                .update(cx, |account, cx| account.start_login(cx))
                                        }))
                                },
                            )
                            .child(qr),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_center()
                            .text_color(theme.muted_foreground)
                            .child(self.account.read(cx).message.clone()),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Account, AccountState};
    use lyrune_api::{CredentialSession, QqCredential};

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
