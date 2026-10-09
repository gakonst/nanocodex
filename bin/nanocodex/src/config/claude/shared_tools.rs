//! The same shell, Computer Use and managed-memory handlers and schemas used
//! by Codex, bridged into Claude's native tool catalog.
use super::*;
use nanocodex::tools::ToolDefinition as RuntimeDefinition;

pub(super) fn install(
    mut native: ClaudeTools,
    runtime: Arc<RetainedHost>,
    workspace: Arc<worktree::Workspace>,
) -> ClaudeTools {
    let leases = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
        i64,
        worktree::WorkspaceLease,
    >::new()));
    // The Responses presentation groups namespaced functions. Restore their
    // canonical dispatch names without changing provider schemas or instructions.
    let mut definitions = Vec::new();
    for definition in runtime.model_specs("") {
        match definition {
            RuntimeDefinition::Namespace {
                name: namespace,
                tools,
                ..
            } => {
                for mut definition in tools {
                    if let RuntimeDefinition::Function { name, .. } = &mut definition {
                        *name = format!("{namespace}__{name}").into();
                        definitions.push(definition);
                    }
                }
            }
            definition @ RuntimeDefinition::Function { .. } => definitions.push(definition),
            _ => {}
        }
    }
    for definition in definitions {
        let RuntimeDefinition::Function {
            name,
            description,
            parameters,
            ..
        } = definition
        else {
            continue;
        };
        if !name.starts_with("mcp__cua_repl__")
            && !matches!(name.as_ref(), "exec_command" | "write_stdin")
            && !crate::managed_memory::is_memory_tool(&name)
        {
            continue;
        }
        let name = name.to_string();
        let definition = ToolDefinition {
            name: name.clone(),
            description: description.into(),
            input_schema: serde_json::to_value(parameters).expect("provider schema JSON"),
            strict: None,
            defer_loading: false,
        };
        let runtime = runtime.clone();
        let workspace = workspace.clone();
        let leases = leases.clone();
        native = native.tool_with_context(definition, move |mut input, invocation| {
            let runtime = runtime.clone();
            let name = name.clone();
            let workspace = workspace.clone();
            let leases = leases.clone();
            async move {
                let (cwd, lease) = workspace.pin_current();
                if name == "exec_command"
                    && let Some(object) = input.as_object_mut()
                {
                    object.entry("workdir").or_insert_with(|| json!(cwd));
                }
                let context = ToolContext::new(
                    &invocation.model,
                    &invocation.session_id,
                    &invocation.call_id,
                    &[],
                    usize::MAX,
                )
                .with_turn_id(Some(&invocation.turn_id))
                .with_host_context(invocation.host_context.as_deref())
                .with_instruction_revision(invocation.instruction_revision);
                let output = runtime
                    .execute_tool(
                        &name,
                        ToolInput::Function(
                            to_raw_value(&input).map_err(|error| error.to_string())?,
                        ),
                        context,
                    )
                    .await
                    .map_err(|error| error.to_string())?
                    .into_wire()
                    .map_err(|error| error.to_string())?;
                let mut reply = runtime_reply(&output.output, output.success)?;
                reply.metadata = output
                    .metadata
                    .map(|value| serde_json::from_str(value.get()))
                    .transpose()
                    .map_err(|error| error.to_string())?;
                reply.structured_result = output
                    .structured_result
                    .map(|value| serde_json::from_str(value.get()))
                    .transpose()
                    .map_err(|error| error.to_string())?;
                if let Some(result) = &reply.structured_result {
                    let mut leases = leases.lock().expect("shell workspace leases");
                    if name == "exec_command" {
                        if let Some(id) = result.get("session_id").and_then(Value::as_i64) {
                            leases.insert(id, lease);
                        }
                    } else if name == "write_stdin"
                        && result.get("exit_code").is_some()
                        && let Some(id) = input.get("session_id").and_then(Value::as_i64)
                    {
                        leases.remove(&id);
                    }
                }
                Ok(reply)
            }
        });
    }
    native
}
