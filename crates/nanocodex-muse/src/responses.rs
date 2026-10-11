//! Meta Responses request encoding, normalization, and summary compaction.
use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use nanocodex_oai_api::tower::{
    CodeCallKind, CompactionOutput, GenerationOutput, ResponsesAttempt, ResponsesAttemptKind,
    ResponsesOutput, ResponsesServiceError,
};
use nanocodex_oai_api::{
    responses::{ContentItem, MessageRole, ResponseItem, ToolDefinition},
    transport::{EncodedRequest, ResponsesError},
};

const SUMMARY_OPEN: &str = "<muse_context_summary>\n";
const SUMMARY_CLOSE: &str = "\n</muse_context_summary>";
// Lifted from nanocodex-claude::agent::compact_locked.
const SUMMARY_PROMPT: &str = "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools. Summarize the conversation so far, preserving user goals, constraints, decisions and tool results.";

pub(crate) fn encode(
    encoded: EncodedRequest,
    compact: bool,
) -> Result<EncodedRequest, ResponsesError> {
    let mut body: Value =
        serde_json::from_str(encoded.raw().get()).map_err(ResponsesError::InvalidJson)?;
    let mut tools = Vec::new();
    let mut input = Vec::new();
    let mut items = body["input"]
        .as_array_mut()
        .map(std::mem::take)
        .unwrap_or_default();
    items.retain(|item| item["type"] != "compaction_trigger");
    if compact {
        let typed = items
            .iter()
            .map(ResponseItem::deserialize)
            .collect::<Result<Vec<_>, _>>()
            .map_err(ResponsesError::InvalidJson)?;
        items.truncate(continuation_start(&typed));
    }
    for mut item in items {
        match item["type"].as_str() {
            Some("additional_tools") => {
                for tool in item["tools"].as_array().into_iter().flatten() {
                    flatten_tool(tool, None, &mut tools);
                }
                continue;
            }
            Some("custom_tool_call") => {
                item["type"] = json!("function_call");
                item["arguments"] = json!(json!({"input": item["input"]}).to_string());
                item.as_object_mut().unwrap().remove("input");
            }
            Some("custom_tool_call_output") => {
                item["type"] = json!("function_call_output");
                item.as_object_mut().unwrap().remove("name");
            }
            Some("tool_search_call") => {
                item = json!({"type":"function_call", "call_id":item["call_id"], "name":"tool_search", "arguments":item["arguments"].to_string()});
            }
            Some("tool_search_output") => {
                for tool in item["tools"].as_array().into_iter().flatten() {
                    flatten_tool(tool, None, &mut tools);
                }
                item = json!({"type":"function_call_output", "call_id":item["call_id"], "output":item["tools"].to_string()});
            }
            Some("reasoning") => {
                // Muse requires this even when reasoning summaries are unavailable.
                if item.get("summary").is_none() {
                    item["summary"] = json!([]);
                }
                item.as_object_mut().unwrap().remove("content");
            }
            _ => {}
        }
        if let Some(namespace) = item["namespace"].as_str().map(str::to_owned)
            && let Some(name) = item["name"].as_str()
        {
            item["name"] = json!(flat_name(Some(&namespace), name));
        }
        if let Some(fields) = item.as_object_mut() {
            for field in [
                "namespace",
                "async",
                "caller",
                "created_by",
                "encrypted_function_args",
                "internal_chat_message_metadata_passthrough",
            ] {
                fields.remove(field);
            }
        }
        input.push(item);
    }
    if compact {
        input.push(json!({"type":"message", "role":"user", "content":[{"type":"input_text", "text":SUMMARY_PROMPT}]}));
    }
    body["input"] = Value::Array(input);
    body["tools"] = Value::Array(tools);
    body["tool_choice"] = json!(if compact { "none" } else { "auto" });
    body["store"] = json!(false);
    body["stream"] = json!(true);
    body["truncation"] = json!("disabled");
    body["reasoning"] = json!({"effort": if compact { "minimal" } else { body["reasoning"]["effort"].as_str().unwrap_or("low") }, "summary":"auto"});
    if compact {
        body["max_output_tokens"] = json!(4096);
    }
    let fields = body.as_object_mut().unwrap();
    for field in [
        "type",
        "previous_response_id",
        "client_metadata",
        "prompt_cache_key",
        "service_tier",
    ] {
        fields.remove(field);
    }
    body["text"].as_object_mut().unwrap().remove("verbosity");
    EncodedRequest::new(&body)
}

fn flatten_tool(tool: &Value, namespace: Option<&str>, tools: &mut Vec<Value>) {
    let kind = tool["type"].as_str().unwrap_or_default();
    if kind == "namespace" {
        for child in tool["tools"].as_array().into_iter().flatten() {
            flatten_tool(child, tool["name"].as_str(), tools);
        }
        return;
    }
    let name = tool["name"].as_str().unwrap_or("tool_search");
    let name = flat_name(namespace, name);
    let mut declaration = tool.clone();
    declaration["name"] = json!(name);
    if kind == "custom" {
        declaration["type"] = json!("function");
        declaration["parameters"] = json!({"type":"object", "properties":{"input":{"type":"string", "description":"Free-form tool input"}}, "required":["input"], "additionalProperties":false});
        declaration["strict"] = json!(true);
        declaration.as_object_mut().unwrap().remove("format");
    }
    if kind == "tool_search" {
        // Keep discovery client-owned, exposed as an ordinary function.
        declaration["type"] = json!("function");
        declaration.as_object_mut().unwrap().remove("execution");
    }
    for field in ["async", "defer_loading"] {
        declaration.as_object_mut().unwrap().remove(field);
    }
    tools.push(declaration);
}

fn tool_mapping(request: &ResponsesAttempt) -> BTreeMap<String, (String, Option<String>, bool)> {
    fn collect(
        tool: &ToolDefinition,
        ns: Option<&str>,
        out: &mut BTreeMap<String, (String, Option<String>, bool)>,
    ) {
        match tool {
            ToolDefinition::Namespace { name, tools, .. } => {
                for tool in tools {
                    collect(tool, Some(name), out);
                }
            }
            ToolDefinition::Function { name, .. } | ToolDefinition::Custom { name, .. } => {
                out.insert(
                    flat_name(ns, name),
                    (
                        name.to_string(),
                        ns.map(str::to_owned),
                        matches!(tool, ToolDefinition::Custom { .. }),
                    ),
                );
            }
            ToolDefinition::ToolSearch { .. } => {
                out.insert("tool_search".into(), ("tool_search".into(), None, false));
            }
        }
    }
    let mut out = BTreeMap::new();
    for item in request.input_items() {
        match item {
            ResponseItem::AdditionalTools { tools, .. } => {
                for tool in tools {
                    collect(tool, None, &mut out);
                }
            }
            ResponseItem::ToolSearchOutput { tools, .. } => {
                for tool in tools {
                    if let Ok(definition) = ToolDefinition::deserialize(tool.as_value()) {
                        collect(&definition, None, &mut out);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub(crate) fn decode(
    output: ResponsesOutput,
    request: &ResponsesAttempt,
) -> Result<ResponsesOutput, ResponsesServiceError> {
    let ResponsesOutput::Generation(mut generated) = output else {
        return Ok(output);
    };
    normalize(&mut generated, request)?;
    Ok(
        if matches!(request.kind(), ResponsesAttemptKind::Compaction) {
            ResponsesOutput::Compaction(summary(generated)?)
        } else {
            ResponsesOutput::Generation(generated)
        },
    )
}

fn normalize(
    output: &mut GenerationOutput,
    request: &ResponsesAttempt,
) -> Result<(), ResponsesServiceError> {
    let mapping = tool_mapping(request);
    for call in &mut output.code_calls {
        if let Some((name, ns, custom)) = mapping.get(&call.name) {
            call.name.clone_from(name);
            call.namespace.clone_from(ns);
            if name == "tool_search" && ns.is_none() {
                call.kind = CodeCallKind::ToolSearch;
            }
            if *custom {
                call.input = wrapped_input(&call.input)?
                    .as_str()
                    .ok_or_else(|| invalid("Muse custom-tool wrapper requires a string input"))?
                    .to_owned();
                call.kind = CodeCallKind::Custom;
            }
        }
    }
    if output.end_turn == Some(false) {
        for item in &mut output.output_items {
            if let ResponseItem::Message {
                role: MessageRole::Assistant,
                phase,
                ..
            } = item
            {
                *phase = Some(nanocodex_oai_api::responses::MessagePhase::Commentary);
            }
        }
    }
    // Restore the internal representation; outbound encoding wraps it again on replay.
    for item in &mut output.output_items {
        if let ResponseItem::FunctionCall {
            name,
            namespace,
            call_id,
            arguments,
            ..
        } = item
            && let Some((original, ns, custom)) = mapping.get(name.as_ref())
        {
            if original == "tool_search" && ns.is_none() {
                let input: Value = serde_json::from_str(arguments)
                    .map_err(|_| invalid("Muse tool-search arguments must be JSON"))?;
                *item = serde_json::from_value(json!({"type":"tool_search_call", "call_id":call_id, "execution":"client", "arguments":input}))
                    .map_err(|_| invalid("invalid Muse tool search call"))?;
            } else if *custom {
                let mut restored = json!({"type":"custom_tool_call", "name":original, "call_id":call_id, "input":wrapped_input(arguments)?});
                if let Some(ns) = ns {
                    restored["namespace"] = json!(ns);
                }
                *item = serde_json::from_value(restored)
                    .map_err(|_| invalid("invalid Muse custom tool call"))?;
            } else {
                *name = original.clone().into();
                *namespace = ns.clone().map(Into::into);
            }
        }
    }
    Ok(())
}

fn summary(output: GenerationOutput) -> Result<CompactionOutput, ResponsesServiceError> {
    if output.status != "completed" || !output.code_calls.is_empty() {
        return Err(invalid("Muse compaction must complete without tool calls"));
    }
    let mut text = String::new();
    for item in &output.output_items {
        match item {
            ResponseItem::Reasoning { .. } => {}
            ResponseItem::Message {
                role: MessageRole::Assistant,
                content,
                ..
            } => {
                for part in content {
                    if let ContentItem::OutputText { text: part, .. } = part {
                        text.push_str(part);
                    } else {
                        return Err(invalid("Muse compaction returned non-text content"));
                    }
                }
            }
            _ => return Err(invalid("Muse compaction returned an unexpected item")),
        }
    }
    if text.trim().is_empty() {
        return Err(invalid("Muse compaction returned an empty summary"));
    }
    Ok(CompactionOutput {
        id: output.id,
        status: output.status,
        item: ResponseItem::message(
            MessageRole::User,
            [ContentItem::InputText {
                text: format!("{SUMMARY_OPEN}{text}{SUMMARY_CLOSE}").into(),
            }],
        ),
        usage: output.usage,
        time_to_first_event_ns: output.time_to_first_event_ns,
        time_to_first_output_ns: output.time_to_first_output_ns,
        pipeline_stats: output.pipeline_stats,
    })
}

fn flat_name(namespace: Option<&str>, name: &str) -> String {
    namespace.map_or_else(|| name.to_owned(), |ns| format!("{ns}__{name}"))
}

fn wrapped_input(arguments: &str) -> Result<Value, ResponsesServiceError> {
    let mut arguments: Value = serde_json::from_str(arguments)
        .map_err(|_| invalid("Muse custom-tool wrapper arguments must be JSON"))?;
    arguments
        .as_object_mut()
        .and_then(|fields| fields.remove("input"))
        .filter(Value::is_string)
        .ok_or_else(|| invalid("Muse custom-tool wrapper must contain a string input"))
}

const fn invalid(detail: &'static str) -> ResponsesServiceError {
    ResponsesServiceError::protocol(detail)
}

fn is_summary(item: &ResponseItem) -> bool {
    matches!(item, ResponseItem::Message {role:MessageRole::User, content, ..}
        if matches!(content.as_slice(), [ContentItem::InputText {text}] if text.starts_with(SUMMARY_OPEN) && text.ends_with(SUMMARY_CLOSE)))
}

pub(crate) fn install_summary(
    mut history: Vec<ResponseItem>,
    initial: impl IntoIterator<Item = ResponseItem>,
    summary: ResponseItem,
) -> Vec<ResponseItem> {
    let kept = history.split_off(continuation_start(&history));
    let mut result = initial
        .into_iter()
        .filter(|item| !item.is_user_message())
        .collect::<Vec<_>>();
    result.push(summary);
    result.extend(kept.into_iter().filter(|item| !is_summary(item)));
    result
}

fn continuation_start(history: &[ResponseItem]) -> usize {
    // Lift Claude's latest-assistant boundary policy, retaining the entire Responses
    // reasoning/call batch and all matching tool receipts. Do not summarize that suffix.
    if matches!(history.last(), Some(ResponseItem::Message {role:MessageRole::Assistant, phase, ..})
        if !matches!(phase, Some(nanocodex_oai_api::responses::MessagePhase::Commentary)))
    {
        return history.len();
    }
    if history.last().is_some_and(ResponseItem::is_user_message) {
        return history.len() - 1;
    }
    let is_assistant = |item: &ResponseItem| {
        matches!(
            item,
            ResponseItem::Message {
                role: MessageRole::Assistant,
                ..
            } | ResponseItem::Reasoning { .. }
                | ResponseItem::FunctionCall { .. }
                | ResponseItem::CustomToolCall { .. }
                | ResponseItem::ToolSearchCall { .. }
        )
    };
    let Some(mut start) = history.iter().rposition(is_assistant) else {
        return history.len();
    };
    while start > 0 && is_assistant(&history[start - 1]) {
        start -= 1;
    }
    start
}

pub(crate) const fn auto_compact_token_limit(tokens: u64) -> u64 {
    if tokens <= 33_000 {
        tokens.saturating_mul(95) / 100
    } else {
        tokens - 33_000
    }
}
