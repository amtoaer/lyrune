//! 音源解析、解码准备和播放控制；队列组成在 queue，外部状态同步在 playback_sync。

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use gpui::{Context, Window};
use lyrune_api::{CredentialSession, PlaybackOption, ProtocolClient, Quality, Track};
use tokio::sync::mpsc;

use crate::cache::{AudioCache, PreparedStream};
use crate::player::{AudioPlayer, PreparedPlayback};
use crate::runtime::RUNTIME;
use crate::settings::{PersistedPlayback, PersistedQueueContinuation};

use super::LyruneView;
#[cfg(target_os = "linux")]
use super::playback_sync::{duration_micros, mpris_track_id};
use super::queue::{PlaybackQueue, RepeatMode};

// 播放会话独立于当前浏览页和窗口控件；关闭窗口后仍由应用实体持有。
pub(super) struct Playback {
    pub(super) audio: Option<Arc<AudioPlayer>>,
    pub(super) queue: Option<PlaybackQueue>,
    pub(super) queue_generation: u64,
    // loading 可在当前歌曲播放时预取；waiting 表示已到队尾，结果到达后需要续播。
    pub(super) recommendation_loading: bool,
    pub(super) waiting_for_recommendation: bool,
    // 属于播放队列，不是当前页面表格中的行号。
    pub(super) current_track: Option<usize>,
    pub(super) load: PlaybackLoad,
    // 表示音源已装入播放器；暂停时仍为 true，用于区分暂停与尚未开始/已经停止。
    pub(super) started: bool,
    pub(super) location: Option<PlaybackLocation>,
    // 记录回退后实际采用的音质；用户偏好保存在 settings.playback_quality。
    pub(super) active_quality: Quality,
    pub(super) available_qualities: Vec<Quality>,
    pub(super) position: Duration,
    pub(super) shuffle: bool,
    pub(super) repeat_mode: RepeatMode,
    pub(super) pending_restore: Option<PersistedPlayback>,
    pub(super) last_persisted_at: Instant,
}

impl Playback {
    pub(super) fn new(
        audio: Option<Arc<AudioPlayer>>,
        quality: Quality,
        pending_restore: Option<PersistedPlayback>,
    ) -> Self {
        Self {
            audio,
            queue: None,
            queue_generation: 0,
            recommendation_loading: false,
            waiting_for_recommendation: false,
            current_track: None,
            load: PlaybackLoad::default(),
            started: false,
            location: None,
            active_quality: quality,
            available_qualities: Vec::new(),
            position: Duration::ZERO,
            shuffle: false,
            repeat_mode: RepeatMode::Off,
            pending_restore,
            last_persisted_at: Instant::now(),
        }
    }

    pub(super) fn replace_queue(&mut self, queue: PlaybackQueue) -> u64 {
        // 队列补全与音源加载有不同的寿命；换队列不代替随后 start_playback 的音源失效。
        self.queue_generation = self.queue_generation.wrapping_add(1);
        self.recommendation_loading = false;
        self.waiting_for_recommendation = false;
        self.queue = Some(queue);
        self.queue_generation
    }

    pub(super) fn track(&self) -> Option<&Track> {
        self.queue
            .as_ref()?
            .tracks
            .get(self.current_track?)
            .map(Arc::as_ref)
    }

    pub(super) fn duration(&self) -> Option<Duration> {
        self.track()
            .map(|track| Duration::from_secs(track.duration_seconds))
    }

    pub(super) fn is_advancing(&self) -> bool {
        self.started
            && self.load.track.is_none()
            && self.audio.as_deref().is_some_and(AudioPlayer::is_playing)
    }

    pub(super) fn snapshot(&mut self, account_id: u64) -> Option<PersistedPlayback> {
        let queue = self.queue.as_ref()?;
        let track = queue.tracks.get(self.current_track?)?;
        // 加载/停止时设备时钟可能仍属于上一音源；保存显式位置，不能用设备位置覆盖。
        let position = if self.load.track.is_some() || !self.started {
            self.position
        } else {
            self.audio
                .as_deref()
                .map(AudioPlayer::position)
                .unwrap_or(self.position)
        };
        self.position = position;
        Some(PersistedPlayback {
            account_id,
            playlist_id: queue.playlist_id.clone(),
            track_mid: track.mid.clone(),
            position_ms: position.as_millis().min(u64::MAX as u128) as u64,
            queue_tracks: queue.tracks.clone(),
            queue_modified: queue.modified,
            queue_continuation: queue.continuation,
        })
    }
}

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
        let Some(credential) = self.account.read(cx).credential.clone() else {
            self.notify_error("请先登录 QQ 音乐", cx);
            cx.notify();
            return;
        };
        let Some(track) = self
            .playback
            .queue
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
        let Some(audio) = &self.playback.audio else {
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
        let same_track = (self.playback.current_track == Some(index)
            && self.playback.load.track.is_some())
            || self
                .playback
                .location
                .as_ref()
                .is_some_and(|location| location.track_mid == track.mid);
        if !same_track {
            self.playback.available_qualities.clear();
            self.quality_menu_open = false;
        }
        let reused_options = self
            .playback
            .location
            .as_ref()
            .filter(|location| location.track_mid == track.mid)
            .map(|location| location.options.clone());

        audio.stop();
        if !same_track {
            self.lyrics_view.update(cx, |view, cx| {
                view.start_track(&track.mid, self.cover_backdrop_expanded, cx)
            });
        }
        self.ensure_track_lyrics(
            client.clone(),
            credential.clone(),
            track.as_ref().clone(),
            cx,
        );
        let generation = self.playback.load.invalidate();
        self.playback.current_track = Some(index);
        self.playback.load.track = Some(index);
        self.ensure_track_like_state(track.mid.clone(), cx);
        self.playback.load.autoplay = autoplay;
        self.playback.started = false;
        self.wake_playback_tick();
        self.playback.position = resume_at;
        self.progress.update(cx, |view, cx| {
            view.start_track(resume_at, Duration::from_secs(track.duration_seconds), cx);
        });
        self.playback.waiting_for_recommendation = false;
        self.persist_current_playback(cx);
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
                    if !this.playback.load.is_current(generation) {
                        return;
                    }
                    match event {
                        // 保留有界通道中的事件，以及下方的列表同步和界面通知。
                        PlaybackLoadEvent::ResolvingOptions => {}
                        PlaybackLoadEvent::Options(available_qualities) => {
                            this.playback.available_qualities = available_qualities;
                        }
                        PlaybackLoadEvent::Finished(result) => {
                            this.playback.load.clear();
                            let playback_result = match result {
                                Ok((playback, location, available_qualities)) => {
                                    let quality = location.quality;
                                    this.playback.location = Some(location);
                                    this.playback.active_quality = quality;
                                    this.playback.available_qualities = available_qualities;
                                    this.playback
                                        .audio
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
                                    if let Some(audio) = &this.playback.audio {
                                        audio.set_volume(this.settings.volume);
                                    }
                                    this.playback.started = true;
                                    this.wake_playback_tick();
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
        if self.playback.load.track.is_some() {
            return;
        }
        let Some(index) = self.playback.current_track else {
            if !self.track_table.read(cx).delegate().tracks().is_empty() {
                self.select_track(0, cx);
            }
            return;
        };
        let Some(audio) = &self.playback.audio else {
            return;
        };
        if !self.playback.started || audio.is_empty() {
            self.start_playback(index, Duration::ZERO, None, true, cx);
            return;
        }
        audio.toggle();
        self.wake_playback_tick();
        self.persist_current_playback(cx);
        self.sync_table_playback_state(cx);
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        #[cfg(target_os = "linux")]
        self.sync_inhibit();
        cx.notify();
    }

    #[cfg(target_os = "linux")]
    pub(super) fn play(&mut self, cx: &mut Context<Self>) {
        if self.playback.load.track.is_some() {
            return;
        }
        let Some(index) = self.playback.current_track else {
            if !self.track_table.read(cx).delegate().tracks().is_empty() {
                self.select_track(0, cx);
            }
            return;
        };
        let Some(audio) = &self.playback.audio else {
            return;
        };
        if !self.playback.started || audio.is_empty() {
            self.start_playback(index, Duration::ZERO, None, true, cx);
        } else if !audio.is_playing() {
            self.toggle_playback(cx);
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn pause_playback(&mut self, cx: &mut Context<Self>) {
        if self.playback.load.track.is_none()
            && self
                .playback
                .audio
                .as_deref()
                .is_some_and(AudioPlayer::is_playing)
        {
            self.toggle_playback(cx);
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn stop_playback(&mut self, cx: &mut Context<Self>) {
        if self.playback.current_track.is_none() {
            return;
        }
        self.playback.load.invalidate();
        if let Some(audio) = &self.playback.audio {
            audio.stop();
        }
        self.playback.load.clear();
        self.playback.started = false;
        self.wake_playback_tick();
        self.playback.position = Duration::ZERO;
        self.progress.update(cx, |view, cx| view.stop(cx));
        self.persist_current_playback(cx);
        self.sync_table_playback_state(cx);
        self.sync_mpris(false);
        self.sync_inhibit();
        cx.notify();
    }

    pub(super) fn seek_to(&mut self, target: Duration, cx: &mut Context<Self>) {
        let Some(index) = self.playback.current_track else {
            return;
        };
        let target = self
            .playback
            .duration()
            .map_or(target, |duration| target.min(duration));
        let autoplay = self
            .playback
            .audio
            .as_deref()
            .is_some_and(AudioPlayer::is_playing);
        self.start_playback(
            index,
            target,
            Some(self.playback.active_quality),
            autoplay,
            cx,
        );
        #[cfg(target_os = "linux")]
        self.sync_mpris(true);
        #[cfg(target_os = "linux")]
        self.sync_inhibit();
    }

    #[cfg(target_os = "linux")]
    pub(super) fn seek_by(&mut self, offset_micros: i64, cx: &mut Context<Self>) {
        if self.playback.load.track.is_some() || !self.playback.started {
            return;
        }
        let Some(duration) = self.playback.duration() else {
            return;
        };
        let position = self
            .playback
            .audio
            .as_deref()
            .map(AudioPlayer::position)
            .unwrap_or(self.playback.position);
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
        if self.playback.load.track.is_some() || !self.playback.started {
            return;
        }
        let Some(track) = self.playback.track() else {
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
        let Some(index) = self.playback.current_track else {
            return;
        };
        let queue_len = self
            .playback
            .queue
            .as_ref()
            .map_or(0, |queue| queue.tracks.len());
        if self.playback.position >= Duration::from_secs(3) {
            self.seek_to(Duration::ZERO, cx);
            return;
        }
        let previous = if index > 0 {
            Some(index - 1)
        } else if self.playback.repeat_mode == RepeatMode::All && queue_len > 0 {
            Some(queue_len - 1)
        } else {
            None
        };
        if let Some(previous) = previous {
            self.start_playback(previous, Duration::ZERO, None, true, cx);
        }
    }

    pub(super) fn play_next(&mut self, automatic: bool, cx: &mut Context<Self>) {
        let Some(index) = self.playback.current_track else {
            return;
        };
        let len = self
            .playback
            .queue
            .as_ref()
            .map_or(0, |queue| queue.tracks.len());
        if len == 0 {
            return;
        }
        let continuation = self
            .playback
            .queue
            .as_ref()
            .and_then(|queue| queue.continuation);
        let can_extend = continuation.is_some_and(PersistedQueueContinuation::can_load_more);
        let next = if automatic && self.playback.repeat_mode == RepeatMode::One {
            Some(index)
        } else if continuation.is_none() && self.playback.shuffle && len > 1 {
            Some(self.random_track_index(index, len))
        } else if index + 1 < len {
            Some(index + 1)
        } else if !can_extend && self.playback.repeat_mode == RepeatMode::All {
            Some(0)
        } else {
            None
        };
        if let Some(next) = next {
            self.start_playback(next, Duration::ZERO, None, true, cx);
            return;
        }

        self.playback.started = false;
        self.wake_playback_tick();
        if can_extend {
            self.playback.waiting_for_recommendation = true;
            self.maybe_load_queue_recommendations(true, cx);
        } else {
            self.playback.position = self.playback.duration().unwrap_or_default();
        }
        self.persist_current_playback(cx);
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        #[cfg(target_os = "linux")]
        self.sync_inhibit();
        cx.notify();
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
        if let Some(audio) = &self.playback.audio {
            audio.set_volume(self.settings.volume);
        }
        #[cfg(target_os = "linux")]
        self.sync_mpris(false);
        cx.notify();
    }

    pub(super) fn set_playback_quality(&mut self, quality: Quality, cx: &mut Context<Self>) {
        if !self.playback.available_qualities.contains(&quality) {
            return;
        }
        self.quality_menu_open = false;
        if self.settings.playback_quality != quality {
            self.settings.playback_quality = quality;
            self.persist_settings();
        }
        if self.playback.active_quality == quality {
            cx.notify();
            return;
        }
        let Some(index) = self.playback.current_track else {
            cx.notify();
            return;
        };
        let loading = self.playback.load.track.is_some();
        let autoplay = if loading {
            self.playback.load.autoplay
        } else {
            self.playback
                .audio
                .as_deref()
                .is_some_and(AudioPlayer::is_playing)
        };
        let resume_at = if loading {
            self.playback.position
        } else {
            self.playback
                .audio
                .as_deref()
                .map(AudioPlayer::position)
                .unwrap_or(self.playback.position)
        };
        self.start_playback(index, resume_at, Some(quality), autoplay, cx);
    }

    pub(super) fn toggle_mute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let volume = if self.settings.volume > 0. {
            0.
        } else {
            self.settings.last_nonzero_volume
        };
        self.player_controls
            .update(cx, |view, cx| view.set_volume(volume, window, cx));
        self.set_volume(volume, cx);
        self.persist_settings();
    }
}
