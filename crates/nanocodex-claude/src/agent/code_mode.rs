//! Claude adapter for the native per-agent JavaScript runtime and its journal.
use super::*;
use nanocodex_oai_tools::{
    ToolContext, ToolDefinition as NativeDefinition, ToolInput, runtime::ToolRuntime,
};

impl ClaudeBuilder {
    /// Installs the native Code Mode runtime and its selected nested/direct tools.
    /// Durable agents attach their authoritative cell journal during build.
    pub fn code_tools(mut self, tools: nanocodex_oai_tools::Tools) -> Self {
        self.code_tools = Some(tools);
        self
    }

    pub(super) fn install_code_mode(
        &mut self,
        session_id: &str,
    ) -> Result<Option<Arc<ToolRuntime>>> {
        let Some(mut tools) = self.code_tools.take() else {
            return Ok(None);
        };
        if let Some(journal) = self
            .policy
            .as_ref()
            .and_then(|policy| policy.code_mode_journal())
        {
            tools = tools.with_code_journal(journal);
        }
        let tools = tools.for_session(session_id);
        let runtime = Arc::new(ToolRuntime::new_with_tools(
            &self.workspace,
            None,
            None,
            &tools,
        ));
        let mut definitions = runtime.model_specs(session_id);
        while let Some(definition) = definitions.pop() {
            let (schema, freeform) = match definition {
                NativeDefinition::Namespace { tools, .. } => {
                    definitions.extend(tools);
                    continue;
                }
                NativeDefinition::Function {
                    name,
                    description,
                    parameters,
                    ..
                } => (
                    ToolDefinition {
                        name: name.into(),
                        description: description.into(),
                        input_schema: parameters.as_value().clone(),
                        strict: None,
                        defer_loading: false,
                    },
                    false,
                ),
                NativeDefinition::Custom {
                    name, description, ..
                } => (
                    ToolDefinition {
                        name: name.into(),
                        description: description.into(),
                        input_schema: json!({"type":"object","properties":{"code":{"type":"string"}},"required":["code"],"additionalProperties":false}),
                        strict: None,
                        defer_loading: false,
                    },
                    true,
                ),
                NativeDefinition::ToolSearch {
                    description,
                    parameters,
                    ..
                } => (
                    ToolDefinition {
                        name: "tool_search".into(),
                        description: description.into(),
                        input_schema: parameters.as_value().clone(),
                        strict: None,
                        defer_loading: false,
                    },
                    false,
                ),
            };
            let name = schema.name.clone();
            let runtime = runtime.clone();
            self.tools.push((schema, Arc::new(move |input, invocation| {
                let runtime = runtime.clone(); let name = name.clone();
                Box::pin(async move {
                    let context = ToolContext::new(&invocation.model, &invocation.session_id, &invocation.call_id, &[], 10_000)
                        .with_turn_id(Some(&invocation.turn_id))
                        .with_journal_scope(invocation.journal_scope.as_deref())
                        .with_instruction_revision(invocation.instruction_revision)
                        .with_host_context(invocation.host_context.as_deref());
                    if matches!(name.as_str(), "exec" | "wait") {
                        let result = if name == "exec" {
                            let source = input.get("code").and_then(Value::as_str).ok_or("Code Mode requires code source")?;
                            runtime.execute_code(source, context).await
                        } else { runtime.wait_for_code(&input.to_string(), context).await };
                        let result = result.map_err(|error| error.to_string())?;
                        let structured = json!({"cell":result.cell,"output":result.output,"success":result.success,
                            "nested_calls":result.nested_calls.iter().map(|call| json!({"call_id":call.call_id,"name":call.name,"input":call.input,"output":call.output,"structured_result":call.structured_result,"success":call.success,"started_after_ns":call.started_after_ns,"duration_ns":call.duration_ns,"metadata":call.metadata})).collect::<Vec<_>>()});
                        Ok(ClaudeToolReply { content: ToolResultContent::Text(structured.to_string()), is_error: !result.success, metadata: None, structured_result: Some(structured) })
                    } else {
                        let input = if freeform { ToolInput::Freeform(input.get("code").and_then(Value::as_str).ok_or("Custom tool requires code source")?.into()) }
                            else { ToolInput::Function(serde_json::value::to_raw_value(&input).map_err(|error| error.to_string())?) };
                        let result = runtime.execute_tool(&name, input, context).await.map_err(|error| error.to_string())?;
                        let success = result.success;
                        let wire = result.into_wire().map_err(|error| error.to_string())?;
                        let structured = serde_json::to_value(&wire).map_err(|error| error.to_string())?;
                        Ok(ClaudeToolReply { content: ToolResultContent::Text(serde_json::to_string(&wire.output).map_err(|error| error.to_string())?), is_error: !success, metadata: None, structured_result: Some(structured) })
                    }
                })
            })));
        }
        Ok(Some(runtime))
    }
}
