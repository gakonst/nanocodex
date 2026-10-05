// Copyright 2023-2026 SpaceXAI
// SPDX-License-Identifier: Apache-2.0
// Adapted by Nanocodex from xai-org/grok-build at the revision in UPSTREAM.md.
// Original: crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs
// and sanitize_tool_arguments in its parent conversation.rs.
// Changes: replace async-openai/ConversationItem types with serde_json values,
// retain native output order directly, and validate client-call identities before
// host dispatch. This module has no transport, credential or tool-execution I/O.
use serde_json::Value;

/// A complete client function invocation, keyed by the provider's call_id.
/// The output-item id is a separate identity and must not correlate tool results.
pub(crate) struct FunctionCall {
    pub(crate) call_id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
}

/// Adapted from response_to_conversation_items and conversation_item_to_input_items.
/// Reasoning and server-executed tool items remain native siblings. Replay uses
/// emitted order instead of folding all messages/calls into one AssistantItem.
pub(crate) fn replay_output(output: &[Value]) -> Vec<Value> {
    let mut input = Vec::with_capacity(output.len());
    for original in output {
        // Match the upstream converter's bounded replay set. In particular,
        // MCP calls are not locally executable function calls.
        if !matches!(
            original["type"].as_str(),
            Some(
                "message"
                    | "reasoning"
                    | "function_call"
                    | "web_search_call"
                    | "x_search_call"
                    | "custom_tool_call"
                    | "code_interpreter_call"
            )
        ) {
            continue;
        }
        let mut item = original.clone();
        // Upstream explicitly strips reasoning's output-only status.
        // Its FunctionToolCall input construction also omits id/status.
        if matches!(item["type"].as_str(), Some("reasoning" | "function_call"))
            && let Some(object) = item.as_object_mut()
        {
            object.remove("status");
            if object.get("type").and_then(Value::as_str) == Some("function_call") {
                object.remove("id");
                // Port of upstream sanitize_tool_arguments: retain the paired
                // error result while preventing one invalid call from making
                // every later provider request fail JSON validation.
                if object
                    .get("arguments")
                    .and_then(Value::as_str)
                    .is_some_and(|arguments| serde_json::from_str::<Value>(arguments).is_err())
                {
                    object.insert("arguments".into(), Value::String("{}".into()));
                }
            }
        }
        input.push(item);
    }
    patch_reasoning_text_types(&mut input);
    input
}

/// Port of upstream patch_reasoning_text_types: the xAI input API requires this
/// discriminator even when its response content only contains a text field.
pub(crate) fn patch_reasoning_text_types(input: &mut [Value]) {
    for item in input {
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in content {
            if let Some(object) = part.as_object_mut() {
                object
                    .entry("type")
                    .or_insert_with(|| Value::String("reasoning_text".into()));
            }
        }
    }
}

/// Extract only client functions. Hosted search/code calls have already run on
/// the provider and must never be replayed as an application effect.
pub(crate) fn function_calls(output: &[Value]) -> Result<Vec<FunctionCall>, String> {
    let mut calls = Vec::new();
    for item in output {
        if item["type"] != "function_call" {
            continue;
        }
        let required = |key: &str| -> Result<&str, String> {
            item.get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| format!("xAI function call omitted a valid {key}"))
        };
        let call_id = required("call_id")?.to_owned();
        let name = required("name")?.to_owned();
        // Preserve malformed JSON as a string: the loop returns a paired tool
        // error for invalid arguments without invoking the host callback.
        let arguments = item
            .get("arguments")
            .and_then(Value::as_str)
            .ok_or_else(|| "xAI function call arguments must be a string".to_owned())?
            .to_owned();
        calls.push(FunctionCall {
            call_id,
            name,
            arguments,
        });
    }
    Ok(calls)
}

/// Port of the upstream output-text collection, including its newline separator
/// between text parts. Reasoning/refusals/hosted results are not assistant text.
pub(crate) fn assistant_text(output: &[Value]) -> String {
    let mut content = String::new();
    for item in output {
        if item["type"] != "message" {
            continue;
        }
        let Some(parts) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            if part["type"] == "output_text"
                && let Some(text) = part.get("text").and_then(Value::as_str)
            {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(text);
            }
        }
    }
    content
}
