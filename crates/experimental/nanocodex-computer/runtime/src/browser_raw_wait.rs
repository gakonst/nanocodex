//! Native raw-event wait registry; browser host admission owns its provenance.
//! Neither request JSON nor a retained ProviderControl establishes this owner.
use super::{Query, Selector};
use crate::{Error, Result, browser::chooser::Owner};
use serde_json::Value;
use std::{collections::BTreeMap, time::Instant};
const MAX_WAITS: usize = 128;
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(in crate::browser) struct Identity {
    pub owner: Owner,
    pub tab: String,
    pub generation: u64,
}
pub(in crate::browser) enum Started {
    Ready(Value),
    Pending(String),
}
pub(in crate::browser) enum Polled {
    Pending,
    Ready(Result<Value>),
}
#[derive(Clone)]
pub(in crate::browser) struct Provenance {
    pub policy: crate::security::RawWaitPolicy,
    pub guardian: Option<String>,
    pub connection: String,
    pub session: String,
}
impl Provenance {
    fn bytes(&self) -> usize {
        self.guardian
            .as_ref()
            .map_or(0, String::len)
            .saturating_add(self.connection.len())
            .saturating_add(self.session.len())
    }
}
#[derive(Clone)]
pub(in crate::browser) struct Snapshot {
    pub identity: Identity,
    pub provenance: Provenance,
    pub terminal: bool,
}
struct Wait {
    identity: Identity,
    provenance: Option<Provenance>,
    after: f64,
    query: Query,
    deadline: Instant,
    terminal: Option<Result<Value>>,
    bytes: usize,
}
#[derive(Default)]
pub(in crate::browser) struct State {
    waits: BTreeMap<String, Wait>,
    bytes: usize,
}
fn satisfied(value: &Value) -> bool {
    value["truncated"] == true || value["events"].as_array().is_some_and(|v| !v.is_empty())
}
fn query_bytes(identity: &Identity, query: &Query) -> usize {
    identity
        .owner
        .scope
        .len()
        .saturating_add(identity.tab.len())
        .saturating_add(256)
        .saturating_add(query.methods.as_ref().map_or(0, |methods| {
            methods
                .iter()
                .fold(0usize, |n, m| n.saturating_add(m.len()).saturating_add(32))
        }))
        .saturating_add(match &query.target {
            Some(Selector::SessionId(value) | Selector::TargetId(value)) => value.len(),
            _ => 0,
        })
}

impl State {
    /// The caller computes the original before/after attachment cursor and
    /// supplies a native clock deadline only for its explicitly supported range.
    /// A read under registration closes the original listener-installation gap.
    #[cfg(test)]
    pub fn start(
        &mut self,
        identity: Identity,
        after: f64,
        query: Query,
        deadline: Option<Instant>,
        read: impl FnOnce(&str, f64, &Query) -> Value,
    ) -> Result<Started> {
        self.start_inner(identity, None, after, query, deadline, read)
    }
    pub fn start_native(
        &mut self,
        identity: Identity,
        provenance: Provenance,
        after: f64,
        query: Query,
        deadline: Instant,
        read: impl FnOnce(&str, f64, &Query) -> Value,
    ) -> Result<Started> {
        self.start_inner(
            identity,
            Some(provenance),
            after,
            query,
            Some(deadline),
            read,
        )
    }
    fn start_inner(
        &mut self,
        identity: Identity,
        provenance: Option<Provenance>,
        after: f64,
        query: Query,
        deadline: Option<Instant>,
        read: impl FnOnce(&str, f64, &Query) -> Value,
    ) -> Result<Started> {
        let value = read(&identity.tab, after, &query);
        let Some(deadline) = deadline.filter(|_| !satisfied(&value)) else {
            return Ok(Started::Ready(value));
        };
        let bytes = query_bytes(&identity, &query)
            .saturating_add(provenance.as_ref().map_or(0, Provenance::bytes));
        if self.waits.len() >= MAX_WAITS || self.bytes.saturating_add(bytes) > MAX_BYTES {
            return Err(Error::action("Raw event wait capacity exceeded"));
        }
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce)
            .map_err(|_| Error::action("Cannot generate a raw event wait identifier"))?;
        let id = format!(
            "raw-events-{}",
            nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        if self.waits.contains_key(&id) {
            return Err(Error::action("Raw event wait identifier collision"));
        }
        self.bytes += bytes;
        self.waits.insert(
            id.clone(),
            Wait {
                identity,
                provenance,
                after,
                query,
                deadline,
                terminal: None,
                bytes,
            },
        );
        Ok(Started::Pending(id))
    }
    fn finish(&mut self, id: &str, value: Result<Value>) {
        let Some(wait) = self.waits.get_mut(id).filter(|w| w.terminal.is_none()) else {
            return;
        };
        let fixed = query_bytes(&wait.identity, &Query::default())
            .saturating_add(wait.provenance.as_ref().map_or(0, Provenance::bytes));
        self.bytes -= wait.bytes - fixed;
        wait.bytes = fixed;
        wait.query = Query::default();
        let size = value.as_ref().map_or(0, |value| value.to_string().len());
        let (value, size) = if self.bytes.saturating_add(size) > MAX_BYTES {
            // The fixed entry allowance already reserves its bounded error.
            (
                Err(Error::action("Raw event wait result capacity exceeded")),
                0,
            )
        } else {
            (value, size)
        };
        self.bytes += size;
        wait.bytes += size;
        wait.terminal = Some(value);
    }
    /// Called at the native event delivery boundary after record/discard.
    /// Original listeners recheck their own query on every emitted event.
    pub fn event(&mut self, read: impl Fn(&str, f64, &Query) -> Value) {
        let ids = self
            .waits
            .iter()
            .filter(|(_, w)| w.terminal.is_none())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let wait = &self.waits[&id];
            let value = read(&wait.identity.tab, wait.after, &wait.query);
            if satisfied(&value) {
                self.finish(&id, Ok(value));
            }
        }
    }
    /// Native timer delivery is explicit: no polling heuristic or human clock
    /// exemption is introduced by this state owner. Freeze one result at a time.
    pub fn timers(&mut self, now: Instant, read: impl Fn(&str, f64, &Query) -> Value) {
        let ids = self
            .waits
            .iter()
            .filter(|(_, w)| w.terminal.is_none() && now >= w.deadline)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let wait = &self.waits[&id];
            let value = read(&wait.identity.tab, wait.after, &wait.query);
            self.finish(&id, Ok(value));
        }
    }
    pub fn timer(&mut self, id: &str, now: Instant, read: impl Fn(&str, f64, &Query) -> Value) {
        if let Some(wait) = self
            .waits
            .get(id)
            .filter(|w| w.terminal.is_none() && now >= w.deadline)
        {
            let value = read(&wait.identity.tab, wait.after, &wait.query);
            self.finish(id, Ok(value));
        }
    }
    /// A matching tab detach completes even when a future cursor is not
    /// truncated. The supplied log has already processed its history discard.
    pub fn detach(&mut self, tab: &str, read: impl Fn(&str, f64, &Query) -> Value) {
        let ids = self
            .waits
            .iter()
            .filter(|(_, w)| w.terminal.is_none() && w.identity.tab == tab)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let wait = &self.waits[&id];
            let value = read(tab, wait.after, &wait.query);
            self.finish(&id, Ok(value));
        }
    }
    pub fn snapshot(&self, id: &str, owner: &Owner, tab: &str) -> Result<Snapshot> {
        let wait = self
            .waits
            .get(id)
            .filter(|w| &w.identity.owner == owner && w.identity.tab == tab)
            .ok_or_else(|| Error::new(-32800, "Raw event wait is unavailable for this owner"))?;
        let provenance = wait
            .provenance
            .clone()
            .ok_or_else(|| Error::new(-32800, "Raw event wait has no native admission"))?;
        Ok(Snapshot {
            identity: wait.identity.clone(),
            provenance,
            terminal: wait.terminal.is_some(),
        })
    }
    pub fn entries(&self) -> Vec<(String, Identity)> {
        self.waits
            .iter()
            .map(|(id, w)| (id.clone(), w.identity.clone()))
            .collect()
    }
    pub fn is_waiting(&self) -> bool {
        self.waits.values().any(|w| w.terminal.is_none())
    }
    fn remove(&mut self, id: &str) -> Option<Wait> {
        let wait = self.waits.remove(id)?;
        self.bytes -= wait.bytes;
        Some(wait)
    }
    pub fn poll(&mut self, id: &str, identity: &Identity) -> Result<Polled> {
        let wait = self
            .waits
            .get(id)
            .filter(|w| w.identity.owner == identity.owner && w.identity.tab == identity.tab)
            .ok_or_else(|| Error::new(-32800, "Raw event wait is unavailable for this owner"))?;
        if wait.terminal.is_none() && wait.identity.generation != identity.generation {
            self.remove(id);
            return Err(Error::new(-32800, "Raw event wait attachment changed"));
        }
        if wait.terminal.is_none() {
            return Ok(Polled::Pending);
        }
        // A detached/reattached tab may have a newer live generation. A terminal
        // packet is already frozen native data for this same cell; retrieving
        // it reads no new record and creates no new provider authority.
        Ok(Polled::Ready(self.remove(id).unwrap().terminal.unwrap()))
    }
    pub fn cancel(&mut self, id: &str, identity: &Identity) -> Result<()> {
        if self
            .waits
            .get(id)
            .is_some_and(|w| w.identity.owner != identity.owner || w.identity.tab != identity.tab)
        {
            return Err(Error::new(
                -32800,
                "Raw event wait is unavailable for this owner",
            ));
        }
        self.remove(id);
        Ok(())
    }
    pub fn cancel_owner(&mut self, scope: &str, cell: Option<u64>) {
        let ids = self
            .waits
            .iter()
            .filter(|(_, w)| {
                w.identity.owner.scope == scope
                    && cell.is_none_or(|cell| w.identity.owner.cell == cell)
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            self.remove(&id);
        }
    }
    pub fn clear(&mut self) {
        self.waits.clear();
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::raw_events::Log;
    use serde_json::json;
    use std::time::Duration;
    fn identity() -> Identity {
        Identity {
            owner: Owner {
                scope: "owned-scope".into(),
                cell: 1,
            },
            tab: "1".into(),
            generation: 1,
        }
    }
    fn record(log: &mut Log, method: &str, tab: u64) {
        log.record(
            &tab.to_string(),
            json!({"sessionId":"owned-root"}),
            None,
            method,
            Some(json!({"tab":tab})),
        )
        .unwrap();
    }
    fn start(
        state: &mut State,
        log: &Log,
        identity: Identity,
        after: f64,
        query: Query,
        now: Instant,
    ) -> String {
        match state
            .start(
                identity,
                after,
                query,
                Some(now + Duration::from_millis(50)),
                |tab, after, query| log.read(tab, after, query),
            )
            .unwrap()
        {
            Started::Pending(id) => id,
            Started::Ready(_) => panic!("fixture must begin pending"),
        }
    }
    fn complete(state: &mut State, id: &str, identity: &Identity) -> Value {
        match state.poll(id, identity).unwrap() {
            Polled::Ready(value) => value.unwrap(),
            Polled::Pending => panic!("fixture must have completed"),
        }
    }
    #[test]
    fn raw_event_wait_delivery_packets_match_original_and_release_active_watch() {
        let oracle: Value =
            serde_json::from_str(include_str!("../tests/oracles/browser_raw_wait.json")).unwrap();
        for name in [
            "selected event completes and removes listeners",
            "timeout returns current nonmatching cursor",
            "discard truncation wakes empty event selection",
            "matching detach resolves even future cursor",
            "zero limit hasMore still waits for timer",
            "registration gap is closed by immediate listener check",
        ] {
            let expected = &oracle["results"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["name"] == name)
                .unwrap()["result"];
            let mut log = Log::default();
            let mut state = State::default();
            let owner = identity();
            let now = Instant::now();
            let mut query = Query::default();
            if name.starts_with("selected event") || name.starts_with("timeout returns") {
                query.methods = Some(vec!["Owned.match".into()]);
            }
            if name.starts_with("discard") {
                query.methods = Some(vec!["missing".into()]);
            }
            if name.starts_with("zero limit") {
                record(&mut log, "Owned.match", 1);
                query.limit = Some(0);
            }
            if name.starts_with("registration") {
                // A receipt between the caller's initial read and registry
                // insertion is included by the mandatory registration read.
                record(&mut log, "Owned.match", 1);
                let Started::Ready(value) = state
                    .start(
                        owner,
                        0.,
                        query,
                        Some(now + Duration::from_millis(50)),
                        |tab, after, q| log.read(tab, after, q),
                    )
                    .unwrap()
                else {
                    panic!("gap recheck failed")
                };
                assert_eq!(&value, expected);
                assert!(!state.is_waiting());
                continue;
            }
            let after = if name.starts_with("matching detach") {
                1e100
            } else {
                0.
            };
            let id = start(&mut state, &log, owner.clone(), after, query, now);
            assert!(state.is_waiting());
            match name {
                "selected event completes and removes listeners" => {
                    record(&mut log, "Owned.other", 1);
                    state.event(|t, a, q| log.read(t, a, q));
                    assert!(matches!(state.poll(&id, &owner).unwrap(), Polled::Pending));
                    record(&mut log, "Owned.match", 2);
                    state.event(|t, a, q| log.read(t, a, q));
                    assert!(matches!(state.poll(&id, &owner).unwrap(), Polled::Pending));
                    record(&mut log, "Owned.match", 1);
                    state.event(|t, a, q| log.read(t, a, q));
                    record(&mut log, "Owned.later", 1);
                    state.event(|t, a, q| log.read(t, a, q));
                }
                "timeout returns current nonmatching cursor" => {
                    record(&mut log, "Owned.other", 1);
                    state.event(|t, a, q| log.read(t, a, q));
                    state.timers(now + Duration::from_millis(49), |t, a, q| log.read(t, a, q));
                    assert!(state.is_waiting());
                    state.timers(now + Duration::from_millis(50), |t, a, q| log.read(t, a, q));
                }
                "discard truncation wakes empty event selection" => {
                    record(&mut log, "Owned.match", 1);
                    state.event(|t, a, q| log.read(t, a, q));
                    log.discard("1");
                    state.event(|t, a, q| log.read(t, a, q));
                }
                "matching detach resolves even future cursor" => {
                    record(&mut log, "Owned.match", 1);
                    state.detach("2", |t, a, q| log.read(t, a, q));
                    assert!(state.is_waiting());
                    log.discard("1");
                    state.detach("1", |t, a, q| log.read(t, a, q));
                }
                "zero limit hasMore still waits for timer" => {
                    record(&mut log, "Owned.match", 1);
                    state.event(|t, a, q| log.read(t, a, q));
                    assert!(state.is_waiting());
                    state.timers(now + Duration::from_millis(50), |t, a, q| log.read(t, a, q));
                }
                _ => unreachable!(),
            }
            assert!(!state.is_waiting(), "{name}");
            assert_eq!(&complete(&mut state, &id, &owner), expected, "{name}");
            assert!(state.waits.is_empty());
            assert_eq!(state.bytes, 0);
        }
    }
    #[test]
    fn raw_event_wait_native_owner_generation_and_single_consumption_fence() {
        let now = Instant::now();
        let log = Log::default();
        let mut state = State::default();
        let owner = identity();
        let id = start(&mut state, &log, owner.clone(), 0., Query::default(), now);
        let mut wrong = owner.clone();
        wrong.owner.cell += 1;
        assert!(state.poll(&id, &wrong).is_err());
        assert!(state.cancel(&id, &wrong).is_err());
        assert!(state.is_waiting());
        wrong = owner.clone();
        wrong.owner.scope = "other".into();
        assert!(state.poll(&id, &wrong).is_err());
        wrong = owner.clone();
        wrong.tab = "other".into();
        assert!(state.poll(&id, &wrong).is_err());
        wrong = owner.clone();
        wrong.generation += 1;
        assert!(state.poll(&id, &wrong).is_err());
        assert!(state.waits.is_empty());
        let id = start(&mut state, &log, owner.clone(), 0., Query::default(), now);
        state.detach("1", |t, a, q| log.read(t, a, q));
        assert!(!state.is_waiting());
        // A completion delivered before replacement is only old frozen data.
        assert_eq!(
            complete(&mut state, &id, &wrong),
            json!({"cursor":0,"events":[],"hasMore":false,"truncated":false})
        );
        assert!(state.poll(&id, &owner).is_err());
        let _id = start(&mut state, &log, owner, 0., Query::default(), now);
        state.cancel_owner("owned-scope", Some(2));
        assert!(state.is_waiting());
        state.cancel_owner("owned-scope", Some(1));
        assert!(state.waits.is_empty());
        assert_eq!(state.bytes, 0);
    }
    #[test]
    fn raw_event_wait_bounds_pending_and_completed_storage() {
        let now = Instant::now();
        let log = Log::default();
        let mut state = State::default();
        let owner = identity();
        for _ in 0..MAX_WAITS {
            start(&mut state, &log, owner.clone(), 0., Query::default(), now);
        }
        assert!(
            state
                .start(owner.clone(), 0., Query::default(), Some(now), |t, a, q| {
                    log.read(t, a, q)
                })
                .is_err()
        );
        state.clear();
        assert_eq!(state.bytes, 0);
        let query = Query {
            methods: Some(vec!["x".repeat(MAX_BYTES)]),
            ..Default::default()
        };
        assert!(
            state
                .start(owner.clone(), 0., query, Some(now), |t, a, q| log
                    .read(t, a, q))
                .is_err()
        );
        let ids = (0..3)
            .map(|_| start(&mut state, &log, owner.clone(), 0., Query::default(), now))
            .collect::<Vec<_>>();
        state.event(|_,_,_|json!({"cursor":1,"events":[{"owned":"x".repeat(MAX_BYTES/2)}],"hasMore":false,"truncated":false}));
        assert!(!state.is_waiting());
        assert!(state.bytes <= MAX_BYTES);
        let mut successes = 0;
        let mut capacity_errors = 0;
        for id in ids {
            match state.poll(&id, &owner).unwrap() {
                Polled::Ready(Ok(_)) => successes += 1,
                Polled::Ready(Err(error)) => {
                    assert!(error.message.contains("capacity"));
                    capacity_errors += 1;
                }
                Polled::Pending => panic!("terminal retention still pending"),
            }
        }
        assert_eq!((successes, capacity_errors), (1, 2));
        assert_eq!(state.bytes, 0);
    }
}
