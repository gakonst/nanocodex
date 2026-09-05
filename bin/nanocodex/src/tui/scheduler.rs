use std::time::{Duration, Instant};

pub(super) const ANIMATION_TICK_INTERVAL: Duration = Duration::from_millis(80);

/// Maximum demand-driven redraw rate. Thirty frames per second keeps streamed
/// terminal text responsive while bounding repeated full-frame layout work.
/// Input and resize redraws bypass this limit.
pub(super) const STREAM_FRAME_INTERVAL: Duration = Duration::from_nanos(33_333_334);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum RenderScope {
    Animation,
    Full,
}

#[derive(Debug)]
pub(super) struct RenderScheduler {
    frame_interval: Duration,
    last_presented: Option<Instant>,
    deadline: Option<Instant>,
    scope: Option<RenderScope>,
}

impl RenderScheduler {
    pub(super) const fn new(frame_interval: Duration, now: Instant) -> Self {
        Self {
            frame_interval,
            last_presented: None,
            deadline: Some(now),
            scope: Some(RenderScope::Full),
        }
    }

    pub(super) fn request_streaming(&mut self, now: Instant) {
        self.request(now, RenderScope::Full);
    }

    pub(super) fn request_animation(&mut self, now: Instant) {
        self.request(now, RenderScope::Animation);
    }

    fn request(&mut self, now: Instant, scope: RenderScope) {
        self.scope = Some(self.scope.map_or(scope, |pending| pending.max(scope)));
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
        self.scope = Some(RenderScope::Full);
        self.deadline = Some(self.deadline.map_or(now, |deadline| deadline.min(now)));
    }

    pub(super) fn request_input_burst(&mut self, now: Instant) {
        self.scope = Some(RenderScope::Full);
        let burst_deadline = now + self.frame_interval;
        self.deadline = Some(
            self.deadline
                .map_or(burst_deadline, |deadline| deadline.min(burst_deadline)),
        );
    }

    pub(super) const fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) const fn scope(&self) -> Option<RenderScope> {
        self.scope
    }

    pub(super) fn is_due(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| deadline <= now)
    }

    pub(super) const fn presented(&mut self, now: Instant) {
        self.deadline = None;
        self.scope = None;
        self.last_presented = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{RenderScheduler, RenderScope, STREAM_FRAME_INTERVAL};

    const FRAME: Duration = STREAM_FRAME_INTERVAL;

    #[test]
    fn streaming_frame_budget_is_thirty_per_second() {
        assert_eq!(
            Duration::from_secs(1).as_nanos().div_ceil(FRAME.as_nanos()),
            30
        );
    }

    #[test]
    fn animation_tick_budget_is_thirteen_per_second() {
        assert_eq!(
            Duration::from_secs(1)
                .as_nanos()
                .div_ceil(super::ANIMATION_TICK_INTERVAL.as_nanos()),
            13
        );
    }

    #[test]
    fn initial_frame_is_due_immediately() {
        let now = Instant::now();
        let scheduler = RenderScheduler::new(FRAME, now);

        assert!(scheduler.is_due(now));
        assert_eq!(scheduler.deadline(), Some(now));
        assert_eq!(scheduler.scope(), Some(RenderScope::Full));
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
        assert_eq!(scheduler.scope(), Some(RenderScope::Full));
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
        assert_eq!(scheduler.scope(), None);
        assert!(!scheduler.is_due(now + FRAME));
    }

    #[test]
    fn animation_redraw_stays_scoped_until_full_content_changes() {
        let start = Instant::now();
        let mut scheduler = RenderScheduler::new(FRAME, start);
        scheduler.presented(start);

        scheduler.request_animation(start + Duration::from_millis(1));
        assert_eq!(scheduler.scope(), Some(RenderScope::Animation));

        scheduler.request_streaming(start + Duration::from_millis(2));
        assert_eq!(scheduler.scope(), Some(RenderScope::Full));

        scheduler.request_animation(start + Duration::from_millis(3));
        assert_eq!(scheduler.scope(), Some(RenderScope::Full));
    }
}
