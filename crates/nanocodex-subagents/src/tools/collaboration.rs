//! Codex MultiAgentV2 model contract, pinned to ac192cd793.
use super::*;
use crate::runtime::ModelMessage;
use nanocodex_agent::ForkTurns;
mod spec;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnTask {
    task_name: String,
    message: String,
    #[serde(default)]
    fork_turns: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<Thinking>,
}

impl SpawnTask {
    fn options(&self) -> AgentToolResult<(SpawnOptions, ForkTurns)> {
        let fork_turns = match self
            .fork_turns
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("all")
        {
            value if value.eq_ignore_ascii_case("none") => ForkTurns::None,
            value if value.eq_ignore_ascii_case("all") => ForkTurns::All,
            value => ForkTurns::Last(value.parse().map_err(|_| {
                std::io::Error::other("fork_turns must be none, all, or a positive integer string")
            })?),
        };
        if fork_turns == ForkTurns::All && (self.model.is_some() || self.reasoning_effort.is_some())
        {
            return Err(std::io::Error::other("full-history forks inherit the parent model and effort; use none or a positive fork_turns value for overrides").into());
        }
        let mut options = SpawnOptions::new();
        if let Some(model) = &self.model {
            options = options.model(model.parse::<Model>().map_err(std::io::Error::other)?);
        }
        if let Some(effort) = self.reasoning_effort {
            options = options.thinking(effort);
        }
        Ok((options, fork_turns))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageTask {
    target: String,
    message: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetTask {
    target: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListTask {
    #[serde(default)]
    path_prefix: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MailboxWait {
    #[serde(default)]
    timeout_ms: Option<i64>,
}

struct CollaborationTool {
    name: &'static str,
    parent: AgentHandle,
    registry: Weak<Registry>,
}

#[async_trait]
impl Tool for CollaborationTool {
    fn definition(&self) -> ToolDefinition {
        let (description, mut properties, required) = match self.name {
            "spawn_agent" => (
                "Spawns an agent for a concrete, bounded task that can run independently alongside useful local work. Agents share the workspace and tools. If your path is /root/task1, task_name task_3 creates /root/task1/task_3. Relative child names and canonical task paths are accepted as targets. The final answer is delivered to the parent. Inherit the current model unless a different model was explicitly requested. fork_turns defaults to all; none passes only the initial task. Full-history forks cannot override model or reasoning effort.",
                json!({
                    "task_name":{"type":"string","description":"Task name for the new agent. Use lowercase letters, digits, and underscores."},
                    "message":{"type":"string","description":"Initial plain-text task for the new agent."},
                    "fork_turns":{"type":"string","description":"Optional number of turns to fork. Defaults to all. Use none, all, or a positive integer string such as 3."},
                    "model":{"type":"string","description":"Optional model override: gpt-6-astra, gpt-5.6-sol, gpt-5.6-terra, or gpt-5.6-luna. Omit to inherit the parent model."},
                    "reasoning_effort":{"type":"string","enum":["none","low","medium","high","xhigh","max"],"description":"Optional reasoning effort override. Omit to inherit the parent effort."}
                }),
                vec!["task_name", "message"],
            ),
            "send_message" => (
                "Send a message to an existing agent. Delivery is prompt at model boundaries or after a pending tool call. Does not trigger a turn. Use a canonical task path when messaging outside your direct children.",
                json!({"target":{"type":"string","description":"Agent ID or relative/canonical task name."},"message":{"type":"string","description":"Message to queue on the target agent."}}),
                vec!["target", "message"],
            ),
            "followup_task" => (
                "Send a follow-up task to an existing non-root target agent and trigger a turn if it is idle. If already running, deliver the task at a model boundary or after the pending tool call completes.",
                json!({"target":{"type":"string","description":"Agent ID or relative/canonical task name."},"message":{"type":"string","description":"Follow-up task for the target agent."}}),
                vec!["target", "message"],
            ),
            "list_agents" => (
                "List live agents in the current root task tree. Optionally filter by task-path prefix.",
                json!({"path_prefix":{"type":"string","description":"Task-path prefix without a trailing slash. Relative paths resolve below the caller. Omit to list all agents."}}),
                vec![],
            ),
            "wait_agent" => (
                "Wait for a mailbox update from any agent, including messages and final-status notifications. Returns a summary of agents with updates or a timeout; message content is delivered into the conversation.",
                json!({"timeout_ms":{"type":"integer","description":"Timeout in milliseconds. Defaults to 30000, min 10000, max 3600000."}}),
                vec![],
            ),
            "interrupt_agent" => (
                "Interrupt an agent's current turn, if any, and return its previous status. The agent remains available for messages and follow-up tasks.",
                json!({"target":{"type":"string","description":"Agent ID or relative/canonical task name."}}),
                vec!["target"],
            ),
            _ => unreachable!(),
        };
        if let Some(message) = properties.get_mut("message") {
            message["encrypted"] = json!(true);
        }
        let definition = ToolDefinition::function(
            format!("collaboration__{}", self.name),
            description,
            json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        );
        match self.name {
            "spawn_agent" => {
                definition.with_output_schema(spec::spawn_agent_output_schema_v2(true))
            }
            "list_agents" => definition.with_output_schema(spec::list_agents_output_schema()),
            "wait_agent" => definition.with_output_schema(spec::wait_output_schema_v2()),
            "interrupt_agent" => {
                definition.with_output_schema(spec::agent_previous_status_output_schema(
                    "The agent status observed before the interrupt request was handled.",
                ))
            }
            _ => definition,
        }
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let encrypted = context.has_encrypted_arguments();
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| std::io::Error::other("subagent runtime is closed"))?;
        match self.name {
            "spawn_agent" => {
                let task: SpawnTask = input.decode_json()?;
                let (options, fork_turns) = task.options()?;
                let report = registry
                    .spawn_named(
                        &self.parent,
                        task.task_name,
                        ModelMessage {
                            body: task.message,
                            encrypted,
                        },
                        options,
                        fork_turns,
                        context.host_context().map(Arc::from),
                    )
                    .await?;
                json_output(&report)
            }
            "send_message" | "followup_task" => {
                let MessageTask { target, message } = input.decode_json()?;
                registry
                    .collaboration_message(
                        context.session_id(),
                        &target,
                        ModelMessage {
                            body: message,
                            encrypted,
                        },
                        self.name == "followup_task",
                    )
                    .await?;
                Ok(ToolOutput::text(""))
            }
            "list_agents" => {
                let ListTask { path_prefix } = input.decode_json()?;
                json_output(
                    &registry
                        .collaboration_directory(context.session_id(), path_prefix.as_deref())
                        .await?,
                )
            }
            "wait_agent" => {
                let MailboxWait { timeout_ms } = input.decode_json()?;
                if timeout_ms.is_some_and(|timeout| timeout > 3600000) {
                    return Err(std::io::Error::other("timeout_ms must be at most 3600000").into());
                }
                let timeout = timeout_ms.unwrap_or(30000).max(10000);
                let mut report = registry
                    .wait_mailbox(
                        context.session_id(),
                        Duration::from_millis(u64::try_from(timeout).expect("positive timeout")),
                    )
                    .await;
                if let Some(requested) = timeout_ms.filter(|requested| *requested < timeout) {
                    report["message"] = json!(format!(
                        "{}\n\nRequested timeout of {requested}ms was clamped to the minimum of {timeout}ms.",
                        report["message"].as_str().unwrap_or_default(),
                    ));
                }
                json_output(&report)
            }
            "interrupt_agent" => {
                let TargetTask { target } = input.decode_json()?;
                json_output(
                    &registry
                        .interrupt_target(context.session_id(), &target)
                        .await?,
                )
            }
            _ => unreachable!(),
        }
    }
}

pub(super) fn install(
    tools: Tools,
    parent: AgentHandle,
    registry: Arc<Registry>,
) -> Result<Tools, ToolsBuildError> {
    registry.register_handle(parent.clone());
    let mut builder = tools.into_builder();
    if parent.depth() > 0 && parent.task_name().is_none() {
        builder = builder.tool(SubmitResult {
            registry: Arc::downgrade(&registry),
        });
    }
    for name in [
        "spawn_agent",
        "send_message",
        "followup_task",
        "list_agents",
        "wait_agent",
        "interrupt_agent",
    ] {
        builder = builder.tool_with_exposure(
            CollaborationTool {
                name,
                parent: parent.clone(),
                registry: Arc::downgrade(&registry),
            },
            nanocodex_tools::ToolExposure::DirectOnly,
        );
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_policy_defaults_and_override_rules_match_codex() {
        let task = |fields: Value| -> SpawnTask { serde_json::from_value(fields).unwrap() };
        let inherited = task(json!({"task_name":"review","message":"inspect"}));
        assert_eq!(inherited.options().unwrap().1, ForkTurns::All);
        for invalid in ["0", "-1", "recent", "1.5"] {
            assert!(
                task(json!({"task_name":"review","message":"inspect","fork_turns":invalid}))
                    .options()
                    .is_err()
            );
        }
        assert!(
            task(json!({"task_name":"review","message":"inspect","model":"gpt-6-astra"}))
                .options()
                .is_err()
        );
        let explicit = task(
            json!({"task_name":"review","message":"inspect","fork_turns":"3","model":"gpt-6-astra","reasoning_effort":"low"}),
        );
        let (options, fork) = explicit.options().unwrap();
        assert_eq!(
            fork,
            ForkTurns::Last(std::num::NonZeroUsize::new(3).unwrap())
        );
        assert_eq!(options.selected_model(), Some(Model::Astra));
        assert_eq!(options.selected_thinking(), Some(Thinking::Low));
        assert!(
            serde_json::from_value::<SpawnTask>(
                json!({"role":"review","task":"inspect","output_schema":true})
            )
            .is_err()
        );
    }
}
