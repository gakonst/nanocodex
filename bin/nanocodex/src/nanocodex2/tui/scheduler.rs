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
