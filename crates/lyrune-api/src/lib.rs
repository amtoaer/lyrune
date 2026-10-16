//! Lyrune 内部使用的 QQ 音乐协议与登录实现。

mod account;
mod client;
mod error;
mod login;
mod models;
mod signing;

pub use account::{CredentialSession, LoginEvent, QqCredential, new_client_guid, run_qr_login};
pub use client::{CdnCache, ProtocolClient};
pub use login::qq::run_qq_login;
pub use login::quick::{QqAccount, QqQuickLogin};
pub use login::wechat::run_wechat_login;
pub use models::{
    Chart, ChartGroup, ChartHistory, ChartPreviewTrack, LyricResult, PlaybackOption, PlaylistPage,
    Quality, RadarTrackPage, RecommendationKind, SearchAlbum, SearchArtist, SearchPage,
    SearchResults, Track, UserPlaylist, UserPlaylistId, UserProfile,
};
