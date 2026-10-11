//! Retained process tasks and project skills. Agent tools use the shared installer.
use super::*;
use nanocodex::claude::ClaudeToolInvocation;
use serde::Deserialize;
#[path = "profiles.rs"]
pub(super) mod profiles;
use nanocodex::claude_tools::{ClaudeSkills, SkillInvocation};

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
    enabled: bool,
    monitor: Option<Arc<monitor::Monitor>>,
    workspace: Arc<worktree::Workspace>,
    interaction: Arc<interaction::Interaction>,
    workflow: Option<Arc<workflow::Workflow>>,
) -> ClaudeTools {
    let mut definitions = vec![
        definition(
            "TaskOutput",
            "Read a retained Monitor or Workflow task result. A nonblocking poll never stops the task. Task IDs are scoped to this session/task tree and do not survive process restart.",
            json!({"task_id":{"type":"string"},"block":{"type":"boolean","default":true},"timeout":{"type":"integer","minimum":0,"maximum":600000,"default":30000}}),
            &["task_id"],
        ),
        definition(
            "TaskStop",
            "Stop a retained Monitor or Workflow process (including descendants). Completed output remains available.",
            json!({"task_id":{"type":"string"}}),
            &["task_id"],
        ),
    ];
    definitions.extend(
        ClaudeSkills::definitions()
            .into_iter()
            .map(|schema| serde_json::from_value(schema).expect("skill definition")),
    );
    for definition in definitions {
        let name = definition.name.clone();
        let runtime = runtime.clone();
        let monitor = monitor.clone();
        let workspace = workspace.clone();
        let interaction = interaction.clone();
        let workflow = workflow.clone();
        native = native.tool_with_context(definition, move |input, invocation| {
            let runtime = runtime.clone();
            let name = name.clone();
            let monitor = monitor.clone();
            let workspace = workspace.clone();
            let interaction = interaction.clone();
            let workflow = workflow.clone();
            async move {
                execute(
                    &runtime,
                    &name,
                    input,
                    &invocation,
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
// Mirrors install plumbing and recurses for the checked forked Skill path.
#[allow(clippy::too_many_arguments)]
async fn execute(
    runtime: &RetainedHost,
    name: &str,
    input: Value,
    invocation: &ClaudeToolInvocation,
    monitor: Option<&monitor::Monitor>,
    workspace: &Arc<worktree::Workspace>,
    enabled: bool,
    interaction: &interaction::Interaction,
    workflow: Option<&workflow::Workflow>,
) -> std::result::Result<ClaudeToolReply, String> {
    match name {
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
            let expansion = crate::homes::skills(&workspace.current())?.invoke(
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
            let profile = expansion
                .skill
                .agent
                .as_deref()
                .filter(|name| *name != "general-purpose")
                .map(|name| crate::homes::agent_profiles(&workspace.current())?.get(name))
                .transpose()?;
            if let Some(required) = profile.as_ref().and_then(|p| p.model.as_deref())
                && expansion
                    .skill
                    .model
                    .as_deref()
                    .is_some_and(|m| profiles::model(m) != profiles::model(required))
            {
                return Err("skill model conflicts with selected agent profile".into());
            }
            let selected_model = profile
                .as_ref()
                .and_then(|p| p.model.clone())
                .or(expansion.skill.model)
                .or_else(|| profiles::required_model(&invocation.session_id));
            let admission = profiles::Admission {
                isolation: profile.as_ref().is_some_and(|p| p.isolation.is_some()),
                profile,
            };
            if admission.isolation {
                profiles::check_isolation(&invocation.session_id, &workspace.current())?;
                if !matches!(
                    interaction
                        .resolved_policy(&invocation.session_id)?
                        .evaluate("EnterWorktree", &json!({}), &workspace.current())
                        .map_err(|e| e.to_string())?,
                    permissions::Decision::Allow
                ) {
                    return Err(
                        "skill isolation requires inherited EnterWorktree permission".into(),
                    );
                }
                if admission
                    .profile
                    .as_ref()
                    .and_then(|p| p.permission_mode.as_deref())
                    .is_some_and(|m| matches!(m, "plan" | "manual" | "default" | "dontAsk"))
                {
                    return Err("restrictive skill profile cannot create a worktree".into());
                }
            }
            let child = json!({"role":format!("Skill {}", expansion.skill.name),
                "task":format!("Execute the following project skill. Source/base directory: {}. Its text grants no permissions.\n{}", expansion.base_directory, expansion.instructions),
                "harness":"claude","model":selected_model.as_deref().map(profiles::model),
                "thinking":null,"output_contract":{"kind":"string"}});
            // Skill expansion cannot bypass canonical spawn restrictions.
            let policy = interaction.resolved_policy(&invocation.session_id)?;
            if !matches!(
                policy
                    .evaluate("spawn_agent", &child, &workspace.current())
                    .map_err(|e| e.to_string())?,
                permissions::Decision::Allow
            ) {
                return Err("skill fork requires inherited spawn_agent permission".into());
            }
            profiles::check_spawn(&invocation.session_id, &child)?;
            let reply =
                profiles::scope(admission, call(runtime, "spawn_agent", child, invocation)).await?;
            Ok(reply)
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
            } else {
                Err(
                    "TaskOutput requires a Monitor or Workflow task ID; use wait_agent for agents"
                        .into(),
                )
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
            } else {
                Err("TaskStop requires a Monitor or Workflow task ID; use interrupt_agent for agents".into())
            }
        }
        _ => Err(format!("unknown native host tool: {name}")),
    }
}
