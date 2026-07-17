use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    model::ModelConfig,
    protocol::Task,
    tools::{ToolOutputBody, ToolRuntime},
};

const PROJECT_CONTEXT_HEADER: &str = "# Project context";

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
                    "tools": [runtime.exec_spec()],
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
    project_instructions: Option<&str>,
) -> Vec<Value> {
    let mut context = vec![json!({
        "type": "input_text",
        "text": PROJECT_CONTEXT_HEADER,
    })];
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
        "text": format!(
            "<environment_context>\n<cwd>{workspace}</cwd>\n<shell>/bin/sh</shell>\n</environment_context>"
        ),
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

#[derive(Serialize)]
pub(in crate::model) struct ResponseCreate<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    input: &'a [Value],
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
            input,
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

pub(in crate::model) fn custom_tool_output(call_id: &str, output: &ToolOutputBody) -> Value {
    json!({
        "type": "custom_tool_call_output",
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
    fn prompt_cache_key_is_scoped_to_the_session() {
        let config = ModelConfig {
            model: "test-model".to_owned(),
            api_key: "test-key".to_owned(),
            effort: ReasoningEffort::Low,
            websocket_url: "ws://localhost".to_owned(),
        };
        let runtime = ToolRuntime::new(".");
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
        assert!(request["reasoning"].get("mode").is_none());
        assert!(request.get("context_management").is_none());
    }
}
