use std::{sync::Once, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, header},
    },
};

use crate::{ResponsesError, Result};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const RESPONSES_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub(crate) struct ConnectionMetadata {
    pub(crate) status: u16,
    pub(crate) request_id: Option<String>,
    pub(crate) server_model: Option<String>,
    pub(crate) reasoning_included: bool,
}

pub(crate) struct ResponsesSocket {
    socket: Socket,
}

impl ResponsesSocket {
    pub(crate) async fn connect(
        endpoint: &str,
        api_key: &str,
    ) -> Result<(Self, ConnectionMetadata)> {
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
            HeaderValue::from_static(RESPONSES_WEBSOCKET_BETA),
        );
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
            .map_err(ResponsesError::Handshake)?;
        let metadata = ConnectionMetadata {
            status: response.status().as_u16(),
            request_id: header_string(response.headers(), "x-request-id"),
            server_model: header_string(response.headers(), "openai-model"),
            reasoning_included: response.headers().contains_key("x-reasoning-included"),
        };
        Ok((Self { socket }, metadata))
    }

    pub(crate) async fn send<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let payload = serde_json::to_string(value).map_err(ResponsesError::EncodeRequest)?;
        timeout(
            SEND_TIMEOUT,
            self.socket.send(Message::Text(payload.into())),
        )
        .await
        .map_err(|_| ResponsesError::SendTimeout {
            seconds: SEND_TIMEOUT.as_secs(),
        })?
        .map_err(ResponsesError::Send)?;
        Ok(())
    }

    pub(crate) async fn next_json(&mut self) -> Result<Value> {
        loop {
            let message = timeout(EVENT_IDLE_TIMEOUT, self.socket.next())
                .await
                .map_err(|_| ResponsesError::IdleTimeout {
                    seconds: EVENT_IDLE_TIMEOUT.as_secs(),
                })?
                .ok_or(ResponsesError::UnexpectedEnd)?
                .map_err(ResponsesError::Receive)?;

            match message {
                Message::Text(text) => {
                    return serde_json::from_str(text.as_ref())
                        .map_err(ResponsesError::InvalidJson)
                        .map_err(Into::into);
                }
                Message::Binary(bytes) => {
                    return serde_json::from_slice(bytes.as_ref())
                        .map_err(ResponsesError::InvalidJson)
                        .map_err(Into::into);
                }
                Message::Ping(payload) => {
                    timeout(SEND_TIMEOUT, self.socket.send(Message::Pong(payload)))
                        .await
                        .map_err(|_| ResponsesError::PongTimeout {
                            seconds: SEND_TIMEOUT.as_secs(),
                        })?
                        .map_err(ResponsesError::Pong)?;
                }
                Message::Pong(_) | Message::Frame(_) => {}
                Message::Close(frame) => {
                    let detail = frame.map_or_else(
                        || "without a reason".to_owned(),
                        |frame| format!("with code {}: {}", frame.code, frame.reason),
                    );
                    return Err(ResponsesError::Closed { detail }.into());
                }
            }
        }
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
