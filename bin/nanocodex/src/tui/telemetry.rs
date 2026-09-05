use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Instant,
};

use nanocodex::agent::events::{AgentEventKind, TimedAgentEvent, monotonic_now_ns};
use tracing::{info, info_span};

use super::{
    app::{App, PaneId},
    terminal::DrawMetrics,
};

const TARGET: &str = "nanocodex_stream_timing";

pub(super) struct ReceivedEvent {
    pane: PaneId,
    request_id: Arc<str>,
    seq: u64,
    kind: AgentEventKind,
    payload_bytes: usize,
    source_received_ns: Option<u64>,
    emitted_ns: u64,
    received_ns: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ViewState {
    btw_id: Option<u64>,
    btw_request_id: Option<Arc<str>>,
    focus: PaneId,
}

impl ViewState {
    fn from_app(app: &App) -> Self {
        Self {
            btw_id: app.btw.as_ref().map(|btw| btw.id),
            btw_request_id: app
                .btw
                .as_ref()
                .and_then(|btw| btw.request_id.as_ref().map(Arc::clone)),
            focus: app.focus,
        }
    }

    const fn view(&self) -> &'static str {
        if self.btw_id.is_some() {
            "split"
        } else {
            "main"
        }
    }

    const fn focus(&self) -> &'static str {
        pane_name(self.focus)
    }
}

#[derive(Default)]
pub(super) struct ViewTelemetry {
    main_session_id: Option<Arc<str>>,
    change_index: u64,
    last: Option<ViewState>,
}

impl ViewTelemetry {
    pub(super) const fn new(main_session_id: Arc<str>) -> Self {
        Self {
            main_session_id: Some(main_session_id),
            change_index: 0,
            last: None,
        }
    }

    pub(super) fn observe(&mut self, app: &App) {
        let state = ViewState::from_app(app);
        if self.last.as_ref() == Some(&state) {
            return;
        }

        self.change_index = self.change_index.saturating_add(1);
        let previous_view = self.last.as_ref().map_or("none", ViewState::view);
        let previous_focus = self.last.as_ref().map_or("none", ViewState::focus);
        let transition = transition(self.last.as_ref(), &state);
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
            tui.main.session_id = tracing::field::Empty,
            tui.active.session_id = tracing::field::Empty,
            tui.btw.open = state.btw_id.is_some(),
            tui.btw.id = tracing::field::Empty,
            tui.btw.session_id = tracing::field::Empty,
            previous.tui.view = previous_view,
            previous.tui.focus = previous_focus,
        );
        if let Some(main_session_id) = &self.main_session_id {
            span.record("tui.main.session_id", main_session_id.as_ref());
        }
        let active_session_id = match state.focus {
            PaneId::Main => self.main_session_id.as_deref(),
            PaneId::Btw(_) => state.btw_request_id.as_deref(),
        };
        if let Some(active_session_id) = active_session_id {
            span.record("tui.active.session_id", active_session_id);
        }
        if let Some(id) = state.btw_id {
            span.record("tui.btw.id", id);
        }
        if let Some(request_id) = &state.btw_request_id {
            span.record("tui.btw.session_id", request_id.as_ref());
        }
        span.in_scope(|| info!(target: "nanocodex", "TUI view state changed"));
        self.last = Some(state);
    }
}

fn transition(previous: Option<&ViewState>, current: &ViewState) -> &'static str {
    let Some(previous) = previous else {
        return "initialized";
    };
    match (previous.btw_id, current.btw_id) {
        (None, Some(_)) => "btw_opened",
        (Some(_), None) => "btw_closed",
        _ if previous.focus != current.focus => "focus_changed",
        _ if previous.btw_request_id != current.btw_request_id => "btw_attached",
        _ => "state_changed",
    }
}

pub(super) const fn pane_name(pane: PaneId) -> &'static str {
    match pane {
        PaneId::Main => "main",
        PaneId::Btw(_) => "btw",
    }
}

#[derive(Default)]
pub(super) struct StreamTelemetry {
    frame: u64,
    pending: HashMap<PaneId, PendingFrame>,
    registered_turns: HashMap<PaneId, VecDeque<RegisteredTurn>>,
    active_turns: HashMap<PaneId, ActiveTurn>,
    finishing_turns: HashMap<PaneId, Vec<ActiveTurn>>,
}

struct PendingFrame {
    request_id: Arc<str>,
    first_seq: u64,
    last_seq: u64,
    first_source_received_ns: Option<u64>,
    last_source_received_ns: Option<u64>,
    first_emitted_ns: u64,
    last_emitted_ns: u64,
    first_received_ns: u64,
    last_received_ns: u64,
    last_applied_ns: u64,
    event_count: usize,
    assistant_delta_count: usize,
    payload_bytes: usize,
}

struct RegisteredTurn {
    id: u64,
    span: tracing::Span,
}

struct ActiveTurn {
    request_id: Arc<str>,
    turn_id: Option<u64>,
    span: Option<tracing::Span>,
    waiting_for_registration: bool,
    event_count: u64,
    assistant_delta_count: u64,
    payload_bytes: u64,
    frame_count: u64,
    render_sum_ns: u64,
    render_max_ns: u64,
    changed_cells: u64,
    changed_cells_max_frame: u64,
    output_bytes: u64,
    output_bytes_max_frame: u64,
    source_to_emit_sum_ns: u64,
    source_to_emit_max_ns: u64,
    emit_to_receive_sum_ns: u64,
    emit_to_receive_max_ns: u64,
    receive_to_apply_sum_ns: u64,
    receive_to_apply_max_ns: u64,
    receive_to_apply_max_kind: Option<AgentEventKind>,
    receive_to_apply_max_seq: u64,
    source_to_present_max_ns: u64,
    first_source_to_present_ns: Option<u64>,
    last_source_to_present_ns: Option<u64>,
    pending_first_source_ns: Option<u64>,
    pending_last_source_ns: Option<u64>,
    terminal_pending: bool,
}

impl ActiveTurn {
    fn new(request_id: Arc<str>, registered: Option<RegisteredTurn>) -> Self {
        let waiting_for_registration = registered.is_none();
        Self {
            request_id,
            turn_id: registered.as_ref().map(|turn| turn.id),
            span: registered.map(|turn| turn.span),
            waiting_for_registration,
            event_count: 0,
            assistant_delta_count: 0,
            payload_bytes: 0,
            frame_count: 0,
            render_sum_ns: 0,
            render_max_ns: 0,
            changed_cells: 0,
            changed_cells_max_frame: 0,
            output_bytes: 0,
            output_bytes_max_frame: 0,
            source_to_emit_sum_ns: 0,
            source_to_emit_max_ns: 0,
            emit_to_receive_sum_ns: 0,
            emit_to_receive_max_ns: 0,
            receive_to_apply_sum_ns: 0,
            receive_to_apply_max_ns: 0,
            receive_to_apply_max_kind: None,
            receive_to_apply_max_seq: 0,
            source_to_present_max_ns: 0,
            first_source_to_present_ns: None,
            last_source_to_present_ns: None,
            pending_first_source_ns: None,
            pending_last_source_ns: None,
            terminal_pending: false,
        }
    }

    fn record_event(&mut self, event: &ReceivedEvent, applied_ns: u64, schedules_frame: bool) {
        self.event_count = self.event_count.saturating_add(1);
        self.assistant_delta_count = self
            .assistant_delta_count
            .saturating_add(u64::from(event.kind == AgentEventKind::AssistantDelta));
        self.payload_bytes = self
            .payload_bytes
            .saturating_add(event.payload_bytes.try_into().unwrap_or(u64::MAX));
        if let Some(source_ns) = event.source_received_ns {
            let duration = event.emitted_ns.saturating_sub(source_ns);
            self.source_to_emit_sum_ns = self.source_to_emit_sum_ns.saturating_add(duration);
            self.source_to_emit_max_ns = self.source_to_emit_max_ns.max(duration);
        }
        let emit_to_receive_ns = event.received_ns.saturating_sub(event.emitted_ns);
        self.emit_to_receive_sum_ns = self
            .emit_to_receive_sum_ns
            .saturating_add(emit_to_receive_ns);
        self.emit_to_receive_max_ns = self.emit_to_receive_max_ns.max(emit_to_receive_ns);
        let receive_to_apply_ns = applied_ns.saturating_sub(event.received_ns);
        self.receive_to_apply_sum_ns = self
            .receive_to_apply_sum_ns
            .saturating_add(receive_to_apply_ns);
        if receive_to_apply_ns > self.receive_to_apply_max_ns {
            self.receive_to_apply_max_ns = receive_to_apply_ns;
            self.receive_to_apply_max_kind = Some(event.kind);
            self.receive_to_apply_max_seq = event.seq;
        }
        if schedules_frame {
            if self.pending_first_source_ns.is_none() {
                self.pending_first_source_ns = event.source_received_ns;
            }
            if event.source_received_ns.is_some() {
                self.pending_last_source_ns = event.source_received_ns;
            }
            self.terminal_pending |= event.kind.is_terminal();
        }
    }

    fn record_frame(&mut self, presented_ns: u64, render_ns: u64, draw: DrawMetrics) {
        self.frame_count = self.frame_count.saturating_add(1);
        self.render_sum_ns = self.render_sum_ns.saturating_add(render_ns);
        self.render_max_ns = self.render_max_ns.max(render_ns);
        self.changed_cells = self.changed_cells.saturating_add(draw.changed_cells);
        self.changed_cells_max_frame = self.changed_cells_max_frame.max(draw.changed_cells);
        self.output_bytes = self.output_bytes.saturating_add(draw.output_bytes);
        self.output_bytes_max_frame = self.output_bytes_max_frame.max(draw.output_bytes);
        if let Some(source_ns) = self.pending_first_source_ns.take() {
            let duration = presented_ns.saturating_sub(source_ns);
            self.first_source_to_present_ns.get_or_insert(duration);
            self.source_to_present_max_ns = self.source_to_present_max_ns.max(duration);
        }
        if let Some(source_ns) = self.pending_last_source_ns.take() {
            let duration = presented_ns.saturating_sub(source_ns);
            self.last_source_to_present_ns = Some(duration);
            self.source_to_present_max_ns = self.source_to_present_max_ns.max(duration);
        }
    }
}

impl PendingFrame {
    fn new(event: ReceivedEvent, applied_ns: u64) -> Self {
        Self {
            request_id: event.request_id,
            first_seq: event.seq,
            last_seq: event.seq,
            first_source_received_ns: event.source_received_ns,
            last_source_received_ns: event.source_received_ns,
            first_emitted_ns: event.emitted_ns,
            last_emitted_ns: event.emitted_ns,
            first_received_ns: event.received_ns,
            last_received_ns: event.received_ns,
            last_applied_ns: applied_ns,
            event_count: 1,
            assistant_delta_count: usize::from(event.kind == AgentEventKind::AssistantDelta),
            payload_bytes: event.payload_bytes,
        }
    }

    fn push(&mut self, event: ReceivedEvent, applied_ns: u64) {
        self.request_id = event.request_id;
        self.last_seq = event.seq;
        if self.first_source_received_ns.is_none() {
            self.first_source_received_ns = event.source_received_ns;
        }
        if event.source_received_ns.is_some() {
            self.last_source_received_ns = event.source_received_ns;
        }
        self.last_emitted_ns = event.emitted_ns;
        self.last_received_ns = event.received_ns;
        self.last_applied_ns = applied_ns;
        self.event_count = self.event_count.saturating_add(1);
        self.assistant_delta_count = self
            .assistant_delta_count
            .saturating_add(usize::from(event.kind == AgentEventKind::AssistantDelta));
        self.payload_bytes = self.payload_bytes.saturating_add(event.payload_bytes);
    }
}

impl StreamTelemetry {
    pub(super) fn register_turn(&mut self, pane: PaneId, id: u64, span: tracing::Span) {
        if let Some(active) = self
            .active_turns
            .get_mut(&pane)
            .filter(|active| active.waiting_for_registration)
        {
            active.turn_id = Some(id);
            active.span = Some(span);
            active.waiting_for_registration = false;
            return;
        }
        self.registered_turns
            .entry(pane)
            .or_default()
            .push_back(RegisteredTurn { id, span });
    }

    pub(super) fn reject_turn(&mut self, pane: PaneId, id: u64) {
        let Some(turns) = self.registered_turns.get_mut(&pane) else {
            return;
        };
        if let Some(index) = turns.iter().position(|turn| turn.id == id) {
            drop(turns.remove(index));
        }
    }

    pub(super) fn event_received(&self, pane: PaneId, timed: &TimedAgentEvent) -> ReceivedEvent {
        let received_ns = monotonic_now_ns();
        let event = &timed.event;
        let payload_bytes = event.payload.get().len();
        let trace = || {
            tracing::trace!(
                target: TARGET,
                stage = "tui_event_received",
                request.id = %event.request_id,
                tui.pane = pane_name(pane),
                tui.btw.id = pane_btw_id(pane).unwrap_or_default(),
                event.seq = event.seq,
                event.kind = ?event.kind,
                payload.bytes = payload_bytes,
                source_to_agent_emit_ns = timed
                    .timing
                    .source_received_ns
                    .map(|source| timed.timing.emitted_ns.saturating_sub(source))
                    .unwrap_or_default(),
                agent_emit_to_tui_receive_ns = received_ns.saturating_sub(timed.timing.emitted_ns),
                "TUI received an agent event"
            );
        };
        if let Some(span) = self.event_span(pane) {
            span.in_scope(trace);
        } else {
            trace();
        }
        ReceivedEvent {
            pane,
            request_id: Arc::clone(&event.request_id),
            seq: event.seq,
            kind: event.kind,
            payload_bytes,
            source_received_ns: timed.timing.source_received_ns,
            emitted_ns: timed.timing.emitted_ns,
            received_ns,
        }
    }

    fn event_span(&self, pane: PaneId) -> Option<&tracing::Span> {
        self.active_turns
            .get(&pane)
            .and_then(|active| active.span.as_ref())
            .or_else(|| {
                self.registered_turns
                    .get(&pane)
                    .and_then(|turns| turns.front())
                    .map(|turn| &turn.span)
            })
    }

    pub(super) fn event_applied(&mut self, event: ReceivedEvent, schedules_frame: bool) {
        self.event_applied_at(event, schedules_frame, monotonic_now_ns());
    }

    fn event_applied_at(&mut self, event: ReceivedEvent, schedules_frame: bool, applied_ns: u64) {
        if event.kind == AgentEventKind::RunStarted {
            let registered = self
                .registered_turns
                .get_mut(&event.pane)
                .and_then(VecDeque::pop_front);
            if let Some(previous) = self.active_turns.remove(&event.pane) {
                self.finishing_turns
                    .entry(event.pane)
                    .or_default()
                    .push(previous);
            }
            self.active_turns.insert(
                event.pane,
                ActiveTurn::new(Arc::clone(&event.request_id), registered),
            );
        }
        if let Some(active) = self.active_turns.get_mut(&event.pane) {
            active.record_event(&event, applied_ns, schedules_frame);
        }
        self.trace_applied_event(&event, schedules_frame, applied_ns);
        if !schedules_frame {
            return;
        }
        if let Some(pending) = self.pending.get_mut(&event.pane) {
            pending.push(event, applied_ns);
        } else {
            self.pending
                .insert(event.pane, PendingFrame::new(event, applied_ns));
        }
    }

    fn trace_applied_event(&self, event: &ReceivedEvent, schedules_frame: bool, applied_ns: u64) {
        let source_to_emit_ns = event
            .source_received_ns
            .map(|source| event.emitted_ns.saturating_sub(source));
        let emit_to_receive_ns = event.received_ns.saturating_sub(event.emitted_ns);
        let receive_to_apply_ns = applied_ns.saturating_sub(event.received_ns);
        let trace = || {
            tracing::trace!(
                target: TARGET,
                stage = "tui_event_applied",
                request.id = %event.request_id,
                tui.pane = pane_name(event.pane),
                tui.btw.id = pane_btw_id(event.pane).unwrap_or_default(),
                event.seq = event.seq,
                event.kind = ?event.kind,
                schedules_frame,
                source_to_agent_emit_ns = source_to_emit_ns.unwrap_or_default(),
                agent_emit_to_tui_receive_ns = emit_to_receive_ns,
                tui_receive_to_apply_ns = receive_to_apply_ns,
                source_to_tui_apply_ns = event
                    .source_received_ns
                    .map(|source| applied_ns.saturating_sub(source))
                    .unwrap_or_default(),
                "TUI applied an agent event"
            );
        };
        if let Some(span) = self
            .active_turns
            .get(&event.pane)
            .and_then(|active| active.span.as_ref())
        {
            span.in_scope(trace);
        } else {
            trace();
        }
    }

    pub(super) fn frame_presented(
        &mut self,
        render_started: Instant,
        presented_at: Instant,
        draw: DrawMetrics,
        app: &App,
    ) {
        self.frame_presented_at(render_started, presented_at, monotonic_now_ns(), draw, app);
    }

    fn frame_presented_at(
        &mut self,
        render_started: Instant,
        presented_at: Instant,
        presented_ns: u64,
        draw: DrawMetrics,
        app: &App,
    ) {
        self.frame = self.frame.saturating_add(1);
        let render_ns = elapsed_ns(render_started, presented_at);
        let view = ViewState::from_app(app);
        if self.pending.is_empty() {
            tracing::trace!(
                target: TARGET,
                stage = "frame_presented",
                frame = self.frame,
                tui.view = view.view(),
                tui.focus = view.focus(),
                tui.btw.open = view.btw_id.is_some(),
                tui.btw.id = view.btw_id.unwrap_or_default(),
                stream.event_count = 0,
                assistant.delta.count = 0,
                payload.bytes = 0,
                render_ns,
                terminal.changed_cells = draw.changed_cells,
                terminal.output_bytes = draw.output_bytes,
                "TUI presented a frame"
            );
            return;
        }

        for (pane, pending) in std::mem::take(&mut self.pending) {
            let first_source_to_present_ns = pending
                .first_source_received_ns
                .map(|source| presented_ns.saturating_sub(source));
            let last_source_to_present_ns = pending
                .last_source_received_ns
                .map(|source| presented_ns.saturating_sub(source));
            let first_emit_to_present_ns = presented_ns.saturating_sub(pending.first_emitted_ns);
            let last_emit_to_present_ns = presented_ns.saturating_sub(pending.last_emitted_ns);
            let first_receive_to_present_ns =
                presented_ns.saturating_sub(pending.first_received_ns);
            let last_receive_to_present_ns = presented_ns.saturating_sub(pending.last_received_ns);
            let apply_to_present_ns = presented_ns.saturating_sub(pending.last_applied_ns);
            let trace = || {
                tracing::trace!(
                    target: TARGET,
                    stage = "frame_presented",
                    frame = self.frame,
                    tui.view = view.view(),
                    tui.focus = view.focus(),
                    tui.btw.open = view.btw_id.is_some(),
                    tui.btw.id = pane_btw_id(pane).unwrap_or_default(),
                    tui.pane = pane_name(pane),
                    request.id = %pending.request_id,
                    first.event.seq = pending.first_seq,
                    last.event.seq = pending.last_seq,
                    stream.event_count = pending.event_count,
                    assistant.delta.count = pending.assistant_delta_count,
                    payload.bytes = pending.payload_bytes,
                    first_source_to_present_ns = first_source_to_present_ns.unwrap_or_default(),
                    last_source_to_present_ns = last_source_to_present_ns.unwrap_or_default(),
                    first_agent_emit_to_present_ns = first_emit_to_present_ns,
                    last_agent_emit_to_present_ns = last_emit_to_present_ns,
                    first_tui_receive_to_present_ns = first_receive_to_present_ns,
                    last_tui_receive_to_present_ns = last_receive_to_present_ns,
                    tui_apply_to_present_ns = apply_to_present_ns,
                    render_ns,
                    terminal.changed_cells = draw.changed_cells,
                    terminal.output_bytes = draw.output_bytes,
                    "TUI presented a frame"
                );
            };
            if let Some(span) = self
                .active_turns
                .get(&pane)
                .and_then(|active| active.span.as_ref())
            {
                span.in_scope(trace);
            } else {
                trace();
            }

            if let Some(active) = self.active_turns.get_mut(&pane) {
                active.record_frame(presented_ns, render_ns, draw);
            }
            if self
                .active_turns
                .get(&pane)
                .is_some_and(|active| active.terminal_pending)
            {
                self.finish_active_turn(pane);
            }
            if let Some(finishing) = self.finishing_turns.remove(&pane) {
                for mut turn in finishing {
                    turn.record_frame(presented_ns, render_ns, draw);
                    Self::log_finished_turn(pane, &turn);
                }
            }
        }
    }

    fn finish_active_turn(&mut self, pane: PaneId) {
        let Some(active) = self.active_turns.remove(&pane) else {
            return;
        };
        Self::log_finished_turn(pane, &active);
    }

    fn log_finished_turn(pane: PaneId, active: &ActiveTurn) {
        let log = || {
            info!(
                target: "nanocodex",
                stage = "tui.stream.completed",
                request.id = %active.request_id,
                tui.pane = pane_name(pane),
                tui.btw.id = pane_btw_id(pane).unwrap_or_default(),
                tui.turn.id = active.turn_id.unwrap_or_default(),
                stream.event_count = active.event_count,
                assistant.delta.count = active.assistant_delta_count,
                payload.bytes = active.payload_bytes,
                frame.count = active.frame_count,
                render.sum_ns = active.render_sum_ns,
                render.max_ns = active.render_max_ns,
                frame.events_per_frame_milli = active
                    .event_count
                    .saturating_mul(1_000)
                    / active.frame_count.max(1),
                terminal.changed_cells = active.changed_cells,
                terminal.changed_cells.max_frame = active.changed_cells_max_frame,
                terminal.output_bytes = active.output_bytes,
                terminal.output_bytes.max_frame = active.output_bytes_max_frame,
                source_to_agent_emit.sum_ns = active.source_to_emit_sum_ns,
                source_to_agent_emit.max_ns = active.source_to_emit_max_ns,
                agent_emit_to_tui_receive.sum_ns = active.emit_to_receive_sum_ns,
                agent_emit_to_tui_receive.max_ns = active.emit_to_receive_max_ns,
                tui_receive_to_apply.sum_ns = active.receive_to_apply_sum_ns,
                tui_receive_to_apply.max_ns = active.receive_to_apply_max_ns,
                tui_receive_to_apply.max_kind = ?active.receive_to_apply_max_kind,
                tui_receive_to_apply.max_seq = active.receive_to_apply_max_seq,
                source_to_present.first_ns = active.first_source_to_present_ns.unwrap_or_default(),
                source_to_present.last_ns = active.last_source_to_present_ns.unwrap_or_default(),
                source_to_present.max_ns = active.source_to_present_max_ns,
                "TUI stream timing completed"
            );
        };
        if let Some(span) = active.span.as_ref() {
            span.in_scope(log);
        } else {
            log();
        }
    }
}

pub(super) const fn pane_btw_id(pane: PaneId) -> Option<u64> {
    match pane {
        PaneId::Main => None,
        PaneId::Btw(id) => Some(id),
    }
}

pub(super) fn elapsed_ns(start: Instant, end: Instant) -> u64 {
    u64::try_from(end.saturating_duration_since(start).as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc, time::Instant};

    use nanocodex::agent::events::AgentEventKind;

    use crate::tui::{
        app::{App, PaneId},
        terminal::DrawMetrics,
    };

    use super::{ReceivedEvent, StreamTelemetry, ViewTelemetry};

    fn received(seq: u64, kind: AgentEventKind, source_ns: Option<u64>) -> ReceivedEvent {
        ReceivedEvent {
            pane: PaneId::Main,
            request_id: Arc::from("request"),
            seq,
            kind,
            payload_bytes: 5,
            source_received_ns: source_ns,
            emitted_ns: source_ns.unwrap_or(100).saturating_add(10),
            received_ns: source_ns.unwrap_or(100).saturating_add(20),
        }
    }

    #[test]
    fn coalesced_events_are_consumed_by_one_presented_frame() {
        let mut telemetry = StreamTelemetry::default();
        let app = App::new(PathBuf::from("."));
        telemetry.event_applied_at(received(1, AgentEventKind::RunStarted, None), true, 130);
        for seq in 2..=4 {
            telemetry.event_applied_at(
                received(seq, AgentEventKind::AssistantDelta, Some(100 + seq)),
                true,
                140 + seq,
            );
        }

        let pending = telemetry.pending.get(&PaneId::Main).unwrap();
        assert_eq!(pending.event_count, 4);
        assert_eq!(pending.assistant_delta_count, 3);
        assert_eq!(pending.payload_bytes, 20);

        let now = Instant::now();
        telemetry.frame_presented_at(
            now,
            now,
            200,
            DrawMetrics {
                changed_cells: 9,
                output_bytes: 30,
            },
            &app,
        );
        assert!(telemetry.pending.is_empty());
        let active = telemetry.active_turns.get(&PaneId::Main).unwrap();
        assert_eq!(active.frame_count, 1);
        assert_eq!(active.changed_cells, 9);
        assert_eq!(active.output_bytes, 30);
        assert_eq!(active.source_to_emit_max_ns, 10);
        assert_eq!(active.emit_to_receive_max_ns, 10);
        assert_eq!(active.receive_to_apply_max_ns, 20);
        assert_eq!(
            active.receive_to_apply_max_kind,
            Some(AgentEventKind::AssistantDelta)
        );
        assert_eq!(active.receive_to_apply_max_seq, 2);
        assert_eq!(active.source_to_present_max_ns, 98);
    }

    #[test]
    fn terminal_frame_finishes_the_active_turn() {
        let mut telemetry = StreamTelemetry::default();
        let app = App::new(PathBuf::from("."));
        telemetry.event_applied_at(received(1, AgentEventKind::RunStarted, None), true, 130);
        telemetry.event_applied_at(received(2, AgentEventKind::RunCompleted, None), true, 140);
        let now = Instant::now();
        telemetry.frame_presented_at(now, now, 200, DrawMetrics::default(), &app);
        assert!(!telemetry.active_turns.contains_key(&PaneId::Main));
    }

    #[test]
    fn queued_run_does_not_replace_terminal_turn_before_flush() {
        let mut telemetry = StreamTelemetry::default();
        let app = App::new(PathBuf::from("."));
        telemetry.event_applied_at(received(1, AgentEventKind::RunStarted, None), true, 130);
        telemetry.event_applied_at(
            received(2, AgentEventKind::RunCompleted, Some(140)),
            true,
            170,
        );
        telemetry.event_applied_at(received(3, AgentEventKind::RunStarted, None), true, 180);

        assert_eq!(telemetry.finishing_turns[&PaneId::Main].len(), 1);
        assert_eq!(telemetry.active_turns[&PaneId::Main].event_count, 1);

        let now = Instant::now();
        telemetry.frame_presented_at(now, now, 220, DrawMetrics::default(), &app);
        assert!(!telemetry.finishing_turns.contains_key(&PaneId::Main));
        assert!(telemetry.active_turns.contains_key(&PaneId::Main));
    }

    #[test]
    fn view_state_tracks_btw_lifecycle_focus_and_session_mapping() {
        let mut app = App::new(PathBuf::from("."));
        let mut telemetry = ViewTelemetry::new(Arc::from("main-session"));

        telemetry.observe(&app);
        assert_eq!(telemetry.change_index, 1);
        assert_eq!(telemetry.last.as_ref().unwrap().view(), "main");
        assert_eq!(telemetry.main_session_id.as_deref(), Some("main-session"));

        let id = app.begin_btw();
        telemetry.observe(&app);
        assert_eq!(telemetry.change_index, 2);
        assert_eq!(telemetry.last.as_ref().unwrap().focus(), "btw");

        app.btw_opened(id, Arc::from("btw-session"));
        telemetry.observe(&app);
        assert_eq!(telemetry.change_index, 3);
        assert_eq!(
            telemetry.last.as_ref().unwrap().btw_request_id.as_deref(),
            Some("btw-session")
        );

        app.toggle_focus();
        telemetry.observe(&app);
        assert_eq!(telemetry.change_index, 4);
        assert_eq!(telemetry.last.as_ref().unwrap().focus(), "main");

        app.close_btw(id);
        telemetry.observe(&app);
        assert_eq!(telemetry.change_index, 5);
        assert_eq!(telemetry.last.as_ref().unwrap().view(), "main");
    }
}
