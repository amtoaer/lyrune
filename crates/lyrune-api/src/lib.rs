//! Lyrune 内部使用的 QQ 音乐协议与登录实现。

mod account;
mod client;
mod error;
mod login;
mod models;
mod signing;

pub use account::{CredentialSession, LoginEvent, QqCredential, run_qr_login};
pub use client::{CdnCache, ProtocolClient};
pub use models::{
    Chart, ChartGroup, ChartHistory, ChartPreviewTrack, LyricResult, PlaybackOption, PlaylistPage,
    Quality, RadarTrackPage, RecommendationKind, SearchAlbum, SearchArtist, SearchPage,
    SearchResults, Track, UserPlaylist, UserPlaylistId, UserProfile,
};
