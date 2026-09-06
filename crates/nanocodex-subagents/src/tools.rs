// Derived from clabby/tact@1d9ccaefd1d8613dab020812af04a91cd9b4c52c (Apache-2.0).
// Modified for Nanocodex's reusable native/WASM extension runtime.

use super::{
    message::MAX_MESSAGE_BYTES,
    model::{
        AgentDescriptor, AgentId, AgentStatus, AgentUpdate, MessageId, MessagePriority,
        MessagePurpose, agent_prompt,
    },
    runtime::{AgentDirectoryEntry, AgentSummary, OutputContract, Registry, forward_events},
};
use async_trait::async_trait;
use futures_util::future::join_all;
use nanocodex_agent::{AgentHandle, Model, SpawnOptions, Thinking};
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, Tools,
    runtime::ToolsBuildError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::sync::oneshot;

const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_WAIT_TIMEOUT: Duration = Duration::from_secs(300);
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentTask {
    role: String,
    task: String,
    #[serde(default)]
    model: Option<Model>,
    #[serde(default)]
    thinking: Option<Thinking>,
    output_schema: Value,
}

impl SpawnAgentTask {
    fn into_parts(self) -> (AgentTask, SpawnOptions) {
        let mut options = SpawnOptions::new();
        if let Some(model) = self.model {
            options = options.model(model);
        }
        if let Some(thinking) = self.thinking {
            options = options.thinking(thinking);
        }
        (
            AgentTask {
                role: self.role,
                task: self.task,
                output_schema: self.output_schema,
            },
            options,
        )
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
    let prepared = prepare_batch(tasks)?;
    let mut startup = registry.batch_startup();
    let capacities = registry.reserve_turns(prepared.len())?;
    let reservations = registry.reserve_many(session_id, prepared.len()).await?;
    let host_context = registry.host_context_for_session(session_id).await;
    let children = parent
        .spawn_many_observed_with_host_context(
            prepared.len(),
            observe_session,
            host_context.as_ref().map(Arc::clone),
        )
        .await?;

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

async fn start_agent_with_host_context(
    parent: &AgentHandle,
    registry: &Arc<Registry>,
    session_id: &str,
    task: AgentTask,
    options: SpawnOptions,
    host_context: Option<Arc<str>>,
) -> AgentToolResult<AgentStartReport> {
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
    let (child, events) = parent
        .spawn_with_host_context(options, host_context.as_ref().map(Arc::clone))
        .await?;
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
            "Starts a reusable clean-room subagent without inherited conversation history and immediately returns its ID.",
            spawn_agent_parameters(),
        )
        .with_output_schema(spawn_agent_output_schema())
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let (task, options) = input.decode_json::<SpawnAgentTask>()?.into_parts();
        let host_context = context.host_context().map(Arc::<str>::from);
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| std::io::Error::other("subagent runtime is closed"))?;
        let report = start_agent_with_host_context(
            &self.parent,
            &registry,
            context.session_id(),
            task,
            options,
            host_context,
        )
        .await?;
        json_output(&report)
    }
}

fn spawn_agent_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "role": {
                "type": "string",
                "description": "A short role describing the subagent's specialty."
            },
            "task": {
                "type": "string",
                "description": "A complete, focused task for the subagent."
            },
            "model": {
                "type": "string",
                "enum": ["sol", "terra", "luna", "astra"],
                "description": "Model override for the new agent. Omit to inherit the parent's current model."
            },
            "thinking": {
                "type": "string",
                "enum": ["none", "low", "medium", "high", "xhigh", "max"],
                "description": "Reasoning effort override for the new agent. Omit to inherit the parent's current thinking level."
            },
            "output_schema": {
                "description": "The JSON Schema that every successful result from this agent must satisfy. Use an object with one string field for a free-form report."
            }
        },
        "required": ["role", "task", "output_schema"],
        "additionalProperties": false
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitResultArgs {
    turn_token: u64,
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
            "Submits the current child subagent turn's final JSON output. This tool is unavailable to the root agent; root agents return final output as assistant text. A child calls it exactly once with the turn_token and a value matching its task output schema. Invalid values can be corrected and retried.",
            json!({
                "type": "object",
                "properties": {
                    "output": {
                        "description": "The final JSON value required by this agent's output schema."
                    },
                    "turn_token": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "The current turn token stated in the task prompt."
                    }
                },
                "required": ["turn_token", "output"],
                "additionalProperties": false
            }),
        )
        .with_output_schema(json!({
            "type": "object",
            "properties": {
                "accepted": { "type": "boolean", "const": true }
            },
            "required": ["accepted"],
            "additionalProperties": false
        }))
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let SubmitResultArgs { turn_token, output } = input.decode_json()?;
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| std::io::Error::other("subagent runtime is closed"))?;
        registry
            .submit_result(context.session_id(), turn_token, output)
            .await?;
        Ok(ToolOutput::from_json(json!({ "accepted": true }), true))
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
                        "description": "The focused message body. The runtime enforces a 2048-byte UTF-8 limit."
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
                        "description": "A typed coordination intent. Delegate is restricted to agents the sender can manage."
                    },
                    "in_reply_to": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "A message ID from the same two-party thread. Replies must reverse the original direction."
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
            "Lists a compact directory of agents in the same task tree. Active recipients are returned by default; completed agents can be included when a follow-up message is needed.",
            json!({
                "type": "object",
                "properties": {
                    "include_completed": {
                        "type": "boolean",
                        "default": false,
                        "description": "Includes completed, interrupted, failed, and closed agents."
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
                .await,
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
            "Waits until any requested subagent reaches a terminal status and returns a snapshot of every requested agent. The result includes pending, running, and closing agents; consumers must preserve those nonterminal agents and act only on completed, failed, interrupted, or closed entries. Use one call with multiple IDs instead of polling the workspace.",
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
                        "maximum": 300000,
                        "description": "Bounded wait in milliseconds. Defaults to 30000."
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
            .unwrap_or(DEFAULT_WAIT_TIMEOUT)
            .min(MAX_WAIT_TIMEOUT);
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
                "Interrupts an agent's active turn and every active descendant, waits for their model and tool resources to stop, and keeps the sessions reusable."
            }
            LifecycleOperation::Close => {
                "Closes an agent and its entire descendant subtree, waiting for active model and tool resources to stop before returning. Closed agents remain inspectable but are not reusable."
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

pub fn install_tools(
    tools: Tools,
    parent: AgentHandle,
    registry: Arc<Registry>,
) -> Result<Tools, ToolsBuildError> {
    tools
        .into_builder()
        .tool(SubmitResult {
            registry: Arc::downgrade(&registry),
        })
        .tool(SpawnAgent {
            parent,
            registry: Arc::downgrade(&registry),
        })
        .tool(SendAgentMessage {
            registry: Arc::downgrade(&registry),
        })
        .tool(ListAgents {
            registry: Arc::downgrade(&registry),
        })
        .tool(WaitAgent {
            registry: Arc::downgrade(&registry),
        })
        .tool(ChangeAgentLifecycle {
            registry: Arc::downgrade(&registry),
            operation: LifecycleOperation::Interrupt,
        })
        .tool(ChangeAgentLifecycle {
            registry: Arc::downgrade(&registry),
            operation: LifecycleOperation::Close,
        })
        .build()
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
mod tests {
    use super::{
        AgentTask, SendAgentMessage, SpawnAgentTask, SubmitResult, WaitAgent, prepare_batch,
        spawn_agent_parameters,
    };
    use crate::runtime::Registry;
    use nanocodex_agent::{Model, SpawnOptions, Thinking};
    use nanocodex_tools::Tool;
    use serde_json::json;
    use std::sync::Weak;

    #[test]
    fn batch_preparation_validates_every_schema_before_reservation() {
        let error = prepare_batch(vec![
            AgentTask {
                role: "planner".to_owned(),
                task: "plan".to_owned(),
                output_schema: json!({ "type": "object" }),
            },
            AgentTask {
                role: "reviewer".to_owned(),
                task: "review".to_owned(),
                output_schema: json!({ "type": 42 }),
            },
        ]);
        let error = match error {
            Ok(_) => panic!("invalid batch schema was accepted"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("invalid output_schema"));
    }

    #[test]
    fn spawn_agent_exposes_optional_model_and_thinking_overrides() {
        let parameters = spawn_agent_parameters();

        assert_eq!(
            parameters["properties"]["model"]["enum"],
            json!(["sol", "terra", "luna", "astra"])
        );
        assert_eq!(
            parameters["properties"]["thinking"]["enum"],
            json!(["none", "low", "medium", "high", "xhigh", "max"])
        );
        assert_eq!(
            parameters["required"],
            json!(["role", "task", "output_schema"])
        );

        let overridden: SpawnAgentTask = serde_json::from_value(json!({
            "role": "researcher",
            "task": "inspect the implementation",
            "model": "luna",
            "thinking": "medium",
            "output_schema": { "type": "object" }
        }))
        .unwrap();
        assert_eq!(overridden.model, Some(Model::Luna));
        assert_eq!(overridden.thinking, Some(Thinking::Medium));
        let (_, options) = overridden.into_parts();
        assert_eq!(
            options,
            SpawnOptions::new()
                .model(Model::Luna)
                .thinking(Thinking::Medium)
        );

        let inherited: SpawnAgentTask = serde_json::from_value(json!({
            "role": "researcher",
            "task": "inspect the implementation",
            "output_schema": { "type": "object" }
        }))
        .unwrap();
        assert_eq!(inherited.model, None);
        assert_eq!(inherited.thinking, None);
        let (_, options) = inherited.into_parts();
        assert_eq!(options, SpawnOptions::new());

        let astra: SpawnAgentTask = serde_json::from_value(json!({
            "role": "researcher",
            "task": "inspect the implementation",
            "model": "astra",
            "thinking": "max",
            "output_schema": { "type": "object" }
        }))
        .unwrap();
        assert_eq!(astra.model, Some(Model::Astra));
        assert_eq!(astra.thinking, Some(Thinking::Max));
    }

    #[test]
    fn send_message_definition_names_deferred_delivery_and_queued_waiting() {
        let definition = SendAgentMessage {
            registry: Weak::<Registry>::new(),
        }
        .definition();
        let priority = &definition.parameters().unwrap().as_value()["properties"]["priority"];

        assert_eq!(priority["enum"], json!(["deferred", "urgent"]));
        assert_eq!(priority["default"], json!("deferred"));
        assert!(definition.description().contains("do not wait"));
        assert!(definition.description().contains("finish the turn"));
    }

    #[test]
    fn submit_result_requires_the_turn_token_and_one_output_value() {
        let definition = SubmitResult {
            registry: Weak::<Registry>::new(),
        }
        .definition();
        let parameters = definition.parameters().unwrap().as_value();

        assert_eq!(parameters["required"], json!(["turn_token", "output"]));
        assert_eq!(parameters["additionalProperties"], json!(false));
        assert_eq!(parameters["properties"].as_object().unwrap().len(), 2);
        assert!(
            definition
                .description()
                .contains("unavailable to the root agent")
        );
    }

    #[test]
    fn wait_agent_only_refers_to_clean_spawns() {
        let definition = WaitAgent {
            registry: Weak::<Registry>::new(),
        }
        .definition();
        let description =
            &definition.parameters().unwrap().as_value()["properties"]["agent_ids"]["description"];

        assert!(description.as_str().unwrap().contains("spawn_agent"));
        assert!(!description.as_str().unwrap().contains("fork_agent"));
    }

    #[test]
    fn wait_agent_definition_requires_callers_to_preserve_nonterminal_agents() {
        let definition = WaitAgent {
            registry: Weak::<Registry>::new(),
        }
        .definition();

        assert!(definition.description().contains("every requested agent"));
        assert!(definition.description().contains("preserve"));
        assert!(definition.description().contains("nonterminal"));
    }
}
