//! Bounded host-owned provider work, scheduled after the current JS checkpoint.
use super::tasks;
use crate::{Error, Result};
use serde_json::Value;
use std::collections::VecDeque;

pub(super) const MAX_CALLS: usize = 1024;
const MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) struct Call<R> {
    pub method: String,
    pub input: Value,
    pub resolver: R,
    pub context: tasks::Context,
    pub suspend: bool,
    pub request: u32,
    bytes: usize,
}
pub(super) struct Queue<R> {
    calls: VecDeque<Call<R>>,
    bytes: usize,
}
impl<R> Default for Queue<R> {
    fn default() -> Self {
        Self {
            calls: VecDeque::new(),
            bytes: 0,
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
        let bytes = method.len().saturating_add(input.len());
        if self.calls.len() >= MAX_CALLS || bytes > MAX_BYTES.saturating_sub(self.bytes) {
            return Err(Error::action(
                "Pending host requests exceed the 1024 call / 4 MiB budget",
            ));
        }
        let input = serde_json::from_str(&input)?;
        self.calls.push_back(Call {
            method,
            input,
            resolver,
            context,
            suspend,
            request,
            bytes,
        });
        self.bytes += bytes;
        Ok(())
    }
    pub fn pop(&mut self) -> Option<Call<R>> {
        let call = self.calls.pop_front()?;
        self.bytes -= call.bytes;
        Some(call)
    }
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }
    pub fn clear(&mut self) {
        self.calls.clear();
        self.bytes = 0;
    }
}

pub(super) fn elapsed_ms(started: std::time::Instant) -> u64 {
    // Original kernel uses Math.round(performance.now() - start).
    ((started.elapsed().as_nanos() + 500_000) / 1_000_000).min(u64::MAX as u128) as u64
}
