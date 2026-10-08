//! Credential-isolated access to one public shared thread.
use crate::{EventCursor, ManagedError, ManagedEvent, SharePermission, TurnState};
use reqwest::{
    Method, Response, StatusCode,
    header::{CONTENT_TYPE, HeaderValue},
};
use serde::Deserialize;
use serde_json::Value;
use std::{fmt, time::Duration};
use tokio::time::{Instant, sleep, timeout_at};
use url::{Host, Url};

/// Public metadata for a shared conversation.
#[derive(Debug, Deserialize)]
pub struct SharedThreadMetadata {
    /// Thread identifier.
    pub agent_id: String,
    /// Conversation title.
    pub title: String,
    /// Bearer permission.
    pub permission: SharePermission,
    /// Snapshot cursor from which to begin live replay.
    pub latest_event_cursor: String,
}

/// A chronological guest history page, including a cursor across hidden events.
#[derive(Debug)]
pub struct SharedHistoryPage {
    /// Visible conversation events.
    pub data: Vec<ManagedEvent>,
    /// Whether the archive contains older events.
    pub has_more: bool,
    /// Cursor for older events, even when no visible events were returned.
    pub next_cursor: Option<String>,
    /// Latest durable event at the time of the history read.
    pub latest_cursor: String,
}

/// Admission evidence for a submitted guest turn.
#[derive(Debug, Deserialize)]
pub struct SharedTurnReceipt {
    /// Caller-selected durable turn identity.
    pub turn_id: String,
    /// Current durable turn state.
    pub state: TurnState,
    /// Acceptance event cursor.
    pub accepted_cursor: String,
}

/// Cookie-free, redirect-disabled guest transport. Debug never reveals the bearer.
#[derive(Clone)]
pub struct SharedThreadClient {
    http: reqwest::Client,
    base: Url,
    origin: String,
    agent_id: String,
    bearer: HeaderValue,
}
impl fmt::Debug for SharedThreadClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedThreadClient")
            .field("agent_id", &self.agent_id)
            .finish_non_exhaustive()
    }
}
fn invalid_url() -> ManagedError {
    ManagedError::Configuration("invalid shared thread URL".into())
}
fn http_error(status: StatusCode) -> ManagedError {
    // Never echo an untrusted error body, which could contain the bearer.
    ManagedError::Http {
        status,
        code: "shared_thread_request_failed".into(),
        message: "Shared thread access failed".into(),
    }
}
impl SharedThreadClient {
    /// Parses a fragment-bearing share URL without consulting account credentials.
    pub fn from_url(value: &str) -> Result<Self, ManagedError> {
        let mut url = Url::parse(value).map_err(|_| invalid_url())?;
        let loopback = matches!(url.host(), Some(Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(url.host(), Some(Host::Ipv6(ip)) if ip.is_loopback())
            || matches!(url.host(), Some(Host::Domain(host)) if host == "localhost" || host.ends_with(".localhost"));
        if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || value.contains('\\')
            || value.chars().any(char::is_control)
        {
            return Err(invalid_url());
        }
        let id = url
            .path()
            .strip_prefix("/share/")
            .ok_or_else(invalid_url)?
            .trim_end_matches('/');
        let parsed = uuid::Uuid::parse_str(id).map_err(|_| invalid_url())?;
        if parsed.to_string() != id {
            return Err(invalid_url());
        }
        let agent_id = id.to_owned();
        let token = url
            .fragment()
            .and_then(|v| v.strip_prefix("token="))
            .ok_or_else(invalid_url)?;
        let secret = token.strip_prefix("nsl_").ok_or_else(invalid_url)?;
        if secret.len() != 43
            || !secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(invalid_url());
        }
        let mut bearer =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| invalid_url())?;
        bearer.set_sensitive(true);
        let origin = url.origin().ascii_serialization();
        url.set_fragment(None);
        url.set_path(&format!("/v1/shared/{agent_id}"));
        crate::client::install_default_rustls_crypto_provider();
        let http = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .build()
            .map_err(ManagedError::Transport)?;
        Ok(Self {
            http,
            base: url,
            origin,
            agent_id,
            bearer,
        })
    }
    /// Identifier of the shared conversation.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }
    fn url(&self, suffix: &str) -> Url {
        let mut url = self.base.clone();
        url.set_path(&format!("{}{}", self.base.path(), suffix));
        url
    }
    fn request(&self, method: Method, url: Url) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            .header("authorization", self.bearer.clone())
            .header("cache-control", "no-store")
    }
    async fn json(&self, url: Url) -> Result<Value, ManagedError> {
        let response = self
            .request(Method::GET, url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(ManagedError::Transport)?;
        read_json(response).await
    }
    /// Reads current access and the live replay starting cursor.
    pub async fn metadata(&self) -> Result<SharedThreadMetadata, ManagedError> {
        let metadata: SharedThreadMetadata =
            serde_json::from_value(self.json(self.base.clone()).await?)
                .map_err(|_| ManagedError::InvalidResponse("invalid shared metadata"))?;
        if metadata.agent_id != self.agent_id {
            return Err(ManagedError::InvalidResponse(
                "shared thread identity mismatch",
            ));
        }
        crate::sse::validate_numeric_cursor(&metadata.latest_event_cursor)?;
        Ok(metadata)
    }
    /// Reads one guest history page. Empty pages may still have a next cursor
    /// because private owner events are removed by the shared projection.
    pub async fn history(
        &self,
        before: Option<&str>,
        limit: u16,
    ) -> Result<SharedHistoryPage, ManagedError> {
        if !(1..=256).contains(&limit) {
            return Err(ManagedError::Configuration(
                "history limit must be 1 through 256".into(),
            ));
        }
        if let Some(cursor) = before {
            crate::sse::validate_numeric_cursor(cursor)?;
            if cursor == "0" {
                return Err(ManagedError::Configuration(
                    "history cursor must be positive".into(),
                ));
            }
        }
        let mut url = self.url("/events/history");
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string());
        if let Some(cursor) = before {
            url.query_pairs_mut().append_pair("before", cursor);
        }
        #[derive(Deserialize)]
        struct Page {
            data: Vec<Value>,
            has_more: bool,
            latest_cursor: String,
            next_cursor: Option<String>,
        }
        let page: Page = serde_json::from_value(self.json(url).await?)
            .map_err(|_| ManagedError::InvalidResponse("invalid shared history"))?;
        crate::sse::validate_numeric_cursor(&page.latest_cursor)?;
        if page.data.len() > usize::from(limit) {
            return Err(ManagedError::InvalidResponse(
                "oversized shared history page",
            ));
        }
        let data = page
            .data
            .into_iter()
            .map(decode_event)
            .collect::<Result<Vec<_>, _>>()?;
        let mut previous: Option<&str> = None;
        for event in &data {
            if previous.is_some_and(|p| !crate::sse::cursor_before(p, &event.cursor))
                || before.is_some_and(|p| !crate::sse::cursor_before(&event.cursor, p))
                || crate::sse::cursor_before(&page.latest_cursor, &event.cursor)
            {
                return Err(ManagedError::InvalidResponse("unordered shared history"));
            }
            previous = Some(&event.cursor);
        }
        if page.has_more {
            let next = page
                .next_cursor
                .as_deref()
                .ok_or(ManagedError::InvalidResponse(
                    "missing shared history cursor",
                ))?;
            crate::sse::validate_numeric_cursor(next)?;
            if next == "0"
                || before.is_some_and(|p| !crate::sse::cursor_before(next, p))
                || data
                    .first()
                    .is_some_and(|first| crate::sse::cursor_before(&first.cursor, next))
            {
                return Err(ManagedError::InvalidResponse(
                    "nonprogressing shared history cursor",
                ));
            }
        }
        Ok(SharedHistoryPage {
            data,
            has_more: page.has_more,
            next_cursor: page.next_cursor,
            latest_cursor: page.latest_cursor,
        })
    }
    /// Opens a resumable guest stream using the public `after` query parameter.
    pub fn events(&self, cursor: EventCursor) -> SharedEventStream {
        SharedEventStream {
            client: self.clone(),
            cursor,
            response: None,
            buffer: Vec::new(),
            search_from: 0,
            delay: Duration::from_secs(1),
            terminal: None,
        }
    }
    /// Submits one guest prompt with a stable identity; does not automatically retry.
    pub async fn submit(&self, text: &str, id: &str) -> Result<SharedTurnReceipt, ManagedError> {
        if self.metadata().await?.permission != SharePermission::Write {
            return Err(http_error(StatusCode::FORBIDDEN));
        }
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
            || text.trim().is_empty()
        {
            return Err(ManagedError::Configuration(
                "invalid shared prompt or turn identity".into(),
            ));
        }
        let body = serde_json::to_vec(&serde_json::json!({"id":id,"input":text}))
            .map_err(|_| ManagedError::InvalidResponse("invalid shared prompt"))?;
        if body.len() > 32_768 {
            return Err(ManagedError::Configuration(
                "shared prompt exceeds 32768 bytes".into(),
            ));
        }
        let response = self
            .request(Method::POST, self.url("/turns"))
            .header("origin", &self.origin)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(ManagedError::Transport)?;
        let receipt: SharedTurnReceipt = serde_json::from_value(read_json(response).await?)
            .map_err(|_| ManagedError::InvalidResponse("invalid shared turn receipt"))?;
        if receipt.turn_id != id {
            return Err(ManagedError::InvalidResponse(
                "shared turn receipt identity mismatch",
            ));
        }
        crate::sse::validate_numeric_cursor(&receipt.accepted_cursor)?;
        Ok(receipt)
    }
}
async fn read_json(mut response: Response) -> Result<Value, ManagedError> {
    if !response.status().is_success() {
        return Err(http_error(response.status()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(ManagedError::Transport)? {
        if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
            return Err(ManagedError::InvalidResponse("shared response too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| ManagedError::InvalidResponse("invalid shared JSON"))
}
fn decode_event(mut value: Value) -> Result<ManagedEvent, ManagedError> {
    // Shared projection intentionally omits owner-only replay and citation metadata.
    match value.get("type").and_then(Value::as_str) {
        Some("turn_accepted") => {
            value["replayed"] = Value::Bool(false);
        }
        Some("turn_completed") => {
            value["citations"] = Value::Array(Vec::new());
        }
        _ => {}
    }
    let event: ManagedEvent = serde_json::from_value(value)
        .map_err(|_| ManagedError::InvalidResponse("invalid shared event"))?;
    crate::sse::validate_numeric_cursor(&event.cursor)?;
    Ok(event)
}

/// Guest SSE connection with resumable cursors and terminal access failures.
pub struct SharedEventStream {
    client: SharedThreadClient,
    cursor: EventCursor,
    response: Option<(Response, Instant)>,
    buffer: Vec<u8>,
    search_from: usize,
    delay: Duration,
    terminal: Option<StatusCode>,
}
impl fmt::Debug for SharedEventStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedEventStream")
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}
impl SharedEventStream {
    /// Last fully observed event cursor.
    pub fn cursor(&self) -> &EventCursor {
        &self.cursor
    }
    /// Returns the next strictly newer projected event, reconnecting transient failures.
    pub async fn next(&mut self) -> Result<ManagedEvent, ManagedError> {
        if let Some(status) = self.terminal {
            return Err(http_error(status));
        }
        loop {
            if let Some(frame) = crate::sse::take_sse_frame(&mut self.buffer, &mut self.search_from)
            {
                if frame.len() > 8 * 1024 * 1024 {
                    return Err(ManagedError::InvalidResponse("shared SSE frame too large"));
                }
                let parsed = crate::sse::parse_sse_frame(&frame)?;
                if let Some(delay) = parsed.retry {
                    self.delay = delay;
                }
                if let Some(data) = parsed.data {
                    let event =
                        decode_event(serde_json::from_str(&data).map_err(|_| {
                            ManagedError::InvalidResponse("invalid shared SSE JSON")
                        })?)?;
                    if parsed.id.as_deref() != Some(event.cursor.as_str())
                        || parsed.event.as_deref() != Some(event.data.event_name())
                    {
                        return Err(ManagedError::InvalidResponse(
                            "shared SSE envelope mismatch",
                        ));
                    }
                    if self.cursor.observe(event.cursor.clone())? {
                        return Ok(event);
                    }
                } else if let Some(cursor) = parsed.control_cursor {
                    self.cursor.observe(cursor)?;
                }
                continue;
            }
            if self.buffer.len() > 8 * 1024 * 1024 {
                return Err(ManagedError::InvalidResponse("shared SSE frame too large"));
            }
            if self.response.is_none() {
                let mut url = self.client.url("/events");
                url.query_pairs_mut()
                    .append_pair("after", self.cursor.as_str());
                match self
                    .client
                    .request(Method::GET, url)
                    .header("accept", "text/event-stream")
                    .send()
                    .await
                {
                    Ok(response) if response.status() == StatusCode::OK => {
                        if !response
                            .headers()
                            .get(CONTENT_TYPE)
                            .and_then(|v| v.to_str().ok())
                            .is_some_and(|v| {
                                v.split(';').next().is_some_and(|v| {
                                    v.trim().eq_ignore_ascii_case("text/event-stream")
                                })
                            })
                        {
                            return Err(ManagedError::InvalidResponse(
                                "shared response is not SSE",
                            ));
                        }
                        self.delay = Duration::from_secs(1);
                        self.response = Some((response, Instant::now() + Duration::from_secs(60)));
                    }
                    Ok(response)
                        if response.status() == StatusCode::TOO_MANY_REQUESTS
                            || response.status().is_server_error() =>
                    {
                        let retry_after = response
                            .headers()
                            .get("retry-after")
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.parse::<u64>().ok())
                            .map(Duration::from_secs)
                            .unwrap_or(self.delay);
                        sleep(retry_after.max(self.delay)).await;
                        self.delay = (self.delay * 2).min(Duration::from_secs(30));
                        continue;
                    }
                    Ok(response) => {
                        self.terminal = Some(response.status());
                        return Err(http_error(response.status()));
                    }
                    Err(_) => {
                        sleep(self.delay).await;
                        self.delay = (self.delay * 2).min(Duration::from_secs(30));
                        continue;
                    }
                }
            }
            let (response, deadline) = self
                .response
                .as_mut()
                .ok_or(ManagedError::InvalidResponse("missing shared SSE response"))?;
            match timeout_at(*deadline, response.chunk()).await {
                Ok(Ok(Some(bytes))) => self.buffer.extend_from_slice(&bytes),
                _ => {
                    self.response = None;
                    self.buffer.clear();
                    self.search_from = 0;
                    sleep(self.delay).await;
                }
            }
        }
    }
}
