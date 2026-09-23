//! Payload-free, bounded diagnostics. Never format transport errors: they may
//! retain request URLs, SDP, credentials, or broker-controlled close reasons.
use std::{
    net::IpAddr,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

pub(crate) struct Budget {
    count: AtomicUsize,
    started: Instant,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            count: AtomicUsize::new(0),
            started: Instant::now(),
        }
    }
}
impl Budget {
    pub(crate) fn take(&self) -> bool {
        self.count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < 32).then_some(n + 1)
            })
            .is_ok()
    }
    pub(crate) fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
    pub(crate) fn event(
        &self,
        phase: &'static str,
        outcome: &'static str,
        http_status: Option<u16>,
    ) {
        if self.take() {
            tracing::info!(target: "nanocodex2", stage = "screen.negotiation", phase, outcome, http_status, elapsed_ms = self.elapsed_ms());
        }
    }
}

/// Return only a fixed label, including when input is malformed or contains
/// sensitive text. ICE host candidates can remain unresolved mDNS names.
pub(crate) fn address_family(address: &str) -> &'static str {
    match address.parse::<IpAddr>() {
        Ok(IpAddr::V4(_)) => "ipv4",
        Ok(IpAddr::V6(_)) => "ipv6",
        Err(_) => {
            let name = address.strip_suffix('.').unwrap_or(address);
            if !name.is_empty()
                && name.len() <= 253
                && name.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label.as_bytes()[0].is_ascii_alphanumeric()
                        && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
            {
                "hostname"
            } else {
                "unknown"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HttpOutcome {
    pub(crate) category: &'static str,
    pub(crate) status: Option<u16>,
    pub(crate) success: bool,
}
impl HttpOutcome {
    pub(crate) fn response(result: &Result<reqwest::Response, reqwest::Error>) -> Self {
        match result {
            Ok(response) => Self {
                category: if response.status().is_success() {
                    "success"
                } else {
                    "http_rejected"
                },
                status: Some(response.status().as_u16()),
                success: response.status().is_success(),
            },
            Err(error) => Self {
                category: if error.is_timeout() {
                    "timeout"
                } else if error.is_connect() {
                    "connect_failed"
                } else {
                    "transport_failed"
                },
                status: None,
                success: false,
            },
        }
    }
}

pub(crate) fn close_reason(reason: &str) -> &'static str {
    match reason {
        "Host replaced" => "host_replaced",
        "Invalid remote signaling" => "invalid_signaling",
        "Remote connection closed" => "remote_closed",
        "Screen connection unavailable" => "screen_unavailable",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn candidate_families_are_allowlisted_without_echoing_addresses() {
        for (address, expected) in [
            ("192.0.2.1", "ipv4"),
            ("2001:db8::1", "ipv6"),
            ("fixture-123.local", "hostname"),
            ("fixture.example.invalid.", "hostname"),
            ("", "unknown"),
            ("bad..local", "unknown"),
            ("-bad.local", "unknown"),
            ("https://example.invalid/?token=secret", "unknown"),
            ("a=ice-pwd:secret\n", "unknown"),
        ] {
            assert_eq!(address_family(address), expected);
        }
        assert_eq!(address_family(&"a".repeat(254)), "unknown");
    }
    #[test]
    fn untrusted_close_reasons_never_become_diagnostic_text() {
        assert_eq!(close_reason("Host replaced"), "host_replaced");
        assert_eq!(
            close_reason("Invalid remote signaling"),
            "invalid_signaling"
        );
        assert_eq!(
            close_reason("https://example.invalid/?token=secret\na=ice-pwd:secret"),
            "other"
        );
        assert_eq!(close_reason("Host replaced\nsecret"), "other");
    }
    #[test]
    fn diagnostics_remain_bounded_across_concurrent_callbacks() {
        let budget = Budget::default();
        let accepted = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..8)
                .map(|_| {
                    let budget = &budget;
                    scope.spawn(move || (0..100).filter(|_| budget.take()).count())
                })
                .collect();
            jobs.into_iter()
                .map(|job| job.join().unwrap())
                .sum::<usize>()
        });
        assert_eq!(accepted, 32);
        assert!(!budget.take());
    }
    #[tokio::test]
    async fn renewal_status_is_preserved_without_url_or_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        crate::tls::ensure_crypto_provider();
        for status in [200, 401, 403, 409, 503] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).await.unwrap();
                stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret").as_bytes()).await.unwrap();
            });
            let result = reqwest::Client::new()
                .post(format!("http://{address}/?token=secret"))
                .send()
                .await;
            let diagnostic = HttpOutcome::response(&result);
            assert_eq!(diagnostic.status, Some(status));
            assert_eq!(diagnostic.success, status == 200);
            assert!(!format!("{diagnostic:?}").contains("secret"));
            server.await.unwrap();
        }
        let result = reqwest::Client::new()
            .get("invalid://example.invalid/?token=secret")
            .send()
            .await;
        let diagnostic = HttpOutcome::response(&result);
        assert_eq!(diagnostic.status, None);
        assert!(!diagnostic.success);
        assert!(!format!("{diagnostic:?}").contains("secret"));
    }
}
