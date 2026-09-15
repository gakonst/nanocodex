//! Cooperative raw reads. Native cell/policy/connection provenance is checked
//! before polling an existing socket; request JSON supplies only a lookup key.
use super::{
    Browser, Browsers,
    chooser::Owner,
    raw_events::{
        Query,
        waiting::{Identity, Polled, Provenance, Snapshot, Started},
    },
};
use crate::{
    Error, Result,
    engine::string,
    security::{RawWaitPolicy, Security},
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub(super) struct StartAdmission {
    owner: Option<Owner>,
    policy: Option<RawWaitPolicy>,
    guardian: Option<String>,
}
struct Packet<'a> {
    browser: &'a str,
    tab: &'a str,
    id: &'a str,
    cancel: bool,
}
impl<'a> Packet<'a> {
    fn parse(args: &'a Value) -> Result<Self> {
        let object = args
            .as_object()
            .ok_or_else(|| Error::invalid("Raw event continuation must be an object"))?;
        if object.len() != 3
            || object
                .keys()
                .any(|k| !["browser", "tab", "__skyreRawWait"].contains(&k.as_str()))
        {
            return Err(Error::invalid(
                "Raw event continuation has unexpected fields",
            ));
        }
        let packet = args["__skyreRawWait"]
            .as_object()
            .ok_or_else(|| Error::invalid("Invalid raw event continuation"))?;
        if packet.len() != 2 || packet.keys().any(|k| k != "id" && k != "op") {
            return Err(Error::invalid("Invalid raw event continuation fields"));
        }
        let id = string(&args["__skyreRawWait"], "id")?;
        if !id.strip_prefix("raw-events-").is_some_and(|v| {
            v.len() == 64
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(Error::invalid("Invalid raw event continuation ID"));
        }
        let cancel = match args["__skyreRawWait"]["op"].as_str() {
            Some("poll") => false,
            Some("cancel") => true,
            _ => return Err(Error::invalid("Unknown raw event continuation operation")),
        };
        let browser = string(args, "browser")?;
        let tab = string(args, "tab")?;
        if browser.is_empty() || tab.is_empty() {
            return Err(Error::invalid("Raw event continuation binding is empty"));
        }
        Ok(Self {
            browser,
            tab,
            id,
            cancel,
        })
    }
}
fn ended(message: &str) -> Error {
    Error::new(-32800, message)
}
fn pending(id: &str) -> Value {
    json!({"__skyreRawWait":{"id":id}})
}
fn complete(value: &Value) -> bool {
    value["truncated"] == true || value["events"].as_array().is_some_and(|v| !v.is_empty())
}
impl Browsers {
    pub(crate) fn execute_raw_events_admitted(
        &mut self,
        args: &Value,
        scope: &str,
        policy: Option<RawWaitPolicy>,
        guardian: Option<&str>,
    ) -> Result<Value> {
        let owner = self
            .chooser_owner
            .clone()
            .filter(|owner| owner.scope == scope);
        self.execute_normalized_with_raw_admission(
            "cdp_events",
            args,
            Some(StartAdmission {
                owner,
                policy,
                guardian: guardian.map(str::to_owned),
            }),
        )
    }
    pub(crate) fn continue_raw_wait(
        &mut self,
        args: &Value,
        scope: &str,
        security: &Security,
        guardian: Option<&str>,
    ) -> Result<Value> {
        let packet = Packet::parse(args)?;
        let owner = self
            .chooser_owner
            .clone()
            .filter(|owner| owner.scope == scope)
            .ok_or_else(|| ended("Raw event wait has no active cell in this scope"))?;
        let snapshot = self.validate_raw_wait(&packet, &owner, security, guardian)?;
        if packet.cancel {
            self.providers
                .get_mut(packet.browser)
                .unwrap()
                .surface
                .raw_events
                .lock()
                .unwrap()
                .waits
                .cancel(packet.id, &snapshot.identity)?;
            return Ok(Value::Null);
        }
        if !snapshot.terminal {
            let browser = self.providers.get_mut(packet.browser).unwrap();
            browser.raw_wait_timer(packet.id);
            let now_terminal = browser
                .surface
                .raw_events
                .lock()
                .unwrap()
                .waits
                .snapshot(packet.id, &owner, packet.tab)?
                .terminal;
            if !now_terminal && let Err(error) = browser.poll_raw_existing() {
                // No reconnect/retry; this owner cannot trust further data
                // from a failed event stream, including frozen packets.
                browser.client = None;
                browser.sessions.clear();
                browser.invalidated = true;
                if let Some(authority) = &mut browser.iab {
                    authority.invalidate_inputs();
                }
                browser.surface.disconnect();
                browser.contract.disconnect();
                return Err(error);
            }
        }
        // Re-read native lifecycle state after receipt callbacks, not only the
        // pre-poll copy. A callback may retire a tab or its provider context.
        let Some(current_owner) = self
            .chooser_owner
            .clone()
            .filter(|current| current == &owner && current.scope == scope)
        else {
            self.cancel_raw_wait_scope(&owner.scope, Some(owner.cell));
            return Err(ended("Raw event wait cell changed during receipt"));
        };
        let owner = current_owner;
        let snapshot = self.validate_raw_wait(&packet, &owner, security, guardian)?;
        let browser = self.providers.get_mut(packet.browser).unwrap();
        browser.raw_wait_timer(packet.id);
        match browser
            .surface
            .raw_events
            .lock()
            .unwrap()
            .waits
            .poll(packet.id, &snapshot.identity)?
        {
            Polled::Pending => Ok(pending(packet.id)),
            Polled::Ready(result) => result,
        }
    }
    fn validate_raw_wait(
        &mut self,
        packet: &Packet<'_>,
        owner: &Owner,
        security: &Security,
        guardian: Option<&str>,
    ) -> Result<Snapshot> {
        let browser = self
            .providers
            .get(packet.browser)
            .ok_or_else(|| ended("Raw event browser owner ended"))?;
        // Unknown/wrong-cell IDs cannot cause activity or retire another wait.
        let snapshot = browser
            .surface
            .raw_events
            .lock()
            .unwrap()
            .waits
            .snapshot(packet.id, owner, packet.tab)?;
        let checked = (|| {
            if !security.validates_raw_wait(&snapshot.provenance.policy)
                || snapshot.provenance.guardian.as_deref() != guardian
            {
                return Err(ended("Raw event wait policy or control authority changed"));
            }
            if !self.host_visible(browser) {
                return Err(ended("Raw event wait host route ended"));
            }
            browser.ensure_context()?;
            if browser.client.is_none()
                || browser.authorization_connection != snapshot.provenance.connection
            {
                return Err(ended("Raw event wait connection changed"));
            }
            if !snapshot.terminal {
                if let Some(authority) = &browser.iab {
                    authority.require_tab(packet.tab)?;
                }
                let attachment = browser
                    .surface
                    .raw_events
                    .lock()
                    .unwrap()
                    .attachment(packet.tab);
                if attachment.as_ref().is_none_or(|(generation, session)| {
                    *generation != snapshot.identity.generation
                        || session != &snapshot.provenance.session
                }) || browser.sessions.get(packet.tab) != Some(&snapshot.provenance.session)
                {
                    return Err(ended("Raw event wait attachment changed"));
                }
            }
            Ok(())
        })();
        if let Err(error) = checked {
            browser
                .surface
                .raw_events
                .lock()
                .unwrap()
                .waits
                .cancel(packet.id, &snapshot.identity)?;
            return Err(error);
        }
        Ok(snapshot)
    }
    pub(crate) fn tick_raw_wait_timers(
        &mut self,
        scope: &str,
        security: &Security,
        guardian: Option<&str>,
    ) {
        let Some(owner) = self
            .chooser_owner
            .clone()
            .filter(|owner| owner.scope == scope)
        else {
            self.cancel_all_raw_waits();
            return;
        };
        let ids: Vec<_> = self
            .providers
            .iter()
            .flat_map(|(browser, provider)| {
                provider
                    .surface
                    .raw_events
                    .lock()
                    .unwrap()
                    .waits
                    .entries()
                    .into_iter()
                    .map(|(id, identity)| (browser.clone(), id, identity))
                    .collect::<Vec<_>>()
            })
            .collect();
        for (browser, id, identity) in ids {
            if identity.owner != owner {
                if let Some(browser) = self.providers.get(&browser) {
                    let _ = browser
                        .surface
                        .raw_events
                        .lock()
                        .unwrap()
                        .waits
                        .cancel(&id, &identity);
                }
                continue;
            }
            let packet = Packet {
                browser: &browser,
                tab: &identity.tab,
                id: &id,
                cancel: false,
            };
            let _ = self.validate_raw_wait(&packet, &owner, security, guardian);
        }
        for browser in self.providers.values_mut() {
            browser.raw_wait_timers();
        }
    }
    pub(crate) fn cancel_raw_wait_scope(&mut self, scope: &str, cell: Option<u64>) {
        for browser in self.providers.values_mut() {
            browser
                .surface
                .raw_events
                .lock()
                .unwrap()
                .waits
                .cancel_owner(scope, cell);
        }
    }
    pub(crate) fn cancel_all_raw_waits(&mut self) {
        for browser in self.providers.values_mut() {
            browser.cancel_raw_waits();
        }
    }
}
impl Browser {
    pub(super) fn cancel_raw_waits(&mut self) {
        self.surface.raw_events.lock().unwrap().waits.clear();
    }
    fn raw_wait_timer(&mut self, id: &str) {
        self.surface
            .raw_events
            .lock()
            .unwrap()
            .with_waits(|waits, log| {
                waits.timer(id, Instant::now(), |tab, after, query| {
                    log.read(tab, after, query)
                });
            });
    }
    fn raw_wait_timers(&mut self) {
        let mut log = self.surface.raw_events.lock().unwrap();
        if log.waits.is_waiting() {
            log.with_waits(|waits, log| {
                waits.timers(Instant::now(), |tab, after, query| {
                    log.read(tab, after, query)
                })
            });
        }
    }
    /// Same receipt observers and bounded Cdp poll, without reconnecting or
    /// starting trailing chooser/download maintenance. Receipt callbacks retain
    /// their independent native protocol authority and may send responses.
    fn poll_raw_existing(&mut self) -> Result<()> {
        let result = self
            .client
            .as_mut()
            .ok_or_else(|| ended("Raw event connection ended"))?
            .poll_events();
        if let Some(client) = self.client.as_mut() {
            for (event, internal) in client.drain_events() {
                self.iab_event(&event);
                self.contract.event(&event);
                self.surface.event(event, &mut self.sessions, internal);
            }
        }
        result
    }
    pub(super) fn raw_events_start(
        &mut self,
        args: &Value,
        admission: Option<StartAdmission>,
    ) -> Result<Value> {
        if args.get("__skyreRawWait").is_some() {
            return Err(ended("Raw event continuation requires native admission"));
        }
        self.ensure_context()?;
        let tab = string(args, "tab")?;
        let query = Query::native(args)?;
        let timeout = args
            .get("timeoutMs")
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0)
                    .ok_or_else(|| Error::invalid("CDP timeoutMs must be a nonnegative integer"))
            })
            .transpose()?;
        let before = self.surface.raw_events.lock().unwrap().cursor(tab) as f64;
        self.page(tab)?;
        let after = query.after.unwrap_or_else(|| {
            if timeout.is_some() {
                before
            } else {
                self.surface.raw_events.lock().unwrap().cursor(tab) as f64
            }
        });
        self.poll_events()?;
        let value = self
            .surface
            .raw_events
            .lock()
            .unwrap()
            .read(tab, after, &query);
        if complete(&value) || timeout.is_none_or(|ms| ms == 0.0) {
            return Ok(value);
        }
        let admission = admission
            .ok_or_else(|| Error::unsupported("Pending raw reads require native cell admission"))?;
        let owner = admission
            .owner
            .ok_or_else(|| Error::unsupported("Pending raw reads require an active native cell"))?;
        let policy = admission.policy.ok_or_else(|| {
            Error::unsupported("Pending raw reads are unavailable under restricted origin policy")
        })?;
        let ms = timeout.unwrap();
        if ms > i32::MAX as f64 {
            return Err(Error::unsupported(
                "Raw event timeout is outside the supported native timer range",
            ));
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(ms as u64))
            .ok_or_else(|| {
                Error::unsupported("Raw event timeout cannot be represented by the native clock")
            })?;
        self.ensure_context()?;
        if self.client.is_none() {
            return Err(ended("Raw event connection ended before registration"));
        }
        let mut log = self.surface.raw_events.lock().unwrap();
        let (generation, session) = log
            .attachment(tab)
            .ok_or_else(|| ended("Raw event attachment ended before registration"))?;
        if self.sessions.get(tab) != Some(&session) {
            return Err(ended("Raw event attachment changed before registration"));
        }
        let identity = Identity {
            owner,
            tab: tab.into(),
            generation,
        };
        let provenance = Provenance {
            policy,
            guardian: admission.guardian,
            connection: self.authorization_connection.clone(),
            session,
        };
        match log.with_waits(|waits, log| {
            waits.start_native(
                identity,
                provenance,
                after,
                query,
                deadline,
                |tab, after, query| log.read(tab, after, query),
            )
        })? {
            Started::Ready(value) => Ok(value),
            Started::Pending(id) => Ok(pending(&id)),
        }
    }
}
