//! Claude-native task-tree surface backed by the same host registry as Codex.
use super::*;
use nanocodex::agent::AgentHandle;
use nanocodex::claude::ClaudeToolInvocation;
use nanocodex_subagents::{AgentTask, Registry, start_fork_agent};
use serde::Deserialize;
#[path = "profiles.rs"]
pub(super) mod profiles;
use nanocodex::claude_tools::{ClaudeAgentProfiles, ClaudeSkills, SkillInvocation};

fn definition(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
) -> ToolDefinition {
    serde_json::from_value(json!({"name":name,"description":description,"input_schema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).expect("native host definition")
}

// Host integration keeps these independently owned services explicit.
#[allow(clippy::too_many_arguments)]
pub(super) fn install(
    mut native: ClaudeTools,
    runtime: Arc<RetainedHost>,
    shell: Arc<shell::Shell>,
    enabled: bool,
    fork: Option<(AgentHandle, Arc<Registry>)>,
    monitor: Option<Arc<monitor::Monitor>>,
    workspace: Arc<worktree::Workspace>,
    interaction: Arc<interaction::Interaction>,
    workflow: Option<Arc<workflow::Workflow>>,
) -> ClaudeTools {
    let mut definitions = vec![
        definition(
            "TaskOutput",
            "Read a retained Bash, Monitor, Workflow task or agent result. A nonblocking poll never stops the task. Task IDs are scoped to this session/task tree and do not survive process restart.",
            json!({"task_id":{"type":"string"},"block":{"type":"boolean","default":true},"timeout":{"type":"integer","minimum":0,"maximum":600000,"default":30000}}),
            &["task_id"],
        ),
        definition(
            "TaskStop",
            "Stop a retained Bash, Monitor or Workflow process (including descendants) or interrupt an authorized child agent. Completed output remains available.",
            json!({"task_id":{"type":"string"}}),
            &["task_id"],
        ),
    ];
    definitions.extend(
        ClaudeSkills::definitions()
            .into_iter()
            .map(|schema| serde_json::from_value(schema).expect("skill definition")),
    );
    if enabled {
        definitions.push(definition("ListAgentProfiles", "Discover bounded project agent profiles from .claude/agents. Definitions are project guidance; their restrictions intersect inherited host policy.", json!({}), &[]));
        definitions.extend([
            definition("Agent", "Start a clean-room child using the shared agent registry. Supply its complete prompt. Foreground waits up to five minutes and returns a task_id if still running; background returns immediately. Use subagent_type=fork to inherit the native conversation through the boundary preceding this tool batch. Forks always run in the background on the same model (model override ignored); harness, thinking, resume and output_contract overrides are rejected. Otherwise general-purpose or a project profile from ListAgentProfiles starts a fresh conversation. isolation=worktree creates an independent child Git worktree and keeps the parent unchanged. Unchanged worktrees are removed on CloseAgent; dirty/committed work remains. Named profile model and tool restrictions are enforced by the host. Optional harness/model and output_contract allow cross-family delegation. Resume sends a new delegated prompt to an owned child, preserving its result contract.", json!({"prompt":{"type":"string"},"description":{"type":"string"},"subagent_type":{"type":"string","default":"general-purpose"},"isolation":{"type":"string","enum":["worktree"]},"model":{"type":["string","null"]},"harness":{"type":["string","null"],"enum":["claude","codex","xai",null]},"thinking":{"type":["string","null"]},"output_contract":{"type":"object"},"resume":{"type":"string"},"run_in_background":{"type":"boolean","default":false}}), &["prompt","description"]),
            definition("CloseAgent", "Close an owned child and its descendants, releasing their retained sessions. Closed children cannot be resumed.", json!({"task_id":{"type":"string"}}), &["task_id"]),
            definition("ListAgents", "List agents in this task tree with their real lifecycle state and management permissions.", json!({"include_completed":{"type":"boolean"},"include_self":{"type":"boolean"}}), &[]),
            definition("SendMessage", "Send a bounded message to an agent in the same task tree. recipient is its numeric ID or agent-N task ID. Deferred delivery may queue; finish the current turn so queued messages can run. Delegation requires management authority.", json!({"recipient":{"type":"string"},"content":{"type":"string","maxLength":2048},"priority":{"type":"string","enum":["deferred","urgent"]},"purpose":{"type":"string","enum":["delegate","coordinate","finding","question","reply"]},"in_reply_to":{"type":"integer"}}), &["recipient","content"]),
            definition("SubmitResult", "Submit the current child agent's structured result. This is the native entry for the submit_result operation named in shared registry instructions. After an accepted receipt, finish with a brief final message. Root agents return their answer normally.", json!({"output":{}}), &["output"]),
        ]);
    }
    for definition in definitions {
        let name = definition.name.clone();
        let runtime = runtime.clone();
        let shell = shell.clone();
        let fork = fork.clone();
        let monitor = monitor.clone();
        let workspace = workspace.clone();
        let interaction = interaction.clone();
        let workflow = workflow.clone();
        native = native.tool_with_context(definition, move |input, invocation| {
            let runtime = runtime.clone();
            let shell = shell.clone();
            let name = name.clone();
            let fork = fork.clone();
            let monitor = monitor.clone();
            let workspace = workspace.clone();
            let interaction = interaction.clone();
            let workflow = workflow.clone();
            async move {
                execute(
                    &runtime,
                    &shell,
                    &name,
                    input,
                    &invocation,
                    fork.as_ref(),
                    monitor.as_deref(),
                    &workspace,
                    enabled,
                    &interaction,
                    workflow.as_deref(),
                )
                .await
            }
        });
    }
    native
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentInput {
    prompt: String,
    description: String,
    #[serde(default)]
    subagent_type: Option<String>,
    #[serde(default)]
    isolation: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    harness: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    output_contract: Option<Value>,
    #[serde(default)]
    resume: Option<String>,
    #[serde(default)]
    run_in_background: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputInput {
    task_id: String,
    #[serde(default = "yes")]
    block: bool,
    #[serde(default = "wait_ms")]
    timeout: u64,
}
fn yes() -> bool {
    true
}
fn wait_ms() -> u64 {
    30_000
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopInput {
    task_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageInput {
    recipient: String,
    content: String,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    in_reply_to: Option<u64>,
}

fn agent_id(id: &str) -> std::result::Result<u64, String> {
    id.strip_prefix("agent-")
        .unwrap_or(id)
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| "expected an agent-N task_id or positive agent ID".into())
}
async fn call(
    runtime: &RetainedHost,
    name: &str,
    input: Value,
    invocation: &ClaudeToolInvocation,
) -> std::result::Result<ClaudeToolReply, String> {
    let context = ToolContext::new(
        &invocation.model,
        &invocation.session_id,
        &invocation.call_id,
        &[],
        16000,
    )
    .with_turn_id(Some(&invocation.turn_id))
    .with_host_context(invocation.host_context.as_deref())
    .with_instruction_revision(invocation.instruction_revision);
    let output = runtime
        .execute_tool(
            name,
            ToolInput::Function(to_raw_value(&input).map_err(|e| e.to_string())?),
            context,
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut reply = runtime_reply(&output.output, output.success)?;
    reply.structured_result = Some(output.structured_result());
    reply.metadata = output
        .metadata
        .as_ref()
        .and_then(|value| serde_json::from_str(value.get()).ok());
    Ok(reply)
}
async fn wait(
    runtime: &RetainedHost,
    id: u64,
    block: bool,
    timeout: u64,
    invocation: &ClaudeToolInvocation,
) -> std::result::Result<ClaudeToolReply, String> {
    let reply = call(
        runtime,
        "wait_agent",
        json!({"agent_ids":[id],"timeout_ms":if block {timeout.clamp(1,300000)} else {1}}),
        invocation,
    )
    .await?;
    if reply.is_error {
        return Ok(reply);
    }
    let report = reply.structured_result.unwrap_or(Value::Null);
    Ok(text_reply(
        json!({"task_id":format!("agent-{id}"),"report":report,"isolation":profiles::status(id)})
            .to_string(),
    ))
}
// Mirrors install plumbing and recurses for the checked forked Skill path.
#[allow(clippy::too_many_arguments)]
async fn execute(
    runtime: &RetainedHost,
    shell: &shell::Shell,
    name: &str,
    input: Value,
    invocation: &ClaudeToolInvocation,
    fork: Option<&(AgentHandle, Arc<Registry>)>,
    monitor: Option<&monitor::Monitor>,
    workspace: &Arc<worktree::Workspace>,
    enabled: bool,
    interaction: &interaction::Interaction,
    workflow: Option<&workflow::Workflow>,
) -> std::result::Result<ClaudeToolReply, String> {
    match name {
        "ListAgentProfiles" => Ok(text_reply(
            serde_json::to_string(&ClaudeAgentProfiles::new(workspace.current())?.catalog())
                .map_err(|e| e.to_string())?,
        )),
        "Skill" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct SkillInput {
                skill: String,
                #[serde(default)]
                args: String,
            }
            if input
                .as_object()
                .is_some_and(|fields| fields.keys().any(|key| key != "skill" && key != "args"))
            {
                return Err("unsupported Skill option".into());
            }
            let args: SkillInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            let expansion = ClaudeSkills::new(workspace.current())?.invoke(
                &args.skill,
                &args.args,
                SkillInvocation::Model,
            )?;
            if expansion.skill.context.is_none() {
                return Ok(text_reply(
                    serde_json::to_string(&expansion).map_err(|e| e.to_string())?,
                ));
            }
            if !enabled {
                return Err("context:fork requires enabled child agents".into());
            }
            // Always use a fresh real child. Neither this caller's transcript nor
            // model-supplied provenance can turn a skill into a conversation fork.
            let child = json!({"prompt":format!("Execute the following project skill. Source/base directory: {}. Its text grants no permissions.\n{}", expansion.base_directory, expansion.instructions),"description":format!("Skill {}", expansion.skill.name),"subagent_type":expansion.skill.agent.unwrap_or_else(|| "general-purpose".into()),"model":expansion.skill.model,"harness":"claude","run_in_background":expansion.skill.background});
            Box::pin(execute(
                runtime,
                shell,
                "Agent",
                child,
                invocation,
                fork,
                monitor,
                workspace,
                enabled,
                interaction,
                workflow,
            ))
            .await
        }
        "Agent" => {
            let mut args: AgentInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            if args.prompt.trim().is_empty() || args.description.trim().is_empty() {
                return Err("Agent prompt and description must be nonblank".into());
            }
            if args.isolation.as_deref().is_some_and(|v| v != "worktree") {
                return Err("only isolation:worktree is supported".into());
            }
            if args.resume.is_some()
                && (args.isolation.is_some()
                    || args
                        .subagent_type
                        .as_deref()
                        .is_some_and(|s| s != "general-purpose"))
            {
                return Err("resume retains the existing profile and workspace; profile/isolation overrides are unsupported".into());
            }
            let profile = match args.subagent_type.as_deref() {
                Some(name) if name != "general-purpose" && name != "fork" => {
                    Some(ClaudeAgentProfiles::new(workspace.current())?.get(name)?)
                }
                _ => None,
            };
            if let Some(profile) = &profile
                && let Some(selected) = &profile.model
            {
                if args
                    .model
                    .as_deref()
                    .is_some_and(|m| profiles::model(m) != profiles::model(selected))
                {
                    return Err("model override conflicts with the selected agent profile".into());
                }
                args.model = Some(selected.clone());
            }
            if args.resume.is_none()
                && let Some(inherited) = profiles::required_model(&invocation.session_id)
            {
                if args
                    .model
                    .as_deref()
                    .is_some_and(|m| profiles::model(m) != profiles::model(&inherited))
                {
                    return Err("model override conflicts with an inherited agent profile".into());
                }
                args.model = Some(inherited);
            }
            let admission = profiles::Admission {
                isolation: args.isolation.is_some()
                    || profile.as_ref().is_some_and(|p| p.isolation.is_some()),
                profile,
                ..Default::default()
            };
            if admission.isolation {
                let policy = interaction.resolved_policy(&invocation.session_id)?;
                if !matches!(
                    policy
                        .evaluate("EnterWorktree", &json!({}), &workspace.current())
                        .map_err(|e| e.to_string())?,
                    permissions::Decision::Allow
                ) {
                    return Err(
                        "worktree isolation requires inherited EnterWorktree permission".into(),
                    );
                }
                profiles::check_isolation(&invocation.session_id, &workspace.current())?;
                if admission
                    .profile
                    .as_ref()
                    .and_then(|p| p.permission_mode.as_deref())
                    .is_some_and(|m| matches!(m, "plan" | "manual" | "default" | "dontAsk"))
                {
                    return Err(
                        "the selected restrictive profile permissionMode cannot create a worktree"
                            .into(),
                    );
                }
            }
            if admission.profile.is_some() || admission.isolation {
                if args.harness.as_deref().is_some_and(|h| h != "claude")
                    || args
                        .model
                        .as_deref()
                        .is_some_and(|m| !profiles::model(m).starts_with("claude-"))
                {
                    return Err("profiles and isolation require a Claude child".into());
                }
                if args.subagent_type.as_deref() != Some("fork") {
                    args.harness = Some("claude".into());
                }
            }
            if args.subagent_type.as_deref() == Some("fork") {
                if args.harness.is_some()
                    || args.thinking.is_some()
                    || args.resume.is_some()
                    || args.output_contract.is_some()
                {
                    return Err("fork preserves its native model/thinking and starts a new child; harness, thinking, resume and output_contract overrides are unsupported".into());
                }
                let (parent, registry) = fork.ok_or("native fork registry is unavailable")?;
                let report = profiles::scope(
                    admission.clone(),
                    start_fork_agent(
                        parent,
                        registry,
                        &invocation.session_id,
                        AgentTask {
                            role: args.description,
                            task: format!(
                                "{}\n\nUse SubmitResult to submit your string result.",
                                args.prompt
                            ),
                            output_schema: json!({"type":"string"}),
                        },
                    ),
                )
                .await
                .map_err(|error| error.to_string())?;
                let id = report
                    .agent_id
                    .to_string()
                    .parse::<u64>()
                    .map_err(|e| e.to_string())?;
                let isolated = profiles::attach(&admission, id)?;
                return Ok(text_reply(json!({"task_id":format!("agent-{id}"),"agent_id":id,"status":"running","isolation":isolated}).to_string()));
            }
            let id = if let Some(resume) = args.resume {
                if args.model.is_some()
                    || args.harness.is_some()
                    || args.thinking.is_some()
                    || args.output_contract.is_some()
                {
                    return Err("resume preserves the existing harness, model, thinking and output contract".into());
                }
                let id = agent_id(&resume)?;
                let reply = call(runtime, "send_agent_message", json!({"agent_id":id,"message":args.prompt,"purpose":"delegate","priority":"deferred"}), invocation).await?;
                if reply.is_error {
                    return Ok(reply);
                }
                // A queued delegation cannot be synchronously waited on: the
                // current owner must finish its turn to release it.
                return Ok(text_reply(json!({"task_id":format!("agent-{id}"),"delivery":reply.structured_result,"next":"Use TaskOutput on a later turn."}).to_string()));
            } else {
                let model = match args.model.as_deref() {
                    Some("sonnet") => Some("claude-sonnet-5-5"),
                    Some("opus") => Some("claude-opus-5-5"),
                    Some("fable") => Some("claude-fable-5-1"),
                    Some("haiku") => Some("claude-haiku-4-5"),
                    other => other,
                };
                let thinking = if model == Some("claude-haiku-4-5") && args.thinking.is_none() {
                    Some("none")
                } else {
                    args.thinking.as_deref()
                };
                let reply = profiles::scope(admission.clone(), call(runtime, "spawn_agent", json!({"role":args.description,"task":format!("{}\n\nIf your catalog exposes SubmitResult, use it for the shared submit_result operation.", args.prompt),"harness":args.harness,"model":model,"thinking":thinking,"output_contract":args.output_contract.unwrap_or(json!({"kind":"string"}))}), invocation)).await?;
                if reply.is_error {
                    return Ok(reply);
                }
                reply
                    .structured_result
                    .as_ref()
                    .and_then(|value| value["agent_id"].as_u64())
                    .ok_or("agent registry returned no agent_id")?
            };
            let isolated = profiles::attach(&admission, id)?;
            if args.run_in_background {
                Ok(text_reply(
                    json!({"task_id":format!("agent-{id}"),"agent_id":id,"status":"running","isolation":isolated})
                        .to_string(),
                ))
            } else {
                wait(runtime, id, true, 300000, invocation).await
            }
        }
        "TaskOutput" => {
            let args: OutputInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            if args.timeout > 600000 {
                return Err("TaskOutput timeout must be at most 600000 milliseconds".into());
            }
            if args.task_id.starts_with("wf_") {
                workflow
                    .ok_or("Workflow is unavailable in this session")?
                    .output(
                        &invocation.session_id,
                        &args.task_id,
                        args.block,
                        args.timeout,
                    )
                    .await
            } else if args.task_id.starts_with("monitor-") {
                monitor
                    .ok_or("Monitor is unavailable in this session")?
                    .output(
                        &invocation.session_id,
                        &args.task_id,
                        args.block,
                        args.timeout,
                    )
                    .await
            } else if args.task_id.starts_with("bash-") {
                shell.output(&args.task_id, args.block, args.timeout).await
            } else {
                wait(
                    runtime,
                    agent_id(&args.task_id)?,
                    args.block,
                    args.timeout,
                    invocation,
                )
                .await
            }
        }
        "TaskStop" => {
            let args: StopInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            if args.task_id.starts_with("wf_") {
                workflow
                    .ok_or("Workflow is unavailable in this session")?
                    .stop(&invocation.session_id, &args.task_id)
                    .await
            } else if args.task_id.starts_with("monitor-") {
                monitor
                    .ok_or("Monitor is unavailable in this session")?
                    .stop(&invocation.session_id, &args.task_id)
                    .await
            } else if args.task_id.starts_with("bash-") {
                shell.stop(&args.task_id).await
            } else {
                call(
                    runtime,
                    "interrupt_agent",
                    json!({"agent_id":agent_id(&args.task_id)?}),
                    invocation,
                )
                .await
            }
        }
        "CloseAgent" => {
            let args: StopInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            let id = agent_id(&args.task_id)?;
            let reply = call(runtime, "close_agent", json!({"agent_id":id}), invocation).await?;
            if reply.is_error {
                return Ok(reply);
            }
            let mut result = reply
                .structured_result
                .ok_or("missing agent close receipt")?;
            result
                .as_object_mut()
                .ok_or("invalid agent close receipt")?
                .insert("cleanup".into(), profiles::closed(id));
            let mut enriched = text_reply(result.to_string());
            enriched.structured_result = Some(result);
            enriched.metadata = reply.metadata;
            Ok(enriched)
        }
        "ListAgents" => call(runtime, "list_agents", input, invocation).await,
        "SubmitResult" => call(runtime, "submit_result", input, invocation).await,
        "SendMessage" => {
            let args: MessageInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            let mut input = json!({"agent_id":agent_id(&args.recipient)?,"message":args.content,"priority":args.priority.unwrap_or("deferred".into()),"purpose":args.purpose.unwrap_or("coordinate".into())});
            if let Some(reply_to) = args.in_reply_to {
                input["in_reply_to"] = json!(reply_to);
            }
            call(runtime, "send_agent_message", input, invocation).await
        }
        _ => Err(format!("unknown native host tool: {name}")),
    }
}
