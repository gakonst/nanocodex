use chrono::{Local, Utc};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    model::ModelConfig,
    protocol::Task,
    tools::{ToolOutputBody, ToolRuntime},
};

pub(in crate::model) struct RequestProfile {
    prompt_cache_key: String,
    prefix: Vec<Value>,
}

impl RequestProfile {
    pub(in crate::model) fn new(session_id: &str, runtime: &ToolRuntime) -> Self {
        Self {
            prompt_cache_key: session_id.to_owned(),
            prefix: vec![
                json!({
                    "type": "additional_tools",
                    "role": "developer",
                    "tools": runtime.model_specs(),
                }),
                json!({
                    "type": "message",
                    "role": "developer",
                    "content": [{
                        "type": "input_text",
                        "text": ModelConfig::system_prompt(),
                    }],
                }),
            ],
        }
    }

    pub(in crate::model) fn prompt_cache_key(&self) -> &str {
        &self.prompt_cache_key
    }

    pub(in crate::model) fn prefix(&self) -> &[Value] {
        &self.prefix
    }

    pub(in crate::model) fn full_input(&self, history: &[Value]) -> Vec<Value> {
        let mut input = Vec::with_capacity(self.prefix.len() + history.len());
        input.extend_from_slice(&self.prefix);
        input.extend_from_slice(history);
        input
    }
}

pub(in crate::model) fn task_input(
    task: &Task,
    workspace: &str,
    shell: &str,
    project_instructions: Option<&str>,
) -> Vec<Value> {
    let (current_date, timezone) = local_time_context();
    task_input_with_time_context(
        task,
        workspace,
        shell,
        project_instructions,
        &current_date,
        &timezone,
    )
}

fn task_input_with_time_context(
    task: &Task,
    workspace: &str,
    shell: &str,
    project_instructions: Option<&str>,
    current_date: &str,
    timezone: &str,
) -> Vec<Value> {
    let mut context = Vec::with_capacity(2);
    if let Some(project_instructions) = project_instructions {
        context.push(json!({
            "type": "input_text",
            "text": format!(
                "# AGENTS.md instructions for {workspace}\n\n<INSTRUCTIONS>\n{project_instructions}\n</INSTRUCTIONS>"
            ),
        }));
    }
    context.push(json!({
        "type": "input_text",
        "text": environment_context(workspace, shell, current_date, timezone),
    }));
    vec![
        json!({
            "type": "message",
            "role": "user",
            "content": context,
        }),
        json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": task.instruction,
            }],
        }),
    ]
}

fn local_time_context() -> (String, String) {
    match iana_time_zone::get_timezone() {
        Ok(timezone) => (Local::now().format("%Y-%m-%d").to_string(), timezone),
        Err(_) => (
            Utc::now().format("%Y-%m-%d").to_string(),
            "Etc/UTC".to_owned(),
        ),
    }
}

fn environment_context(workspace: &str, shell: &str, current_date: &str, timezone: &str) -> String {
    let mut context = String::from("<environment_context>\n  <cwd>");
    push_xml_escaped_text(&mut context, workspace);
    context.push_str("</cwd>\n  <shell>");
    push_xml_escaped_text(&mut context, shell);
    context.push_str("</shell>\n  <current_date>");
    push_xml_escaped_text(&mut context, current_date);
    context.push_str("</current_date>\n  <timezone>");
    push_xml_escaped_text(&mut context, timezone);
    context.push_str("</timezone>\n  <filesystem><workspace_roots><root>");
    push_xml_escaped_text(&mut context, workspace);
    context.push_str(
        "</root></workspace_roots><permission_profile type=\"disabled\"><file_system type=\"unrestricted\" /></permission_profile></filesystem>\n</environment_context>",
    );
    context
}

fn push_xml_escaped_text(output: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ => output.push(character),
        }
    }
}

#[derive(Serialize)]
pub(in crate::model) struct ResponseCreate<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    input: Vec<Value>,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: ReasoningControls,
    store: bool,
    stream: bool,
    include: [&'static str; 1],
    prompt_cache_key: &'a str,
    text: TextControls,
    #[serde(skip_serializing_if = "Option::is_none")]
    generate: Option<bool>,
    client_metadata: ClientMetadata<'a>,
}

impl<'a> ResponseCreate<'a> {
    pub(in crate::model) fn warmup(
        config: &'a ModelConfig,
        profile: &'a RequestProfile,
        turn_state: Option<&'a str>,
    ) -> Self {
        Self::new(
            config,
            profile.prefix(),
            None,
            Some(false),
            profile,
            turn_state,
        )
    }

    pub(in crate::model) fn generation(
        config: &'a ModelConfig,
        input: &'a [Value],
        previous_response_id: Option<&'a str>,
        profile: &'a RequestProfile,
        turn_state: Option<&'a str>,
    ) -> Self {
        Self::new(
            config,
            input,
            previous_response_id,
            None,
            profile,
            turn_state,
        )
    }

    fn new(
        config: &'a ModelConfig,
        input: &'a [Value],
        previous_response_id: Option<&'a str>,
        generate: Option<bool>,
        profile: &'a RequestProfile,
        turn_state: Option<&'a str>,
    ) -> Self {
        Self {
            kind: "response.create",
            model: &config.model,
            previous_response_id,
            input: responses_lite_input(input),
            tool_choice: "auto",
            parallel_tool_calls: false,
            reasoning: ReasoningControls {
                effort: config.effort.as_str(),
                context: "all_turns",
            },
            store: false,
            stream: true,
            include: ["reasoning.encrypted_content"],
            prompt_cache_key: profile.prompt_cache_key(),
            text: TextControls { verbosity: "low" },
            generate,
            client_metadata: ClientMetadata {
                session_id: profile.prompt_cache_key(),
                thread_id: profile.prompt_cache_key(),
                responses_lite: "true",
                turn_state,
            },
        }
    }
}

fn responses_lite_input(input: &[Value]) -> Vec<Value> {
    let mut input = input.to_vec();
    for item in &mut input {
        strip_image_details(item);
    }
    input
}

fn strip_image_details(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                strip_image_details(value);
            }
        }
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("input_image") {
                object.remove("detail");
            }
            for value in object.values_mut() {
                strip_image_details(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

pub(in crate::model) fn custom_tool_output(call_id: &str, output: &ToolOutputBody) -> Value {
    json!({
        "type": "custom_tool_call_output",
        "call_id": call_id,
        "output": output,
    })
}

pub(in crate::model) fn custom_tool_notification(call_id: &str, text: &str) -> Value {
    json!({
        "type": "custom_tool_call_output",
        "call_id": call_id,
        "name": "exec",
        "output": text,
    })
}

pub(in crate::model) fn function_tool_output(call_id: &str, output: &ToolOutputBody) -> Value {
    json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output,
    })
}

#[derive(Clone, Copy, Serialize)]
struct ReasoningControls {
    effort: &'static str,
    context: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct TextControls {
    verbosity: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct ClientMetadata<'a> {
    session_id: &'a str,
    thread_id: &'a str,
    #[serde(rename = "ws_request_header_x_openai_internal_codex_responses_lite")]
    responses_lite: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "x-codex-turn-state")]
    turn_state: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReasoningEffort;

    #[test]
    fn task_input_matches_codex_context_shape() {
        let task = Task {
            instruction: "fix the bug".to_owned(),
            workspace: None,
        };

        assert_eq!(
            task_input_with_time_context(
                &task,
                "/workspace/a&b",
                "bash",
                Some("Follow the project formatter."),
                "2026-07-17",
                "America/Los_Angeles",
            ),
            vec![
                json!({
                    "type": "message",
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": "# AGENTS.md instructions for /workspace/a&b\n\n<INSTRUCTIONS>\nFollow the project formatter.\n</INSTRUCTIONS>",
                        },
                        {
                            "type": "input_text",
                            "text": "<environment_context>\n  <cwd>/workspace/a&amp;b</cwd>\n  <shell>bash</shell>\n  <current_date>2026-07-17</current_date>\n  <timezone>America/Los_Angeles</timezone>\n  <filesystem><workspace_roots><root>/workspace/a&amp;b</root></workspace_roots><permission_profile type=\"disabled\"><file_system type=\"unrestricted\" /></permission_profile></filesystem>\n</environment_context>",
                        },
                    ],
                }),
                json!({
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "fix the bug",
                    }],
                }),
            ],
        );
    }

    #[test]
    fn prompt_cache_key_is_scoped_to_the_session() {
        let config = ModelConfig {
            model: "test-model".to_owned(),
            api_key: "test-key".to_owned(),
            effort: ReasoningEffort::Low,
            websocket_url: "ws://localhost".to_owned(),
            api_base_url: "http://localhost/v1".to_owned(),
        };
        let runtime = ToolRuntime::new(
            ".",
            crate::tools::WebSearchConfig {
                endpoint: config.search_endpoint(),
                api_key: config.api_key.clone(),
            },
            crate::tools::ImageGenerationConfig {
                api_base_url: config.api_base_url.clone(),
                api_key: config.api_key.clone(),
                save_root: std::env::temp_dir().join("harness-test-images"),
            },
        );
        let first = RequestProfile::new("session-a", &runtime);
        let same = RequestProfile::new("session-a", &runtime);
        let different = RequestProfile::new("session-b", &runtime);

        assert_eq!(first.prompt_cache_key(), "session-a");
        assert_eq!(first.prompt_cache_key(), same.prompt_cache_key());
        assert_ne!(first.prompt_cache_key(), different.prompt_cache_key());

        let request = ResponseCreate::warmup(&config, &first, None);
        let request = serde_json::to_value(request).expect("request should serialize");
        assert_eq!(request["store"], false);
        assert_eq!(request["generate"], false);
        assert!(request.get("tools").is_none());
        assert!(request.get("instructions").is_none());
        assert!(request["reasoning"].get("summary").is_none());
        assert!(request["reasoning"].get("mode").is_none());
        assert!(request.get("context_management").is_none());
    }

    #[test]
    fn responses_lite_request_copy_strips_image_details() {
        let config = ModelConfig {
            model: "test-model".to_owned(),
            api_key: "test-key".to_owned(),
            effort: ReasoningEffort::Low,
            websocket_url: "ws://localhost".to_owned(),
            api_base_url: "http://localhost/v1".to_owned(),
        };
        let runtime = ToolRuntime::new(
            ".",
            crate::tools::WebSearchConfig {
                endpoint: config.search_endpoint(),
                api_key: config.api_key.clone(),
            },
            crate::tools::ImageGenerationConfig {
                api_base_url: config.api_base_url.clone(),
                api_key: config.api_key.clone(),
                save_root: std::env::temp_dir().join("harness-test-images"),
            },
        );
        let profile = RequestProfile::new("session-a", &runtime);
        let input = vec![json!({
            "type": "custom_tool_call_output",
            "call_id": "call-1",
            "output": [
                {"type": "input_text", "text": "before"},
                {
                    "type": "input_image",
                    "image_url": "data:image/png;base64,a",
                    "detail": "original"
                }
            ]
        })];
        let original = input.clone();

        let request = ResponseCreate::generation(&config, &input, None, &profile, None);
        let request = serde_json::to_value(request).expect("request should serialize");

        assert!(request["input"][0]["output"][1].get("detail").is_none());
        assert_eq!(input, original);
    }
}
