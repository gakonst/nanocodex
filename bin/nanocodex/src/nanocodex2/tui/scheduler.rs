// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Demand-driven frame scheduling.

use std::time::{Duration, Instant};

/// Maximum frame rate for streaming agent updates.
pub(crate) const STREAM_FRAME_INTERVAL: Duration = Duration::from_nanos(8_333_334);

#[derive(Debug)]
pub(crate) struct RenderScheduler {
    frame_interval: Duration,
    last_presented: Option<Instant>,
    deadline: Option<Instant>,
}

impl RenderScheduler {
    pub(crate) fn new(frame_interval: Duration, now: Instant) -> Self {
        Self {
            frame_interval,
            last_presented: None,
            deadline: Some(now),
        }
    }

    pub(crate) fn request_streaming(&mut self, now: Instant) {
        if self.deadline.is_some() {
            return;
        }

        let next_frame = self
            .last_presented
            .map_or(now, |presented| presented + self.frame_interval);
        self.deadline = Some(next_frame.max(now));
    }

    pub(crate) fn request_immediate(&mut self, now: Instant) {
        let deadline = self.deadline.map_or(now, |deadline| deadline.min(now));
        self.deadline = Some(deadline);
    }

    pub(crate) const fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(crate) fn is_due(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| deadline <= now)
    }

    pub(crate) fn presented(&mut self, now: Instant) {
        self.deadline = None;
        self.last_presented = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::{RenderScheduler, STREAM_FRAME_INTERVAL};
    use std::time::{Duration, Instant};

    #[test]
    fn streaming_updates_coalesce_at_120_hz() {
        let start = Instant::now();
        let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, start);
        scheduler.presented(start);

        for offset in 1..8 {
            scheduler.request_streaming(start + Duration::from_millis(offset));
        }

        assert_eq!(scheduler.deadline(), Some(start + STREAM_FRAME_INTERVAL));
        assert!(!scheduler.is_due(start + Duration::from_millis(8)));
        assert!(scheduler.is_due(start + STREAM_FRAME_INTERVAL));
    }

    #[test]
    fn input_preempts_a_streaming_deadline() {
        let start = Instant::now();
        let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, start);
        scheduler.presented(start);
        scheduler.request_streaming(start + Duration::from_millis(1));

        let input_at = start + Duration::from_millis(7);
        scheduler.request_immediate(input_at);

        assert_eq!(scheduler.deadline(), Some(input_at));
        assert!(scheduler.is_due(input_at));
    }

    #[test]
    fn idle_scheduler_has_no_deadline() {
        let now = Instant::now();
        let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, now);

        scheduler.presented(now);

        assert_eq!(scheduler.deadline(), None);
        assert!(!scheduler.is_due(now + Duration::from_secs(1)));
    }
}
