//! Keep a briefly disconnected peer alive without retaining its input authority.
use crate::{audio_duplex::Microphone, capture::Task, video::Event};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState as State;

// ICE Disconnected is recoverable. This bounds retained media resources, not
// authorization: control and microphone permission are revoked synchronously.
const GRACE: Duration = Duration::from_secs(5);
struct StateData {
    permission: Arc<AtomicBool>,
    established: bool,
    grant: Option<String>,
    revoked_grant: Option<String>,
    terminal: bool,
    deadline: Option<tokio::time::Instant>,
}
pub(crate) struct Recovery {
    state: Mutex<StateData>,
    pub connected: Arc<AtomicBool>,
    microphone: Arc<Microphone>,
    deadline: watch::Sender<Option<tokio::time::Instant>>,
    events: mpsc::Sender<Event>,
    failed: Arc<AtomicBool>,
    viewer: String,
}
impl Recovery {
    pub fn new(
        microphone: Arc<Microphone>,
        events: mpsc::Sender<Event>,
        failed: Arc<AtomicBool>,
        viewer: String,
    ) -> (Arc<Self>, Task) {
        let (deadline, mut updates) = watch::channel(None);
        let recovery = Arc::new(Self {
            state: Mutex::new(StateData {
                permission: Arc::new(AtomicBool::new(false)),
                established: false,
                grant: None,
                revoked_grant: None,
                terminal: false,
                deadline: None,
            }),
            connected: Arc::new(AtomicBool::new(false)),
            microphone,
            deadline,
            events,
            failed,
            viewer,
        });
        let owned = recovery.clone();
        let task = Task(tokio::spawn(async move {
            loop {
                let deadline = *updates.borrow_and_update();
                if let Some(deadline) = deadline {
                    tokio::select! {
                        biased;
                        changed = updates.changed() => { if changed.is_err() { break; } }
                        _ = tokio::time::sleep_until(deadline) => {
                            let mut state = owned.state.lock().unwrap_or_else(|e| e.into_inner());
                            // Recovery may race the timer. Only this peer's current
                            // outage owns the deadline; repeated notifications cannot extend it.
                            if state.deadline == Some(deadline) && !state.terminal {
                                owned.expire(&mut state);
                                break;
                            }
                        }
                    }
                } else if updates.changed().await.is_err() {
                    break;
                }
            }
        }));
        (recovery, task)
    }
    fn event(&self, kind: &str) {
        if self
            .events
            .try_send(Event {
                value: json!({"type":kind,"viewer_id":self.viewer}),
                outgoing: false,
                created: Instant::now(),
                active: None,
            })
            .is_err_and(|error| matches!(error, mpsc::error::TrySendError::Full(_)))
        {
            self.failed.store(true, Ordering::Release);
        }
    }
    fn expire(&self, state: &mut StateData) {
        self.revoke(state);
        state.terminal = true;
        state.deadline = None;
        self.deadline.send_replace(None);
        self.event("viewer_left");
    }
    fn revoke(&self, state: &mut StateData) {
        if let Some(grant) = state.grant.take() {
            state.revoked_grant = Some(grant);
        }
        self.connected.store(false, Ordering::Release);
        state.permission.store(false, Ordering::Release);
        self.microphone.revoke();
    }
    pub fn transition(&self, next: State) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.terminal {
            return;
        }
        // The callback and timer share an absolute deadline. A busy executor
        // cannot let a late Connected callback resurrect the peer.
        if state
            .deadline
            .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
        {
            self.expire(&mut state);
            return;
        }
        match next {
            State::Connected => {
                let recovering = state.deadline.take().is_some();
                if !state.permission.load(Ordering::Acquire) {
                    // Never revive old leases or queued input when ICE recovers.
                    state.permission = Arc::new(AtomicBool::new(true));
                }
                state.established = true;
                self.connected.store(true, Ordering::Release);
                self.deadline.send_replace(None);
                if recovering {
                    self.event("viewer_resumed");
                }
            }
            State::Disconnected | State::Connecting if state.established => {
                self.revoke(&mut state);
                if state.deadline.is_none() {
                    state.deadline = Some(tokio::time::Instant::now() + GRACE);
                    self.deadline.send_replace(state.deadline);
                    self.event("viewer_suspended");
                }
            }
            State::Disconnected | State::Failed | State::Closed => {
                self.revoke(&mut state);
                state.terminal = true;
                state.deadline = None;
                self.deadline.send_replace(None);
                self.event("viewer_left");
            }
            _ => {}
        }
    }
    pub fn grant(&self, permission: &Arc<AtomicBool>, generation: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !Arc::ptr_eq(&state.permission, permission) || !permission.load(Ordering::Acquire) {
            return false;
        }
        state.grant = Some(generation.to_owned());
        true
    }
    pub fn revoked_control(&self, data: &serde_json::Value) -> bool {
        if !matches!(data["type"].as_str(), Some("renew" | "release")) {
            return false;
        }
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // SCTP can deliver an old renewal/release after recovery. Only this
        // peer's last revoked grant is inert; unknown/malformed requests fail closed.
        state
            .revoked_grant
            .as_deref()
            .is_some_and(|generation| data["generation"].as_str() == Some(generation))
    }
    pub fn permission(&self) -> Option<Arc<AtomicBool>> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .permission
            .load(Ordering::Acquire)
            .then(|| state.permission.clone())
    }
    pub fn suspended(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.deadline.is_some() || state.terminal
    }
    pub fn microphone(
        &self,
        enabled: bool,
        remaining: Duration,
        permission: &Arc<AtomicBool>,
    ) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Arc::ptr_eq(&state.permission, permission)
            && permission.load(Ordering::Acquire)
            && self.microphone.set_enabled(enabled, remaining)
    }
    pub fn retire(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.revoke(&mut state);
        state.terminal = true;
    }
}

#[cfg(test)]
#[path = "peer_recovery_tests.rs"]
mod tests;
