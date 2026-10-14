use super::ProtocolClient;
use super::response::{integer_field, string_field};
use crate::account::new_client_guid;
use anyhow::{Context as _, Result, bail};
use futures_util::StreamExt as _;
use reqwest::header::{ACCEPT_ENCODING, RANGE, REFERER};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(super) const DEFAULT_STREAM_DOMAIN: &str = "http://dl.stream.qqmusic.qq.com/";
const CDN_PROBE_BYTES: usize = 64 * 1024;
const CDN_PROBE_NODE_LIMIT: usize = 4;
const CDN_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
const DEFAULT_CDN_REFRESH: Duration = Duration::from_secs(30 * 60);
const MIN_CDN_REFRESH: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct CdnCache {
    client_guid: String,
    domains: Vec<String>,
    ranked_domains: Vec<String>,
    test_file: String,
    fetched_at: u64,
    refresh_time: u64,
    cache_time: u64,
    expiration: u64,
    measured_at: u64,
}

impl Default for CdnCache {
    fn default() -> Self {
        Self {
            client_guid: new_client_guid(),
            domains: Vec::new(),
            ranked_domains: Vec::new(),
            test_file: String::new(),
            fetched_at: 0,
            refresh_time: DEFAULT_CDN_REFRESH.as_secs(),
            cache_time: DEFAULT_CDN_REFRESH.as_secs(),
            expiration: DEFAULT_CDN_REFRESH.as_secs(),
            measured_at: 0,
        }
    }
}

impl CdnCache {
    pub(super) fn normalized(mut self) -> Self {
        if self.client_guid.trim().is_empty() {
            self.client_guid = new_client_guid();
        }

        let mut domains = Vec::new();
        append_unique_domains(&mut domains, self.domains);
        let has_direct_domain = domains.iter().any(|domain| !is_ws_stream_domain(domain));
        if has_direct_domain {
            domains.retain(|domain| !is_ws_stream_domain(domain));
        }

        let mut ranked_domains = Vec::new();
        append_unique_domains(&mut ranked_domains, self.ranked_domains);
        ranked_domains.retain(|domain| domains.contains(domain));
        append_unique_domains(&mut ranked_domains, domains.iter());

        self.domains = domains;
        self.ranked_domains = ranked_domains;
        self.refresh_time = self.refresh_time.max(MIN_CDN_REFRESH.as_secs());
        self.cache_time = self.cache_time.max(MIN_CDN_REFRESH.as_secs());
        self.expiration = self.expiration.max(MIN_CDN_REFRESH.as_secs());
        self
    }

    fn is_valid_at(&self, now: u64) -> bool {
        self.fetched_at != 0
            && !self.domains.is_empty()
            && now < self.fetched_at.saturating_add(self.expiration)
    }

    fn refresh_delay_at(&self, now: u64) -> Duration {
        if !self.is_valid_at(now) {
            return Duration::ZERO;
        }
        let refresh_at = self
            .fetched_at
            .saturating_add(self.refresh_time.min(self.cache_time).min(self.expiration));
        Duration::from_secs(refresh_at.saturating_sub(now))
    }

    fn measurement_is_fresh_at(&self, now: u64) -> bool {
        self.is_valid_at(now)
            && self.measured_at != 0
            && now < self.measured_at.saturating_add(self.cache_time)
    }

    fn has_same_nodes(&self, other: &Self) -> bool {
        self.domains.len() == other.domains.len()
            && self
                .domains
                .iter()
                .all(|domain| other.domains.contains(domain))
    }

    fn playback_domains_at(&self, now: u64) -> Vec<String> {
        if self.is_valid_at(now) {
            self.ranked_domains.clone()
        } else {
            Vec::new()
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct CdnProbe {
    first_byte: Duration,
    elapsed: Duration,
}

impl CdnProbe {
    fn score(self) -> Duration {
        self.first_byte.saturating_add(self.elapsed)
    }
}

impl ProtocolClient {
    pub async fn cdn_refresh_delay(&self) -> Duration {
        self.cdn.read().await.refresh_delay_at(unix_timestamp())
    }

    pub async fn refresh_cdn(&self) -> Result<CdnCache> {
        let _refresh = self.cdn_refresh.lock().await;
        let previous = self.cdn.read().await.clone();
        let data = self
            .call_anonymous(
                "music.audioCdnDispatch.cdnDispatch",
                "GetCdnDispatch",
                json!({
                    "guid": previous.client_guid.clone(),
                    "uid": "0",
                    "use_new_domain": 1,
                    "use_ipv6": 1,
                }),
                &previous.client_guid,
            )
            .await
            .context("无法获取 QQ 音乐 CDN 调度信息")?;
        let now = unix_timestamp();
        let mut dispatch = parse_cdn_dispatch(&data, previous.client_guid.clone(), now)?;

        let reuse_measurement =
            previous.has_same_nodes(&dispatch) && previous.measurement_is_fresh_at(now);
        if reuse_measurement {
            dispatch.ranked_domains = previous.ranked_domains;
            dispatch.measured_at = previous.measured_at;
        }

        // Publish the service-provided order immediately. Probing runs in the
        // caller's background maintenance task and never blocks song changes.
        *self.cdn.write().await = dispatch.clone();

        if !reuse_measurement
            && !dispatch.test_file.is_empty()
            && let Some(ranked_domains) = self
                .rank_cdn_domains(&dispatch.domains, &dispatch.test_file)
                .await
        {
            dispatch.ranked_domains = ranked_domains;
            dispatch.measured_at = now;
            *self.cdn.write().await = dispatch.clone();
        }

        Ok(dispatch)
    }

    pub(super) async fn cached_cdn_domains(&self) -> Vec<String> {
        self.cdn.read().await.playback_domains_at(unix_timestamp())
    }

    async fn rank_cdn_domains(&self, domains: &[String], test_file: &str) -> Option<Vec<String>> {
        let mut successful = Vec::new();
        for (index, domain) in domains.iter().take(CDN_PROBE_NODE_LIMIT).enumerate() {
            if let Some(probe) = self.probe_cdn(domain, test_file).await {
                successful.push((index, probe.score()));
            }
        }
        successful.sort_by_key(|(_, score)| *score);
        if successful.is_empty() {
            return None;
        }

        let mut ranked = Vec::with_capacity(domains.len());
        for (index, _) in successful {
            append_unique_domain(&mut ranked, &domains[index]);
        }
        append_unique_domains(&mut ranked, domains.iter().cloned());
        Some(ranked)
    }

    async fn probe_cdn(&self, domain: &str, test_file: &str) -> Option<CdnProbe> {
        let url = playback_url(domain, &playback_path_and_query(test_file));
        let started = Instant::now();
        let response = self
            .client
            .get(url)
            .header(REFERER, "https://y.qq.com/")
            .header(ACCEPT_ENCODING, "identity")
            .header(RANGE, format!("bytes=0-{}", CDN_PROBE_BYTES - 1))
            .timeout(CDN_PROBE_TIMEOUT)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        let mut stream = response.bytes_stream();
        let mut received = 0;
        let mut first_byte = None;
        while received < CDN_PROBE_BYTES {
            let Some(chunk) = stream.next().await else {
                break;
            };
            let chunk = chunk.ok()?;
            if chunk.is_empty() {
                continue;
            }
            first_byte.get_or_insert_with(|| started.elapsed());
            received += chunk.len().min(CDN_PROBE_BYTES - received);
        }
        Some(CdnProbe {
            first_byte: first_byte?,
            elapsed: started.elapsed(),
        })
    }
}

fn parse_cdn_dispatch(data: &Value, client_guid: String, fetched_at: u64) -> Result<CdnCache> {
    let mut domains = Vec::new();
    if let Some(items) = data.get("sip").and_then(Value::as_array) {
        append_unique_domains(&mut domains, items.iter().filter_map(Value::as_str));
    }
    if let Some(items) = data.get("sipinfo").and_then(Value::as_array) {
        append_unique_domains(
            &mut domains,
            items.iter().filter_map(|item| string_field(item, &["cdn"])),
        );
    }
    let has_direct_domain = domains.iter().any(|domain| !is_ws_stream_domain(domain));
    if has_direct_domain {
        domains.retain(|domain| !is_ws_stream_domain(domain));
    }
    if domains.is_empty() {
        bail!("QQ 音乐 CDN 调度没有返回可用节点");
    }

    let refresh_time = cdn_policy_seconds(
        data,
        &["refreshTime", "refresh_time"],
        DEFAULT_CDN_REFRESH.as_secs(),
    );
    let cache_time = cdn_policy_seconds(data, &["cacheTime", "cache_time"], refresh_time);
    let expiration = cdn_policy_seconds(data, &["expiration"], cache_time);
    Ok(CdnCache {
        client_guid,
        ranked_domains: domains.clone(),
        domains,
        test_file: string_field(data, &["keepalivefile", "test_file"]).unwrap_or_default(),
        fetched_at,
        refresh_time,
        cache_time,
        expiration,
        measured_at: 0,
    })
}

fn cdn_policy_seconds(data: &Value, keys: &[&str], fallback: u64) -> u64 {
    integer_field(data, keys)
        .filter(|seconds| *seconds > 0)
        .unwrap_or(fallback)
        .max(MIN_CDN_REFRESH.as_secs())
}

pub(super) fn playback_stream_domains(data: &Value) -> Vec<String> {
    let mut domains = Vec::new();
    if let Some(items) = data.get("sip").and_then(Value::as_array) {
        append_unique_domains(&mut domains, items.iter().filter_map(Value::as_str));
    }
    let has_direct_domain = domains.iter().any(|domain| !is_ws_stream_domain(domain));
    if has_direct_domain {
        domains.retain(|domain| !is_ws_stream_domain(domain));
    }
    domains
}

pub(super) fn append_unique_domains<I, S>(domains: &mut Vec<String>, candidates: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for domain in candidates {
        append_unique_domain(domains, domain.as_ref());
    }
}

pub(super) fn append_unique_domain(domains: &mut Vec<String>, domain: &str) {
    let domain = domain.trim();
    if domain.is_empty() || !(domain.starts_with("http://") || domain.starts_with("https://")) {
        return;
    }
    let domain = format!("{}/", domain.trim_end_matches('/'));
    if !domains.contains(&domain) {
        domains.push(domain);
    }
}

pub(super) fn playback_urls(domains: &[String], path: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let absolute =
        (path.starts_with("http://") || path.starts_with("https://")).then(|| path.to_owned());
    let path = playback_path_and_query(path);
    for domain in domains {
        let url = playback_url(domain, &path);
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    if let Some(absolute) = absolute
        && !urls.contains(&absolute)
    {
        urls.push(absolute);
    }
    urls
}

fn playback_path_and_query(path: &str) -> String {
    reqwest::Url::parse(path).map_or_else(
        |_| path.trim_start_matches('/').to_owned(),
        |url| {
            let mut value = url.path().trim_start_matches('/').to_owned();
            if let Some(query) = url.query() {
                value.push('?');
                value.push_str(query);
            }
            value
        },
    )
}

fn playback_url(domain: &str, path: &str) -> String {
    format!(
        "{}/{}",
        domain.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn is_ws_stream_domain(domain: &str) -> bool {
    let domain = domain.trim();
    let host = domain
        .strip_prefix("http://")
        .or_else(|| domain.strip_prefix("https://"))
        .unwrap_or(domain)
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default();
    let label = host.split('.').next().unwrap_or_default();
    label == "ws"
        || label.strip_prefix("ws").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_a_non_ws_stream_domain_returned_by_qq_music() {
        let data = json!({
            "sip": [
                "http://ws6.stream.qqmusic.qq.com/",
                "https://isure.stream.qqmusic.qq.com/",
                "http://dl.stream.qqmusic.qq.com/"
            ]
        });

        assert_eq!(
            playback_stream_domains(&data),
            [
                "https://isure.stream.qqmusic.qq.com/",
                "http://dl.stream.qqmusic.qq.com/"
            ]
        );
        assert_eq!(
            playback_stream_domains(&json!({
                "sip": ["https://ws.stream.qqmusic.qq.com/"]
            })),
            ["https://ws.stream.qqmusic.qq.com/"]
        );
        assert!(is_ws_stream_domain("http://ws12.stream.qqmusic.qq.com/"));
        assert!(!is_ws_stream_domain("https://isure.stream.qqmusic.qq.com/"));
        assert!(playback_stream_domains(&json!({ "sip": [] })).is_empty());
    }

    #[test]
    fn parses_cdn_dispatch_nodes_and_refresh_policy() {
        let dispatch = parse_cdn_dispatch(
            &json!({
                "sip": [
                    "https://ws.stream.qqmusic.qq.com/",
                    "https://first.example/"
                ],
                "sipinfo": [
                    { "cdn": "https://first.example/", "quic": 1 },
                    { "cdn": "https://second.example/" }
                ],
                "keepalivefile": "test/keepalive.bin",
                "refreshTime": 900,
                "cacheTime": 1800,
                "expiration": 3600
            }),
            "installation-guid".to_owned(),
            10_000,
        )
        .expect("parse CDN dispatch");

        assert_eq!(
            dispatch.domains,
            ["https://first.example/", "https://second.example/"]
        );
        assert_eq!(dispatch.test_file, "test/keepalive.bin");
        assert_eq!(dispatch.client_guid, "installation-guid");
        assert_eq!(dispatch.fetched_at, 10_000);
        assert_eq!(dispatch.refresh_time, 900);
        assert_eq!(dispatch.cache_time, 1800);
        assert_eq!(dispatch.expiration, 3600);
        assert_eq!(dispatch.refresh_delay_at(10_200), Duration::from_secs(700));
        assert_eq!(dispatch.refresh_delay_at(10_900), Duration::ZERO);
        assert!(dispatch.is_valid_at(13_599));
        assert!(!dispatch.is_valid_at(13_600));
    }

    #[test]
    fn persisted_cdn_ranking_is_reused_until_its_cache_time() {
        let mut previous = parse_cdn_dispatch(
            &json!({
                "sip": ["https://first.example/", "https://second.example/"],
                "refreshTime": 900,
                "cacheTime": 1800,
                "expiration": 3600
            }),
            "installation-guid".to_owned(),
            10_000,
        )
        .expect("parse previous CDN dispatch");
        previous.ranked_domains.reverse();
        previous.measured_at = 10_100;
        let current = parse_cdn_dispatch(
            &json!({
                "sip": ["https://second.example/", "https://first.example/"],
                "refreshTime": 900,
                "cacheTime": 1800,
                "expiration": 3600
            }),
            "installation-guid".to_owned(),
            10_900,
        )
        .expect("parse current CDN dispatch");

        assert!(previous.has_same_nodes(&current));
        assert!(previous.measurement_is_fresh_at(11_899));
        assert!(!previous.measurement_is_fresh_at(11_900));
    }

    #[tokio::test]
    #[ignore = "requires live QQ Music network access"]
    async fn anonymously_loads_cdn_dispatch() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = ProtocolClient::new().expect("create protocol client");

        let cache = client
            .refresh_cdn()
            .await
            .expect("load anonymous CDN dispatch");

        assert!(!cache.domains.is_empty());
        assert!(cache.refresh_time > 0);
        assert!(cache.cache_time > 0);
        assert!(cache.expiration > 0);
    }

    #[test]
    fn builds_cdn_fallbacks_without_changing_the_vkey_path() {
        let domains = vec![
            "https://fast.example/".to_owned(),
            "https://backup.example/".to_owned(),
        ];
        let urls = playback_urls(
            &domains,
            "https://original.example/C400media.m4a?vkey=opaque",
        );

        assert_eq!(
            urls,
            [
                "https://fast.example/C400media.m4a?vkey=opaque",
                "https://backup.example/C400media.m4a?vkey=opaque",
                "https://original.example/C400media.m4a?vkey=opaque",
            ]
        );
    }
}
