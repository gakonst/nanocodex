//! Shared ownership for raw CDP streams and internal viewport captures.
//!
//! The browser actor is synchronous today, but ownership is per tab: callers
//! queue before dispatch and guards release on success, errors and unwinding.
use super::Browser;
use crate::{Error, Result};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, LinkedList, VecDeque},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const RETIRED_LIMIT: usize = 32;
const RETIRE_FOR_MS: f64 = 10_000.;

fn wallclock_ms() -> f64 {
    epoch_millis(SystemTime::now())
}

fn epoch_millis(time: SystemTime) -> f64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_millis() as f64,
        // Millisecond timestamps floor relative to the epoch on both sides.
        Err(before) => -(before.duration().as_nanos().div_ceil(1_000_000) as f64),
    }
}

#[derive(Clone, Default)]
pub(super) struct State(Arc<Shared>);
#[derive(Default)]
struct Shared {
    tabs: Mutex<BTreeMap<String, Tab>>,
    ready: Condvar,
}
#[derive(Default)]
struct Tab {
    // Only live operations occupy this FIFO. Linked nodes are freed as each
    // operation leaves, even when raw/retired state keeps the tab alive.
    queue: LinkedList<Arc<OperationId>>,
    raw: bool,
    active: Option<Internal>,
    retired: VecDeque<(f64, f64)>,
}
struct OperationId;

struct Internal {
    id: Arc<OperationId>,
    ids: Vec<f64>,
    pending: Option<Value>,
    armed: bool,
}

/// Owns a place in one tab's queue without holding the state mutex during CDP.
pub(super) struct Operation {
    state: State,
    tab: String,
    id: Arc<OperationId>,
    internal: bool,
}

impl State {
    fn tabs(&self) -> MutexGuard<'_, BTreeMap<String, Tab>> {
        self.0.tabs.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(super) fn acquire(&self, tab: &str) -> Operation {
        let mut tabs = self.tabs();
        let id = Arc::new(OperationId);
        tabs.entry(tab.into())
            .or_default()
            .queue
            .push_back(id.clone());
        while !tabs[tab]
            .queue
            .front()
            .is_some_and(|head| Arc::ptr_eq(head, &id))
        {
            tabs = self.0.ready.wait(tabs).unwrap_or_else(|e| e.into_inner());
        }
        Operation {
            state: self.clone(),
            tab: tab.into(),
            id,
            internal: false,
        }
    }
    /// All events still pass the browser's other observers. This return value
    /// controls only inclusion in its public raw-event ring.
    pub(super) fn observe(&self, tab: &str, top_level: bool, event: &Value) -> bool {
        self.observe_with_clock(tab, top_level, event, wallclock_ms)
    }
    fn observe_with_clock(
        &self,
        tab: &str,
        top_level: bool,
        event: &Value,
        mut clock: impl FnMut() -> f64,
    ) -> bool {
        if !top_level {
            return false;
        }
        let method = event["method"].as_str().unwrap_or("");
        let id = (method == "Page.screencastFrame")
            .then(|| event["params"]["sessionId"].as_f64())
            .flatten();
        // A nonnumeric frame must not prune. A later wallclock rollback can
        // make an unpruned retired entry visible to this filter again.
        let mut tabs = self.tabs();
        let now = id.map(|_| clock());
        let Some(state) = tabs.get_mut(tab) else {
            return false;
        };
        if method == "Page.screencastVisibilityChanged" {
            if let Some(active) = &mut state.active {
                if active.armed && event["params"]["visible"] == false {
                    active.pending = Some(event.clone());
                    active.armed = false;
                }
                return true;
            }
            return false;
        }
        if method != "Page.screencastFrame" {
            return false;
        }
        if let Some(now) = now {
            state.retired.retain(|(_, expiry)| *expiry > now);
        }
        if state.queue.is_empty()
            && !state.raw
            && state.active.is_none()
            && state.retired.is_empty()
        {
            tabs.remove(tab);
            return false;
        }
        if id.is_some_and(|id| state.retired.iter().any(|(old, _)| *old == id)) {
            return true;
        }
        let Some(active) = &mut state.active else {
            return false;
        };
        if let Some(id) = id {
            if !active.ids.contains(&id) {
                active.ids.push(id);
            }
            if active.armed {
                active.pending = Some(event.clone());
                active.armed = false;
            }
        }
        true
    }
    pub(super) fn forget(&self, tab: &str) {
        let mut tabs = self.tabs();
        if let Some(state) = tabs.get_mut(tab) {
            state.raw = false;
            state.retired.clear();
            // An in-flight callback owns its finally block, including retiring
            // the IDs it observed even if detachment happened in that callback.
            if state.queue.is_empty() && state.active.is_none() {
                tabs.remove(tab);
            }
        }
    }
    pub(super) fn disconnect(&self) {
        let mut tabs = self.tabs();
        for state in tabs.values_mut() {
            state.raw = false;
            state.retired.clear();
        }
        tabs.retain(|_, state| !state.queue.is_empty() || state.active.is_some());
    }
}
impl Operation {
    pub(super) fn raw_succeeded(&self, method: &str) {
        let mut tabs = self.state.tabs();
        let state = tabs.get_mut(&self.tab).unwrap();
        match method {
            "Page.startScreencast" => state.raw = true,
            "Page.stopScreencast" => state.raw = false,
            _ => unreachable!("only raw start/stop own a screencast operation"),
        }
    }
    fn begin_internal(mut self) -> Option<Self> {
        {
            let mut tabs = self.state.tabs();
            let state = tabs.get_mut(&self.tab).unwrap();
            if state.raw {
                return None;
            }
            state.active = Some(Internal {
                id: self.id.clone(),
                ids: Vec::new(),
                pending: None,
                armed: true,
            });
        }
        self.internal = true;
        Some(self)
    }
    fn take_event(&self) -> Option<Value> {
        self.state
            .tabs()
            .get_mut(&self.tab)?
            .active
            .as_mut()
            .filter(|active| Arc::ptr_eq(&active.id, &self.id))?
            .pending
            .take()
    }
    fn arm(&self) {
        if let Some(active) = self
            .state
            .tabs()
            .get_mut(&self.tab)
            .and_then(|state| state.active.as_mut())
            .filter(|active| Arc::ptr_eq(&active.id, &self.id))
        {
            active.armed = true;
        }
    }
    fn finish_with_clock(&mut self, mut clock: impl FnMut() -> f64) {
        let mut tabs = self.state.tabs();
        let state = tabs.get_mut(&self.tab).unwrap();
        if self.internal {
            if let Some(active) = state.active.take() {
                assert!(Arc::ptr_eq(&active.id, &self.id));
                if !active.ids.is_empty() {
                    // These are separate wallclock reads in the original.
                    // Empty captures do neither, including no expiry pruning.
                    let prune_now = clock();
                    state.retired.retain(|(_, expiry)| *expiry > prune_now);
                    let expires_at = clock() + RETIRE_FOR_MS;
                    for id in active.ids {
                        if let Some((_, expiry)) =
                            state.retired.iter_mut().find(|(old, _)| *old == id)
                        {
                            *expiry = expires_at;
                        } else {
                            state.retired.push_back((id, expires_at));
                        }
                    }
                    while state.retired.len() > RETIRED_LIMIT {
                        state.retired.pop_front();
                    }
                }
            }
            self.internal = false;
        }
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.finish_with_clock(wallclock_ms);
        let mut tabs = self.state.tabs();
        let state = tabs.get_mut(&self.tab).unwrap();
        let head = state
            .queue
            .pop_front()
            .expect("live operation has a queue entry");
        assert!(Arc::ptr_eq(&head, &self.id));
        if state.queue.is_empty() && !state.raw && state.retired.is_empty() {
            tabs.remove(&self.tab);
        }
        self.state.0.ready.notify_all();
    }
}

impl Browser {
    /// The original viewport path first tries a current screencast frame,
    /// falling back to captureScreenshot on invisibility, failure or timeout.
    /// Keep this replacement's public PNG contract for both capture paths.
    pub(super) fn viewport_screencast(&mut self, tab: &str) -> Option<String> {
        let capture = self.surface.screencast.acquire(tab).begin_internal()?;
        let mut started = false;
        let result = (|| -> Result<Option<String>> {
            self.page(tab)?;
            let deadline = Instant::now() + Duration::from_secs(2);
            let start_timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| Error::action("System clock precedes Unix epoch"))?
                .as_secs_f64();
            // Even a refused start may have partly changed provider state.
            self.tab_attempt(
                tab,
                "Page.startScreencast",
                json!({"everyNthFrame":1,"format":"png","quality":80}),
                &mut started,
            )?;
            loop {
                let event = loop {
                    if let Some(event) = capture.take_event() {
                        break event;
                    }
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    self.poll_events()?;
                    if self.invalidated {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                };
                if event["method"] != "Page.screencastFrame" {
                    return Ok(None);
                }
                let id = event["params"]["sessionId"].clone();
                let current = event["params"]["metadata"]["timestamp"]
                    .as_f64()
                    .is_some_and(|timestamp| timestamp.is_finite() && timestamp >= start_timestamp);
                if !current {
                    capture.arm();
                    self.tab_maintenance(tab, "Page.screencastFrameAck", json!({"sessionId":id}))?;
                    continue;
                }
                let stop = self.tab_maintenance(tab, "Page.stopScreencast", json!({}));
                if stop.is_ok() {
                    started = false;
                }
                // ACK is a finally action even when the stop is refused.
                let ack =
                    self.tab_maintenance(tab, "Page.screencastFrameAck", json!({"sessionId":id}));
                stop?;
                ack?;
                return Ok(event["params"]["data"]
                    .as_str()
                    .filter(|data| !data.is_empty())
                    .map(str::to_owned));
            }
        })();
        if started {
            let _ = self.tab_maintenance(tab, "Page.stopScreencast", json!({}));
        }
        result.ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_screencast_transition_traces_match() {
        let oracle: Value =
            serde_json::from_str(include_str!("../tests/oracles/browser_screencast.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let state = State::default();
            let mut now_ms = 1000;
            let mut captures = BTreeMap::new();
            for (action, expected) in case["actions"]
                .as_array()
                .unwrap()
                .iter()
                .zip(case["rows"].as_array().unwrap())
            {
                let tab = action["tab"].as_str().unwrap_or("1");
                let now = now_ms as f64;
                let mut observed = json!({});
                match action["op"].as_str().unwrap() {
                    "raw" => {
                        let operation = state.acquire(tab);
                        if action["success"] != false {
                            operation.raw_succeeded(action["method"].as_str().unwrap());
                        }
                    }
                    "begin" => {
                        let capture = state.acquire(tab).begin_internal();
                        observed["invoked"] = json!(capture.is_some());
                        if let Some(capture) = capture {
                            captures.insert(tab.to_owned(), capture);
                        }
                    }
                    "end" => {
                        let mut capture = captures.remove(tab).unwrap();
                        capture.finish_with_clock(|| now);
                    }
                    "advance" => now_ms += action["ms"].as_u64().unwrap(),
                    "event" => {
                        let top = action["child"] != true && action["target"] != true;
                        let event = json!({"method":action["method"],"params":action["params"]});
                        observed["hidden"] =
                            json!(state.observe_with_clock(tab, top, &event, || now));
                        let tabs = state.tabs();
                        let active = tabs.get(tab).and_then(|state| state.active.as_ref());
                        observed["current"] = json!(
                            top && active.is_some_and(|active| {
                                event["method"] == "Page.screencastVisibilityChanged"
                                    || event["method"] == "Page.screencastFrame"
                                        && event["params"]["sessionId"]
                                            .as_f64()
                                            .is_some_and(|id| active.ids.contains(&id))
                            })
                        );
                    }
                    op => panic!("unknown oracle operation {op}"),
                }
                let mut tabs = state.tabs();
                let actual = tabs.entry(tab.into()).or_default();
                actual.retired.retain(|(_, expiry)| *expiry > now_ms as f64);
                observed["raw"] = json!(actual.raw);
                observed["active"] = json!(actual.active.is_some());
                observed["ids"] = json!(
                    actual
                        .active
                        .as_ref()
                        .map(|a| a.ids.clone())
                        .unwrap_or_default()
                );
                observed["retired"] = json!(
                    actual
                        .retired
                        .iter()
                        .map(|(id, expiry)| json!([id, expiry]))
                        .collect::<Vec<_>>()
                );
                // JSON numbers with integer vs fractional storage represent
                // the same JavaScript Number; compare via serde conversion.
                fn numbers(value: &Value) -> Value {
                    match value {
                        Value::Number(number) => json!(number.as_f64().unwrap()),
                        Value::Array(values) => {
                            json!(values.iter().map(numbers).collect::<Vec<_>>())
                        }
                        Value::Object(values) => Value::Object(
                            values
                                .iter()
                                .map(|(key, value)| (key.clone(), numbers(value)))
                                .collect(),
                        ),
                        value => value.clone(),
                    }
                }
                assert_eq!(
                    numbers(&observed),
                    numbers(expected),
                    "{}: {action}",
                    case["name"]
                );
            }
        }
    }

    #[test]
    fn screencast_wallclock_conversion_floors_fractional_epoch_milliseconds() {
        assert_eq!(epoch_millis(UNIX_EPOCH), 0.);
        for (nanos, positive, negative) in [
            (1, 0., -1.),
            (500_000, 0., -1.),
            (999_999, 0., -1.),
            (1_000_000, 1., -1.),
            (1_000_001, 1., -2.),
            (1_500_000, 1., -2.),
            (2_000_000, 2., -2.),
        ] {
            let duration = Duration::from_nanos(nanos);
            assert_eq!(epoch_millis(UNIX_EPOCH + duration), positive);
            assert_eq!(epoch_millis(UNIX_EPOCH - duration), negative);
        }
    }

    #[test]
    fn original_retirement_clock_and_nonpruning_partitions_match() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../tests/oracles/browser_screencast_retirement.json"
        ))
        .unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let state = State::default();
            let mut capture = None;
            for (action, expected) in case["actions"]
                .as_array()
                .unwrap()
                .iter()
                .zip(case["rows"].as_array().unwrap())
            {
                let mut times: VecDeque<f64> = action["clocks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|value| value.as_f64().unwrap())
                    .collect();
                let mut reads = Vec::new();
                let mut clock = || {
                    let now = times.pop_front().expect("unexpected wallclock read");
                    reads.push(now);
                    now
                };
                let mut observed = json!({});
                match action["op"].as_str().unwrap() {
                    "begin" => {
                        capture = state.acquire("1").begin_internal();
                        observed["invoked"] = json!(capture.is_some());
                    }
                    "end" => capture.take().unwrap().finish_with_clock(&mut clock),
                    "event" => {
                        observed["hidden"] = json!(state.observe_with_clock(
                            "1",
                            action["child"] != true && action["target"] != true,
                            &json!({"method":action["method"],"params":action["params"]}),
                            &mut clock,
                        ));
                    }
                    "retire" => {
                        let mut owned = state.acquire("1").begin_internal().unwrap();
                        state
                            .tabs()
                            .get_mut("1")
                            .unwrap()
                            .active
                            .as_mut()
                            .unwrap()
                            .ids = action["ids"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|id| id.as_f64().unwrap())
                            .collect();
                        owned.finish_with_clock(&mut clock);
                    }
                    "raw" => {
                        let operation = state.acquire("1");
                        if action["fail"] != true {
                            operation.raw_succeeded(action["method"].as_str().unwrap());
                        }
                    }
                    op => panic!("unknown retirement operation {op}"),
                }
                assert!(
                    times.is_empty(),
                    "{}: unused clock samples: {action}",
                    case["name"]
                );
                let tabs = state.tabs();
                let current = tabs.get("1");
                observed["reads"] = json!(reads);
                observed["raw"] = json!(current.is_some_and(|tab| tab.raw));
                observed["active"] = json!(current.is_some_and(|tab| tab.active.is_some()));
                observed["ids"] = json!(
                    current
                        .and_then(|tab| tab.active.as_ref())
                        .map(|active| active.ids.clone())
                        .unwrap_or_default()
                );
                observed["retired"] = json!(
                    current
                        .map(|tab| tab
                            .retired
                            .iter()
                            .map(|(id, expiry)| json!([id, expiry]))
                            .collect::<Vec<_>>())
                        .unwrap_or_default()
                );
                fn numbers(value: &Value) -> Value {
                    match value {
                        Value::Number(n) => json!(n.as_f64().unwrap()),
                        Value::Array(a) => json!(a.iter().map(numbers).collect::<Vec<_>>()),
                        Value::Object(o) => {
                            Value::Object(o.iter().map(|(k, v)| (k.clone(), numbers(v))).collect())
                        }
                        value => value.clone(),
                    }
                }
                assert_eq!(
                    numbers(&observed),
                    numbers(expected),
                    "{}: {action}",
                    case["name"]
                );
            }
            assert!(capture.is_none());
        }
    }

    #[test]
    fn per_tab_fifo_releases_after_unwinding_and_other_tabs_do_not_wait() {
        use std::{sync::mpsc, thread};
        let state = State::default();
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let owner_state = state.clone();
        let owner = thread::spawn(move || {
            std::panic::catch_unwind(move || {
                let _capture = owner_state.acquire("one").begin_internal().unwrap();
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                panic!("owned callback failure");
            })
            .is_err()
        });
        held_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (order_tx, order_rx) = mpsc::channel();
        let mut waiters = Vec::new();
        for index in 0..5 {
            let state_copy = state.clone();
            let tx = order_tx.clone();
            waiters.push(thread::spawn(move || {
                let _operation = state_copy.acquire("one");
                tx.send(index).unwrap();
            }));
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if state.tabs()["one"].queue.len() == index + 2 {
                    break;
                }
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
        }
        let unrelated = state.acquire("two");
        assert!(order_rx.try_recv().is_err());
        drop(unrelated);
        release_tx.send(()).unwrap();
        assert!(owner.join().unwrap());
        assert_eq!(
            (0..5)
                .map(|_| order_rx.recv_timeout(Duration::from_secs(2)).unwrap())
                .collect::<Vec<_>>(),
            [0, 1, 2, 3, 4]
        );
        for waiter in waiters {
            waiter.join().unwrap();
        }
        assert!(state.tabs().is_empty());
    }

    #[test]
    fn screencast_forget_disconnect_and_unobserved_waiters_preserve_live_fifo() {
        use std::{sync::mpsc, thread};
        let state = State::default();
        let current = state.acquire("one").begin_internal().unwrap();
        let (ran, received) = mpsc::channel();
        let mut waiters = Vec::new();
        for index in 0..2 {
            let shared = state.clone();
            let ran = ran.clone();
            waiters.push(thread::spawn(move || {
                let _operation = shared.acquire("one");
                ran.send(index).unwrap();
            }));
            let deadline = Instant::now() + Duration::from_secs(2);
            while state.tabs()["one"].queue.len() != index + 2 {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
        }
        // Losing the caller's thread handle does not cancel a queued operation.
        // The original unresolved promise chain likewise owns its continuation.
        drop(waiters.remove(0));
        state.forget("one");
        state.disconnect();
        assert_eq!(state.tabs()["one"].queue.len(), 3);
        assert!(state.tabs()["one"].active.is_some());
        drop(state.acquire("two"));
        assert!(received.try_recv().is_err());
        drop(current);
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        waiters.pop().unwrap().join().unwrap();
        assert!(state.tabs().is_empty());
    }

    #[test]
    fn screencast_queue_ids_are_live_owned_and_completed_nodes_are_reclaimed() {
        let state = State::default();
        let first = state.acquire("one");
        first.raw_succeeded("Page.startScreencast");
        let retained_old_id = first.id.clone();
        let old_weak = Arc::downgrade(&retained_old_id);
        drop(first);
        for _ in 0..2048 {
            let operation = state.acquire("one");
            assert!(!Arc::ptr_eq(&retained_old_id, &operation.id));
            let weak = Arc::downgrade(&operation.id);
            assert_eq!(state.tabs()["one"].queue.len(), 1);
            drop(operation);
            assert!(
                weak.upgrade().is_none(),
                "completed queue nodes retain no ID"
            );
            assert!(state.tabs()["one"].queue.is_empty());
        }
        drop(retained_old_id);
        assert!(old_weak.upgrade().is_none());
        let stop = state.acquire("one");
        stop.raw_succeeded("Page.stopScreencast");
        drop(stop);
        assert!(state.tabs().is_empty());
    }

    #[test]
    fn internal_listener_arms_before_dispatch_and_rearms_only_for_the_next_wait() {
        let state = State::default();
        let capture = state.acquire("one").begin_internal().unwrap();
        assert!(state.observe(
            "one",
            true,
            &json!({"method":"Page.screencastFrame","params":{"sessionId":1}})
        ));
        assert!(state.observe(
            "one",
            true,
            &json!({"method":"Page.screencastFrame","params":{"sessionId":2}})
        ));
        assert_eq!(capture.take_event().unwrap()["params"]["sessionId"], 1);
        assert!(capture.take_event().is_none());
        capture.arm();
        state.observe(
            "one",
            true,
            &json!({"method":"Page.screencastVisibilityChanged","params":{"visible":true}}),
        );
        assert!(capture.take_event().is_none());
        state.observe(
            "one",
            true,
            &json!({"method":"Page.screencastVisibilityChanged","params":{"visible":false}}),
        );
        assert_eq!(capture.take_event().unwrap()["params"]["visible"], false);
        drop(capture);
        assert_eq!(
            state.tabs()["one"]
                .retired
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            [1., 2.]
        );
    }
}

#[cfg(test)]
#[path = "browser_screencast_queue_tests.rs"]
mod queue_tests;
