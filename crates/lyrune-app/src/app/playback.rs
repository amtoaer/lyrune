//! 音源解析、解码准备和播放控制；队列组成在 queue，外部状态同步在 playback_sync。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use gpui::{Context, Window};
use qqmusic_api::integration::{CredentialSession, PlaybackOption, ProtocolClient, Quality, Track};
use tokio::sync::mpsc;

use crate::cache::{AudioCache, PreparedStream};
use crate::player::{AudioPlayer, PreparedPlayback};
use crate::runtime::RUNTIME;
use crate::settings::PersistedQueueContinuation;

#[cfg(target_os = "linux")]
use super::playback_sync::{duration_micros, mpris_track_id};
use super::{LyruneView, RepeatMode, progress_fraction, progress_slider_state};

#[derive(Default)]
pub(super) struct PlaybackLoad {
    generation: u64,
    // 属于播放队列，不是当前页面表格中的行号。
    pub(super) track: Option<usize>,
    // 加载中再次切换音质时沿用原来的播放意图，此时不能从已停止的音频设备推断。
    pub(super) autoplay: bool,
}

impl PlaybackLoad {
    // 只使旧结果失效，不取消任务或清除加载标记。
    // 停止和退出需要先失效，再停止设备，最后清除加载标记。
    pub(super) fn invalidate(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    pub(super) fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }

    pub(super) fn clear(&mut self) {
        // 保留版本，不能用 Default 重置；重新登录后的新请求仍须排除退出前的结果。
        self.track = None;
        self.autoplay = false;
    }
}

#[derive(Clone)]
pub(super) struct PlaybackLocation {
    pub(super) track_mid: String,
    pub(super) quality: Quality,
    pub(super) options: Vec<PlaybackOption>,
}

async fn prepare_playback_option(
    audio_cache: &AudioCache,
    option: &PlaybackOption,
    track: &Track,
) -> anyhow::Result<PreparedStream> {
    let urls = option.urls().map(str::to_owned).collect::<Vec<_>>();
    if option.encrypted {
        let ekey = option.ekey.as_deref().context("QQ 音乐加密音源缺少 ekey")?;
        audio_cache
            .prepare_encrypted(urls, ekey, track, option.quality)
            .await
    } else {
        audio_cache
            .prepare_with_fallbacks(urls, track, option.quality)
            .await
    }
}

enum PlaybackLoadEvent {
    ResolvingOptions,
    Options(Vec<Quality>),
    Finished(anyhow::Result<(PreparedPlayback, PlaybackLocation, Vec<Quality>)>),
}

// 只准备音源，不读取或修改界面状态；返回后仍由 GPUI 接收端检查播放版本。
async fn prepare_playback_stream(
    audio_cache: &AudioCache,
    client: &ProtocolClient,
    credential: &CredentialSession,
    track: &Track,
    desired_quality: Quality,
    reused_options: Option<Vec<PlaybackOption>>,
    sender: &mpsc::Sender<PlaybackLoadEvent>,
) -> anyhow::Result<(PreparedStream, PlaybackLocation, Vec<Quality>)> {
    // 旧地址可能已过期；复用失败后重新解析音源，再尝试当前可用的音质。
    let reused_stream = if let Some(options) = reused_options
        && let Some(option) = options
            .iter()
            .find(|option| option.quality == desired_quality)
    {
        prepare_playback_option(audio_cache, option, track)
            .await
            .ok()
            .map(|stream| {
                (
                    desired_quality,
                    stream,
                    options.iter().map(|option| option.quality).collect(),
                    options,
                )
            })
    } else {
        None
    };
    let (quality, stream, available_qualities, options) = match reused_stream {
        Some(reused) => reused,
        None => {
            let _ = sender.send(PlaybackLoadEvent::ResolvingOptions).await;
            let options = client.playback_options(credential, track).await?;
            let mut available_qualities = options
                .iter()
                .map(|option| option.quality)
                .collect::<Vec<_>>();
            let _ = sender
                .send(PlaybackLoadEvent::Options(available_qualities.clone()))
                .await;
            let candidates = Quality::fallback_order(&available_qualities, desired_quality);
            let mut prepared = None;
            let mut last_error = None;
            for quality in candidates {
                let Some(option) = options.iter().find(|option| option.quality == quality) else {
                    continue;
                };
                let stream_result = prepare_playback_option(audio_cache, option, track).await;
                match stream_result {
                    Ok(stream) => {
                        prepared = Some((quality, stream));
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
            let (quality, stream) = prepared.ok_or_else(|| {
                last_error.unwrap_or_else(|| anyhow::anyhow!("QQ 音乐没有返回当前账号可播放的音质"))
            })?;
            (quality, stream, available_qualities, options)
        }
    };
    Ok((
        stream,
        PlaybackLocation {
            track_mid: track.mid.clone(),
            quality,
            options,
        },
        available_qualities,
    ))
}

impl LyruneView {
    pub(super) fn start_playback(
        &mut self,
        index: usize,
        resume_at: Duration,
        requested_quality: Option<Quality>,
        autoplay: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(credential) = self.credential.clone() else {
            self.notify_error("请先登录 QQ 音乐", cx);
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
            self.notify_error("音频缓存不可用，无法创建播放流", cx);
            cx.notify();
            return;
        };
        let Some(audio) = &self.audio else {
            self.notify_error("没有可用的音频输出设备", cx);
            cx.notify();
            return;
        };
        let Some(client) = self.protocol_client.clone() else {
            self.notify_error("QQ 音乐客户端不可用", cx);
            cx.notify();
            return;
        };

        let desired_quality = requested_quality.unwrap_or(self.settings.playback_quality);
        let same_track = (self.current_track == Some(index) && self.playback_load.track.is_some())
            || self
                .playback_location
                .as_ref()
                .is_some_and(|location| location.track_mid == track.mid);
        if !same_track {
            self.available_qualities.clear();
            self.quality_menu_open = false;
        }
        let reused_options = self
            .playback_location
            .as_ref()
            .filter(|location| location.track_mid == track.mid)
            .map(|location| location.options.clone());

        audio.stop();
        if !same_track {
            self.lyric_motion_state = None;
            self.pending_lyric_reveal_mid = self.cover_backdrop_expanded.then(|| track.mid.clone());
        }
        self.ensure_track_lyrics(
            client.clone(),
            credential.clone(),
            track.as_ref().clone(),
            cx,
        );
        let generation = self.playback_load.invalidate();
        self.current_track = Some(index);
        self.playback_load.track = Some(index);
        self.ensure_track_like_state(track.mid.clone(), cx);
        self.playback_load.autoplay = autoplay;
        self.playback_started = false;
        self.wake_playback_ticks();
        self.position = resume_at;
        self.progress_hovered = false;
        self.progress_hover_fraction = None;
        let progress = progress_fraction(resume_at, Duration::from_secs(track.duration_seconds));
        self.progress_slider.update(cx, |slider, cx| {
            *slider = progress_slider_state(progress);
            cx.notify();
        });
        self.queue_waiting_for_recommendation = false;
        self.persist_current_playback();
        self.sync_table_playback_state(cx);
        #[cfg(target_os = "linux")]
        self.sync_mpris(!same_track && !resume_at.is_zero());
        #[cfg(target_os = "linux")]
        self.sync_inhibit();
        cx.notify();
        self.maybe_load_queue_recommendations(false, cx);

        let track_title = track.title.clone();
        let (sender, mut receiver) = mpsc::channel(1);
        // 丢弃 Tokio JoinHandle 会让任务继续运行。即使界面已离开，缓存准备仍可完成；
        // 通道关闭只表示结果无人接收，不能因此改变已启动任务的执行路径。
        drop(RUNTIME.spawn(async move {
            let result = async {
                let (stream, location, available_qualities) = prepare_playback_stream(
                    &audio_cache,
                    &client,
                    &credential,
                    &track,
                    desired_quality,
                    reused_options,
                    &sender,
                )
                .await?;
                let playback =
                    tokio::task::spawn_blocking(move || PreparedPlayback::new(stream, resume_at))
                        .await
                        .context("音频解码准备任务异常退出")??;
                Ok::<_, anyhow::Error>((playback, location, available_qualities))
            }
            .await;
            let _ = sender.send(PlaybackLoadEvent::Finished(result)).await;
        }));

        cx.spawn(async move |this, cx| {
            while let Some(event) = receiver.recv().await {
                let finished = matches!(&event, PlaybackLoadEvent::Finished(_));
                let seeked = matches!(&event, PlaybackLoadEvent::Finished(Ok(_)));
                let _ = this.update(cx, |this, cx| {
                    // 切歌、切音质或停止后，旧任务仍可能发来结果；不能据此替换当前音源。
                    if !this.playback_load.is_current(generation) {
                        return;
                    }
                    match event {
                        // 保留有界通道中的事件，以及下方的列表同步和界面通知。
                        PlaybackLoadEvent::ResolvingOptions => {}
                        PlaybackLoadEvent::Options(available_qualities) => {
                            this.available_qualities = available_qualities;
                        }
                        PlaybackLoadEvent::Finished(result) => {
                            this.playback_load.clear();
                            let playback_result = match result {
                                Ok((playback, location, available_qualities)) => {
                                    let quality = location.quality;
                                    this.playback_location = Some(location);
                                    this.active_quality = quality;
                                    this.available_qualities = available_qualities;
                                    this.audio
                                        .as_ref()
                                        .context("音频输出设备不可用")
                                        .and_then(|audio| audio.replace(playback, autoplay))
                                        .map_err(|error| {
                                            format!("播放歌曲“{}”失败：{error:#}", track_title)
                                        })
                                }
                                Err(error) => {
                                    Err(format!("获取歌曲“{}”失败：{error:#}", track_title))
                                }
                            };
                            match playback_result {
                                Ok(()) => {
                                    if let Some(audio) = &this.audio {
                                        audio.set_volume(this.settings.volume);
                                    }
                                    this.playback_started = true;
                                    this.wake_playback_ticks();
                                }
                                Err(error) => {
                                    this.notify_error(error, cx);
                                    this.play_next(false, cx);
                                }
                            }
                        }
                    }
                    this.sync_table_playback_state(cx);
                    #[cfg(target_os = "linux")]
                    if finished {
                        this.sync_mpris(seeked);
                        this.sync_inhibit();
                    }
                    cx.notify();
                });
                if finished {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn toggle_playback(&mut self, cx: &mut Context<Self>) {
        if self.playback_load.track.is_some() {
            return;
        }
        let Some(index) = self.current_track else {
            if !self.track_table.read(cx).delegate().tracks().is_empty() {
                self.select_track(0, cx);
            }
            return;
        };
        let Some(audio) = &self.audio else {
            return;
        };
        if !self.playback_started || audio.is_empty() {
            self.start_playback(index, Duration::ZERO, None, true, cx);
            return;
        }
        audio.toggle();
        self.wake_playback_ticks();
        self.persist_current_playback();
        self.sync_table_playback_state(cx);
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        #[cfg(target_os = "linux")]
        self.sync_inhibit();
        cx.notify();
    }

    #[cfg(target_os = "linux")]
    pub(super) fn play(&mut self, cx: &mut Context<Self>) {
        if self.playback_load.track.is_some() {
            return;
        }
        let Some(index) = self.current_track else {
            if !self.track_table.read(cx).delegate().tracks().is_empty() {
                self.select_track(0, cx);
            }
            return;
        };
        let Some(audio) = &self.audio else {
            return;
        };
        if !self.playback_started || audio.is_empty() {
            self.start_playback(index, Duration::ZERO, None, true, cx);
        } else if !audio.is_playing() {
            self.toggle_playback(cx);
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn pause_playback(&mut self, cx: &mut Context<Self>) {
        if self.playback_load.track.is_none()
            && self.audio.as_ref().is_some_and(AudioPlayer::is_playing)
        {
            self.toggle_playback(cx);
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn stop_playback(&mut self, cx: &mut Context<Self>) {
        if self.current_track.is_none() {
            return;
        }
        self.playback_load.invalidate();
        if let Some(audio) = &self.audio {
            audio.stop();
        }
        self.playback_load.clear();
        self.playback_started = false;
        self.wake_playback_ticks();
        self.position = Duration::ZERO;
        self.seek_preview = None;
        self.progress_slider.update(cx, |slider, cx| {
            *slider = progress_slider_state(0.);
            cx.notify();
        });
        self.persist_current_playback();
        self.sync_table_playback_state(cx);
        self.sync_mpris(false);
        self.sync_inhibit();
        cx.notify();
    }

    pub(super) fn seek_to(&mut self, target: Duration, cx: &mut Context<Self>) {
        let Some(index) = self.current_track else {
            return;
        };
        let target = self
            .current_duration()
            .map_or(target, |duration| target.min(duration));
        let autoplay = self.audio.as_ref().is_some_and(AudioPlayer::is_playing);
        self.start_playback(index, target, Some(self.active_quality), autoplay, cx);
        #[cfg(target_os = "linux")]
        self.sync_mpris(true);
        #[cfg(target_os = "linux")]
        self.sync_inhibit();
    }

    #[cfg(target_os = "linux")]
    pub(super) fn seek_by(&mut self, offset_micros: i64, cx: &mut Context<Self>) {
        if self.playback_load.track.is_some() || !self.playback_started {
            return;
        }
        let Some(duration) = self.current_duration() else {
            return;
        };
        let position = self
            .audio
            .as_ref()
            .map(AudioPlayer::position)
            .unwrap_or(self.position);
        let target_micros = duration_micros(position) as i128 + offset_micros as i128;
        let target_micros = target_micros.clamp(0, i64::MAX as i128) as i64;
        if target_micros >= duration_micros(duration) {
            self.play_next(false, cx);
        } else {
            self.seek_to(Duration::from_micros(target_micros as u64), cx);
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn set_mpris_position(
        &mut self,
        track_id: &str,
        position_micros: i64,
        cx: &mut Context<Self>,
    ) {
        if self.playback_load.track.is_some() || !self.playback_started {
            return;
        }
        let Some(track) = self.current_track_data() else {
            return;
        };
        if track_id != mpris_track_id(&track.mid) || position_micros < 0 {
            return;
        }
        let duration_micros = track
            .duration_seconds
            .saturating_mul(1_000_000)
            .min(i64::MAX as u64) as i64;
        if position_micros >= duration_micros {
            return;
        }
        self.seek_to(Duration::from_micros(position_micros as u64), cx);
    }

    pub(super) fn play_previous(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn play_next(&mut self, automatic: bool, cx: &mut Context<Self>) {
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
        let continuation = self
            .playback_queue
            .as_ref()
            .and_then(|queue| queue.continuation);
        let can_extend = continuation.is_some_and(PersistedQueueContinuation::can_load_more);
        let next = if automatic && self.repeat_mode == RepeatMode::One {
            Some(index)
        } else if continuation.is_none() && self.shuffle && len > 1 {
            Some(self.random_track_index(index, len))
        } else if index + 1 < len {
            Some(index + 1)
        } else if !can_extend && self.repeat_mode == RepeatMode::All {
            Some(0)
        } else {
            None
        };
        if let Some(next) = next {
            self.start_playback(next, Duration::ZERO, None, true, cx);
        } else if can_extend {
            self.playback_started = false;
            self.wake_playback_ticks();
            self.queue_waiting_for_recommendation = true;
            self.maybe_load_queue_recommendations(true, cx);
            self.persist_current_playback();
            #[cfg(target_os = "linux")]
            self.sync_mpris(false);
            #[cfg(target_os = "linux")]
            self.sync_inhibit();
            cx.notify();
        } else {
            self.playback_started = false;
            self.wake_playback_ticks();
            self.position = self.current_duration().unwrap_or_default();
            self.persist_current_playback();
            #[cfg(target_os = "linux")]
            self.sync_mpris(false);
            #[cfg(target_os = "linux")]
            self.sync_inhibit();
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

    pub(super) fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.settings.volume = volume.clamp(0., 1.);
        if self.settings.volume > 0. {
            self.settings.last_nonzero_volume = self.settings.volume;
        }
        if let Some(audio) = &self.audio {
            audio.set_volume(self.settings.volume);
        }
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        cx.notify();
    }

    pub(super) fn set_playback_quality(&mut self, quality: Quality, cx: &mut Context<Self>) {
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
        let loading = self.playback_load.track.is_some();
        let autoplay = if loading {
            self.playback_load.autoplay
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

    pub(super) fn toggle_mute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
}
