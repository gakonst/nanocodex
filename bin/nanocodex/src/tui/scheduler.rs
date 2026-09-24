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
