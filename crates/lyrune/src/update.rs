use std::sync::LazyLock;

use anyhow::{Context as _, bail, ensure};

const LATEST_RELEASE_URL: &str = "https://github.com/amtoaer/lyrune/releases/latest";
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(concat!("lyrune/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("create update check client")
});

#[derive(Debug)]
struct Release {
    version: String,
}

#[derive(Debug)]
pub struct AvailableUpdate {
    pub version: String,
}

pub async fn check() -> anyhow::Result<Option<AvailableUpdate>> {
    let response = CLIENT
        .head(LATEST_RELEASE_URL)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .context("request latest release")?
        .error_for_status()
        .context("latest release response")?;
    ensure!(
        response.status().is_redirection(),
        "latest release did not redirect: {}",
        response.status()
    );
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .context("latest release response has no location")?
        .to_str()
        .context("decode latest release location")?;
    let release = parse_latest_release(location)?;
    if !is_newer_version(&release.version)? {
        return Ok(None);
    }
    Ok(Some(AvailableUpdate {
        version: release.version,
    }))
}

fn parse_latest_release(location: &str) -> anyhow::Result<Release> {
    let version = location
        .strip_prefix("https://github.com/amtoaer/lyrune/releases/tag/v")
        .context("latest release did not redirect to a version tag")?;
    if version.is_empty() {
        bail!("latest release has no version");
    }
    Ok(Release {
        version: version.to_owned(),
    })
}

fn is_newer_version(version: &str) -> anyhow::Result<bool> {
    let current = parse_version(env!("CARGO_PKG_VERSION"))?;
    let latest = parse_version(version)?;
    Ok(latest > current)
}

fn parse_version(version: &str) -> anyhow::Result<[u64; 3]> {
    let mut parts = version.split('.');
    let parsed = [parts.next(), parts.next(), parts.next()]
        .map(|part| part.and_then(|part| part.parse().ok()));
    if parts.next().is_some() || parsed.iter().any(Option::is_none) {
        bail!("invalid release version: {version}");
    }
    Ok(parsed.map(Option::unwrap))
}

#[cfg(test)]
mod tests {
    use super::parse_latest_release;

    #[test]
    fn test_parse_latest_release() {
        let release = parse_latest_release("https://github.com/amtoaer/lyrune/releases/tag/v1.6.0")
            .expect("parse release redirect");
        assert_eq!(release.version, "1.6.0");
    }

    #[test]
    fn test_invalid_release_redirect() {
        for location in [
            "https://github.com/amtoaer/lyrune/releases",
            "https://github.com/amtoaer/lyrune/releases/tag/v",
            "https://github.com/other/lyrune/releases/tag/v1.6.0",
        ] {
            assert!(parse_latest_release(location).is_err());
        }
    }
}
