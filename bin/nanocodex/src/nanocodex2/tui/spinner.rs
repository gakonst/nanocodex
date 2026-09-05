// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Demand-driven spinner shared by streaming components.

use std::time::{Duration, Instant};

pub(crate) const SPINNER_INTERVAL: Duration = Duration::from_millis(80);
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy, Debug)]
pub(crate) struct Spinner {
    started_at: Instant,
    next_frame: Instant,
    frame: usize,
}

impl Spinner {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            started_at: now,
            next_frame: now + SPINNER_INTERVAL,
            frame: 0,
        }
    }

    pub(crate) fn advance(&mut self, now: Instant) -> bool {
        if now < self.next_frame {
            return false;
        }

        let frame = frame_index(self.started_at, now);
        self.next_frame = now + SPINNER_INTERVAL;
        if frame == self.frame {
            return false;
        }
        self.frame = frame;
        true
    }

    pub(crate) const fn symbol(self) -> &'static str {
        FRAMES[self.frame]
    }

    pub(crate) fn deadline(self) -> Instant {
        self.next_frame
    }
}

fn frame_index(started_at: Instant, now: Instant) -> usize {
    let ticks =
        now.saturating_duration_since(started_at).as_millis() / SPINNER_INTERVAL.as_millis();
    usize::try_from(ticks).unwrap_or(usize::MAX) % FRAMES.len()
}

#[cfg(test)]
mod tests {
    use super::{SPINNER_INTERVAL, Spinner};
    use std::time::Instant;

    #[test]
    fn spinner_advances_only_at_demand_driven_deadlines() {
        let started_at = Instant::now();
        let mut spinner = Spinner::new(started_at);

        assert_eq!(spinner.symbol(), "⠋");
        assert!(!spinner.advance(started_at + SPINNER_INTERVAL / 2));
        assert!(spinner.advance(started_at + SPINNER_INTERVAL));
        assert_eq!(spinner.symbol(), "⠙");
        assert_eq!(spinner.deadline(), started_at + SPINNER_INTERVAL * 2);
    }

    #[test]
    fn delayed_spinner_frame_schedules_its_next_deadline_in_the_future() {
        let started_at = Instant::now();
        let mut spinner = Spinner::new(started_at);
        let delayed = started_at + SPINNER_INTERVAL * 25;

        assert!(spinner.advance(delayed));

        assert!(spinner.deadline() > delayed);
    }
}
