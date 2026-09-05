use std::{fmt, future::Future, pin::Pin, str::FromStr, time::Duration};

use reqwest::{
    Response, StatusCode,
    header::{ACCEPT, CONTENT_TYPE},
};
use tokio::time::sleep;

use crate::{
    ManagedClient, ManagedError, ManagedEvent,
    client::{agent_path, response_error},
};

const MAX_SSE_FRAME_BYTES: usize = 3 * 1024 * 1024;
const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MIN_RECONNECT_DELAY: Duration = Duration::from_millis(100);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

/// Validated durable event-stream cursor.
///
/// A cursor is either the literal latest position or a canonical unsigned
/// decimal string. Numeric cursors never use integer parsing, so ordering does
/// not impose an integer-width limit.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EventCursor(String);

impl EventCursor {
    /// Returns the cursor that begins at the latest durable service position.
    #[must_use]
    pub fn latest() -> Self {
        Self("latest".to_owned())
    }

    /// Parses a latest or canonical unsigned-decimal durable cursor.
    ///
    /// # Errors
    ///
    /// Returns ManagedError::Configuration for an empty, signed,
    /// non-decimal, or leading-zero numeric cursor.
    pub fn parse(value: impl Into<String>) -> Result<Self, ManagedError> {
        let value = value.into();
        if value != "latest" {
            validate_numeric_cursor(&value)?;
        }
        Ok(Self(value))
    }

    /// Returns the validated wire representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn observe(&mut self, cursor: String) -> Result<bool, ManagedError> {
        validate_numeric_cursor(&cursor)?;
        let is_new = self.0 == "latest" || cursor_before(&self.0, &cursor);
        if is_new {
            self.0 = cursor;
        }
        Ok(is_new)
    }
}

impl Default for EventCursor {
    fn default() -> Self {
        Self::latest()
    }
}

impl fmt::Display for EventCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for EventCursor {
    type Err = ManagedError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// Resumable durable server-sent event stream for one managed agent.
///
/// The stream reconnects after transient transport, rate-limit, and server
/// failures. It advances only after a complete validated event and discards
/// every incomplete response-local frame before reconnecting.
pub struct ManagedEventStream {
    client: ManagedClient,
    agent_id: String,
    cursor: EventCursor,
    reconnect_delay: Duration,
    response: Option<Response>,
    buffer: Vec<u8>,
}

/// Boxed future returned by a caller-defined managed event source.
pub type ManagedEventFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ManagedEvent, ManagedError>> + Send + 'a>>;

/// Owned durable-event transport returned by a managed Tower service.
///
/// Implementations own connection, resumption, and retry policy. This keeps
/// the lifecycle driver independent from HTTP, SSE, WebSocket, or test
/// transports while preserving one canonical cursor contract.
pub trait ManagedEventSource: Send + 'static {
    /// Returns the last completely observed durable cursor.
    fn cursor(&self) -> &EventCursor;

    /// Waits for the next strictly newer durable event.
    fn next(&mut self) -> ManagedEventFuture<'_>;
}

/// Type-erased durable event transport consumed by the managed lifecycle.
pub struct ManagedEvents {
    source: Box<dyn ManagedEventSource>,
}

impl ManagedEvents {
    /// Erases one concrete durable event source after a Tower service opens it.
    #[must_use]
    pub fn new(source: impl ManagedEventSource) -> Self {
        Self {
            source: Box::new(source),
        }
    }

    /// Returns the last completely observed durable cursor.
    #[must_use]
    pub fn cursor(&self) -> &EventCursor {
        self.source.cursor()
    }

    /// Waits for the next strictly newer durable event.
    pub async fn next(&mut self) -> Result<ManagedEvent, ManagedError> {
        self.source.next().await
    }
}

impl fmt::Debug for ManagedEvents {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedEvents")
            .field("cursor", self.cursor())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ManagedEventStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedEventStream")
            .field("agent_id", &self.agent_id)
            .field("cursor", &self.cursor)
            .field("reconnect_delay", &self.reconnect_delay)
            .finish_non_exhaustive()
    }
}

impl ManagedEventStream {
    pub(crate) const fn new(client: ManagedClient, agent_id: String, cursor: EventCursor) -> Self {
        Self {
            client,
            agent_id,
            cursor,
            reconnect_delay: DEFAULT_RECONNECT_DELAY,
            response: None,
            buffer: Vec::new(),
        }
    }

    /// Returns the last completely observed durable cursor.
    #[must_use]
    pub const fn cursor(&self) -> &EventCursor {
        &self.cursor
    }

    /// Waits for and returns the next strictly newer durable event.
    ///
    /// Duplicate and stale cursors are discarded. A response-local partial
    /// frame is discarded before reconnecting, preventing frame splicing.
    ///
    /// # Errors
    ///
    /// Returns a terminal HTTP error or a malformed, mismatched, or oversized
    /// SSE frame.
    pub async fn next(&mut self) -> Result<ManagedEvent, ManagedError> {
        loop {
            if let Some(frame) = take_sse_frame(&mut self.buffer)? {
                let parsed = parse_sse_frame(&frame)?;
                if let Some(delay) = parsed.retry {
                    self.reconnect_delay = delay;
                }
                let Some(data) = parsed.data else {
                    if let Some(control_cursor) = parsed.control_cursor {
                        self.cursor.observe(control_cursor)?;
                    }
                    continue;
                };
                let event: ManagedEvent = serde_json::from_str(&data).map_err(|error| {
                    ManagedError::InvalidEvent(format!("event data is not typed JSON: {error}"))
                })?;
                let id = parsed.id.ok_or_else(|| {
                    ManagedError::InvalidEvent(
                        "data-bearing SSE frame is missing a durable id".to_owned(),
                    )
                })?;
                if event.cursor != id {
                    return Err(ManagedError::InvalidEvent(
                        "SSE id does not match the durable event cursor".to_owned(),
                    ));
                }
                let event_name = parsed.event.ok_or_else(|| {
                    ManagedError::InvalidEvent(
                        "data-bearing SSE frame is missing an event name".to_owned(),
                    )
                })?;
                if event_name != event.data.event_name() {
                    return Err(ManagedError::InvalidEvent(format!(
                        "SSE event name {event_name:?} does not match envelope type {:?}",
                        event.data.event_name()
                    )));
                }
                if let Some(control_cursor) = parsed.control_cursor {
                    if !cursor_before(&control_cursor, &id) {
                        return Err(ManagedError::InvalidEvent(
                            "SSE control cursor does not precede the event cursor".to_owned(),
                        ));
                    }
                    self.cursor.observe(control_cursor)?;
                }
                if !self.cursor.observe(id)? {
                    continue;
                }
                return Ok(event);
            }

            if self.response.is_none() {
                self.connect().await?;
            }
            let chunk = match self.response.as_mut() {
                Some(response) => response.chunk().await,
                None => continue,
            };
            match chunk {
                Ok(Some(bytes)) => {
                    self.buffer.extend_from_slice(&bytes);
                    validate_buffer_bound(&self.buffer)?;
                }
                Ok(None) | Err(_) => {
                    self.response = None;
                    self.buffer.clear();
                    sleep(self.reconnect_delay).await;
                }
            }
        }
    }

    async fn connect(&mut self) -> Result<(), ManagedError> {
        let mut url = self
            .client
            .url(&format!("{}/events", agent_path(&self.agent_id)))?;
        url.query_pairs_mut()
            .append_pair("cursor", self.cursor.as_str());
        loop {
            match self
                .client
                .http
                .get(url.clone())
                .header(ACCEPT, "text/event-stream")
                .send()
                .await
            {
                Ok(response) if response.status() == StatusCode::OK => {
                    let is_event_stream = response
                        .headers()
                        .get(CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.split(';').next())
                        .is_some_and(|value| {
                            value.trim().eq_ignore_ascii_case("text/event-stream")
                        });
                    if !is_event_stream {
                        return Err(ManagedError::InvalidEvent(
                            "managed event response is not text/event-stream".to_owned(),
                        ));
                    }
                    self.response = Some(response);
                    return Ok(());
                }
                Ok(response)
                    if response.status() == StatusCode::TOO_MANY_REQUESTS
                        || response.status().is_server_error() =>
                {
                    drop(response);
                }
                Ok(response) => return Err(response_error(response).await),
                Err(_) => {}
            }
            sleep(self.reconnect_delay).await;
        }
    }
}

impl ManagedEventSource for ManagedEventStream {
    fn cursor(&self) -> &EventCursor {
        self.cursor()
    }

    fn next(&mut self) -> ManagedEventFuture<'_> {
        Box::pin(self.next())
    }
}

struct ParsedSseFrame {
    id: Option<String>,
    event: Option<String>,
    retry: Option<Duration>,
    control_cursor: Option<String>,
    data: Option<String>,
}

fn take_sse_frame(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>, ManagedError> {
    validate_buffer_bound(buffer)?;
    let Some((index, delimiter)) = find_sse_boundary(buffer) else {
        return Ok(None);
    };
    let frame = buffer[..index].to_vec();
    buffer.drain(..index + delimiter);
    Ok(Some(frame))
}

fn validate_buffer_bound(buffer: &[u8]) -> Result<(), ManagedError> {
    let mut remaining = buffer;
    loop {
        match find_sse_boundary(remaining) {
            Some((index, delimiter)) => {
                if index > MAX_SSE_FRAME_BYTES {
                    return Err(oversized_frame());
                }
                remaining = &remaining[index + delimiter..];
            }
            None if remaining.len() > MAX_SSE_FRAME_BYTES => return Err(oversized_frame()),
            None => return Ok(()),
        }
    }
}

fn oversized_frame() -> ManagedError {
    ManagedError::InvalidEvent(format!("SSE frame exceeded {MAX_SSE_FRAME_BYTES} bytes"))
}

fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    for (index, window) in buffer.windows(2).enumerate() {
        if matches!(window, b"\n\n" | b"\r\r") {
            return Some((index, 2));
        }
    }
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4))
}

fn parse_sse_frame(frame: &[u8]) -> Result<ParsedSseFrame, ManagedError> {
    let frame = std::str::from_utf8(frame)
        .map_err(|_| ManagedError::InvalidEvent("SSE frame is not UTF-8".to_owned()))?;
    let normalized = frame.replace("\r\n", "\n").replace('\r', "\n");
    let mut id = None;
    let mut event = None;
    let mut retry = None;
    let mut control_cursor = None;
    let mut data = Vec::new();
    for line in normalized.lines() {
        if let Some(comment) = line.strip_prefix(':') {
            if let Some(cursor) = comment.trim_start().strip_prefix("cursor ") {
                validate_numeric_cursor(cursor)?;
                control_cursor = Some(cursor.to_owned());
            }
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "id" if !value.contains('\0') => {
                validate_numeric_cursor(value)?;
                id = Some(value.to_owned());
            }
            "event" if !value.contains(['\r', '\n', '\0']) => {
                event = Some(value.to_owned());
            }
            "retry" if value.bytes().all(|byte| byte.is_ascii_digit()) => {
                if let Ok(milliseconds) = value.parse::<u64>() {
                    retry = Some(
                        Duration::from_millis(milliseconds)
                            .clamp(MIN_RECONNECT_DELAY, MAX_RECONNECT_DELAY),
                    );
                }
            }
            "data" => data.push(value),
            _ => {}
        }
    }
    Ok(ParsedSseFrame {
        id,
        event,
        retry,
        control_cursor,
        data: (!data.is_empty()).then(|| data.join("\n")),
    })
}

pub(crate) fn validate_numeric_cursor(value: &str) -> Result<(), ManagedError> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(ManagedError::Configuration(
            "managed cursor must be an unsigned decimal string".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn cursor_before(left: &str, right: &str) -> bool {
    left.len() < right.len() || (left.len() == right.len() && left < right)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Router,
        body::Body,
        extract::{Query, State},
        http::{HeaderMap, Response, StatusCode},
        response::IntoResponse,
        routing::get,
    };

    use super::{
        EventCursor, MAX_RECONNECT_DELAY, MAX_SSE_FRAME_BYTES, MIN_RECONNECT_DELAY,
        parse_sse_frame, take_sse_frame,
    };
    use crate::{ManagedApiKey, ManagedClient, ManagedError, ManagedEventData};

    fn key() -> String {
        format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))
    }

    #[test]
    fn parses_multiline_sse_and_clamps_retry() {
        let frame = b": cursor 7\r\nid: 8\r\nevent: turn_cancelled\r\nretry: 25\r\ndata: {\"cursor\":\"8\",\r\ndata: \"type\":\"turn_cancelled\",\"id\":\"turn-1\"}\r\n";
        let parsed = parse_sse_frame(frame).unwrap();
        assert_eq!(parsed.id.as_deref(), Some("8"));
        assert_eq!(parsed.event.as_deref(), Some("turn_cancelled"));
        assert_eq!(parsed.control_cursor.as_deref(), Some("7"));
        assert_eq!(parsed.retry, Some(MIN_RECONNECT_DELAY));
        let data: serde_json::Value =
            serde_json::from_str(parsed.data.as_deref().unwrap()).unwrap();
        assert_eq!(data["type"], "turn_cancelled");
        assert_eq!(
            parse_sse_frame(b"retry: 999999\n").unwrap().retry,
            Some(MAX_RECONNECT_DELAY)
        );
    }

    #[test]
    fn decimal_cursor_order_does_not_use_integer_width() {
        let mut cursor = EventCursor::parse("9").unwrap();
        assert!(cursor.observe("10".to_owned()).unwrap());
        assert!(!cursor.observe("10".to_owned()).unwrap());
        assert!(!cursor.observe("2".to_owned()).unwrap());
        assert!(EventCursor::parse("01").is_err());
    }

    #[test]
    fn sse_frame_bound_is_three_mebibytes_and_per_frame() {
        let mut exact = vec![b'x'; MAX_SSE_FRAME_BYTES];
        exact.extend_from_slice(b"\n\n");
        assert_eq!(
            take_sse_frame(&mut exact).unwrap().unwrap().len(),
            MAX_SSE_FRAME_BYTES
        );
        let mut oversized = vec![b'x'; MAX_SSE_FRAME_BYTES + 1];
        assert!(matches!(
            take_sse_frame(&mut oversized),
            Err(ManagedError::InvalidEvent(_))
        ));
        let mut two_frames = vec![b'x'; 2 * 1024 * 1024];
        two_frames.extend_from_slice(b"\n\n");
        two_frames.extend(std::iter::repeat_n(b'y', 2 * 1024 * 1024));
        two_frames.extend_from_slice(b"\n\n");
        assert!(take_sse_frame(&mut two_frames).unwrap().is_some());
        assert!(take_sse_frame(&mut two_frames).unwrap().is_some());

        let mut complete_then_oversized_tail = b"id: 1\n\n".to_vec();
        complete_then_oversized_tail.extend(std::iter::repeat_n(b'z', MAX_SSE_FRAME_BYTES + 1));
        assert!(matches!(
            take_sse_frame(&mut complete_then_oversized_tail),
            Err(ManagedError::InvalidEvent(_))
        ));
    }

    #[derive(Clone)]
    struct StreamState {
        expected_authorization: String,
        cursors: Arc<Mutex<Vec<String>>>,
    }

    #[tokio::test]
    async fn reconnects_strictly_after_last_cursor_and_types_event_name() {
        let secret = key();
        let state = StreamState {
            expected_authorization: format!("Bearer {secret}"),
            cursors: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/v1/agents/{agent_id}/events", get(stream_events))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(secret.clone()).unwrap(),
        )
        .unwrap();
        let mut events = client
            .events("agent-1", EventCursor::parse("1").unwrap())
            .unwrap();
        let first = events.next().await.unwrap();
        assert_eq!(first.cursor, "2");
        assert_eq!(first.data.agent_event().unwrap().unwrap().seq, 1);
        let second = events.next().await.unwrap();
        assert_eq!(second.cursor, "3");
        assert_eq!(
            second.data.terminal_result("turn-1").unwrap().unwrap(),
            "done"
        );
        assert_eq!(
            state.cursors.lock().unwrap().as_slice(),
            ["1".to_owned(), "2".to_owned()]
        );
        assert!(!format!("{client:?}").contains(&secret));
        server.abort();
    }

    #[derive(Clone)]
    struct PartialState {
        attempts: Arc<AtomicUsize>,
    }

    #[tokio::test]
    async fn reconnect_discards_partial_response_buffer() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/v1/agents/{agent_id}/events", get(partial_events))
            .with_state(PartialState {
                attempts: Arc::clone(&attempts),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(key()).unwrap(),
        )
        .unwrap();
        let mut events = client
            .events("agent-1", EventCursor::parse("1").unwrap())
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(3), events.next())
            .await
            .expect("managed event replay timed out")
            .unwrap();
        assert_eq!(event.cursor, "2");
        assert!(matches!(event.data, ManagedEventData::Event { .. }));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn rejects_successful_responses_with_the_wrong_media_type() {
        let app = Router::new().route(
            "/v1/agents/{agent_id}/events",
            get(|| async {
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(key()).unwrap(),
        )
        .unwrap();
        let mut events = client
            .events("agent-1", EventCursor::parse("1").unwrap())
            .unwrap();

        let error = events.next().await.unwrap_err();
        assert!(matches!(error, ManagedError::InvalidEvent(_)));
        server.abort();
    }

    async fn stream_events(
        State(state): State<StreamState>,
        headers: HeaderMap,
        Query(query): Query<HashMap<String, String>>,
    ) -> impl IntoResponse {
        if headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            != Some(state.expected_authorization.as_str())
        {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Body::from("unauthorized"))
                .unwrap();
        }
        let cursor = query.get("cursor").cloned().unwrap_or_default();
        state.cursors.lock().unwrap().push(cursor.clone());
        let body = match cursor.as_str() {
            "1" => concat!(
                "id: 2\n",
                "event: event\n",
                "data: {\"cursor\":\"2\",\"created_at\":1,\"turn_id\":\"turn-1\",",
                "\"type\":\"event\",\"event\":{\"protocol_version\":1,",
                "\"request_id\":\"request-1\",\"seq\":1,",
                "\"type\":\"assistant.delta\",\"payload\":{\"delta\":\"hi\"}}}\n\n"
            ),
            "2" => concat!(
                "id: 3\n",
                "event: turn_completed\n",
                "data: {\"cursor\":\"3\",\"created_at\":2,\"turn_id\":\"turn-1\",",
                "\"type\":\"turn_completed\",\"id\":\"turn-1\",",
                "\"final_message\":\"done\",\"usage\":null,\"citations\":[],",
                "\"usage_error\":null}\n\n"
            ),
            _ => "",
        };
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from(body))
            .unwrap()
    }

    async fn partial_events(State(state): State<PartialState>) -> impl IntoResponse {
        let attempt = state.attempts.fetch_add(1, Ordering::SeqCst);
        let body = if attempt == 0 {
            "id: 2\nevent: event\ndata: {\"cursor\":\"2\""
        } else {
            concat!(
                "id: 2\n",
                "event: event\n",
                "data: {\"cursor\":\"2\",\"created_at\":1,\"turn_id\":\"turn-1\",",
                "\"type\":\"event\",\"event\":{\"protocol_version\":1,",
                "\"request_id\":\"request-1\",\"seq\":1,",
                "\"type\":\"assistant.delta\",\"payload\":{\"delta\":\"hi\"}}}\n\n"
            )
        };
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from(body))
            .unwrap()
    }
}
