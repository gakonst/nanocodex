use std::time::Duration;

use js_sys::Promise;
use nanocodex::oai::transport::host::{
    ConnectedHost, HostConnectRequest, HostConnection, HostConnectionMetadata, HostError,
    HostFuture, HostMessage, HostTransport,
};
use serde::Deserialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = connect)]
    fn host_connect(
        endpoint: &str,
        bearer_token: &str,
        account_id: Option<&str>,
        fedramp: bool,
        session_id: &str,
        thread_id: &str,
        turn_state: Option<&str>,
    ) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = send)]
    fn host_send(handle: u32, message: &str) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = next)]
    fn host_next(handle: u32, timeout_ms: u32) -> Result<Promise, JsValue>;

    #[wasm_bindgen(js_namespace = ["globalThis", "nanocodexHost"], js_name = close)]
    fn host_close(handle: u32);

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = sleep)]
    fn host_sleep(session_id: &str, milliseconds: u32) -> Result<Promise, JsValue>;
}

pub(super) struct JavaScriptResponsesHost;

struct JavaScriptHostConnection {
    handle: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostConnectionWire {
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
struct HostSendWire {
    ok: bool,
    #[serde(default)]
    reconnectable: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HostMessageWire {
    Text {
        text: String,
    },
    Closed {
        detail: String,
    },
    Error {
        detail: String,
        #[serde(default = "default_reconnectable")]
        reconnectable: bool,
    },
    Timeout,
    Binary,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HostFailureWire {
    Transport {
        detail: String,
        #[serde(default)]
        reconnectable: bool,
    },
    HandshakeRejected {
        status: u16,
        #[serde(default)]
        body: String,
        #[serde(default)]
        retry_after: Option<f64>,
    },
}

impl HostTransport for JavaScriptResponsesHost {
    fn connect<'a>(
        &'a self,
        request: HostConnectRequest<'a>,
    ) -> HostFuture<'a, Result<ConnectedHost, HostError>> {
        Box::pin(async move {
            let promise = host_connect(
                request.endpoint(),
                request.bearer_token(),
                request.account_id(),
                request.is_fedramp(),
                request.session_id(),
                request.thread_id(),
                request.turn_state(),
            )
            .map_err(|error| decode_host_error(&error, true))?;
            let connection: HostConnectionWire = await_json(promise)
                .await
                .map_err(|error| decode_host_error(&error, true))?;
            let mut metadata = HostConnectionMetadata::new(connection.status)
                .with_reasoning_included(connection.reasoning_included);
            if let Some(request_id) = connection.request_id {
                metadata = metadata.with_request_id(request_id);
            }
            if let Some(server_model) = connection.server_model {
                metadata = metadata.with_server_model(server_model);
            }
            if let Some(turn_state) = connection.turn_state {
                metadata = metadata.with_turn_state(turn_state);
            }
            Ok(ConnectedHost::new(
                JavaScriptHostConnection {
                    handle: connection.handle,
                },
                metadata,
            ))
        })
    }

    fn sleep<'a>(&'a self, session_id: &'a str, duration: Duration) -> HostFuture<'a, ()> {
        Box::pin(async move {
            let milliseconds = u32::try_from(duration.as_millis()).unwrap_or(u32::MAX);
            let Ok(promise) = host_sleep(session_id, milliseconds) else {
                return;
            };
            drop(JsFuture::from(promise).await);
        })
    }
}

impl HostConnection for JavaScriptHostConnection {
    fn send<'a>(&'a self, message: &'a str) -> HostFuture<'a, Result<(), HostError>> {
        Box::pin(async move {
            let promise = host_send(self.handle, message)
                .map_err(|error| decode_host_error(&error, false))?;
            let result: HostSendWire = await_json(promise)
                .await
                .map_err(|error| decode_host_error(&error, false))?;
            if result.ok {
                return Ok(());
            }
            Err(HostError::new(
                result
                    .error
                    .unwrap_or_else(|| "JavaScript host rejected the frame".to_owned()),
            )
            .with_reconnectable(result.reconnectable))
        })
    }

    fn next(&mut self, idle_timeout: Duration) -> HostFuture<'_, Result<HostMessage, HostError>> {
        Box::pin(async move {
            let timeout_ms = u32::try_from(idle_timeout.as_millis()).unwrap_or(u32::MAX);
            let promise = host_next(self.handle, timeout_ms)
                .map_err(|error| decode_host_error(&error, true))?;
            let message: HostMessageWire = await_json(promise)
                .await
                .map_err(|error| decode_host_error(&error, true))?;
            match message {
                HostMessageWire::Text { text } => Ok(HostMessage::Text(text)),
                HostMessageWire::Closed { detail } => Ok(HostMessage::Closed { detail }),
                HostMessageWire::Error {
                    detail,
                    reconnectable,
                } => Err(HostError::new(detail).with_reconnectable(reconnectable)),
                HostMessageWire::Timeout => Ok(HostMessage::Timeout),
                HostMessageWire::Binary => Ok(HostMessage::Binary),
            }
        })
    }

    fn close(&mut self) {
        host_close(self.handle);
    }
}

const fn default_reconnectable() -> bool {
    true
}

async fn await_json<T: for<'de> Deserialize<'de>>(promise: Promise) -> Result<T, JsValue> {
    let value = JsFuture::from(promise).await?;
    let text = value.as_string().ok_or_else(|| {
        JsValue::from_str("JavaScript Responses host returned a non-string result")
    })?;
    serde_json::from_str(&text).map_err(|error| {
        JsValue::from_str(&format!(
            "JavaScript Responses host returned invalid JSON: {error}"
        ))
    })
}

fn decode_host_error(error: &JsValue, reconnectable: bool) -> HostError {
    let detail = host_error_message(error);
    let Ok(error) = serde_json::from_str::<HostFailureWire>(&detail) else {
        return HostError::new(detail).with_reconnectable(reconnectable);
    };
    match error {
        HostFailureWire::Transport {
            detail,
            reconnectable,
        } => HostError::new(detail).with_reconnectable(reconnectable),
        HostFailureWire::HandshakeRejected {
            status,
            body,
            retry_after,
        } => HostError::handshake_rejected(
            status,
            body,
            retry_after.and_then(|seconds| Duration::try_from_secs_f64(seconds).ok()),
        ),
    }
}

fn host_error_message(error: &JsValue) -> String {
    error.as_string().unwrap_or_else(|| format!("{error:?}"))
}
