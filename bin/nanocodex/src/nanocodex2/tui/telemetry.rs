//! Stream and view telemetry for the TUI: how long agent output takes to reach
//! the screen and how much terminal work each frame costs. Spans go to the
//! regular tracing pipeline (log file and, when configured, OTLP).

use std::{
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use nanocodex_managed::{ManagedEvent, ManagedEventData};
use tracing::{info, info_span};

use super::terminal::DrawMetrics;

const TARGET: &str = "nanocodex_stream_timing";

/// One agent event as it reached the TUI.
pub(crate) struct Received {
    kind: &'static str,
    turn_id: Option<Arc<str>>,
    terminal: bool,
    payload_bytes: u64,
    /// Producer timestamp in Unix nanoseconds, when the source supplied one.
    source_unix_ns: Option<u64>,
    received: Instant,
}

impl Received {
    pub(crate) fn managed(event: &ManagedEvent) -> Self {
        let (kind, terminal, payload_bytes) = match &event.data {
            ManagedEventData::AgentCreated { .. } => ("agent.created", false, 0),
            ManagedEventData::TurnAccepted { .. } => ("turn.accepted", false, 0),
            ManagedEventData::TurnCancelling { .. } => ("turn.cancelling", false, 0),
            ManagedEventData::TurnCompleted { .. } => ("turn.completed", true, 0),
            ManagedEventData::TurnCancelled { .. } => ("turn.cancelled", true, 0),
            ManagedEventData::TurnRetryable { .. } => ("turn.retryable", false, 0),
            ManagedEventData::TurnFailed { .. } => ("turn.failed", true, 0),
            ManagedEventData::Event { event, .. } => ("agent.event", false, event.get().len()),
            ManagedEventData::StreamFailed { .. } => ("stream.failed", false, 0),
        };
        Self {
            kind,
            turn_id: event
                .turn_id
                .as_deref()
                .or_else(|| event.data.turn_id())
                .map(Arc::from),
            terminal,
            payload_bytes: u64::try_from(payload_bytes).unwrap_or(u64::MAX),
            source_unix_ns: event
                .created_at
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                .map(|seconds| (seconds * 1e9) as u64),
            received: Instant::now(),
        }
    }

    /// A local agent event. Local turns supply their own monotonic timing,
    /// which this boundary reports relative to receipt.
    pub(crate) fn local(
        kind: &'static str,
        turn_id: Option<&str>,
        terminal: bool,
        payload_bytes: usize,
    ) -> Self {
        Self {
            kind,
            turn_id: turn_id.map(Arc::from),
            terminal,
            payload_bytes: u64::try_from(payload_bytes).unwrap_or(u64::MAX),
            source_unix_ns: Some(unix_now_ns()),
            received: Instant::now(),
        }
    }

    pub(crate) const fn is_terminal(&self) -> bool {
        self.terminal
    }

    pub(crate) const fn kind(&self) -> &'static str {
        self.kind
    }
}

#[derive(Default)]
struct ActiveTurn {
    turn_id: Option<Arc<str>>,
    started: Option<Instant>,
    event_count: u64,
    payload_bytes: u64,
    frame_count: u64,
    render_sum_ns: u64,
    render_max_ns: u64,
    changed_cells: u64,
    changed_cells_max_frame: u64,
    output_bytes: u64,
    output_bytes_max_frame: u64,
    source_to_receive_max_ns: u64,
    receive_to_present_max_ns: u64,
    first_receive_to_present_ns: Option<u64>,
    source_to_present_first_ns: Option<u64>,
    source_to_present_last_ns: Option<u64>,
    source_to_present_max_ns: u64,
    terminal_pending: bool,
}

#[derive(Default)]
struct PendingFrame {
    event_count: u64,
    payload_bytes: u64,
    first_received: Option<Instant>,
    last_received: Option<Instant>,
    first_source_ns: Option<u64>,
    last_source_ns: Option<u64>,
}

/// Per-turn stream timing from event receipt to presented frame.
#[derive(Default)]
pub(crate) struct StreamTelemetry {
    frame: u64,
    pending: PendingFrame,
    active: Option<ActiveTurn>,
}

impl StreamTelemetry {
    pub(crate) fn received(&mut self, session_id: &str, event: Received) {
        let source_to_receive_ns = event
            .source_unix_ns
            .map(|source| unix_now_ns().saturating_sub(source));
        tracing::trace!(
            target: TARGET,
            stage = "tui_event_received",
            session.id = session_id,
            turn.id = event.turn_id.as_deref().unwrap_or_default(),
            event.kind = event.kind,
            payload.bytes = event.payload_bytes,
            source_to_tui_receive_ns = source_to_receive_ns.unwrap_or_default(),
            "TUI received an agent event"
        );
        let active = self.active.get_or_insert_with(|| ActiveTurn {
            started: Some(event.received),
            ..ActiveTurn::default()
        });
        if active.turn_id.is_none() {
            active.turn_id.clone_from(&event.turn_id);
        }
        active.event_count = active.event_count.saturating_add(1);
        active.payload_bytes = active.payload_bytes.saturating_add(event.payload_bytes);
        if let Some(duration) = source_to_receive_ns {
            active.source_to_receive_max_ns = active.source_to_receive_max_ns.max(duration);
        }
        active.terminal_pending |= event.terminal;
        let pending = &mut self.pending;
        pending.event_count = pending.event_count.saturating_add(1);
        pending.payload_bytes = pending.payload_bytes.saturating_add(event.payload_bytes);
        pending.first_received.get_or_insert(event.received);
        pending.last_received = Some(event.received);
        if pending.first_source_ns.is_none() {
            pending.first_source_ns = event.source_unix_ns;
        }
        if event.source_unix_ns.is_some() {
            pending.last_source_ns = event.source_unix_ns;
        }
    }

    pub(crate) fn presented(
        &mut self,
        session_id: &str,
        view: &ViewState,
        render_started: Instant,
        draw: DrawMetrics,
    ) {
        let presented = Instant::now();
        let presented_unix_ns = unix_now_ns();
        self.frame = self.frame.saturating_add(1);
        let render_ns = elapsed_ns(render_started, presented);
        let pending = std::mem::take(&mut self.pending);
        let first_receive_to_present_ns =
            pending.first_received.map(|at| elapsed_ns(at, presented));
        let last_receive_to_present_ns = pending.last_received.map(|at| elapsed_ns(at, presented));
        let first_source_to_present_ns = pending
            .first_source_ns
            .map(|source| presented_unix_ns.saturating_sub(source));
        let last_source_to_present_ns = pending
            .last_source_ns
            .map(|source| presented_unix_ns.saturating_sub(source));
        tracing::trace!(
            target: TARGET,
            stage = "frame_presented",
            frame = self.frame,
            session.id = session_id,
            tui.view = view.view(),
            tui.focus = view.focus(),
            stream.event_count = pending.event_count,
            payload.bytes = pending.payload_bytes,
            first_tui_receive_to_present_ns = first_receive_to_present_ns.unwrap_or_default(),
            last_tui_receive_to_present_ns = last_receive_to_present_ns.unwrap_or_default(),
            first_source_to_present_ns = first_source_to_present_ns.unwrap_or_default(),
            last_source_to_present_ns = last_source_to_present_ns.unwrap_or_default(),
            render_ns,
            terminal.changed_cells = draw.changed_cells,
            terminal.output_bytes = draw.output_bytes,
            "TUI presented a frame"
        );
        let Some(active) = self.active.as_mut() else {
            return;
        };
        active.frame_count = active.frame_count.saturating_add(1);
        active.render_sum_ns = active.render_sum_ns.saturating_add(render_ns);
        active.render_max_ns = active.render_max_ns.max(render_ns);
        active.changed_cells = active.changed_cells.saturating_add(draw.changed_cells);
        active.changed_cells_max_frame = active.changed_cells_max_frame.max(draw.changed_cells);
        active.output_bytes = active.output_bytes.saturating_add(draw.output_bytes);
        active.output_bytes_max_frame = active.output_bytes_max_frame.max(draw.output_bytes);
        if let Some(duration) = last_receive_to_present_ns {
            active.receive_to_present_max_ns = active.receive_to_present_max_ns.max(duration);
        }
        if active.first_receive_to_present_ns.is_none() {
            active.first_receive_to_present_ns = first_receive_to_present_ns;
        }
        if let Some(duration) = first_source_to_present_ns {
            active.source_to_present_first_ns.get_or_insert(duration);
            active.source_to_present_max_ns = active.source_to_present_max_ns.max(duration);
        }
        if let Some(duration) = last_source_to_present_ns {
            active.source_to_present_last_ns = Some(duration);
            active.source_to_present_max_ns = active.source_to_present_max_ns.max(duration);
        }
        if active.terminal_pending
            && let Some(turn) = self.active.take()
        {
            log_finished_turn(session_id, view, &turn, presented);
        }
    }
}

fn log_finished_turn(session_id: &str, view: &ViewState, turn: &ActiveTurn, presented: Instant) {
    let span = info_span!(
        target: "nanocodex",
        parent: None,
        "tui.stream",
        otel.kind = "internal",
        session.id = session_id,
        turn.id = turn.turn_id.as_deref().unwrap_or_default(),
        tui.view = view.view(),
    );
    span.in_scope(|| {
        info!(
            target: "nanocodex",
            stage = "tui.stream.completed",
            session.id = session_id,
            turn.id = turn.turn_id.as_deref().unwrap_or_default(),
            stream.duration_ns = turn.started.map_or(0, |started| elapsed_ns(started, presented)),
            stream.event_count = turn.event_count,
            payload.bytes = turn.payload_bytes,
            frame.count = turn.frame_count,
            frame.events_per_frame_milli = turn.event_count.saturating_mul(1_000) / turn.frame_count.max(1),
            render.sum_ns = turn.render_sum_ns,
            render.max_ns = turn.render_max_ns,
            terminal.changed_cells = turn.changed_cells,
            terminal.changed_cells.max_frame = turn.changed_cells_max_frame,
            terminal.output_bytes = turn.output_bytes,
            terminal.output_bytes.max_frame = turn.output_bytes_max_frame,
            source_to_tui_receive.max_ns = turn.source_to_receive_max_ns,
            tui_receive_to_present.first_ns = turn.first_receive_to_present_ns.unwrap_or_default(),
            tui_receive_to_present.max_ns = turn.receive_to_present_max_ns,
            source_to_present.first_ns = turn.source_to_present_first_ns.unwrap_or_default(),
            source_to_present.last_ns = turn.source_to_present_last_ns.unwrap_or_default(),
            source_to_present.max_ns = turn.source_to_present_max_ns,
            "TUI stream timing completed"
        );
    });
}

/// Which panes are visible and focused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ViewState {
    pub(crate) split: bool,
    pub(crate) focus_main: bool,
    pub(crate) screen: bool,
}

impl ViewState {
    const fn view(&self) -> &'static str {
        if self.split { "split" } else { "main" }
    }

    const fn focus(&self) -> &'static str {
        if self.screen {
            "screen"
        } else if self.focus_main {
            "main"
        } else {
            "btw"
        }
    }
}

/// Emits one tui.view_state span whenever panes open, close or change focus.
#[derive(Default)]
pub(crate) struct ViewTelemetry {
    change_index: u64,
    last: Option<ViewState>,
    session_id: String,
}

impl ViewTelemetry {
    pub(crate) fn observe(&mut self, session_id: &str, state: &ViewState) {
        if self.last.as_ref() == Some(state) && self.session_id == session_id {
            return;
        }
        self.change_index = self.change_index.saturating_add(1);
        let transition = match &self.last {
            None => "initialized",
            Some(previous) if !previous.split && state.split => "btw_opened",
            Some(previous) if previous.split && !state.split => "btw_closed",
            Some(previous) if previous != state => "focus_changed",
            Some(_) => "session_changed",
        };
        let span = info_span!(
            target: "nanocodex",
            parent: None,
            "tui.view_state",
            otel.kind = "internal",
            otel.status_code = "OK",
            state.change_index = self.change_index,
            transition,
            tui.view = state.view(),
            tui.focus = state.focus(),
            tui.main.session_id = session_id,
            previous.tui.view = self.last.as_ref().map_or("none", ViewState::view),
            previous.tui.focus = self.last.as_ref().map_or("none", ViewState::focus),
        );
        span.in_scope(|| info!(target: "nanocodex", "TUI view state changed"));
        self.last = Some(state.clone());
        session_id.clone_into(&mut self.session_id);
    }
}

#[derive(clap::Parser)]
struct EnvObservability {
    #[command(flatten)]
    args: crate::observability::ObservabilityArgs,
}

/// Installs TUI logging from the environment only (RUST_LOG, OTEL_LEVEL,
/// NANOCODEX_LOG_FILE, NANOCODEX_LOG_FORMAT, OTEL_EXPORTER_OTLP_ENDPOINT), so
/// managed command-line flags are unchanged. Without NANOCODEX_LOG_FILE the
/// log goes to the per-launch TUI log file, never to the terminal.
pub(crate) fn install_observability() -> Option<nanocodex_observability::ObservabilityGuard> {
    use clap::Parser as _;
    let parsed = EnvObservability::try_parse_from(["nanocodex"]).ok()?;
    parsed.args.install(true).ok()
}

fn elapsed_ns(start: Instant, end: Instant) -> u64 {
    u64::try_from(end.saturating_duration_since(start).as_nanos()).unwrap_or(u64::MAX)
}

fn unix_now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        .unwrap_or_default()
}
