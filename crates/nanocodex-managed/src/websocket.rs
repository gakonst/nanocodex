use std::{
    sync::OnceLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{Message, client::IntoClientRequest as _, protocol::WebSocketConfig},
};

use crate::{
    AgentCapabilities, AgentReceipt, AgentSettings, AgentState, EventCursor, ManagedClient,
    ManagedError, ManagedEvent, ManagedEventData, ManagedEventFuture, ManagedEventSource,
    PromptInput, TurnState, TurnView,
    client::{agent_path, validate_id, validate_idempotency_key},
};

const PREPARE_HEADER: &str = "x-nanocodex-prepare";
const PREPARE_ACTIVE_CONVERSATION: &str = "active-conversation";
const EVENT_CAPACITY: usize = 256;
const RECONNECT_MIN: Duration = Duration::from_millis(100);
const RECONNECT_MAX: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(60);
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(90);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct ConnectedSocket {
    socket: Socket,
    replay_through: String,
    preparation_accepted: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ManagedSocket {
    commands: mpsc::Sender<Command>,
}

#[derive(Debug)]
pub(crate) struct ManagedSocketEvents {
    cursor: EventCursor,
    events: mpsc::Receiver<Result<ManagedEvent, ManagedError>>,
}

enum Command {
    Submit {
        id: String,
        input: PromptInput,
        result: oneshot::Sender<Result<TurnView, ManagedError>>,
    },
}

struct PendingSubmit {
    id: String,
    input: PromptInput,
    result: oneshot::Sender<Result<TurnView, ManagedError>>,
    sent_at: Option<tokio::time::Instant>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage<'a> {
    Prompt { id: &'a str, input: &'a PromptInput },
}

#[derive(Deserialize)]
struct MessageKind {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ReadyMessage {
    #[serde(rename = "type")]
    kind: String,
    session_id: String,
    restored: bool,
    active_turns: Vec<String>,
    capabilities: AgentCapabilities,
    settings: AgentSettings,
    latest_event_cursor: String,
}

impl ManagedSocket {
    pub(crate) async fn create(
        client: ManagedClient,
        settings: AgentSettings,
    ) -> Result<(AgentReceipt, Self, ManagedSocketEvents), ManagedError> {
        let settings = settings.validate()?;
        let mut endpoint = client.url("v1/agents/live")?;
        set_websocket_scheme(&mut endpoint)?;
        append_create_settings(&mut endpoint, settings);
        let (connected, ready) = connect_endpoint(&client, endpoint.clone(), None, "0").await?;
        if ready.settings != settings {
            return Err(live_error(
                "managed WebSocket ready settings do not match creation request",
            ));
        }
        let agent_id = ready.session_id.clone();
        validate_id("agent", &agent_id)?;
        let cursor = EventCursor::parse(ready.latest_event_cursor.clone())?;
        let events_url = client
            .url(&format!("{}/events", agent_path(&agent_id)))?
            .to_string();
        let receipt = AgentReceipt {
            agent_id: agent_id.clone(),
            session_id: agent_id.clone(),
            events_url,
            websocket_url: endpoint.to_string(),
            initial_state: Some(AgentState {
                agent_id: agent_id.clone(),
                session_id: agent_id.clone(),
                has_snapshot: ready.restored,
                completed_turns: 0,
                last_active: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64()
                    * 1_000.0,
                active_turns: ready.active_turns,
                agent_loaded: false,
                connected_clients: 1,
                capabilities: ready.capabilities,
                settings: ready.settings,
                latest_event_cursor: ready.latest_event_cursor,
                stream_error: None,
            }),
        };
        let (socket, events) = Self::start(client, agent_id, cursor, Some(connected));
        Ok((receipt, socket, events))
    }

    pub(crate) async fn open(
        client: ManagedClient,
        agent_id: String,
        cursor: EventCursor,
    ) -> Result<(Self, ManagedSocketEvents), ManagedError> {
        validate_id("agent", &agent_id)?;
        // The durable state and history remain usable while the live socket
        // reconnects. Connection establishment belongs to the background loop.
        Ok(Self::start(client, agent_id, cursor, None))
    }

    fn start(
        client: ManagedClient,
        agent_id: String,
        cursor: EventCursor,
        connected: Option<ConnectedSocket>,
    ) -> (Self, ManagedSocketEvents) {
        let (commands, command_rx) = mpsc::channel(1);
        let (event_tx, events) = mpsc::channel(EVENT_CAPACITY);
        tokio::spawn(run(
            client,
            agent_id,
            cursor.as_str().to_owned(),
            connected,
            command_rx,
            event_tx,
            RECOVERY_TIMEOUT,
        ));
        (Self { commands }, ManagedSocketEvents { cursor, events })
    }

    pub(crate) async fn submit(
        &self,
        id: String,
        input: PromptInput,
    ) -> Result<TurnView, ManagedError> {
        self.submit_with_timeout(id, input, SUBMIT_TIMEOUT).await
    }

    async fn submit_with_timeout(
        &self,
        id: String,
        input: PromptInput,
        timeout: Duration,
    ) -> Result<TurnView, ManagedError> {
        validate_idempotency_key(&id)?;
        let id = websocket_turn_id(&id);
        let (result, receiver) = oneshot::channel();
        let submission = async {
            self.commands
                .send(Command::Submit { id, input, result })
                .await
                .map_err(|_| live_error("managed WebSocket stopped before submission"))?;
            receiver.await.map_err(|_| live_error("managed WebSocket stopped during submission; delivery is unknown; retry with the same request ID"))?
        };
        tokio::time::timeout(timeout, submission)
            .await
            .map_err(|_| live_error(
                "managed WebSocket submission acknowledgement timed out; delivery is unknown; retry with the same request ID",
            ))?
    }
}

fn append_create_settings(endpoint: &mut url::Url, settings: AgentSettings) {
    endpoint
        .query_pairs_mut()
        .append_pair("model", settings.model.as_str())
        .append_pair("thinking", settings.thinking.as_str())
        .append_pair("reasoning_mode", settings.reasoning_mode.as_str())
        .append_pair(
            "fast_mode",
            if settings.fast_mode { "true" } else { "false" },
        );
}

impl ManagedSocketEvents {
    pub(crate) const fn cursor(&self) -> &EventCursor {
        &self.cursor
    }
}

impl ManagedEventSource for ManagedSocketEvents {
    fn cursor(&self) -> &EventCursor {
        &self.cursor
    }

    fn next(&mut self) -> ManagedEventFuture<'_> {
        Box::pin(async move {
            let event = self
                .events
                .recv()
                .await
                .ok_or_else(|| live_error("managed WebSocket event stream stopped"))??;
            if !self.cursor.observe(event.cursor.clone())? {
                return Err(live_error("managed WebSocket emitted a stale event"));
            }
            Ok(event)
        })
    }
}

async fn run(
    client: ManagedClient,
    agent_id: String,
    mut cursor: String,
    mut connected: Option<ConnectedSocket>,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Result<ManagedEvent, ManagedError>>,
    recovery_timeout: Duration,
) {
    let mut pending: Option<PendingSubmit> = None;
    let mut backoff = RECONNECT_MIN;
    let mut preparation_fallback_started = false;
    let mut last_connect_error = String::from("connection repeatedly closed");
    let mut recovery_deadline = tokio::time::Instant::now() + recovery_timeout;
    loop {
        if connected.is_none() {
            // Check explicitly: timeout_at can accept an immediately ready
            // connection even after its deadline has elapsed.
            let attempt = if tokio::time::Instant::now() >= recovery_deadline {
                None
            } else {
                tokio::select! {
                    attempt = tokio::time::timeout_at(recovery_deadline, connect(&client, &agent_id, &cursor)) => attempt.ok(),
                    () = events.closed() => return,
                }
            };
            let Some(attempt) = attempt else {
                // Stop the command receiver before publishing failure. Queued
                // submissions must not be delivered after recovery takes over.
                commands.close();
                drop(pending.take());
                while commands.try_recv().is_ok() {}
                let _ = events.send(Err(live_error(format!("managed WebSocket recovery timed out: {last_connect_error}; submission delivery may be unknown")))).await;
                return;
            };
            match attempt {
                Ok(socket) => connected = Some(socket),
                Err(error) => {
                    last_connect_error = error.to_string();
                    tokio::select! {
                        () = tokio::time::sleep_until((tokio::time::Instant::now() + backoff).min(recovery_deadline)) => {},
                        () = events.closed() => return,
                    }
                    backoff = (backoff * 2).min(RECONNECT_MAX);
                    continue;
                }
            }
        }
        let mut live = connected.take().expect("socket was connected above");
        if !live.preparation_accepted && !preparation_fallback_started {
            // Older Workers ignore the upgrade opt-in. Keep their best-effort
            // HTTP preparation, once per driver, without delaying submission.
            client.prepare_active_conversation(&agent_id);
            preparation_fallback_started = true;
        }
        if let Some(pending) = pending.as_mut() {
            pending.sent_at = None;
        }
        let connected_at = tokio::time::Instant::now();
        let disconnected = connection(
            &mut live.socket,
            &mut commands,
            &events,
            &mut pending,
            &mut cursor,
            &live.replay_through,
        )
        .await;
        if !disconnected || events.is_closed() {
            return;
        }
        if connected_at.elapsed() >= HEARTBEAT_INTERVAL {
            recovery_deadline = tokio::time::Instant::now() + recovery_timeout;
        }
        if connected_at.elapsed() >= Duration::from_millis(250) {
            backoff = RECONNECT_MIN;
        }
        tokio::select! {
            () = tokio::time::sleep_until((tokio::time::Instant::now() + backoff).min(recovery_deadline)) => {}
            () = events.closed() => return,
        }
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

async fn connection(
    socket: &mut Socket,
    commands: &mut mpsc::Receiver<Command>,
    events: &mpsc::Sender<Result<ManagedEvent, ManagedError>>,
    pending: &mut Option<PendingSubmit>,
    cursor: &mut String,
    replay_through: &str,
) -> bool {
    let mut last_received = tokio::time::Instant::now();
    let mut heartbeat =
        tokio::time::interval_at(last_received + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        // A timed-out or cancelled caller no longer authorizes a future send.
        // An already sent prompt may still have been admitted: never invent a
        // replacement ID or describe cancellation as proof of non-delivery.
        if pending
            .as_ref()
            .is_some_and(|submission| submission.result.is_closed())
        {
            pending.take();
        }
        if let Some(submission) = pending.as_mut()
            && submission.sent_at.is_none()
            && !crate::sse::cursor_before(cursor, replay_through)
        {
            if !matches!(
                tokio::time::timeout(CONNECT_TIMEOUT, send_prompt(socket, submission)).await,
                Ok(Ok(()))
            ) {
                return true;
            }
            submission.sent_at = Some(tokio::time::Instant::now());
        }
        let admission_deadline = pending
            .as_ref()
            .and_then(|pending| pending.sent_at)
            .map(|sent| sent + Duration::from_secs(30));
        tokio::select! {
            () = tokio::time::sleep_until(admission_deadline.unwrap_or_else(tokio::time::Instant::now)), if admission_deadline.is_some() => {
                // Prompt IDs are durable idempotency keys. A lost admission
                // acknowledgement is recovered by reconnecting and replaying
                // the same ID, even when heartbeat traffic still succeeds.
                return true;
            },
            () = events.closed() => return false,
            _ = heartbeat.tick() => {
                if last_received.elapsed() >= HEARTBEAT_TIMEOUT {
                    return true;
                }
                // Application-level ping also verifies that the Worker can
                // process messages, rather than just its WebSocket proxy.
                if !matches!(tokio::time::timeout(CONNECT_TIMEOUT,
                    socket.send(Message::Text(r#"{"type":"ping"}"#.into()))).await, Ok(Ok(()))) {
                    return true;
                }
            },
            command = commands.recv(), if pending.is_none() => match command {
                Some(Command::Submit { id, input, result }) => {
                    *pending = Some(PendingSubmit { id, input, result, sent_at: None });
                }
                None => return false,
            },
            message = socket.next() => match message {
                Some(Ok(Message::Text(encoded))) => {
                    last_received = tokio::time::Instant::now();
                    if handle_message(encoded.as_str(), events, pending, cursor).await.is_err() {
                        return false;
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    last_received = tokio::time::Instant::now();
                    if !matches!(tokio::time::timeout(CONNECT_TIMEOUT, socket.send(Message::Pong(payload))).await, Ok(Ok(()))) {
                        return true;
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return true,
                Some(Ok(_)) => {}
            }
        }
    }
}

async fn handle_message(
    encoded: &str,
    events: &mpsc::Sender<Result<ManagedEvent, ManagedError>>,
    pending: &mut Option<PendingSubmit>,
    cursor: &mut String,
) -> Result<(), ()> {
    let kind: MessageKind = match serde_json::from_str(encoded) {
        Ok(kind) => kind,
        Err(error) => {
            let _ = events
                .send(Err(live_error(format!(
                    "managed WebSocket sent invalid JSON: {error}"
                ))))
                .await;
            return Err(());
        }
    };
    if matches!(kind.kind.as_str(), "ready" | "status" | "pong") {
        return Ok(());
    }
    if kind.kind == "error" {
        #[derive(Deserialize)]
        struct ErrorMessage {
            code: String,
            message: String,
        }
        let error = serde_json::from_str::<ErrorMessage>(encoded)
            .map(|error| live_error(format!("{}: {}", error.code, error.message)))
            .unwrap_or_else(|_| live_error("managed WebSocket command failed"));
        if let Some(submission) = pending.take() {
            let _ = submission.result.send(Err(error));
            return Ok(());
        }
        let _ = events.send(Err(error)).await;
        return Err(());
    }
    let event: ManagedEvent = match serde_json::from_str(encoded) {
        Ok(event) => event,
        Err(error) => {
            let _ = events
                .send(Err(live_error(format!(
                    "managed WebSocket event is malformed: {error}"
                ))))
                .await;
            return Err(());
        }
    };
    let is_new = crate::sse::cursor_before(cursor, &event.cursor);
    if let Some(submission) = pending.as_ref()
        && event.data.turn_id() == Some(submission.id.as_str())
        && matches!(
            event.data,
            ManagedEventData::TurnAccepted { .. }
                | ManagedEventData::TurnCancelling { .. }
                | ManagedEventData::TurnCompleted { .. }
                | ManagedEventData::TurnCancelled { .. }
                | ManagedEventData::TurnRetryable { .. }
                | ManagedEventData::TurnFailed { .. }
        )
    {
        let submission = pending
            .take()
            .expect("pending submission was just observed");
        let view = turn_view(&event, submission.input);
        let _ = submission.result.send(Ok(view));
    }
    if is_new {
        cursor.clone_from(&event.cursor);
        events.send(Ok(event)).await.map_err(|_| ())?;
    }
    Ok(())
}

fn turn_view(event: &ManagedEvent, input: PromptInput) -> TurnView {
    let (state, terminal, error, retry_at) = match &event.data {
        ManagedEventData::TurnCancelling {
            error, retry_at, ..
        } => (TurnState::Cancelling, None, error.clone(), *retry_at),
        ManagedEventData::TurnCompleted { .. } => {
            (TurnState::Completed, Some(event.data.clone()), None, None)
        }
        ManagedEventData::TurnCancelled { .. } => {
            (TurnState::Cancelled, Some(event.data.clone()), None, None)
        }
        ManagedEventData::TurnFailed { error, .. } => (
            TurnState::Failed,
            Some(event.data.clone()),
            Some(error.clone()),
            None,
        ),
        ManagedEventData::TurnRetryable { error, .. } => {
            (TurnState::Accepted, None, Some(error.clone()), None)
        }
        _ => (TurnState::Accepted, None, None, None),
    };
    let terminal_cursor = terminal.as_ref().map(|_| event.cursor.clone());
    TurnView {
        turn_id: event.data.turn_id().unwrap_or_default().to_owned(),
        state,
        input,
        accepted_cursor: event.cursor.clone(),
        terminal_cursor,
        created_at: event.created_at.unwrap_or_default(),
        accepted_at: event.created_at.unwrap_or_default(),
        updated_at: event.created_at.unwrap_or_default(),
        attempt_count: 1,
        retry_at,
        error,
        terminal,
    }
}

pub(crate) fn websocket_turn_id(request_id: &str) -> String {
    if validate_id("turn", request_id).is_ok() {
        return request_id.to_owned();
    }
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, request_id.as_bytes()).to_string()
}

async fn send_prompt(socket: &mut Socket, submission: &PendingSubmit) -> Result<(), ManagedError> {
    let encoded = serde_json::to_string(&ClientMessage::Prompt {
        id: &submission.id,
        input: &submission.input,
    })
    .map_err(|_| live_error("failed to encode managed WebSocket prompt"))?;
    socket
        .send(Message::Text(encoded.into()))
        .await
        .map_err(|error| live_error(format!("managed WebSocket send failed: {error}")))
}

async fn connect(
    client: &ManagedClient,
    agent_id: &str,
    cursor: &str,
) -> Result<ConnectedSocket, ManagedError> {
    let mut endpoint = client.url(&format!("{}/ws", agent_path(agent_id)))?;
    set_websocket_scheme(&mut endpoint)?;
    endpoint.query_pairs_mut().append_pair("cursor", cursor);
    let (connected, _) = connect_endpoint(client, endpoint, Some(agent_id), cursor).await?;
    Ok(connected)
}

fn set_websocket_scheme(endpoint: &mut url::Url) -> Result<(), ManagedError> {
    endpoint
        .set_scheme(match endpoint.scheme() {
            "http" => "ws",
            "https" => "wss",
            _ => return Err(live_error("managed WebSocket origin is not HTTP(S)")),
        })
        .map_err(|_| live_error("failed to derive managed WebSocket endpoint"))
}

async fn connect_endpoint(
    client: &ManagedClient,
    endpoint: url::Url,
    expected_agent_id: Option<&str>,
    cursor: &str,
) -> Result<(ConnectedSocket, ReadyMessage), ManagedError> {
    let mut request = endpoint
        .as_str()
        .into_client_request()
        .map_err(|error| live_error(format!("invalid managed WebSocket request: {error}")))?;
    let mut authorization = format!("Bearer {}", client.bearer)
        .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
        .map_err(|_| live_error("managed bearer credential cannot form a WebSocket header"))?;
    authorization.set_sensitive(true);
    request.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        authorization,
    );
    request.headers_mut().insert(
        PREPARE_HEADER,
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(PREPARE_ACTIVE_CONVERSATION),
    );
    if let Some(origin) = &client.request_origin {
        request
            .headers_mut()
            .insert("x-nanocodex-client-context", origin.clone());
    }
    // The service's ingress limit applies to client writes, not event reads.
    // Retain transport backpressure without imposing tungstenite's default
    // 16 MiB frame / 64 MiB message ceiling on durable event replay.
    let config = WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None);
    let (mut socket, response) = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let tls_timing = ConnectionTiming::new("managed_native_tls");
        let connector = if request.uri().scheme_str() == Some("wss") {
            Some(tokio_tungstenite::Connector::Rustls(
                nanocodex_oai_api::tls::native_client_config().await?,
            ))
        } else {
            None
        };
        drop(tls_timing);
        let _upgrade_timing = ConnectionTiming::new("managed_ws_upgrade");
        connect_async_tls_with_config(request, Some(config), true, connector).await
    })
    .await
    .map_err(|_| live_error("managed WebSocket handshake timed out"))?
    .map_err(|error| live_error(format!("managed WebSocket handshake failed: {error}")))?;
    let _ready_timing = ConnectionTiming::new("managed_ws_ready");
    match tokio::time::timeout(CONNECT_TIMEOUT, socket.next())
        .await
        .map_err(|_| live_error("managed WebSocket ready timed out"))?
    {
        Some(Ok(Message::Text(encoded))) => {
            let ready: ReadyMessage = serde_json::from_str(encoded.as_str())
                .map_err(|_| live_error("managed WebSocket ready frame is malformed"))?;
            if ready.kind != "ready" {
                return Err(live_error("managed WebSocket did not begin with ready"));
            }
            if !ready.settings.is_valid() {
                return Err(live_error(
                    "managed WebSocket ready settings are incompatible",
                ));
            }
            if expected_agent_id.is_some_and(|agent_id| ready.session_id != agent_id) {
                return Err(live_error(
                    "managed WebSocket ready session does not match agent",
                ));
            }
            crate::sse::validate_numeric_cursor(&ready.latest_event_cursor)?;
            if crate::sse::cursor_before(&ready.latest_event_cursor, cursor) {
                return Err(live_error(
                    "managed WebSocket ready cursor is behind request",
                ));
            }
            Ok((
                ConnectedSocket {
                    socket,
                    replay_through: ready.latest_event_cursor.clone(),
                    preparation_accepted: response
                        .headers()
                        .get(PREPARE_HEADER)
                        .is_some_and(|value| value == PREPARE_ACTIVE_CONVERSATION),
                },
                ready,
            ))
        }
        _ => Err(live_error("managed WebSocket closed before ready")),
    }
}

// Opt-in, content-free connection phases share the CLI startup diagnostic.
// Only static stage names and monotonic durations enter stderr; requests,
// responses, origins, identifiers and credentials are never formatted here.
struct ConnectionTiming {
    stage: &'static str,
    started: Option<Instant>,
}

impl ConnectionTiming {
    fn new(stage: &'static str) -> Self {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        let enabled = *ENABLED.get_or_init(|| {
            std::env::var_os("NANOCODEX_STARTUP_TIMING").is_some_and(|value| value == "1")
        });
        Self {
            stage,
            started: enabled.then(Instant::now),
        }
    }
}

impl Drop for ConnectionTiming {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            eprintln!(
                "{}",
                serde_json::json!({
                    "type": "client.startup", "stage": self.stage,
                    "duration_ms": started.elapsed().as_secs_f64() * 1000.0,
                })
            );
        }
    }
}

fn live_error(message: impl Into<String>) -> ManagedError {
    ManagedError::InvalidEvent(message.into())
}

#[cfg(test)]
mod tests {
    use nanocodex_oai_api::{Model, ReasoningMode, Thinking};
    use serde_json::json;

    use super::{Message, ReadyMessage, append_create_settings};
    use crate::AgentSettings;

    #[tokio::test]
    async fn unavailable_socket_fails_events_and_queued_submissions() {
        use super::*;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let client = ManagedClient::new(
            format!("http://{address}"),
            crate::ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)))
                .unwrap(),
        )
        .unwrap();
        let (commands, command_rx) = mpsc::channel(1);
        let (events, mut event_rx) = mpsc::channel(1);
        let socket = ManagedSocket { commands };
        let worker = tokio::spawn(run(
            client,
            "agent-1".into(),
            "0".into(),
            None,
            command_rx,
            events,
            Duration::from_millis(20),
        ));
        let submit = socket.submit("request-1".into(), PromptInput::Text("hello".into()));
        let (result, event) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(submit, event_rx.recv())
        })
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(
            event
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("recovery timed out")
        );
        worker.await.unwrap();
        assert!(socket.commands.is_closed());
    }

    #[tokio::test]
    async fn submission_timeout_covers_queue_wait_and_revokes_unsent_work() {
        use super::*;
        let (commands, mut queued) = mpsc::channel(1);
        let socket = ManagedSocket { commands };
        let result = socket
            .submit_with_timeout(
                "request-1".into(),
                PromptInput::Text("hello".into()),
                Duration::from_millis(10),
            )
            .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("delivery is unknown")
        );
        // The occupied queue must not make subsequent submissions wait forever.
        let result = socket
            .submit_with_timeout(
                "request-2".into(),
                PromptInput::Text("hello".into()),
                Duration::from_millis(10),
            )
            .await;
        assert!(result.is_err());
        let Command::Submit { id, result, .. } = queued.recv().await.unwrap();
        assert_eq!(id, websocket_turn_id("request-1"));
        assert!(result.is_closed());
        assert!(queued.try_recv().is_err());
    }

    fn test_ready(cursor: &str) -> Message {
        Message::Text(
            json!({ "type": "ready", "session_id": "agent-1", "restored": true,
            "active_turns": [], "latest_event_cursor": cursor,
            "capabilities": { "durable_turns": true, "resumable_events": true,
                "workspace": "cloud", "execution_environments": true,
                "execution_namespace": "cwd-root-v1", "native_cross_mounts": false },
            "settings": { "model": "gpt-6-astra", "thinking": "low",
                "reasoning_mode": "standard", "fast_mode": false } })
            .to_string()
            .into(),
        )
    }

    #[tokio::test]
    async fn rapid_ready_close_cycles_exhaust_recovery_budget() {
        use super::*;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = connections.clone();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_hdr_async(
                    stream,
                    |_: &tokio_tungstenite::tungstenite::handshake::server::Request,
                     mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                        response.headers_mut().insert(
                            PREPARE_HEADER,
                            tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
                                PREPARE_ACTIVE_CONVERSATION,
                            ),
                        );
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                socket.send(test_ready("0")).await.unwrap();
                socket.close(None).await.unwrap();
            }
        });
        let client = ManagedClient::new(
            format!("http://{address}"),
            crate::ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)))
                .unwrap(),
        )
        .unwrap();
        let (_commands, command_rx) = mpsc::channel(1);
        let (events, mut event_rx) = mpsc::channel(1);
        let worker = tokio::spawn(run(
            client,
            "agent-1".into(),
            "0".into(),
            None,
            command_rx,
            events,
            Duration::from_millis(350),
        ));
        // The third close starts a 400ms backoff; it must stop at 350ms,
        // rather than complete that sleep at roughly 700ms.
        let event = tokio::time::timeout(Duration::from_millis(600), event_rx.recv())
            .await
            .unwrap();
        assert!(
            event
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("recovery timed out")
        );
        worker.await.unwrap();
        assert!(connections.load(std::sync::atomic::Ordering::SeqCst) >= 2);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn cancelled_unsent_submission_never_reaches_network() {
        use super::*;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.send(test_ready("0")).await.unwrap();
            let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let value: serde_json::Value =
                serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(value["id"], websocket_turn_id("live-request"));
        });
        let client = ManagedClient::new(
            format!("http://{address}"),
            crate::ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)))
                .unwrap(),
        )
        .unwrap();
        let (commands, command_rx) = mpsc::channel(2);
        let (result, receiver) = oneshot::channel();
        drop(receiver);
        commands
            .send(Command::Submit {
                id: websocket_turn_id("cancelled-request"),
                input: PromptInput::Text("cancelled".into()),
                result,
            })
            .await
            .unwrap();
        let (result, _receiver) = oneshot::channel();
        commands
            .send(Command::Submit {
                id: websocket_turn_id("live-request"),
                input: PromptInput::Text("live".into()),
                result,
            })
            .await
            .unwrap();
        let (events, _event_rx) = mpsc::channel(1);
        let worker = tokio::spawn(run(
            client,
            "agent-1".into(),
            "0".into(),
            None,
            command_rx,
            events,
            Duration::from_secs(1),
        ));
        server.await.unwrap();
        worker.abort();
        let _ = worker.await;
    }

    #[tokio::test]
    async fn receives_event_above_default_frame_limit_and_the_following_frame() {
        use crate::{ManagedApiKey, ManagedClient, ManagedEvent};
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let payload = "x".repeat(17 * 1024 * 1024);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let output = payload.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let ready = json!({ "type": "ready", "session_id": "agent-1", "restored": true,
                "active_turns": [], "latest_event_cursor": "0",
                "capabilities": { "durable_turns": true, "resumable_events": true,
                    "workspace": "cloud",
                    "execution_environments": true, "execution_namespace": "cwd-root-v1", "native_cross_mounts": false },
                "settings": { "model": "gpt-6-astra", "thinking": "low", "reasoning_mode": "standard", "fast_mode": false } });
            socket
                .send(Message::Text(ready.to_string().into()))
                .await
                .unwrap();
            let event = json!({ "cursor": "1", "type": "turn_completed", "id": "turn-1",
                "final_message": output, "usage": null, "citations": [] });
            socket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({ "cursor": "2", "type": "turn_cancelled", "id": "turn-2" })
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        let key = format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(key).unwrap(),
        )
        .unwrap();
        let endpoint = url::Url::parse(&format!("ws://{address}/v1/agents/agent-1/ws")).unwrap();
        let (mut connected, _) = super::connect_endpoint(&client, endpoint, Some("agent-1"), "0")
            .await
            .unwrap();
        let event = connected
            .socket
            .next()
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let event: ManagedEvent = serde_json::from_str(event.as_str()).unwrap();
        assert_eq!(
            event.data.terminal_result("turn-1").unwrap().unwrap(),
            payload
        );
        let next = connected
            .socket
            .next()
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<ManagedEvent>(next.as_str())
                .unwrap()
                .cursor,
            "2"
        );
        server.await.unwrap();
    }

    #[test]
    fn create_live_uses_exact_canonical_settings_query() {
        let mut endpoint = url::Url::parse("wss://managed.example/v1/agents/live")
            .expect("fixture URL should parse");
        append_create_settings(
            &mut endpoint,
            AgentSettings {
                model: Model::Astra,
                thinking: Thinking::Max,
                reasoning_mode: ReasoningMode::Standard,
                fast_mode: true,
            },
        );
        assert_eq!(
            endpoint.query(),
            Some("model=gpt-6-astra&thinking=max&reasoning_mode=standard&fast_mode=true")
        );
    }

    #[test]
    fn ready_settings_are_required_and_typed() {
        let mut ready = json!({
            "type": "ready",
            "session_id": "agent-1",
            "restored": false,
            "active_turns": [],
            "capabilities": {
                "durable_turns": true,
                "resumable_events": true,
                "workspace": "cloud",
                "execution_environments": true,
                "execution_namespace": "cwd-root-v1",
                "native_cross_mounts": false
            },
            "latest_event_cursor": "0"
        });
        assert!(serde_json::from_value::<ReadyMessage>(ready.clone()).is_err());

        ready["settings"] = json!({
            "model": "gpt-6-astra",
            "thinking": "low",
            "reasoning_mode": "standard",
            "fast_mode": true
        });
        let parsed: ReadyMessage =
            serde_json::from_value(ready).expect("ready settings should deserialize");
        assert_eq!(parsed.settings.model, Model::Astra);
        assert_eq!(parsed.settings.thinking, Thinking::Low);
        assert!(parsed.settings.fast_mode);
        assert_eq!(parsed.capabilities.execution_namespace, "cwd-root-v1");
        assert!(!parsed.capabilities.native_cross_mounts);
    }
}
