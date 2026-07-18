use std::{sync::Once, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use serde_json::value::{RawValue, to_raw_value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Error as WebSocketError, Message, Utf8Bytes,
        client::IntoClientRequest,
        http::{HeaderValue, header},
    },
};

use crate::ResponsesError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_IDLE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(300)
};
const RESPONSES_WEBSOCKETS_BETA: &str = "responses_websockets=2026-02-06";
const RESPONSES_LITE_HEADER: &str = "x-openai-internal-codex-responses-lite";
const TURN_STATE_HEADER: &str = "x-codex-turn-state";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub(crate) struct ConnectionMetadata {
    pub(crate) status: u16,
    pub(crate) request_id: Option<String>,
    pub(crate) server_model: Option<String>,
    pub(crate) reasoning_included: bool,
    pub(crate) turn_state: Option<String>,
}

pub(crate) struct ResponsesSocket {
    pump: SocketPump,
    turn_state: Option<String>,
}

/// A request serialized once at the API boundary and ready for transport.
pub(crate) struct EncodedRequest(Box<RawValue>);

struct SocketPump {
    commands: mpsc::Sender<SocketCommand>,
    messages: mpsc::UnboundedReceiver<std::result::Result<Message, WebSocketError>>,
    task: tokio::task::JoinHandle<()>,
}

enum SocketCommand {
    Send {
        message: Message,
        result: oneshot::Sender<std::result::Result<(), WebSocketError>>,
    },
}

impl EncodedRequest {
    pub(crate) fn new<T: Serialize + ?Sized>(request: &T) -> Result<Self, ResponsesError> {
        to_raw_value(request)
            .map(Self)
            .map_err(ResponsesError::EncodeRequest)
    }

    pub(crate) fn raw(&self) -> &RawValue {
        &self.0
    }
}

impl ResponsesSocket {
    pub(crate) async fn connect(
        endpoint: &str,
        api_key: &str,
        session_id: &str,
    ) -> Result<(Self, ConnectionMetadata), ResponsesError> {
        ensure_crypto_provider();
        let mut request = endpoint
            .into_client_request()
            .map_err(ResponsesError::InvalidUrl)?;
        let authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(ResponsesError::InvalidAuthorization)?;
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, authorization);
        request.headers_mut().insert(
            "OpenAI-Beta",
            HeaderValue::from_static(RESPONSES_WEBSOCKETS_BETA),
        );
        request
            .headers_mut()
            .insert(RESPONSES_LITE_HEADER, HeaderValue::from_static("true"));
        for name in ["session-id", "thread-id", "x-client-request-id"] {
            request.headers_mut().insert(
                name,
                HeaderValue::from_str(session_id).map_err(ResponsesError::InvalidSessionId)?,
            );
        }
        request.headers_mut().insert(
            "x-responsesapi-include-timing-metrics",
            HeaderValue::from_static("true"),
        );
        request.headers_mut().insert(
            header::USER_AGENT,
            HeaderValue::from_static(concat!("harness/", env!("CARGO_PKG_VERSION"))),
        );
        let (socket, response) = timeout(CONNECT_TIMEOUT, connect_async(request))
            .await
            .map_err(|_| ResponsesError::HandshakeTimeout {
                seconds: CONNECT_TIMEOUT.as_secs(),
            })?
            .map_err(map_handshake_error)?;
        let turn_state = header_string(response.headers(), TURN_STATE_HEADER);
        let metadata = ConnectionMetadata {
            status: response.status().as_u16(),
            request_id: header_string(response.headers(), "x-request-id"),
            server_model: header_string(response.headers(), "openai-model"),
            reasoning_included: response.headers().contains_key("x-reasoning-included"),
            turn_state: turn_state.clone(),
        };
        Ok((
            Self {
                pump: SocketPump::new(socket),
                turn_state,
            },
            metadata,
        ))
    }

    pub(crate) async fn send(&self, request: &EncodedRequest) -> Result<(), ResponsesError> {
        let message = Message::Text(request.raw().get().to_owned().into());
        timeout(SEND_TIMEOUT, self.pump.send(message))
            .await
            .map_err(|_| ResponsesError::SendTimeout {
                seconds: SEND_TIMEOUT.as_secs(),
            })?
            .map_err(ResponsesError::Send)?;
        Ok(())
    }

    pub(crate) async fn next_text_or_idle_timeout(&mut self) -> Result<Utf8Bytes, ResponsesError> {
        timeout(EVENT_IDLE_TIMEOUT, self.next_text())
            .await
            .map_err(|_| ResponsesError::IdleTimeout {
                seconds: EVENT_IDLE_TIMEOUT.as_secs(),
            })?
    }

    pub(crate) async fn next_text(&mut self) -> Result<Utf8Bytes, ResponsesError> {
        loop {
            let message = self
                .pump
                .next()
                .await
                .ok_or(ResponsesError::UnexpectedEnd)?
                .map_err(ResponsesError::Receive)?;

            match message {
                Message::Text(text) => {
                    self.capture_turn_state(text.as_str());
                    return Ok(text);
                }
                Message::Binary(_) => return Err(ResponsesError::UnexpectedBinary),
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                Message::Close(frame) => {
                    let detail = frame.map_or_else(
                        || "without a reason".to_owned(),
                        |frame| format!("with code {}: {}", frame.code, frame.reason),
                    );
                    return Err(ResponsesError::Closed { detail });
                }
            }
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
                    name.eq_ignore_ascii_case(TURN_STATE_HEADER)
                        .then(|| value.as_str().map(str::to_owned))
                        .flatten()
                })
            });
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

impl SocketPump {
    fn new(mut socket: Socket) -> Self {
        let (commands, mut command_receiver) = mpsc::channel(32);
        let (message_sender, messages) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    command = command_receiver.recv() => {
                        let Some(command) = command else {
                            break;
                        };
                        match command {
                            SocketCommand::Send { message, result } => {
                                let send_result = socket.send(message).await;
                                let should_stop = send_result.is_err();
                                drop(result.send(send_result));
                                if should_stop {
                                    break;
                                }
                            }
                        }
                    }
                    message = socket.next() => {
                        let Some(message) = message else {
                            break;
                        };
                        match message {
                            Ok(Message::Ping(payload)) => {
                                if let Err(error) = socket.send(Message::Pong(payload)).await {
                                    drop(message_sender.send(Err(error)));
                                    break;
                                }
                            }
                            Ok(Message::Pong(_)) => {}
                            Ok(message) => {
                                let should_stop = matches!(message, Message::Close(_));
                                if message_sender.send(Ok(message)).is_err() || should_stop {
                                    break;
                                }
                            }
                            Err(error) => {
                                drop(message_sender.send(Err(error)));
                                break;
                            }
                        }
                    }
                }
            }
        });
        Self {
            commands,
            messages,
            task,
        }
    }

    async fn send(&self, message: Message) -> std::result::Result<(), WebSocketError> {
        let (result, receiver) = oneshot::channel();
        self.commands
            .send(SocketCommand::Send { message, result })
            .await
            .map_err(|_| WebSocketError::ConnectionClosed)?;
        receiver
            .await
            .unwrap_or(Err(WebSocketError::ConnectionClosed))
    }

    async fn next(&mut self) -> Option<std::result::Result<Message, WebSocketError>> {
        self.messages.recv().await
    }
}

impl Drop for SocketPump {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn map_handshake_error(error: WebSocketError) -> ResponsesError {
    let WebSocketError::Http(response) = error else {
        return ResponsesError::Handshake(error);
    };
    let status = response.status().as_u16();
    let body = response.body().as_deref().map_or_else(
        || "empty response body".to_owned(),
        |body| String::from_utf8_lossy(body).into_owned(),
    );
    ResponsesError::HandshakeRejected { status, body }
}

fn ensure_crypto_provider() {
    static INITIALIZE: Once = Once::new();
    INITIALIZE.call_once(|| {
        drop(rustls::crypto::ring::default_provider().install_default());
    });
}

fn header_string(
    headers: &tokio_tungstenite::tungstenite::http::HeaderMap,
    name: &str,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use eyre::{Result, eyre};
    use futures_util::{SinkExt, StreamExt};
    use tokio::{net::TcpListener, time::timeout};
    use tokio_tungstenite::{
        accept_hdr_async,
        tungstenite::{Message, handshake::server::Request},
    };

    use super::{ResponsesSocket, parse_raw_json};

    #[tokio::test]
    #[allow(
        clippy::result_large_err,
        reason = "tungstenite fixes the handshake callback's error response type"
    )]
    async fn answers_ping_while_response_consumer_is_idle() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let keepalive = b"keepalive".to_vec();
        let expected_keepalive = keepalive.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_hdr_async(stream, |request: &Request, response| {
                assert_eq!(
                    request
                        .headers()
                        .get("session-id")
                        .and_then(|v| v.to_str().ok()),
                    Some("session-test")
                );
                assert_eq!(
                    request
                        .headers()
                        .get("thread-id")
                        .and_then(|v| v.to_str().ok()),
                    Some("session-test")
                );
                assert_eq!(
                    request
                        .headers()
                        .get("x-client-request-id")
                        .and_then(|v| v.to_str().ok()),
                    Some("session-test")
                );
                assert_eq!(
                    request
                        .headers()
                        .get("OpenAI-Beta")
                        .and_then(|v| v.to_str().ok()),
                    Some("responses_websockets=2026-02-06")
                );
                assert_eq!(
                    request
                        .headers()
                        .get("x-openai-internal-codex-responses-lite")
                        .and_then(|v| v.to_str().ok()),
                    Some("true")
                );
                Ok(response)
            })
            .await?;
            socket.send(Message::Ping(keepalive.into())).await?;
            let reply = timeout(Duration::from_secs(1), socket.next())
                .await
                .map_err(|_| eyre!("client did not answer WebSocket ping"))?
                .ok_or_else(|| eyre!("client closed before answering WebSocket ping"))??;
            assert_eq!(reply, Message::Pong(expected_keepalive.into()));
            socket
                .send(Message::Text(r#"{"type":"probe"}"#.into()))
                .await?;
            socket.send(Message::Binary(b"{}".to_vec().into())).await?;
            Result::<()>::Ok(())
        });

        let endpoint = format!("ws://{address}");
        let (mut socket, _) =
            ResponsesSocket::connect(&endpoint, "test-key", "session-test").await?;

        server.await??;
        let text = socket.next_text().await?;
        assert_eq!(parse_raw_json(text.as_str())?.get(), r#"{"type":"probe"}"#);
        assert!(matches!(
            socket.next_text().await,
            Err(crate::ResponsesError::UnexpectedBinary)
        ));
        Ok(())
    }
}
