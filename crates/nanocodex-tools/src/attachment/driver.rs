use super::{
    OBSERVATION_TIMEOUT, ObservationProvider, ObservationSurface, observation::ObservationResult,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{
    connect_async,
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

pub(crate) struct Config {
    pub(crate) endpoint: Url,
    pub(crate) authorization: Box<str>,
    pub(crate) tools: Value,
    pub(crate) metadata: Option<AttachmentMetadata>,
    pub(crate) observation: Option<Arc<dyn ObservationProvider>>,
    pub(crate) observation_surfaces: Vec<ObservationSurface>,
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
    let mut backoff = Duration::from_millis(100);
    let terminal = loop {
        let _ = status.send(AttachmentStatus::Connecting);
        emit(&events, AttachmentEvent::Connecting);
        let request = match request(&config) {
            Ok(request) => request,
            Err(error) => break Err(error),
        };
        let connected = tokio::select! {
            command = commands.recv() => match command { Some(Command::Detach) | None => break Ok(()) },
            connected = connect_async(request) => connected,
        };
        let socket = match connected {
            Ok((socket, _)) => socket,
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
    Result {
        call_id: Box<str>,
        outcome: Value,
        observed: AttachmentCallOutcome,
    },
}

struct InFlight {
    task: tokio::task::JoinHandle<()>,
    identity: CallIdentity,
    parallel_safe: bool,
    events: CallEvents,
}

struct PendingCall {
    identity: CallIdentity,
    tool_timeout: u64,
    parallel_safe: bool,
    events: CallEvents,
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

#[derive(Clone, PartialEq)]
struct CallIdentity {
    session_id: Box<str>,
    call_id: Box<str>,
    model: Box<str>,
    name: Box<str>,
    input: Value,
    output_token_budget: u64,
    output_byte_budget: u64,
    deadline_at: u64,
}

struct Receipt {
    identity: CallIdentity,
    outcome: Value,
}

fn admitted_identity<'a>(
    in_flight: &'a HashMap<Box<str>, InFlight>,
    pending: &'a VecDeque<PendingCall>,
    call_id: &str,
) -> Option<&'a CallIdentity> {
    in_flight
        .get(call_id)
        .map(|call| &call.identity)
        .or_else(|| {
            pending
                .iter()
                .find(|call| call.identity.call_id.as_ref() == call_id)
                .map(|call| &call.identity)
        })
}

fn start_ready_calls(
    runtime: &Arc<PreparedToolRuntime>,
    pending: &mut VecDeque<PendingCall>,
    in_flight: &mut HashMap<Box<str>, InFlight>,
    completed: &mpsc::Sender<Completion>,
) {
    while in_flight.len() < protocol::MAX_IN_FLIGHT {
        if in_flight.values().any(|call| !call.parallel_safe) {
            break;
        }
        let Some(next) = pending.front() else { break };
        if !next.parallel_safe && !in_flight.is_empty() {
            break;
        }
        let Some(PendingCall {
            identity,
            tool_timeout,
            parallel_safe,
            events,
        }) = pending.pop_front()
        else {
            break;
        };
        let runtime = Arc::clone(runtime);
        let tx = completed.clone();
        let id = identity.call_id.clone();
        let id_for_task = id.clone();
        let task_identity = identity.clone();
        let task_span = events.span.clone();
        let task = tokio::spawn(
            async move {
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
                    );
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
                let _ = tx
                    .send(Completion::Result {
                        call_id: id_for_task,
                        outcome,
                        observed,
                    })
                    .await;
            }
            .instrument(task_span),
        );
        in_flight.insert(
            id,
            InFlight {
                task,
                identity,
                parallel_safe,
                events,
            },
        );
        if !parallel_safe {
            break;
        }
    }
}

struct ConnectionContext<'a> {
    config: &'a Config,
    runtime: &'a Arc<PreparedToolRuntime>,
    events: &'a mpsc::Sender<AttachmentEvent>,
    status: &'a watch::Sender<AttachmentStatus>,
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
    } = context;
    if let Err(error) = send(
        &mut socket,
        &ExecutorFrame::Catalog {
            tools: &config.tools,
            observation_surfaces: (!config.observation_surfaces.is_empty())
                .then_some(config.observation_surfaces.as_slice()),
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
    emit(events, AttachmentEvent::Attached);
    let _ = status.send(AttachmentStatus::Ready);
    emit(
        events,
        AttachmentEvent::CatalogPublished {
            tool_count: config.tools.as_array().map_or(0, Vec::len),
        },
    );

    let (completed_tx, mut completed_rx) = mpsc::channel::<Completion>(protocol::MAX_IN_FLIGHT);
    let mut in_flight = HashMap::<Box<str>, InFlight>::new();
    let mut pending = VecDeque::<PendingCall>::new();
    let mut receipts = HashMap::<Box<str>, Receipt>::new();
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + protocol::HEARTBEAT_INTERVAL,
        protocol::HEARTBEAT_INTERVAL,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pong_timeout = Box::pin(tokio::time::sleep(PONG_TIMEOUT));
    let mut awaiting_pong: Option<String> = None;
    let mut captures = tokio::task::JoinSet::new();
    let mut capture_request: Option<String> = None;
    let mut detaching = false;
    let mut draining = false;

    let end = loop {
        start_ready_calls(runtime, &mut pending, &mut in_flight, &completed_tx);
        if detaching
            && draining
            && pending.is_empty()
            && in_flight.is_empty()
            && receipts.is_empty()
        {
            break ConnectionEnd::Detached;
        }
        tokio::select! {
            command = commands.recv(), if !detaching => {
                match command { Some(Command::Detach) | None => {} }
                if let Err(error) = send(&mut socket, &ExecutorFrame::Drain {}).await {
                    break ConnectionEnd::DetachFailed(error);
                }
                captures.abort_all();
                capture_request = None;
                detaching = true;
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
            captured = captures.join_next(), if !captures.is_empty() => {
                if let Some(Ok((request_id, result))) = captured {
                    if capture_request.as_ref() == Some(&request_id) && !detaching {
                        capture_request = None;
                        if let Err(error) = send(&mut socket, &ExecutorFrame::Observation { request_id: &request_id, result: &result }).await {
                            break ConnectionEnd::Failed(error);
                        }
                    }
                }
            }
            completion = completed_rx.recv() => if let Some(Completion::Result { call_id, outcome, observed }) = completion {
                let Some(call) = in_flight.remove(&call_id) else { continue };
                let _ = call.task.await;
                call.events.complete(events, observed);
                if receipts.len() >= protocol::MAX_RECEIPTS { break ConnectionEnd::Rejected("result receipt capacity exceeded".into()); }
                receipts.insert(call_id.clone(), Receipt { identity: call.identity, outcome: outcome.clone() });
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
                    RemoteFrame::ObserveCancel { request_id } => {
                        if capture_request.as_ref() == Some(&request_id) {
                            captures.abort_all();
                            capture_request = None;
                        }
                    }
                    RemoteFrame::Observe { request_id, surface_id } => {
                        if detaching || !captures.is_empty() || !config.observation_surfaces.iter().any(|s| s.id() == surface_id) {
                            if let Err(error) = send(&mut socket, &ExecutorFrame::Observation { request_id: &request_id, result: &ObservationResult::unavailable() }).await {
                                break ConnectionEnd::Failed(error);
                            }
                            continue;
                        }
                        let Some(provider) = config.observation.clone() else { continue };
                        capture_request = Some(request_id.clone());
                        captures.spawn(async move {
                            let result = match tokio::time::timeout(OBSERVATION_TIMEOUT, provider.capture(&surface_id)).await {
                                Ok(Ok(frame)) => ObservationResult::Frame { frame },
                                _ => ObservationResult::unavailable(),
                            };
                            (request_id, result)
                        });
                    }
                    RemoteFrame::Call { session_id, call_id, model, name, input, output_token_budget, output_byte_budget, deadline_at } => {
                        if draining { break ConnectionEnd::Rejected("call received after drain barrier".into()); }
                        let identity = CallIdentity { session_id:session_id.into(), call_id:call_id.clone().into(), model:model.into(), name:name.clone().into(), input:input.clone(), output_token_budget, output_byte_budget, deadline_at };
                        if let Some(receipt) = receipts.get(call_id.as_str()) {
                            if receipt.identity != identity { break ConnectionEnd::Rejected("duplicate call changed immutable fields".into()); }
                            if let Err(error) = send_result(&mut socket, &call_id, &receipt.outcome).await { break ConnectionEnd::Failed(error); }
                            continue;
                        }
                        if let Some(admitted) = admitted_identity(&in_flight, &pending, &call_id) {
                            if admitted != &identity { break ConnectionEnd::Rejected("duplicate in-flight call changed immutable fields".into()); }
                            continue;
                        }
                        let call_events = begin_call_events(
                            events,
                            call_id.clone().into(),
                            name.clone().into(),
                            config.metadata.as_ref().map(AttachmentMetadata::attachment_id),
                        );
                        if receipts.len().saturating_add(in_flight.len()).saturating_add(pending.len()) >= protocol::MAX_RECEIPTS {
                            call_events.complete(events, AttachmentCallOutcome::Unavailable);
                            break ConnectionEnd::Rejected("result receipt capacity exhausted".into());
                        }
                        if in_flight.len().saturating_add(pending.len()) >= protocol::MAX_IN_FLIGHT {
                            let outcome = unavailable("attachment execution capacity is exhausted");
                            call_events.complete(events, AttachmentCallOutcome::Unavailable);
                            if let Err(error) = send_result(&mut socket, &call_id, &outcome).await { break ConnectionEnd::Failed(error); }
                            receipts.insert(call_id.into(), Receipt { identity, outcome });
                            continue;
                        }
                        let tool_timeout = runtime.timeout_ms(&name).unwrap_or(0);
                        if deadline_at <= now_ms() || tool_timeout == 0 {
                            let outcome = unavailable(if tool_timeout == 0 { "tool is not in the pinned catalog" } else { "tool deadline elapsed before execution" });
                            call_events.complete(events, AttachmentCallOutcome::Unavailable);
                            if let Err(error) = send_result(&mut socket, &call_id, &outcome).await { break ConnectionEnd::Failed(error); }
                            receipts.insert(call_id.into(), Receipt { identity, outcome });
                            continue;
                        }
                        let parallel_safe = runtime.parallel_safe(&name);
                        pending.push_back(PendingCall { identity, tool_timeout, parallel_safe, events:call_events });
                    }
                    RemoteFrame::Cancel { call_id } => {
                        let outcome = cancelled();
                        if let Some(index) = pending.iter().position(|call| call.identity.call_id.as_ref() == call_id) {
                            let call = pending.remove(index).expect("position came from the same queue");
                            call.events.complete(events, AttachmentCallOutcome::Cancelled);
                            receipts.insert(call_id.clone().into(), Receipt { identity: call.identity, outcome: outcome.clone() });
                            if let Err(error) = send_result(&mut socket, &call_id, &outcome).await { break ConnectionEnd::Failed(error); }
                        } else if let Some(call) = in_flight.remove(call_id.as_str()) {
                            call.task.abort();
                            let _ = call.task.await;
                            let outcome = ambiguous("tool execution was cancelled after dispatch");
                            call.events.complete(events, AttachmentCallOutcome::Ambiguous);
                            receipts.insert(call_id.clone().into(), Receipt { identity: call.identity, outcome: outcome.clone() });
                            if let Err(error) = send_result(&mut socket, &call_id, &outcome).await { break ConnectionEnd::Failed(error); }
                        }
                    }
                    RemoteFrame::Ack { call_id } => {
                        if receipts.remove(call_id.as_str()).is_none() {
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

    captures.shutdown().await;

    if !matches!(end, ConnectionEnd::Detached) {
        for call in pending {
            call.events
                .complete(events, AttachmentCallOutcome::Ambiguous);
        }
        for (call_id, call) in in_flight {
            call.task.abort();
            let _ = call.task.await;
            call.events
                .complete(events, AttachmentCallOutcome::Ambiguous);
            receipts.entry(call_id).or_insert(Receipt {
                identity: call.identity,
                outcome: ambiguous(
                    "tool execution was interrupted before its result was acknowledged",
                ),
            });
        }
    }
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
    if text.len() > protocol::MAX_FRAME_BYTES {
        return Err(AttachmentError::Transport(
            "outbound frame exceeds 256 KiB".into(),
        ));
    }
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
fn cancelled() -> Value {
    json!({"status":"cancelled", "message":"tool attachment call was cancelled"})
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
