use std::sync::LazyLock;

use anyhow::{Context as _, anyhow, bail};
use quick_xml::{Reader, XmlVersion, events::Event};

const LATEST_RELEASE_URL: &str = "https://github.com/amtoaer/lyrune/releases.atom";
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(concat!("lyrune/", env!("CARGO_PKG_VERSION")))
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
    let feed = CLIENT
        .get(LATEST_RELEASE_URL)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .context("request latest release")?
        .error_for_status()
        .context("latest release response")?
        .text()
        .await
        .context("decode latest release")?;
    let release = parse_latest_release(&feed)?;
    if !is_newer_version(&release.version)? {
        return Ok(None);
    }
    Ok(Some(AvailableUpdate {
        version: release.version,
    }))
}

fn parse_latest_release(feed: &str) -> anyhow::Result<Release> {
    let mut reader = Reader::from_str(feed);
    loop {
        match reader.read_event().context("read release feed")? {
            Event::Start(element) if element.local_name().as_ref() == b"entry" => {
                return parse_release_entry(&mut reader);
            }
            Event::Eof => bail!("release feed contains no entries"),
            _ => {}
        }
    }
}

fn parse_release_entry(reader: &mut Reader<&[u8]>) -> anyhow::Result<Release> {
    let mut title = None;
    let mut url = None;
    loop {
        match reader.read_event().context("read release entry")? {
            Event::Start(element) if element.local_name().as_ref() == b"title" => {
                title = Some(
                    reader
                        .read_text(element.name())
                        .context("read release title")?
                        .decode()
                        .context("decode release title")?
                        .into_owned(),
                );
            }
            Event::Empty(element) | Event::Start(element)
                if element.local_name().as_ref() == b"link" =>
            {
                for attribute in element.attributes() {
                    let attribute = attribute.context("read release link")?;
                    if attribute.key.local_name().as_ref() == b"href" {
                        let href = attribute
                            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                            .context("decode release link")?
                            .into_owned();
                        if href.contains("/releases/tag/") {
                            url = Some(href);
                        }
                    }
                }
            }
            Event::End(element) if element.local_name().as_ref() == b"entry" => break,
            Event::Eof => bail!("release entry is incomplete"),
            _ => {}
        }
    }
    let title = title.context("release entry has no title")?;
    let url = url.context("release entry has no release link")?;
    let version = url
        .rsplit_once("/releases/tag/")
        .map(|(_, tag)| tag)
        .unwrap_or(title.as_str())
        .strip_prefix('v')
        .unwrap_or_else(|| title.strip_prefix('v').unwrap_or(title.as_str()));
    if version.is_empty() {
        return Err(anyhow!("release entry has no version"));
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
    fn parses_latest_release_feed() {
        let release = parse_latest_release(
            r#"
                <feed xmlns="http://www.w3.org/2005/Atom">
                    <entry>
                        <title>v1.5.0</title>
                        <link rel="alternate" href="https://github.com/amtoaer/lyrune/releases/tag/v1.5.0"/>
                    </entry>
                    <entry>
                        <title>v1.4.1</title>
                        <link rel="alternate" href="https://github.com/amtoaer/lyrune/releases/tag/v1.4.1"/>
                    </entry>
                </feed>
            "#,
        )
        .expect("parse release feed");
        assert_eq!(release.version, "1.5.0");
    }
}
