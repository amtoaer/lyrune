use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use tokio::runtime::{Builder, Runtime};

use crate::cache::AudioCache;
use crate::credentials::CredentialStore;
use crate::player::{AudioPlayer, PreparedPlayback};
use qqmusic_api::integration::{
    LoginEvent, ProtocolClient, QqCredential, Quality, Track, refresh_credential, run_qr_login,
};

static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
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

pub struct LyruneView {
    account_state: AccountState,
    credential: Option<QqCredential>,
    qr_image: Option<Arc<Image>>,
    tracks: Vec<Track>,
    tracks_loading: bool,
    quality: Quality,
    audio: Option<AudioPlayer>,
    audio_cache: Option<AudioCache>,
    current_track: Option<usize>,
    loading_track: Option<usize>,
    status: String,
    login_generation: u64,
    play_generation: u64,
}

impl LyruneView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (audio, mut initial_status) = match AudioPlayer::new() {
            Ok(player) => (Some(player), "正在读取已保存的登录状态…".to_owned()),
            Err(error) => (
                None,
                format!("音频设备初始化失败：{error:#}；仍可继续验证登录与歌单加载"),
            ),
        };
        let audio_cache = match AudioCache::new() {
            Ok(cache) => Some(cache),
            Err(error) => {
                initial_status = format!("{initial_status}；音频缓存初始化失败：{error:#}");
                None
            }
        };

        let mut view = Self {
            account_state: AccountState::Restoring,
            credential: None,
            qr_image: None,
            tracks: Vec::new(),
            tracks_loading: false,
            quality: Quality::High,
            audio,
            audio_cache,
            current_track: None,
            loading_track: None,
            status: initial_status,
            login_generation: 0,
            play_generation: 0,
        };
        view.restore_credential(cx);
        view
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
                    this.status = "已恢复 QQ 音乐登录，正在加载“我喜欢”…".to_owned();
                    this.persist_credential(credential, cx);
                    this.load_liked_tracks(cx);
                }
                Ok(None) => {
                    this.account_state = AccountState::SignedOut;
                    this.status = "尚未登录，请使用 QQ 音乐 App 扫码".to_owned();
                    cx.notify();
                }
                Err(error) => {
                    this.account_state = AccountState::SignedOut;
                    this.status = format!("无法恢复登录：{error:#}；请重新扫码");
                    cx.notify();
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
        self.status = "正在向 QQ 音乐申请二维码…".to_owned();
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
                    if this.login_generation != generation {
                        return;
                    }
                    this.handle_login_event(event, cx);
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
                self.status = "请使用 QQ 音乐 App 扫描二维码".to_owned();
            }
            LoginEvent::WaitingScan => {
                self.status = "等待扫码…".to_owned();
            }
            LoginEvent::WaitingConfirm => {
                self.status = "已扫码，请在手机上确认登录".to_owned();
            }
            LoginEvent::Succeeded(credential) => {
                self.account_state = AccountState::SignedIn;
                self.qr_image = None;
                self.credential = Some(credential.clone());
                self.status = "登录成功，正在加载“我喜欢”…".to_owned();
                self.persist_credential(credential, cx);
                self.load_liked_tracks(cx);
            }
            LoginEvent::Expired => {
                self.account_state = AccountState::SignedOut;
                self.qr_image = None;
                self.status = "二维码已过期，请重新获取".to_owned();
            }
            LoginEvent::Failed(error) => {
                self.account_state = AccountState::SignedOut;
                self.qr_image = None;
                self.status = format!("扫码登录失败：{error}");
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
                this.status =
                    format!("已登录，但凭据未能保存到系统钥匙串：{error:#}；本次运行仍可继续使用");
                cx.notify();
            });
        })
        .detach();
    }

    fn load_liked_tracks(&mut self, cx: &mut Context<Self>) {
        let Some(credential) = self.credential.clone() else {
            return;
        };
        self.tracks_loading = true;
        self.status = "正在加载“我喜欢”…".to_owned();
        cx.notify();

        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let started = Instant::now();
            eprintln!("[lyrune] requesting QQ Music liked tracks");
            let result = async {
                let client = ProtocolClient::new()?;
                tokio::time::timeout(
                    Duration::from_secs(30),
                    client.liked_tracks(&credential, 100),
                )
                .await
                .context("QQ 音乐“我喜欢”请求等待超过 30 秒")?
            }
            .await;
            eprintln!(
                "[lyrune] QQ Music liked tracks finished after {:?}: {}",
                started.elapsed(),
                if result.is_ok() { "ok" } else { "error" }
            );
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.tracks_loading = false;
                match result {
                    Ok(tracks) => {
                        let count = tracks.len();
                        this.tracks = tracks;
                        this.status = if count == 0 {
                            "“我喜欢”中暂时没有歌曲".to_owned()
                        } else {
                            format!("已加载“我喜欢”的前 {count} 首歌曲")
                        };
                    }
                    Err(error) => {
                        this.status = format!("加载“我喜欢”失败：{error:#}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_track(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.current_track == Some(index) && self.loading_track.is_none() {
            let Some(audio) = &self.audio else {
                self.status = "没有可用的音频输出设备".to_owned();
                cx.notify();
                return;
            };
            let is_playing = audio.toggle();
            self.status = if is_playing {
                "继续播放".to_owned()
            } else {
                "已暂停".to_owned()
            };
            cx.notify();
            return;
        }
        self.start_playback(index, Duration::ZERO, cx);
    }

    fn start_playback(&mut self, index: usize, resume_at: Duration, cx: &mut Context<Self>) {
        let Some(credential) = self.credential.clone() else {
            self.status = "请先登录 QQ 音乐".to_owned();
            cx.notify();
            return;
        };
        let Some(track) = self.tracks.get(index).cloned() else {
            return;
        };
        let Some(audio_cache) = self.audio_cache.clone() else {
            self.status = "音频缓存不可用，无法创建播放流".to_owned();
            cx.notify();
            return;
        };
        let Some(audio) = &self.audio else {
            self.status = "没有可用的音频输出设备".to_owned();
            cx.notify();
            return;
        };

        audio.stop();
        self.play_generation = self.play_generation.wrapping_add(1);
        let generation = self.play_generation;
        let quality = self.quality;
        self.current_track = Some(index);
        self.loading_track = Some(index);
        self.status = format!("正在缓冲“{}” · {}…", track.title, quality.label());
        cx.notify();

        let title = track.title.clone();
        let (sender, receiver) = async_channel::bounded(1);
        drop(RUNTIME.spawn(async move {
            let result = async {
                let client = ProtocolClient::new()?;
                let url = client.playback_url(&credential, &track, quality).await?;
                let stream = audio_cache.prepare(&url, &track, quality).await?;
                tokio::task::spawn_blocking(move || PreparedPlayback::new(stream, resume_at))
                    .await
                    .context("音频解码准备任务异常退出")?
            }
            .await;
            let _ = sender.send(result).await;
        }));

        cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.play_generation != generation {
                    return;
                }
                this.loading_track = None;
                match result {
                    Ok(playback) => {
                        let cache_status = playback.cache_status().description();
                        let result = this
                            .audio
                            .as_ref()
                            .context("音频输出设备不可用")
                            .and_then(|audio| audio.replace(playback));
                        match result {
                            Ok(()) => {
                                this.status = format!(
                                    "正在播放“{title}” · {} · {cache_status}",
                                    this.quality.label()
                                );
                            }
                            Err(error) => {
                                this.status = format!("播放失败：{error:#}");
                            }
                        }
                    }
                    Err(error) => {
                        this.status = format!("获取歌曲失败：{error:#}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_quality(&mut self, quality: Quality, cx: &mut Context<Self>) {
        if self.quality == quality {
            return;
        }
        let resume_at = self
            .audio
            .as_ref()
            .filter(|audio| audio.is_playing())
            .map(AudioPlayer::position)
            .unwrap_or_default();
        self.quality = quality;
        if let Some(index) = self.current_track {
            self.start_playback(index, resume_at, cx);
        } else {
            self.status = format!("已选择 {}", quality.label());
            cx.notify();
        }
    }

    fn logout(&mut self, cx: &mut Context<Self>) {
        self.login_generation = self.login_generation.wrapping_add(1);
        self.play_generation = self.play_generation.wrapping_add(1);
        if let Some(audio) = &self.audio {
            audio.stop();
        }
        self.account_state = AccountState::SignedOut;
        self.credential = None;
        self.qr_image = None;
        self.tracks.clear();
        self.current_track = None;
        self.loading_track = None;
        self.status = "已退出登录".to_owned();
        cx.notify();

        drop(RUNTIME.spawn(async move {
            let _ = tokio::task::spawn_blocking(CredentialStore::delete).await;
        }));
    }

    fn account_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let qr = match &self.qr_image {
            Some(image) => img(image.clone())
                .size(px(220.))
                .rounded(px(8.))
                .into_any_element(),
            None => div()
                .size(px(220.))
                .rounded(px(8.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .text_color(theme.muted_foreground)
                .flex()
                .items_center()
                .justify_center()
                .child(match self.account_state {
                    AccountState::SignedIn => "QQ 音乐已登录",
                    AccountState::SigningIn => "正在生成二维码…",
                    AccountState::Restoring => "正在恢复登录…",
                    AccountState::SignedOut => "点击下方按钮扫码登录",
                })
                .into_any_element(),
        };

        let action = if self.account_state == AccountState::SignedIn {
            Button::new("logout")
                .label("退出登录")
                .outline()
                .on_click(cx.listener(|this, _, _, cx| this.logout(cx)))
                .into_any_element()
        } else {
            Button::new("login")
                .label(if self.account_state == AccountState::SigningIn {
                    "等待扫码"
                } else {
                    "QQ 音乐扫码登录"
                })
                .primary()
                .disabled(matches!(
                    self.account_state,
                    AccountState::Restoring | AccountState::SigningIn
                ))
                .on_click(cx.listener(|this, _, _, cx| this.begin_login(cx)))
                .into_any_element()
        };

        v_flex()
            .w(px(280.))
            .h_full()
            .flex_shrink_0()
            .gap_4()
            .p_5()
            .rounded(px(12.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .items_center()
            .child(
                div()
                    .w_full()
                    .text_lg()
                    .font_semibold()
                    .child("QQ 音乐账户"),
            )
            .child(qr)
            .child(action)
            .child(
                div()
                    .w_full()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(self.status.clone()),
            )
            .into_any_element()
    }

    fn tracks_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let quality_buttons = Quality::ALL
            .into_iter()
            .enumerate()
            .map(|(index, quality)| {
                Button::new(("quality", index))
                    .label(quality.label())
                    .small()
                    .outline()
                    .selected(self.quality == quality)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_quality(quality, cx)))
            });

        let rows =
            self.tracks
                .clone()
                .into_iter()
                .enumerate()
                .map(|(index, track)| {
                    let selected = self.current_track == Some(index);
                    let loading = self.loading_track == Some(index);
                    h_flex()
                        .id(("liked-track", index))
                        .w_full()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded(px(8.))
                        .cursor_pointer()
                        .when(selected, |row| row.bg(theme.list_active))
                        .hover(|row| row.bg(theme.list_hover))
                        .on_click(cx.listener(move |this, _, _, cx| this.select_track(index, cx)))
                        .child(div().w(px(28.)).text_color(theme.muted_foreground).child(
                            if loading {
                                "…".to_owned()
                            } else {
                                format!("{:02}", index + 1)
                            },
                        ))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_medium().child(track.title))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{} · {}", track.artists, track.album)),
                                ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(format_duration(track.duration_seconds)),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>();

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .gap_3()
            .p_5()
            .rounded(px(12.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .child(if self.tracks_loading {
                                "我喜欢 · 加载中".to_owned()
                            } else {
                                format!("我喜欢 · {}", self.tracks.len())
                            }),
                    )
                    .child(
                        Button::new("reload-liked")
                            .label("重新加载")
                            .small()
                            .outline()
                            .disabled(
                                self.account_state != AccountState::SignedIn || self.tracks_loading,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.load_liked_tracks(cx))),
                    ),
            )
            .child(h_flex().gap_2().children(quality_buttons))
            .child(
                v_flex()
                    .id("liked-track-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_1()
                    .children(rows),
            )
            .into_any_element()
    }

    fn player_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let track = self
            .current_track
            .and_then(|index| self.tracks.get(index))
            .cloned();
        let is_playing = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
        let button_label = if is_playing { "暂停" } else { "播放" };
        let current_index = self.current_track;
        let has_track = track.is_some();
        let (track_title, track_artists) = track
            .map(|track| (track.title, track.artists))
            .unwrap_or_else(|| {
                (
                    "尚未选择歌曲".to_owned(),
                    "从“我喜欢”中点击一首歌曲开始播放".to_owned(),
                )
            });

        h_flex()
            .w_full()
            .gap_4()
            .p_4()
            .rounded(px(12.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                Button::new("play-pause")
                    .label(button_label)
                    .primary()
                    .disabled(!has_track || self.loading_track.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(index) = current_index {
                            this.select_track(index, cx);
                        }
                    })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_semibold().child(track_title))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(track_artists),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(self.quality.label()),
            )
            .into_any_element()
    }
}

impl Render for LyruneView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .gap_4()
            .p_5()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(div().text_2xl().font_bold().child("Lyrune"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("QQ 音乐可行性原型"),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .gap_4()
                    .child(self.account_panel(cx))
                    .child(self.tracks_panel(cx)),
            )
            .child(self.player_bar(cx))
    }
}

fn format_duration(seconds: u64) -> String {
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}
