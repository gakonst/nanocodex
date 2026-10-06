//! Native CLI public-page capability. Each hop resolves and pins only public
//! addresses before connecting; model URLs cannot supply credentials or proxies.
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use nanocodex::claude_tools::web::{ApprovedPage, ApprovedWebFetchSource, WebFetchRequest};
use reqwest::{Client, Url, redirect::Policy};

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;
const MAX_PAGE_BYTES: usize = 128 * 1024;

#[derive(Default)]
pub(super) struct PublicWebFetch {
    // This capability exists only in the integration-test executable. It lets
    // that test serve synthetic HTTPS pages without a public Internet service.
    #[cfg(test)]
    pub(super) fixture: Option<(String, SocketAddr, reqwest::Certificate)>,
}

impl PublicWebFetch {
    pub(super) fn new() -> Self {
        Self::default()
    }

    async fn client(&self, url: &Url) -> Result<Client, String> {
        let host = url.host_str().ok_or("WebFetch URL has no host")?;
        let port = url
            .port_or_known_default()
            .ok_or("WebFetch URL has no port")?;
        let mut builder = Client::builder()
            // Never inherit HTTP(S)_PROXY/ALL_PROXY or ambient credentials.
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .redirect(Policy::none())
            .referer(false)
            .connect_timeout(Duration::from_secs(5))
            .timeout(TIMEOUT)
            .user_agent("Nanocodex-WebFetch/1.0");
        #[cfg(test)]
        if let Some((fixture_host, address, certificate)) = &self.fixture
            && host == fixture_host
            && port == address.port()
        {
            return builder
                .resolve(host, *address)
                .add_root_certificate(certificate.clone())
                .build()
                .map_err(|_| "WebFetch fixture TLS initialization failed".into());
        }
        let addresses: Vec<SocketAddr> =
            if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
                vec![SocketAddr::new(ip, port)]
            } else {
                tokio::net::lookup_host((host, port))
                    .await
                    .map_err(|_| "WebFetch public DNS resolution failed")?
                    .take(33)
                    .collect()
            };
        if addresses.is_empty()
            || addresses.len() > 32
            || addresses.iter().any(|address| !public_ip(address.ip()))
        {
            return Err("WebFetch requires exclusively public Internet addresses".into());
        }
        // A fresh per-hop client uses exactly the checked addresses, preventing
        // a second DNS lookup (rebinding) and cross-origin pooled connections.
        builder = builder.resolve_to_addrs(host, &addresses);
        builder
            .build()
            .map_err(|_| "WebFetch HTTP client initialization failed".into())
    }

    async fn page(&self, request: WebFetchRequest) -> Result<ApprovedPage, String> {
        let mut url = checked_url(&request.url)?;
        let limit = request.max_output_bytes.min(MAX_PAGE_BYTES);
        if limit == 0 {
            return Err("WebFetch requires a positive capture limit".into());
        }
        for hop in 0..=MAX_REDIRECTS {
            let client = self.client(&url).await?;
            let mut response = client
                .get(url.clone())
                .header(
                    reqwest::header::ACCEPT,
                    "text/html, text/plain, application/xhtml+xml",
                )
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .send()
                .await
                .map_err(|_| "WebFetch HTTPS request failed")?;
            if response.status().is_redirection() {
                if hop == MAX_REDIRECTS {
                    return Err("WebFetch exceeded five redirects".into());
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .ok_or("WebFetch redirect has no Location")?
                    .to_str()
                    .map_err(|_| "WebFetch redirect Location is invalid")?;
                if location.len() > 2048 {
                    return Err("WebFetch redirect URL exceeds 2048 bytes".into());
                }
                let target = url
                    .join(location)
                    .map_err(|_| "WebFetch redirect URL is invalid")?;
                url = checked_url(target.as_str())?;
                continue;
            }
            if !response.status().is_success() {
                return Err(format!(
                    "WebFetch HTTP status {}",
                    response.status().as_u16()
                ));
            }
            if let Some(encoding) = response.headers().get(reqwest::header::CONTENT_ENCODING)
                && encoding != "identity"
            {
                return Err("WebFetch requires an uncompressed text response".into());
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("");
            let mime = content_type.split(';').next().unwrap_or("").trim();
            if !["text/plain", "text/html", "application/xhtml+xml"]
                .iter()
                .any(|allowed| mime.eq_ignore_ascii_case(allowed))
            {
                return Err("WebFetch supports HTML or plain-text pages only".into());
            }
            let mut bytes = Vec::with_capacity(limit);
            let mut truncated = false;
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| "WebFetch response read failed")?
            {
                let remaining = limit - bytes.len();
                if chunk.len() > remaining {
                    bytes.extend_from_slice(&chunk[..remaining]);
                    truncated = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
                // Do not read an additional chunk just to detect truncation.
                if bytes.len() == limit {
                    truncated = true;
                    break;
                }
            }
            let mut content = String::from_utf8_lossy(&bytes).into_owned();
            let marker = "\n[Page capture truncated]";
            let max_text = if truncated && limit >= marker.len() {
                limit - marker.len()
            } else {
                limit
            };
            let mut end = content.len().min(max_text);
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            content.truncate(end);
            if truncated && limit >= marker.len() {
                content.push_str(marker);
            }
            return Ok(ApprovedPage {
                final_url: url.to_string(),
                content,
            });
        }
        unreachable!("redirect limit returns within loop")
    }
}

impl ApprovedWebFetchSource for PublicWebFetch {
    async fn fetch_source(&self, request: WebFetchRequest) -> Result<ApprovedPage, String> {
        tokio::time::timeout(TIMEOUT, self.page(request))
            .await
            .map_err(|_| "WebFetch timed out after 20 seconds".to_owned())?
    }
}

fn checked_url(raw: &str) -> Result<Url, String> {
    if raw.len() > 2048
        || raw
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '\\')
    {
        return Err("WebFetch URL is invalid or exceeds 2048 bytes".into());
    }
    let mut url = Url::parse(raw).map_err(|_| "WebFetch URL is invalid")?;
    if url.scheme() != "https" {
        return Err("WebFetch requires HTTPS".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("WebFetch URL must not contain credentials".into());
    }
    if url.host_str().is_none() || url.port() == Some(0) {
        return Err("WebFetch URL has no valid host/port".into());
    }
    url.set_fragment(None);
    Ok(url)
}

// Conservative Internet-unicast policy. Exclude local, metadata, multicast,
// reserved, documentation, benchmarking and transition/translation ranges.
pub(super) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && (c == 0 || c == 2))
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            (segments[0] & 0xe000) == 0x2000
                && !(segments[0] == 0x2001 && (segments[1] <= 0x01ff || segments[1] == 0x0db8))
                && segments[0] != 0x2002
                && segments[0] != 0x3fff
        }
    }
}
