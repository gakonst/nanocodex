//! A native, one-operation before-unload sender. Stored replies confer no authority.
use super::{Browser, Cdp, dialog, raw_events};
use crate::{Error, Result, origin_elicitation::ConnectionLiveness, runtime::ExecutionValidity};
use serde_json::Value;
use std::{
    cell::Cell,
    collections::BTreeMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_PENDING: usize = 16;
const MAX_BYTES: usize = 8192;
const MAX_ID: usize = 256;
const ACK_LIFETIME: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Navigate,
    Close,
}

/// Constructed by the Engine only after its ordinary normalized admission.
/// The native capabilities are not reconstructible from request JSON.
pub(crate) struct RuntimeAdmission {
    browser: String,
    tab: String,
    kind: Kind,
    execution: ExecutionValidity,
    connection: ConnectionLiveness,
}
impl RuntimeAdmission {
    pub(crate) fn new(
        browser: &str,
        tab: &str,
        kind: Kind,
        execution: ExecutionValidity,
        connection: ConnectionLiveness,
    ) -> Option<Self> {
        if [browser, tab]
            .iter()
            .any(|id| id.is_empty() || id.len() > MAX_ID)
        {
            return None;
        }
        Some(Self {
            browser: browser.into(),
            tab: tab.into(),
            kind,
            execution,
            connection,
        })
    }
    fn validate(&self) -> Result<()> {
        self.connection.validate()?;
        self.execution.validate()
    }
}

struct Revocation {
    tab: String,
    session: String,
    live: AtomicBool,
}
/// Never cloned or stored on Cdp. Only a scoped native call borrows this object.
pub(super) struct Operation<'a> {
    runtime: &'a RuntimeAdmission,
    connection: Arc<()>,
    generation: u64,
    session: String,
    revoked: Arc<Revocation>,
    started_deadline: Cell<Option<Instant>>,
}
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        self.revoked.live.store(false, Ordering::Release);
    }
}
impl Operation<'_> {
    pub(super) fn kind(&self) -> Kind {
        self.runtime.kind
    }
    pub(super) fn tab(&self) -> &str {
        &self.runtime.tab
    }
    pub(super) fn dispatched(&self, method: &str, deadline: Instant) {
        if self.started_deadline.get().is_none()
            && matches!(
                (self.runtime.kind, method),
                (Kind::Navigate, "Page.navigate") | (Kind::Close, "Target.closeTarget")
            )
        {
            self.started_deadline.set(Some(deadline));
        }
    }
    fn allows_command(&self, method: &str, params: &Value, session: Option<&str>) -> bool {
        match self.runtime.kind {
            Kind::Navigate => {
                self.started_deadline.get().is_none()
                    && method == "Page.navigate"
                    && session == Some(self.session.as_str())
            }
            Kind::Close => {
                session.is_none()
                    && (self.started_deadline.get().is_none()
                        && method == "Target.closeTarget"
                        && params["targetId"] == self.runtime.tab
                        || self.started_deadline.get().is_some() && method == "Target.getTargets")
            }
        }
    }
}
struct Pending {
    session: String,
    expires: Instant,
}
/// A revocation observer and bounded ACK-disposal table, not an ambient grant.
#[derive(Default)]
pub(super) struct State {
    connection: Arc<()>,
    watchers: Vec<Weak<Revocation>>,
    pending: BTreeMap<u64, Pending>,
}
pub(super) struct Fresh {
    tab: String,
    session: String,
    id: String,
}
impl State {
    fn prune(&mut self, now: Instant) {
        self.pending.retain(|_, pending| now < pending.expires);
        self.watchers.retain(|watcher| {
            watcher
                .upgrade()
                .is_some_and(|watcher| watcher.live.load(Ordering::Acquire))
        });
    }
    fn bind<'a>(
        &mut self,
        runtime: &'a RuntimeAdmission,
        generation: u64,
        session: String,
    ) -> Option<Operation<'a>> {
        self.prune(Instant::now());
        if session.is_empty()
            || session.len() > MAX_ID
            || self.watchers.len() >= MAX_PENDING
            || runtime.validate().is_err()
        {
            return None;
        }
        let revoked = Arc::new(Revocation {
            tab: runtime.tab.clone(),
            session: session.clone(),
            live: AtomicBool::new(true),
        });
        self.watchers.push(Arc::downgrade(&revoked));
        Some(Operation {
            runtime,
            connection: self.connection.clone(),
            generation,
            session,
            revoked,
            started_deadline: Cell::new(None),
        })
    }
    pub(super) fn observe(&mut self, context: &raw_events::Context, event: &Value) {
        self.prune(Instant::now());
        for watcher in self.watchers.iter().filter_map(Weak::upgrade) {
            let same_root = context.source.as_ref().is_some_and(|source| {
                source.top_level
                    && source.tab == watcher.tab
                    && source.source["sessionId"] == watcher.session
            });
            let replaced = same_root
                && match event["method"].as_str() {
                    Some("Page.frameNavigated") => event["params"]["frame"]
                        .get("parentId")
                        .is_none_or(Value::is_null),
                    // Conservatively refuse after any root-session context replacement
                    // or frame retirement. Child-source records cannot revoke another root.
                    Some(
                        "Runtime.executionContextsCleared"
                        | "Runtime.executionContextCreated"
                        | "Runtime.executionContextDestroyed"
                        | "Page.frameDetached",
                    ) => true,
                    _ => false,
                };
            if replaced || context.discard.as_deref() == Some(watcher.tab.as_str()) {
                watcher.live.store(false, Ordering::Release);
            }
        }
    }
    pub(super) fn fresh(
        dialogs: &dialog::State,
        context: &raw_events::Context,
        event: &Value,
        previous: Option<&str>,
    ) -> Option<Fresh> {
        if event["method"] != "Page.javascriptDialogOpening"
            || event["params"]["type"] != "beforeunload"
        {
            return None;
        }
        let source = context.source.as_ref().filter(|source| source.top_level)?;
        let session = source.source["sessionId"].as_str()?;
        let id = dialogs.beforeunload_id(&source.tab, session)?;
        if previous == Some(id) {
            return None;
        }
        Some(Fresh {
            tab: source.tab.clone(),
            session: session.into(),
            id: id.into(),
        })
    }
    fn valid(
        &self,
        operation: &Operation<'_>,
        fresh: &Fresh,
        dialogs: &dialog::State,
        attachment: Option<(u64, String)>,
    ) -> bool {
        Arc::ptr_eq(&self.connection, &operation.connection)
            && operation.revoked.live.load(Ordering::Acquire)
            && fresh.tab == operation.runtime.tab
            && fresh.session == operation.session
            && attachment == Some((operation.generation, operation.session.clone()))
            && dialogs.beforeunload_id(&fresh.tab, &fresh.session) == Some(fresh.id.as_str())
    }
    fn room(&mut self, session: &str, now: Instant) -> bool {
        self.prune(now);
        self.pending.len() < MAX_PENDING
            && self
                .pending
                .values()
                .map(|entry| entry.session.len() + 32)
                .sum::<usize>()
                + session.len()
                + 32
                <= MAX_BYTES
    }
    /// Matching success/error ACKs consume only this private metadata. A wrong
    /// session does not consume the expected ACK and never reaches a dialog watch.
    pub(super) fn discard_reply(&mut self, value: &Value) -> bool {
        self.prune(Instant::now());
        let Some(id) = value["id"].as_u64() else {
            return false;
        };
        let Some(pending) = self.pending.get(&id) else {
            return false;
        };
        if value["sessionId"].as_str() == Some(pending.session.as_str()) {
            self.pending.remove(&id);
        }
        true
    }
}

impl Browser {
    pub(super) fn beforeunload_operation<'a>(
        &mut self,
        browser: &str,
        tab: &str,
        runtime: &'a RuntimeAdmission,
    ) -> Option<Operation<'a>> {
        if runtime.browser != browser
            || runtime.tab != tab
            || self.extension
            || self.iab.is_some()
            || self.host_binding.is_some()
            || self.durable.is_some()
            || self.invalidated
            || self.ensure_context().is_err()
        {
            return None;
        }
        // Existing ownership only. Neither this admission nor its sender attaches,
        // polls a selector, starts a provider, or reconnects a channel.
        let client = self.client.as_mut()?;
        let (generation, session) = self
            .surface
            .raw_events
            .lock()
            .unwrap()
            .attachment(&runtime.tab)?;
        if self.sessions.get(&runtime.tab) != Some(&session)
            || !self
                .surface
                .dialogs
                .lock()
                .unwrap()
                .root_matches(&runtime.tab, &session)
        {
            return None;
        }
        client.beforeunload.bind(runtime, generation, session)
    }
}
impl Cdp {
    pub(super) fn beforeunload_command_valid(
        &self,
        operation: &Operation<'_>,
        method: &str,
        params: &Value,
        session: Option<&str>,
    ) -> bool {
        operation.allows_command(method, params, session)
            && Arc::ptr_eq(&self.beforeunload.connection, &operation.connection)
            && operation.revoked.live.load(Ordering::Acquire)
            && self.raw_events.lock().unwrap().attachment(operation.tab())
                == Some((operation.generation, operation.session.clone()))
            && self
                .dialogs
                .lock()
                .unwrap()
                .root_matches(operation.tab(), &operation.session)
    }
    pub(super) fn send_beforeunload(
        &mut self,
        operation: &Operation<'_>,
        fresh: Fresh,
        deadline: Instant,
    ) -> Result<()> {
        let Some(started_deadline) = operation.started_deadline.get() else {
            return Ok(());
        };
        let deadline = deadline.min(started_deadline);
        if operation.runtime.validate().is_err() {
            return Ok(());
        }
        // No callback is invoked with dialog, raw-event, or broker state locked.
        let attachment = self.raw_events.lock().unwrap().attachment(&fresh.tab);
        if !self
            .beforeunload
            .valid(operation, &fresh, &self.dialogs.lock().unwrap(), attachment)
        {
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline || !self.beforeunload.room(&fresh.session, now) {
            return Ok(());
        }
        let expires = deadline.min(now + ACK_LIFETIME);
        let id = super::allocate_request_id(&mut self.next)?;
        let request = serde_json::json!({"id":id,"sessionId":fresh.session,"method":"Page.handleJavaScriptDialog","params":{"accept":true}}).to_string();
        if let tungstenite::stream::MaybeTlsStream::Plain(stream) = self.socket.get_ref() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            stream.set_write_timeout(Some(Duration::from_millis(
                remaining.as_millis().max(1) as u64
            )))?;
        }
        // Re-read the native clock immediately before the one direct send. This
        // creates no suspension, nested CDP request, retry, or ACK wait.
        if operation.runtime.validate().is_err() {
            return Ok(());
        }
        let attachment = self.raw_events.lock().unwrap().attachment(&fresh.tab);
        if !self
            .beforeunload
            .valid(operation, &fresh, &self.dialogs.lock().unwrap(), attachment)
        {
            return Ok(());
        }
        if operation.runtime.execution.validate().is_err() || Instant::now() >= deadline {
            return Ok(());
        }
        self.beforeunload.pending.insert(
            id,
            Pending {
                session: fresh.session,
                expires,
            },
        );
        if let Err(error) = self.socket.send(tungstenite::Message::text(request)) {
            self.beforeunload.pending.remove(&id);
            return Err(Error::new(-32006, error.to_string()));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "browser_beforeunload_tests.rs"]
mod tests;
