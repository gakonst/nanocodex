use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
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

use super::protocol::{self, DiagnosticStage, ExecutorFrame, ReceiptTiming, RemoteFrame};
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

const HEARTBEAT_TIMEOUT_REASON: &str = "attachment heartbeat timed out";
/// Fenced reason when the endpoint reports the attachment permanently gone (HTTP 410).
pub(crate) const ATTACHMENT_REVOKED_REASON: &str =
    "attachment endpoint permanently revoked this attachment";
#[cfg(not(test))]
const MAX_REJECTED_BACKOFF: Duration = Duration::from_secs(60);
#[cfg(test)]
const MAX_REJECTED_BACKOFF: Duration = Duration::from_millis(500);

pub(crate) enum Authorization {
    Static(Box<str>),
    /// Fetched before every connection attempt; never cached here.
    Dynamic(Arc<dyn super::AttachmentCredentials>),
}

impl Authorization {
    async fn header(&self) -> Result<Box<str>, AttachmentError> {
        match self {
            Self::Static(value) => Ok(value.clone()),
            Self::Dynamic(credentials) => credentials
                .bearer()
                .await
                .map(|bearer| format!("Bearer {bearer}").into()),
        }
    }
}

pub(crate) struct Config {
    pub(crate) endpoint: Url,
    pub(crate) authorization: Authorization,
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
    // Transport generations may change while the same runtime owns processes.
    // A new driver gets a new identity so local numeric IDs cannot be retargeted.
    let runtime_id = uuid::Uuid::new_v4().to_string();
    let mut active = Vec::<InFlight>::new();
    let mut journal = HashMap::<Box<str>, RetainedCall>::new();
    let (completed_tx, mut completed_rx) = mpsc::unbounded_channel::<Completion>();
    let mut backoff = Duration::from_millis(100);
    let mut attempt = 0_u64;
    let mut previous_delay = Duration::ZERO;
    let terminal = loop {
        attempt = attempt.saturating_add(1);
        let connection_id = uuid::Uuid::new_v4().to_string();
        let connection_span = tracing::info_span!(target: "nanocodex_oai_tools::attachment", "attachment.connection",
            runtime_id = runtime_id.as_str(), client_connection_id = connection_id.as_str(),
            attempt, reconnect_delay_ms = previous_delay.as_millis() as u64);
        let _ = status.send(AttachmentStatus::Connecting);
        connection_span.in_scope(|| emit(&events, AttachmentEvent::Connecting));
        let authorization = tokio::select! {
            command = commands.recv() => match command { Some(Command::Detach) | None => break Ok(()) },
            authorization = config.authorization.header() => authorization,
        };
        let authorization = match authorization {
            Ok(authorization) => authorization,
            Err(error @ AttachmentError::Authentication(_)) => {
                connection_span.in_scope(|| tracing::warn!(target: "nanocodex_oai_tools::attachment", stage = "attachment.credential.rejected", reason_code = "credential_unavailable", pending_calls = active.len(), "attachment credential permanently unavailable"));
                break Err(error);
            }
            Err(_) => {
                connection_span.in_scope(|| tracing::warn!(target: "nanocodex_oai_tools::attachment", stage = "attachment.credential.failed", reason_code = "credential_failure", reconnect_delay_ms = backoff.as_millis() as u64, pending_calls = active.len(), "attachment credential refresh failed"));
                let _ = status.send(AttachmentStatus::Disconnected);
                previous_delay = backoff;
                if wait_backoff(&mut commands, backoff).await {
                    break Ok(());
                }
                backoff = (backoff * 2).min(MAX_REJECTED_BACKOFF);
                continue;
            }
        };
        let request = match request(&config, &authorization, &connection_id, &runtime_id) {
            Ok(request) => request,
            Err(error) => break Err(error),
        };
        let connect_started = Instant::now();
        let connected = tokio::select! {
            command = commands.recv() => match command { Some(Command::Detach) | None => break Ok(()) },
            connected = tokio::time::timeout(Duration::from_secs(5), async {
                let connector = if request.uri().scheme_str() == Some("wss") {
                    Some(tokio_tungstenite::Connector::Rustls(nanocodex_oai_api::tls::native_client_config().await?))
                } else { None };
                tracing::info!(target: "nanocodex_oai_tools::attachment",
                    stage = "attachment.socket.trust",
                    elapsed_ms = connect_started.elapsed().as_secs_f64() * 1000.0,
                    "attachment TLS trust ready");
                let host = request.uri().host().ok_or(tokio_tungstenite::tungstenite::Error::Url(
                    tokio_tungstenite::tungstenite::error::UrlError::NoHostName,
                ))?;
                let host = host.trim_start_matches('[').trim_end_matches(']');
                let port = request.uri().port_u16().unwrap_or(if connector.is_some() { 443 } else { 80 });
                let dns_started = Instant::now();
                let addresses: Vec<_> = tokio::net::lookup_host((host, port)).await?.collect();
                tracing::info!(target: "nanocodex_oai_tools::attachment",
                    stage = "attachment.socket.resolved",
                    duration_ms = dns_started.elapsed().as_secs_f64() * 1000.0,
                    elapsed_ms = connect_started.elapsed().as_secs_f64() * 1000.0,
                    "attachment address resolved");
                let tcp_started = Instant::now();
                let stream = tokio::net::TcpStream::connect(addresses.as_slice()).await?;
                stream.set_nodelay(true)?;
                tracing::info!(target: "nanocodex_oai_tools::attachment",
                    stage = "attachment.socket.tcp",
                    duration_ms = tcp_started.elapsed().as_secs_f64() * 1000.0,
                    elapsed_ms = connect_started.elapsed().as_secs_f64() * 1000.0,
                    "attachment TCP connected");
                let upgrade_started = Instant::now();
                let upgraded = client_async_tls_with_config(request, stream, None, connector).await;
                // tokio-tungstenite owns TLS and HTTP upgrade in one public call.
                // Keep this combined duration explicit rather than inventing a split.
                tracing::info!(target: "nanocodex_oai_tools::attachment", stage = "attachment.socket.tls_upgrade",
                    duration_ms = upgrade_started.elapsed().as_secs_f64() * 1000.0,
                    reason_code = if upgraded.is_ok() { "connected" } else { "tls_upgrade_failure" }, "attachment TLS and upgrade finished");
                upgraded
            }.instrument(connection_span.clone())) => connected,
        };
        let socket = match connected {
            Ok(Ok((socket, response))) => {
                tracing::info!(target: "nanocodex_oai_tools::attachment",
                    stage = "attachment.websocket_connected",
                    duration_ms = connect_started.elapsed().as_secs_f64() * 1000.0,
                    runtime_id = runtime_id.as_str(), client_connection_id = connection_id.as_str(), attempt,
                    request_id = response.headers().get("x-nanocodex-request-id").and_then(|v| v.to_str().ok()).and_then(safe_uuid).unwrap_or_default(),
                    "attachment WebSocket connected");
                socket
            }
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
                if matches!(response.status().as_u16(), 401 | 403) =>
            {
                connection_span.in_scope(|| tracing::warn!(target: "nanocodex_oai_tools::attachment", stage = "attachment.socket.rejected", reason_code = "authentication_rejected", pending_calls = active.len(), "attachment connection rejected"));
                break Err(AttachmentError::Authentication(
                    "endpoint rejected the bearer credential".into(),
                ));
            }
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
                if response.status().as_u16() == 410 =>
            {
                // The attachment's allocation or conversation is permanently
                // gone. Reconnecting can never succeed; let the owner reap it.
                connection_span.in_scope(|| tracing::warn!(target: "nanocodex_oai_tools::attachment", stage = "attachment.socket.rejected", reason_code = "attachment_revoked", pending_calls = active.len(), "attachment permanently revoked"));
                break Err(AttachmentError::Fenced(ATTACHMENT_REVOKED_REASON.into()));
            }
            other => {
                // An HTTP rejection proves the endpoint is reachable but refusing
                // this attachment; back off further than for transport loss.
                let rejected = matches!(
                    &other,
                    Ok(Err(tokio_tungstenite::tungstenite::Error::Http(_)))
                );
                connection_span.in_scope(|| tracing::warn!(target: "nanocodex_oai_tools::attachment", stage = "attachment.socket.connect_failed", reason_code = if rejected { "http_rejected" } else if other.is_err() { "connect_timeout" } else { "connect_failure" }, reconnect_delay_ms = backoff.as_millis() as u64, pending_calls = active.len(), "attachment connection attempt failed"));
                let _ = status.send(AttachmentStatus::Disconnected);
                previous_delay = backoff;
                if wait_backoff(&mut commands, backoff).await {
                    break Ok(());
                }
                backoff = (backoff * 2).min(if rejected {
                    MAX_REJECTED_BACKOFF
                } else {
                    Duration::from_secs(5)
                });
                continue;
            }
        };
        let connected_at = Instant::now();
        let end = connection(
            socket,
            ConnectionContext {
                config: &config,
                runtime_id: &runtime_id,
                connection_id: &connection_id,
                runtime: &runtime,
                events: &events,
                status: &status,
                active: &mut active,
                journal: &mut journal,
                completed_tx: &completed_tx,
                completed_rx: &mut completed_rx,
            },
            &mut commands,
        )
        .instrument(connection_span.clone())
        .await;
        if matches!(*status.borrow(), AttachmentStatus::Ready)
            && connected_at.elapsed() >= STABLE_CONNECTION
        {
            backoff = Duration::from_millis(100);
        }
        connection_span.in_scope(|| {
            if let ConnectionEnd::Failed(error) | ConnectionEnd::DetachFailed(error) = &end {
                tracing::debug!(target: "nanocodex_oai_tools::attachment", %error, "attachment transport ended");
            }
            tracing::info!(target: "nanocodex_oai_tools::attachment",
            stage = "attachment.socket.closed", reason_code = end.reason_code(),
            pending_calls = active.iter().filter(|call| !call.task.is_finished()).count(),
            reconnect_delay_ms = backoff.as_millis() as u64, "attachment connection ended")
        });
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
            ConnectionEnd::Failed(_)
            | ConnectionEnd::Disconnected
            | ConnectionEnd::HeartbeatTimeout => {
                let _ = status.send(AttachmentStatus::Disconnected);
                previous_delay = backoff;
                if wait_backoff(&mut commands, backoff).await {
                    break Ok(());
                }
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
    };
    // Closure must not leave surviving handles advertising a ready transport.
    let _ = status.send(
        if matches!(
            terminal,
            Err(AttachmentError::Authentication(_) | AttachmentError::Fenced(_))
        ) {
            AttachmentStatus::Fenced
        } else {
            AttachmentStatus::Disconnected
        },
    );
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

fn request(
    config: &Config,
    authorization: &str,
    connection_id: &str,
    runtime_id: &str,
) -> Result<http::Request<()>, AttachmentError> {
    let mut request = config
        .endpoint
        .as_str()
        .into_client_request()
        .map_err(|error| AttachmentError::Transport(error.to_string().into()))?;
    let mut authorization = http::HeaderValue::from_str(authorization)
        .map_err(|_| AttachmentError::Authentication("invalid bearer credential".into()))?;
    authorization.set_sensitive(true);
    request
        .headers_mut()
        .insert(http::header::AUTHORIZATION, authorization);
    request.headers_mut().insert(
        "x-nanocodex-request-id",
        http::HeaderValue::from_str(connection_id)
            .map_err(|_| AttachmentError::Transport("invalid connection identity".into()))?,
    );
    // Only account machine publishers support regional pre-upgrade routing.
    // Named/scoped attachments retain the legacy owner route. Metadata::machine
    // guarantees one machine whose exact ID is also the catalog attachment_id.
    // Account machine publishers always use regional relays.
    if config.endpoint.path() == "/v1/account/tool-host"
        && let Some(machine) = config
            .metadata
            .as_ref()
            .and_then(AttachmentMetadata::attached_machine)
    {
        request.headers_mut().insert(
            "x-nanocodex-hand-machine-id",
            http::HeaderValue::from_str(machine.id())
                .map_err(|_| AttachmentError::Transport("invalid machine identity".into()))?,
        );
        request.headers_mut().insert(
            "x-nanocodex-hand-runtime-id",
            http::HeaderValue::from_str(runtime_id)
                .map_err(|_| AttachmentError::Transport("invalid runtime identity".into()))?,
        );
    }
    Ok(request)
}

fn safe_uuid(value: &str) -> Option<String> {
    uuid::Uuid::parse_str(value)
        .ok()
        .filter(|id| id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122)
        .map(|id| id.to_string())
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
    HeartbeatTimeout,
    Failed(AttachmentError),
    Rejected(Box<str>),
}

impl ConnectionEnd {
    const fn reason_code(&self) -> &'static str {
        match self {
            Self::Detached => "detached",
            Self::DetachFailed(_) => "detach_transport_failure",
            Self::Disconnected => "remote_disconnect",
            Self::HeartbeatTimeout => "heartbeat_timeout",
            Self::Failed(_) => "transport_failure",
            Self::Rejected(_) => "protocol_rejected",
        }
    }
}

enum Completion {
    Diagnostic {
        call_id: Box<str>,
        timing: Arc<Mutex<CallClock>>,
    },
    Result {
        call_id: Box<str>,
        outcome: Value,
    },
}

struct CallClock {
    received: Instant,
    task_started: Option<Instant>,
    execution_started: Option<Instant>,
    execution_finished: Option<Instant>,
    result_queued: Option<Instant>,
    diagnostics_sent: u8,
    span: tracing::Span,
}

impl CallClock {
    fn received() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            received: Instant::now(),
            task_started: None,
            execution_started: None,
            execution_finished: None,
            result_queued: None,
            diagnostics_sent: 0,
            span: tracing::Span::current(),
        }))
    }

    fn snapshot(&self, now: Instant) -> ReceiptTiming {
        let started = self
            .execution_started
            .unwrap_or(self.task_started.unwrap_or(now));
        let finished = self.execution_finished.unwrap_or(now);
        let queued = self.result_queued.unwrap_or(now);
        let ms = |end: Instant, start: Instant| {
            end.saturating_duration_since(start).as_secs_f64() * 1000.0
        };
        ReceiptTiming {
            scheduler_ms: ms(started, self.received),
            execution_gate_ms: 0.0,
            execution_ms: if self.execution_started.is_some() {
                ms(finished, started)
            } else {
                0.0
            },
            result_encode_ms: if self.execution_finished.is_some() {
                ms(queued, finished)
            } else {
                0.0
            },
            result_queue_ms: if self.result_queued.is_some() {
                ms(now, queued)
            } else {
                0.0
            },
            host_elapsed_ms: ms(now, self.received),
        }
    }
}

// This RAM journal belongs to the living executor runtime, never a socket.
// A process restart loses proof; recovery may only report missing in that case.
struct RetainedCall {
    identity: CallIdentity,
    task: Option<tokio::task::AbortHandle>,
    timing: Arc<Mutex<CallClock>>,
    receipt: Option<String>,
}

impl RetainedCall {
    fn finish(&mut self, outcome: &Value) -> Result<(), AttachmentError> {
        if self.receipt.is_none() {
            self.receipt = Some(encode_result(
                &self.identity.call_id,
                outcome,
                &self.timing,
            )?);
            self.task = None;
            let span = self.timing.lock().unwrap().span.clone();
            span.in_scope(|| {
                tracing::info!(target: "nanocodex_oai_tools::attachment",
                stage = "attachment.result_retained",
                transport_call_id = self.identity.call_id.as_ref(),
                "attachment terminal receipt retained")
            });
        }
        Ok(())
    }
}

struct InFlight {
    task: tokio::task::JoinHandle<()>,
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
        target: "nanocodex_oai_tools::attachment",
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
            target: "nanocodex_oai_tools::attachment",
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
            target: "nanocodex_oai_tools::attachment",
            stage = "attachment.call.completed",
            "attachment call completed"
        );
    } else {
        tracing::info!(
            target: "nanocodex_oai_tools::attachment",
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
    turn_id: Option<Box<str>>,
    call_id: Box<str>,
    model: Box<str>,
    name: Box<str>,
    input: Value,
    output_token_budget: u64,
    output_byte_budget: u64,
    deadline_at: u64,
}

#[allow(
    clippy::too_many_arguments,
    reason = "keep per-call identity and independently owned execution/event channels explicit"
)]
fn start_call(
    runtime: &Arc<PreparedToolRuntime>,
    active: &mut Vec<InFlight>,
    identity: CallIdentity,
    timing: Arc<Mutex<CallClock>>,
    tool_timeout: u64,
    events: CallEvents,
    completed: mpsc::UnboundedSender<Completion>,
    event_sender: &mpsc::Sender<AttachmentEvent>,
) -> tokio::task::AbortHandle {
    let runtime = Arc::clone(runtime);
    let task_span = events.span.clone();
    let mut events = TaskEvents {
        call: Some(events),
        events: event_sender.clone(),
    };
    let task = tokio::spawn(
        async move {
            timing.lock().unwrap().task_started = Some(Instant::now());
            let task_identity = identity;
            let remaining = task_identity.deadline_at.saturating_sub(now_ms());
            let (outcome, observed) = if remaining == 0 {
                (
                    unavailable("tool deadline elapsed before execution"),
                    AttachmentCallOutcome::Unavailable,
                )
            } else {
                // Settle locally just before the hosted deadline so the broker
                // receives this runtime's own outcome rather than expiring it.
                let duration = Duration::from_millis(
                    remaining
                        .saturating_sub(EXECUTION_DEADLINE_MARGIN_MS)
                        .max(1)
                        .min(tool_timeout),
                );
                let call = PreparedToolCall::new(
                    task_identity.model.to_string(),
                    task_identity.session_id.to_string(),
                    task_identity.call_id.to_string(),
                    task_identity.name.to_string(),
                    clamp_shell_yield(&task_identity.name, task_identity.input.clone(), remaining),
                    task_identity.output_token_budget as usize,
                )
                .with_turn_id(task_identity.turn_id.as_deref().map(str::to_owned));
                let execute = async {
                    let _phase = ExecutionPhase::start(&timing, &completed, &task_identity.call_id);
                    runtime.execute(call).await
                };
                let executed = tokio::time::timeout(duration, execute).await;
                match executed {
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
            // The runtime owns this channel across every transport connection.
            timing.lock().unwrap().result_queued = Some(Instant::now());
            let _ = completed.send(Completion::Result {
                call_id: task_identity.call_id,
                outcome,
            });
        }
        .instrument(task_span),
    );
    let abort = task.abort_handle();
    active.push(InFlight { task });
    abort
}

/// Time reserved after a local execution timeout for its result to reach the broker.
const EXECUTION_DEADLINE_MARGIN_MS: u64 = 1_000;
#[cfg(not(feature = "workspace-runtime"))]
fn clamp_shell_yield(_name: &str, input: Value, _remaining_ms: u64) -> Value {
    input
}

/// Time reserved after a shell yield for result encoding and delivery.
#[cfg(feature = "workspace-runtime")]
const SHELL_YIELD_DEADLINE_MARGIN_MS: u64 = 3_000;
#[cfg(feature = "workspace-runtime")]
const MIN_SHELL_YIELD_MS: u64 = 250;

/// Shell yields are observation deadlines: a process that outlives the hosted
/// call deadline must return its live session before that deadline instead of
/// letting the broker settle the call as ambiguous. Shorter yields and the
/// tools' own defaults are preserved.
#[cfg(feature = "workspace-runtime")]
fn clamp_shell_yield(name: &str, mut input: Value, remaining_ms: u64) -> Value {
    let exec = name == crate::StandardTool::ExecCommand.name();
    if !exec && name != crate::StandardTool::WriteStdin.name() {
        return input;
    }
    let Some(object) = input.as_object_mut() else {
        return input;
    };
    let budget = remaining_ms
        .saturating_sub(SHELL_YIELD_DEADLINE_MARGIN_MS)
        .max(MIN_SHELL_YIELD_MS);
    let requested = match object.get("yield_time_ms") {
        Some(Value::Number(value)) => value.as_u64(),
        Some(Value::Null) | None => None,
        // Leave invalid input to the tool's own validation.
        Some(_) => return input,
    };
    let effective = requested.unwrap_or_else(|| {
        let polling = object
            .get("chars")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty);
        match (exec, polling) {
            (true, _) => crate::shell::DEFAULT_EXEC_YIELD_MS,
            (false, true) => crate::shell::DEFAULT_POLL_YIELD_MS,
            (false, false) => crate::shell::DEFAULT_WRITE_YIELD_MS,
        }
    });
    if effective > budget {
        object.insert("yield_time_ms".to_owned(), Value::from(budget));
    }
    input
}

// Diagnostics never wait on the socket; the runtime channel survives disconnects.
// The clock retains phases and receipt transmission flushes them before the result.
struct ExecutionPhase<'a> {
    timing: &'a Arc<Mutex<CallClock>>,
    completed: &'a mpsc::UnboundedSender<Completion>,
    call_id: &'a str,
}

impl<'a> ExecutionPhase<'a> {
    fn start(
        timing: &'a Arc<Mutex<CallClock>>,
        completed: &'a mpsc::UnboundedSender<Completion>,
        call_id: &'a str,
    ) -> Self {
        timing.lock().unwrap().execution_started = Some(Instant::now());
        let phase = Self {
            timing,
            completed,
            call_id,
        };
        phase.trace(DiagnosticStage::ExecutionStarted);
        phase.notify();
        phase
    }

    fn trace(&self, stage: DiagnosticStage) {
        let clock = self.timing.lock().unwrap();
        let elapsed_ms =
            (clock.received.elapsed().as_secs_f64() * 1000.0).min(9_007_199_254_740_991.0);
        clock.span.in_scope(|| tracing::info!(target: "nanocodex_oai_tools::attachment", stage = stage.name(), elapsed_ms, "attachment execution phase"));
    }

    fn notify(&self) {
        let _ = self.completed.send(Completion::Diagnostic {
            call_id: self.call_id.into(),
            timing: self.timing.clone(),
        });
    }
}

impl Drop for ExecutionPhase<'_> {
    fn drop(&mut self) {
        self.timing.lock().unwrap().execution_finished = Some(Instant::now());
        self.trace(DiagnosticStage::ExecutionFinished);
        self.notify();
    }
}

struct ConnectionContext<'a> {
    config: &'a Config,
    runtime_id: &'a str,
    connection_id: &'a str,
    runtime: &'a Arc<PreparedToolRuntime>,
    events: &'a mpsc::Sender<AttachmentEvent>,
    status: &'a watch::Sender<AttachmentStatus>,
    active: &'a mut Vec<InFlight>,
    journal: &'a mut HashMap<Box<str>, RetainedCall>,
    completed_tx: &'a mpsc::UnboundedSender<Completion>,
    completed_rx: &'a mut mpsc::UnboundedReceiver<Completion>,
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
        runtime_id,
        connection_id,
        runtime,
        events,
        status,
        active,
        journal,
        completed_tx,
        completed_rx,
    } = context;
    let catalog_started = Instant::now();
    if let Err(error) = send(
        &mut socket,
        &ExecutorFrame::Catalog {
            capabilities: ["turn_metadata"],
            runtime_id,
            command_recovery: true,
            turn_lifecycle: true,
            diagnostics: Some(true),
            connection_id: Some(connection_id),
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
    tracing::info!(target: "nanocodex_oai_tools::attachment", stage = "attachment.catalog_sent", duration_ms = catalog_sent.duration_since(catalog_started).as_secs_f64() * 1000.0, "attachment catalog sent");
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
    tracing::info!(target: "nanocodex_oai_tools::attachment",
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

    // Collect completions that arrived offline before replaying retained receipts.
    while let Ok(completion) = completed_rx.try_recv() {
        if let Completion::Result {
            call_id, outcome, ..
        } = completion
            && let Some(call) = journal.get_mut(&call_id)
            && let Err(error) = call.finish(&outcome)
        {
            return ConnectionEnd::Failed(error);
        }
    }
    for call in journal.values() {
        let replayed = true;
        if call.receipt.is_some()
            && let Err(error) = send_retained(&mut socket, call, replayed).await
        {
            return ConnectionEnd::Failed(error);
        }
    }
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + protocol::HEARTBEAT_INTERVAL,
        protocol::HEARTBEAT_INTERVAL,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pong_timeout = Box::pin(tokio::time::sleep(PONG_TIMEOUT));
    let mut awaiting_pong: Option<Vec<u8>> = None;
    let mut ping_started: Option<Instant> = None;
    let mut detach_deadline = Box::pin(tokio::time::sleep(Duration::from_secs(10)));
    let mut detaching = false;
    let mut draining = false;

    let end = loop {
        if detaching && draining && journal.is_empty() {
            break ConnectionEnd::Detached;
        }
        tokio::select! {
            command = commands.recv(), if !detaching => {
                match command { Some(Command::Detach) | None => {} }
                if let Err(error) = send(&mut socket, &ExecutorFrame::Drain {}).await {
                    break ConnectionEnd::DetachFailed(error);
                }
                tracing::info!(target: "nanocodex_oai_tools::attachment", stage = "attachment.drain_requested", pending_calls = journal.values().filter(|call| call.task.is_some()).count(), pending_receipts = journal.values().filter(|call| call.receipt.is_some()).count(), reason_code = "detach_requested", "attachment drain requested");
                detaching = true;
                detach_deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(10));
                shutdown_calls(active).await;
                while let Ok(completion) = completed_rx.try_recv() {
                    if let Completion::Result { call_id, outcome, .. } = completion
                        && let Some(call) = journal.get_mut(&call_id)
                        && let Err(error) = call.finish(&outcome) { return ConnectionEnd::DetachFailed(error); }
                }
                for call in journal.values_mut() {
                    let replayed = call.receipt.is_some();
                    if call.receipt.is_none()
                        && let Err(error) = call.finish(&ambiguous("attachment shut down during execution")) { return ConnectionEnd::DetachFailed(error); }
                    if let Err(error) = send_retained(&mut socket, call, replayed).await { return ConnectionEnd::DetachFailed(error); }
                }
            }
            _ = &mut detach_deadline, if detaching => break ConnectionEnd::Detached,
            _ = &mut pong_timeout, if awaiting_pong.is_some() => {
                tracing::warn!(target: "nanocodex_oai_tools::attachment", stage = "attachment.heartbeat_timeout", pending_calls = journal.values().filter(|call| call.task.is_some()).count(), pending_receipts = journal.values().filter(|call| call.receipt.is_some()).count(), reason_code = "heartbeat_timeout", "attachment heartbeat timed out");
                break ConnectionEnd::HeartbeatTimeout;
            },
            _ = heartbeat.tick() => {
                if awaiting_pong.is_some() { break ConnectionEnd::HeartbeatTimeout; }
                let nonce = uuid::Uuid::new_v4().as_bytes().to_vec();
                ping_started = Some(Instant::now());
                if let Err(error) = socket.send(Message::Ping(nonce.clone().into())).await.map_err(|error| AttachmentError::Transport(error.to_string().into())) {
                    break if detaching { ConnectionEnd::DetachFailed(error) } else { ConnectionEnd::Failed(error) };
                }
                tracing::info!(target: "nanocodex_oai_tools::attachment", stage = "attachment.heartbeat_ping", pending_calls = journal.values().filter(|call| call.task.is_some()).count(), pending_receipts = journal.values().filter(|call| call.receipt.is_some()).count(), "attachment heartbeat sent");
                awaiting_pong = Some(nonce);
                pong_timeout.as_mut().reset(tokio::time::Instant::now() + PONG_TIMEOUT);
            }
            completion = completed_rx.recv() => if let Some(completion) = completion {
                let sent = match completion {
                    Completion::Diagnostic { call_id, timing } => {
                        if !journal.contains_key(&call_id) { continue; }
                        send_diagnostics(&mut socket, &call_id, &timing, false).await
                    }
                    Completion::Result { call_id, outcome } => {
                        let Some(call) = journal.get_mut(&call_id) else { continue; };
                        let replayed = call.receipt.is_some();
                        if let Err(error) = call.finish(&outcome) { break ConnectionEnd::Failed(error); }
                        send_retained(&mut socket, call, replayed).await
                    }
                };
                if let Err(error) = sent {
                    break if detaching { ConnectionEnd::DetachFailed(error) } else { ConnectionEnd::Failed(error) };
                }
            },
            incoming = socket.next() => {
                if let Some(Ok(Message::Pong(payload))) = &incoming {
                    if awaiting_pong.as_deref() == Some(payload.as_ref()) {
                        awaiting_pong = None;
                        let roundtrip_ms = ping_started.take().expect("pending ping clock").elapsed().as_secs_f64() * 1000.0;
                        tracing::info!(target: "nanocodex_oai_tools::attachment", stage = "attachment.transport_rtt", connection_id, roundtrip_ms, "established socket heartbeat roundtrip observed");
                        tracing::info!(target: "nanocodex_oai_tools::attachment", stage = "attachment.heartbeat_pong", pending_calls = journal.values().filter(|call| call.task.is_some()).count(), pending_receipts = journal.values().filter(|call| call.receipt.is_some()).count(), "attachment heartbeat acknowledged");
                    }
                    continue;
                }
                let frame = match incoming_frame(&mut socket, incoming).await {
                    Ok(Some(frame)) => frame,
                    Ok(None) => continue,
                    Err(ConnectionEnd::Disconnected) if detaching => break ConnectionEnd::DetachFailed(AttachmentError::Transport("websocket closed while draining".into())),
                    Err(end) => break end,
                };
                match frame {
                    RemoteFrame::Call { session_id, turn_id, call_id, model, name, input, output_token_budget, output_byte_budget, deadline_at } => {
                        let timing = CallClock::received();
                        if draining { break ConnectionEnd::Rejected("call received after drain barrier".into()); }
                        let identity = CallIdentity { session_id:session_id.clone().into(), turn_id:turn_id.clone().map(Into::into), call_id:call_id.clone().into(), model:model.clone().into(), name:name.clone().into(), input:input.clone(), output_token_budget, output_byte_budget, deadline_at };
                        if let Some(retained) = journal.get(call_id.as_str()) {
                            if retained.identity != identity { break ConnectionEnd::Rejected("call identity conflicts with retained command".into()); }
                            let sent = if retained.receipt.is_some() {
                                send_retained(&mut socket, retained, true).await
                            } else { send(&mut socket, &ExecutorFrame::Status { call_id: &call_id, state: "running" }).await };
                            if let Err(error) = sent { break ConnectionEnd::Failed(error); }
                            continue;
                        }
                        let phase_span = tracing::info_span!(target: "nanocodex_oai_tools::attachment", "attachment.call.phases",
                            transport_call_id = call_id.as_str(), session_id = session_id.as_str(),
                            host_turn_id = turn_id.as_deref().filter(|id| id.len() <= 256 && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))).unwrap_or(""));
                        timing.lock().unwrap().span = phase_span;
                        if let Err(error) = send_diagnostics(&mut socket, &call_id, &timing, false).await { break ConnectionEnd::Failed(error); }
                        let call_events = begin_call_events(
                            events,
                            call_id.clone().into(),
                            name.clone().into(),
                            config.metadata.as_ref().map(AttachmentMetadata::attachment_id),
                        );
                        let tool_timeout = runtime.timeout_ms(&name).unwrap_or(0);
                        active.retain(|call| !call.task.is_finished());
                        let reason = if tool_timeout == 0 {
                            Some("tool is not in the pinned catalog")
                        } else if deadline_at <= now_ms() {
                            Some("tool deadline elapsed before execution")
                        } else { None };
                        if let Some(reason) = reason {
                            call_events.complete(events, AttachmentCallOutcome::Unavailable);
                            let mut retained = RetainedCall { identity, task: None, timing, receipt: None };
                            if let Err(error) = retained.finish(&unavailable(reason)) { break ConnectionEnd::Failed(error); }
                            journal.insert(call_id.clone().into(), retained);
                            if let Err(error) = send_retained(&mut socket, journal.get(call_id.as_str()).unwrap(), false).await { break ConnectionEnd::Failed(error); }
                            continue;
                        }
                        let task = start_call(runtime, active, identity.clone(), Arc::clone(&timing), tool_timeout, call_events, completed_tx.clone(), events);
                        journal.insert(call_id.into(), RetainedCall { identity, task: Some(task), timing, receipt: None });
                    }
                    RemoteFrame::TurnEnded { session_id, turn_id, hook_event_name } => {
                        // The trusted broker sends this only after the turn has
                        // settled. Keep cleanup ordered before another turn can
                        // use this same retained provider session.
                        if tokio::time::timeout(Duration::from_secs(5), runtime.end_turn(&session_id, &turn_id, &hook_event_name)).await.is_err() {
                            tracing::warn!(target: "nanocodex_oai_tools::attachment",
                                "turn cleanup timed out; not retried");
                        }
                    }
                    RemoteFrame::Cancel { call_id } => {
                        tracing::info!(target: "nanocodex_oai_tools::attachment", stage = "attachment.call.cancel_received", transport_call_id = call_id.as_str(), pending_calls = journal.values().filter(|call| call.task.is_some()).count(), pending_receipts = journal.values().filter(|call| call.receipt.is_some()).count(), reason_code = "cancel_received", "attachment cancellation received");
                        if let Some(call) = journal.get_mut(call_id.as_str()) {
                            let replayed = call.receipt.is_some();
                            // A queued terminal receipt wins cancellation races.
                            if call.timing.lock().unwrap().result_queued.is_some() { continue; }
                            if let Some(task) = call.task.take() {
                                task.abort();
                                if let Some(index) = active.iter().position(|active| active.task.id() == task.id()) {
                                    let _ = active.swap_remove(index).task.await;
                                }
                                // Completion may have won between the first clock check and abort.
                                if call.timing.lock().unwrap().result_queued.is_some() { continue; }
                                if let Err(error) = call.finish(&ambiguous("tool execution was cancelled after dispatch")) { break ConnectionEnd::Failed(error); }
                            }
                            if call.receipt.is_some()
                                && let Err(error) = send_retained(&mut socket, call, replayed).await { break ConnectionEnd::Failed(error); }
                        }
                    }
                    RemoteFrame::Ack { call_id } => {
                        tracing::info!(target: "nanocodex_oai_tools::attachment", stage = "attachment.result_ack_received", transport_call_id = call_id.as_str(), pending_calls = journal.values().filter(|call| call.task.is_some()).count(), pending_receipts = journal.values().filter(|call| call.receipt.is_some()).count(), "attachment receipt acknowledgement received");
                        if journal.get(call_id.as_str()).is_some_and(|call| call.receipt.is_some()) {
                            journal.remove(call_id.as_str());
                        }
                    }
                    RemoteFrame::Recover { call_ids } => {
                        for call_id in call_ids {
                            let sent = match journal.get(call_id.as_str()) {
                                Some(call) if call.receipt.is_some() => send_retained(&mut socket, call, true).await,
                                Some(_) => send(&mut socket, &ExecutorFrame::Status { call_id: &call_id, state: "running" }).await,
                                None => send(&mut socket, &ExecutorFrame::Status { call_id: &call_id, state: "missing" }).await,
                            };
                            if let Err(error) = sent { return ConnectionEnd::Failed(error); }
                        }
                    }
                    RemoteFrame::Draining {} => {
                        if !detaching || draining { break ConnectionEnd::Rejected("unexpected draining acknowledgement".into()); }
                        tracing::info!(target: "nanocodex_oai_tools::attachment", stage = "attachment.draining", pending_calls = journal.values().filter(|call| call.task.is_some()).count(), pending_receipts = journal.values().filter(|call| call.receipt.is_some()).count(), "attachment drain barrier acknowledged");
                        draining = true;
                    }
                    RemoteFrame::Ready {} => break ConnectionEnd::Rejected("unexpected ready".into()),
                }
            }
        }
    };

    tracing::info!(target: "nanocodex_oai_tools::attachment", stage = "attachment.socket.pending_at_close", reason_code = end.reason_code(), pending_calls = journal.values().filter(|call| call.task.is_some()).count(), pending_receipts = journal.values().filter(|call| call.receipt.is_some()).count(), "attachment socket pending work at close");
    if matches!(&end, ConnectionEnd::HeartbeatTimeout) {
        // The transport is still writable: send a reason before relinquishing
        // the socket, including when a detach was draining in the background.
        close_heartbeat_timeout(&mut socket).await;
    }

    let end = if detaching {
        match end {
            ConnectionEnd::Disconnected => ConnectionEnd::DetachFailed(AttachmentError::Transport(
                "websocket disconnected while draining".into(),
            )),
            ConnectionEnd::HeartbeatTimeout => ConnectionEnd::DetachFailed(
                AttachmentError::Transport(HEARTBEAT_TIMEOUT_REASON.into()),
            ),
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
        ConnectionEnd::HeartbeatTimeout => {}
        ConnectionEnd::Disconnected | ConnectionEnd::Failed(_) | ConnectionEnd::DetachFailed(_) => {
        }
    }
    end
}

async fn close_heartbeat_timeout<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tracing::warn!(target: "nanocodex_oai_tools::attachment", stage = "attachment.socket.close_sent", close_code = u16::from(CloseCode::Restart), reason_code = "heartbeat_timeout", "attachment close sent");
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        socket.close(Some(CloseFrame {
            code: CloseCode::Restart,
            reason: HEARTBEAT_TIMEOUT_REASON.into(),
        })),
    )
    .await;
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
            Err(_) => {
                tracing::warn!(target: "nanocodex_oai_tools::attachment", stage = "attachment.ready_timeout", reason_code = "ready_timeout", "attachment readiness timed out");
                Err(ConnectionEnd::Failed(AttachmentError::Transport("timed out waiting for attachment readiness".into())))
            },
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
    if let Some(Ok(Message::Close(frame))) = &incoming {
        tracing::info!(target: "nanocodex_oai_tools::attachment", stage = "attachment.socket.close_received", close_code = frame.as_ref().map(|frame| u16::from(frame.code)), reason_code = "remote_close", "attachment close received");
    }
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
        // Unsolicited control pongs are harmless; only matching pongs prove health.
        Some(Ok(Message::Pong(_))) => Ok(None),
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
    tracing::warn!(target: "nanocodex_oai_tools::attachment", stage = "attachment.socket.close_sent", close_code = u16::from(CloseCode::Policy), reason_code = "protocol_rejected", "attachment close sent");
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

async fn send_diagnostics<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    call_id: &str,
    clock: &Arc<Mutex<CallClock>>,
    prepared: bool,
) -> Result<(), AttachmentError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let phases = {
        let mut clock = clock.lock().unwrap();
        let now = Instant::now();
        let phases = [
            (DiagnosticStage::Received, Some(clock.received)),
            (DiagnosticStage::ExecutionStarted, clock.execution_started),
            (DiagnosticStage::ExecutionFinished, clock.execution_finished),
            (DiagnosticStage::ResultPrepared, prepared.then_some(now)),
        ];
        let mut pending = Vec::with_capacity(4);
        for (index, (stage, at)) in phases.into_iter().enumerate() {
            if let Some(at) = at
                && clock.diagnostics_sent & (1 << index) == 0
            {
                clock.diagnostics_sent |= 1 << index;
                let elapsed_ms = (at.saturating_duration_since(clock.received).as_secs_f64()
                    * 1000.0)
                    .min(9_007_199_254_740_991.0);
                pending.push((stage, elapsed_ms, clock.span.clone()));
            }
        }
        pending
    };
    let pending_count = phases.len();
    for (stage, elapsed_ms, span) in phases {
        if matches!(
            stage,
            DiagnosticStage::Received | DiagnosticStage::ResultPrepared
        ) {
            span.in_scope(|| tracing::info!(target: "nanocodex_oai_tools::attachment", stage = stage.name(), elapsed_ms, "attachment execution phase"));
        }
        feed(
            socket,
            &ExecutorFrame::Diagnostic {
                call_id,
                stage,
                elapsed_ms,
            },
        )
        .await?;
    }
    // Live progress remains promptly visible; terminal phases share the result flush.
    if !prepared && pending_count > 0 {
        socket
            .flush()
            .await
            .map_err(|error| AttachmentError::Transport(error.to_string().into()))?;
    }
    Ok(())
}

async fn send_retained<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    call: &RetainedCall,
    replayed: bool,
) -> Result<(), AttachmentError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let started = Instant::now();
    let received = call.timing.lock().unwrap().received;
    tracing::info!(target: "nanocodex_oai_tools::attachment",
        stage = "attachment.result_send_started", transport_call_id = call.identity.call_id.as_ref(),
        replayed, elapsed_ms = started.duration_since(received).as_secs_f64() * 1000.0,
        "attachment terminal transmission started");
    send_diagnostics(socket, &call.identity.call_id, &call.timing, true).await?;
    socket
        .feed(Message::Text(
            call.receipt
                .as_ref()
                .expect("terminal receipt")
                .clone()
                .into(),
        ))
        .await
        .map_err(|error| AttachmentError::Transport(error.to_string().into()))?;
    let flush_started = Instant::now();
    tracing::info!(target: "nanocodex_oai_tools::attachment",
        stage = "attachment.result_flush_started", transport_call_id = call.identity.call_id.as_ref(),
        replayed, feed_ms = flush_started.duration_since(started).as_secs_f64() * 1000.0,
        elapsed_ms = flush_started.duration_since(received).as_secs_f64() * 1000.0,
        "attachment terminal flush started");
    socket
        .flush()
        .await
        .map_err(|error| AttachmentError::Transport(error.to_string().into()))?;
    let flushed = Instant::now();
    // Local flush completion is not peer receipt or acknowledgement. Keep these
    // observations out of the immutable journaled result and its replay identity.
    tracing::info!(target: "nanocodex_oai_tools::attachment",
        stage = if replayed { "attachment.result_replayed" } else { "attachment.result_sent" },
        transport_call_id = call.identity.call_id.as_ref(),
        feed_ms = flush_started.duration_since(started).as_secs_f64() * 1000.0,
        flush_ms = flushed.duration_since(flush_started).as_secs_f64() * 1000.0,
        send_ms = flushed.duration_since(started).as_secs_f64() * 1000.0,
        elapsed_ms = flushed.duration_since(received).as_secs_f64() * 1000.0,
        "attachment terminal receipt flushed");
    Ok(())
}

fn encode_result(
    call_id: &str,
    outcome: &Value,
    clock: &Arc<Mutex<CallClock>>,
) -> Result<String, AttachmentError> {
    let encode_started = Instant::now();
    let mut timing = clock.lock().unwrap().snapshot(encode_started);
    // Encode the business receipt once, then append small timing metadata. Its
    // final encoding/append and socket write remain in the roundtrip residual.
    let mut text = serde_json::to_string(&ExecutorFrame::Result {
        call_id,
        outcome,
        timing: None,
    })
    .map_err(|error| AttachmentError::Transport(error.to_string().into()))?;
    let encoded_at = Instant::now();
    let encode_ms = encoded_at.duration_since(encode_started).as_secs_f64() * 1000.0;
    timing.result_encode_ms += encode_ms;
    timing.host_elapsed_ms += encode_ms;
    text.pop(); // The serialized tagged result is always a JSON object.
    text.push_str(",\"timing\":");
    text.push_str(
        &serde_json::to_string(&timing)
            .map_err(|error| AttachmentError::Transport(error.to_string().into()))?,
    );
    text.push('}');
    Ok(text)
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

async fn feed<S, T: serde::Serialize>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    frame: &T,
) -> Result<(), AttachmentError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let text = serde_json::to_string(frame)
        .map_err(|error| AttachmentError::Transport(error.to_string().into()))?;
    socket
        .feed(Message::Text(text.into()))
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
            target: "nanocodex_oai_tools::attachment",
            stage = "attachment.connecting",
            "connecting attachment"
        ),
        AttachmentEvent::Attached => tracing::info!(
            target: "nanocodex_oai_tools::attachment",
            stage = "attachment.attached",
            "attachment accepted"
        ),
        AttachmentEvent::CatalogPublished { tool_count } => tracing::info!(
            target: "nanocodex_oai_tools::attachment",
            stage = "attachment.catalog_published",
            tool.count = tool_count,
            "attachment catalog published"
        ),
        AttachmentEvent::Detached { .. } => tracing::info!(
            target: "nanocodex_oai_tools::attachment",
            stage = "attachment.detached",
            "attachment detached"
        ),
        AttachmentEvent::Fenced { .. } => tracing::warn!(
            target: "nanocodex_oai_tools::attachment",
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

    #[tokio::test]
    async fn heartbeat_timeout_sends_a_close_reason_to_the_broker() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (hand_io, broker_io) = tokio::io::duplex(1024);
        let (mut hand, mut broker) = tokio::join!(
            tokio_tungstenite::WebSocketStream::from_raw_socket(hand_io, Role::Client, None),
            tokio_tungstenite::WebSocketStream::from_raw_socket(broker_io, Role::Server, None),
        );
        close_heartbeat_timeout(&mut hand).await;
        let Some(Ok(Message::Close(Some(frame)))) = broker.next().await else {
            panic!("heartbeat timeout must deliver a WebSocket close frame");
        };
        assert_eq!(frame.code, CloseCode::Restart);
        assert_eq!(frame.reason.to_string(), HEARTBEAT_TIMEOUT_REASON);
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
        assert_eq!(captured.target, "nanocodex_oai_tools::attachment");
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
