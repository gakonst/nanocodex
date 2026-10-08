//! Private HLS upload sink for portable playback links.
//! Upload URLs and bearer tokens never enter logs, status values or errors.
use serde_json::Value;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

pub(crate) const MAX_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PLAYLIST_BYTES: usize = 16 * 1024;
pub(crate) const WINDOW: usize = 6;
pub(crate) const SEGMENT_SECONDS: u32 = 2;
const MAX_LIFETIME_MS: u64 = 8 * 3600 * 1000 + 60_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONSECUTIVE_FAILURES: u32 = 3;
pub(crate) const LOCAL_PLAYLIST: &str = "local.m3u8";

#[derive(Clone)]
pub(crate) struct Target {
    pub stream_id: String,
    base: Url,
    token: String,
    pub expires_at: u64,
}
impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("stream_id", &self.stream_id)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
pub(crate) fn valid_stream_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
fn loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(host)) => host == "localhost",
        None => false,
    }
}
/// Upload base URL: HTTPS (or loopback HTTP for local tests), path ending in `/`,
/// no credentials, query or fragment. The caller additionally pins its origin.
pub(crate) fn valid_upload_url(value: &str) -> Option<Url> {
    if value.len() > 2048 || value.bytes().any(|c| c.is_ascii_control() || c.is_ascii_whitespace()) {
        return None;
    }
    let url = Url::parse(value).ok()?;
    let scheme = match url.scheme() {
        "https" => true,
        "http" => loopback(&url),
        _ => false,
    };
    (scheme
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.port() != Some(0)
        && url.path().ends_with('/')
        && url.path().len() > 1)
        .then_some(url)
}
fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'~' | b'-'))
}
/// Parse a broker `start` request. `origin` is the authenticated publisher origin.
pub(crate) fn parse_start(request: &Value, origin: &url::Origin, now: u64) -> Result<Target, &'static str> {
    let stream_id = request["stream_id"].as_str().filter(|s| valid_stream_id(s)).ok_or("invalid_request")?;
    let upload = &request["upload"];
    let base = upload["url"].as_str().and_then(valid_upload_url).ok_or("invalid_request")?;
    if &base.origin() != origin {
        return Err("invalid_request");
    }
    let token = upload["token"].as_str().filter(|s| valid_token(s)).ok_or("invalid_request")?;
    let expires_at = upload["expires_at"].as_u64().ok_or("invalid_request")?;
    if expires_at <= now {
        return Err("expired");
    }
    if expires_at > now + MAX_LIFETIME_MS {
        return Err("invalid_request");
    }
    Ok(Target { stream_id: stream_id.into(), base, token: token.into(), expires_at })
}
