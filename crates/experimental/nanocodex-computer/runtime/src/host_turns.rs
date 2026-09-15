//! Opt-in trusted host turn events. This API is absent from the model facade and
//! requires an independently configured capability even on the parent RPC peer.
use crate::{
    Error, Result,
    browser::{Browsers, persistence::Store},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Route {
    pub conversation_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}
impl Route {
    pub fn key(&self) -> String {
        serde_json::to_string(self).unwrap()
    }
    pub fn validate(&self) -> Result<()> {
        identifier(&self.conversation_id)?;
        if let Some(thread) = &self.thread_id {
            identifier(thread)?;
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Binding {
    pub browser_id: String,
    pub route: Route,
    #[serde(default)]
    pub extension_authority_token: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub authority_token: String,
    pub state_directory: PathBuf,
    pub bindings: Vec<Binding>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Event {
    pub event_id: String,
    pub sequence: u64,
    pub phase: Phase,
    pub route: Route,
    pub turn_id: String,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Started,
    Ended,
}
#[derive(Clone, Deserialize, Serialize)]
struct Receipt {
    event: Event,
    applied: BTreeSet<String>,
    complete: bool,
}
#[derive(Default, Deserialize, Serialize)]
struct State {
    schema: u32,
    binding: String,
    last_sequence: u64,
    active: BTreeMap<String, String>,
    receipts: BTreeMap<String, Receipt>,
}
pub struct Controller {
    store: Store,
    state: State,
    token_hash: [u8; 32],
    bindings: Vec<Binding>,
}
/// Read-only host capability verifier for transport cancellation. It cannot
/// mutate routes and is never exposed through a model-facing provider method.
#[derive(Clone)]
pub struct Authorization([u8; 32]);
impl Authorization {
    pub fn authorize(&self, token: &str) -> Result<()> {
        let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        if candidate
            .iter()
            .zip(self.0)
            .fold(0u8, |different, (a, b)| different | (a ^ b))
            != 0
        {
            return Err(Error::new(
                -32003,
                "Trusted host turn capability is required",
            ));
        }
        Ok(())
    }
}
fn identifier(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 {
        Err(Error::invalid(
            "Host turn identifiers must contain 1–256 bytes",
        ))
    } else {
        Ok(())
    }
}
impl Event {
    fn validate(&self) -> Result<()> {
        identifier(&self.event_id)?;
        identifier(&self.turn_id)?;
        self.route.validate()?;
        if self.sequence == 0 {
            return Err(Error::invalid("Host turn sequence must be positive"));
        }
        Ok(())
    }
    pub fn metadata(&self) -> Value {
        let mut value = json!({"session_id":self.route.conversation_id,"turn_id":self.turn_id});
        if let Some(thread) = &self.route.thread_id {
            value["thread_source"] = json!("subagent");
            value["thread_id"] = json!(thread);
        }
        value
    }
}
impl Controller {
    pub fn open(config: Config, browsers: &mut Browsers) -> Result<Self> {
        if config.authority_token.len() != 64
            || !config
                .authority_token
                .bytes()
                .all(|c| c.is_ascii_hexdigit())
        {
            return Err(Error::invalid(
                "Host turn authority must be an independent 256-bit hex capability",
            ));
        }
        if config.bindings.is_empty() || config.bindings.len() > 128 {
            return Err(Error::invalid(
                "Host turn configuration needs 1–128 browser bindings",
            ));
        }
        let mut seen = BTreeSet::new();
        let mut approval_scopes = BTreeMap::new();
        let mut identities = vec![];
        for binding in &config.bindings {
            identifier(&binding.browser_id)?;
            binding.route.validate()?;
            if !seen.insert(&binding.browser_id) {
                return Err(Error::invalid("Duplicate host browser binding"));
            }
            let scope = binding
                .route
                .thread_id
                .as_deref()
                .unwrap_or(&binding.route.conversation_id);
            if let Some(previous) = approval_scopes.insert(scope, &binding.route)
                && previous != &binding.route
            {
                return Err(Error::invalid(
                    "Distinct host routes cannot share a conversation/subagent approval identity",
                ));
            }
        }
        for binding in &config.bindings {
            identities.push(browsers.host_binding_identity(&binding.browser_id, &binding.route)?);
        }
        let token_hash: [u8; 32] = Sha256::digest(config.authority_token.as_bytes()).into();
        let expected = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(
                &json!({"routes":identities,"authority":format!("{:x}",Sha256::digest(token_hash))})
            )?)
        );
        let store = Store::open(&config.state_directory, "trusted-host-turns")?;
        let saved: Option<State> = store.load()?;
        let state = if let Some(state) = saved {
            if state.schema != 1 || state.binding != expected {
                return Err(Error::action(
                    "Trusted host route/endpoint configuration differs from durable state",
                ));
            }
            state
        } else {
            State {
                schema: 1,
                binding: expected,
                ..Default::default()
            }
        };
        for binding in &config.bindings {
            browsers.bind_host_route(
                &binding.browser_id,
                binding.route.clone(),
                binding.extension_authority_token.clone(),
            )?;
        }
        for binding in &config.bindings {
            let mut effective = state.active.get(&binding.route.key()).map(String::as_str);
            for receipt in state.receipts.values().filter(|receipt| {
                !receipt.complete && receipt.applied.contains(&binding.browser_id)
            }) {
                effective = match receipt.event.phase {
                    Phase::Started => Some(receipt.event.turn_id.as_str()),
                    Phase::Ended => None,
                };
            }
            browsers.resume_host_route(&binding.browser_id, effective, &config.state_directory)?;
        }
        store.save(&state)?;
        Ok(Self {
            store,
            state,
            token_hash,
            bindings: config.bindings,
        })
    }
    pub fn authorize(&self, token: &str) -> Result<()> {
        self.authorization().authorize(token)
    }
    pub fn authorization(&self) -> Authorization {
        Authorization(self.token_hash)
    }
    pub fn recovery(
        &self,
        token: &str,
        args: &Value,
        browsers: &mut Browsers,
        resolve: bool,
    ) -> Result<Value> {
        self.authorize(token)?;
        let id = args["browserId"]
            .as_str()
            .ok_or_else(|| Error::invalid("Recovery browserId is required"))?;
        if !self.bindings.iter().any(|binding| binding.browser_id == id) {
            return Err(Error::new(
                -32003,
                "Browser is outside trusted host bindings",
            ));
        }
        if resolve {
            let operation = args["operationId"]
                .as_u64()
                .ok_or_else(|| Error::invalid("Recovery operationId is required"))?;
            let context = args["contextId"]
                .as_str()
                .ok_or_else(|| Error::invalid("Recovery contextId is required"))?;
            browsers.resolve_iab_context(id, operation, context)
        } else {
            browsers.iab_recovery_status(id)
        }
    }
    pub fn event(&mut self, token: &str, event: Event, browsers: &mut Browsers) -> Result<Value> {
        let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        if candidate
            .iter()
            .zip(self.token_hash)
            .fold(0u8, |different, (a, b)| different | (a ^ b))
            != 0
        {
            return Err(Error::new(
                -32003,
                "Trusted host turn capability is required",
            ));
        }
        event.validate()?;
        if self
            .state
            .receipts
            .values()
            .any(|receipt| !receipt.complete && receipt.event.event_id != event.event_id)
        {
            return Err(Error::action(
                "Resolve the pending host turn event before advancing",
            ));
        }
        let key = event.route.key();
        let targets: Vec<_> = self
            .bindings
            .iter()
            .filter(|binding| binding.route == event.route)
            .map(|binding| binding.browser_id.clone())
            .collect();
        if targets.is_empty() {
            return Err(Error::action(
                "No browser route is configured for this host event",
            ));
        }
        if let Some(previous) = self.state.receipts.get(&event.event_id) {
            if previous.event != event {
                return Err(Error::action(
                    "Host event identity was reused with different content",
                ));
            }
        } else {
            if event.phase == Phase::Started
                && self.state.receipts.values().any(|receipt| {
                    receipt.complete
                        && receipt.event.phase == Phase::Ended
                        && receipt.event.route == event.route
                        && receipt.event.turn_id == event.turn_id
                })
            {
                return Err(Error::action("Host turn identity is retired"));
            }
            if event.sequence <= self.state.last_sequence {
                return Err(Error::action("Host turn event sequence is stale"));
            }
            if self
                .state
                .receipts
                .values()
                .any(|receipt| !receipt.complete)
            {
                return Err(Error::action(
                    "Resolve the pending host turn event before advancing",
                ));
            }
            if self.state.receipts.len() >= 10000 {
                return Err(Error::action("Host turn event history limit exceeded"));
            }
            match (event.phase, self.state.active.get(&key)) {
                (Phase::Started, Some(turn)) if turn != &event.turn_id => {
                    return Err(Error::action(
                        "End the active host turn before starting another",
                    ));
                }
                (Phase::Ended, Some(turn)) if turn == &event.turn_id => (),
                (Phase::Ended, _) => {
                    return Err(Error::action("Host turn end does not match an active turn"));
                }
                _ => (),
            }
            self.state.last_sequence = event.sequence;
            self.state.receipts.insert(
                event.event_id.clone(),
                Receipt {
                    event: event.clone(),
                    applied: BTreeSet::new(),
                    complete: false,
                },
            );
            self.store.save(&self.state)?;
        }
        self.store.save(&self.state)?;
        if !self.state.receipts[&event.event_id].complete {
            // A partially applied route transition cannot expose the providers
            // that have not reached the same lifecycle state yet.
            browsers.select_host_route(None);
            for id in targets {
                if self.state.receipts[&event.event_id].applied.contains(&id) {
                    continue;
                }
                browsers.apply_host_turn(&id, &event)?;
                self.state
                    .receipts
                    .get_mut(&event.event_id)
                    .unwrap()
                    .applied
                    .insert(id);
                self.store.save(&self.state)?;
            }
            match event.phase {
                Phase::Started => {
                    self.state.active.insert(key.clone(), event.turn_id.clone());
                }
                Phase::Ended => {
                    self.state.active.remove(&key);
                }
            }
            self.state
                .receipts
                .get_mut(&event.event_id)
                .unwrap()
                .complete = true;
            self.store.save(&self.state)?;
        }
        // Re-selecting an already acknowledged live start changes only the trusted
        // request context. It does not repeat provider lifecycle effects or inputs.
        self.store.save(&self.state)?;
        let active = self.state.active.get(&key) == Some(&event.turn_id);
        browsers.select_host_route(if active { Some(key) } else { None });
        Ok(
            json!({"eventId":event.event_id,"sequence":event.sequence,"phase":event.phase,"route":event.route,"applied":true,"requestMeta":if active{json!({"x-codex-turn-metadata":event.metadata()})}else{Value::Null}}),
        )
    }
}
