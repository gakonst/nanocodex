use std::time::Duration;

use http::header;
use nanocodex_core::{OpenAiAuthSnapshot, monotonic_now_ns};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Utf8Bytes;

use crate::{EncodedRequest, ResponsesError, socket::ReceivedText};

const EVENT_IDLE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(300)
};
const RESPONSES_LITE_HEADER: &str = "x-openai-internal-codex-responses-lite";

#[derive(Clone)]
pub(crate) struct ResponsesHttp {
    client: reqwest::Client,
}

pub(crate) struct ResponsesHttpStream {
    response: reqwest::Response,
    decoder: SseDecoder,
    ended: bool,
}

pub(crate) struct HttpMetadata {
    pub(crate) reasoning_included: bool,
}

impl ResponsesHttp {
    pub(crate) fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    pub(crate) async fn send(
        &self,
        api_base_url: &str,
        auth: &OpenAiAuthSnapshot,
        session_id: &str,
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
            .header("thread-id", session_id)
            .header("x-client-request-id", session_id)
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
        let response = builder.send().await.map_err(ResponsesError::HttpRequest)?;
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
    pub(crate) async fn next_text_or_idle_timeout(
        &mut self,
    ) -> Result<ReceivedText, ResponsesError> {
        timeout(EVENT_IDLE_TIMEOUT, self.next_text())
            .await
            .map_err(|_| ResponsesError::IdleTimeout {
                seconds: EVENT_IDLE_TIMEOUT.as_secs(),
            })?
    }

    async fn next_text(&mut self) -> Result<ReceivedText, ResponsesError> {
        loop {
            if let Some(text) = self.decoder.next()? {
                return Ok(ReceivedText {
                    text: Utf8Bytes::from(text),
                    received_ns: monotonic_now_ns(),
                });
            }
            if self.ended {
                return Err(ResponsesError::UnexpectedEnd);
            }
            if let Some(chunk) = self
                .response
                .chunk()
                .await
                .map_err(ResponsesError::HttpRequest)?
            {
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
    data: Vec<String>,
    finished: bool,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
    }

    fn finish(&mut self) {
        self.finished = true;
        if !self.bytes.is_empty() {
            self.bytes.push(b'\n');
        }
        self.bytes.push(b'\n');
    }

    fn next(&mut self) -> Result<Option<String>, ResponsesError> {
        loop {
            let Some(newline) = self.bytes.iter().position(|byte| *byte == b'\n') else {
                return Ok(None);
            };
            let mut line = self.bytes.drain(..=newline).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8(line).map_err(ResponsesError::InvalidSseUtf8)?;
            if line.is_empty() {
                if self.data.is_empty() {
                    if self.finished && self.bytes.is_empty() {
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
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::SseDecoder;

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
}
