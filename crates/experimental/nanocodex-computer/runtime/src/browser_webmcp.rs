//! Native fetched-registration authority, separate from renderer descriptors.
use super::{Browser, Browsers, raw_events};
use crate::{Error, Result, engine::string};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

const STALE: &str = "WebMCP tool registration is stale. Call fetchTools() again.";
const RETENTION: &str =
    "WebMCP descriptor retention limit exceeded; refresh the page and fetchTools again.";
// Independent storage bounds; not the original configurable descriptor limits.
const MAX_TOOLS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum SessionKey {
    Host(String),
    LocalConnection,
}

#[derive(Default)]
struct Document;
#[derive(Clone)]
pub(super) struct Fetch(Arc<Document>);
#[derive(Clone, Debug, PartialEq, Eq)]
struct CatalogProof {
    frame: String,
    name: String,
    registration: String,
    session: String,
    target: Option<String>,
    top_level: bool,
}
#[derive(Clone)]
pub(super) struct Tool {
    descriptor: Value,
    catalog: Option<CatalogProof>,
}
struct Snapshot {
    document: Arc<Document>,
    tools: Vec<Tool>,
}
#[derive(Default)]
struct Tab {
    document: Arc<Document>,
    disabled: bool,
    mode: Option<bool>,
    snapshots: BTreeMap<SessionKey, Snapshot>,
    catalog: BTreeMap<(String, String), Tool>,
    catalog_bytes: usize,
}
#[derive(Default)]
pub(super) struct State {
    tabs: BTreeMap<String, Tab>,
    // Synthetic IDs belong only to the independent CDP provider. Never reuse
    // them after detach, document replacement, or connection cleanup.
    next_registration: u64,
}
#[derive(Clone)]
pub(super) struct Admission {
    tab: String,
    owner: SessionKey,
    document: Arc<Document>,
    tool: Tool,
}
impl Tool {
    fn bytes(&self) -> usize {
        self.descriptor.to_string().len()
            + self.catalog.as_ref().map_or(0, |proof| {
                proof.frame.len()
                    + proof.name.len()
                    + proof.registration.len()
                    + proof.session.len()
                    + proof.target.as_ref().map_or(0, String::len)
            })
    }
    fn name(&self) -> &str {
        self.descriptor["name"].as_str().unwrap()
    }
    fn registration(&self) -> &str {
        self.descriptor["registrationId"].as_str().unwrap()
    }
}
impl State {
    fn enabled(tab: &Tab) -> Result<()> {
        if tab.disabled {
            return Err(Error::action(RETENTION));
        }
        Ok(())
    }
    pub(super) fn begin_fetch(&mut self, tab: &str) -> Result<Fetch> {
        let state = self.tabs.entry(tab.into()).or_default();
        Self::enabled(state)?;
        Ok(Fetch(state.document.clone()))
    }
    pub(super) fn mode(&self, tab: &str) -> Option<bool> {
        self.tabs.get(tab).and_then(|state| state.mode)
    }
    pub(super) fn set_mode(&mut self, tab: &str, mode: bool) {
        self.tabs.entry(tab.into()).or_default().mode = Some(mode);
    }
    pub(super) fn catalog(&self, tab: &str) -> Result<Vec<Tool>> {
        let state = self.tabs.get(tab).ok_or_else(|| Error::action(STALE))?;
        Self::enabled(state)?;
        Ok(state.catalog.values().cloned().collect())
    }
    pub(super) fn fetched_page_tools(value: Value) -> Result<Vec<Tool>> {
        let values = value
            .as_array()
            .ok_or_else(|| Error::action("WebMCP fetchTools failed: no result returned."))?;
        values
            .iter()
            .map(|descriptor| {
                if !descriptor["name"].is_string()
                    || !descriptor["registrationId"].is_string()
                    || ["title", "description"].iter().any(|name| {
                        descriptor
                            .get(name)
                            .is_some_and(|v| !v.is_null() && !v.is_string())
                    })
                {
                    return Err(Error::action(
                        "WebMCP returned an invalid registration descriptor",
                    ));
                }
                Ok(Tool {
                    descriptor: descriptor.clone(),
                    catalog: None,
                })
            })
            .collect()
    }
    pub(super) fn finish_fetch(
        &mut self,
        tab: &str,
        owner: SessionKey,
        fetch: &Fetch,
        tools: Vec<Tool>,
    ) -> Result<Value> {
        let state = self.tabs.get_mut(tab).ok_or_else(|| Error::action(STALE))?;
        Self::enabled(state)?;
        let tools = if Arc::ptr_eq(&state.document, &fetch.0) {
            tools
        } else {
            vec![]
        };
        if tools.len() > MAX_TOOLS
            || tools.iter().map(Tool::bytes).sum::<usize>() > crate::protocol::MAX_FRAME
        {
            state.disabled = true;
            return Err(Error::action(RETENTION));
        }
        let result = Value::Array(tools.iter().map(|tool| tool.descriptor.clone()).collect());
        state.snapshots.insert(
            owner,
            Snapshot {
                document: state.document.clone(),
                tools,
            },
        );
        Ok(result)
    }
    pub(super) fn fetch_is_current(&self, tab: &str, fetch: &Fetch) -> Result<bool> {
        let state = self.tabs.get(tab).ok_or_else(|| Error::action(STALE))?;
        Self::enabled(state)?;
        Ok(Arc::ptr_eq(&state.document, &fetch.0))
    }
    pub(super) fn admit(&self, tab: &str, owner: SessionKey, args: &Value) -> Result<Admission> {
        let name = string(args, "name")?;
        let registration = requested_registration(args)?;
        let state = self.tabs.get(tab).ok_or_else(|| Error::action(STALE))?;
        Self::enabled(state)?;
        let snapshot = state
            .snapshots
            .get(&owner)
            .ok_or_else(|| Error::action(STALE))?;
        let tool = snapshot
            .tools
            .iter()
            .find(|tool| tool.name() == name)
            .filter(|tool| tool.registration() == registration)
            .filter(|_| Arc::ptr_eq(&snapshot.document, &state.document))
            .ok_or_else(|| Error::action(STALE))?;
        let admission = Admission {
            tab: tab.into(),
            owner,
            document: state.document.clone(),
            tool: tool.clone(),
        };
        self.validate(&admission)?;
        Ok(admission)
    }
    pub(super) fn validate(&self, admission: &Admission) -> Result<()> {
        let state = self
            .tabs
            .get(&admission.tab)
            .ok_or_else(|| Error::action(STALE))?;
        Self::enabled(state)?;
        let snapshot = state
            .snapshots
            .get(&admission.owner)
            .ok_or_else(|| Error::action(STALE))?;
        if !Arc::ptr_eq(&state.document, &admission.document)
            || !Arc::ptr_eq(&state.document, &snapshot.document)
            || !snapshot
                .tools
                .iter()
                .find(|tool| tool.name() == admission.tool.name())
                .is_some_and(|tool| tool.registration() == admission.tool.registration())
        {
            return Err(Error::action(STALE));
        }
        if let Some(proof) = &admission.tool.catalog
            && !state
                .catalog
                .get(&(proof.frame.clone(), proof.name.clone()))
                .is_some_and(|current| current.catalog.as_ref() == Some(proof))
        {
            return Err(Error::action(STALE));
        }
        Ok(())
    }
    pub(super) fn event(&mut self, context: &raw_events::Context, event: &Value) {
        if let Some(tab) = &context.discard {
            self.remove(tab);
        }
        let Some(source) = &context.source else {
            return;
        };
        let method = event["method"].as_str();
        if method == Some("Page.frameNavigated")
            && source.top_level
            && event["params"]["frame"]
                .get("parentId")
                .is_none_or(Value::is_null)
        {
            if let Some(state) = self.tabs.get_mut(&source.tab) {
                state.document = Arc::new(Document);
                state.disabled = false;
                state.catalog.clear();
                state.catalog_bytes = 0;
            }
            return;
        }
        if !matches!(method, Some("WebMCP.toolsAdded" | "WebMCP.toolsRemoved")) {
            return;
        }
        let Some(session) = source.source["sessionId"].as_str() else {
            return;
        };
        let state = self.tabs.entry(source.tab.clone()).or_default();
        for descriptor in event["params"]["tools"].as_array().into_iter().flatten() {
            let (Some(frame), Some(name)) =
                (descriptor["frameId"].as_str(), descriptor["name"].as_str())
            else {
                continue;
            };
            let key = (frame.to_owned(), name.to_owned());
            if method == Some("WebMCP.toolsRemoved") {
                // A child route cannot remove a registration attributed to a
                // different active native source merely by reusing its name.
                if state.catalog.get(&key).is_some_and(|tool| {
                    tool.catalog
                        .as_ref()
                        .is_some_and(|proof| proof.session == session)
                }) && let Some(old) = state.catalog.remove(&key)
                {
                    state.catalog_bytes -= old.bytes();
                }
                continue;
            }
            if state.disabled {
                continue;
            }
            let Some(next) = self.next_registration.checked_add(1) else {
                state.disabled = true;
                state.catalog.clear();
                state.catalog_bytes = 0;
                continue;
            };
            self.next_registration = next;
            let registration = format!("cdp-{next}");
            let mut value = descriptor.clone();
            value["registrationId"] = json!(registration);
            let tool = Tool {
                descriptor: value,
                catalog: Some(CatalogProof {
                    frame: frame.into(),
                    name: name.into(),
                    registration,
                    session: session.into(),
                    target: source.tracked_target.clone(),
                    top_level: source.top_level,
                }),
            };
            let bytes =
                state.catalog_bytes - state.catalog.get(&key).map_or(0, Tool::bytes) + tool.bytes();
            let count = state.catalog.len() + usize::from(!state.catalog.contains_key(&key));
            if bytes > crate::protocol::MAX_FRAME || count > MAX_TOOLS {
                state.disabled = true;
                state.catalog.clear();
                state.catalog_bytes = 0;
                continue;
            }
            state.catalog.insert(key, tool);
            state.catalog_bytes = bytes;
        }
    }
    pub(super) fn remove(&mut self, tab: &str) {
        self.tabs.remove(tab);
    }
    pub(super) fn disconnect(&mut self) {
        self.tabs.clear();
    }
    pub(super) fn clear_fetched(&mut self) {
        for state in self.tabs.values_mut() {
            state.snapshots.clear();
        }
    }
    fn abandon_fetch(&mut self, tab: &str, fetch: &Fetch) {
        if self.tabs.get(tab).is_some_and(|state| {
            state.mode.is_none()
                && state.snapshots.is_empty()
                && state.catalog.is_empty()
                && Arc::ptr_eq(&state.document, &fetch.0)
        }) {
            self.tabs.remove(tab);
        }
    }
}
impl Admission {
    pub(super) fn apply_description(&self, args: &mut Value) {
        for (external, descriptor) in [("tool_title", "title"), ("tool_description", "description")]
        {
            args.as_object_mut().unwrap().remove(external);
            if let Some(value) = self
                .tool
                .descriptor
                .get(descriptor)
                .filter(|value| !value.is_null())
            {
                args[external] = value.clone();
            }
        }
    }
    pub(super) fn check_source(&self, dialogs: &super::dialog::State) -> Result<()> {
        if let Some(proof) = &self.tool.catalog {
            // Only a previously retained native source is looked up here; no
            // request argument or renderer field can supply route authority.
            let context = dialogs.raw_event_context(&json!({"sessionId":proof.session}));
            if !context.source.is_some_and(|source| {
                source.tab == self.tab
                    && source.tracked_target == proof.target
                    && source.top_level == proof.top_level
            }) {
                return Err(Error::action(STALE));
            }
        }
        Ok(())
    }
}
pub(super) fn requested_registration(args: &Value) -> Result<&str> {
    args["registrationId"]
        .as_str()
        .ok_or_else(|| Error::invalid("registrationId must be a string"))
}

/// Project canonical schema fields before the generic compatibility aliases.
pub(super) fn normalize(method: &str, args: &Value) -> Result<Option<(String, Value)>> {
    if !matches!(method, "webmcp_list_tools" | "webmcp_invoke_tool") {
        return Ok(None);
    }
    let mut result = json!({"browser":string(args,"browser_id")?,"tab":string(args,"tab_id")?});
    if method == "webmcp_list_tools" {
        return Ok(Some(("webmcp_list".into(), result)));
    }
    result["name"] = json!(string(args, "tool_name")?);
    result["registrationId"] = json!(string(args, "registration_id")?);
    for field in ["tool_title", "tool_description"] {
        if let Some(value) = args.get(field) {
            if !value.is_string() {
                return Err(Error::invalid(format!("{field} must be a string")));
            }
            result[field] = value.clone();
        }
    }
    if let Some(input) = args.get("input") {
        result["arguments"] = input.clone();
    }
    if let Some(timeout) = args.get("timeout_ms") {
        if !timeout
            .as_f64()
            .is_some_and(|n| n.is_finite() && n > 0. && n.fract() == 0.)
        {
            return Err(Error::invalid("timeout_ms must be a positive integer"));
        }
        result["timeoutMs"] = timeout.clone();
    }
    Ok(Some(("webmcp_invoke".into(), result)))
}

impl Browser {
    fn webmcp_session(&self) -> Result<SessionKey> {
        self.ensure_context()?;
        if let Some(authority) = &self.iab {
            return Ok(SessionKey::Host(authority.context()?.session_id.clone()));
        }
        if let Some(binding) = &self.host_binding {
            return Ok(SessionKey::Host(
                binding
                    .route
                    .thread_id
                    .as_ref()
                    .unwrap_or(&binding.route.conversation_id)
                    .clone(),
            ));
        }
        Ok(SessionKey::LocalConnection)
    }
    fn webmcp_admit(&self, args: &Value) -> Result<Admission> {
        let tab = string(args, "tab")?;
        if let Some(authority) = &self.iab {
            authority.require_tab(tab)?;
        }
        let admission =
            self.surface
                .webmcp
                .lock()
                .unwrap()
                .admit(tab, self.webmcp_session()?, args)?;
        admission.check_source(&self.surface.dialogs.lock().unwrap())?;
        Ok(admission)
    }
    pub(super) fn validate_webmcp_context(&self, admission: &Admission) -> Result<()> {
        // Re-read native context after provider preparation. Host lifecycle
        // writers need &mut Browsers, so cannot run while this Browser remains
        // exclusively borrowed through the synchronous final send.
        if self.webmcp_session()? != admission.owner {
            return Err(Error::action(STALE));
        }
        if let Some(authority) = &self.iab {
            authority.require_tab(&admission.tab)?;
        }
        Ok(())
    }
    pub(super) fn webmcp(&mut self, tab: &str, method: &str, args: &Value) -> Result<Value> {
        let admission = if method == "webmcp_invoke" {
            Some(self.webmcp_admit(args)?)
        } else {
            None
        };
        if admission.is_some() {
            // Consume already received lifecycle traffic before preparing an
            // invocation. The final wire guard also covers nested callbacks.
            self.poll_events()?;
        }
        let fetch = if method == "webmcp_list" {
            Some(self.surface.webmcp.lock().unwrap().begin_fetch(tab)?)
        } else {
            None
        };
        let result = self.webmcp_dispatch(tab, args, admission.as_ref(), fetch.as_ref());
        if result.is_err()
            && let Some(fetch) = fetch
        {
            // Failed attachment must not leave one native state allocation per
            // arbitrary requested target. Existing live catalog/snapshots stay.
            self.surface
                .webmcp
                .lock()
                .unwrap()
                .abandon_fetch(tab, &fetch);
        }
        result
    }
    fn webmcp_dispatch(
        &mut self,
        tab: &str,
        args: &Value,
        admission: Option<&Admission>,
        fetch: Option<&Fetch>,
    ) -> Result<Value> {
        let mode = self.surface.webmcp.lock().unwrap().mode(tab);
        if mode.is_none() {
            self.page(tab)?;
            let mode = match self.tab(tab, "WebMCP.enable", json!({})) {
                Ok(_) => true,
                Err(error) if error.code == -32601 => false,
                Err(error) => return Err(error),
            };
            self.surface.webmcp.lock().unwrap().set_mode(tab, mode);
        }
        let mode = self.surface.webmcp.lock().unwrap().mode(tab);
        if mode == Some(false) {
            // The original registration owner and typed facade refer to the
            // top-level document. Legacy selected-frame evaluation could not
            // bind its snapshot and is now refused, before page evaluation.
            if args.get("frame").is_some()
                || args
                    .get("isolated")
                    .is_some_and(|v| *v != false && !v.is_null())
            {
                return Err(Error::invalid(
                    "WebMCP page-registry snapshots require the top-level document",
                ));
            }
            if let Some(fetch) = fetch {
                let value = self.evaluate(tab, "(()=>{const c=navigator.modelContext;if(!c?.codexGetTools&&!c?.getTools)throw new Error('No supported WebMCP registry is installed');return (c.codexGetTools??c.getTools).call(c);})()", args)?;
                // Match the original ordering: a replaced document yields []
                // before trying to parse the old document's returned value.
                let current = self
                    .surface
                    .webmcp
                    .lock()
                    .unwrap()
                    .fetch_is_current(tab, fetch)?;
                let tools = if current {
                    State::fetched_page_tools(value)?
                } else {
                    vec![]
                };
                let owner = self.webmcp_session()?;
                return self
                    .surface
                    .webmcp
                    .lock()
                    .unwrap()
                    .finish_fetch(tab, owner, fetch, tools);
            }
            let admission = admission.unwrap();
            let expression = format!(
                "(async()=>{{const c=navigator.modelContext;if(!c?.executeTool)throw new Error('WebMCP registry unavailable');const fn=c.codexExecuteTool??c.executeTool;const result=await fn.call(c,{},{});return typeof result==='string'?JSON.parse(result):result;}})()",
                json!({"name":admission.tool.name(),"registrationId":admission.tool.registration()}),
                json!(
                    args.get("arguments")
                        .cloned()
                        .unwrap_or(json!({}))
                        .to_string()
                )
            );
            let session = self.session(tab)?;
            let params = json!({"expression":expression,"returnByValue":true,"awaitPromise":true,"timeout":super::surface::timeout(args,10000)?.as_millis()});
            return super::surface::eval_value(self.call_webmcp(
                "Runtime.evaluate",
                params,
                Some(&session),
                admission,
            )?);
        }
        self.pump(tab)?;
        if let Some(fetch) = fetch {
            let tools = self.surface.webmcp.lock().unwrap().catalog(tab)?;
            let owner = self.webmcp_session()?;
            return self
                .surface
                .webmcp
                .lock()
                .unwrap()
                .finish_fetch(tab, owner, fetch, tools);
        }
        let admission = admission.unwrap();
        let proof = admission
            .tool
            .catalog
            .as_ref()
            .ok_or_else(|| Error::action(STALE))?;
        // Keep the policy-checked frame selection. A catalog row cannot select
        // a different frame implicitly, and an extra selector cannot choose a
        // different registration with the same name.
        let frame = self.frame(tab, args)?;
        if frame["id"].as_str() != Some(proof.frame.as_str()) {
            return Err(Error::action(STALE));
        }
        let session = self.session(tab)?;
        let request = self.call_webmcp("WebMCP.invokeTool", json!({"frameId":proof.frame,"toolName":admission.tool.name(),"input":args.get("arguments").cloned().unwrap_or(json!({}))}), Some(&session), admission)?;
        let id = string(&request, "invocationId")?.to_owned();
        let deadline = std::time::Instant::now() + super::surface::timeout(args, 10000)?;
        loop {
            self.pump(tab)?;
            if let Some(result) = self.surface.webmcp_responses.remove(&id) {
                return if result["status"] == "Completed" {
                    Ok(result["output"].clone())
                } else {
                    Err(Error::action(
                        result["errorText"]
                            .as_str()
                            .unwrap_or("WebMCP invocation failed or canceled"),
                    ))
                };
            }
            if std::time::Instant::now() >= deadline {
                let _ = self.tab_maintenance(
                    tab,
                    "WebMCP.cancelInvocation",
                    json!({"invocationId":id}),
                );
                return Err(Error::action("WebMCP invocation timed out"));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
impl Browsers {
    /// No discovery, connection, attachment, or renderer call. Engine invokes
    /// this only after its guardian/command/frame checks have admitted the route.
    pub(crate) fn prepare_webmcp(&self, method: &str, args: &mut Value) -> Result<()> {
        if method != "webmcp_invoke" {
            return Ok(());
        }
        requested_registration(args)?;
        let browser = args["browser"]
            .as_str()
            .and_then(|id| self.providers.get(id))
            .or_else(|| {
                if args.get("browser").is_none() {
                    self.providers
                        .values()
                        .find(|browser| self.host_visible(browser))
                } else {
                    None
                }
            })
            .ok_or_else(|| Error::action("Browser not found"))?;
        if !self.host_visible(browser) {
            return Err(Error::action(
                "Browser route is outside the current trusted host context",
            ));
        }
        browser.webmcp_admit(args)?.apply_description(args);
        Ok(())
    }
}

#[cfg(test)]
#[path = "browser_webmcp_tests.rs"]
mod tests;
