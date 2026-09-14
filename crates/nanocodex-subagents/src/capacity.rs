// Derived from clabby/tact@1d9ccaefd1d8613dab020812af04a91cd9b4c52c (Apache-2.0).
// Modified for Nanocodex's reusable native/WASM extension runtime.

use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Clone)]
pub(super) struct Capacity {
    state: Arc<Mutex<CapacityState>>,
    revision: watch::Sender<u64>,
}

struct CapacityState {
    active: usize,
    limit: usize,
}

pub(super) struct TurnCapacity {
    state: Arc<Mutex<CapacityState>>,
    revision: watch::Sender<u64>,
}

impl Capacity {
    pub(super) fn new(limit: usize) -> Self {
        let (revision, _) = watch::channel(0);
        Self {
            state: Arc::new(Mutex::new(CapacityState { active: 0, limit })),
            revision,
        }
    }

    pub(super) fn reserve(&self) -> std::io::Result<TurnCapacity> {
        let mut state = self
            .state
            .lock()
            .expect("subagent capacity lock should not be poisoned");
        if state.limit != usize::MAX && state.active >= state.limit {
            return Err(std::io::Error::other(format!(
                "sub-agent concurrency limit of {} has been reached; try delegation again later",
                state.limit
            )));
        }
        state.active += 1;
        drop(state);

        Ok(TurnCapacity {
            state: Arc::clone(&self.state),
            revision: self.revision.clone(),
        })
    }

    pub(super) fn reserve_many(&self, count: usize) -> std::io::Result<Vec<TurnCapacity>> {
        let mut state = self
            .state
            .lock()
            .expect("subagent capacity lock should not be poisoned");
        if state.limit != usize::MAX && count > state.limit.saturating_sub(state.active) {
            return Err(std::io::Error::other(format!(
                "sub-agent concurrency limit of {} has been reached; try delegation again later",
                state.limit
            )));
        }
        state.active += count;
        drop(state);

        Ok((0..count)
            .map(|_| TurnCapacity {
                state: Arc::clone(&self.state),
                revision: self.revision.clone(),
            })
            .collect())
    }

    pub(super) fn set_limit(&self, limit: usize) {
        self.state
            .lock()
            .expect("subagent capacity lock should not be poisoned")
            .limit = limit;
        self.changed();
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
    }

    fn changed(&self) {
        self.revision.send_modify(|revision| {
            *revision = revision.wrapping_add(1);
        });
    }
}

impl Drop for TurnCapacity {
    fn drop(&mut self) {
        self.state
            .lock()
            .expect("subagent capacity lock should not be poisoned")
            .active -= 1;
        self.revision.send_modify(|revision| {
            *revision = revision.wrapping_add(1);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::Capacity;

    #[test]
    fn default_concurrency_is_unlimited_and_can_be_explicitly_limited() {
        let capacity = Capacity::new(crate::DEFAULT_MAX_SUBAGENTS);
        assert_eq!(crate::DEFAULT_MAX_SUBAGENTS, usize::MAX);
        let batch = capacity.reserve_many(128).unwrap();
        let extra = capacity.reserve().unwrap();
        capacity.set_limit(2);
        assert!(capacity.reserve().is_err());
        drop(batch);
        let second = capacity.reserve().unwrap();
        assert!(capacity.reserve().is_err());
        drop((extra, second));
        capacity.set_limit(crate::DEFAULT_MAX_SUBAGENTS);
        assert_eq!(capacity.reserve_many(256).unwrap().len(), 256);
    }

    #[test]
    fn capacity_is_released_for_later_delegation() {
        let capacity = Capacity::new(1);
        let reservation = capacity.reserve().unwrap();

        let error = capacity.reserve().err().unwrap();
        assert_eq!(
            error.to_string(),
            "sub-agent concurrency limit of 1 has been reached; try delegation again later"
        );

        drop(reservation);
        assert!(capacity.reserve().is_ok());
    }

    #[test]
    fn limit_can_change_while_turns_are_active() {
        let capacity = Capacity::new(1);
        let _first = capacity.reserve().unwrap();

        capacity.set_limit(2);
        let _second = capacity.reserve().unwrap();
        capacity.set_limit(1);

        assert!(capacity.reserve().is_err());
    }
}
