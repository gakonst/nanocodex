//! Account-authenticated managed voice transport; provider credentials stay server-side.
use crate::{
    ManagedClient, ManagedError,
    client::{agent_path, decode_response, response_error, validate_id},
};
use futures_util::{SinkExt, StreamExt};
use nanocodex_voice_protocol::{decode_chatgpt_realtime_call, valid_realtime_call_id};
use reqwest::Method;
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

/// A negotiated media answer and its associated sideband identity.
#[derive(Debug)]
pub struct ManagedVoiceCall {
    /// SDP answer to apply to the native media peer.
    pub sdp: String,
    /// Provider call identity, validated before routing a sideband connection.
    pub call_id: String,
}

/// Bounded, authenticated control channel for a single media call.
pub struct ManagedVoiceSocket(WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>);
impl ManagedVoiceSocket {
    /// Sends one protocol frame, with a bounded write deadline.
    pub async fn send(&mut self, frame: &str) -> Result<(), ManagedError> {
        if frame.len() > 65_536 {
            return Err(invalid("voice frame too large"));
        }
        tokio::time::timeout(
            Duration::from_secs(5),
            self.0.send(Message::Text(frame.to_owned().into())),
        )
        .await
        .map_err(|_| invalid("voice send timed out"))?
        .map_err(|_| invalid("voice sideband disconnected"))
    }
    /// Receives one protocol event. Reconnection belongs to the voice session.
    pub async fn next(&mut self) -> Result<Value, ManagedError> {
        loop {
            match self.0.next().await {
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str(&text).map_err(|_| invalid("invalid voice event"));
                }
                Some(Ok(Message::Binary(bytes))) => {
                    return serde_json::from_slice(&bytes)
                        .map_err(|_| invalid("invalid voice event"));
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                _ => return Err(invalid("voice sideband disconnected")),
            }
        }
    }
}
impl ManagedClient {
    /// Starts, stops, delegates, or prefetches a managed voice session.
    /// Lifecycle operation IDs must be reused when retrying an ambiguous request.
    pub async fn voice_operation(
        &self,
        agent: &str,
        session: &str,
        operation: &str,
        body: Value,
    ) -> Result<Value, ManagedError> {
        validate_voice_ids(agent, session)?;
        if !["start", "stop", "delegate", "prefetch"].contains(&operation) {
            return Err(invalid("invalid voice operation"));
        }
        let mut body = body
            .as_object()
            .cloned()
            .ok_or_else(|| invalid("invalid voice body"))?;
        if operation != "prefetch" {
            validate_id(
                "voice operation",
                body.get("operation_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )?;
        }
        if operation == "delegate" {
            bounded_text(body.get("input"), 32_768)?;
        }
        if operation == "prefetch" {
            bounded_text(body.get("query"), 512)?;
        }
        body.insert("voice_session_id".into(), json!(session));
        let operation_id = body.get("operation_id").cloned();
        let encoded = serde_json::to_vec(&body).map_err(|_| invalid("invalid voice body"))?;
        let path = format!("{}/realtime/{operation}", agent_path(agent));
        for attempt in 0..3 {
            let result = async {
                let response = self
                    .request(Method::POST, &path, Some(&encoded), None)
                    .await?;
                decode_response::<Value>(response).await
            }
            .await;
            match result {
                Ok(value) => {
                    if operation != "prefetch"
                        && (value["voice_session_id"] != session
                            || Some(&value["operation_id"]) != operation_id.as_ref())
                    {
                        return Err(invalid("voice operation identity mismatch"));
                    }
                    if operation == "delegate" {
                        validate_id("voice turn", value["turn_id"].as_str().unwrap_or_default())?;
                        if !matches!(value["route"].as_str(), Some("started" | "steered")) {
                            return Err(invalid("invalid voice route"));
                        }
                    }
                    return Ok(value);
                }
                Err(error) if attempt < 2 && retryable(&error) => {
                    tokio::time::sleep(Duration::from_millis(250 << attempt)).await
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!()
    }

    /// Negotiates native media without exposing provider authorization to the client.
    pub async fn voice_call(
        &self,
        agent: &str,
        session: &str,
        sdp: &str,
        settings: Value,
    ) -> Result<ManagedVoiceCall, ManagedError> {
        validate_voice_ids(agent, session)?;
        bounded_text(Some(&json!(sdp)), 32_768)?;
        let body = serde_json::to_vec(&json!({"sdp":sdp,"session":settings}))
            .map_err(|_| invalid("invalid voice call"))?;
        if body.len() > 65_536 {
            return Err(invalid("voice call too large"));
        }
        let url = self.url(&format!("{}/realtime/calls", agent_path(agent)))?;
        let request = self
            .http
            .post(url.clone())
            .timeout(Duration::from_secs(30))
            .header("x-nanocodex-voice-session-id", session)
            .header("content-type", "application/json")
            .body(body);
        let mut response = self
            .send_with_access(request, &url)
            .await
            .map_err(ManagedError::Transport)?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let location = response
            .headers()
            .get("x-nanocodex-realtime-location")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(ManagedError::Transport)? {
            if bytes.len() + chunk.len() > 65_536 {
                return Err(invalid("voice answer too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid("invalid voice answer"))?;
        let call = decode_chatgpt_realtime_call(text, &location)
            .map_err(|_| invalid("invalid voice answer"))?;
        Ok(ManagedVoiceCall {
            sdp: call.sdp,
            call_id: call.call_id,
        })
    }

    /// Opens a control channel with the same account authority as the managed agent.
    pub async fn voice_sideband(
        &self,
        agent: &str,
        session: &str,
        call: &str,
    ) -> Result<ManagedVoiceSocket, ManagedError> {
        validate_voice_ids(agent, session)?;
        if !valid_realtime_call_id(call) {
            return Err(invalid("invalid voice call identity"));
        }
        let mut url = self.url(&format!("{}/realtime/sideband", agent_path(agent)))?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme)
            .map_err(|_| invalid("invalid voice endpoint"))?;
        url.query_pairs_mut()
            .append_pair("call_id", call)
            .append_pair("voice_session_id", session);
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| invalid("invalid voice endpoint"))?;
        let mut authorization = format!("Bearer {}", self.bearer)
            .parse::<reqwest::header::HeaderValue>()
            .map_err(|_| invalid("invalid voice authority"))?;
        authorization.set_sensitive(true);
        request.headers_mut().insert("authorization", authorization);
        if let Some(origin) = &self.request_origin {
            request
                .headers_mut()
                .insert("x-nanocodex-client-context", origin.clone());
        }
        let config = WebSocketConfig::default()
            .max_message_size(Some(256 * 1024))
            .max_frame_size(Some(256 * 1024));
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(20),
            tokio_tungstenite::connect_async_with_config(request, Some(config), true),
        )
        .await
        .map_err(|_| invalid("voice sideband timed out"))?
        .map_err(|_| invalid("voice sideband connection failed"))?;
        Ok(ManagedVoiceSocket(socket))
    }
}
fn validate_voice_ids(agent: &str, session: &str) -> Result<(), ManagedError> {
    validate_id("agent", agent)?;
    let id = uuid::Uuid::parse_str(session).map_err(|_| invalid("invalid voice session"))?;
    if id.get_version_num() != 7 || id.to_string() != session {
        return Err(invalid("invalid voice session"));
    }
    Ok(())
}
fn bounded_text(value: Option<&Value>, limit: usize) -> Result<(), ManagedError> {
    match value.and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() && text.len() <= limit => Ok(()),
        _ => Err(invalid("invalid voice text")),
    }
}
const fn invalid(message: &'static str) -> ManagedError {
    ManagedError::InvalidResponse(message)
}
fn retryable(error: &ManagedError) -> bool {
    matches!(error, ManagedError::Transport(_))
        || matches!(error, ManagedError::Http {status, ..} if [408,429,502,503,504].contains(&status.as_u16()))
}
