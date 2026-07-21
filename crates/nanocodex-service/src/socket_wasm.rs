use std::time::Duration;

use js_sys::Promise;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, value::RawValue, value::to_raw_value};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::ResponsesError;

const EVENT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = connect)]
    fn host_connect(
        endpoint: &str,
        bearer_token: &str,
        session_id: &str,
    ) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = send)]
    fn host_send(handle: u32, message: &str) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = next)]
    fn host_next(handle: u32, timeout_ms: u32) -> Result<Promise, JsValue>;

    #[wasm_bindgen(js_namespace = ["globalThis", "nanocodexHost"], js_name = close)]
    fn host_close(handle: u32);
}

pub(crate) struct ConnectionMetadata {
    pub status: u16,
    pub request_id: Option<String>,
    pub server_model: Option<String>,
    pub reasoning_included: bool,
    pub turn_state: Option<String>,
}

pub(crate) struct ResponsesSocket {
    handle: u32,
    turn_state: Option<String>,
}

/// A request serialized once at the API boundary and ready for transport.
pub struct EncodedRequest(Box<RawValue>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostConnection {
    handle: u32,
    status: u16,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    server_model: Option<String>,
    #[serde(default)]
    reasoning_included: bool,
    #[serde(default)]
    turn_state: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostSend {
    ok: bool,
    #[serde(default)]
    reconnectable: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HostMessage {
    Text { text: String },
    Closed { detail: String },
    Error { detail: String },
    Timeout,
    Binary,
}

impl EncodedRequest {
    /// Serializes a request once into compact raw JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be serialized.
    pub fn new<T: Serialize + ?Sized>(request: &T) -> Result<Self, ResponsesError> {
        to_raw_value(request)
            .map(Self)
            .map_err(ResponsesError::EncodeRequest)
    }

    #[must_use]
    pub fn raw(&self) -> &RawValue {
        &self.0
    }
}

impl ResponsesSocket {
    pub(crate) async fn connect(
        endpoint: &str,
        auth: &nanocodex_core::OpenAiAuthSnapshot,
        session_id: &str,
    ) -> Result<(Self, ConnectionMetadata), ResponsesError> {
        let promise = host_connect(endpoint, auth.bearer(), session_id).map_err(|error| {
            ResponsesError::Connect {
                detail: js_error(&error),
            }
        })?;
        let connection: HostConnection = await_json(promise)
            .await
            .map_err(|detail| ResponsesError::Connect { detail })?;
        let metadata = ConnectionMetadata {
            status: connection.status,
            request_id: connection.request_id,
            server_model: connection.server_model,
            reasoning_included: connection.reasoning_included,
            turn_state: connection.turn_state.clone(),
        };
        Ok((
            Self {
                handle: connection.handle,
                turn_state: connection.turn_state,
            },
            metadata,
        ))
    }

    pub(crate) async fn send(&self, request: EncodedRequest) -> Result<(), ResponsesError> {
        let promise =
            host_send(self.handle, request.raw().get()).map_err(|error| ResponsesError::Send {
                detail: js_error(&error),
                reconnectable: false,
            })?;
        let result: HostSend =
            await_json(promise)
                .await
                .map_err(|detail| ResponsesError::Send {
                    detail,
                    reconnectable: false,
                })?;
        if result.ok {
            return Ok(());
        }
        Err(ResponsesError::Send {
            detail: result
                .error
                .unwrap_or_else(|| "JavaScript host rejected the frame".to_owned()),
            reconnectable: result.reconnectable,
        })
    }

    pub(crate) async fn next_text_or_idle_timeout(&mut self) -> Result<String, ResponsesError> {
        let timeout_ms = u32::try_from(EVENT_IDLE_TIMEOUT.as_millis()).unwrap_or(u32::MAX);
        let promise = host_next(self.handle, timeout_ms)
            .map_err(|error| ResponsesError::Receive(js_error(&error)))?;
        let message: HostMessage = await_json(promise).await.map_err(ResponsesError::Receive)?;
        match message {
            HostMessage::Text { text } => {
                self.capture_turn_state(&text);
                Ok(text)
            }
            HostMessage::Closed { detail } => Err(ResponsesError::Closed { detail }),
            HostMessage::Error { detail } => Err(ResponsesError::Receive(detail)),
            HostMessage::Timeout => Err(ResponsesError::IdleTimeout {
                seconds: EVENT_IDLE_TIMEOUT.as_secs(),
            }),
            HostMessage::Binary => Err(ResponsesError::UnexpectedBinary),
        }
    }

    pub(crate) fn turn_state(&self) -> Option<&str> {
        self.turn_state.as_deref()
    }

    fn capture_turn_state(&mut self, text: &str) {
        if self.turn_state.is_some() {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(text) else {
            return;
        };
        if event.get("type").and_then(Value::as_str) != Some("response.metadata") {
            return;
        }
        self.turn_state = event
            .get("headers")
            .and_then(Value::as_object)
            .and_then(|headers| {
                headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("x-codex-turn-state")
                        .then(|| value.as_str().map(str::to_owned))
                        .flatten()
                })
            });
    }
}

impl Drop for ResponsesSocket {
    fn drop(&mut self) {
        host_close(self.handle);
    }
}

pub(crate) fn parse_raw_json(text: &str) -> Result<&RawValue, ResponsesError> {
    serde_json::from_str(text).map_err(ResponsesError::InvalidJson)
}

pub(crate) fn decode_event<T: DeserializeOwned>(event: &RawValue) -> Result<T, ResponsesError> {
    serde_json::from_str(event.get()).map_err(|source| ResponsesError::InvalidPayload {
        source,
        event: event.get().to_owned(),
    })
}

async fn await_json<T: DeserializeOwned>(promise: Promise) -> Result<T, String> {
    let value = JsFuture::from(promise)
        .await
        .map_err(|error| js_error(&error))?;
    let text = value
        .as_string()
        .ok_or_else(|| "JavaScript host returned a non-string result".to_owned())?;
    serde_json::from_str(&text)
        .map_err(|error| format!("JavaScript host returned invalid JSON: {error}"))
}

fn js_error(error: &JsValue) -> String {
    error.as_string().unwrap_or_else(|| format!("{error:?}"))
}
