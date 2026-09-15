//! Private transport broker for a download approval inside a synchronous CDP call.
//! It cannot dispatch Engine work, grant permission, or be reached from JavaScript.
use crate::{Error, Result, protocol, runtime::ProviderControl};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub type Emit = Arc<dyn Fn(&Value, Instant) -> Result<()> + Send + Sync>;
pub type ResetPredicate = Arc<dyn Fn(&Value, bool) -> bool + Send + Sync>;
pub type TransitionPredicate = Arc<dyn Fn(&Value) -> bool + Send + Sync>;

#[derive(Clone)]
pub struct Context {
    pub request_id: Value,
    pub mcp_initialized: bool,
    pub elicitation_supported: bool,
    pub elicitation_timeout: Duration,
}

struct Active {
    token: Arc<()>,
    context: Context,
    aborted: Option<Error>,
    provider: Option<(Arc<()>, ProviderControl)>,
}
struct Pending {
    id: Value,
    token: Arc<()>,
    deadline: Instant,
    reply: mpsc::SyncSender<Result<Value>>,
}
#[derive(Default)]
struct State {
    disconnected: bool,
    active: Option<Active>,
    pending: Option<Pending>,
}
struct Inner {
    state: Mutex<State>,
    emit: Emit,
    reset: ResetPredicate,
    transition: Option<TransitionPredicate>,
}

#[derive(Clone)]
pub struct DownloadElicitationBroker(Arc<Inner>);
/// A host-created immutable cell identity. Cloning never changes its authority.
#[derive(Clone)]
pub struct Ticket {
    broker: DownloadElicitationBroker,
    token: Arc<()>,
    suspended: Arc<Mutex<Duration>>,
}

/// A main-thread binding to exactly one active provider call.
pub struct ProviderBinding {
    ticket: Ticket,
    token: Arc<()>,
}
impl Drop for ProviderBinding {
    fn drop(&mut self) {
        let mut state = self.ticket.broker.0.state.lock().unwrap();
        if let Some(active) = state.active.as_mut()
            && Arc::ptr_eq(&active.token, &self.ticket.token)
            && active
                .provider
                .as_ref()
                .is_some_and(|(token, _)| Arc::ptr_eq(token, &self.token))
        {
            active.provider = None;
        }
    }
}

fn expired() -> Error {
    Error::new(-32008, "Download approval timed out")
}
fn ended() -> Error {
    Error::new(-32800, "Download approval scope ended")
}
impl State {
    fn abort(&mut self, error: Error) {
        if let Some(active) = self.active.as_mut() {
            active.aborted.get_or_insert(error.clone());
        }
        if let Some(pending) = self.pending.take() {
            let _ = pending.reply.try_send(Err(error));
        }
    }
    fn context(&self, token: &Arc<()>) -> Result<&Context> {
        if self.disconnected {
            return Err(Error::action("Elicitation client disconnected"));
        }
        let active = self
            .active
            .as_ref()
            .filter(|active| Arc::ptr_eq(&active.token, token))
            .ok_or_else(ended)?;
        if let Some(error) = &active.aborted {
            return Err(error.clone());
        }
        Ok(&active.context)
    }
}

impl DownloadElicitationBroker {
    pub fn new(emit: Emit, reset: ResetPredicate, transition: Option<TransitionPredicate>) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            emit,
            reset,
            transition,
        }))
    }
    /// Main transport only: begin a cell and invalidate any previous cell.
    pub fn activate(&self, context: Context) -> Ticket {
        let token = Arc::new(());
        let mut state = self.0.state.lock().unwrap();
        state.abort(ended());
        state.active = Some(Active {
            token: token.clone(),
            context,
            aborted: None,
            provider: None,
        });
        Ticket {
            broker: self.clone(),
            token,
            suspended: Arc::new(Mutex::new(Duration::ZERO)),
        }
    }
    pub fn abort_active(&self, error: Error) {
        self.0.state.lock().unwrap().abort(error);
    }
    pub fn disconnect(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.disconnected = true;
        state.abort(Error::action("Elicitation client disconnected"));
    }
    /// Reader only. Matching replies are consumed. Control requests revoke the
    /// current admission but return false so the main transport still handles them.
    pub fn route(&self, message: &Value) -> bool {
        let active = {
            let mut state = self.0.state.lock().unwrap();
            if message.get("method").is_none()
                && state
                    .pending
                    .as_ref()
                    .is_some_and(|pending| message.get("id") == Some(&pending.id))
            {
                let pending = state.pending.take().unwrap();
                let result = if Instant::now() >= pending.deadline {
                    Err(expired())
                } else {
                    state.context(&pending.token).and_then(|_| {
                        protocol::validate_response(message)?;
                        if let Some(error) = message.get("error") {
                            return Err(Error::new(-32004, error["message"].as_str().unwrap()));
                        }
                        Ok(message["result"].clone())
                    })
                };
                let _ = pending.reply.try_send(result);
                return true;
            }
            state
                .active
                .as_ref()
                .map(|active| (active.token.clone(), active.context.clone()))
        };
        let Some((token, context)) = active else {
            return false;
        };
        if protocol::validate_request(message).is_err() {
            return false;
        }
        let reason = if message["method"] == "notifications/cancelled"
            && message.get("id").is_none()
            && message["params"].get("requestId") == Some(&context.request_id)
        {
            Some("Evaluation cancelled")
        } else if (self.0.reset)(message, context.mcp_initialized) {
            Some("Evaluation cancelled by kernel reset")
        } else if matches!(
            message["method"].as_str(),
            Some("host/turn" | "host/recover")
        ) && self
            .0
            .transition
            .as_ref()
            .is_some_and(|check| check(message))
        {
            Some("Evaluation cancelled by trusted host transition")
        } else {
            None
        };
        if let Some(reason) = reason {
            let mut state = self.0.state.lock().unwrap();
            if state
                .active
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(&active.token, &token))
            {
                state.abort(Error::new(-32800, reason));
            }
        }
        false
    }
}

impl Ticket {
    /// A pending attachment can outlive the CDP click acknowledgement. Retain
    /// it during idle maintenance until an active provider call parks model JS.
    /// A revoked cell or expired control is an error, not indefinite deferral.
    pub fn provider_ready(&self) -> Result<bool> {
        let state = self.broker.0.state.lock().unwrap();
        state.context(&self.token)?;
        match &state.active.as_ref().unwrap().provider {
            None => Ok(false),
            Some((_, control)) if control.is_active() => Ok(true),
            Some(_) => Err(ended()),
        }
    }
    pub fn bind_provider(&self, control: ProviderControl) -> Result<ProviderBinding> {
        let mut state = self.broker.0.state.lock().unwrap();
        state.context(&self.token)?;
        let active = state.active.as_mut().unwrap();
        if active.provider.is_some() {
            return Err(Error::action("Download provider call already bound"));
        }
        let token = Arc::new(());
        active.provider = Some((token.clone(), control));
        Ok(ProviderBinding {
            ticket: self.clone(),
            token,
        })
    }
    /// Trusted accounting only; no response metadata can alter this ledger.
    pub fn suspended_duration(&self) -> Result<Duration> {
        self.suspended
            .lock()
            .map(|value| *value)
            .map_err(|_| Error::action("Download suspension accounting unavailable"))
    }
    fn record_suspension(&self, elapsed: Duration) -> Result<()> {
        let mut total = self
            .suspended
            .lock()
            .map_err(|_| Error::action("Download suspension accounting unavailable"))?;
        *total = total
            .checked_add(elapsed)
            .ok_or_else(|| Error::action("Download suspension accounting overflow"))?;
        Ok(())
    }
    /// Check the originating cell even when a host policy has a cached grant.
    pub fn validate(&self) -> Result<()> {
        self.broker
            .0
            .state
            .lock()
            .unwrap()
            .context(&self.token)
            .map(|_| ())
    }
    /// The emitter must bound its own queue/write acknowledgement by `deadline`.
    /// No broker lock spans the emitter or response wait, and expired results
    /// cannot authorize continuation even if a transport write finishes late.
    pub fn request(&self, canonical_params: Value, deadline: Instant) -> Result<Value> {
        let (context, control) = {
            let state = self.broker.0.state.lock().unwrap();
            let context = state.context(&self.token)?.clone();
            let control = state
                .active
                .as_ref()
                .unwrap()
                .provider
                .as_ref()
                .map(|(_, control)| control.clone());
            (context, control)
        };
        if !context.mcp_initialized || !context.elicitation_supported {
            return Err(Error::unsupported(
                "Browser Use requires a host that supports elicitations",
            ));
        }
        if context.elicitation_timeout.is_zero() {
            return Err(Error::invalid("elicitation_timeout_ms must be positive"));
        }
        let host_deadline = Instant::now()
            .checked_add(context.elicitation_timeout)
            .ok_or_else(|| Error::invalid("Elicitation timeout exceeds the clock range"))?;
        if Instant::now() >= deadline {
            return Err(expired());
        }
        let caller_deadline = deadline;
        // Only a real, call-bound runtime control can exclude approval waiting
        // from the caller's network budget. Standalone brokers retain the bound.
        let deadline = if control.is_some() {
            host_deadline
        } else {
            deadline.min(host_deadline)
        };
        let mut params = canonical_params;
        let fields = params
            .as_object_mut()
            .ok_or_else(|| Error::invalid("Elicitation params must be an object"))?;
        fields.insert(
            "requestedSchema".into(),
            json!({"type":"object","properties":{}}),
        );
        if let Some(meta) = fields.remove("meta") {
            fields.insert("_meta".into(), meta);
        }
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce)
            .map_err(|_| Error::action("Cannot generate an elicitation request ID"))?;
        let id = Value::String(format!(
            "skyre-download-elicitation-{}",
            nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        let outgoing =
            json!({"jsonrpc":"2.0","id":id,"method":"elicitation/create","params":params});
        if serde_json::to_vec(&outgoing)?.len() > protocol::MAX_FRAME {
            return Err(Error::invalid("Elicitation request exceeds 8 MiB"));
        }
        let (reply, receive) = mpsc::sync_channel(1);
        {
            let mut state = self.broker.0.state.lock().unwrap();
            state.context(&self.token)?;
            if state.pending.is_some() {
                return Err(Error::action("Download elicitation already pending"));
            }
            if Instant::now() >= deadline {
                return Err(expired());
            }
            state.pending = Some(Pending {
                id: id.clone(),
                token: self.token.clone(),
                deadline,
                reply,
            });
        }
        let result = (|| {
            let guard = control.as_ref().map(ProviderControl::suspend).transpose()?;
            let result = (|| {
                self.validate()?;
                if Instant::now() >= caller_deadline {
                    return Err(expired());
                }
                (self.broker.0.emit)(&outgoing, deadline)?;
                loop {
                    self.validate()?;
                    if control.as_ref().is_some_and(|control| !control.is_active()) {
                        return Err(ended());
                    }
                    let remaining = deadline
                        .checked_duration_since(Instant::now())
                        .ok_or_else(expired)?;
                    match receive.recv_timeout(remaining.min(Duration::from_millis(20))) {
                        Ok(result) => break result,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            return Err(Error::action("Elicitation response channel closed"));
                        }
                    }
                }
            })();
            if let Some(guard) = guard {
                let elapsed_before_resume = guard.elapsed();
                let resumed = guard.resume();
                self.record_suspension(resumed.as_ref().copied().unwrap_or(elapsed_before_resume))?;
                resumed?;
            }
            result
        })();
        let mut state = self.broker.0.state.lock().unwrap();
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
        {
            state.pending = None;
        }
        state.context(&self.token)?;
        if Instant::now() >= deadline {
            return Err(expired());
        }
        result
    }
    /// Finishing an old ticket never invalidates a newly selected cell.
    pub fn finish(&self) {
        let mut state = self.broker.0.state.lock().unwrap();
        if state
            .active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.token, &self.token))
        {
            state.abort(ended());
            state.active = None;
        }
    }
}
