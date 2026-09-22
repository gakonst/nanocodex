use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{
    client_async_tls_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};
use tracing::Instrument as _;
use url::Url;

use super::protocol::{self, ExecutorFrame, RemoteFrame};
use super::{
    AttachmentCallOutcome, AttachmentError, AttachmentEvent, AttachmentMetadata, AttachmentStatus,
};
use crate::prepared::{PreparedToolCall, PreparedToolError, PreparedToolRuntime};

#[cfg(not(test))]
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(100);
#[cfg(not(test))]
const PONG_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const PONG_TIMEOUT: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const STABLE_CONNECTION: Duration = Duration::from_secs(30);
#[cfg(test)]
const STABLE_CONNECTION: Duration = Duration::from_millis(250);

const MAX_ACTIVE_CALLS: usize = 32;

pub(crate) struct Config {
    pub(crate) endpoint: Url,
    pub(crate) authorization: Box<str>,
    pub(crate) tools: Value,
    pub(crate) metadata: Option<AttachmentMetadata>,
}

pub(crate) enum Command {
    Detach,
}

pub(crate) async fn run(
    config: Config,
    runtime: Arc<PreparedToolRuntime>,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<AttachmentEvent>,
    status: watch::Sender<AttachmentStatus>,
    closed: watch::Sender<Option<Result<(), AttachmentError>>>,
) {
    let mut active = Vec::<InFlight>::new();
    let mut backoff = Duration::from_millis(100);
    let terminal = loop {
        let _ = status.send(AttachmentStatus::Connecting);
        emit(&events, AttachmentEvent::Connecting);
        let request = match request(&config) {
            Ok(request) => request,
            Err(error) => break Err(error),
        };
        let connect_started = Instant::now();
        let connected = tokio::select! {
            command = commands.recv() => match command { Some(Command::Detach) | None => break Ok(()) },
            connected = async {
                let connector = if request.uri().scheme_str() == Some("wss") {
                    Some(tokio_tungstenite::Connector::Rustls(nanocodex_oai_api::tls::native_client_config().await?))
                } else { None };
                tracing::info!(target: "nanocodex_tools::attachment",
                    stage = "attachment.socket.trust",
                    elapsed_ms = connect_started.elapsed().as_secs_f64() * 1000.0,
                    "attachment TLS trust ready");
                let host = request.uri().host().ok_or(tokio_tungstenite::tungstenite::Error::Url(
                    tokio_tungstenite::tungstenite::error::UrlError::NoHostName,
                ))?;
                let host = host.trim_start_matches('[').trim_end_matches(']');
                let port = request.uri().port_u16().unwrap_or(if connector.is_some() { 443 } else { 80 });
                let addresses: Vec<_> = tokio::net::lookup_host((host, port)).await?.collect();
                tracing::info!(target: "nanocodex_tools::attachment",
                    stage = "attachment.socket.resolved",
                    elapsed_ms = connect_started.elapsed().as_secs_f64() * 1000.0,
                    "attachment address resolved");
                let stream = tokio::net::TcpStream::connect(addresses.as_slice()).await?;
                stream.set_nodelay(true)?;
                tracing::info!(target: "nanocodex_tools::attachment",
                    stage = "attachment.socket.tcp",
                    elapsed_ms = connect_started.elapsed().as_secs_f64() * 1000.0,
                    "attachment TCP connected");
                client_async_tls_with_config(request, stream, None, connector).await
            } => connected,
        };
        let socket = match connected {
            Ok((socket, response)) => {
                tracing::info!(target: "nanocodex_tools::attachment",
                    stage = "attachment.websocket_connected",
                    duration_ms = connect_started.elapsed().as_secs_f64() * 1000.0,
                    request_id = response.headers().get("x-nanocodex-request-id").and_then(|v| v.to_str().ok()).unwrap_or(""),
                    "attachment WebSocket connected");
                socket
            }
            Err(tokio_tungstenite::tungstenite::Error::Http(response))
                if matches!(response.status().as_u16(), 401 | 403) =>
            {
                break Err(AttachmentError::Authentication(
                    "endpoint rejected the bearer credential".into(),
                ));
            }
            Err(_) => {
                let _ = status.send(AttachmentStatus::Disconnected);
                if wait_backoff(&mut commands, backoff).await {
                    break Ok(());
                }
                backoff = (backoff * 2).min(Duration::from_secs(5));
                continue;
            }
        };
        let connected_at = Instant::now();
        let end = connection(
            socket,
            ConnectionContext {
                config: &config,
                runtime: &runtime,
                events: &events,
                status: &status,
                active: &mut active,
            },
            &mut commands,
        )
        .await;
        if matches!(*status.borrow(), AttachmentStatus::Ready)
            && connected_at.elapsed() >= STABLE_CONNECTION
        {
            backoff = Duration::from_millis(100);
        }
        match end {
            ConnectionEnd::Detached => break Ok(()),
            ConnectionEnd::DetachFailed(error) => break Err(error),
            ConnectionEnd::Rejected(reason) => {
                let _ = status.send(AttachmentStatus::Fenced);
                emit(
                    &events,
                    AttachmentEvent::Fenced {
                        reason: reason.clone(),
                    },
                );
                break Err(AttachmentError::Fenced(reason));
            }
            ConnectionEnd::Failed(_) | ConnectionEnd::Disconnected => {
                let _ = status.send(AttachmentStatus::Disconnected);
                if wait_backoff(&mut commands, backoff).await {
                    break Ok(());
                }
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
    };
    shutdown_calls(&mut active).await;
    runtime.shutdown().await;
    if terminal.is_ok() {
        emit(
            &events,
            AttachmentEvent::Detached {
                reason: "closed".into(),
            },
        );
    }
    let _ = closed.send(Some(terminal));
}

fn request(config: &Config) -> Result<http::Request<()>, AttachmentError> {
    let mut request = config
        .endpoint
        .as_str()
        .into_client_request()
        .map_err(|error| AttachmentError::Transport(error.to_string().into()))?;
    let mut authorization = http::HeaderValue::from_str(&config.authorization)
        .map_err(|_| AttachmentError::Authentication("invalid bearer credential".into()))?;
    authorization.set_sensitive(true);
    request
        .headers_mut()
        .insert(http::header::AUTHORIZATION, authorization);
    Ok(request)
}

async fn wait_backoff(commands: &mut mpsc::Receiver<Command>, delay: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        command = commands.recv() => matches!(command, Some(Command::Detach) | None),
    }
}

enum ConnectionEnd {
    Detached,
    DetachFailed(AttachmentError),
    Disconnected,
    Failed(AttachmentError),
    Rejected(Box<str>),
}

enum Completion {
    Result { call_id: Box<str>, outcome: Value },
}

struct InFlight {
    task: tokio::task::JoinHandle<()>,
    parallel_safe: bool,
}

async fn shutdown_calls(active: &mut Vec<InFlight>) {
    for call in active.iter() {
        call.task.abort();
    }
    for call in active.drain(..) {
        let _ = call.task.await;
    }
}

// Completion telemetry follows execution, even after the receiving socket is gone.
struct TaskEvents {
    call: Option<CallEvents>,
    events: mpsc::Sender<AttachmentEvent>,
}

impl Drop for TaskEvents {
    fn drop(&mut self) {
        if let Some(call) = self.call.take() {
            call.complete(&self.events, AttachmentCallOutcome::Ambiguous);
        }
    }
}

struct CallEvents {
    call_id: Box<str>,
    span: tracing::Span,
    started_at: Instant,
}

impl CallEvents {
    fn complete(self, events: &mpsc::Sender<AttachmentEvent>, outcome: AttachmentCallOutcome) {
        let status = attachment_call_outcome_name(outcome);
        let duration_ns = u64::try_from(self.started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.span.record("status", status);
        self.span.record(
            "otel.status_code",
            if matches!(outcome, AttachmentCallOutcome::Completed) {
                "OK"
            } else {
                "ERROR"
            },
        );
        self.span.record("duration_ns", duration_ns);
        self.span.in_scope(|| {
            record_attachment_call_completion(outcome);
        });
        emit(
            events,
            AttachmentEvent::CallCompleted {
                call_id: self.call_id,
                outcome,
            },
        );
    }
}

fn begin_call_events(
    events: &mpsc::Sender<AttachmentEvent>,
    call_id: Box<str>,
    name: Box<str>,
    attachment_id: Option<&str>,
) -> CallEvents {
    let span = tracing::info_span!(
        target: "nanocodex_tools::attachment",
        "attachment.call",
        otel.kind = "server",
        otel.status_code = tracing::field::Empty,
        attachment.id = tracing::field::Empty,
        tool.name = name.as_ref(),
        tool.call_id = call_id.as_ref(),
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    );
    if let Some(attachment_id) = attachment_id {
        span.record("attachment.id", attachment_id);
    }
    span.in_scope(|| {
        tracing::info!(
            target: "nanocodex_tools::attachment",
            stage = "attachment.call.started",
            "attachment call started"
        );
    });
    emit(
        events,
        AttachmentEvent::CallStarted {
            call_id: call_id.clone(),
            name,
        },
    );
    CallEvents {
        call_id,
        span,
        started_at: Instant::now(),
    }
}

fn record_attachment_call_completion(outcome: AttachmentCallOutcome) {
    if matches!(
        outcome,
        AttachmentCallOutcome::Unavailable | AttachmentCallOutcome::Ambiguous
    ) {
        tracing::warn!(
            target: "nanocodex_tools::attachment",
            stage = "attachment.call.completed",
            "attachment call completed"
        );
    } else {
        tracing::info!(
            target: "nanocodex_tools::attachment",
            stage = "attachment.call.completed",
            "attachment call completed"
        );
    }
}

const fn attachment_call_outcome_name(outcome: AttachmentCallOutcome) -> &'static str {
    match outcome {
        AttachmentCallOutcome::Completed => "completed",
        AttachmentCallOutcome::Unavailable => "unavailable",
        AttachmentCallOutcome::Ambiguous => "ambiguous",
        AttachmentCallOutcome::Cancelled => "cancelled",
    }
}

struct CallIdentity {
    session_id: Box<str>,
    turn_id: Option<Box<str>>,
    call_id: Box<str>,
    model: Box<str>,
    name: Box<str>,
    input: Value,
    output_token_budget: u64,
    output_byte_budget: u64,
    deadline_at: u64,
}

fn start_call(
    runtime: &Arc<PreparedToolRuntime>,
    active: &mut Vec<InFlight>,
    identity: CallIdentity,
    tool_timeout: u64,
    events: CallEvents,
    completed: mpsc::UnboundedSender<Completion>,
    event_sender: &mpsc::Sender<AttachmentEvent>,
) -> tokio::task::AbortHandle {
    let parallel_safe = runtime.parallel_safe(&identity.name);
    let runtime = Arc::clone(runtime);
    let task_span = events.span.clone();
    let mut events = TaskEvents {
        call: Some(events),
        events: event_sender.clone(),
    };
    let task = tokio::spawn(
        async move {
            let task_identity = identity;
            let remaining = task_identity.deadline_at.saturating_sub(now_ms());
            let (outcome, observed) = if remaining == 0 {
                (
                    unavailable("tool deadline elapsed before execution"),
                    AttachmentCallOutcome::Unavailable,
                )
            } else {
                let duration = Duration::from_millis(remaining.min(tool_timeout));
                let call = PreparedToolCall::new(
                    task_identity.model.to_string(),
                    task_identity.session_id.to_string(),
                    task_identity.call_id.to_string(),
                    task_identity.name.to_string(),
                    task_identity.input.clone(),
                    task_identity.output_token_budget as usize,
                )
                .with_turn_id(task_identity.turn_id.as_deref().map(str::to_owned));
                match tokio::time::timeout(duration, runtime.execute(call)).await {
                    Ok(Ok(output)) => match serde_json::to_value(output) {
                        Ok(output)
                            if serde_json::to_vec(&output).is_ok_and(|bytes| {
                                bytes.len() as u64 <= task_identity.output_byte_budget
                            }) =>
                        {
                            (
                                json!({"status":"completed", "output":output}),
                                AttachmentCallOutcome::Completed,
                            )
                        }
                        Ok(_) => bounded_completed_failure(
                            "tool output exceeded byte budget",
                            task_identity.output_byte_budget,
                        ),
                        Err(_) => (
                            ambiguous("tool output could not be encoded"),
                            AttachmentCallOutcome::Ambiguous,
                        ),
                    },
                    Ok(Err(error @ PreparedToolError::InvalidOutput(_))) => (
                        ambiguous(&error.to_string()),
                        AttachmentCallOutcome::Ambiguous,
                    ),
                    Ok(Err(error)) => (
                        unavailable(&error.to_string()),
                        AttachmentCallOutcome::Unavailable,
                    ),
                    Err(_) => (
                        ambiguous("tool deadline elapsed"),
                        AttachmentCallOutcome::Ambiguous,
                    ),
                }
            };
            if let Some(call) = events.call.take() {
                call.complete(&events.events, observed);
            }
            // This channel belongs only to the socket that dispatched the call.
            let _ = completed.send(Completion::Result {
                call_id: task_identity.call_id,
                outcome,
            });
        }
        .instrument(task_span),
    );
    let abort = task.abort_handle();
    active.push(InFlight {
        task,
        parallel_safe,
    });
    abort
}

struct ConnectionContext<'a> {
    config: &'a Config,
    runtime: &'a Arc<PreparedToolRuntime>,
    events: &'a mpsc::Sender<AttachmentEvent>,
    status: &'a watch::Sender<AttachmentStatus>,
    active: &'a mut Vec<InFlight>,
}

async fn connection<S>(
    mut socket: tokio_tungstenite::WebSocketStream<S>,
    context: ConnectionContext<'_>,
    commands: &mut mpsc::Receiver<Command>,
) -> ConnectionEnd
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let ConnectionContext {
        config,
        runtime,
        events,
        status,
        active,
    } = context;
    let catalog_started = Instant::now();
    if let Err(error) = send(
        &mut socket,
        &ExecutorFrame::Catalog {
            capabilities: ["turn_metadata"],
            tools: &config.tools,
            machines: config
                .metadata
                .as_ref()
                .and_then(AttachmentMetadata::attached_machine)
                .map(std::slice::from_ref),
            attachment_id: config
                .metadata
                .as_ref()
                .map(AttachmentMetadata::attachment_id),
        },
    )
    .await
    {
        return ConnectionEnd::Failed(error);
    }
    let catalog_sent = Instant::now();
    match next_handshake_frame(&mut socket, commands).await {
        Ok(RemoteFrame::Ready {}) => {}
        Ok(frame) => {
            return reject(
                &mut socket,
                format!("expected ready after catalog, received {}", frame.kind()),
            )
            .await;
        }
        Err(ConnectionEnd::Rejected(reason)) => return reject(&mut socket, reason).await,
        Err(end) => return end,
    }
    tracing::info!(target: "nanocodex_tools::attachment",
        stage = "attachment.catalog_ready",
        send_ms = catalog_sent.duration_since(catalog_started).as_secs_f64() * 1000.0,
        acknowledge_ms = catalog_sent.elapsed().as_secs_f64() * 1000.0,
        "attachment catalog acknowledged");
    emit(events, AttachmentEvent::Attached);
    let _ = status.send(AttachmentStatus::Ready);
    emit(
        events,
        AttachmentEvent::CatalogPublished {
            tool_count: config.tools.as_array().map_or(0, Vec::len),
        },
    );

    let (completed_tx, mut completed_rx) = mpsc::unbounded_channel::<Completion>();
    let mut in_flight = HashMap::<Box<str>, tokio::task::AbortHandle>::new();
    let mut receipts = HashSet::<Box<str>>::new();
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + protocol::HEARTBEAT_INTERVAL,
        protocol::HEARTBEAT_INTERVAL,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pong_timeout = Box::pin(tokio::time::sleep(PONG_TIMEOUT));
    let mut awaiting_pong: Option<String> = None;
    let mut detaching = false;
    let mut draining = false;

    let end = loop {
        if detaching && draining && in_flight.is_empty() && receipts.is_empty() {
            break ConnectionEnd::Detached;
        }
        tokio::select! {
            command = commands.recv(), if !detaching => {
                match command { Some(Command::Detach) | None => {} }
                if let Err(error) = send(&mut socket, &ExecutorFrame::Drain {}).await {
                    break ConnectionEnd::DetachFailed(error);
                }
                detaching = true;
                shutdown_calls(active).await;
                for (call_id, _) in in_flight.drain() {
                    let outcome = ambiguous("attachment shut down during execution");
                    if let Err(error) = send_result(&mut socket, &call_id, &outcome).await {
                        return ConnectionEnd::DetachFailed(error);
                    }
                    receipts.insert(call_id);
                }
            }
            _ = &mut pong_timeout, if awaiting_pong.is_some() => {
                break if detaching {
                    ConnectionEnd::DetachFailed(AttachmentError::Transport("heartbeat timed out while draining".into()))
                } else {
                    ConnectionEnd::Disconnected
                };
            },
            _ = heartbeat.tick() => {
                if awaiting_pong.is_some() { break ConnectionEnd::Disconnected; }
                let nonce = uuid::Uuid::new_v4().to_string();
                if let Err(error) = send(&mut socket, &ExecutorFrame::Ping { nonce: &nonce }).await {
                    break if detaching { ConnectionEnd::DetachFailed(error) } else { ConnectionEnd::Failed(error) };
                }
                awaiting_pong = Some(nonce);
                pong_timeout.as_mut().reset(tokio::time::Instant::now() + PONG_TIMEOUT);
            }
            completion = completed_rx.recv() => if let Some(Completion::Result { call_id, outcome }) = completion {
                if in_flight.remove(&call_id).is_none() { continue; }
                receipts.insert(call_id.clone());
                if let Err(error) = send_result(&mut socket, &call_id, &outcome).await {
                    break if detaching { ConnectionEnd::DetachFailed(error) } else { ConnectionEnd::Failed(error) };
                }
            },
            incoming = socket.next() => {
                let frame = match incoming_frame(&mut socket, incoming).await {
                    Ok(Some(frame)) => frame,
                    Ok(None) => continue,
                    Err(ConnectionEnd::Disconnected) if detaching => break ConnectionEnd::DetachFailed(AttachmentError::Transport("websocket closed while draining".into())),
                    Err(end) => break end,
                };
                match frame {
                    RemoteFrame::Call { session_id, turn_id, call_id, model, name, input, output_token_budget, output_byte_budget, deadline_at } => {
                        if draining { break ConnectionEnd::Rejected("call received after drain barrier".into()); }
                        let identity = CallIdentity { session_id:session_id.into(), turn_id:turn_id.map(Into::into), call_id:call_id.clone().into(), model:model.into(), name:name.clone().into(), input:input.clone(), output_token_budget, output_byte_budget, deadline_at };
                        if receipts.contains(call_id.as_str()) || in_flight.contains_key(call_id.as_str()) {
                            break ConnectionEnd::Rejected("duplicate call on socket".into());
                        }
                        if receipts.len() + in_flight.len() >= 64 {
                            break ConnectionEnd::Disconnected;
                        }
                        let call_events = begin_call_events(
                            events,
                            call_id.clone().into(),
                            name.clone().into(),
                            config.metadata.as_ref().map(AttachmentMetadata::attachment_id),
                        );
                        let tool_timeout = runtime.timeout_ms(&name).unwrap_or(0);
                        let parallel_safe = runtime.parallel_safe(&name);
                        active.retain(|call| !call.task.is_finished());
                        let reason = if tool_timeout == 0 {
                            Some("tool is not in the pinned catalog")
                        } else if deadline_at <= now_ms() {
                            Some("tool deadline elapsed before execution")
                        } else if active.len() >= MAX_ACTIVE_CALLS
                            || active.iter().any(|call| !parallel_safe || !call.parallel_safe) {
                            Some("attachment execution capacity exhausted")
                        } else { None };
                        if let Some(reason) = reason {
                            call_events.complete(events, AttachmentCallOutcome::Unavailable);
                            if let Err(error) = send_result(&mut socket, &call_id, &unavailable(reason)).await { break ConnectionEnd::Failed(error); }
                            receipts.insert(call_id.into());
                            continue;
                        }
                        let task = start_call(runtime, active, identity, tool_timeout, call_events, completed_tx.clone(), events);
                        in_flight.insert(call_id.into(), task);
                    }
                    RemoteFrame::Cancel { call_id } => {
                        if let Some(task) = in_flight.get(call_id.as_str()) {
                            // If execution already finished, its queued result wins the race.
                            if task.is_finished() { continue; }
                            task.abort();
                            in_flight.remove(call_id.as_str());
                            receipts.insert(call_id.clone().into());
                            let outcome = ambiguous("tool execution was cancelled after dispatch");
                            if let Err(error) = send_result(&mut socket, &call_id, &outcome).await { break ConnectionEnd::Failed(error); }
                        }
                    }
                    RemoteFrame::Ack { call_id } => {
                        if !receipts.remove(call_id.as_str()) {
                            break ConnectionEnd::Rejected("acknowledgement did not match a retained result".into());
                        }
                    }
                    RemoteFrame::Pong { nonce } => {
                        let Some(expected) = awaiting_pong.take() else { break ConnectionEnd::Rejected("unexpected pong without an outstanding ping".into()) };
                        if nonce != expected { break ConnectionEnd::Rejected("pong nonce did not match the outstanding ping".into()); }
                    }
                    RemoteFrame::Draining {} => {
                        if !detaching || draining { break ConnectionEnd::Rejected("unexpected draining acknowledgement".into()); }
                        draining = true;
                    }
                    RemoteFrame::Ready {} => break ConnectionEnd::Rejected("unexpected ready".into()),
                }
            }
        }
    };

    let end = if detaching {
        match end {
            ConnectionEnd::Disconnected => ConnectionEnd::DetachFailed(AttachmentError::Transport(
                "websocket disconnected while draining".into(),
            )),
            ConnectionEnd::Failed(error) => ConnectionEnd::DetachFailed(error),
            end => end,
        }
    } else {
        end
    };

    match &end {
        ConnectionEnd::Detached => {
            let _ = socket.close(None).await;
        }
        ConnectionEnd::Rejected(reason) => {
            policy_close(&mut socket, reason).await;
        }
        ConnectionEnd::Disconnected | ConnectionEnd::Failed(_) | ConnectionEnd::DetachFailed(_) => {
        }
    }
    end
}

async fn next_handshake_frame<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    commands: &mut mpsc::Receiver<Command>,
) -> Result<RemoteFrame, ConnectionEnd>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::select! {
        command = commands.recv() => match command { Some(Command::Detach) | None => Err(ConnectionEnd::Detached) },
        frame = tokio::time::timeout(HANDSHAKE_TIMEOUT, next_frame(socket)) => match frame {
            Ok(frame) => frame,
            Err(_) => Err(ConnectionEnd::Failed(AttachmentError::Transport("timed out waiting for attachment readiness".into()))),
        },
    }
}

async fn incoming_frame<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    incoming: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
) -> Result<Option<RemoteFrame>, ConnectionEnd>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match incoming {
        Some(Ok(Message::Text(text))) => RemoteFrame::parse(&text)
            .map(Some)
            .map_err(|reason| ConnectionEnd::Rejected(reason.into())),
        Some(Ok(Message::Ping(payload))) => {
            socket.send(Message::Pong(payload)).await.map_err(|error| {
                ConnectionEnd::Failed(AttachmentError::Transport(error.to_string().into()))
            })?;
            Ok(None)
        }
        Some(Ok(Message::Close(Some(frame)))) if frame.code == CloseCode::Policy => {
            Err(ConnectionEnd::Rejected(if frame.reason.is_empty() {
                "endpoint rejected the attachment".into()
            } else {
                frame.reason.to_string().into()
            }))
        }
        Some(Ok(Message::Close(_))) | None => Err(ConnectionEnd::Disconnected),
        Some(Ok(_)) => Err(ConnectionEnd::Rejected(
            "endpoint sent a non-text frame".into(),
        )),
        Some(Err(error)) => Err(ConnectionEnd::Failed(AttachmentError::Transport(
            error.to_string().into(),
        ))),
    }
}

async fn next_frame<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
) -> Result<RemoteFrame, ConnectionEnd>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let incoming = socket.next().await;
        match incoming_frame(socket, incoming).await? {
            Some(frame) => return Ok(frame),
            None => continue,
        }
    }
}

async fn reject<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    reason: impl Into<Box<str>>,
) -> ConnectionEnd
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let reason = reason.into();
    policy_close(socket, &reason).await;
    ConnectionEnd::Rejected(reason)
}

async fn policy_close<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>, reason: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let reason = bounded(reason);
    let reason = reason
        .get(..reason.len().min(123))
        .unwrap_or("attachment protocol violation");
    let _ = socket
        .close(Some(CloseFrame {
            code: CloseCode::Policy,
            reason: reason.into(),
        }))
        .await;
}

async fn send_result<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    call_id: &str,
    outcome: &Value,
) -> Result<(), AttachmentError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send(socket, &ExecutorFrame::Result { call_id, outcome }).await
}

async fn send<S, T: serde::Serialize>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    frame: &T,
) -> Result<(), AttachmentError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let text = serde_json::to_string(frame)
        .map_err(|error| AttachmentError::Transport(error.to_string().into()))?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|error| AttachmentError::Transport(error.to_string().into()))
}

fn unavailable(message: &str) -> Value {
    json!({"status":"unavailable", "message":bounded(message)})
}
fn ambiguous(message: &str) -> Value {
    json!({"status":"ambiguous", "message":bounded(message)})
}
fn bounded_completed_failure(
    message: &str,
    output_byte_budget: u64,
) -> (Value, AttachmentCallOutcome) {
    let output = json!({"output":bounded(message),"success":false,"structured_result":null,"metadata":null,"process_trace":null});
    if serde_json::to_vec(&output).is_ok_and(|bytes| bytes.len() as u64 <= output_byte_budget) {
        (
            json!({"status":"completed", "output":output}),
            AttachmentCallOutcome::Completed,
        )
    } else {
        (
            ambiguous("tool output exceeded byte budget"),
            AttachmentCallOutcome::Ambiguous,
        )
    }
}
fn bounded(message: &str) -> &str {
    message
        .get(..message.len().min(2048))
        .unwrap_or("tool failed")
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}
fn emit(events: &mpsc::Sender<AttachmentEvent>, event: AttachmentEvent) {
    trace_attachment_event(&event);
    let _ = events.try_send(event);
}

fn trace_attachment_event(event: &AttachmentEvent) {
    match event {
        AttachmentEvent::Connecting => tracing::info!(
            target: "nanocodex_tools::attachment",
            stage = "attachment.connecting",
            "connecting attachment"
        ),
        AttachmentEvent::Attached => tracing::info!(
            target: "nanocodex_tools::attachment",
            stage = "attachment.attached",
            "attachment accepted"
        ),
        AttachmentEvent::CatalogPublished { tool_count } => tracing::info!(
            target: "nanocodex_tools::attachment",
            stage = "attachment.catalog_published",
            tool.count = tool_count,
            "attachment catalog published"
        ),
        AttachmentEvent::Detached { .. } => tracing::info!(
            target: "nanocodex_tools::attachment",
            stage = "attachment.detached",
            "attachment detached"
        ),
        AttachmentEvent::Fenced { .. } => tracing::warn!(
            target: "nanocodex_tools::attachment",
            stage = "attachment.fenced",
            "attachment fenced"
        ),
        AttachmentEvent::CallStarted { .. } | AttachmentEvent::CallCompleted { .. } => {}
    }
}

#[cfg(test)]
mod tracing_tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use tracing::{
        Subscriber,
        field::{Field, Visit},
        span::{Attributes, Id, Record},
    };
    use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

    use super::*;

    #[derive(Clone, Default)]
    struct CallSpanCapture(Arc<Mutex<Option<CapturedCallSpan>>>);

    struct CapturedCallSpan {
        id: u64,
        target: &'static str,
        fields: HashMap<String, String>,
        closed: bool,
    }

    struct FieldCapture<'a>(&'a mut HashMap<String, String>);

    impl Visit for FieldCapture<'_> {
        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0.insert(field.name().to_owned(), value.to_string());
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.insert(field.name().to_owned(), value.to_owned());
        }

        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().to_owned(), format!("{value:?}"));
        }
    }

    impl<S> Layer<S> for CallSpanCapture
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, _context: Context<'_, S>) {
            if attributes.metadata().name() != "attachment.call" {
                return;
            }
            let mut fields = HashMap::new();
            attributes.record(&mut FieldCapture(&mut fields));
            *self.0.lock().unwrap() = Some(CapturedCallSpan {
                id: id.clone().into_u64(),
                target: attributes.metadata().target(),
                fields,
                closed: false,
            });
        }

        fn on_record(&self, id: &Id, values: &Record<'_>, _context: Context<'_, S>) {
            let mut captured = self.0.lock().unwrap();
            let Some(captured) = captured.as_mut() else {
                return;
            };
            if captured.id == id.clone().into_u64() {
                values.record(&mut FieldCapture(&mut captured.fields));
            }
        }

        fn on_close(&self, id: Id, _context: Context<'_, S>) {
            let mut captured = self.0.lock().unwrap();
            let Some(captured) = captured.as_mut() else {
                return;
            };
            if captured.id == id.into_u64() {
                captured.closed = true;
            }
        }
    }

    #[test]
    fn attachment_call_span_owns_bounded_structural_telemetry() {
        let capture = CallSpanCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        let dispatch = tracing::Dispatch::new(subscriber);
        let (events, mut received) = mpsc::channel(4);

        tracing::dispatcher::with_default(&dispatch, || {
            begin_call_events(
                &events,
                "call-1".into(),
                "exec_command".into(),
                Some("hand-1"),
            )
            .complete(&events, AttachmentCallOutcome::Completed);
        });

        assert!(matches!(
            received.try_recv().unwrap(),
            AttachmentEvent::CallStarted { .. }
        ));
        assert!(matches!(
            received.try_recv().unwrap(),
            AttachmentEvent::CallCompleted { .. }
        ));
        let captured = capture.0.lock().unwrap();
        let captured = captured.as_ref().unwrap();
        assert_eq!(captured.target, "nanocodex_tools::attachment");
        assert!(captured.closed);
        assert_eq!(captured.fields.get("attachment.id").unwrap(), "hand-1");
        assert_eq!(captured.fields.get("tool.name").unwrap(), "exec_command");
        assert_eq!(captured.fields.get("tool.call_id").unwrap(), "call-1");
        assert_eq!(captured.fields.get("status").unwrap(), "completed");
        assert_eq!(captured.fields.get("otel.status_code").unwrap(), "OK");
        assert!(captured.fields.contains_key("duration_ns"));
        assert_eq!(
            captured.fields.len(),
            7,
            "attachment call spans must remain structural: {:?}",
            captured.fields
        );
    }
}
