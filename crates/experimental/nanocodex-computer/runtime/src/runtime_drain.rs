//! Native accounting for the deliberately narrow, settled-module output drain.
//! Promise ancestry is not authority. Only explicit trusted wrapper derivations
//! carry a request identity, and every registered unfinished obligation counts.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
const LIMIT: usize = 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Proof {
    pub continuations: Vec<String>,
}
impl Proof {
    pub fn validate(&self) -> Result<()> {
        if self.continuations.len() > LIMIT
            || self
                .continuations
                .iter()
                .any(|v| v.is_empty() || v.len() > 256)
        {
            return Err(Error::invalid("Invalid native drain proof"));
        }
        Ok(())
    }
}
#[derive(Default)]
struct Request {
    continuation: Option<String>,
    live: bool,
    refs: usize,
}
#[derive(Default)]
pub(super) struct Registry {
    cell: Option<u64>,
    draining: bool,
    next: u32,
    requests: BTreeMap<u32, Request>,
    obligations: BTreeMap<u32, Option<u32>>,
}
impl Registry {
    pub fn begin(&mut self, cell: u64) {
        self.finish();
        self.cell = Some(cell);
    }
    pub fn finish(&mut self) {
        self.cell = None;
        self.draining = false;
        self.requests.clear();
        self.obligations.clear();
    }
    pub fn settled(&mut self) {
        if self.cell.is_some() {
            self.draining = true;
        }
    }
    fn id(&mut self) -> Result<u32> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| Error::action("Native continuation identity exhausted"))?;
        Ok(self.next)
    }
    pub fn request(&mut self) -> Result<u32> {
        if self.cell.is_none() || self.requests.len() >= LIMIT {
            return Err(Error::action(
                "Native continuation budget exceeded or cell ended",
            ));
        }
        let id = self.id()?;
        self.requests.insert(
            id,
            Request {
                live: true,
                ..Default::default()
            },
        );
        Ok(id)
    }
    pub fn contains(&self, request: u32) -> bool {
        self.requests.contains_key(&request)
    }
    pub fn register(&mut self, cell: u64, request: Option<u32>) -> Result<u32> {
        if self.cell != Some(cell) || self.obligations.len() >= LIMIT {
            return Err(Error::action(
                "Native obligation budget exceeded or cell ended",
            ));
        }
        let id = self.id()?;
        let request = request.filter(|id| self.requests.contains_key(id));
        if let Some(request) = request {
            self.requests.get_mut(&request).unwrap().refs += 1;
        }
        self.obligations.insert(id, request);
        Ok(id)
    }
    pub fn complete(&mut self, id: u32) {
        if let Some(Some(request)) = self.obligations.remove(&id) {
            let entry = self.requests.get_mut(&request).unwrap();
            entry.refs -= 1;
        }
        self.collect();
    }
    pub fn replied(&mut self, id: u32, continuation: Option<String>) {
        if let Some(request) = self.requests.get_mut(&id) {
            request.live = false;
            request.continuation = continuation;
        }
        self.collect();
    }
    fn collect(&mut self) {
        self.requests
            .retain(|_, request| request.live || request.refs > 0);
    }
    pub fn proof(&self, current: u32) -> Option<Proof> {
        if !self.draining || self.obligations.is_empty() || !self.requests.get(&current)?.live {
            return None;
        }
        let mut continuations = BTreeSet::new();
        let mut owns_current = false;
        for request in self.obligations.values() {
            let id = (*request)?;
            if id == current {
                owns_current = true;
                continue;
            }
            let request = self.requests.get(&id)?;
            if request.live {
                return None;
            }
            continuations.insert(request.continuation.clone()?);
        }
        owns_current.then(|| Proof {
            continuations: continuations.into_iter().collect(),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_phase_and_complete_obligations_are_required() {
        let mut r = Registry::default();
        r.begin(1);
        let source = r.request().unwrap();
        let outer = r.register(1, Some(source)).unwrap();
        r.replied(source, Some("owned-operation".into()));
        let poll = r.request().unwrap();
        let polling = r.register(1, Some(poll)).unwrap();
        assert!(r.proof(poll).is_none());
        r.settled();
        assert_eq!(r.proof(poll).unwrap().continuations, ["owned-operation"]);
        let unknown = r.register(1, None).unwrap();
        assert!(r.proof(poll).is_none());
        r.complete(unknown);
        assert!(r.proof(poll).is_some());
        // Removing a JS drain entry has no native operation here: it remains counted.
        r.complete(outer);
        r.complete(polling);
        assert!(r.proof(poll).is_none());
        r.begin(2);
        r.complete(outer);
        assert!(!r.contains(source));
        assert!(r.register(1, Some(source)).is_err());
    }
    #[test]
    fn mixed_unbound_and_live_provider_requests_veto() {
        let mut r = Registry::default();
        r.begin(1);
        r.settled();
        let a = r.request().unwrap();
        r.register(1, Some(a)).unwrap();
        let b = r.request().unwrap();
        r.register(1, Some(b)).unwrap();
        assert!(r.proof(a).is_none());
        r.replied(b, None);
        assert!(r.proof(a).is_none());
        r.replied(b, Some("human".into()));
        assert!(r.proof(a).is_some());
    }
}
