#[cfg(not(target_family = "wasm"))]
use std::time::Duration;

use crate::{OpenAiAuthSnapshot, monotonic_now_ns};
use http::header;

use crate::{EncodedRequest, ResponsesError, socket::ReceivedText};

#[cfg(not(target_family = "wasm"))]
const RESPONSES_LITE_HEADER: &str = "x-openai-internal-codex-responses-lite";
#[cfg(not(target_family = "wasm"))]
const TURN_STATE_HEADER: &str = "x-codex-turn-state";

#[derive(Clone)]
pub(crate) struct ResponsesHttp {
    #[cfg(not(target_family = "wasm"))]
    client: reqwest::Client,
    #[cfg(target_family = "wasm")]
    host: Option<std::sync::Arc<dyn crate::transport::host::HostTransport>>,
}

pub(crate) struct ResponsesHttpStream {
    #[cfg(not(target_family = "wasm"))]
    response: reqwest::Response,
    #[cfg(target_family = "wasm")]
    response: Box<dyn crate::transport::host::HostHttpBody>,
    decoder: SseDecoder,
    ended: bool,
}

pub(crate) struct HttpMetadata {
    pub(crate) reasoning_included: bool,
    pub(crate) turn_state: Option<String>,
}

#[cfg(not(target_family = "wasm"))]
impl ResponsesHttp {
    pub(crate) const fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    pub(crate) async fn send(
        &self,
        api_base_url: &str,
        auth: &OpenAiAuthSnapshot,
        session_id: &str,
        thread_id: &str,
        turn_state: Option<&str>,
        request: &EncodedRequest,
    ) -> Result<(ResponsesHttpStream, HttpMetadata), ResponsesError> {
        let endpoint = format!("{}/responses", api_base_url.trim_end_matches('/'));
        let mut builder = self
            .client
            .post(endpoint)
            .bearer_auth(auth.bearer())
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "text/event-stream")
            .header(RESPONSES_LITE_HEADER, "true")
            .header("session-id", session_id)
            .header("thread-id", thread_id)
            .header("x-client-request-id", thread_id)
            .header(
                header::USER_AGENT,
                concat!("nanocodex/", env!("CARGO_PKG_VERSION")),
            )
            .body(request.raw().get().to_owned());
        if let Some(account_id) = auth.account_id() {
            builder = builder.header("ChatGPT-Account-ID", account_id);
        }
        if auth.is_fedramp() {
            builder = builder.header("X-OpenAI-Fedramp", "true");
        }
        if let Some(turn_state) =
            turn_state.and_then(|value| header::HeaderValue::from_bytes(value.as_bytes()).ok())
        {
            builder = builder.header(TURN_STATE_HEADER, turn_state);
        }
        let response = builder.send().await.map_err(map_http_error)?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = retry_after(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(ResponsesError::HttpRejected {
                status: status.as_u16(),
                body,
                retry_after,
            });
        }
        let metadata = HttpMetadata {
            reasoning_included: response.headers().contains_key("x-reasoning-included"),
            turn_state: response
                .headers()
                .get(TURN_STATE_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        };
        Ok((
            ResponsesHttpStream {
                response,
                decoder: SseDecoder::default(),
                ended: false,
            },
            metadata,
        ))
    }
}

impl ResponsesHttpStream {
    pub(crate) async fn next_text(&mut self) -> Result<ReceivedText, ResponsesError> {
        loop {
            if let Some(text) = self.decoder.next()? {
                return Ok(ReceivedText {
                    #[cfg(not(target_family = "wasm"))]
                    text: text.into(),
                    #[cfg(target_family = "wasm")]
                    text,
                    received_ns: monotonic_now_ns(),
                });
            }
            if self.ended {
                return Err(ResponsesError::UnexpectedEnd);
            }
            #[cfg(not(target_family = "wasm"))]
            let chunk = self.response.chunk().await.map_err(map_http_error)?;
            #[cfg(target_family = "wasm")]
            let chunk = self.response.next().await.map_err(map_host_error)?;
            if let Some(chunk) = chunk {
                self.decoder.push(&chunk);
            } else {
                self.ended = true;
                self.decoder.finish();
            }
        }
    }
}

#[derive(Default)]
struct SseDecoder {
    bytes: Vec<u8>,
    cursor: usize,
    data: Vec<String>,
    finished: bool,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) {
        self.compact();
        self.bytes.extend_from_slice(chunk);
    }

    fn finish(&mut self) {
        self.finished = true;
        self.compact();
        if !self.bytes.is_empty() {
            self.bytes.push(b'\n');
        }
        self.bytes.push(b'\n');
    }

    fn next(&mut self) -> Result<Option<String>, ResponsesError> {
        loop {
            let Some(relative_newline) = self.bytes[self.cursor..]
                .iter()
                .position(|byte| *byte == b'\n')
            else {
                return Ok(None);
            };
            let line_start = self.cursor;
            let newline = line_start + relative_newline;
            self.cursor = newline + 1;
            let line_end = if newline > line_start && self.bytes.get(newline - 1) == Some(&b'\r') {
                newline - 1
            } else {
                newline
            };
            let line = std::str::from_utf8(&self.bytes[line_start..line_end]).map_err(|error| {
                ResponsesError::InvalidSseUtf8 {
                    detail: error.to_string(),
                }
            })?;
            if line.is_empty() {
                if self.data.is_empty() {
                    if self.finished && self.cursor == self.bytes.len() {
                        return Ok(None);
                    }
                    continue;
                }
                let event = self.data.join("\n");
                self.data.clear();
                if event == "[DONE]" {
                    continue;
                }
                return Ok(Some(event));
            }
            if let Some(data) = line.strip_prefix("data:") {
                self.data
                    .push(data.strip_prefix(' ').unwrap_or(data).to_owned());
            }
        }
    }

    fn compact(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let remaining = self.bytes.len() - self.cursor;
        self.bytes.copy_within(self.cursor.., 0);
        self.bytes.truncate(remaining);
        self.cursor = 0;
    }
}

#[cfg(not(target_family = "wasm"))]
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(not(target_family = "wasm"))]
fn map_http_error(error: reqwest::Error) -> ResponsesError {
    ResponsesError::HttpRequest {
        retryable: error.is_connect() || error.is_body() || error.is_request(),
        timeout: error.is_timeout(),
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::SseDecoder;

    #[test]
    fn emits_complete_event_before_eof_and_preserves_split_utf8() {
        let mut decoder = SseDecoder::default();
        let event = "data: héllo\r\n\r\n".as_bytes();
        let split = event.iter().position(|byte| *byte == 0xc3).unwrap() + 1;
        decoder.push(&event[..split]);
        assert_eq!(decoder.next().unwrap(), None);
        decoder.push(&event[split..]);
        assert_eq!(decoder.next().unwrap().as_deref(), Some("héllo"));
        assert_eq!(decoder.next().unwrap(), None);
        // The host may remain open indefinitely after this event.
        assert!(!decoder.finished);
    }

    #[test]
    fn reports_invalid_utf8_only_after_a_complete_line() {
        let mut decoder = SseDecoder::default();
        decoder.push(b"data: \xff");
        assert_eq!(decoder.next().unwrap(), None);
        decoder.push(b"\n\n");
        assert!(matches!(
            decoder.next(),
            Err(crate::ResponsesError::InvalidSseUtf8 { .. })
        ));
    }

    #[test]
    fn decodes_fragmented_and_multiline_sse_events() {
        let mut decoder = SseDecoder::default();
        decoder.push(b": keepalive\n\ndata: {\"type\":\"response.");
        assert_eq!(decoder.next().unwrap(), None);
        decoder.push(b"created\"}\r\n\r\ndata: first\ndata: second\n\n");
        assert_eq!(
            decoder.next().unwrap().as_deref(),
            Some("{\"type\":\"response.created\"}")
        );
        assert_eq!(decoder.next().unwrap().as_deref(), Some("first\nsecond"));
        assert_eq!(decoder.next().unwrap(), None);
    }

    #[test]
    fn skips_done_and_flushes_an_unterminated_final_event() {
        let mut decoder = SseDecoder::default();
        decoder.push(b"data: [DONE]\n\ndata: final");
        decoder.finish();
        assert_eq!(decoder.next().unwrap().as_deref(), Some("final"));
        assert_eq!(decoder.next().unwrap(), None);
    }

    #[test]
    fn decodes_many_events_from_one_chunk_without_repacking_each_line() {
        let mut body = String::new();
        for index in 0..4_096 {
            body.push_str("data: event-");
            body.push_str(&index.to_string());
            body.push_str("\n\n");
        }

        let mut decoder = SseDecoder::default();
        decoder.push(body.as_bytes());
        for index in 0..4_096 {
            assert_eq!(
                decoder.next().unwrap().as_deref(),
                Some(format!("event-{index}").as_str())
            );
        }
        assert_eq!(decoder.next().unwrap(), None);
    }
}

#[cfg(target_family = "wasm")]
impl ResponsesHttp {
    pub(crate) fn new(
        host: Option<std::sync::Arc<dyn crate::transport::host::HostTransport>>,
    ) -> Self {
        Self { host }
    }

    pub(crate) async fn send(
        &self,
        api_base_url: &str,
        auth: &OpenAiAuthSnapshot,
        session_id: &str,
        thread_id: &str,
        turn_state: Option<&str>,
        request: &EncodedRequest,
    ) -> Result<(ResponsesHttpStream, HttpMetadata), ResponsesError> {
        use crate::transport::host::HostConnectRequest;
        let host = self
            .host
            .as_deref()
            .ok_or(ResponsesError::HostUnavailable)?;
        let endpoint = format!("{}/responses", api_base_url.trim_end_matches('/'));
        let turn_state =
            turn_state.filter(|value| header::HeaderValue::from_bytes(value.as_bytes()).is_ok());
        let response = host
            .http(
                HostConnectRequest::new_with_thread_id(
                    &endpoint,
                    auth.bearer(),
                    auth.account_id(),
                    auth.is_fedramp(),
                    session_id,
                    thread_id,
                    turn_state,
                ),
                request.raw().get(),
            )
            .await
            .map_err(map_host_error)?;
        let metadata = HttpMetadata {
            reasoning_included: response.metadata.reasoning_included(),
            turn_state: response.metadata.turn_state().map(str::to_owned),
        };
        Ok((
            ResponsesHttpStream {
                response: response.body,
                decoder: SseDecoder::default(),
                ended: false,
            },
            metadata,
        ))
    }
}

#[cfg(target_family = "wasm")]
fn map_host_error(error: crate::transport::host::HostError) -> ResponsesError {
    use crate::transport::host::HostError;
    match error {
        HostError::HandshakeRejected {
            status,
            body,
            retry_after,
        } => ResponsesError::HttpRejected {
            status,
            body,
            retry_after,
        },
        HostError::Transport {
            detail,
            reconnectable,
        } => ResponsesError::HttpRequest {
            detail,
            retryable: reconnectable,
            timeout: false,
        },
    }
}
