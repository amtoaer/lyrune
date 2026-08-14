use std::io::BufReader;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context as _, Result};
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source as _};
use tokio_util::sync::CancellationToken;

use crate::cache::{CacheStatus, CachedAudioSource, PreparedStream};

type StreamingDecoder = Decoder<BufReader<CachedAudioSource>>;

pub struct PreparedPlayback {
    decoder: StreamingDecoder,
    resume_at: Duration,
    cache_status: CacheStatus,
    cancellation: Option<CancellationToken>,
}

impl PreparedPlayback {
    pub fn new(stream: PreparedStream, resume_at: Duration) -> Result<Self> {
        let builder = Decoder::builder()
            .with_data(BufReader::new(stream.source))
            .with_hint(stream.format_hint)
            .with_seekable(false);
        let decoder = match stream.content_length {
            Some(content_length) => builder
                .with_byte_len(content_length)
                .with_seekable(false)
                .build(),
            None => builder.build(),
        }
        .context("无法解码歌曲音频流")?;

        Ok(Self {
            decoder,
            resume_at,
            cache_status: stream.cache_status,
            cancellation: stream.cancellation,
        })
    }

    pub fn cache_status(&self) -> CacheStatus {
        self.cache_status
    }
}

pub struct AudioPlayer {
    _device: MixerDeviceSink,
    player: Player,
    active_stream: Mutex<Option<CancellationToken>>,
}

impl AudioPlayer {
    pub fn new() -> Result<Self> {
        let device = DeviceSinkBuilder::open_default_sink().context("无法打开默认音频输出设备")?;
        let player = Player::connect_new(device.mixer());
        Ok(Self {
            _device: device,
            player,
            active_stream: Mutex::new(None),
        })
    }

    pub fn replace(&self, playback: PreparedPlayback) -> Result<()> {
        self.cancel_active_stream();
        self.player.clear();
        let PreparedPlayback {
            decoder,
            resume_at,
            cancellation,
            ..
        } = playback;
        if resume_at.is_zero() {
            self.player.append(decoder);
        } else {
            self.player.append(decoder.skip_duration(resume_at));
        }
        *self
            .active_stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = cancellation;
        self.player.play();
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
        self.player.get_pos()
    }

    pub fn stop(&self) {
        self.cancel_active_stream();
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
