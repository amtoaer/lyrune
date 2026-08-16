use anyhow::{Context as _, Result};
use qqmusic_api::integration::QqCredential;

const SERVICE: &str = "dev.lyrune";
const ACCOUNT: &str = "qqmusic";

pub struct CredentialStore;

impl CredentialStore {
    pub fn load() -> Result<Option<QqCredential>> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).context("无法连接系统钥匙串")?;

        let serialized = match entry.get_password() {
            Ok(value) => value,
            Err(keyring::Error::NoEntry) => return Ok(None),
            Err(error) => return Err(error).context("无法读取系统钥匙串"),
        };

        serde_json::from_str(&serialized)
            .map(Some)
            .context("系统钥匙串中的 QQ 音乐凭据格式无效")
    }

    pub fn save(credential: &QqCredential) -> Result<()> {
        let serialized = serde_json::to_string(credential).context("无法序列化 QQ 音乐凭据")?;
        keyring::Entry::new(SERVICE, ACCOUNT)
            .context("无法连接系统钥匙串")?
            .set_password(&serialized)
            .context("无法写入系统钥匙串")
    }

    pub fn delete() -> Result<()> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).context("无法连接系统钥匙串")?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error).context("无法删除系统钥匙串中的凭据"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use qqmusic_api::integration::{ProtocolClient, refresh_credential};

    use super::CredentialStore;

    #[tokio::test]
    #[ignore = "requires a system keyring entry and live QQ Music network access"]
    async fn stored_credential_loads_liked_tracks() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let Some(credential) = tokio::task::spawn_blocking(CredentialStore::load)
            .await
            .expect("join keyring read")
            .expect("read system keyring")
        else {
            eprintln!("no stored QQ Music credential in this test process");
            return;
        };

        let refresh_started = Instant::now();
        let credential =
            tokio::time::timeout(Duration::from_secs(30), refresh_credential(credential))
                .await
                .expect("credential refresh timed out")
                .expect("refresh credential");
        eprintln!("credential restored in {:?}", refresh_started.elapsed());

        let liked_started = Instant::now();
        let tracks = tokio::time::timeout(
            Duration::from_secs(30),
            ProtocolClient::new()
                .expect("create protocol client")
                .liked_tracks(&credential, 1),
        )
        .await
        .expect("liked tracks request timed out")
        .expect("load liked tracks");
        eprintln!(
            "loaded {} liked tracks in {:?}",
            tracks.len(),
            liked_started.elapsed()
        );

        if let Some(track) = tracks.first() {
            assert!(
                track.artists.trim().is_empty() || !track.artist_details.is_empty(),
                "first liked track has artists but no artist navigation metadata"
            );
            assert!(
                track.album.trim().is_empty() || !track.album_mid.trim().is_empty(),
                "first liked track has an album but no album navigation metadata"
            );
            let options = ProtocolClient::new()
                .expect("create protocol client")
                .playback_options(&credential, track)
                .await
                .expect("load playback options");
            assert!(
                !options.is_empty(),
                "first liked track has no playable quality"
            );
            eprintln!(
                "available qualities for first track: {}",
                options
                    .iter()
                    .map(|option| option.quality.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires a system keyring entry and live QQ Music network access"]
    async fn stored_credential_searches_each_category_and_opens_collections() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let Some(credential) = tokio::task::spawn_blocking(CredentialStore::load)
            .await
            .expect("join keyring read")
            .expect("read system keyring")
        else {
            eprintln!("no stored QQ Music credential in this test process");
            return;
        };
        let credential =
            tokio::time::timeout(Duration::from_secs(30), refresh_credential(credential))
                .await
                .expect("credential refresh timed out")
                .expect("refresh credential");
        let client = ProtocolClient::new().expect("create protocol client");
        let results = tokio::time::timeout(
            Duration::from_secs(30),
            client.search(&credential, "周杰伦", 3),
        )
        .await
        .expect("search timed out")
        .expect("search QQ Music");

        assert!(!results.songs.items.is_empty(), "song search is empty");
        assert!(!results.artists.items.is_empty(), "artist search is empty");
        assert!(!results.albums.items.is_empty(), "album search is empty");
        assert!(
            !results.playlists.items.is_empty(),
            "playlist search is empty"
        );

        let artist = results.artists.items[0].clone();
        let artist_albums = tokio::time::timeout(
            Duration::from_secs(30),
            client.artist_albums(&credential, &artist, 0, 3),
        )
        .await
        .expect("artist albums request timed out")
        .expect("load artist albums");
        assert!(!artist_albums.items.is_empty(), "artist albums are empty");
        assert!(
            artist_albums.items.len() <= 3,
            "artist album limit was ignored"
        );

        for collection in [
            artist.into_playlist(),
            results.albums.items[0].clone().into_playlist(),
            results.playlists.items[0].clone(),
        ] {
            let page = tokio::time::timeout(
                Duration::from_secs(30),
                client.playlist_page(&credential, &collection, 0, 3),
            )
            .await
            .expect("collection request timed out")
            .expect("open search collection");
            assert!(
                !page.tracks.is_empty(),
                "{} returned no tracks",
                collection.title
            );
        }
    }
}
