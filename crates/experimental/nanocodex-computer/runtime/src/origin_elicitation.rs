//! Cooperative, host-owned origin consent transport. No provider call control is
//! retained: begin and poll never wait for a human or dispatch browser commands.
pub use crate::download_elicitation::{Context, ResetPredicate, TransitionPredicate};
use crate::{Error, Result, protocol};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};
pub type Emit = Arc<dyn Fn(&Value, Instant) -> Result<mpsc::Receiver<Result<()>>> + Send + Sync>;
struct Active {
    token: Arc<()>,
    context: Context,
    aborted: Option<Error>,
}
struct Pending {
    deadline: Instant,
    written: bool,
    acknowledgement: mpsc::Receiver<Result<()>>,
    result: Option<Result<Value>>,
}
#[derive(Default)]
struct State {
    disconnected: bool,
    active: Option<Active>,
    pending: BTreeMap<String, Pending>,
}
struct Inner {
    state: Mutex<State>,
    wake: Condvar,
    output_health: Option<Arc<dyn Fn() -> Result<()> + Send + Sync>>,
    emit: Emit,
    reset: ResetPredicate,
    transition: Option<TransitionPredicate>,
}
#[derive(Clone)]
pub struct OriginElicitationBroker(Arc<Inner>);
#[derive(Clone)]
pub struct Ticket {
    broker: OriginElicitationBroker,
    token: Arc<()>,
}
/// Read-only connection/cell lifetime. This view cannot start an approval,
/// obtain a decision, grant access, or suspend execution time.
#[derive(Clone)]
pub struct ConnectionLiveness(Ticket);
impl ConnectionLiveness {
    pub fn validate(&self) -> Result<()> {
        self.0.validate()?;
        // Output health belongs to the connection owner. It may itself retire
        // this ticket, so invoke it without the broker lock and recheck after.
        let check = self
            .0
            .broker
            .0
            .output_health
            .as_ref()
            .ok_or_else(|| Error::action("Connection output health is unavailable"))?;
        let result = check();
        self.0.validate()?;
        result
    }
}
fn ended() -> Error {
    Error::new(-32800, "Origin approval scope ended")
}
impl State {
    fn context(&self, token: &Arc<()>) -> Result<&Context> {
        if self.disconnected {
            return Err(Error::action("Elicitation client disconnected"));
        }
        let active = self
            .active
            .as_ref()
            .filter(|a| Arc::ptr_eq(&a.token, token))
            .ok_or_else(ended)?;
        if let Some(error) = &active.aborted {
            return Err(error.clone());
        }
        Ok(&active.context)
    }
    fn abort(&mut self, error: Error) {
        if let Some(active) = &mut self.active {
            active.aborted.get_or_insert(error);
        }
        self.pending.clear();
    }
}
impl OriginElicitationBroker {
    pub fn new(emit: Emit, reset: ResetPredicate, transition: Option<TransitionPredicate>) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            output_health: None,
            emit,
            reset,
            transition,
        }))
    }
    /// Trusted connection owner supplies terminal output status, including after
    /// a prompt's successful initial acknowledgement.
    pub fn with_output_health(mut self, health: Arc<dyn Fn() -> Result<()> + Send + Sync>) -> Self {
        Arc::get_mut(&mut self.0)
            .expect("configure broker before sharing")
            .output_health = Some(health);
        self
    }
    pub fn activate(&self, context: Context) -> Ticket {
        let token = Arc::new(());
        let mut state = self.0.state.lock().unwrap();
        state.pending.clear();
        state.active = Some(Active {
            token: token.clone(),
            context,
            aborted: None,
        });
        Ticket {
            broker: self.clone(),
            token,
        }
    }
    pub fn abort_active(&self, error: Error) {
        self.0.state.lock().unwrap().abort(error);
        self.0.wake.notify_all();
    }
    pub fn disconnect(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.disconnected = true;
        state.abort(ended());
        self.0.wake.notify_all();
    }
    /// Reader records decisions only. Late replies cannot populate a grant cache.
    pub fn route(&self, message: &Value) -> bool {
        let mut state = self.0.state.lock().unwrap();
        if message.get("method").is_none()
            && let Some(id) = message["id"].as_str()
            && id.starts_with("skyre-origin-elicitation-")
        {
            if let Some(pending) = state.pending.get_mut(id)
                && pending.result.is_none()
            {
                pending.result = Some(protocol::validate_response(message).and_then(|_| {
                    if message.get("error").is_some() {
                        Err(Error::new(-32004, "Origin elicitation failed"))
                    } else {
                        match message["result"]["action"].as_str() {
                            Some(action @ ("accept" | "decline" | "cancel")) => {
                                Ok(json!({"action": action}))
                            }
                            _ => Err(Error::invalid("Invalid origin approval decision")),
                        }
                    }
                }));
            }
            self.0.wake.notify_all();
            return true;
        }
        let Some(active) = &state.active else {
            return false;
        };
        if protocol::validate_request(message).is_err() {
            return false;
        }
        let cancelled = message["method"] == "notifications/cancelled"
            && message.get("id").is_none()
            && message["params"].get("requestId") == Some(&active.context.request_id);
        let reset = (self.0.reset)(message, active.context.mcp_initialized);
        let transition = matches!(
            message["method"].as_str(),
            Some("host/turn" | "host/recover")
        ) && self
            .0
            .transition
            .as_ref()
            .is_some_and(|check| check(message));
        if cancelled || reset || transition {
            state.abort(ended());
            self.0.wake.notify_all();
        }
        false
    }
}
impl Ticket {
    pub fn connection_liveness(&self) -> Option<ConnectionLiveness> {
        self.broker
            .0
            .output_health
            .as_ref()
            .map(|_| ConnectionLiveness(self.clone()))
    }
    pub fn validate(&self) -> Result<()> {
        self.broker
            .0
            .state
            .lock()
            .unwrap()
            .context(&self.token)
            .map(|_| ())
    }
    pub fn finish(&self) {
        let mut state = self.broker.0.state.lock().unwrap();
        if state
            .active
            .as_ref()
            .is_some_and(|a| Arc::ptr_eq(&a.token, &self.token))
        {
            state.abort(ended());
            state.active = None;
            self.broker.0.wake.notify_all();
        }
    }
    pub fn begin(&self, origin: &str) -> Result<String> {
        let deadline = {
            let state = self.broker.0.state.lock().unwrap();
            let context = state.context(&self.token)?;
            if !context.mcp_initialized || !context.elicitation_supported {
                return Err(Error::unsupported(
                    "Browser origin access requires a host that supports elicitations",
                ));
            }
            if context.elicitation_timeout.is_zero() {
                return Err(Error::invalid("elicitation_timeout_ms must be positive"));
            }
            if state.pending.len() >= 128 {
                return Err(Error::action("Too many pending origin approvals"));
            }
            Instant::now()
                .checked_add(context.elicitation_timeout)
                .ok_or_else(|| Error::invalid("Elicitation timeout exceeds clock range"))?
        };
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce)
            .map_err(|_| Error::action("Cannot generate origin approval ID"))?;
        let id = format!(
            "skyre-origin-elicitation-{}",
            nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let outgoing = json!({"jsonrpc":"2.0","id":id,"method":"elicitation/create","params":{"message":format!("Allow Browser Use to access {origin}?"),"requestedSchema":{"type":"object","properties":{}},"_meta":{"codex_approval_kind":"mcp_tool_call","codex_sensitive_action":true,"codex_request_type":"approval_request","connector_id":"browser-use","connector_name":"Browser Use","tool_name":"access_browser_origin","tool_title":"Access browser origin","tool_params":{"origin":origin},"tool_params_display":[],"origin":origin}}});
        if serde_json::to_vec(&outgoing)?.len() > protocol::MAX_FRAME {
            return Err(Error::invalid("Origin elicitation request exceeds 8 MiB"));
        }
        // Register before enqueue so a fast client response cannot overtake it.
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut state = self.broker.0.state.lock().unwrap();
        state.context(&self.token)?;
        if state.pending.len() >= 128 {
            return Err(Error::action("Too many pending origin approvals"));
        }
        state.pending.insert(
            id.clone(),
            Pending {
                deadline,
                written: false,
                acknowledgement: receiver,
                result: None,
            },
        );
        drop(state);
        let acknowledgement = (self.broker.0.emit)(&outgoing, deadline);
        let mut state = self.broker.0.state.lock().unwrap();
        state.context(&self.token)?;
        match acknowledgement {
            Ok(receiver) => {
                if let Some(p) = state.pending.get_mut(&id) {
                    p.acknowledgement = receiver;
                }
            }
            Err(error) => {
                state.pending.remove(&id);
                return Err(error);
            }
        }
        drop(sender);
        Ok(id)
    }
    pub fn poll(&self, id: &str) -> Result<Option<Value>> {
        let mut state = self.broker.0.state.lock().unwrap();
        state.context(&self.token)?;
        let pending = state
            .pending
            .get_mut(id)
            .ok_or_else(|| Error::action("Origin approval is no longer pending"))?;
        let result = (|| {
            if Instant::now() >= pending.deadline {
                return Err(Error::new(-32008, "Origin approval timed out"));
            }
            if !pending.written {
                match pending.acknowledgement.try_recv() {
                    Ok(result) => {
                        result?;
                        pending.written = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => return Ok(None),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(Error::action("Elicitation output disconnected"));
                    }
                }
            }
            pending.result.take().transpose()
        })();
        if !matches!(result, Ok(None)) {
            state.pending.remove(id);
        }
        result
    }
    pub(crate) fn human_ready(&self, id: &str) -> Result<bool> {
        if let Some(check) = &self.broker.0.output_health {
            check()?;
        }
        let state = self.broker.0.state.lock().unwrap();
        state.context(&self.token)?;
        Ok(state.pending.get(id).is_some_and(|pending| {
            pending.written && pending.result.is_none() && Instant::now() < pending.deadline
        }))
    }
    /// One actual provider call owns this guard. It never crosses the JS resolver
    /// boundary, and no expired model or prompt deadline receives later credit.
    pub(crate) fn wait_slice(
        &self,
        id: &str,
        control: &crate::runtime::ProviderControl,
    ) -> Result<()> {
        if !self.human_ready(id)? {
            return Ok(());
        }
        let slice_deadline = {
            let state = self.broker.0.state.lock().unwrap();
            state.context(&self.token)?;
            let Some(pending) = state.pending.get(id) else {
                return Ok(());
            };
            let now = Instant::now();
            if !pending.written || pending.result.is_some() || now >= pending.deadline {
                return Ok(());
            }
            pending.deadline.min(now + Duration::from_millis(10))
        };
        let suspension = control.suspend()?;
        let result = (|| {
            if let Some(check) = &self.broker.0.output_health {
                check()?;
            }
            let state = self.broker.0.state.lock().unwrap();
            state.context(&self.token)?;
            let Some(pending) = state.pending.get(id) else {
                return Ok(());
            };
            let now = Instant::now();
            if !pending.written || pending.result.is_some() || now >= pending.deadline {
                return Ok(());
            }
            // Suspension protocol setup consumes the slice instead of starting
            // a fresh ten milliseconds after an acknowledgement arrives.
            let duration = slice_deadline.saturating_duration_since(now);
            let (state, _) = self.broker.0.wake.wait_timeout(state, duration).unwrap();
            state.context(&self.token)?;
            drop(state);
            if let Some(check) = &self.broker.0.output_health {
                check()?;
            }
            Ok(())
        })();
        suspension.resume()?;
        result
    }
    pub fn cancel(&self, id: &str) {
        let mut state = self.broker.0.state.lock().unwrap();
        if state.context(&self.token).is_ok() {
            state.pending.remove(id);
            self.broker.0.wake.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn fixture(timeout: Duration) -> (OriginElicitationBroker, Ticket, mpsc::Receiver<Value>) {
        let (send, receive) = mpsc::channel();
        let broker = OriginElicitationBroker::new(
            Arc::new(move |value, _| {
                send.send(value.clone()).unwrap();
                let (s, r) = mpsc::channel();
                s.send(Ok(())).unwrap();
                Ok(r)
            }),
            Arc::new(|m, _| m["method"] == "reset"),
            Some(Arc::new(|m| m["params"]["authorityToken"] == "owned")),
        );
        let ticket = broker.activate(Context {
            request_id: json!(1),
            mcp_initialized: true,
            elicitation_supported: true,
            elicitation_timeout: timeout,
        });
        (broker, ticket, receive)
    }
    #[test]
    fn independent_pending_replies_and_retired_cells() {
        let (broker, ticket, receive) = fixture(Duration::from_secs(1));
        let a = ticket.begin("https://a.example").unwrap();
        let b = ticket.begin("https://b.example").unwrap();
        assert_eq!(receive.try_iter().count(), 2);
        assert!(ticket.poll(&a).unwrap().is_none());
        assert!(broker.route(&json!({"jsonrpc":"2.0","id":b,"result":{"action":"accept"}})));
        assert_eq!(ticket.poll(&b).unwrap().unwrap()["action"], "accept");
        assert!(ticket.poll(&a).unwrap().is_none());
        ticket.cancel(&a);
        broker.route(&json!({"jsonrpc":"2.0","id":a,"result":{"action":"accept"}}));
        assert!(ticket.poll(&a).is_err());
        ticket.finish();
        assert!(ticket.begin("https://a.example").is_err());
    }
    #[test]
    fn a_decision_does_not_overtake_failed_output_acknowledgement() {
        let (acks, receive) = mpsc::channel();
        let broker = OriginElicitationBroker::new(
            Arc::new(move |_, _| {
                let (send, receive) = mpsc::channel();
                acks.send(send).unwrap();
                Ok(receive)
            }),
            Arc::new(|_, _| false),
            None,
        );
        let ticket = broker.activate(Context {
            request_id: json!(1),
            mcp_initialized: true,
            elicitation_supported: true,
            elicitation_timeout: Duration::from_secs(1),
        });
        let id = ticket.begin("https://owned.example").unwrap();
        broker.route(&json!({"jsonrpc":"2.0","id":id,"result":{"action":"accept","unused":"untrusted optional fields are not retained"}}));
        assert!(ticket.poll(&id).unwrap().is_none());
        receive
            .recv()
            .unwrap()
            .send(Err(Error::action("Owned output failure")))
            .unwrap();
        assert!(ticket.poll(&id).is_err());
        let id = ticket.begin("https://owned.example").unwrap();
        receive.recv().unwrap().send(Ok(())).unwrap();
        broker.route(&json!({"jsonrpc":"2.0","id":id,"result":{"action":"unknown"}}));
        assert!(ticket.poll(&id).is_err());
    }
    #[test]
    fn reset_authenticated_transition_expiry_and_disconnect_revoke() {
        for control in [
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
            json!({"jsonrpc":"2.0","id":2,"method":"reset"}),
            json!({"jsonrpc":"2.0","id":2,"method":"host/turn","params":{"authorityToken":"owned"}}),
        ] {
            let (broker, ticket, _receive) = fixture(Duration::from_secs(1));
            let id = ticket.begin("https://a.example").unwrap();
            assert!(!broker.route(&json!({"jsonrpc":"2.0","id":2,"method":"host/turn","params":{"authorityToken":"forged"}})));
            ticket.validate().unwrap();
            broker.route(&control);
            broker.route(&json!({"jsonrpc":"2.0","id":id,"result":{"action":"accept"}}));
            assert!(ticket.poll(&id).is_err());
        }
        let (broker, ticket, _receive) = fixture(Duration::from_millis(1));
        let id = ticket.begin("https://a.example").unwrap();
        std::thread::sleep(Duration::from_millis(3));
        assert_eq!(ticket.poll(&id).unwrap_err().code, -32008);
        broker.disconnect();
        assert!(ticket.validate().is_err());
    }
}

#[cfg(test)]
mod drain_tests {
    use super::*;
    use crate::runtime::ProviderControl;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    fn setup(
        timeout: Duration,
        healthy: Arc<AtomicBool>,
    ) -> (OriginElicitationBroker, Ticket, String) {
        let broker = OriginElicitationBroker::new(
            Arc::new(|_, _| {
                let (s, r) = mpsc::channel();
                s.send(Ok(())).unwrap();
                Ok(r)
            }),
            Arc::new(|_, _| false),
            None,
        )
        .with_output_health(Arc::new(move || {
            if healthy.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(Error::action("Owned terminal output failure"))
            }
        }));
        let ticket = broker.activate(Context {
            request_id: json!(1),
            mcp_initialized: true,
            elicitation_supported: true,
            elicitation_timeout: timeout,
        });
        let id = ticket.begin("https://owned.example").unwrap();
        assert!(ticket.poll(&id).unwrap().is_none());
        (broker, ticket, id)
    }
    #[test]
    fn native_human_slice_balances_on_abort_and_terminal_output() {
        for output_failure in [false, true] {
            let healthy = Arc::new(AtomicBool::new(true));
            let (broker, ticket, id) = setup(Duration::from_secs(1), healthy.clone());
            let transitions = Arc::new(AtomicUsize::new(0));
            let changes = transitions.clone();
            let (started, start) = mpsc::channel();
            let control = ProviderControl::new(move |active| {
                changes.fetch_add(1, Ordering::AcqRel);
                if active {
                    started.send(()).unwrap();
                }
                Ok(())
            });
            let worker = std::thread::spawn(move || {
                start.recv().unwrap();
                std::thread::sleep(Duration::from_millis(2));
                if output_failure {
                    healthy.store(false, Ordering::Release);
                } else {
                    broker.abort_active(ended());
                }
            });
            let began = Instant::now();
            assert!(ticket.wait_slice(&id, &control).is_err());
            worker.join().unwrap();
            assert!(began.elapsed() < Duration::from_millis(100));
            assert_eq!(transitions.load(Ordering::Acquire), 2);
        }
    }
    #[test]
    fn expired_prompt_and_inactive_call_never_acquire_a_slice() {
        let (broker, ticket, id) = setup(Duration::from_secs(60), Arc::new(AtomicBool::new(true)));
        let transitions = Arc::new(AtomicUsize::new(0));
        let changes = transitions.clone();
        let control = ProviderControl::new(move |_| {
            changes.fetch_add(1, Ordering::AcqRel);
            Ok(())
        });
        // Expire the acknowledged prompt deterministically. A one-millisecond
        // setup deadline can expire before the fixture consumes its write ACK.
        broker
            .0
            .state
            .lock()
            .unwrap()
            .pending
            .get_mut(&id)
            .unwrap()
            .deadline = Instant::now();
        ticket.wait_slice(&id, &control).unwrap();
        assert_eq!(transitions.load(Ordering::Acquire), 0);
        let (_, ticket, id) = setup(Duration::from_secs(1), Arc::new(AtomicBool::new(true)));
        control.close().unwrap();
        assert!(ticket.wait_slice(&id, &control).is_err());
        assert_eq!(transitions.load(Ordering::Acquire), 0);
    }
}

#[cfg(test)]
mod connection_liveness_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn context(id: u64) -> Context {
        Context {
            request_id: json!(id),
            mcp_initialized: true,
            elicitation_supported: false,
            elicitation_timeout: Duration::from_secs(1),
        }
    }
    fn broker() -> OriginElicitationBroker {
        OriginElicitationBroker::new(
            Arc::new(|_, _| panic!("A liveness probe must not enqueue a prompt")),
            Arc::new(|_, _| false),
            None,
        )
    }

    #[test]
    fn native_connection_liveness_requires_installed_output_health() {
        let broker = broker();
        let ticket = broker.activate(context(1));
        ticket.validate().unwrap();
        assert!(ticket.connection_liveness().is_none());
        assert!(broker.0.state.lock().unwrap().pending.is_empty());
    }

    #[test]
    fn native_connection_liveness_checks_output_without_requesting_approval() {
        let healthy = Arc::new(AtomicBool::new(true));
        let observed = healthy.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let checked = calls.clone();
        let broker = broker().with_output_health(Arc::new(move || {
            checked.fetch_add(1, Ordering::AcqRel);
            if observed.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(Error::action("Owned connection output failed"))
            }
        }));
        let ticket = broker.activate(context(1));
        let liveness = ticket.connection_liveness().unwrap();
        liveness.validate().unwrap();
        healthy.store(false, Ordering::Release);
        assert_eq!(
            liveness.validate().unwrap_err().message,
            "Owned connection output failed"
        );
        assert_eq!(calls.load(Ordering::Acquire), 2);
        // The old Ticket API retains its existing scope-only meaning.
        ticket.validate().unwrap();
        assert!(broker.0.state.lock().unwrap().pending.is_empty());
        broker.disconnect();
        healthy.store(true, Ordering::Release);
        assert!(liveness.validate().is_err());
        assert_eq!(calls.load(Ordering::Acquire), 2);
    }

    #[test]
    fn native_connection_liveness_rechecks_replacement_after_unlocked_health() {
        let holder: Arc<Mutex<Option<OriginElicitationBroker>>> = Arc::new(Mutex::new(None));
        let callback_owner = holder.clone();
        let broker = broker().with_output_health(Arc::new(move || {
            let owner = callback_owner.lock().unwrap().take().unwrap();
            // A mistaken outer broker lock produces an explicit failure,
            // rather than making the reentrant replacement test deadlock.
            drop(
                owner
                    .0
                    .state
                    .try_lock()
                    .map_err(|_| Error::action("Broker state was locked"))?,
            );
            owner.activate(context(2));
            Ok(())
        }));
        let ticket = broker.activate(context(1));
        let liveness = ticket.connection_liveness().unwrap();
        *holder.lock().unwrap() = Some(broker.clone());
        assert_eq!(
            liveness.validate().unwrap_err().message,
            "Origin approval scope ended"
        );
        assert_eq!(
            broker
                .0
                .state
                .lock()
                .unwrap()
                .active
                .as_ref()
                .unwrap()
                .context
                .request_id,
            json!(2)
        );
    }

    #[test]
    fn native_connection_liveness_observes_reader_cancellation_and_finished_ticket() {
        let checks = Arc::new(AtomicUsize::new(0));
        let checked = checks.clone();
        let broker = broker().with_output_health(Arc::new(move || {
            checked.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }));
        let ticket = broker.activate(context(1));
        let liveness = ticket.connection_liveness().unwrap();
        broker.route(
            &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
        );
        assert!(liveness.validate().is_err());
        assert_eq!(checks.load(Ordering::Acquire), 0);
        let next = broker.activate(context(2));
        assert!(liveness.validate().is_err());
        let current = next.connection_liveness().unwrap();
        current.validate().unwrap();
        next.finish();
        assert!(current.validate().is_err());
        assert_eq!(checks.load(Ordering::Acquire), 1);
    }
}
