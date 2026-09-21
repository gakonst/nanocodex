//! Account discovery diagnostics deliberately exclude URLs, bodies and credentials.
use super::{ManagedError, error};
use reqwest::{Client, StatusCode, header::HeaderMap};
use serde_json::Value;
use std::time::Duration;

const ATTEMPTS: usize = 3;
const MAX_DELAY: Duration = Duration::from_secs(2);

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some(
        (date.with_timezone(&chrono::Utc) - chrono::Utc::now())
            .to_std()
            .unwrap_or_default(),
    )
}

fn diagnostic(status: Option<StatusCode>, headers: &HeaderMap, attempt: usize) -> String {
    let summary = match status.map(|s| s.as_u16()) {
        Some(401) => "Computer Hand authentication expired or invalid; run nanocodex2 login",
        Some(403) => "Computer Hand account access denied; check account permissions",
        Some(429) => "Computer Hand account rate limited; try again later",
        Some(500..=599) => "Computer Hand account service unavailable; try again later",
        None => "Computer Hand account connection failed; check network access and try again",
        _ => "Computer Hand account request rejected",
    };
    // Only bounded, token-shaped correlation IDs are useful for support. Never
    // print arbitrary response headers or a reqwest error (which can carry URLs).
    let request_id = ["x-request-id", "cf-ray"]
        .iter()
        .find_map(|name| {
            let value = headers.get(*name)?.to_str().ok()?;
            (!value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
            .then_some(value)
        })
        .unwrap_or("unavailable");
    format!(
        "{summary} [time={} status={} request_id={request_id} version={} build={} attempts={attempt}]",
        chrono::Utc::now().to_rfc3339(),
        status.map_or_else(|| "transport".into(), |s| s.as_u16().to_string()),
        env!("CARGO_PKG_VERSION"),
        option_env!("VERGEN_GIT_SHA").unwrap_or("unknown")
    )
}

pub(super) async fn identify(
    client: &Client,
    origin: &str,
    key: &str,
) -> Result<Value, ManagedError> {
    for attempt in 1..=ATTEMPTS {
        let result = client
            .get(format!("{origin}/v1/me"))
            .bearer_auth(key)
            .send()
            .await;
        let (status, headers, retryable) = match result {
            Ok(response) if response.status().is_success() => {
                return response
                    .json()
                    .await
                    .map_err(|_| error("Invalid Hand account response"));
            }
            Ok(response) => {
                let status = response.status();
                let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                (Some(status), response.headers().clone(), retryable)
            }
            Err(e) => (
                None,
                HeaderMap::new(),
                e.is_connect() || e.is_timeout() || e.is_body() || e.is_request(),
            ),
        };
        let delay = retry_after(&headers).unwrap_or(Duration::from_millis(250 * attempt as u64));
        // A long server-directed wait ends this background attempt instead of
        // retrying too soon or holding startup indefinitely. Emit only one error.
        if !retryable || attempt == ATTEMPTS || delay > MAX_DELAY {
            return Err(error(diagnostic(status, &headers, attempt)));
        }
        tokio::time::sleep(delay).await;
    }
    unreachable!("bounded attempts always return")
}

#[cfg(test)]
mod tests;
