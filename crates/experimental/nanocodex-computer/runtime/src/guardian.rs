//! Local control lease coordinator; OS lock/intervention state comes only from
//! trusted host callbacks. This module never unlocks the operating system.
use crate::{Error, Result};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
struct HostState {
    locked: bool,
    revision: u64,
}
struct Lease {
    token: String,
    owner: String,
    expires: Duration,
    revision: u64,
    generation: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct LeaseReceipt {
    pub lease: String,
    pub owner: String,
    pub expires_in_ms: u64,
    pub host_revision: u64,
    pub generation: u64,
}
pub struct Guardian {
    state: Option<HostState>,
    suppressed: bool,
    lease: Option<Lease>,
    generation: u64,
    clock: Box<dyn Fn() -> Duration>,
    last_clock: Duration,
}
impl Default for Guardian {
    fn default() -> Self {
        Self::new()
    }
}
impl Guardian {
    pub fn new() -> Self {
        let start = Instant::now();
        Self::with_clock(move || start.elapsed())
    }
    /// An injected monotonic clock supports deterministic lifecycle testing.
    pub fn with_clock(clock: impl Fn() -> Duration + 'static) -> Self {
        Self {
            state: None,
            suppressed: false,
            lease: None,
            generation: 0,
            clock: Box::new(clock),
            last_clock: Duration::ZERO,
        }
    }
    fn now(&mut self) -> Duration {
        self.last_clock = self.last_clock.max((self.clock)());
        self.last_clock
    }
    fn expire(&mut self) {
        let now = self.now();
        if self.lease.as_ref().is_some_and(|l| l.expires <= now) {
            self.lease = None;
        }
    }
    fn invalidate(&mut self) -> Result<()> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| Error::action("Guardian generation exhausted"))?;
        self.lease = None;
        Ok(())
    }
    /// Host-only. Repeating an identical snapshot is harmless; stale revisions or
    /// changing state without a new revision fail. Every new revision revokes grants.
    pub fn set_host_state(&mut self, locked: bool, revision: u64) -> Result<()> {
        if let Some(state) = self.state {
            if revision < state.revision || (revision == state.revision && locked != state.locked) {
                return Err(Error::new(
                    -32009,
                    "Stale or inconsistent guardian host state",
                ));
            }
            if revision == state.revision {
                return Ok(());
            }
        }
        self.invalidate()?;
        self.state = Some(HostState { locked, revision });
        Ok(())
    }
    /// Physical intervention revokes current access and suppresses new leases until
    /// the host acknowledges a later user turn with resume_after_intervention.
    pub fn intervene(&mut self, revision: u64) -> Result<()> {
        let state = self
            .state
            .ok_or_else(|| Error::action("Guardian host state is unavailable"))?;
        if revision <= state.revision {
            return Err(Error::new(
                -32009,
                "Intervention requires a new host revision",
            ));
        }
        self.set_host_state(state.locked, revision)?;
        self.suppressed = true;
        Ok(())
    }
    pub fn resume_after_intervention(&mut self, revision: u64) -> Result<()> {
        let state = self
            .state
            .ok_or_else(|| Error::action("Guardian host state is unavailable"))?;
        if state.locked {
            return Err(Error::new(-32003, "Computer is locked"));
        }
        if revision <= state.revision {
            return Err(Error::new(-32009, "Resume requires a newer host revision"));
        }
        self.set_host_state(false, revision)?;
        self.suppressed = false;
        Ok(())
    }
    pub fn acquire(&mut self, owner: &str, ttl_ms: u64) -> Result<LeaseReceipt> {
        validate_owner(owner)?;
        validate_ttl(ttl_ms)?;
        self.expire();
        let state = self.require_available()?;
        if self.lease.is_some() {
            return Err(Error::new(
                -32009,
                "Computer control already has an active owner",
            ));
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::action("Cannot generate control lease"))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let expires = self.now() + Duration::from_millis(ttl_ms);
        let receipt = LeaseReceipt {
            lease: token.clone(),
            owner: owner.into(),
            expires_in_ms: ttl_ms,
            host_revision: state.revision,
            generation: self.generation,
        };
        self.lease = Some(Lease {
            token,
            owner: owner.into(),
            expires,
            revision: state.revision,
            generation: self.generation,
        });
        Ok(receipt)
    }
    fn require_available(&self) -> Result<HostState> {
        let state = self
            .state
            .ok_or_else(|| Error::new(-32003, "Guardian host state is unavailable"))?;
        if state.locked {
            return Err(Error::new(-32003, "Computer is locked"));
        }
        if self.suppressed {
            return Err(Error::new(
                -32010,
                "Computer control is paused after user intervention",
            ));
        }
        Ok(state)
    }
    pub fn authorize(&mut self, owner: &str, token: &str) -> Result<()> {
        validate_owner(owner)?;
        self.expire();
        let state = self.require_available()?;
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| Error::new(-32003, "No active control lease"))?;
        if lease.owner != owner
            || !same_token(&lease.token, token)
            || lease.revision != state.revision
            || lease.generation != self.generation
        {
            return Err(Error::new(
                -32003,
                "Control lease does not authorize this owner or host state",
            ));
        }
        Ok(())
    }
    pub fn renew(&mut self, owner: &str, token: &str, ttl_ms: u64) -> Result<LeaseReceipt> {
        validate_ttl(ttl_ms)?;
        self.authorize(owner, token)?;
        let expires = self.now() + Duration::from_millis(ttl_ms);
        let lease = self.lease.as_mut().unwrap();
        lease.expires = expires;
        Ok(LeaseReceipt {
            lease: lease.token.clone(),
            owner: lease.owner.clone(),
            expires_in_ms: ttl_ms,
            host_revision: lease.revision,
            generation: lease.generation,
        })
    }
    pub fn release(&mut self, owner: &str, token: &str) -> Result<bool> {
        self.expire();
        let Some(lease) = self.lease.as_ref() else {
            return Ok(false);
        };
        if lease.owner != owner || !same_token(&lease.token, token) {
            return Err(Error::new(
                -32003,
                "Cannot release another owner's control lease",
            ));
        }
        self.lease = None;
        Ok(true)
    }
    /// Trusted host cleanup when an owner exits; no lease token is required because
    /// this function must never be exposed as a caller-selected owner RPC.
    pub fn revoke_owner(&mut self, owner: &str) -> Result<bool> {
        if self.lease.as_ref().is_some_and(|l| l.owner == owner) {
            self.invalidate()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn status(&mut self) -> Value {
        self.expire();
        let now = self.now();
        json!({"host_state_available":self.state.is_some(),"locked":self.state.map(|s|s.locked),"host_revision":self.state.map(|s|s.revision),"intervention_suppressed":self.suppressed,"generation":self.generation,"active_owner":self.lease.as_ref().map(|l|&l.owner),"remaining_ms":self.lease.as_ref().map(|l|l.expires.saturating_sub(now).as_millis()as u64)})
    }
}
fn validate_owner(owner: &str) -> Result<()> {
    if owner.trim().is_empty() || owner.len() > 4096 {
        return Err(Error::invalid(
            "Control owner must be a nonempty bounded identity",
        ));
    }
    Ok(())
}
fn validate_ttl(ttl: u64) -> Result<()> {
    if !(1..=300000).contains(&ttl) {
        return Err(Error::invalid("Lease TTL must be 1..300000ms"));
    }
    Ok(())
}
fn same_token(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |n, (x, y)| n | (x ^ y)) == 0
}
