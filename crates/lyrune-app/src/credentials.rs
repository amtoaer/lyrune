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
        let Some(credential) = CredentialStore::load().expect("read system keyring") else {
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
                .liked_tracks(&credential, 100),
        )
        .await
        .expect("liked tracks request timed out")
        .expect("load liked tracks");
        eprintln!(
            "loaded {} liked tracks in {:?}",
            tracks.len(),
            liked_started.elapsed()
        );
    }
}
