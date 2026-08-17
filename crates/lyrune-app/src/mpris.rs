use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use async_channel::{Receiver, Sender};
use mpris_server::{
    LoopStatus, Metadata, PlaybackStatus, Player, Time, TrackId, zbus::Result as ZbusResult,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MprisPlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MprisLoopStatus {
    None,
    Track,
    Playlist,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MprisTrack {
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub art_url: Option<String>,
    pub length_micros: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MprisSnapshot {
    pub playback_status: MprisPlaybackStatus,
    pub loop_status: MprisLoopStatus,
    pub shuffle: bool,
    pub volume: f64,
    pub position_micros: i64,
    pub track: Option<MprisTrack>,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_seek: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MprisCommand {
    Raise,
    Quit,
    Next,
    Previous,
    Pause,
    PlayPause,
    Stop,
    Play,
    Seek(i64),
    SetPosition { track_id: String, position: i64 },
    SetLoopStatus(MprisLoopStatus),
    SetShuffle(bool),
    SetVolume(f64),
}

enum MprisUpdate {
    State {
        snapshot: MprisSnapshot,
        seeked: bool,
    },
    Position(i64),
}

#[derive(Clone)]
pub struct MprisHandle {
    updates: Sender<MprisUpdate>,
}

impl MprisHandle {
    pub fn update(&self, snapshot: MprisSnapshot) {
        let _ = self.updates.try_send(MprisUpdate::State {
            snapshot,
            seeked: false,
        });
    }

    pub fn seeked(&self, snapshot: MprisSnapshot) {
        let _ = self.updates.try_send(MprisUpdate::State {
            snapshot,
            seeked: true,
        });
    }

    pub fn update_position(&self, position_micros: i64) {
        let _ = self
            .updates
            .try_send(MprisUpdate::Position(position_micros));
    }
}

pub struct MprisService {
    handle: MprisHandle,
    shutdown: Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MprisService {
    pub fn handle(&self) -> MprisHandle {
        self.handle.clone()
    }
}

impl Drop for MprisService {
    fn drop(&mut self) {
        let _ = self.shutdown.try_send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn install() -> Result<(MprisService, Receiver<MprisCommand>)> {
    let (commands, command_events) = async_channel::unbounded();
    let (updates, update_events) = async_channel::unbounded();
    let (shutdown, shutdown_events) = async_channel::bounded(1);
    let (startup, startup_result) = std::sync::mpsc::sync_channel(1);

    let thread = thread::Builder::new()
        .name("lyrune-mpris".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = startup.send(Err(format!("无法创建 MPRIS 运行时：{error}")));
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, async move {
                let player = match tokio::time::timeout(
                    STARTUP_TIMEOUT,
                    Player::builder("lyrune")
                        .identity("Lyrune")
                        .desktop_entry("lyrune")
                        .can_raise(true)
                        .can_quit(true)
                        .can_control(true)
                        .build(),
                )
                .await
                {
                    Ok(Ok(player)) => player,
                    Ok(Err(error)) => {
                        let _ = startup.send(Err(format!("无法注册 MPRIS 服务：{error}")));
                        return;
                    }
                    Err(_) => {
                        let _ = startup.send(Err("注册 MPRIS 服务超时".to_owned()));
                        return;
                    }
                };

                connect_commands(&player, commands);
                tokio::task::spawn_local(player.run());
                let _ = startup.send(Ok(()));

                loop {
                    tokio::select! {
                        _ = shutdown_events.recv() => break,
                        update = update_events.recv() => {
                            let Ok(update) = update else {
                                break;
                            };
                            if let Err(error) = apply_update(&player, update).await {
                                eprintln!("更新 MPRIS 状态失败：{error}");
                            }
                        }
                    }
                }
            });
        })
        .context("无法启动 MPRIS 服务线程")?;

    match startup_result.recv_timeout(STARTUP_TIMEOUT + Duration::from_secs(1)) {
        Ok(Ok(())) => Ok((
            MprisService {
                handle: MprisHandle { updates },
                shutdown,
                thread: Some(thread),
            },
            command_events,
        )),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(anyhow!(error))
        }
        Err(error) => {
            let _ = shutdown.try_send(());
            let _ = thread.join();
            Err(error).context("等待 MPRIS 服务启动失败")
        }
    }
}

fn connect_commands(player: &Player, commands: Sender<MprisCommand>) {
    let sender = commands.clone();
    player.connect_raise(move |_| send_command(&sender, MprisCommand::Raise));
    let sender = commands.clone();
    player.connect_quit(move |_| send_command(&sender, MprisCommand::Quit));
    let sender = commands.clone();
    player.connect_next(move |_| send_command(&sender, MprisCommand::Next));
    let sender = commands.clone();
    player.connect_previous(move |_| send_command(&sender, MprisCommand::Previous));
    let sender = commands.clone();
    player.connect_pause(move |_| send_command(&sender, MprisCommand::Pause));
    let sender = commands.clone();
    player.connect_play_pause(move |_| send_command(&sender, MprisCommand::PlayPause));
    let sender = commands.clone();
    player.connect_stop(move |_| send_command(&sender, MprisCommand::Stop));
    let sender = commands.clone();
    player.connect_play(move |_| send_command(&sender, MprisCommand::Play));
    let sender = commands.clone();
    player.connect_seek(move |_, offset| {
        send_command(&sender, MprisCommand::Seek(offset.as_micros()));
    });
    let sender = commands.clone();
    player.connect_set_position(move |_, track_id, position| {
        send_command(
            &sender,
            MprisCommand::SetPosition {
                track_id: track_id.to_string(),
                position: position.as_micros(),
            },
        );
    });
    let sender = commands.clone();
    player.connect_set_loop_status(move |_, status| {
        send_command(
            &sender,
            MprisCommand::SetLoopStatus(match status {
                LoopStatus::None => MprisLoopStatus::None,
                LoopStatus::Track => MprisLoopStatus::Track,
                LoopStatus::Playlist => MprisLoopStatus::Playlist,
            }),
        );
    });
    let sender = commands.clone();
    player.connect_set_shuffle(move |_, shuffle| {
        send_command(&sender, MprisCommand::SetShuffle(shuffle));
    });
    player.connect_set_volume(move |_, volume| {
        send_command(&commands, MprisCommand::SetVolume(volume));
    });
}

fn send_command(commands: &Sender<MprisCommand>, command: MprisCommand) {
    let _ = commands.try_send(command);
}

async fn apply_update(player: &Player, update: MprisUpdate) -> ZbusResult<()> {
    let (snapshot, seeked) = match update {
        MprisUpdate::State { snapshot, seeked } => (snapshot, seeked),
        MprisUpdate::Position(position_micros) => {
            player.set_position(Time::from_micros(position_micros));
            return Ok(());
        }
    };
    player.set_position(Time::from_micros(snapshot.position_micros));
    player.set_metadata(metadata(snapshot.track)).await?;
    player
        .set_loop_status(loop_status(snapshot.loop_status))
        .await?;
    player.set_shuffle(snapshot.shuffle).await?;
    player.set_volume(snapshot.volume).await?;
    player.set_can_go_next(snapshot.can_go_next).await?;
    player.set_can_go_previous(snapshot.can_go_previous).await?;
    player.set_can_play(snapshot.can_play).await?;
    player.set_can_pause(snapshot.can_pause).await?;
    player.set_can_seek(snapshot.can_seek).await?;
    player
        .set_playback_status(playback_status(snapshot.playback_status))
        .await?;
    if seeked {
        player
            .seeked(Time::from_micros(snapshot.position_micros))
            .await?;
    }
    Ok(())
}

fn metadata(track: Option<MprisTrack>) -> Metadata {
    let Some(track) = track else {
        return Metadata::new();
    };
    let track_id = TrackId::try_from(track.id).expect("generated MPRIS track ID must be valid");
    let mut metadata = Metadata::builder()
        .trackid(track_id)
        .length(Time::from_micros(track.length_micros))
        .title(track.title);
    if !track.artists.is_empty() {
        metadata = metadata.artist(track.artists);
    }
    if let Some(album) = track.album {
        metadata = metadata.album(album);
    }
    if let Some(art_url) = track.art_url {
        metadata = metadata.art_url(art_url);
    }
    metadata.build()
}

fn playback_status(status: MprisPlaybackStatus) -> PlaybackStatus {
    match status {
        MprisPlaybackStatus::Playing => PlaybackStatus::Playing,
        MprisPlaybackStatus::Paused => PlaybackStatus::Paused,
        MprisPlaybackStatus::Stopped => PlaybackStatus::Stopped,
    }
}

fn loop_status(status: MprisLoopStatus) -> LoopStatus {
    match status {
        MprisLoopStatus::None => LoopStatus::None,
        MprisLoopStatus::Track => LoopStatus::Track,
        MprisLoopStatus::Playlist => LoopStatus::Playlist,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_contains_the_required_track_identity_and_length() {
        let metadata = metadata(Some(MprisTrack {
            id: "/dev/lyrune/track/id_1234".to_owned(),
            title: "Song".to_owned(),
            artists: vec!["Artist".to_owned()],
            album: Some("Album".to_owned()),
            art_url: Some("https://example.com/cover.jpg".to_owned()),
            length_micros: 123_000_000,
        }));

        assert_eq!(
            metadata.trackid().as_ref().map(ToString::to_string),
            Some("/dev/lyrune/track/id_1234".to_owned())
        );
        assert_eq!(metadata.length(), Some(Time::from_secs(123)));
        assert_eq!(metadata.title(), Some("Song"));
        assert_eq!(metadata.artist(), Some(vec!["Artist".to_owned()]));
    }
}
