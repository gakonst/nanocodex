//! Parent-owned kill switch independent of the runtime protocol's blocking writers.
use crate::{Error, Result};
use std::{
    process::Child,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const GRACE: Duration = Duration::from_millis(250);
const HEARTBEAT_LIMIT: Duration = Duration::from_secs(2);
struct Clock {
    stop: bool,
    active: bool,
    ticket: Option<u64>,
    started: bool,
    timeout: Duration,
    deadline: Instant,
    heartbeat: Instant,
    suspended: u32,
    suspended_at: Option<Instant>,
    cancelled_at: Option<Instant>,
}
struct State {
    clock: Mutex<Clock>,
    wake: Condvar,
    reason: AtomicU8,
    cancel: Arc<AtomicBool>,
}
pub struct Watchdog {
    state: Arc<State>,
    thread: Option<JoinHandle<()>>,
}
impl Watchdog {
    pub fn new(child: Arc<Mutex<Child>>, cancel: Arc<AtomicBool>) -> Self {
        let state = Arc::new(State {
            clock: Mutex::new(Clock {
                stop: false,
                active: false,
                ticket: None,
                started: false,
                timeout: Duration::ZERO,
                deadline: Instant::now(),
                heartbeat: Instant::now(),
                suspended: 0,
                suspended_at: None,
                cancelled_at: None,
            }),
            wake: Condvar::new(),
            reason: AtomicU8::new(0),
            cancel: cancel.clone(),
        });
        let shared = state.clone();
        let thread = thread::spawn(move || {
            let mut clock = shared.clock.lock().unwrap();
            loop {
                if clock.stop {
                    break;
                }
                if !clock.active {
                    clock = shared.wake.wait(clock).unwrap();
                    continue;
                }
                let now = Instant::now();
                let reason = if cancel.load(Ordering::Acquire) {
                    let start = *clock.cancelled_at.get_or_insert(now);
                    if now.duration_since(start) >= GRACE {
                        1
                    } else {
                        0
                    }
                } else if clock.ticket.is_some()
                    && now.duration_since(clock.heartbeat) >= HEARTBEAT_LIMIT
                {
                    3
                } else if clock.suspended == 0
                    && now >= clock.deadline
                    && now.duration_since(clock.deadline) >= GRACE
                {
                    2
                } else {
                    0
                };
                if reason != 0 {
                    shared.reason.store(reason, Ordering::Release);
                    clock.active = false;
                    drop(clock);
                    let _ = child.lock().unwrap().kill();
                    clock = shared.clock.lock().unwrap();
                    continue;
                }
                clock = shared
                    .wake
                    .wait_timeout(clock, Duration::from_millis(5))
                    .unwrap()
                    .0;
            }
        });
        Self {
            state,
            thread: Some(thread),
        }
    }
    pub fn arm(&self, ticket: Option<u64>, timeout: Duration) -> Result<()> {
        let now = Instant::now();
        let mut clock = self.state.clock.lock().unwrap();
        let deadline = now
            .checked_add(if ticket.is_some() {
                Duration::from_secs(10)
            } else {
                timeout
            })
            .ok_or_else(|| Error::invalid("Runtime watchdog clock overflow"))?;
        *clock = Clock {
            stop: false,
            active: true,
            ticket,
            started: false,
            timeout,
            deadline,
            heartbeat: now,
            suspended: 0,
            suspended_at: None,
            cancelled_at: None,
        };
        self.state.reason.store(0, Ordering::Release);
        self.state.wake.notify_all();
        Ok(())
    }
    pub fn idle(&self) {
        let mut clock = self.state.clock.lock().unwrap();
        clock.active = false;
        self.state.wake.notify_all();
    }
    pub fn reason(&self) -> u8 {
        self.state.reason.load(Ordering::Acquire)
    }
    pub fn stop(&mut self) {
        self.state.clock.lock().unwrap().stop = true;
        self.state.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
    pub fn observer(&self) -> Observer {
        Observer(self.state.clone())
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop();
    }
}
#[derive(Clone)]
pub struct Observer(Arc<State>);
impl Observer {
    /// Read the current parent-owned execution clock. This is intentionally
    /// stricter than the delayed kill reason and never changes suspension or
    /// grants the watchdog's grace interval to a new provider action.
    pub fn validate_execution(&self, ticket: u64) -> Result<()> {
        self.validate_execution_with_clock(ticket, Instant::now)
    }
    fn validate_execution_with_clock(
        &self,
        ticket: u64,
        read_now: impl FnOnce() -> Instant,
    ) -> Result<()> {
        let ended = || Error::new(-32800, "Runtime execution is no longer valid");
        if self.0.cancel.load(Ordering::Acquire) || self.0.reason.load(Ordering::Acquire) != 0 {
            return Err(ended());
        }
        {
            let clock = self.0.clock.lock().unwrap();
            // Sample after acquiring the clock, so lock contention cannot make
            // a pre-lock timestamp admit already expired execution.
            let now = read_now();
            if clock.stop
                || !clock.active
                || !clock.started
                || clock.ticket != Some(ticket)
                || clock.cancelled_at.is_some()
                || clock.suspended != 0
                || now >= clock.deadline
                || now.saturating_duration_since(clock.heartbeat) >= HEARTBEAT_LIMIT
            {
                return Err(ended());
            }
        }
        if self.0.cancel.load(Ordering::Acquire) || self.0.reason.load(Ordering::Acquire) != 0 {
            return Err(ended());
        }
        Ok(())
    }
    #[cfg(test)]
    fn validate_execution_at(&self, ticket: u64, now: Instant) -> Result<()> {
        self.validate_execution_with_clock(ticket, || now)
    }
    pub fn started(&self, ticket: u64) -> Result<()> {
        let mut clock = self.0.clock.lock().unwrap();
        if !clock.active || clock.ticket != Some(ticket) || clock.started {
            return Err(Error::invalid("Unexpected runtime start acknowledgement"));
        }
        clock.deadline = Instant::now()
            .checked_add(clock.timeout)
            .ok_or_else(|| Error::invalid("Runtime deadline overflow"))?;
        clock.started = true;
        clock.heartbeat = Instant::now();
        Ok(())
    }
    pub fn heartbeat(&self) {
        self.0.clock.lock().unwrap().heartbeat = Instant::now();
    }
    pub fn suspend(&self, start: bool) -> Result<()> {
        let mut clock = self.0.clock.lock().unwrap();
        if !clock.active || !clock.started {
            return Err(Error::invalid("Timeout suspension outside active runtime"));
        }
        if start {
            if clock.suspended == 0 {
                clock.suspended_at = Some(Instant::now());
            }
            clock.suspended = clock
                .suspended
                .checked_add(1)
                .ok_or_else(|| Error::invalid("Timeout suspension depth overflow"))?;
        } else if clock.suspended > 0 {
            clock.suspended -= 1;
            if clock.suspended == 0 {
                let elapsed = clock.suspended_at.take().unwrap().elapsed();
                clock.deadline = clock
                    .deadline
                    .checked_add(elapsed)
                    .ok_or_else(|| Error::invalid("Suspended runtime deadline overflow"))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod execution_validity_tests {
    use super::*;

    fn observer(now: Instant) -> Observer {
        Observer(Arc::new(State {
            clock: Mutex::new(Clock {
                stop: false,
                active: true,
                ticket: Some(7),
                started: true,
                timeout: Duration::from_secs(1),
                deadline: now + Duration::from_secs(1),
                heartbeat: now,
                suspended: 0,
                suspended_at: None,
                cancelled_at: None,
            }),
            wake: Condvar::new(),
            reason: AtomicU8::new(0),
            cancel: Arc::new(AtomicBool::new(false)),
        }))
    }

    #[test]
    fn native_watchdog_execution_requires_started_matching_live_ticket() {
        let now = Instant::now();
        let current = observer(now);
        current.validate_execution_at(7, now).unwrap();
        assert!(current.validate_execution_at(8, now).is_err());
        let mutations: [fn(&mut Clock); 6] = [
            |clock| clock.stop = true,
            |clock| clock.active = false,
            |clock| clock.started = false,
            |clock| clock.ticket = None,
            |clock| clock.ticket = Some(8),
            |clock| clock.cancelled_at = Some(clock.heartbeat),
        ];
        for mutate in mutations {
            let current = observer(now);
            mutate(&mut current.0.clock.lock().unwrap());
            assert!(current.validate_execution_at(7, now).is_err());
        }
        let current = observer(now);
        current.0.cancel.store(true, Ordering::Release);
        assert!(current.validate_execution_at(7, now).is_err());
        for reason in [1, 2, 3] {
            let current = observer(now);
            current.0.reason.store(reason, Ordering::Release);
            assert!(current.validate_execution_at(7, now).is_err());
        }
    }

    #[test]
    fn native_watchdog_execution_refuses_deadline_without_kill_grace_or_mutation() {
        let now = Instant::now();
        let current = observer(now);
        let deadline = current.0.clock.lock().unwrap().deadline;
        current
            .validate_execution_at(7, deadline - Duration::from_nanos(1))
            .unwrap();
        assert!(current.validate_execution_at(7, deadline).is_err());
        assert!(
            current
                .validate_execution_at(7, deadline + GRACE / 2)
                .is_err()
        );
        assert_eq!(current.0.reason.load(Ordering::Acquire), 0);
        assert!(!current.0.cancel.load(Ordering::Acquire));
        let clock = current.0.clock.lock().unwrap();
        assert_eq!(clock.deadline, deadline);
        assert_eq!(clock.heartbeat, now);
        assert_eq!(clock.suspended, 0);
        assert!(clock.suspended_at.is_none());
        assert!(clock.cancelled_at.is_none());
        assert!(clock.active);
    }

    #[test]
    fn native_watchdog_execution_reads_current_deadline_and_rejects_suspension() {
        let now = Instant::now();
        let current = observer(now);
        let probe = now + Duration::from_millis(20);
        current.validate_execution_at(7, probe).unwrap();
        current.0.clock.lock().unwrap().deadline = now + Duration::from_millis(10);
        assert!(current.validate_execution_at(7, probe).is_err());
        let updated = now + Duration::from_millis(30);
        current.0.clock.lock().unwrap().deadline = updated;
        current.validate_execution_at(7, probe).unwrap();
        {
            let mut clock = current.0.clock.lock().unwrap();
            clock.suspended = 1;
            clock.suspended_at = Some(now);
        }
        assert!(current.validate_execution_at(7, probe).is_err());
        let clock = current.0.clock.lock().unwrap();
        assert_eq!(clock.deadline, updated);
        assert_eq!(clock.suspended, 1);
        assert_eq!(clock.suspended_at, Some(now));
    }

    #[test]
    fn native_watchdog_execution_checks_heartbeat_before_delayed_kill_reason() {
        let now = Instant::now();
        let current = observer(now);
        current.0.clock.lock().unwrap().deadline = now + Duration::from_secs(10);
        current
            .validate_execution_at(7, now + HEARTBEAT_LIMIT - Duration::from_nanos(1))
            .unwrap();
        assert!(
            current
                .validate_execution_at(7, now + HEARTBEAT_LIMIT)
                .is_err()
        );
        assert_eq!(current.0.reason.load(Ordering::Acquire), 0);
        assert_eq!(current.0.clock.lock().unwrap().heartbeat, now);
    }
}
