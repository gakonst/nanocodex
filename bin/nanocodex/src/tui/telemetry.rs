use std::{sync::Arc, time::Instant};

use nanocodex::{AgentEvent, AgentEventKind};
use tracing::{info, info_span};

use super::app::{App, PaneId};

const TARGET: &str = "nanocodex_stream_timing";

pub(super) struct ReceivedEvent {
    pane: PaneId,
    request_id: Arc<str>,
    seq: u64,
    kind: AgentEventKind,
    payload_bytes: usize,
    received_at: Instant,
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
    pub(super) fn new(main_session_id: Arc<str>) -> Self {
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
    pending: Option<PendingFrame>,
}

struct PendingFrame {
    first_request_id: Arc<str>,
    last_request_id: Arc<str>,
    first_seq: u64,
    last_seq: u64,
    first_received_at: Instant,
    last_received_at: Instant,
    event_count: usize,
    assistant_delta_count: usize,
    payload_bytes: usize,
}

impl StreamTelemetry {
    pub(super) fn event_received(pane: PaneId, event: &AgentEvent) -> Option<ReceivedEvent> {
        if !tracing::enabled!(target: TARGET, tracing::Level::TRACE) {
            return None;
        }

        let received_at = Instant::now();
        let payload_bytes = event.payload.get().len();
        tracing::trace!(
            target: TARGET,
            stage = "tui_event_received",
            request.id = %event.request_id,
            tui.pane = pane_name(pane),
            tui.btw.id = pane_btw_id(pane).unwrap_or_default(),
            event.seq = event.seq,
            event.kind = ?event.kind,
            payload.bytes = payload_bytes,
            "TUI received an agent event"
        );
        Some(ReceivedEvent {
            pane,
            request_id: Arc::clone(&event.request_id),
            seq: event.seq,
            kind: event.kind,
            payload_bytes,
            received_at,
        })
    }

    pub(super) fn event_applied(&mut self, event: ReceivedEvent, schedules_frame: bool) {
        let applied_at = Instant::now();
        tracing::trace!(
            target: TARGET,
            stage = "tui_event_applied",
            request.id = %event.request_id,
            tui.pane = pane_name(event.pane),
            tui.btw.id = pane_btw_id(event.pane).unwrap_or_default(),
            event.seq = event.seq,
            event.kind = ?event.kind,
            schedules_frame,
            event_to_action_ns = elapsed_ns(event.received_at, applied_at),
            "TUI applied an agent event"
        );
        if !schedules_frame {
            return;
        }

        if let Some(pending) = &mut self.pending {
            pending.last_request_id = event.request_id;
            pending.last_seq = event.seq;
            pending.last_received_at = event.received_at;
            pending.event_count = pending.event_count.saturating_add(1);
            pending.assistant_delta_count = pending
                .assistant_delta_count
                .saturating_add(usize::from(event.kind == AgentEventKind::AssistantDelta));
            pending.payload_bytes = pending.payload_bytes.saturating_add(event.payload_bytes);
        } else {
            let request_id = event.request_id;
            self.pending = Some(PendingFrame {
                first_request_id: Arc::clone(&request_id),
                last_request_id: request_id,
                first_seq: event.seq,
                last_seq: event.seq,
                first_received_at: event.received_at,
                last_received_at: event.received_at,
                event_count: 1,
                assistant_delta_count: usize::from(event.kind == AgentEventKind::AssistantDelta),
                payload_bytes: event.payload_bytes,
            });
        }
    }

    pub(super) fn frame_presented(
        &mut self,
        render_started: Instant,
        presented_at: Instant,
        app: &App,
    ) {
        self.frame = self.frame.saturating_add(1);
        let render_ns = elapsed_ns(render_started, presented_at);
        let view = ViewState::from_app(app);
        if let Some(pending) = self.pending.take() {
            tracing::trace!(
                target: TARGET,
                stage = "frame_presented",
                frame = self.frame,
                tui.view = view.view(),
                tui.focus = view.focus(),
                tui.btw.open = view.btw_id.is_some(),
                tui.btw.id = view.btw_id.unwrap_or_default(),
                first.request.id = %pending.first_request_id,
                last.request.id = %pending.last_request_id,
                first.event.seq = pending.first_seq,
                last.event.seq = pending.last_seq,
                stream.event_count = pending.event_count,
                assistant.delta_count = pending.assistant_delta_count,
                payload.bytes = pending.payload_bytes,
                first_event_to_present_ns = elapsed_ns(pending.first_received_at, presented_at),
                last_event_to_present_ns = elapsed_ns(pending.last_received_at, presented_at),
                render_ns,
                "TUI presented a frame"
            );
        } else {
            tracing::trace!(
                target: TARGET,
                stage = "frame_presented",
                frame = self.frame,
                tui.view = view.view(),
                tui.focus = view.focus(),
                tui.btw.open = view.btw_id.is_some(),
                tui.btw.id = view.btw_id.unwrap_or_default(),
                stream.event_count = 0,
                assistant.delta_count = 0,
                payload.bytes = 0,
                render_ns,
                "TUI presented a frame"
            );
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

    use nanocodex::{AgentEventKind, Thinking};

    use crate::tui::app::{App, PaneId};

    use super::{ReceivedEvent, StreamTelemetry, ViewTelemetry};

    #[test]
    fn coalesced_events_are_consumed_by_one_presented_frame() {
        let mut telemetry = StreamTelemetry::default();
        let app = App::new(PathBuf::from("."), Thinking::Medium);
        for seq in 1..=3 {
            telemetry.event_applied(
                ReceivedEvent {
                    pane: PaneId::Main,
                    request_id: Arc::from("request"),
                    seq,
                    kind: AgentEventKind::AssistantDelta,
                    payload_bytes: 5,
                    received_at: Instant::now(),
                },
                true,
            );
        }

        let pending = telemetry.pending.as_ref().unwrap();
        assert_eq!(pending.event_count, 3);
        assert_eq!(pending.assistant_delta_count, 3);
        assert_eq!(pending.payload_bytes, 15);

        let now = Instant::now();
        telemetry.frame_presented(now, now, &app);
        assert!(telemetry.pending.is_none());
    }

    #[test]
    fn view_state_tracks_btw_lifecycle_focus_and_session_mapping() {
        let mut app = App::new(PathBuf::from("."), Thinking::Medium);
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
