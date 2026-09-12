//! Durable IAB authority journal. Persist intent before dispatch and captured
//! resource identities before returning its acknowledgement. Input is never replayed.
use super::{Browser, Browsers, iab::Authority, persistence::Store};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
#[derive(Clone, Deserialize, Serialize)]
struct Operation {
    id: u64,
    method: String,
}
#[derive(Deserialize, Serialize)]
struct Resolution {
    operation: u64,
    context: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    binding: String,
    authority: Value,
    host: Value,
    next: u64,
    pending: Option<Operation>,
    uncertain: Vec<Operation>,
    #[serde(default)]
    resolutions: Vec<Resolution>,
}
pub(super) struct Journal {
    store: Store,
    record: Record,
    recovered: bool,
}
fn binding(browser: &Browser, authority: &Authority) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{}\n{}",
                browser.endpoint,
                serde_json::to_string(&authority.route).unwrap()
            )
            .as_bytes()
        )
    )
}
impl Browser {
    pub(super) fn durable_save(&mut self) -> Result<()> {
        let Some(journal) = self.durable.as_mut() else {
            return Ok(());
        };
        if let Some(authority) = &self.iab {
            journal.record.authority = serde_json::to_value(authority)?;
        }
        journal.record.host = serde_json::to_value(&self.iab_host)?;
        journal.store.save(&journal.record)
    }
    pub(super) fn durable_before(&mut self, method: &str) -> Result<()> {
        // A failed prior fsync must not be bypassed by a subsequent command.
        // Commit the latest captured resource identity before replacing its intent.
        self.durable_save()?;
        if self.durable.is_some() {
            self.iab_host.observe_intent(method);
        }
        let Some(journal) = self.durable.as_mut() else {
            return Ok(());
        };
        if journal
            .record
            .pending
            .as_ref()
            .is_some_and(|pending| pending.method == "Target.createBrowserContext")
            && self.iab_host.context_id().is_none()
        {
            return Err(Error::action(
                "IAB context creation acknowledgement is unknown; explicit trusted host recovery is required",
            ));
        }
        if let Some(previous) = journal.record.pending.take() {
            journal.record.uncertain.push(previous);
            if journal.record.uncertain.len() > 128 {
                journal.record.uncertain.remove(0);
            }
        }
        journal.record.next = journal
            .record
            .next
            .checked_add(1)
            .ok_or_else(|| Error::action("Browser durable operation counter exhausted"))?;
        journal.record.pending = Some(Operation {
            id: journal.record.next,
            method: method.into(),
        });
        self.durable_save()
    }
    pub(super) fn durable_after(&mut self, method: &str, result: &Result<Value>) -> Result<()> {
        if self.durable.is_none() {
            return Ok(());
        }
        if let Ok(value) = result {
            self.iab_host.observe_result(method, value);
        } else if let Err(error) = result {
            self.iab_host.observe_failure(method, error);
        }
        let journal = self.durable.as_mut().unwrap();
        if !result.as_ref().is_err_and(|error| error.code == -32006) {
            journal.record.pending = None;
        }
        self.durable_save()
    }
}
impl Browsers {
    /// Host-only registration: never exposed through browser RPC or model facade.
    pub fn enable_iab_recovery(&mut self, id: &str, directory: &Path) -> Result<Value> {
        let browser = self
            .providers
            .get_mut(id)
            .ok_or_else(|| Error::action("IAB route not found"))?;
        let authority = browser
            .iab
            .as_ref()
            .ok_or_else(|| Error::invalid("Durable IAB recovery requires an IAB route"))?;
        if browser.durable.is_some() {
            return Err(Error::invalid("IAB recovery already configured"));
        }
        let expected = binding(browser, authority);
        let store = Store::open(directory, &format!("iab:{id}"))?;
        let previous: Option<Record> = store.load()?;
        let recovered = previous.is_some();
        let record = if let Some(record) = previous {
            if record.schema != 1 || record.binding != expected {
                return Err(Error::action(
                    "Browser recovery endpoint or route identity differs",
                ));
            }
            let mut restored: Authority = serde_json::from_value(record.authority.clone())?;
            restored.route.validate()?;
            if restored.route != authority.route {
                return Err(Error::action("Browser recovery route differs"));
            }
            restored.invalidate_inputs();
            let restored_host = serde_json::from_value(record.host.clone())?;
            browser.cancel_raw_waits();
            browser.iab_host = restored_host;
            browser.iab_host.restored();
            browser.iab = Some(restored);
            record
        } else {
            Record {
                schema: 1,
                binding: expected,
                authority: serde_json::to_value(authority)?,
                host: serde_json::to_value(&browser.iab_host)?,
                next: 0,
                pending: None,
                uncertain: vec![],
                resolutions: vec![],
            }
        };
        browser.durable = Some(Journal {
            store,
            record,
            recovered,
        });
        browser.durable_save()?;
        if recovered {
            browser.recover_iab()?;
        }
        self.iab_recovery_status(id)
    }
    pub fn iab_recovery_status(&self, id: &str) -> Result<Value> {
        let browser = self
            .providers
            .get(id)
            .ok_or_else(|| Error::action("IAB route not found"))?;
        let journal = browser
            .durable
            .as_ref()
            .ok_or_else(|| Error::action("IAB recovery is not configured"))?;
        Ok(
            json!({"recovered":journal.recovered,"contextId":browser.iab_host.context_id(),"pending":journal.record.pending,"uncertain":journal.record.uncertain,"resolutions":journal.record.resolutions}),
        )
    }
    /// Trusted host attestation of a context whose creation acknowledgement was
    /// lost. Never infer ownership from the global list or replay its creation.
    pub fn resolve_iab_context(
        &mut self,
        id: &str,
        operation: u64,
        context: &str,
    ) -> Result<Value> {
        if context.is_empty() || context.len() > 256 {
            return Err(Error::invalid("Invalid recovery context identity"));
        }
        if self
            .providers
            .iter()
            .any(|(other, browser)| other != id && browser.iab_host.context_id() == Some(context))
        {
            return Err(Error::action("Recovery context belongs to another route"));
        }
        let browser = self
            .providers
            .get_mut(id)
            .ok_or_else(|| Error::action("IAB route not found"))?;
        let journal = browser
            .durable
            .as_ref()
            .ok_or_else(|| Error::action("IAB recovery is not configured"))?;
        if let Some(previous) = journal
            .record
            .resolutions
            .iter()
            .find(|item| item.operation == operation)
        {
            if previous.context != context {
                return Err(Error::action(
                    "Recovery operation was resolved with a different context",
                ));
            }
        } else {
            if !journal.record.pending.as_ref().is_some_and(|pending| {
                pending.id == operation && pending.method == "Target.createBrowserContext"
            }) || browser.iab_host.context_id().is_some()
            {
                return Err(Error::action(
                    "Recovery does not match an unknown context creation",
                ));
            }
            if journal.record.resolutions.len() >= 128 {
                return Err(Error::action("Browser recovery resolution limit exceeded"));
            }
            // This single read-only validation must not overwrite the uncertain
            // creation intent. Preserve the journal on every transport failure.
            let journal = browser.durable.take();
            let actual = browser.call("Target.getBrowserContexts", json!({}), None);
            browser.durable = journal;
            if !actual?["browserContextIds"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(context)))
            {
                return Err(Error::action(
                    "Recovery context is absent from the bound renderer",
                ));
            }
            browser.cancel_raw_waits();
            browser.iab_host.resolve_context(context);
            let journal = browser.durable.as_mut().unwrap();
            journal.record.pending = None;
            journal.record.resolutions.push(Resolution {
                operation,
                context: context.into(),
            });
        }
        // Save even on an idempotent retry: the previous save may have failed.
        browser.durable_save()?;
        browser.recover_iab()?;
        self.iab_recovery_status(id)
    }
}

pub(super) struct HostBinding {
    pub route: crate::host_turns::Route,
    pub active: bool,
    token: Option<String>,
}

impl Browsers {
    pub(super) fn host_visible(&self, browser: &Browser) -> bool {
        if !self.host_managed {
            return true;
        }
        browser.host_binding.as_ref().is_some_and(|binding| {
            binding.active && self.current_host_route.as_deref() == Some(&binding.route.key())
        })
    }
    pub fn select_host_route(&mut self, route: Option<String>) {
        if self.current_host_route != route {
            self.cancel_all_raw_waits();
        }
        self.current_host_route = route;
    }
    pub fn host_binding_identity(
        &self,
        id: &str,
        route: &crate::host_turns::Route,
    ) -> Result<Value> {
        let browser = self
            .providers
            .get(id)
            .ok_or_else(|| Error::action("Host browser binding was not found"))?;
        if let Some(authority) = &browser.iab {
            if authority.route.conversation_id != route.conversation_id
                || authority.route.thread_id != route.thread_id
            {
                return Err(Error::action(
                    "Trusted host route differs from configured IAB route",
                ));
            }
        } else if !browser.extension {
            return Err(Error::unsupported(
                "Trusted browser lifecycle requires an IAB or owned extension provider",
            ));
        }
        Ok(
            json!({"browserId":id,"route":route,"endpoint":format!("{:x}",Sha256::digest(browser.endpoint.as_bytes()))}),
        )
    }
    pub fn bind_host_route(
        &mut self,
        id: &str,
        route: crate::host_turns::Route,
        token: Option<String>,
    ) -> Result<()> {
        self.host_binding_identity(id, &route)?;
        let browser = self.providers.get_mut(id).unwrap();
        if browser.extension
            && token.as_ref().is_none_or(|token| {
                token.len() != 64 || !token.bytes().all(|c| c.is_ascii_hexdigit())
            })
        {
            return Err(Error::invalid(
                "Extension host lifecycle requires its independent bridge authority token",
            ));
        }
        if browser.host_binding.is_some() {
            return Err(Error::invalid("Browser already has a trusted host binding"));
        }
        browser.cancel_raw_waits();
        browser.host_binding = Some(HostBinding {
            route,
            active: false,
            token,
        });
        self.host_managed = true;
        Ok(())
    }
    pub fn resume_host_route(
        &mut self,
        id: &str,
        turn: Option<&str>,
        directory: &Path,
    ) -> Result<()> {
        self.providers
            .get_mut(id)
            .ok_or_else(|| Error::action("Host browser binding was not found"))?
            .cancel_raw_waits();
        let iab = self
            .providers
            .get(id)
            .is_some_and(|browser| browser.iab.is_some());
        if iab && self.providers[id].durable.is_none() {
            self.enable_iab_recovery(id, directory)?;
        }
        let browser = self.providers.get_mut(id).unwrap();
        let binding = browser.host_binding.as_ref().unwrap();
        if let Some(turn) = turn {
            if let Some(authority) = browser.iab.as_mut() {
                let event = crate::host_turns::Event {
                    event_id: "durable-resume".into(),
                    sequence: 1,
                    phase: crate::host_turns::Phase::Started,
                    route: binding.route.clone(),
                    turn_id: turn.into(),
                };
                authority.set_context(Some(&event.metadata()))?;
            }
            browser.host_binding.as_mut().unwrap().active = true;
        }
        browser.durable_save()
    }
    pub fn apply_host_turn(&mut self, id: &str, event: &crate::host_turns::Event) -> Result<()> {
        let browser = self
            .providers
            .get_mut(id)
            .ok_or_else(|| Error::action("Host browser binding was not found"))?;
        let binding = browser
            .host_binding
            .as_ref()
            .ok_or_else(|| Error::action("Browser has no trusted host binding"))?;
        if binding.route != event.route {
            return Err(Error::new(
                -32003,
                "Host event does not own this browser route",
            ));
        }
        browser.surface.raw_events.lock().unwrap().waits.clear();
        if browser.iab.is_some() {
            match event.phase {
                crate::host_turns::Phase::Started => {
                    browser
                        .iab
                        .as_mut()
                        .unwrap()
                        .set_context(Some(&event.metadata()))?;
                    browser.durable_save()?;
                    browser.recover_iab()?;
                }
                crate::host_turns::Phase::Ended => {
                    let errors = browser.end_iab_session();
                    if let Some(error) = errors.into_iter().next() {
                        return Err(error);
                    }
                }
            }
        } else {
            let token = binding.token.clone().unwrap();
            browser.cancel_choosers(None);
            browser.call("Skyre.hostLifecycle",json!({"authorityToken":token,"phase":event.phase,"route":event.route,"turnId":event.turn_id,"eventId":event.event_id,"sequence":event.sequence}),None)?;
            browser.extension_started = true;
        }
        browser.host_binding.as_mut().unwrap().active =
            event.phase == crate::host_turns::Phase::Started;
        browser.durable_save()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn definitely_unsent_connection_failure_does_not_create_unknown_resource_intent() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}/owned", listener.local_addr().unwrap());
        drop(listener);
        let mut browsers = Browsers::default();
        browsers
            .register_iab(
                "iab",
                &endpoint,
                super::super::iab::RouteConfig {
                    conversation_id: "conversation".into(),
                    thread_id: None,
                    window_id: "window".into(),
                },
            )
            .unwrap();
        browsers
            .enable_iab_recovery("iab", directory.path())
            .unwrap();
        let result = browsers.providers.get_mut("iab").unwrap().call(
            "Target.createBrowserContext",
            json!({"disposeOnDetach":false}),
            None,
        );
        assert_eq!(result.unwrap_err().code, -32006);
        let status = browsers.iab_recovery_status("iab").unwrap();
        assert!(status["pending"].is_null());
        assert!(status["uncertain"].as_array().unwrap().is_empty());
        assert!(status["contextId"].is_null());
    }
}
