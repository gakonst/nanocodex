use std::time::{Duration, Instant};

/// Maximum demand-driven redraw rate. Fast terminals can present at 120 Hz,
/// while Ratatui's buffer diff keeps unchanged cells off the wire.
pub(super) const STREAM_FRAME_INTERVAL: Duration = Duration::from_nanos(8_333_334);

#[derive(Debug)]
pub(super) struct RenderScheduler {
    frame_interval: Duration,
    last_presented: Option<Instant>,
    deadline: Option<Instant>,
}

impl RenderScheduler {
    pub(super) fn new(frame_interval: Duration, now: Instant) -> Self {
        Self {
            frame_interval,
            last_presented: None,
            deadline: Some(now),
        }
    }

    pub(super) fn request_streaming(&mut self, now: Instant) {
        if self.deadline.is_some() {
            return;
        }
        self.deadline = Some(
            self.last_presented
                .map_or(now, |presented| presented + self.frame_interval)
                .max(now),
        );
    }

    pub(super) fn request_immediate(&mut self, now: Instant) {
        self.deadline = Some(self.deadline.map_or(now, |deadline| deadline.min(now)));
    }

    pub(super) fn request_input_burst(&mut self, now: Instant) {
        let burst_deadline = now + self.frame_interval;
        self.deadline = Some(
            self.deadline
                .map_or(burst_deadline, |deadline| deadline.min(burst_deadline)),
        );
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) fn is_due(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| deadline <= now)
    }

    pub(super) fn presented(&mut self, now: Instant) {
        self.deadline = None;
        self.last_presented = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{RenderScheduler, STREAM_FRAME_INTERVAL};

    const FRAME: Duration = STREAM_FRAME_INTERVAL;

    #[test]
    fn initial_frame_is_due_immediately() {
        let now = Instant::now();
        let scheduler = RenderScheduler::new(FRAME, now);

        assert!(scheduler.is_due(now));
        assert_eq!(scheduler.deadline(), Some(now));
    }

    #[test]
    fn stream_burst_keeps_one_frame_deadline() {
        let start = Instant::now();
        let mut scheduler = RenderScheduler::new(FRAME, start);
        scheduler.presented(start);

        for offset in 1..8 {
            scheduler.request_streaming(start + Duration::from_millis(offset));
        }

        assert_eq!(scheduler.deadline(), Some(start + FRAME));
        assert!(!scheduler.is_due(start + Duration::from_millis(8)));
        assert!(scheduler.is_due(start + FRAME));
    }

    #[test]
    fn peak_codex_trace_burst_coalesces_to_one_frame() {
        // Sanitized from the retained 2026-07-19 long Codex rollout: the
        // densest 33 ms bucket contained 590 display-affecting records.
        let start = Instant::now();
        let mut scheduler = RenderScheduler::new(FRAME, start);
        scheduler.presented(start);

        for event in 0..590 {
            let offset = Duration::from_micros(event * 50 + 1);
            scheduler.request_streaming(start + offset);
        }

        assert_eq!(scheduler.deadline(), Some(start + FRAME));
    }

    #[test]
    fn input_and_resize_preempt_a_streaming_deadline() {
        let start = Instant::now();
        let mut scheduler = RenderScheduler::new(FRAME, start);
        scheduler.presented(start);
        scheduler.request_streaming(start + Duration::from_millis(1));

        let input_at = start + Duration::from_millis(7);
        scheduler.request_immediate(input_at);

        assert_eq!(scheduler.deadline(), Some(input_at));
        assert!(scheduler.is_due(input_at));
    }

    #[test]
    fn input_burst_gets_one_frame_to_coalesce() {
        let start = Instant::now();
        let mut scheduler = RenderScheduler::new(FRAME, start);
        scheduler.presented(start);

        let first = start + Duration::from_millis(20);
        scheduler.request_input_burst(first);
        scheduler.request_input_burst(first + Duration::from_millis(2));

        assert_eq!(scheduler.deadline(), Some(first + FRAME));
        assert!(!scheduler.is_due(first + Duration::from_millis(8)));
        assert!(scheduler.is_due(first + FRAME));
    }

    #[test]
    fn presentation_clears_dirty_state() {
        let now = Instant::now();
        let mut scheduler = RenderScheduler::new(FRAME, now);

        scheduler.presented(now);

        assert_eq!(scheduler.deadline(), None);
        assert!(!scheduler.is_due(now + FRAME));
    }
}
