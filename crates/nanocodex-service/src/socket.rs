use std::{collections::HashMap, sync::Once, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::value::{RawValue, to_raw_value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{
        Error as WebSocketError, Message, Utf8Bytes,
        client::IntoClientRequest,
        http::{HeaderValue, header},
    },
};

use crate::{ResponsesError, connector::connect_async};
use nanocodex_core::OpenAiAuthSnapshot;

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
    pub status: u16,
    pub request_id: Option<String>,
    pub server_model: Option<String>,
    pub reasoning_included: bool,
    pub turn_state: Option<String>,
}

/// Persistent `OpenAI` Responses WebSocket connection.
pub(crate) struct ResponsesSocket {
    pump: SocketPump,
    turn_state: Option<String>,
}

/// A request serialized once at the API boundary and ready for transport.
pub struct EncodedRequest(Box<RawValue>);

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

    /// Returns the encoded request text without copying its allocation.
    #[must_use]
    pub fn into_string(self) -> String {
        String::from(Box::<str>::from(self.0))
    }
}

impl ResponsesSocket {
    /// Opens a Responses WebSocket with stable session and cache headers.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, timeout, or handshake failure.
    pub(crate) async fn connect(
        endpoint: &str,
        auth: &OpenAiAuthSnapshot,
        session_id: &str,
    ) -> Result<(Self, ConnectionMetadata), ResponsesError> {
        ensure_crypto_provider();
        let mut request = endpoint
            .into_client_request()
            .map_err(ResponsesError::InvalidUrl)?;
        let authorization = HeaderValue::from_str(&format!("Bearer {}", auth.bearer()))
            .map_err(ResponsesError::InvalidAuthorization)?;
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, authorization);
        if let Some(account_id) = auth.account_id() {
            request.headers_mut().insert(
                "ChatGPT-Account-ID",
                HeaderValue::from_str(account_id).map_err(ResponsesError::InvalidAuthorization)?,
            );
        }
        if auth.is_fedramp() {
            request
                .headers_mut()
                .insert("X-OpenAI-Fedramp", HeaderValue::from_static("true"));
        }
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
            HeaderValue::from_static(concat!("nanocodex/", env!("CARGO_PKG_VERSION"))),
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

    /// Sends an encoded request within the configured send timeout.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket closes, sending fails, or times out.
    pub(crate) async fn send(&self, request: EncodedRequest) -> Result<(), ResponsesError> {
        let message = Message::Text(request.into_string().into());
        timeout(SEND_TIMEOUT, self.pump.send(message))
            .await
            .map_err(|_| ResponsesError::SendTimeout {
                seconds: SEND_TIMEOUT.as_secs(),
            })?
            .map_err(ResponsesError::Send)?;
        Ok(())
    }

    /// Receives the next text event within the configured idle timeout.
    ///
    /// # Errors
    ///
    /// Returns an error for timeout, socket failure, closure, or an unexpected frame.
    pub(crate) async fn next_text_or_idle_timeout(&mut self) -> Result<Utf8Bytes, ResponsesError> {
        timeout(EVENT_IDLE_TIMEOUT, self.next_text())
            .await
            .map_err(|_| ResponsesError::IdleTimeout {
                seconds: EVENT_IDLE_TIMEOUT.as_secs(),
            })?
    }

    /// Receives the next text event while handling control frames in the pump.
    ///
    /// # Errors
    ///
    /// Returns an error for socket failure, closure, or an unexpected frame.
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

    #[must_use]
    pub(crate) fn turn_state(&self) -> Option<&str> {
        self.turn_state.as_deref()
    }

    fn capture_turn_state(&mut self, text: &str) {
        if self.turn_state.is_some() {
            return;
        }
        self.turn_state = turn_state_from_event(text);
    }
}

fn turn_state_from_event(text: &str) -> Option<String> {
    if text.starts_with(r#"{"type":""#) && !text.starts_with(r#"{"type":"response.metadata""#) {
        return None;
    }
    let Ok(MetadataEvent::Metadata { headers }) = serde_json::from_str(text) else {
        return None;
    };
    headers.into_iter().find_map(|(name, value)| {
        name.eq_ignore_ascii_case(TURN_STATE_HEADER)
            .then_some(value)
    })
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum MetadataEvent {
    #[serde(rename = "response.metadata")]
    Metadata {
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    #[serde(other)]
    Other,
}

/// Validates an inbound event while preserving its raw JSON representation.
///
/// # Errors
///
/// Returns an error when `text` is not valid JSON.
pub(crate) fn parse_raw_json(text: &str) -> Result<&RawValue, ResponsesError> {
    serde_json::from_str(text).map_err(ResponsesError::InvalidJson)
}

/// Decodes a previously validated raw event into a wire type.
///
/// # Errors
///
/// Returns an error when the event does not match the target type.
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
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<f64>().ok())
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok());
    let body = response.body().as_deref().map_or_else(
        || "empty response body".to_owned(),
        |body| String::from_utf8_lossy(body).into_owned(),
    );
    ResponsesError::HandshakeRejected {
        status,
        body,
        retry_after,
    }
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
    use std::{
        env,
        process::{Command, Stdio},
        time::Duration,
    };

    use eyre::{Result, eyre};
    use futures_util::{SinkExt, StreamExt};
    use tokio::{net::TcpListener, time::timeout};
    use tokio_tungstenite::{
        accept_hdr_async,
        tungstenite::{Message, handshake::server::Request},
    };

    use super::{ResponsesSocket, parse_raw_json, turn_state_from_event};

    #[test]
    fn only_decodes_turn_state_metadata_events() {
        assert_eq!(
            turn_state_from_event(
                r#"{"headers":{"X-Codex-Turn-State":"state-1"},"type":"response.metadata"}"#,
            )
            .as_deref(),
            Some("state-1")
        );
        assert_eq!(
            turn_state_from_event(
                r#"{"type":"response.output_text.delta","delta":"ordinary output"}"#,
            ),
            None
        );
    }

    #[tokio::test]
    #[allow(
        clippy::result_large_err,
        reason = "tungstenite fixes the handshake callback's error response type"
    )]
    async fn respects_http_proxy_for_websocket_connections() -> Result<()> {
        run_proxy_test(
            "HTTP_PROXY",
            "ws://unreachable.nanocodex.invalid/v1/responses",
            "unreachable.nanocodex.invalid:80",
            None,
        )
        .await
    }

    #[tokio::test]
    #[allow(
        clippy::result_large_err,
        reason = "tungstenite fixes the handshake callback's error response type"
    )]
    async fn respects_https_proxy_for_secure_websocket_connections() -> Result<()> {
        run_proxy_test(
            "HTTPS_PROXY",
            "wss://unreachable.nanocodex.invalid/v1/responses",
            "unreachable.nanocodex.invalid:443",
            Some(502),
        )
        .await
    }

    #[allow(
        clippy::result_large_err,
        reason = "tungstenite fixes the handshake callback's error response type"
    )]
    async fn run_proxy_test(
        proxy_env: &str,
        endpoint: &str,
        expected_authority: &str,
        rejection_status: Option<u16>,
    ) -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let proxy_address = listener.local_addr()?;
        let test_binary = env::current_exe()?;
        let mut command = Command::new(test_binary);
        command
            .args([
                "--exact",
                "socket::tests::proxy_connection_child",
                "--ignored",
                "--nocapture",
            ])
            .env("NANOCODEX_HTTP_PROXY_TEST_CHILD", "1")
            .env("NANOCODEX_HTTP_PROXY_TEST_ENDPOINT", endpoint)
            .env_remove("HTTP_PROXY")
            .env_remove("http_proxy")
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .env(proxy_env, format!("http://{proxy_address}"))
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(status) = rejection_status {
            command.env(
                "NANOCODEX_HTTP_PROXY_TEST_EXPECT_REJECTION",
                status.to_string(),
            );
        }
        let child = command.spawn()?;

        let accepted = timeout(Duration::from_secs(5), listener.accept()).await;
        let (stream, _) = if let Ok(connection) = accepted {
            connection?
        } else {
            let output = child.wait_with_output()?;
            return Err(eyre!(
                "WebSocket transport never contacted {proxy_env}; child status: {}; stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        };

        let mut request = Vec::new();
        loop {
            stream.readable().await?;
            let mut bytes = [0_u8; 1024];
            match stream.try_read(&mut bytes) {
                Ok(0) => return Err(eyre!("proxy client closed before CONNECT completed")),
                Ok(read) => {
                    request.extend_from_slice(&bytes[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        let request = String::from_utf8(request)?;
        assert!(
            request.starts_with(&format!("CONNECT {expected_authority} HTTP/1.1\r\n")),
            "unexpected proxy request: {request:?}"
        );

        let response = rejection_status.map_or_else(
            || "HTTP/1.1 200 Connection Established\r\n\r\n".to_owned(),
            |status| format!("HTTP/1.1 {status} Bad Gateway\r\nContent-Length: 0\r\n\r\n"),
        );
        let mut written = 0;
        while written < response.len() {
            stream.writable().await?;
            match stream.try_write(&response.as_bytes()[written..]) {
                Ok(0) => return Err(eyre!("proxy client closed before CONNECT response")),
                Ok(count) => written += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }

        if rejection_status.is_none() {
            let socket =
                accept_hdr_async(stream, |_request: &Request, response| Ok(response)).await?;
            drop(socket);
        }
        let output = child.wait_with_output()?;
        assert!(
            output.status.success(),
            "proxy child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "run only as the isolated child of the proxy connection tests"]
    async fn proxy_connection_child() -> Result<()> {
        if env::var_os("NANOCODEX_HTTP_PROXY_TEST_CHILD").is_none() {
            return Ok(());
        }
        let endpoint = env::var("NANOCODEX_HTTP_PROXY_TEST_ENDPOINT")?;
        let auth = nanocodex_core::OpenAiAuth::api_key("test-key")
            .snapshot()
            .await?;
        let result = ResponsesSocket::connect(&endpoint, &auth, "session-proxy").await;
        let expected_rejection = env::var("NANOCODEX_HTTP_PROXY_TEST_EXPECT_REJECTION")
            .ok()
            .map(|status| status.parse::<u16>())
            .transpose()?;
        match (result, expected_rejection) {
            (Ok(_), None) | (Err(_), Some(_)) => Ok(()),
            (Err(error), None) => Err(error.into()),
            (Ok(_), Some(expected)) => Err(eyre!(
                "proxy connection succeeded; expected HTTP {expected} rejection"
            )),
        }
    }

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
                        .get("authorization")
                        .and_then(|v| v.to_str().ok()),
                    Some("Bearer subscription-token")
                );
                assert_eq!(
                    request
                        .headers()
                        .get("ChatGPT-Account-ID")
                        .and_then(|v| v.to_str().ok()),
                    Some("account-test")
                );
                assert_eq!(
                    request
                        .headers()
                        .get("X-OpenAI-Fedramp")
                        .and_then(|v| v.to_str().ok()),
                    Some("true")
                );
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
        let auth = nanocodex_core::OpenAiAuthSnapshot::new(
            nanocodex_core::OpenAiAuthMode::ChatGpt,
            "subscription-token",
            Some("account-test"),
            true,
            1,
        );
        let (mut socket, _) = ResponsesSocket::connect(&endpoint, &auth, "session-test").await?;

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
