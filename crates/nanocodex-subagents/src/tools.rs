// Derived from clabby/tact@1d9ccaefd1d8613dab020812af04a91cd9b4c52c (Apache-2.0).
// Modified for Nanocodex's reusable native/WASM extension runtime.

use super::{
    message::MAX_MESSAGE_BYTES,
    model::{
        AgentDescriptor, AgentId, AgentStatus, AgentUpdate, MessageId, MessagePriority,
        MessagePurpose, agent_prompt,
    },
    runtime::{
        AgentDirectoryEntry, AgentSummary, OutputContract, Registry, SubmissionOutcome,
        forward_events,
    },
};
use async_trait::async_trait;
use futures_util::future::join_all;
use nanocodex_agent::{
    AgentHandle, ForkRequest, HarnessFamily, HarnessModel, SpawnOptions, Thinking,
};
use nanocodex_oai_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, Tools,
    runtime::ToolsBuildError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::sync::oneshot;

/// Wait used when the caller omits timeout_ms. An explicit timeout is honored
/// as requested, without a ceiling.
const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const SPAWN_AGENT_TOOL: &str = "spawn_agent";
const SUBMIT_RESULT_TOOL: &str = "submit_result";
const SEND_AGENT_MESSAGE_TOOL: &str = "send_agent_message";
const LIST_AGENTS_TOOL: &str = "list_agents";
const WAIT_AGENT_TOOL: &str = "wait_agent";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTask {
    pub role: String,
    pub task: String,
    pub output_schema: Value,
}

/// A closed, recursively typed model-facing language for child result shapes.
/// No caller-authored JSON Schema object is exposed to strict function calling.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OutputContractNode {
    Object { fields: Vec<OutputContractField> },
    Array { items: Box<Self> },
    String,
    StringEnum { values: Vec<String> },
    Integer,
    Number,
    Boolean,
    Null,
    Any,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputContractField {
    name: String,
    schema: OutputContractNode,
    required: bool,
}

impl OutputContractNode {
    fn into_schema(self) -> Value {
        match self {
            Self::Object { fields } => {
                // Last declaration wins on duplicate names, including whether
                // it is required, so every strict-valid input yields one schema.
                let mut deduplicated = BTreeMap::new();
                for field in fields {
                    deduplicated.insert(field.name, (field.schema, field.required));
                }
                let mut properties = Map::new();
                let mut required = Vec::new();
                for (name, (schema, is_required)) in deduplicated {
                    if is_required {
                        required.push(name.clone());
                    }
                    properties.insert(name, schema.into_schema());
                }
                json!({ "type": "object", "properties": properties,
                    "required": required, "additionalProperties": false })
            }
            Self::Array { items } => json!({ "type": "array", "items": items.into_schema() }),
            Self::String => json!({ "type": "string" }),
            Self::StringEnum { values } => {
                if values.is_empty() {
                    json!({ "type": "string" })
                } else {
                    json!({ "type": "string", "enum": values })
                }
            }
            Self::Integer => json!({ "type": "integer" }),
            Self::Number => json!({ "type": "number" }),
            Self::Boolean => json!({ "type": "boolean" }),
            Self::Null => json!({ "type": "null" }),
            Self::Any => Value::Bool(true),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentTask {
    role: String,
    task: String,
    #[serde(default)]
    model: Option<HarnessModel>,
    #[serde(default)]
    harness: Option<HarnessFamily>,
    #[serde(default)]
    thinking: Option<Thinking>,
    // Old in-flight calls can finish after deployment. New model declarations
    // expose only the typed contract, while trusted Rust/JS APIs retain raw
    // schemas through `start_agent` rather than this tool.
    #[serde(default)]
    output_schema: Option<Value>,
    #[serde(default)]
    output_contract: Option<OutputContractNode>,
}

impl SpawnAgentTask {
    fn into_parts(self) -> std::io::Result<(AgentTask, SpawnOptions)> {
        let output_schema = match (self.output_contract, self.output_schema) {
            (Some(contract), None) => contract.into_schema(),
            (None, Some(schema)) => schema,
            _ => {
                return Err(std::io::Error::other(
                    "provide exactly one of output_contract or legacy output_schema",
                ));
            }
        };
        let mut options = SpawnOptions::new();
        if let Some(model) = self.model {
            options = options.harness_model(model);
        }
        if let Some(harness) = self.harness {
            options = options.harness(harness);
        }
        if let Some(thinking) = self.thinking {
            options = options.thinking(thinking);
        }
        options.validate_harness().map_err(std::io::Error::other)?;
        Ok((
            AgentTask {
                role: self.role,
                task: self.task,
                output_schema,
            },
            options,
        ))
    }
}

#[derive(Serialize)]
pub struct AgentStartReport {
    pub agent_id: AgentId,
    pub role: String,
    pub status: AgentStatus,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitTask {
    agent_ids: Vec<AgentId>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetAgent {
    agent_id: AgentId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryTask {
    #[serde(default)]
    include_completed: bool,
    #[serde(default)]
    include_self: bool,
}

#[derive(Serialize)]
struct AgentDirectory {
    agents: Vec<AgentDirectoryEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendMessageTask {
    agent_id: AgentId,
    message: String,
    #[serde(default)]
    priority: MessagePriority,
    #[serde(default)]
    purpose: MessagePurpose,
    #[serde(default)]
    in_reply_to: Option<MessageId>,
}

#[derive(Serialize)]
struct WaitReport {
    agents: Vec<AgentSummary>,
    timed_out: bool,
}

#[derive(Serialize)]
struct LifecycleReport {
    agents: Vec<AgentSummary>,
}

fn json_output(value: &impl Serialize) -> ToolResult {
    Ok(ToolOutput::from_json(serde_json::to_value(value)?, true))
}

pub type AgentToolResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync + 'static>>;

pub async fn start_agent(
    parent: &AgentHandle,
    registry: &Arc<Registry>,
    session_id: &str,
    task: AgentTask,
) -> AgentToolResult<AgentStartReport> {
    start_agent_with(parent, registry, session_id, task, SpawnOptions::new()).await
}

/// Starts a stable, ordered batch of clean-room subagents.
///
/// Every task schema and every available turn slot is validated and reserved
/// before any child is created or its initial turn is launched.
///
/// # Errors
///
/// Returns an error without launching a child when validation or reservation
/// fails. If a later lifecycle step fails, already-created children are closed
/// before returning that error where the runtime remains available.
pub async fn start_agents(
    parent: &AgentHandle,
    registry: &Arc<Registry>,
    session_id: &str,
    tasks: Vec<AgentTask>,
) -> AgentToolResult<Vec<AgentStartReport>> {
    start_agents_observed(parent, registry, session_id, tasks, |_| {}).await
}

/// Equivalent to [`start_agents`], with one synchronous observation of every
/// child session created for cancellation-safe embedding cleanup.
#[doc(hidden)]
pub async fn start_agents_observed(
    parent: &AgentHandle,
    registry: &Arc<Registry>,
    session_id: &str,
    tasks: Vec<AgentTask>,
    observe_session: impl Fn(&str) + Send + Sync + 'static,
) -> AgentToolResult<Vec<AgentStartReport>> {
    registry.register_handle(parent.clone());
    registry.await_restored(session_id).await?;
    let prepared = prepare_batch(tasks)?;
    let mut startup = registry.batch_startup();
    let capacities = registry.reserve_turns(prepared.len())?;
    let reservations = registry.reserve_many(session_id, prepared.len()).await?;
    let host_context = registry.host_context_for_session(session_id).await;
    let children = if let Some(router) = registry.spawn_router() {
        // Resolve all choices before creating a child. No initial turn runs until
        // the entire batch has been bound and inserted below.
        let mut routes = Vec::with_capacity(prepared.len());
        for (task, _) in &prepared {
            let route = router
                .resolve_spawn(
                    session_id,
                    &task.role,
                    &task.task,
                    SpawnOptions::new(),
                    host_context.as_deref(),
                )
                .await?;
            route.validate(SpawnOptions::new())?;
            routes.push(route);
        }
        let mut children: Vec<(nanocodex_agent::Nanocodex, nanocodex_agent::AgentEvents)> =
            Vec::with_capacity(routes.len());
        for route in routes {
            let outcome = parent
                .spawn_with_host_context(
                    route.options(SpawnOptions::new()),
                    host_context.as_ref().map(Arc::clone),
                )
                .await;
            let child = match outcome {
                Ok(child) => child,
                Err(error) => {
                    for (child, _) in &children {
                        child.abandon_created().await;
                    }
                    return Err(error.into());
                }
            };
            observe_session(child.0.session_id());
            if let Err(error) = router.bind(
                session_id,
                child.0.session_id(),
                route.reference(),
                host_context.as_deref(),
            ) {
                child.0.abandon_created().await;
                for (child, _) in &children {
                    child.abandon_created().await;
                }
                return Err(error.into());
            }
            children.push(child);
        }
        children
    } else {
        parent
            .spawn_many_observed_with_host_context(
                prepared.len(),
                observe_session,
                host_context.as_ref().map(Arc::clone),
            )
            .await?
    };

    let mut reports = Vec::with_capacity(prepared.len());
    let mut launches = Vec::with_capacity(prepared.len());
    let mut additions = Vec::with_capacity(prepared.len());
    for (((reservation, (task, contract)), (child, events)), capacity) in reservations
        .into_iter()
        .zip(prepared)
        .zip(children)
        .zip(capacities)
    {
        let id = reservation.id;
        let descriptor = AgentDescriptor {
            id,
            session_id: child.session_id().to_string(),
            role: task.role.clone(),
            task: task.task.clone(),
            parent: reservation.parent,
        };
        let (start_events, events_ready) = oneshot::channel();
        let event_task = forward_events(
            reservation.root_session_id.clone(),
            id,
            events,
            events_ready,
            Arc::downgrade(registry),
            registry.updates.clone(),
        );
        if let Err(error) = registry
            .insert(
                reservation.root_session_id.clone(),
                descriptor.clone(),
                host_context.as_ref().map(Arc::clone),
                child,
                event_task,
                contract,
            )
            .await
        {
            startup.rollback().await;
            return Err(error.into());
        }
        startup.track(&reservation.root_session_id, id);
        reports.push(AgentStartReport {
            agent_id: id,
            role: task.role,
            status: AgentStatus::Running,
        });
        additions.push((
            reservation.root_session_id.clone(),
            descriptor,
            start_events,
        ));
        launches.push((
            reservation.root_session_id,
            id,
            agent_prompt(id, &task.task),
            capacity,
        ));
    }

    for (root_session_id, descriptor, start_events) in additions {
        registry.send(&root_session_id, AgentUpdate::Added(descriptor));
        let _ = start_events.send(());
    }

    let launches = launches
        .into_iter()
        .map(|(root_session_id, id, prompt, capacity)| async move {
            registry
                .launch_initial_turn(&root_session_id, id, prompt, capacity)
                .await
        });
    if let Some(error) = join_all(launches).await.into_iter().find_map(Result::err) {
        startup.rollback().await;
        return Err(error.into());
    }
    startup.commit();
    Ok(reports)
}

fn prepare_batch(tasks: Vec<AgentTask>) -> AgentToolResult<Vec<(AgentTask, OutputContract)>> {
    tasks
        .into_iter()
        .map(|task| {
            let contract = OutputContract::compile(&task.output_schema)?;
            Ok((task, contract))
        })
        .collect()
}

pub async fn start_agent_with(
    parent: &AgentHandle,
    registry: &Arc<Registry>,
    session_id: &str,
    task: AgentTask,
    options: SpawnOptions,
) -> AgentToolResult<AgentStartReport> {
    let host_context = registry.host_context_for_session(session_id).await;
    start_agent_with_host_context(parent, registry, session_id, task, options, host_context).await
}

/// Starts a native conversation fork in the existing task tree. The provider
/// owns the safe fork boundary and preserves its model; mixed-family routing
/// and model overrides do not apply. Output stays on the child until retrieved.
pub async fn start_fork_agent(
    parent: &AgentHandle,
    registry: &Arc<Registry>,
    session_id: &str,
    task: AgentTask,
) -> AgentToolResult<AgentStartReport> {
    let host_context = registry.host_context_for_session(session_id).await;
    start_child(
        parent,
        registry,
        session_id,
        task,
        SpawnOptions::new(),
        host_context,
        true,
    )
    .await
}

async fn start_agent_with_host_context(
    parent: &AgentHandle,
    registry: &Arc<Registry>,
    session_id: &str,
    task: AgentTask,
    options: SpawnOptions,
    host_context: Option<Arc<str>>,
) -> AgentToolResult<AgentStartReport> {
    start_child(
        parent,
        registry,
        session_id,
        task,
        options,
        host_context,
        false,
    )
    .await
}

async fn start_child(
    parent: &AgentHandle,
    registry: &Arc<Registry>,
    session_id: &str,
    task: AgentTask,
    options: SpawnOptions,
    host_context: Option<Arc<str>>,
    fork: bool,
) -> AgentToolResult<AgentStartReport> {
    if fork && parent.session_id() != session_id {
        return Err("child caller identity must match its native parent handle".into());
    }
    registry.register_handle(parent.clone());
    registry.await_restored(session_id).await?;
    let AgentTask {
        role,
        task,
        output_schema,
    } = task;
    let contract = OutputContract::compile(&output_schema)?;
    let capacity = registry.reserve_turn()?;
    let reservation = registry.reserve(session_id).await?;
    let id = reservation.id;
    let host_context = match host_context {
        Some(host_context) => Some(host_context),
        None => registry.host_context_for_session(session_id).await,
    };
    let router = if fork { None } else { registry.spawn_router() };
    let route = if let Some(router) = &router {
        let route = router
            .resolve_spawn(session_id, &role, &task, options, host_context.as_deref())
            .await?;
        route.validate(options)?;
        Some(route)
    } else {
        None
    };
    let (child, events) = if fork {
        parent.fork(ForkRequest::latest()).await?
    } else {
        parent
            .spawn_with_host_context(
                route
                    .as_ref()
                    .map_or(options, |route| route.options(options)),
                host_context.as_ref().map(Arc::clone),
            )
            .await?
    };
    if let (Some(router), Some(route)) = (&router, &route)
        && let Err(error) = router.bind(
            session_id,
            child.session_id(),
            route.reference(),
            host_context.as_deref(),
        )
    {
        let _ = child.shutdown().await;
        return Err(error.into());
    }
    let session_id = child.session_id().to_string();
    let descriptor = AgentDescriptor {
        id,
        session_id,
        role: role.clone(),
        task: task.clone(),
        parent: reservation.parent,
    };
    let (start_events, events_ready) = oneshot::channel();
    let event_task = forward_events(
        reservation.root_session_id.clone(),
        id,
        events,
        events_ready,
        Arc::downgrade(registry),
        registry.updates.clone(),
    );
    registry
        .insert(
            reservation.root_session_id.clone(),
            descriptor.clone(),
            host_context,
            child,
            event_task,
            contract,
        )
        .await?;
    registry.send(&reservation.root_session_id, AgentUpdate::Added(descriptor));
    let _ = start_events.send(());

    registry
        .launch_initial_turn(
            &reservation.root_session_id,
            id,
            agent_prompt(id, &task),
            capacity,
        )
        .await?;
    Ok(AgentStartReport {
        agent_id: id,
        role,
        status: AgentStatus::Running,
    })
}

struct SpawnAgent {
    parent: AgentHandle,
    registry: Weak<Registry>,
}

#[async_trait]
impl Tool for SpawnAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            SPAWN_AGENT_TOOL,
            "Starts a reusable clean-room subagent without inherited conversation history and immediately returns its ID. When the root agent is durable, the task tree survives runtime restarts with the same IDs and interrupted children resume from their latest committed checkpoint; otherwise children are dropped when the parent runtime restarts.",
            spawn_agent_parameters(),
        )
        .with_strict_parameters()
        .with_output_schema(spawn_agent_output_schema())
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let (task, options) = input.decode_json::<SpawnAgentTask>()?.into_parts()?;
        let host_context = context.host_context().map(Arc::<str>::from);
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| std::io::Error::other("subagent runtime is closed"))?;
        #[cfg(not(target_family = "wasm"))]
        let report = start_agent_with_host_context(
            &self.parent,
            &registry,
            context.session_id(),
            task,
            options,
            host_context,
        )
        .await?;
        // Tool futures are Send, while host JS routing and shutdown futures are
        // isolate-local. Poll them on the WASM executor and await a Send receipt.
        // Dropping the tool still cancels startup instead of detaching it.
        #[cfg(target_family = "wasm")]
        let report = {
            let parent = self.parent.clone();
            let session_id = context.session_id().to_owned();
            let pending = super::platform::spawn(async move {
                start_agent_with_host_context(
                    &parent,
                    &registry,
                    &session_id,
                    task,
                    options,
                    host_context,
                )
                .await
            });
            let _cancel = pending.abort_on_drop();
            pending
                .await
                .map_err(|_| std::io::Error::other("subagent startup was cancelled"))??
        };
        json_output(&report)
    }
}

fn spawn_agent_parameters() -> Value {
    let models = [HarnessFamily::Codex, HarnessFamily::Claude]
        .into_iter()
        .flat_map(HarnessModel::for_family)
        .map(|model| Some(model.as_str()))
        .chain([Some("glm-5.3"), Some("kimi"), Some("mimo"), None])
        .collect::<Vec<_>>();
    json!({
        "type": "object",
        "properties": {
            "role": { "type": "string", "description": "A short role describing the subagent's specialty." },
            "task": { "type": "string", "description": "A complete, focused task for the subagent." },
            "harness": {
                "type": ["string", "null"], "enum": ["codex", "claude", null],
                "description": "Native agent-loop family; null inherits the parent's family. A family switch selects that family's defaults."
            },
            "model": {
                "type": ["string", "null"],
                "enum": models,
                "description": "Model must belong to the selected harness. Null inherits within the same family; a family switch uses its default model."
            },
            "thinking": {
                "type": ["string", "null"],
                "enum": ["none", "low", "medium", "high", "xhigh", "max", null],
                "description": "Reasoning override; null inherits the parent's level."
            },
            "output_contract": { "$ref": "#/$defs/node" }
        },
        "required": ["role", "task", "harness", "model", "thinking", "output_contract"],
        "additionalProperties": false,
        "$defs": {
            "node": { "anyOf": [
                { "$ref": "#/$defs/object" },
                { "$ref": "#/$defs/array" },
                { "$ref": "#/$defs/string_enum" },
                { "$ref": "#/$defs/scalar" }
            ] },
            "object": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "object" },
                    "fields": { "type": "array", "items": { "$ref": "#/$defs/field" } }
                },
                "required": ["kind", "fields"], "additionalProperties": false
            },
            "array": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "array" },
                    "items": { "$ref": "#/$defs/node" }
                },
                "required": ["kind", "items"], "additionalProperties": false
            },
            "string_enum": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "string_enum" },
                    "values": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["kind", "values"], "additionalProperties": false
            },
            "scalar": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": [
                        "string", "integer", "number", "boolean", "null", "any"
                    ] }
                },
                "required": ["kind"], "additionalProperties": false
            },
            "field": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "schema": { "$ref": "#/$defs/node" },
                    "required": { "type": "boolean" }
                },
                "required": ["name", "schema", "required"], "additionalProperties": false
            }
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitResultArgs {
    output: Value,
}

struct SubmitResult {
    registry: Weak<Registry>,
}

#[async_trait]
impl Tool for SubmitResult {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            SUBMIT_RESULT_TOOL,
            "Submits the current child subagent turn's final JSON output. This tool is unavailable to the root agent; root agents return final output as assistant text. Supply only output matching the task schema. After an accepted receipt, send a brief final assistant message with no further tool calls; do not finish with an empty response. If superseded, incorporate pending instructions and submit the updated result. Invalid values can be corrected and retried.",
            json!({
                "type": "object",
                "properties": {
                    "output": {
                        "anyOf": [
                            { "type": "object" }, { "type": "array", "items": {} },
                            { "type": "string" }, { "type": "number" },
                            { "type": "boolean" }, { "type": "null" }
                        ],
                        "description": "The final JSON value required by this agent's output schema. Pass objects and arrays directly, not as JSON-encoded strings. Use a string only when the output schema permits a string. A JSON-encoded object or array is decoded once only if it matches the required schema; the receipt reports decoded_json_text."

                    }
                },
                "required": ["output"],
                "additionalProperties": false
            }),
        )
        .with_output_schema(json!({
            "type": "object",
            "properties": {
                "accepted": { "type": "boolean" },
                "status": { "type": "string", "enum": ["accepted", "superseded"] },
                "decoded_json_text": { "type": "boolean", "const": true }
            },
            "required": ["accepted", "status"],
            "additionalProperties": false
        }))
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let SubmitResultArgs { output } = input.decode_json()?;
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| std::io::Error::other("subagent runtime is closed"))?;
        let outcome = registry
            .submit_result(context.session_id(), context.instruction_revision(), output)
            .await?;
        let output = match outcome {
            SubmissionOutcome::Accepted { decoded_json_text } => {
                let mut receipt = json!({ "accepted": true, "status": "accepted" });
                if decoded_json_text {
                    receipt["decoded_json_text"] = json!(true);
                }
                receipt
            }
            SubmissionOutcome::Superseded => json!({ "accepted": false, "status": "superseded" }),
        };
        Ok(ToolOutput::from_json(output, true))
    }
}

struct SendAgentMessage {
    registry: Weak<Registry>,
}

#[async_trait]
impl Tool for SendAgentMessage {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            SEND_AGENT_MESSAGE_TOOL,
            "Sends a bounded directed message to any other agent in the same task tree. Deferred messages start an idle agent or queue behind its active turn. If a send is queued, do not wait for it inside the current turn; finish the turn so queued messages can be delivered. Urgent messages steer a running agent at its next safe model boundary. Delegate messages replace the recipient's assigned task, retain its output schema, and require management authority.",
            json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "The recipient from list_agents. Any non-closing agent in the same task tree can receive coordination messages."
                    },
                    "message": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_MESSAGE_BYTES,
                        "description": "The focused message body. The runtime enforces a 64 KiB UTF-8 limit."
                    },
                    "priority": {
                        "type": "string",
                        "enum": ["deferred", "urgent"],
                        "default": "deferred",
                        "description": "Urgent steers an active turn; deferred preserves turn boundaries. A queued deferred send requires the current turn to finish before delivery."
                    },
                    "purpose": {
                        "type": "string",
                        "enum": ["delegate", "coordinate", "finding", "question", "reply"],
                        "default": "coordinate",
                        "description": "A typed coordination intent, independent of thread correlation. Reply requires in_reply_to and reverses the referenced message direction. Delegate is restricted to agents the sender can manage."
                    },
                    "in_reply_to": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Continue the referenced two-party thread without changing purpose. Non-reply messages may continue in either direction, including follow-ups to your own messages. With purpose=reply, answer a received message and reverse its direction."
                    }
                },
                "required": ["agent_id", "message"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let SendMessageTask {
            agent_id,
            message,
            priority,
            purpose,
            in_reply_to,
        } = input.decode_json()?;
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| std::io::Error::other("subagent runtime is closed"))?;
        #[cfg(not(target_family = "wasm"))]
        let receipt = registry
            .send_message(
                context.session_id(),
                agent_id,
                priority,
                purpose,
                in_reply_to,
                message,
            )
            .await?;
        #[cfg(target_family = "wasm")]
        let receipt = {
            let session_id = context.session_id().to_owned();
            let pending = super::platform::spawn(async move {
                registry
                    .send_message(
                        &session_id,
                        agent_id,
                        priority,
                        purpose,
                        in_reply_to,
                        message,
                    )
                    .await
            });
            let _cancel = pending.abort_on_drop();
            pending
                .await
                .map_err(|_| std::io::Error::other("subagent message was cancelled"))??
        };
        json_output(&receipt)
    }
}

struct ListAgents {
    registry: Weak<Registry>,
}

#[async_trait]
impl Tool for ListAgents {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            LIST_AGENTS_TOOL,
            "Lists a compact directory of agents in the same task tree. Pending, running, and recoverable interrupted agents are returned by default; other retained agents can be included when a follow-up message is needed.",
            json!({
                "type": "object",
                "properties": {
                    "include_completed": {
                        "type": "boolean",
                        "default": false,
                        "description": "Also includes completed, failed, closed, and nonrecoverable interrupted agents."
                    },
                    "include_self": {
                        "type": "boolean",
                        "default": false,
                        "description": "Includes the calling agent for topology inspection. Self-messaging remains unavailable."
                    }
                },
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let DirectoryTask {
            include_completed,
            include_self,
        } = input.decode_json()?;
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| std::io::Error::other("subagent runtime is closed"))?;
        json_output(&AgentDirectory {
            agents: registry
                .directory(context.session_id(), include_completed, include_self)
                .await?,
        })
    }
}

struct WaitAgent {
    registry: Weak<Registry>,
}

#[async_trait]
impl Tool for WaitAgent {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            WAIT_AGENT_TOOL,
            "Waits until any requested subagent reaches a terminal status and returns a snapshot of every requested agent. The result includes pending, running, and closing agents; consumers must preserve those nonterminal agents and act only on completed, failed, interrupted, or closed entries. Each entry's status is an object; read its terminal state from status.state. Only results this caller has not yet received end the wait: already-reported terminal agents do not, so waiting again with the same IDs blocks for the next result. When every requested agent is terminal and already reported, the snapshot returns immediately; repeating that identical call errors because nothing is left to wait for. Use one call with multiple IDs instead of polling the workspace.",
            json!({
                "type": "object",
                "properties": {
                    "agent_ids": {
                        "type": "array",
                        "items": { "type": "integer", "minimum": 1 },
                        "minItems": 1,
                        "description": "Agent IDs returned by spawn_agent. Waiting returns when any one becomes terminal."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Wait in milliseconds. Defaults to 30000."
                    }
                },
                "required": ["agent_ids"],
                "additionalProperties": false
            }),
        )
        .with_output_schema(wait_agent_output_schema())
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let WaitTask {
            agent_ids,
            timeout_ms,
        } = input.decode_json()?;
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| std::io::Error::other("subagent runtime is closed"))?;
        let duration = timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_WAIT_TIMEOUT);
        let (agents, timed_out) = registry
            .wait(context.session_id(), &agent_ids, duration)
            .await?;
        json_output(&WaitReport { agents, timed_out })
    }
}

#[derive(Clone, Copy)]
enum LifecycleOperation {
    Interrupt,
    Close,
}

struct ChangeAgentLifecycle {
    registry: Weak<Registry>,
    operation: LifecycleOperation,
}

impl ChangeAgentLifecycle {
    const fn tool_name(&self) -> &'static str {
        match self.operation {
            LifecycleOperation::Interrupt => "interrupt_agent",
            LifecycleOperation::Close => "close_agent",
        }
    }
}

#[async_trait]
impl Tool for ChangeAgentLifecycle {
    fn definition(&self) -> ToolDefinition {
        let description = match self.operation {
            LifecycleOperation::Interrupt => {
                "Interrupts an agent's active turn and every active descendant, waits for their model and tool resources to stop, and keeps the sessions reusable within the running parent runtime."
            }
            LifecycleOperation::Close => {
                "Closes an agent and its entire descendant subtree, waiting for active model and tool resources to stop before returning. Closed agents remain inspectable within the running parent runtime but are not reusable."
            }
        };
        ToolDefinition::function(
            self.tool_name(),
            description,
            json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "The root of the subagent subtree to stop."
                    }
                },
                "required": ["agent_id"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let TargetAgent { agent_id } = input.decode_json()?;
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| std::io::Error::other("subagent runtime is closed"))?;
        let agents = match self.operation {
            LifecycleOperation::Interrupt => {
                registry.interrupt(context.session_id(), agent_id).await?
            }
            LifecycleOperation::Close => registry.close(context.session_id(), agent_id).await?,
        };
        json_output(&LifecycleReport { agents })
    }
}

fn shared_tools(parent: AgentHandle, registry: &Arc<Registry>) -> Vec<Arc<dyn Tool>> {
    registry.register_handle(parent.clone());
    vec![
        Arc::new(SubmitResult {
            registry: Arc::downgrade(registry),
        }),
        Arc::new(SpawnAgent {
            parent,
            registry: Arc::downgrade(registry),
        }),
        Arc::new(SendAgentMessage {
            registry: Arc::downgrade(registry),
        }),
        Arc::new(ListAgents {
            registry: Arc::downgrade(registry),
        }),
        Arc::new(WaitAgent {
            registry: Arc::downgrade(registry),
        }),
        Arc::new(ChangeAgentLifecycle {
            registry: Arc::downgrade(registry),
            operation: LifecycleOperation::Interrupt,
        }),
        Arc::new(ChangeAgentLifecycle {
            registry: Arc::downgrade(registry),
            operation: LifecycleOperation::Close,
        }),
    ]
}

struct SharedTool(Arc<dyn Tool>);
#[async_trait]
impl Tool for SharedTool {
    fn definition(&self) -> ToolDefinition {
        self.0.definition()
    }
    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        self.0.execute(input, context).await
    }
}

/// Installs the task-tree operations into a Responses tool runtime.
pub fn install_tools(
    tools: Tools,
    parent: AgentHandle,
    registry: Arc<Registry>,
) -> Result<Tools, ToolsBuildError> {
    let mut builder = tools.into_builder();
    for tool in shared_tools(parent, &registry) {
        builder = builder.tool(SharedTool(tool));
    }
    builder.build()
}

/// Installs the identical task-tree operations as native Claude callbacks.
#[cfg(feature = "claude")]
pub fn install_claude_tools(
    mut tools: nanocodex_claude::ClaudeTools,
    parent: AgentHandle,
    registry: Arc<Registry>,
) -> nanocodex_agent::Result<nanocodex_claude::ClaudeTools> {
    for tool in shared_tools(parent, &registry) {
        tools = tools.shared_tool(TurnAuthority(tool))?;
    }
    Ok(tools)
}

/// Claude hosts may run without host context. A child spawned there retains
/// its spawning turn as its authority instead.
#[cfg(feature = "claude")]
struct TurnAuthority(Arc<dyn Tool>);

#[cfg(feature = "claude")]
#[async_trait]
impl Tool for TurnAuthority {
    fn definition(&self) -> ToolDefinition {
        self.0.definition()
    }
    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let host_context = context.host_context().or(context.turn_id());
        self.0
            .execute(input, context.with_host_context(host_context))
            .await
    }
}

fn spawn_agent_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "agent_id": { "type": "integer" },
            "role": { "type": "string" },
            "status": {
                "type": "object",
                "properties": { "state": { "type": "string", "const": "running" } },
                "required": ["state"],
                "additionalProperties": false
            }
        },
        "required": ["agent_id", "role", "status"],
        "additionalProperties": false
    })
}

fn wait_agent_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "agents": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "agent_id": { "type": "integer" },
                        "role": { "type": "string" },
                        "task": { "type": "string" },
                        "parent_agent_id": { "type": ["integer", "null"] },
                        "status": agent_status_schema(),
                        "last_output": {}
                    },
                    "required": ["agent_id", "role", "task", "parent_agent_id", "status"],
                    "additionalProperties": false
                }
            },
            "timed_out": { "type": "boolean" }
        },
        "required": ["agents", "timed_out"],
        "additionalProperties": false
    })
}

fn agent_status_schema() -> Value {
    let state_only = ["pending", "running", "interrupted", "closing", "closed"].map(|state| {
        json!({
            "type": "object",
            "properties": { "state": { "type": "string", "const": state } },
            "required": ["state"],
            "additionalProperties": false
        })
    });
    let mut variants = state_only.into_iter().collect::<Vec<_>>();
    variants.push(json!({
        "type": "object",
        "properties": {
            "state": { "type": "string", "const": "completed" },
            "output": {}
        },
        "required": ["state", "output"],
        "additionalProperties": false
    }));
    variants.push(json!({
        "type": "object",
        "properties": {
            "state": { "type": "string", "const": "failed" },
            "error": { "type": "string" }
        },
        "required": ["state", "error"],
        "additionalProperties": false
    }));
    json!({ "oneOf": variants })
}

#[cfg(test)]
mod strict_spawn_tests {
    use super::*;

    #[test]
    fn model_facing_spawn_is_strict_and_closed_at_every_object() {
        let definition =
            ToolDefinition::function("spawn_agent", "start child", spawn_agent_parameters())
                .with_strict_parameters();
        let serialized = serde_json::to_value(definition).unwrap();
        assert_eq!(serialized["strict"], true);
        let parameters = &serialized["parameters"];
        // The live provider rejects even descriptive siblings of a $ref.
        fn assert_bare_refs(value: &Value) {
            match value {
                Value::Object(object) => {
                    if object.contains_key("$ref") {
                        assert_eq!(object.len(), 1, "$ref must have no sibling keywords");
                    }
                    for child in object.values() {
                        assert_bare_refs(child);
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        assert_bare_refs(item);
                    }
                }
                _ => {}
            }
        }
        assert_bare_refs(parameters);
        assert_eq!(parameters["additionalProperties"], false);
        assert_eq!(
            parameters["required"],
            json!([
                "role",
                "task",
                "harness",
                "model",
                "thinking",
                "output_contract"
            ])
        );
        for shape in ["object", "array", "string_enum", "scalar", "field"] {
            assert_eq!(parameters["$defs"][shape]["additionalProperties"], false);
            let properties = parameters["$defs"][shape]["properties"]
                .as_object()
                .unwrap();
            let required = parameters["$defs"][shape]["required"].as_array().unwrap();
            assert_eq!(properties.len(), required.len());
        }
        let validator = jsonschema::validator_for(parameters).unwrap();
        let valid = json!({
            "role": "audit", "task": "check", "harness": null, "model": null, "thinking": null,
            "output_contract": { "kind": "object", "fields": [
                { "name": "summary", "schema": { "kind": "string" }, "required": true },
                { "name": "items", "schema": { "kind": "array", "items": { "kind": "integer" } }, "required": false }
            ] }
        });
        assert!(validator.is_valid(&valid));
        assert!(!validator.is_valid(
            &json!({ "role": "audit", "task": "check", "harness": null, "model": null,
            "thinking": null, "output_contract": [] })
        ));
        let mut misplaced = valid;
        misplaced["required"] = json!(["summary"]);
        assert!(!validator.is_valid(&misplaced));
    }

    #[test]
    fn typed_contract_compiles_nested_and_optional_results() {
        let parsed: SpawnAgentTask = serde_json::from_value(json!({
            "role": "audit", "task": "check", "model": null, "thinking": null,
            "output_contract": { "kind": "object", "fields": [
                { "name": "summary", "schema": { "kind": "string" }, "required": true },
                { "name": "items", "schema": { "kind": "array", "items": { "kind": "integer" } }, "required": false }
            ] }
        })).unwrap();
        let (task, _) = parsed.into_parts().unwrap();
        OutputContract::compile(&task.output_schema).unwrap();
        let validator = jsonschema::validator_for(&task.output_schema).unwrap();
        assert!(validator.is_valid(&json!({ "summary": "done" })));
        assert!(validator.is_valid(&json!({ "summary": "done", "items": [1, 2] })));
        assert!(!validator.is_valid(&json!({ "items": [1] })));
        assert!(!validator.is_valid(&json!({ "summary": "done", "items": ["bad"] })));
        assert!(!validator.is_valid(&json!({ "summary": "done", "unexpected": true })));
    }

    #[test]
    fn typed_contract_enums_and_duplicate_fields_remain_valid_schemas() {
        let parsed: SpawnAgentTask = serde_json::from_value(json!({
            "role": "audit", "task": "check", "model": null, "thinking": null,
            "output_contract": { "kind": "object", "fields": [
                { "name": "status", "schema": { "kind": "integer" }, "required": true },
                { "name": "status", "schema": { "kind": "string_enum", "values": ["ok", "fail"] }, "required": false }
            ] }
        })).unwrap();
        let (task, _) = parsed.into_parts().unwrap();
        OutputContract::compile(&task.output_schema).unwrap();
        let validator = jsonschema::validator_for(&task.output_schema).unwrap();
        assert!(validator.is_valid(&json!({})));
        assert!(validator.is_valid(&json!({ "status": "ok" })));
        assert!(!validator.is_valid(&json!({ "status": 1 })));
        assert!(!validator.is_valid(&json!({ "status": "other" })));
    }

    #[test]
    fn child_prompt_requires_final_message_after_accepted_submission() {
        let prompt = agent_prompt(AgentId::new(1), "Return the result");
        assert!(prompt.contains("call submit_result({output})"));
        assert!(prompt.contains("When its receipt says accepted, send a brief final"));
        assert!(prompt.contains("do not end with an empty model"));
    }

    #[test]
    fn malformed_contracts_and_legacy_schema_fail_before_child_reservation() {
        for output_contract in [
            json!("array"),
            json!([]),
            json!({ "kind": "array", "items": [] }),
            json!({ "kind": "object", "fields": [{
                "name": "value", "schema": { "kind": "string" },
                "required": true, "unexpected": true
            }] }),
        ] {
            assert!(
                serde_json::from_value::<SpawnAgentTask>(json!({
                    "role": "auditor", "task": "check", "model": null,
                    "thinking": null, "output_contract": output_contract
                }))
                .is_err()
            );
        }
        let legacy: SpawnAgentTask = serde_json::from_value(json!({
            "role": "auditor", "task": "check", "output_schema": "array"
        }))
        .unwrap();
        let (task, _) = legacy.into_parts().unwrap();
        assert!(prepare_batch(vec![task]).is_err());
    }

    #[test]
    fn legacy_in_flight_schema_remains_accepted_but_not_advertised() {
        let parsed: SpawnAgentTask = serde_json::from_value(json!({
            "role": "old", "task": "already started", "output_schema": { "type": "string" }
        }))
        .unwrap();
        let (task, _) = parsed.into_parts().unwrap();
        assert_eq!(task.output_schema, json!({ "type": "string" }));
        assert!(
            spawn_agent_parameters()["properties"]
                .get("output_schema")
                .is_none()
        );
    }
}
