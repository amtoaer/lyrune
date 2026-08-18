use std::io::BufReader;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context as _, Result};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source as _};
use tokio_util::sync::CancellationToken;

use crate::cache::{CachedAudioSource, PreparedStream};

type StreamingDecoder = Decoder<BufReader<CachedAudioSource>>;

pub struct PreparedPlayback {
    decoder: StreamingDecoder,
    cancellation: Option<CancellationToken>,
    position_offset: Duration,
}

impl PreparedPlayback {
    pub fn new(stream: PreparedStream, resume_at: Duration) -> Result<Self> {
        let builder = Decoder::builder()
            .with_data(BufReader::new(stream.source))
            .with_hint(stream.format_hint)
            .with_seekable(true);
        let mut decoder = match stream.content_length {
            Some(content_length) => builder
                .with_byte_len(content_length)
                .with_seekable(true)
                .build(),
            None => builder.build(),
        }
        .context("无法解码歌曲音频流")?;

        if !resume_at.is_zero() {
            decoder
                .try_seek(resume_at)
                .context("无法跳转到目标播放位置")?;
        }

        Ok(Self {
            decoder,
            cancellation: stream.cancellation,
            position_offset: resume_at,
        })
    }
}

pub struct AudioPlayer {
    _device: MixerDeviceSink,
    player: Player,
    active_stream: Mutex<Option<CancellationToken>>,
    position: Mutex<PlaybackPosition>,
}

#[derive(Default)]
struct PlaybackPosition {
    offset: Duration,
    origin: Option<Duration>,
}

impl AudioPlayer {
    pub fn new() -> Result<Self> {
        let mut device =
            DeviceSinkBuilder::open_default_sink().context("无法打开默认音频输出设备")?;
        device.log_on_drop(false);
        let player = Player::connect_new(device.mixer());
        Ok(Self {
            _device: device,
            player,
            active_stream: Mutex::new(None),
            position: Mutex::new(PlaybackPosition::default()),
        })
    }

    pub fn replace(&self, playback: PreparedPlayback, autoplay: bool) -> Result<()> {
        let position_origin = self.player.get_pos();
        self.cancel_active_stream();
        self.player.clear();
        if !autoplay {
            self.player.pause();
        }
        let PreparedPlayback {
            decoder,
            cancellation,
            position_offset,
        } = playback;
        *self
            .position
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = PlaybackPosition {
            offset: position_offset,
            origin: (!position_origin.is_zero()).then_some(position_origin),
        };
        self.player.append(decoder);
        *self
            .active_stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = cancellation;
        if autoplay {
            self.player.play();
        }
        Ok(())
    }

    pub fn toggle(&self) -> bool {
        if self.player.is_paused() {
            self.player.play();
            true
        } else {
            self.player.pause();
            false
        }
    }

    pub fn is_playing(&self) -> bool {
        !self.player.is_paused() && !self.player.empty()
    }

    pub fn position(&self) -> Duration {
        let raw_position = self.player.get_pos();
        let mut position = self
            .position
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let source_position = position_since_replace(raw_position, &mut position.origin);
        absolute_position(position.offset, source_position)
    }

    pub fn set_volume(&self, volume: f32) {
        self.player.set_volume(volume.clamp(0.0, 1.0));
    }

    pub fn is_empty(&self) -> bool {
        self.player.empty()
    }

    pub fn stop(&self) {
        self.cancel_active_stream();
        *self
            .position
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = PlaybackPosition::default();
        self.player.stop();
    }

    fn cancel_active_stream(&self) {
        if let Some(cancellation) = self
            .active_stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            cancellation.cancel();
        }
    }
}

fn position_since_replace(raw_position: Duration, origin: &mut Option<Duration>) -> Duration {
    let Some(previous_position) = *origin else {
        return raw_position;
    };
    if raw_position < previous_position {
        *origin = None;
        raw_position
    } else {
        raw_position.saturating_sub(previous_position)
    }
}

fn absolute_position(source_start: Duration, source_position: Duration) -> Duration {
    source_start.saturating_add(source_position)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeked_source_position_stays_on_the_absolute_timeline() {
        assert_eq!(
            absolute_position(Duration::from_secs(90), Duration::from_secs(3)),
            Duration::from_secs(93)
        );
    }

    #[test]
    fn replacement_position_ignores_the_previous_source_clock_until_it_resets() {
        let mut origin = Some(Duration::from_secs(48));

        assert_eq!(
            position_since_replace(Duration::from_millis(48_120), &mut origin),
            Duration::from_millis(120)
        );
        assert_eq!(
            position_since_replace(Duration::from_millis(15), &mut origin),
            Duration::from_millis(15)
        );
        assert_eq!(origin, None);
    }
}
