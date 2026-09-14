//! Host-owned provider work, scheduled after the current JS checkpoint.
use super::tasks;
use crate::Result;
use serde_json::Value;
use std::collections::VecDeque;

pub(super) struct Call<R> {
    pub method: String,
    pub input: Value,
    pub resolver: R,
    pub context: tasks::Context,
    pub suspend: bool,
    pub request: u32,
}
pub(super) struct Queue<R> {
    calls: VecDeque<Call<R>>,
}
impl<R> Default for Queue<R> {
    fn default() -> Self {
        Self {
            calls: VecDeque::new(),
        }
    }
}
impl<R> Queue<R> {
    pub fn push(
        &mut self,
        method: String,
        input: String,
        resolver: R,
        context: tasks::Context,
        suspend: bool,
        request: u32,
    ) -> Result<()> {
        let input = serde_json::from_str(&input)?;
        self.calls.push_back(Call {
            method,
            input,
            resolver,
            context,
            suspend,
            request,
        });
        Ok(())
    }
    pub fn pop(&mut self) -> Option<Call<R>> {
        self.calls.pop_front()
    }
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }
    pub fn clear(&mut self) {
        self.calls.clear();
    }
}

pub(super) fn elapsed_ms(started: std::time::Instant) -> u64 {
    // Original kernel uses Math.round(performance.now() - start).
    ((started.elapsed().as_nanos() + 500_000) / 1_000_000).min(u64::MAX as u128) as u64
}
