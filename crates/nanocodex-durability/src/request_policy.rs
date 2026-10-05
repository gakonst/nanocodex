//! Persisted prompt/tool configuration and request-scoped virtual routing.
//!
//! Configuration controls declaration rendering, never tool execution authority.
//! Provider messages and opaque signatures remain owned by the native harness.

use crate::{Error, Result};
use nanocodex_agent::{HarnessFamily, HarnessModel};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_RECEIPTS: usize = 16;
const MAX_POLICY_BYTES: usize = 16 * 1024 * 1024;

fn bounded<T: Serialize>(value: &T) -> Result<()> {
    if serde_json::to_vec(value)?.len() > MAX_POLICY_BYTES {
        return Err(invalid("request policy current checkpoint exceeds 16 MiB"));
    }
    Ok(())
}

/// Whether provider-native continuation data forbids a physical model switch.
pub(crate) fn contains_opaque(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key("signature")
                || object.contains_key("encrypted_content")
                || object
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| {
                        matches!(
                            kind,
                            "redacted_thinking"
                                | "image"
                                | "document"
                                | "input_image"
                                | "input_audio"
                                | "input_file"
                                | "audio"
                        )
                    })
                || object.values().any(contains_opaque)
        }
        Value::Array(values) => values.iter().any(contains_opaque),
        _ => false,
    }
}

fn native_tools(request: &Value) -> Vec<Value> {
    if let Some(items) = request["input"].as_array()
        && let Some(item) = items.iter().find(|item| item["type"] == "additional_tools")
    {
        return item["tools"].as_array().cloned().unwrap_or_default();
    }
    request["tools"].as_array().cloned().unwrap_or_default()
}

pub(crate) fn authorize_native(request: &Value, authorized: &Value) -> Result<()> {
    let catalog = native_tools(authorized);
    if native_tools(request)
        .iter()
        .any(|definition| !catalog.contains(definition))
    {
        return Err(invalid(
            "prepared request contains a declaration revoked by current host authorization",
        ));
    }
    Ok(())
}

/// One named instruction section, ordered by first insertion (re-add appends).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptSection {
    /// Stable section key.
    pub name: String,
    /// Exact instruction text.
    pub text: String,
}

/// A named provider-native tool declaration. This does not authorize execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDeclaration {
    /// Stable tool key.
    pub name: String,
    /// Provider-native schema, retained without translation.
    pub definition: Value,
}

/// A historical configuration change at a model-request boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigurationPatch {
    /// Insert or replace a named section in its existing position.
    SetSection {
        /// Section to install.
        section: PromptSection,
    },
    /// Remove a section; unknown keys are harmless.
    RemoveSection {
        /// Section key.
        name: String,
    },
    /// Insert or replace a tool declaration in its existing position.
    SetTool {
        /// Declaration to install.
        tool: ToolDeclaration,
    },
    /// Remove a declaration; unknown keys are harmless.
    RemoveTool {
        /// Tool key.
        name: String,
    },
}

/// All patches admitted together for one request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigurationEntry {
    /// Stable host-derived request identity, not a provider response ID.
    pub request_id: String,
    /// Ordered changes.
    pub patches: Vec<ConfigurationPatch>,
}

/// Effective configuration at one historical boundary.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EffectiveConfiguration {
    /// Ordered named sections.
    pub sections: Vec<PromptSection>,
    /// Ordered native declarations.
    pub tools: Vec<ToolDeclaration>,
}

impl EffectiveConfiguration {
    /// Flatten sections for native adapters without positional patch support.
    pub fn instructions(&self) -> String {
        self.sections
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Reject declarations not present byte-for-JSON-value in the host catalog.
    /// The dispatcher must still perform its normal per-call authorization.
    pub fn authorize(&self, catalog: &[ToolDeclaration]) -> Result<()> {
        if self.tools.iter().any(|tool| !catalog.contains(tool)) {
            return Err(invalid(
                "configuration includes an unauthorized tool declaration",
            ));
        }
        Ok(())
    }

    fn patch(&mut self, patch: &ConfigurationPatch) -> Result<()> {
        match patch {
            ConfigurationPatch::SetSection { section } => {
                nonempty(&section.name)?;
                if let Some(old) = self.sections.iter_mut().find(|s| s.name == section.name) {
                    *old = section.clone();
                } else {
                    self.sections.push(section.clone());
                }
            }
            ConfigurationPatch::RemoveSection { name } => self.sections.retain(|s| &s.name != name),
            ConfigurationPatch::SetTool { tool } => {
                nonempty(&tool.name)?;
                if let Some(old) = self.tools.iter_mut().find(|t| t.name == tool.name) {
                    *old = tool.clone();
                } else {
                    self.tools.push(tool.clone());
                }
            }
            ConfigurationPatch::RemoveTool { name } => self.tools.retain(|t| &t.name != name),
        }
        Ok(())
    }
}

/// Append-only configuration history, serialized in the session document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigurationHistory {
    /// Effective prefix folded out of the bounded current checkpoint. Full changes
    /// remain in immutable native preparation journal records.
    #[serde(default)]
    base: EffectiveConfiguration,
    entries: Vec<ConfigurationEntry>,
}

impl ConfigurationHistory {
    /// Historical changes in admission order.
    pub fn entries(&self) -> &[ConfigurationEntry] {
        &self.entries
    }

    /// Replay through an inclusive boundary; `None` selects the latest.
    pub fn at(&self, request_id: Option<&str>) -> Result<EffectiveConfiguration> {
        let mut configuration = self.base.clone();
        for entry in &self.entries {
            for patch in &entry.patches {
                configuration.patch(patch)?;
            }
            if request_id == Some(entry.request_id.as_str()) {
                return Ok(configuration);
            }
        }
        if request_id.is_some() {
            return Err(invalid("unknown configuration boundary"));
        }
        Ok(configuration)
    }

    fn append(&mut self, entry: ConfigurationEntry) -> Result<()> {
        nonempty(&entry.request_id)?;
        if self
            .entries
            .iter()
            .any(|old| old.request_id == entry.request_id)
        {
            return Err(invalid("duplicate configuration boundary"));
        }
        // Validate before admitting any partial patch.
        let mut next = self.at(None)?;
        for patch in &entry.patches {
            next.patch(patch)?;
        }
        self.entries.push(entry);
        while self.entries.len() > MAX_RECEIPTS {
            let oldest = self.entries.remove(0);
            for patch in &oldest.patches {
                self.base.patch(patch)?;
            }
        }
        Ok(())
    }
}

/// Host-approved physical model and limits; availability/credentials stay native.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhysicalModel {
    /// Concrete native model identity.
    pub model: HarnessModel,
    /// Total context capacity, including reserved output.
    pub context_tokens: u64,
    /// Maximum output tokens.
    pub max_output_tokens: u64,
    /// Explicit native transcript compatibility group. `None` pins this model.
    /// Claude switches remain prohibited when an existing transcript is present.
    pub switch_group: Option<String>,
}

/// A new model request. Retries must reuse the entire value and request identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouteRequest {
    /// Stable host-derived request identity.
    pub request_id: String,
    /// Public virtual model selected by the user.
    pub selection: String,
    /// Prior request whose unfinished tool/stream continuation this belongs to.
    pub continuation_of: Option<String>,
    /// Conservative measured/estimated input tokens for the rendered request.
    pub input_tokens: u64,
    /// Requested output reservation.
    pub output_tokens: u64,
    /// True only at a native boundary permitting a physical model change.
    /// Must be false with unresolved tool results or opaque continuation state.
    pub switch_safe: bool,
}

/// Router input. Policies should be deterministic and have no external effects.
pub struct RoutingInput<'a> {
    /// Request under consideration.
    pub request: &'a RouteRequest,
    /// Branch-local state from the preceding request.
    pub state: &'a Value,
    /// Last dispatched physical model.
    pub previous: Option<HarnessModel>,
    /// Host-approved candidates with physical limits.
    pub models: &'a [PhysicalModel],
}

/// Policy output committed before the provider is called.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoutingChoice {
    /// Physical identity used for transport and usage attribution.
    pub dispatched: HarnessModel,
    /// Replacement branch-local policy state.
    pub state: Value,
}

/// Extension interface invoked once for each newly admitted non-continuation request.
pub trait VirtualModelRouter {
    /// Choose a physical model and the next persisted policy state.
    fn route(&self, input: RoutingInput<'_>) -> Result<RoutingChoice>;
}

impl<F> VirtualModelRouter for F
where
    F: Fn(RoutingInput<'_>) -> Result<RoutingChoice>,
{
    fn route(&self, input: RoutingInput<'_>) -> Result<RoutingChoice> {
        self(input)
    }
}

/// Exact prepared request and routing receipt retained for retries and attribution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreparedRequest {
    /// Original request selection and limit inputs.
    pub request: RouteRequest,
    /// Physical model and router state after this decision.
    pub route: RoutingChoice,
    /// Exact serialized provider request before transport authentication.
    /// Credentials must never be included.
    pub request_json: String,
    /// Configuration rendered into this request.
    pub configuration: EffectiveConfiguration,
    /// Original provider request before configuration/routing; native adapters
    /// compare it on retry to reject reuse of an identity with changed input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_request: Option<Value>,
}

/// One branch's configuration, physical selections, and exact request checkpoints.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestPolicyState {
    /// Positional source history; current native adapters flatten it.
    pub configuration: ConfigurationHistory,
    /// Recent receipts in admission order, bounded to 16 and 16 MiB total.
    /// The durable adapter stores every preparation in immutable journal records.
    pub requests: Vec<PreparedRequest>,
    /// Cumulative actual and reserved warm spend; no credentials are retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_warm_budget: Option<Value>,
    /// Latest actual warm usage; complete receipts remain in immutable journal records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cache_warm: Option<Value>,
}

impl RequestPolicyState {
    /// Prepare a request in memory. Persist the returned state before dispatch;
    /// use the durable request adapter for that boundary.
    /// `render` receives the final effective configuration and physical identity.
    pub fn prepare(
        &mut self,
        request: RouteRequest,
        patches: Vec<ConfigurationPatch>,
        models: &[PhysicalModel],
        authorized_tools: &[ToolDeclaration],
        router: &impl VirtualModelRouter,
        render: impl FnOnce(&EffectiveConfiguration, HarnessModel) -> Result<String>,
    ) -> Result<PreparedRequest> {
        nonempty(&request.request_id)?;
        nonempty(&request.selection)?;
        if let Some(saved) = self
            .requests
            .iter()
            .find(|p| p.request.request_id == request.request_id)
        {
            let entry = self
                .configuration
                .entries
                .iter()
                .find(|e| e.request_id == request.request_id);
            if saved.request != request || entry.is_none_or(|e| e.patches != patches) {
                return Err(invalid(
                    "request identity reused with different routing/configuration input",
                ));
            }
            saved.configuration.authorize(authorized_tools)?;
            validate_limits(&request, saved.route.dispatched, models)?;
            return Ok(saved.clone());
        }
        bounded(self)?;
        let previous = self.requests.last();
        let state = previous.map_or(&Value::Null, |p| &p.route.state);
        let choice = if let Some(id) = &request.continuation_of {
            let predecessor = previous
                .filter(|p| &p.request.request_id == id)
                .ok_or_else(|| invalid("continuation must reference the latest request"))?;
            if request.selection != predecessor.request.selection {
                return Err(invalid(
                    "virtual selection cannot change during continuation",
                ));
            }
            if !patches.is_empty() {
                return Err(invalid(
                    "configuration changes require a completed native boundary",
                ));
            }
            predecessor.route.clone()
        } else {
            router.route(RoutingInput {
                request: &request,
                state,
                previous: previous.map(|p| p.route.dispatched),
                models,
            })?
        };
        validate_limits(&request, choice.dispatched, models)?;
        if let Some(previous) = previous {
            let from = previous.route.dispatched;
            if from.family() != choice.dispatched.family() {
                return Err(invalid("cross-family transcript routing is unsupported"));
            }
            if from != choice.dispatched {
                let old = models.iter().find(|p| p.model == from);
                let new = models.iter().find(|p| p.model == choice.dispatched);
                let compatible = old.zip(new).is_some_and(|(a, b)| {
                    a.switch_group
                        .as_ref()
                        .is_some_and(|g| !g.is_empty() && Some(g) == b.switch_group.as_ref())
                });
                if !request.switch_safe || from.family() == HarnessFamily::Claude || !compatible {
                    return Err(invalid(
                        "native transcript does not permit this physical model switch",
                    ));
                }
            }
        }
        let mut history = self.configuration.clone();
        history.append(ConfigurationEntry {
            request_id: request.request_id.clone(),
            patches,
        })?;
        let configuration = history.at(None)?;
        configuration.authorize(authorized_tools)?;
        let request_json = render(&configuration, choice.dispatched)?;
        serde_json::from_str::<Value>(&request_json)?;
        let prepared = PreparedRequest {
            request,
            route: choice,
            request_json,
            configuration,
            original_request: None,
        };
        let mut next = self.clone();
        next.configuration = history;
        next.requests.push(prepared.clone());
        if next.requests.len() > MAX_RECEIPTS {
            next.requests.drain(..next.requests.len() - MAX_RECEIPTS);
        }
        bounded(&next)?;
        *self = next;
        Ok(prepared)
    }
}

fn validate_limits(
    request: &RouteRequest,
    model: HarnessModel,
    models: &[PhysicalModel],
) -> Result<()> {
    if models.iter().filter(|p| p.model == model).count() != 1 {
        return Err(invalid(
            "physical model must occur exactly once in the approved catalog",
        ));
    }
    let physical = models
        .iter()
        .find(|p| p.model == model)
        .expect("checked model");
    if request.output_tokens == 0
        || request.output_tokens > physical.max_output_tokens
        || request
            .input_tokens
            .checked_add(request.output_tokens)
            .is_none_or(|n| n > physical.context_tokens)
    {
        return Err(invalid("rendered request exceeds dispatched model limits"));
    }
    Ok(())
}

fn nonempty(value: &str) -> Result<()> {
    if value.is_empty() {
        Err(invalid(
            "configuration and request identities must be nonempty",
        ))
    } else {
        Ok(())
    }
}
fn invalid(message: &str) -> Error {
    Error::InvalidState(message.into())
}

/// Host-selected virtual route and named changes for the next native turn.
/// Router state and exact decisions are persisted in the native checkpoint.
#[derive(Clone)]
pub struct RequestPolicySettings {
    /// Public selection, distinct from the dispatched native model.
    pub selection: String,
    /// Changes admitted at a completed turn boundary.
    pub patches: Vec<ConfigurationPatch>,
    /// Host-approved physical identities and measured protocol limits.
    pub models: Vec<PhysicalModel>,
    /// Deterministic router evaluated once for each new turn.
    pub router: std::sync::Arc<dyn VirtualModelRouter + Send + Sync>,
}

/// Exact native preparation retained before external transport dispatch.
#[derive(Clone, Serialize, Deserialize)]
pub struct PreparedNativeRequest {
    /// Provider-native JSON with only supported configuration fields edited.
    pub request: Value,
    /// Persisted branch-local configuration history and routing receipts.
    pub state: Value,
}

impl RequestPolicySettings {
    /// Prepare supported native JSON without changing original transcript items.
    /// Retries replay the exact receipt, irrespective of a changed host router.
    pub fn prepare_native(
        &self,
        id: String,
        continuation: bool,
        switch_safe: bool,
        state: Value,
        request: Value,
        family: HarnessFamily,
    ) -> Result<PreparedNativeRequest> {
        bounded(&state)?;
        let mut state: RequestPolicyState = if state.is_null() {
            RequestPolicyState::default()
        } else {
            serde_json::from_value(state)?
        };
        let catalog: Vec<ToolDeclaration> = native_tools(&request)
            .iter()
            .map(|definition| {
                Ok(ToolDeclaration {
                    name: definition["name"]
                        .as_str()
                        .or_else(|| definition["type"].as_str())
                        .ok_or_else(|| invalid("native declaration lacks name"))?
                        .into(),
                    definition: definition.clone(),
                })
            })
            .collect::<Result<_>>()?;
        if let Some(saved) = state
            .requests
            .iter()
            .find(|entry| entry.request.request_id == id)
        {
            if saved.original_request.as_ref() != Some(&request)
                || saved.request.continuation_of.is_some() != continuation
            {
                return Err(invalid(
                    "request identity reused with different original native input",
                ));
            }
            saved.configuration.authorize(&catalog)?;
            let mut limits = saved.request.clone();
            limits.input_tokens = saved.request_json.len() as u64;
            validate_limits(&limits, saved.route.dispatched, &self.models)?;
            return Ok(PreparedNativeRequest {
                request: serde_json::from_str(&saved.request_json)?,
                state: serde_json::to_value(&state)?,
            });
        }
        let mut patches = if continuation {
            Vec::new()
        } else {
            self.patches.clone()
        };
        if state.requests.is_empty() {
            let mut initial = catalog
                .iter()
                .cloned()
                .map(|tool| ConfigurationPatch::SetTool { tool })
                .collect::<Vec<_>>();
            initial.append(&mut patches);
            patches = initial;
        }
        let predecessor = state.requests.last().map(|p| p.request.request_id.clone());
        if continuation && predecessor.is_none() {
            return Err(invalid("continuation requires a persisted predecessor"));
        }
        let original = request.clone();
        let output = request[if family == HarnessFamily::Claude {
            "max_tokens"
        } else {
            "max_output_tokens"
        }]
        .as_u64()
        .or_else(|| {
            (family == HarnessFamily::Codex)
                .then(|| {
                    self.models
                        .iter()
                        .filter(|physical| physical.model.family() == family)
                        .map(|physical| physical.max_output_tokens)
                        .min()
                })
                .flatten()
        })
        .ok_or_else(|| invalid("native request lacks output limit"))?;
        // A byte bound is conservative for inline textual requests. External
        // media has unknown token cost and needs a native measured estimator.
        reject_external_media(&request)?;
        let input = serde_json::to_vec(&request)?.len() as u64;
        let route_request = RouteRequest {
            request_id: id,
            selection: self.selection.clone(),
            continuation_of: if continuation { predecessor } else { None },
            input_tokens: input,
            output_tokens: output,
            switch_safe,
        };
        let mut prepared = state.prepare(route_request, patches, &self.models, &catalog,
            &|input: RoutingInput<'_>| self.router.route(input), |configuration, model| {
                if model.family() != family { return Err(invalid("native transport and dispatched model family differ")); }
                let mut rendered = original.clone();
                if family == HarnessFamily::Claude {
                    let mut system = match rendered.get("system") {
                        None | Some(Value::Null) => Vec::new(),
                        Some(Value::String(text)) => vec![serde_json::json!({"type":"text", "text":text})],
                        Some(Value::Array(blocks)) => blocks.clone(),
                        _ => return Err(invalid("unsupported native system blocks")),
                    };
                    system.extend(configuration.sections.iter().map(|section| serde_json::json!({"type":"text", "text":section.text})));
                    if !system.is_empty() { rendered["system"] = Value::Array(system); }
                } else if rendered["input"].as_array().is_some_and(|items| items.iter().any(|item| item["type"] == "additional_tools")) {
                    let items = rendered["input"].as_array_mut().expect("checked Lite input");
                    let index = items.iter().position(|item| item["type"] == "additional_tools").expect("checked declaration");
                    let definitions = Value::Array(configuration.tools.iter().map(|tool| tool.definition.clone()).collect());
                    if items[index]["tools"] != definitions {
                        items[index]["tools"] = definitions;
                        // A changed declaration must not reuse a provider item identity.
                        items[index].as_object_mut().expect("declaration object").remove("id");
                    }
                    if !configuration.sections.is_empty() {
                        let position = items.iter().position(|item| item["type"] == "message" && item["role"] == "developer")
                            .map_or(index + 1, |position| position + 1);
                        items.insert(position, serde_json::json!({"type":"message", "role":"developer",
                            "content":[{"type":"input_text", "text":configuration.instructions()}]}));
                    }
                } else {
                    let base = rendered["instructions"].as_str().unwrap_or("");
                    let additional = configuration.instructions();
                    if !additional.is_empty() {
                        rendered["instructions"] = if base.is_empty() { additional } else { format!("{base}\n\n{additional}") }.into();
                    }
                }
                if !(family == HarnessFamily::Codex && rendered["input"].as_array().is_some_and(|items| items.iter().any(|item| item["type"] == "additional_tools")))
                    && (rendered.get("tools").is_some() || !configuration.tools.is_empty()) {
                    rendered["tools"] = Value::Array(configuration.tools.iter().map(|tool| tool.definition.clone()).collect());
                }
                if family == HarnessFamily::Codex { rendered["max_output_tokens"] = output.into(); }
                rendered["model"] = model.as_str().into();
                validate_native_controls(&rendered, model)?;
                Ok(serde_json::to_string(&rendered)?)
            })?;
        let mut limits = prepared.request.clone();
        limits.input_tokens = prepared.request_json.len() as u64;
        validate_limits(&limits, prepared.route.dispatched, &self.models)?;
        prepared.original_request = Some(original);
        *state.requests.last_mut().expect("new receipt") = prepared.clone();
        bounded(&state)?;
        Ok(PreparedNativeRequest {
            request: serde_json::from_str(&prepared.request_json)?,
            state: serde_json::to_value(state)?,
        })
    }

    #[cfg(feature = "claude")]
    pub(crate) fn prepare_claude(
        &self,
        id: String,
        continuation: bool,
        state: Value,
        request: Value,
    ) -> Result<nanocodex_claude::execution::RequestPreparation> {
        let prepared = self.prepare_native(
            id,
            continuation,
            false,
            state,
            request,
            HarnessFamily::Claude,
        )?;
        Ok(nanocodex_claude::execution::RequestPreparation {
            request: prepared.request,
            state: prepared.state,
        })
    }
}

fn reject_external_media(value: &Value) -> Result<()> {
    match value {
        Value::Object(object) => {
            if object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    matches!(
                        kind,
                        "image"
                            | "document"
                            | "input_image"
                            | "input_audio"
                            | "input_file"
                            | "audio"
                    )
                })
            {
                return Err(invalid(
                    "media requests require native measured token limits",
                ));
            }
            for value in object.values() {
                reject_external_media(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                reject_external_media(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_native_controls(request: &Value, model: HarnessModel) -> Result<()> {
    // A model rewrite must never silently reinterpret adaptive thinking or speed.
    if (request["speed"] == "fast" || request["service_tier"] == "priority")
        && !model.supports_fast_mode()
    {
        return Err(invalid(
            "dispatched model does not support requested fast mode",
        ));
    }
    let effort = request["output_config"]["effort"]
        .as_str()
        .or_else(|| request["reasoning"]["effort"].as_str());
    if let Some(effort) = effort {
        let thinking =
            serde_json::from_value::<nanocodex_agent::Thinking>(Value::String(effort.into()))?;
        if !model.supports_thinking(thinking) {
            return Err(invalid(
                "dispatched model does not support requested thinking effort",
            ));
        }
    }
    if model == HarnessModel::Claude(nanocodex_agent::ClaudeModel::Haiku45)
        && request["thinking"]["type"]
            .as_str()
            .is_some_and(|kind| kind != "disabled")
    {
        return Err(invalid(
            "dispatched model does not support adaptive thinking",
        ));
    }
    Ok(())
}

/// Optional request policy installed on the Claude-native durable builder.
#[cfg(feature = "claude")]
pub trait DurableClaudeRequestExt: Sized {
    /// Acquire the native durable owner and install persisted request decisions.
    fn durability_with_request_policy(
        self,
        state: crate::DurableSession,
        settings: RequestPolicySettings,
    ) -> impl std::future::Future<Output = nanocodex_agent::Result<Self>>;
    /// Explicitly enable economically justified cache warming on native requests.
    /// The live client supplies current authentication; credentials are never journaled.
    fn durability_with_request_policy_and_cache_warm(
        self,
        state: crate::DurableSession,
        settings: RequestPolicySettings,
        client: nanocodex_claude::ClaudeClient,
        policy: crate::cache_warm::CacheWarmPolicy,
    ) -> impl std::future::Future<Output = nanocodex_agent::Result<Self>>;
}

/// Optional request policy installed on an OpenAI-native durable builder.
pub trait DurableOpenAiRequestExt: Sized {
    /// Acquire the durable owner and freeze native configuration/routing before dispatch.
    fn durability_with_request_policy(
        self,
        state: crate::DurableSession,
        settings: RequestPolicySettings,
    ) -> impl std::future::Future<Output = nanocodex_agent::Result<Self>>;
}
