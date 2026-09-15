//! Immediate batches keep callbacks registered during a check phase for its next turn.
use std::{collections::VecDeque, time::Instant};
#[derive(Default)]
pub(super) struct Ready {
    batch: VecDeque<u32>,
    after_batch: bool,
}
impl Ready {
    pub fn next(&mut self, timers: impl Iterator<Item = (u32, Instant, bool)>) -> Option<u32> {
        let timers = timers.collect::<Vec<_>>();
        while let Some(id) = self.batch.pop_front() {
            if self.batch.is_empty() {
                self.after_batch = true;
            }
            if timers.iter().any(|entry| entry.0 == id) {
                return Some(id);
            }
        }
        let now = Instant::now();
        if self.after_batch {
            if let Some((id, _, _)) = timers
                .iter()
                .filter(|(_, when, immediate)| !*immediate && *when <= now)
                .min_by_key(|(id, when, _)| (*when, *id))
            {
                return Some(*id);
            }
            self.after_batch = false;
        }
        let (id, _, immediate) = timers
            .iter()
            .filter(|(_, when, _)| *when <= now)
            .min_by_key(|(id, when, _)| (*when, *id))?;
        if !immediate {
            return Some(*id);
        }
        self.batch = timers
            .iter()
            .filter(|(_, _, immediate)| *immediate)
            .map(|(id, _, _)| *id)
            .collect();
        let id = self.batch.pop_front();
        if self.batch.is_empty() {
            self.after_batch = true;
        }
        id
    }
}
