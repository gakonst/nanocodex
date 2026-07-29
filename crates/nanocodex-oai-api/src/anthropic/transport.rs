//! Anthropic Messages transport.
//!
//! Streams `POST /v1/messages` over SSE and translates each Anthropic event into the
//! Responses [`ServerEvent`]s the rest of the pipeline consumes. Translation happens
//! here rather than upstream so the agent loop, rollout, and compaction stay written
//! against a single protocol.

use std::{collections::VecDeque, time::Duration};

use http::header;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Utf8Bytes;

use super::{
    ANTHROPIC_VERSION, AnthropicAuthSnapshot, translate::StreamTranslator, wire::StreamEvent,
};
use crate::{
    ResponsesError, monotonic_now_ns,
    responses::ServerEvent,
    tower::stream::{ResponseEventSource, SourceEvent},
    transport::{
        http::{SseDecoder, map_http_error, retry_after},
        socket::ReceivedText,
    },
};

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

    /// Opens a streaming Messages request.
    ///
    /// API keys authenticate on `x-api-key`; OAuth access tokens authenticate as a
    /// bearer token and additionally require the OAuth beta header. Sending either on
    /// the other header is a 401, so the header choice comes from the snapshot itself.
    pub(crate) async fn send(
        &self,
        api_base_url: &str,
        auth: &AnthropicAuthSnapshot,
        body: String,
        custom_tools: Vec<String>,
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
                concat!("nanocodex/", env!("CARGO_PKG_VERSION")),
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
            translator: StreamTranslator::with_custom_tools(custom_tools),
            pending: VecDeque::new(),
        })
    }
}

/// A streaming Messages response, surfaced as translated Responses events.
pub(crate) struct AnthropicStream {
    response: reqwest::Response,
    decoder: SseDecoder,
    ended: bool,
    translator: StreamTranslator,
    /// Translated events awaiting delivery. One Anthropic event can expand into
    /// several Responses events, so they queue here alongside the text that produced
    /// them; that text is what the inbound `ApiEvent` observability stream records.
    pending: VecDeque<(Utf8Bytes, u64, ServerEvent, bool)>,
}

impl AnthropicStream {
    pub(crate) async fn next_event_or_idle_timeout(
        &mut self,
    ) -> Result<(ReceivedText, ServerEvent, bool), ResponsesError> {
        timeout(EVENT_IDLE_TIMEOUT, self.next_event())
            .await
            .map_err(|_| ResponsesError::IdleTimeout {
                seconds: EVENT_IDLE_TIMEOUT.as_secs(),
            })?
    }

    async fn next_event(&mut self) -> Result<(ReceivedText, ServerEvent, bool), ResponsesError> {
        loop {
            if let Some((text, received_ns, event, emit_raw)) = self.pending.pop_front() {
                return Ok((ReceivedText { text, received_ns }, event, emit_raw));
            }
            if let Some(raw) = self.decoder.next()? {
                // Unparseable payloads are surfaced rather than silently skipped;
                // a malformed frame means the turn's item stream is already suspect.
                let event: StreamEvent =
                    serde_json::from_str(&raw).map_err(ResponsesError::InvalidJson)?;
                let text = Utf8Bytes::from(raw);
                let translated = self.translator.push(event);
                let received_ns = monotonic_now_ns();
                if translated.is_empty() {
                    self.pending
                        .push_back((text, received_ns, ServerEvent::Other, true));
                    continue;
                }
                for (index, translated) in translated.into_iter().enumerate() {
                    self.pending
                        .push_back((text.clone(), received_ns, translated, index == 0));
                }
                continue;
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

impl ResponseEventSource for AnthropicStream {
    async fn next_event_or_idle_timeout(&mut self) -> Result<SourceEvent, ResponsesError> {
        let (received, decoded, emit_raw) = Self::next_event_or_idle_timeout(self).await?;
        Ok(SourceEvent {
            received,
            decoded: Some(decoded),
            emit_raw,
        })
    }
}
