//! Transport-independent input admission. Only the caller may inject accepted input.
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub const LEASE_DURATION: Duration = Duration::from_secs(10);
pub const INPUT_MAX_AGE: Duration = Duration::from_millis(250);

/// A single controller, with separate reliable-event and disposable-motion clocks.
/// Release native keys/buttons before granting a new owner, and after clearing a lease.
#[derive(Default)]
pub struct Lease {
    owner: String,
    generation: String,
    deadline: Option<Instant>,
    motion: u64,
    discrete: u64,
}
impl Lease {
    pub fn owner(&self) -> &str {
        &self.owner
    }
    pub fn generation(&self) -> &str {
        &self.generation
    }
    pub fn expired(&self) -> bool {
        self.expired_at(Instant::now())
    }
    pub fn expired_at(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }
    pub fn acquire(&mut self, owner: &str) {
        self.acquire_at(owner, Instant::now());
    }
    pub fn acquire_at(&mut self, owner: &str, now: Instant) {
        *self = Self {
            owner: owner.into(),
            generation: uuid::Uuid::new_v4().to_string(),
            deadline: Some(now + LEASE_DURATION),
            ..Self::default()
        };
    }
    pub fn valid(&self, owner: &str, generation: &str) -> bool {
        self.valid_at(owner, generation, Instant::now())
    }
    pub fn valid_at(&self, owner: &str, generation: &str, now: Instant) -> bool {
        !self.owner.is_empty()
            && self.owner == owner
            && self.generation == generation
            && !self.expired_at(now)
    }
    pub fn renew(&mut self, owner: &str, generation: &str) -> bool {
        let now = Instant::now();
        if !self.valid_at(owner, generation, now) {
            return false;
        }
        self.deadline = Some(now + LEASE_DURATION);
        true
    }
    /// Run a backend hold refresh only for a current lease, then recheck expiry.
    /// Failed refreshes never extend authorization. The caller releases native input.
    pub async fn renew_with(
        &mut self,
        owner: &str,
        generation: &str,
        refresh: impl std::future::Future<Output = bool>,
    ) -> bool {
        if !self.valid(owner, generation) || !refresh.await {
            return false;
        }
        self.renew(owner, generation)
    }
    /// Revoke authorization synchronously before awaiting native release or transport work.
    pub fn clear(&mut self) -> String {
        let owner = std::mem::take(&mut self.owner);
        *self = Self::default();
        owner
    }
    pub fn accept(&mut self, owner: &str, value: &Value) -> bool {
        if !self.valid(owner, value["generation"].as_str().unwrap_or("")) {
            return false;
        }
        let Some(sequence) = value["sequence"].as_u64().filter(|v| *v > 0) else {
            return false;
        };
        if value["kind"] == "move" {
            if sequence <= self.motion.max(self.discrete) {
                return false;
            }
            self.motion = sequence;
        } else {
            // Reliable key/button/gamepad releases may arrive after newer motion.
            if sequence <= self.discrete {
                return false;
            }
            self.discrete = sequence;
        }
        true
    }
}

/// Unreliable data channels accept motion only. Invalid data closes the viewer;
/// it must never be reclassified as reliable keyboard/gamepad input.
pub fn data_channel_event(viewer: &str, motion: bool, text: bool, bytes: &[u8]) -> Value {
    let data = if text && bytes.len() <= 8192 {
        serde_json::from_slice::<Value>(bytes)
            .ok()
            .filter(|v| v.is_object() && (!motion || v["kind"] == "move"))
    } else {
        None
    };
    match data {
        Some(data) => {
            json!({"type":if data.get("kind").is_some(){"input"}else{"control"},"viewer_id":viewer,"data":data})
        }
        None => json!({"type":"viewer_left","viewer_id":viewer}),
    }
}

/// Discard stale motion, but revoke the entire viewer on stale reliable input.
/// Silently dropping a key-up could otherwise leave native input held forever.
pub fn timely_event(value: Value, age: Duration) -> Option<Value> {
    if age > INPUT_MAX_AGE && value["type"] == "input" {
        if value["data"]["kind"] == "move" {
            return None;
        }
        return Some(json!({"type":"viewer_left","viewer_id":value["viewer_id"]}));
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(generation: &str, sequence: u64, kind: &str) -> Value {
        json!({"generation":generation,"sequence":sequence,"kind":kind})
    }
    #[test]
    fn reliable_release_survives_newer_motion_but_never_replays() {
        let mut lease = Lease::default();
        lease.acquire("viewer");
        let generation = lease.generation().to_owned();
        assert!(lease.accept("viewer", &event(&generation, 1, "key")));
        assert!(lease.accept("viewer", &event(&generation, 4, "move")));
        assert!(lease.accept("viewer", &event(&generation, 2, "key")));
        assert!(lease.accept("viewer", &event(&generation, 3, "gamepad")));
        assert!(!lease.accept("viewer", &event(&generation, 3, "gamepad")));
        assert!(!lease.accept("viewer", &event(&generation, 3, "move")));
        assert!(!lease.accept("viewer", &event(&generation, 0, "button")));
    }
    #[test]
    fn clear_and_reacquire_reject_delayed_input_and_renewal() {
        let mut lease = Lease::default();
        lease.acquire("viewer");
        let previous = lease.generation().to_owned();
        assert!(!lease.accept("other", &event(&previous, 1, "key")));
        assert_eq!(lease.clear(), "viewer");
        assert!(!lease.accept("viewer", &event(&previous, 1, "key")));
        assert!(!lease.renew("viewer", &previous));
        lease.acquire("viewer");
        assert_ne!(lease.generation(), previous);
        assert!(!lease.accept("viewer", &event(&previous, 1, "key")));
        let generation = lease.generation().to_owned();
        assert!(lease.renew("viewer", &generation));
    }
    #[tokio::test]
    async fn keepalive_only_refreshes_current_authorized_holds() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = AtomicUsize::new(0);
        let mut lease = Lease::default();
        lease.acquire("viewer");
        let generation = lease.generation().to_owned();
        for (owner, generation) in [("other", generation.as_str()), ("viewer", "old")] {
            assert!(
                !lease
                    .renew_with(owner, generation, async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        true
                    })
                    .await
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let deadline = lease.deadline;
        assert!(
            !lease
                .renew_with("viewer", &generation, async { false })
                .await
        );
        assert_eq!(lease.deadline, deadline);
        assert!(
            lease
                .renew_with("viewer", &generation, async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    true
                })
                .await
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        lease.deadline = Some(Instant::now());
        assert!(
            !lease
                .renew_with("viewer", &generation, async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    true
                })
                .await
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn exact_deadline_revokes_authority() {
        let mut lease = Lease::default();
        let now = Instant::now();
        lease.acquire_at("viewer", now);
        assert!(lease.valid_at(
            "viewer",
            lease.generation(),
            now + LEASE_DURATION - Duration::from_nanos(1)
        ));
        assert!(!lease.valid_at("viewer", lease.generation(), now + LEASE_DURATION));
    }
    #[test]
    fn unreliable_channel_never_injects_keyboard_or_gamepad() {
        for kind in ["key", "button", "gamepad"] {
            let bytes = json!({"kind":kind,"down":false}).to_string();
            assert_eq!(
                data_channel_event("viewer", true, true, bytes.as_bytes())["type"],
                "viewer_left"
            );
            assert_eq!(
                data_channel_event("viewer", false, true, bytes.as_bytes())["type"],
                "input"
            );
        }
        for bytes in [b"null".as_slice(), b"[]", b"broken"] {
            assert_eq!(
                data_channel_event("viewer", false, true, bytes)["type"],
                "viewer_left"
            );
        }
        assert_eq!(
            data_channel_event("viewer", false, false, b"{}")["type"],
            "viewer_left"
        );
        assert_eq!(
            data_channel_event("viewer", false, true, &[b' '; 8193])["type"],
            "viewer_left"
        );
        assert_eq!(
            data_channel_event("viewer", true, true, br#"{"kind":"move"}"#)["type"],
            "input"
        );
    }
    #[test]
    fn stale_release_revokes_while_stale_motion_is_discarded() {
        for kind in ["key", "button", "gamepad"] {
            let value =
                json!({"type":"input","viewer_id":"viewer","data":{"kind":kind,"down":false}});
            assert_eq!(timely_event(value.clone(), INPUT_MAX_AGE).unwrap(), value);
            assert_eq!(
                timely_event(value, INPUT_MAX_AGE + Duration::from_nanos(1)).unwrap(),
                json!({"type":"viewer_left","viewer_id":"viewer"})
            );
        }
        assert!(
            timely_event(
                json!({"type":"input","data":{"kind":"move"}}),
                Duration::from_secs(1)
            )
            .is_none()
        );
    }
}
