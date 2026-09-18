//! Host-owned provider work, scheduled after the current JS checkpoint.
use super::{ProviderControl, ProviderSuspension, provider_control::ProviderLifetime, tasks};
use crate::Result;
use serde_json::Value;
use std::collections::VecDeque;

/// A native result or a nonblocking completion poller, owned by one provider call.
pub struct ProviderResponse(Box<dyn FnMut() -> Option<Result<Value>>>);
impl ProviderResponse {
    pub fn ready(result: Result<Value>) -> Self {
        let mut result = Some(result);
        Self::pending(move || result.take())
    }
    pub fn pending(poll: impl FnMut() -> Option<Result<Value>> + 'static) -> Self {
        Self(Box::new(poll))
    }
}
pub(super) struct Active<R> {
    pub call: Call<R>,
    pub control: ProviderControl,
    pub lifetime: ProviderLifetime,
    pub guard: Option<ProviderSuspension>,
    response: ProviderResponse,
}
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
    active: Vec<Active<R>>,
    ready: VecDeque<(Call<R>, Result<Value>)>,
}
impl<R> Default for Queue<R> {
    fn default() -> Self {
        Self {
            calls: VecDeque::new(),
            active: Vec::new(),
            ready: VecDeque::new(),
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
        if self.calls.len() + self.active.len() + self.ready.len() >= 256 {
            return Err(crate::Error::action(
                "Pending provider request limit exceeded",
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
        });
        Ok(())
    }
    pub fn admit(
        &mut self,
        call: Call<R>,
        control: ProviderControl,
        response: ProviderResponse,
        guard: Option<ProviderSuspension>,
    ) {
        let lifetime = control.lifetime();
        self.active.push(Active {
            call,
            control,
            response,
            lifetime,
            guard,
        });
    }
    pub fn poll(&mut self) -> Option<(Active<R>, Result<Value>)> {
        for index in 0..self.active.len() {
            if let Some(result) = (self.active[index].response.0)() {
                return Some((self.active.remove(index), result));
            }
        }
        None
    }
    // Native completion releases approval ownership before any JS settlement.
    pub fn completed(&mut self, call: Call<R>, result: Result<Value>) {
        self.ready.push_back((call, result));
    }
    pub fn take_ready(&mut self) -> Option<(Call<R>, Result<Value>)> {
        self.ready.pop_front()
    }
    pub fn pop(&mut self) -> Option<Call<R>> {
        self.calls.pop_front()
    }
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty() && self.active.is_empty() && self.ready.is_empty()
    }
    pub fn clear(&mut self) {
        self.calls.clear();
        self.active.clear();
        self.ready.clear();
    }
}

pub(super) fn elapsed_ms(started: std::time::Instant) -> u64 {
    // Original kernel uses Math.round(performance.now() - start).
    ((started.elapsed().as_nanos() + 500_000) / 1_000_000).min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_queue_bounds_queued_and_admitted_calls_together() {
        let mut queue = Queue::default();
        // Context comes from the same host-owned task constructor as production.
        for request in 0..256 {
            queue
                .push(
                    "fixture".into(),
                    "{}".into(),
                    (),
                    tasks::Context::default(),
                    false,
                    request,
                )
                .unwrap();
        }
        assert!(
            queue
                .push(
                    "overflow".into(),
                    "{}".into(),
                    (),
                    tasks::Context::default(),
                    false,
                    256
                )
                .is_err()
        );
        let call = queue.pop().unwrap();
        let control = ProviderControl::new(|_| Ok(()));
        queue.admit(
            call,
            control.clone(),
            ProviderResponse::pending(|| None),
            None,
        );
        assert!(
            queue
                .push(
                    "overflow".into(),
                    "{}".into(),
                    (),
                    tasks::Context::default(),
                    false,
                    256
                )
                .is_err()
        );
        queue.clear();
        assert!(!control.is_active());
        assert!(queue.is_empty());
    }
}
