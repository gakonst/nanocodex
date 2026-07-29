//! Native Anthropic Messages HTTP/SSE transport.

use std::time::Duration;

use http::header;
use tokio::time::timeout;

use super::{ANTHROPIC_VERSION, AnthropicAuthSnapshot, wire::StreamEvent};
use nanocodex_oai_api::transport::ResponsesError;

const EVENT_IDLE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_mins(5)
};

#[derive(Clone)]
pub(crate) struct AnthropicHttp {
    client: reqwest::Client,
}

impl AnthropicHttp {
    pub(crate) const fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    pub(crate) async fn send(
        &self,
        api_base_url: &str,
        auth: &AnthropicAuthSnapshot,
        body: String,
    ) -> Result<AnthropicStream, ResponsesError> {
        let endpoint = format!("{}/messages", api_base_url.trim_end_matches('/'));
        let (auth_header, auth_value) = auth.authorization_header();
        let mut builder = self
            .client
            .post(endpoint)
            .header(auth_header, auth_value)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "text/event-stream")
            .header(
                header::USER_AGENT,
                concat!("nanocodex-anthropic/", env!("CARGO_PKG_VERSION")),
            )
            .body(body);
        if let Some(beta) = auth.beta() {
            builder = builder.header("anthropic-beta", beta);
        }

        let response = builder.send().await.map_err(map_http_error)?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = retry_after(response.headers());
            let body = response.text().await.map_err(map_http_error)?;
            return Err(ResponsesError::HttpRejected {
                status: status.as_u16(),
                body,
                retry_after,
            });
        }
        Ok(AnthropicStream {
            response,
            decoder: SseDecoder::default(),
            ended: false,
        })
    }
}

pub(crate) struct AnthropicStream {
    response: reqwest::Response,
    decoder: SseDecoder,
    ended: bool,
}

impl AnthropicStream {
    pub(crate) async fn next(&mut self) -> Result<(String, StreamEvent), ResponsesError> {
        timeout(EVENT_IDLE_TIMEOUT, self.next_inner())
            .await
            .map_err(|_| ResponsesError::IdleTimeout {
                seconds: EVENT_IDLE_TIMEOUT.as_secs(),
            })?
    }

    async fn next_inner(&mut self) -> Result<(String, StreamEvent), ResponsesError> {
        loop {
            if let Some(raw) = self.decoder.next()? {
                let event = serde_json::from_str(&raw).map_err(ResponsesError::InvalidJson)?;
                return Ok((raw, event));
            }
            if self.ended {
                return Err(ResponsesError::UnexpectedEnd);
            }
            if let Some(chunk) = self.response.chunk().await.map_err(map_http_error)? {
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

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn map_http_error(error: reqwest::Error) -> ResponsesError {
    ResponsesError::HttpRequest {
        retryable: error.is_connect() || error.is_body(),
        timeout: error.is_timeout(),
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::SseDecoder;

    #[test]
    fn decodes_fragmented_and_multiline_sse_events() {
        let mut decoder = SseDecoder::default();
        decoder.push(b": keepalive\n\ndata: {\"type\":\"message_");
        assert!(decoder.next().unwrap().is_none());
        decoder.push(b"stop\"}\r\n\r\ndata: first\ndata: second\n\n");
        assert_eq!(
            decoder.next().unwrap().as_deref(),
            Some("{\"type\":\"message_stop\"}")
        );
        assert_eq!(decoder.next().unwrap().as_deref(), Some("first\nsecond"));
    }
}
