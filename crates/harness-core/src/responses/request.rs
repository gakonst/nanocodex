use std::sync::Arc;

use serde::{Serialize, ser::SerializeSeq};

use super::ResponseItem;
use crate::ModelConfig;

/// Stable request metadata and prefix shared by every operation in a session.
#[derive(Clone)]
pub struct RequestProfile {
    prompt_cache_key: String,
    prefix: Arc<[ResponseItem]>,
}

impl RequestProfile {
    #[must_use]
    pub fn new(prompt_cache_key: impl Into<String>, prefix: Arc<[ResponseItem]>) -> Self {
        Self {
            prompt_cache_key: prompt_cache_key.into(),
            prefix,
        }
    }

    #[must_use]
    pub fn prompt_cache_key(&self) -> &str {
        &self.prompt_cache_key
    }

    #[must_use]
    pub fn prefix(&self) -> &[ResponseItem] {
        &self.prefix
    }
}

#[derive(Clone, Copy)]
pub struct ResponsesInput<'a> {
    first: &'a [ResponseItem],
    second: &'a [ResponseItem],
    tail: Option<&'a ResponseItem>,
}

impl<'a> ResponsesInput<'a> {
    #[must_use]
    pub const fn new(
        first: &'a [ResponseItem],
        second: &'a [ResponseItem],
        tail: Option<&'a ResponseItem>,
    ) -> Self {
        Self {
            first,
            second,
            tail,
        }
    }

    pub fn iter(self) -> impl Iterator<Item = &'a ResponseItem> {
        self.first.iter().chain(self.second).chain(self.tail)
    }

    #[must_use]
    pub fn len(self) -> usize {
        self.first.len() + self.second.len() + usize::from(self.tail.is_some())
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

impl Serialize for ResponsesInput<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for item in self.iter() {
            sequence.serialize_element(item)?;
        }
        sequence.end()
    }
}

#[derive(Serialize)]
pub struct ResponseCreate<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    input: ResponsesInput<'a>,
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
    #[must_use]
    pub fn warmup(
        config: &'a ModelConfig,
        profile: &'a RequestProfile,
        turn_state: Option<&'a str>,
    ) -> Self {
        Self::new(
            config,
            ResponsesInput::new(profile.prefix(), &[], None),
            None,
            Some(false),
            profile,
            turn_state,
        )
    }

    #[must_use]
    pub fn generation(
        config: &'a ModelConfig,
        input: ResponsesInput<'a>,
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
        input: ResponsesInput<'a>,
        previous_response_id: Option<&'a str>,
        generate: Option<bool>,
        profile: &'a RequestProfile,
        turn_state: Option<&'a str>,
    ) -> Self {
        Self {
            kind: "response.create",
            model: crate::MODEL,
            previous_response_id,
            input,
            tool_choice: "auto",
            parallel_tool_calls: false,
            reasoning: ReasoningControls {
                effort: config.thinking.as_str(),
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
    use crate::{ContentItem, MessageRole, Thinking};
    use serde_json::json;

    #[test]
    fn prompt_cache_key_is_stable_across_the_session() {
        let config = ModelConfig {
            api_key: "test-key".to_owned(),
            thinking: Thinking::Low,
            ..ModelConfig::default()
        };
        let prefix: Arc<[ResponseItem]> = Arc::from([ResponseItem::message(
            MessageRole::Developer,
            [ContentItem::InputText {
                text: "system prompt".into(),
            }],
        )]);
        let profile = RequestProfile::new("session-a", prefix);
        let request = ResponseCreate::warmup(&config, &profile, None);
        let request = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(request["prompt_cache_key"], json!("session-a"));
        assert_eq!(request["client_metadata"]["session_id"], json!("session-a"));
        assert_eq!(request["client_metadata"]["thread_id"], json!("session-a"));
        assert_eq!(request["store"], false);
        assert_eq!(request["generate"], false);
        assert!(request.get("tools").is_none());
        assert!(request.get("instructions").is_none());
        assert!(request["reasoning"].get("summary").is_none());
        assert!(request["reasoning"].get("mode").is_none());
        assert!(request.get("context_management").is_none());
    }

    #[test]
    fn thinking_defaults_to_medium() {
        assert_eq!(ModelConfig::default().thinking, Thinking::Medium);
    }
}
