//! Native Claude adapter over the shared QuickJS runtime. Every nested callback
//! comes from the builder's already-hooked catalog; no native effect bypasses it.
use super::*;
use nanocodex::{
    claude::ClaudeToolInvocation,
    tools::{Tool, ToolDefinition as CodeDefinition, ToolOutput, runtime::DynamicToolProvider},
};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

struct Admissions {
    current: ClaudeTools,
    /// Admitted catalog, host context and conversation root of each cell.
    cells: HashMap<String, (ClaudeTools, Option<Arc<str>>, String)>,
}

struct Catalog {
    native: Arc<ClaudeTools>,
    admissions: Arc<Mutex<Admissions>>,
    interrupted: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl DynamicToolProvider for Catalog {
    fn start(&self) {}
    fn direct_tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }
    fn available_definitions(&self) -> Vec<CodeDefinition> {
        self.admissions
            .lock()
            .expect("code admission lock")
            .current
            .definitions()
            .into_iter()
            .map(|d| CodeDefinition::function(d.name, d.description, d.input_schema))
            .collect()
    }
    fn code_mode_tool_summaries(&self) -> Vec<(String, String)> {
        self.native
            .current_definitions()
            .into_iter()
            .map(|d| {
                (
                    d.name,
                    format!("{}\nInput schema: {}", d.description, d.input_schema),
                )
            })
            .collect()
    }
    async fn execute(
        &self,
        name: &str,
        input: Value,
        context: ToolContext<'_>,
    ) -> Option<ToolOutput> {
        let (admitted, host_context, root_session_id) = self
            .admissions
            .lock()
            .expect("code admission lock")
            .cells
            .get(context.host_context().unwrap_or_default())
            .cloned()?;
        let admitted_definition = admitted
            .definitions()
            .into_iter()
            .find(|d| d.name == name)?;
        if !self
            .native
            .current_definitions()
            .iter()
            .any(|d| d == &admitted_definition)
        {
            return Some(ToolOutput::error(
                "Claude nested tool changed since admission; start a new cell to discover its current definition",
            ));
        }
        let invocation = ClaudeToolInvocation {
            model: context.model().into(),
            session_id: context.session_id().into(),
            root_session_id,
            turn_id: context.turn_id().unwrap_or(context.call_id()).into(),
            call_id: context.call_id().into(),
            instruction_revision: context.instruction_revision(),
            host_context,
        };
        Some(match admitted.execute(name, input, invocation).await {
            Ok(reply) => {
                if matches!(
                    name,
                    "spawn_agent"
                        | "list_agents"
                        | "send_agent_message"
                        | "wait_agent"
                        | "interrupt_agent"
                        | "close_agent"
                        | "submit_result"
                ) {
                    let text = match reply.content {
                        ToolResultContent::Text(text) => text,
                        ToolResultContent::Blocks(blocks) => blocks
                            .iter()
                            .filter_map(|block| block.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    let mut output = if reply.is_error {
                        ToolOutput::error(text)
                    } else if let Some(value) = reply.structured_result {
                        ToolOutput::from_json(value, true)
                    } else {
                        match serde_json::from_str::<Value>(&text) {
                            Ok(value) => ToolOutput::from_json(value, true),
                            Err(error) => ToolOutput::error(format!(
                                "invalid canonical subagent result: {error}"
                            )),
                        }
                    };
                    if let Some(metadata) = reply.metadata {
                        output = output.with_metadata(metadata);
                    }
                    return Some(output);
                }
                let content = match reply.content {
                    ToolResultContent::Text(text) => vec![json!({"type":"text","text":text})],
                    ToolResultContent::Blocks(blocks) => blocks.into_iter().map(|block| {
                        if block["type"] == "image" && block["source"]["type"] == "base64" {
                            json!({"type":"image","data":block["source"]["data"],"mimeType":block["source"]["media_type"]})
                        } else { block }
                    }).collect(),
                };
                let mut value = json!({
                    "content":content, "isError":reply.is_error,
                    "structuredContent":reply.structured_result,
                });
                if reply.is_error {
                    value["message"] = Value::String(
                        content
                            .iter()
                            .filter_map(|block| block.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }
                let mut output = ToolOutput::from_json(value, !reply.is_error);
                if let Some(metadata) = reply.metadata {
                    output = output.with_metadata(metadata);
                }
                output
            }
            Err(error) => {
                if error == ClaudeTools::HOST_INTERRUPTED {
                    self.interrupted.store(true, Ordering::Release);
                }
                ToolOutput::error(error)
            }
        })
    }
}

pub(super) fn wrap(native: ClaudeTools) -> nanocodex::agent::Result<ClaudeTools> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let native = Arc::new(native);
    let admissions = Arc::new(Mutex::new(Admissions {
        current: native.snapshot(),
        cells: HashMap::new(),
    }));
    let selection = Tools::builder()
        .without_defaults()
        .provider(Catalog {
            native: native.clone(),
            admissions: admissions.clone(),
            interrupted: interrupted.clone(),
        })
        .build()
        .map_err(|e| nanocodex::NanocodexError::InvalidRequest(e.to_string()))?;
    let runtime = Arc::new(RetainedHost(ToolRuntime::new_with_tools(
        ".", None, None, &selection,
    )));
    let cleanup = runtime.clone();
    let cleanup_admissions = admissions.clone();
    let cleanup_interrupted = interrupted.clone();
    let mut result = ClaudeTools::new().adapter_cleanup(move || {
        let runtime = cleanup.clone();
        let admissions = cleanup_admissions.clone();
        let interrupted = cleanup_interrupted.clone();
        async move {
            runtime.control().cancel().await;
            admissions
                .lock()
                .expect("code admission lock")
                .cells
                .clear();
            interrupted.store(false, Ordering::Release);
        }
    });
    for definition in runtime.model_specs("") {
        let (name, description, input_schema) = match definition {
            CodeDefinition::Custom {
                name, description, ..
            } if name.as_ref() == "exec" => (
                name.to_string(),
                format!(
                    "{description}\nSupply JavaScript in the code JSON field. Shared subagent tools return their canonical JSON directly. Other native results use content/isError/structuredContent; failed tools reject the Promise. Forward base64 image blocks with image(result.content[i]). Cells and store are process-local: after restart reconcile prior effects; never replay a cell to recover it."
                ),
                json!({"type":"object","properties":{"code":{"type":"string"}},"required":["code"],"additionalProperties":false}),
            ),
            CodeDefinition::Function {
                name,
                description,
                parameters,
                ..
            } if name.as_ref() == "wait" => (
                name.to_string(),
                description.to_string(),
                serde_json::to_value(parameters).expect("schema JSON"),
            ),
            _ => continue,
        };
        let wait = name == "wait";
        let runtime = runtime.clone();
        let interrupted = interrupted.clone();
        let native = native.clone();
        let admissions = admissions.clone();
        result = result.tool_with_context(
            ToolDefinition {
                name,
                description,
                input_schema,
                strict: None,
                defer_loading: false,
            },
            move |input, invocation| {
                let runtime = runtime.clone();
                let interrupted = interrupted.clone();
                let native = native.clone();
                let admissions = admissions.clone();
                async move {
                    if interrupted.load(Ordering::Acquire) {
                        return Err(ClaudeTools::HOST_INTERRUPTED.into());
                    }
                    if !wait {
                        let snapshot = native.snapshot();
                        let mut admissions = admissions.lock().expect("code admission lock");
                        admissions.current = snapshot.clone();
                        admissions.cells.insert(
                            invocation.call_id.clone(),
                            (
                                snapshot,
                                invocation.host_context.clone(),
                                invocation.root_session_id.clone(),
                            ),
                        );
                    }
                    let context = ToolContext::new(
                        &invocation.model,
                        &invocation.session_id,
                        &invocation.call_id,
                        &[],
                        10000,
                    )
                    .with_turn_id(Some(&invocation.turn_id))
                    .with_instruction_revision(invocation.instruction_revision)
                    .with_host_context(Some(&invocation.call_id));
                    let execution = if wait {
                        runtime.wait_for_code(&input.to_string(), context).await
                    } else {
                        runtime
                            .execute_code(
                                input
                                    .get("code")
                                    .and_then(Value::as_str)
                                    .ok_or("exec requires a code string")?,
                                context,
                            )
                            .await
                    }
                    .map_err(|e| e.to_string())?;
                    if interrupted.load(Ordering::Acquire) {
                        runtime.control().cancel().await;
                        return Err(ClaudeTools::HOST_INTERRUPTED.into());
                    }
                    if !execution.cell.as_ref().is_some_and(|cell| cell.running) {
                        let origin = execution.cell.as_ref().map_or(invocation.call_id.as_str(), |cell| cell.origin_call_id.as_str());
                        admissions.lock().expect("code admission lock").cells.remove(origin);
                    }
                    let mut reply = runtime_reply(&execution.output, execution.success)?;
                for notice in &execution.notifications {
                    match &mut reply.content {
                        ToolResultContent::Text(text) => { text.push('\n'); text.push_str(&notice.text); }
                        ToolResultContent::Blocks(blocks) => blocks.push(json!({"type":"text","text":notice.text})),
                    }
                }
                let calls = execution.nested_calls.iter().map(|call| json!({
                    "call_id":call.call_id, "name":call.name, "input":call.input,
                    "output":call.output, "structured_result":call.structured_result,
                    "success":call.success, "started_after_ns":call.started_after_ns,
                    "duration_ns":call.duration_ns, "metadata":call.metadata,
                })).collect::<Vec<_>>();
                reply.metadata = Some(json!({"_nanocodex_code":{
                    "calls":calls,
                    "origin_call_id":execution.cell.as_ref().map_or(invocation.call_id.as_str(), |cell| cell.origin_call_id.as_str()),
                }}));
                Ok(reply)
                }
            },
        );
    }
    Ok(result)
}
