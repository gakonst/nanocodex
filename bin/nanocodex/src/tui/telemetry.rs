use std::{sync::Arc, time::Instant};

use nanocodex::{AgentEvent, AgentEventKind};

const TARGET: &str = "nanocodex_stream_timing";

pub(super) struct ReceivedEvent {
    request_id: Arc<str>,
    seq: u64,
    kind: AgentEventKind,
    payload_bytes: usize,
    received_at: Instant,
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
    pub(super) fn event_received(event: &AgentEvent) -> Option<ReceivedEvent> {
        if !tracing::enabled!(target: TARGET, tracing::Level::TRACE) {
            return None;
        }

        let received_at = Instant::now();
        let payload_bytes = event.payload.get().len();
        tracing::trace!(
            target: TARGET,
            stage = "tui_event_received",
            request.id = %event.request_id,
            event.seq = event.seq,
            event.kind = ?event.kind,
            payload.bytes = payload_bytes,
            "TUI received an agent event"
        );
        Some(ReceivedEvent {
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

    pub(super) fn frame_presented(&mut self, render_started: Instant, presented_at: Instant) {
        self.frame = self.frame.saturating_add(1);
        let render_ns = elapsed_ns(render_started, presented_at);
        if let Some(pending) = self.pending.take() {
            tracing::trace!(
                target: TARGET,
                stage = "frame_presented",
                frame = self.frame,
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
                stream.event_count = 0,
                assistant.delta_count = 0,
                payload.bytes = 0,
                render_ns,
                "TUI presented a frame"
            );
        }
    }
}

fn elapsed_ns(start: Instant, end: Instant) -> u64 {
    u64::try_from(end.saturating_duration_since(start).as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Instant};

    use nanocodex::AgentEventKind;

    use super::{ReceivedEvent, StreamTelemetry};

    #[test]
    fn coalesced_events_are_consumed_by_one_presented_frame() {
        let mut telemetry = StreamTelemetry::default();
        for seq in 1..=3 {
            telemetry.event_applied(
                ReceivedEvent {
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
        telemetry.frame_presented(now, now);
        assert!(telemetry.pending.is_none());
    }
}
