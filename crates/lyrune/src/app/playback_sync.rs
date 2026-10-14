//! 播放状态与持久化、MPRIS 和休眠抑制的同步。
//! 后台 tick 属于应用；窗口关闭后仍处理曲终续播和进度保存。

use std::time::{Duration, Instant};

use futures_util::{
    future::{Either, select},
    pin_mut,
};
use gpui::Context;
use tokio::sync::mpsc;

use super::LyruneView;
#[cfg(target_os = "linux")]
use super::queue::RepeatMode;
#[cfg(target_os = "linux")]
use crate::inhibit::InhibitHandle;
#[cfg(target_os = "linux")]
use crate::mpris::{
    MprisCommand, MprisHandle, MprisLoopStatus, MprisPlaybackStatus, MprisSnapshot, MprisTrack,
};
use crate::player::AudioPlayer;
#[cfg(target_os = "linux")]
use crate::settings::PersistedQueueContinuation;
#[cfg(target_os = "linux")]
use xxhash_rust::xxh3::xxh3_128;

const PROGRESS_TICK: Duration = Duration::from_millis(250);
const PLAYBACK_PERSIST_INTERVAL: Duration = Duration::from_secs(5);

#[cfg(target_os = "linux")]
pub(super) fn duration_micros(duration: Duration) -> i64 {
    duration.as_micros().min(i64::MAX as u128) as i64
}

#[cfg(target_os = "linux")]
pub(super) fn mpris_track_id(track_mid: &str) -> String {
    format!(
        "/dev/lyrune/track/id_{:032x}",
        xxh3_128(track_mid.as_bytes())
    )
}

impl LyruneView {
    // 绑定应用实体而非窗口。暂停时等待唤醒，不靠不断轮询维持后台播放。
    pub(crate) fn start_background_tick(&mut self, cx: &mut Context<Self>) {
        let (wake_sender, mut wake_receiver) = mpsc::channel(1);
        self.background_tick_wake = Some(wake_sender);
        cx.spawn(async move |this, cx| {
            loop {
                let interval = match this.read_with(cx, |this, _| {
                    if !this.playback.is_advancing() {
                        None
                    } else {
                        Some(PROGRESS_TICK)
                    }
                }) {
                    Ok(interval) => interval,
                    Err(_) => break,
                };
                let Some(interval) = interval else {
                    if wake_receiver.recv().await.is_none() {
                        break;
                    }
                    continue;
                };

                let timer = cx.background_executor().timer(interval);
                let wake = wake_receiver.recv();
                pin_mut!(timer, wake);
                match select(timer, wake).await {
                    Either::Left(_) => {
                        if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                            break;
                        }
                    }
                    Either::Right((Some(()), _)) => {}
                    Either::Right((None, _)) => break,
                }
            }
        })
        .detach();
    }

    pub(super) fn wake_playback_tick(&self) {
        if let Some(wake) = &self.background_tick_wake {
            let _ = wake.try_send(());
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn attach_mpris(&mut self, mpris: MprisHandle, cx: &gpui::App) {
        self.mpris = Some(mpris);
        self.sync_mpris(false, cx);
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn attach_inhibit(&mut self, inhibit: InhibitHandle) {
        self.inhibit = Some(inhibit);
        self.sync_inhibit();
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn handle_mpris_command(&mut self, command: MprisCommand, cx: &mut Context<Self>) {
        match command {
            MprisCommand::Raise | MprisCommand::Quit => {}
            MprisCommand::Next => self.play_next(false, cx),
            MprisCommand::Previous => self.play_previous(cx),
            MprisCommand::Pause => self.pause_playback(cx),
            MprisCommand::PlayPause => self.toggle_playback(cx),
            MprisCommand::Stop => self.stop_playback(cx),
            MprisCommand::Play => self.play(cx),
            MprisCommand::Seek(offset) => self.seek_by(offset, cx),
            MprisCommand::SetPosition { track_id, position } => {
                self.set_mpris_position(&track_id, position, cx);
            }
            MprisCommand::SetLoopStatus(status) => {
                self.playback.repeat_mode = match status {
                    MprisLoopStatus::None => RepeatMode::Off,
                    MprisLoopStatus::Track => RepeatMode::One,
                    MprisLoopStatus::Playlist => RepeatMode::All,
                };
                self.sync_mpris(false, cx);
                cx.notify();
            }
            MprisCommand::SetShuffle(shuffle) => {
                self.playback.shuffle = shuffle;
                self.sync_mpris(false, cx);
                cx.notify();
            }
            MprisCommand::SetVolume(volume) => {
                let volume = volume.clamp(0., 1.) as f32;
                self.player_controls
                    .update(cx, |view, cx| view.reset_volume(volume, cx));
                self.set_volume(volume, cx);
                self.preferences.read(cx).save();
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn mpris_snapshot(&self, cx: &gpui::App) -> MprisSnapshot {
        let audio_available = self.playback.audio.is_some();
        let loading = self.playback.load.track.is_some();
        // 系统媒体状态在加载期间沿用 Playing，区别于设备时钟是否实际推进。
        let playback_status = if loading {
            MprisPlaybackStatus::Playing
        } else if !self.playback.started {
            MprisPlaybackStatus::Stopped
        } else if self
            .playback
            .audio
            .as_deref()
            .is_some_and(AudioPlayer::is_playing)
        {
            MprisPlaybackStatus::Playing
        } else {
            MprisPlaybackStatus::Paused
        };
        let (queue_len, current_index) = self.playback.queue.as_ref().map_or((0, None), |queue| {
            (queue.tracks.len(), self.playback.current_track)
        });
        let can_extend = self
            .playback
            .queue
            .as_ref()
            .and_then(|queue| queue.continuation)
            .is_some_and(PersistedQueueContinuation::can_load_more);
        let can_go_next = audio_available
            && current_index.is_some_and(|index| {
                can_extend
                    || (self.playback.shuffle && queue_len > 1)
                    || index + 1 < queue_len
                    || (self.playback.repeat_mode == RepeatMode::All && queue_len > 0)
            });
        let can_go_previous = audio_available
            && current_index.is_some_and(|index| {
                self.playback.position >= Duration::from_secs(3)
                    || index > 0
                    || (self.playback.repeat_mode == RepeatMode::All && queue_len > 0)
            });
        let track = self.playback.track().map(|track| MprisTrack {
            id: mpris_track_id(&track.mid),
            title: track.title.clone(),
            artists: if track.artists.trim().is_empty() {
                Vec::new()
            } else {
                vec![track.artists.clone()]
            },
            album: (!track.album.trim().is_empty()).then(|| track.album.clone()),
            art_url: track.cover_url.clone().filter(|url| !url.trim().is_empty()),
            length_micros: track
                .duration_seconds
                .saturating_mul(1_000_000)
                .min(i64::MAX as u64) as i64,
        });
        let has_track = track.is_some();
        MprisSnapshot {
            playback_status,
            loop_status: match self.playback.repeat_mode {
                RepeatMode::Off => MprisLoopStatus::None,
                RepeatMode::All => MprisLoopStatus::Playlist,
                RepeatMode::One => MprisLoopStatus::Track,
            },
            shuffle: self.playback.shuffle,
            volume: self.preferences.read(cx).settings.volume as f64,
            position_micros: duration_micros(self.playback.position),
            track,
            can_go_next,
            can_go_previous,
            can_play: has_track && audio_available && !loading,
            can_pause: has_track && audio_available && !loading && self.playback.started,
            can_seek: has_track && audio_available && !loading && self.playback.started,
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn sync_mpris(&self, seeked: bool, cx: &gpui::App) {
        if let Some(mpris) = &self.mpris {
            let snapshot = self.mpris_snapshot(cx);
            if seeked {
                mpris.seeked(snapshot);
            } else {
                mpris.update(snapshot);
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn sync_inhibit(&self) {
        if let Some(inhibit) = &self.inhibit {
            inhibit.set_active(self.playback.is_advancing());
        }
    }

    #[cfg(target_os = "linux")]
    fn sync_mpris_position(&self) {
        if let Some(mpris) = &self.mpris {
            mpris.update_position(duration_micros(self.playback.position));
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.progress.read(cx).seek_preview.is_none()
            && self.playback.load.track.is_none()
            && self.playback.started
        {
            self.playback.position = self
                .playback
                .audio
                .as_deref()
                .map(AudioPlayer::position)
                .unwrap_or_default();
        }

        let ended = self.playback.started
            && self.playback.load.track.is_none()
            && self
                .playback
                .audio
                .as_deref()
                .is_some_and(AudioPlayer::is_empty);
        if ended {
            self.playback.started = false;
            self.play_next(true, cx);
        }
        if self.playback.current_track.is_some()
            && self.playback.last_persisted_at.elapsed() >= PLAYBACK_PERSIST_INTERVAL
        {
            self.persist_current_playback(cx);
        }
        #[cfg(target_os = "linux")]
        if self.playback.current_track.is_some()
            && self.playback.load.track.is_none()
            && self.last_mpris_position_sync.elapsed() >= PROGRESS_TICK
        {
            self.sync_mpris_position();
            self.last_mpris_position_sync = Instant::now();
        }
    }
}
