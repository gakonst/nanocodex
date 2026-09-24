use nanocodex_oai_api::{
    Prompt, PromptMessageRole,
    responses::{
        ContentItem, FunctionOutputBody, FunctionOutputContent, MessageRole, ResponseItem,
    },
};
use nanocodex_tools::contract::{ToolOutputBody, ToolOutputContent};
use serde_json::Value;

use super::context::ContextSnapshot;

pub(in crate::model) fn task_input(
    prompt: &Prompt,
    user_content: Vec<ContentItem>,
    context: &ContextSnapshot,
) -> Vec<ResponseItem> {
    let mut input = vec![developer_context(), context.full_item()];
    input.extend(prompt_messages(prompt, user_content));
    input
}

pub(in crate::model) fn prompt_messages(
    prompt: &Prompt,
    user_content: Vec<ContentItem>,
) -> Vec<ResponseItem> {
    let mut input = Vec::with_capacity(prompt.transcript().len() + 1);
    input.extend(prompt.transcript().iter().map(|message| {
        let role = match message.role() {
            PromptMessageRole::User => MessageRole::User,
            PromptMessageRole::Assistant => MessageRole::Assistant,
        };
        let content = match message.role() {
            PromptMessageRole::User => ContentItem::input_text(message.content()),
            PromptMessageRole::Assistant => ContentItem::output_text(message.content()),
        };
        ResponseItem::message(role, [content])
    }));
    input.push(ResponseItem::message(MessageRole::User, user_content));
    input
}

pub(in crate::model) fn turn_aborted() -> ResponseItem {
    ResponseItem::message(
        MessageRole::User,
        [ContentItem::InputText {
            text: concat!(
                "<turn_aborted>\n",
                "The user interrupted the previous turn on purpose. Any running unified exec ",
                "processes may still be running in the background. If any tools/commands were ",
                "aborted, they may have partially executed.\n",
                "</turn_aborted>"
            )
            .into(),
        }],
    )
}

pub(in crate::model) fn developer_context() -> ResponseItem {
    ResponseItem::message(
        MessageRole::Developer,
        [ContentItem::InputText {
            text: permissions_instructions().into(),
        }],
    )
}

fn permissions_instructions() -> String {
    #[cfg(not(target_family = "wasm"))]
    {
        // This native runtime enforces full access, networking, and no escalation.
        // Keep the bundled upstream sections exact; hosted WASM uses host facts.
        let sandbox = include_str!("prompts/danger_full_access.md")
            .replace("{{ network_access }}", "enabled");
        format!(
            "<permissions instructions>\n{sandbox}{}</permissions instructions>",
            include_str!("prompts/never.md"),
        )
    }
    #[cfg(target_family = "wasm")]
    {
        // WASM delegates execution to a host whose grant can vary per tool and
        // hand. It cannot truthfully manufacture a global full-access profile.
        "<host_execution_context>\nExecution permissions are supplied and enforced by the host for each tool and selected environment. The embedded runtime does not grant filesystem, network, or escalation access.\n</host_execution_context>".to_owned()
    }
}

pub(in crate::model) fn custom_tool_output(
    call_id: String,
    output: ToolOutputBody,
) -> ResponseItem {
    ResponseItem::custom_tool_output(call_id, None, function_output(output))
}

pub(in crate::model) fn custom_tool_notification(call_id: String, text: String) -> ResponseItem {
    ResponseItem::custom_tool_output(
        call_id,
        Some("exec".to_owned()),
        FunctionOutputBody::Text(text.into_boxed_str()),
    )
}

pub(in crate::model) fn function_tool_output(
    call_id: String,
    output: ToolOutputBody,
) -> ResponseItem {
    ResponseItem::function_call_output(call_id, function_output(output))
}

pub(in crate::model) fn tool_search_output(call_id: String, tools: Vec<Value>) -> ResponseItem {
    ResponseItem::ToolSearchOutput {
        id: None,
        call_id: Some(call_id.into_boxed_str()),
        status: "completed".into(),
        execution: "client".into(),
        tools: tools.into_iter().map(Into::into).collect(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_output(output: ToolOutputBody) -> FunctionOutputBody {
    match output {
        ToolOutputBody::Text(text) => FunctionOutputBody::Text(text.into_boxed_str()),
        ToolOutputBody::Content(content) => FunctionOutputBody::Content(
            content
                .into_iter()
                .map(|item| match item {
                    ToolOutputContent::InputText { text } => FunctionOutputContent::InputText {
                        text: text.into_boxed_str(),
                    },
                    ToolOutputContent::InputImage {
                        image_url,
                        detail: _,
                    } => FunctionOutputContent::InputImage {
                        image_url: image_url.into_boxed_str(),
                        detail: None,
                    },
                    ToolOutputContent::InputAudio { audio_url } => {
                        FunctionOutputContent::InputAudio {
                            audio_url: audio_url.into_boxed_str(),
                        }
                    }
                    ToolOutputContent::EncryptedContent { encrypted_content } => {
                        FunctionOutputContent::EncryptedContent {
                            encrypted_content: encrypted_content.into_boxed_str(),
                        }
                    }
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanocodex_oai_api::{ImageDetail, PromptMessage};
    use nanocodex_tools::contract::ToolOutputContent;
    use serde_json::json;

    #[test]
    fn task_input_preserves_synthetic_message_roles() {
        let context =
            ContextSnapshot::capture_at("/workspace", "bash", None, "2026-08-05", "Etc/UTC");
        let prompt = Prompt::new("return the second answer").with_transcript([
            PromptMessage::user("question one"),
            PromptMessage::assistant("answer one"),
            PromptMessage::user("question two"),
            PromptMessage::assistant("answer two"),
        ]);
        let input = task_input(
            &prompt,
            vec![ContentItem::input_text("return the second answer")],
            &context,
        );

        let roles = input
            .iter()
            .skip(2)
            .map(|item| serde_json::to_value(item).unwrap()["role"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            roles,
            vec![
                json!("user"),
                json!("assistant"),
                json!("user"),
                json!("assistant"),
                json!("user")
            ]
        );
    }

    #[test]
    fn task_input_preserves_consecutive_synthetic_user_messages() {
        let prompt = Prompt::new("continue").with_transcript([
            PromptMessage::user("benchmark preamble"),
            PromptMessage::user("question"),
            PromptMessage::assistant("answer"),
        ]);

        let input = prompt_messages(&prompt, vec![ContentItem::input_text("continue")]);
        let roles = input
            .iter()
            .map(|item| serde_json::to_value(item).unwrap()["role"].clone())
            .collect::<Vec<_>>();

        assert_eq!(
            roles,
            vec![
                json!("user"),
                json!("user"),
                json!("assistant"),
                json!("user")
            ]
        );
    }

    #[test]
    fn responses_lite_tool_output_omits_image_details_without_request_copy() {
        let input = vec![custom_tool_output(
            "call-1".to_owned(),
            ToolOutputBody::Content(vec![
                ToolOutputContent::InputText {
                    text: "before".to_owned(),
                },
                ToolOutputContent::InputImage {
                    image_url: "data:image/png;base64,a".to_owned(),
                    detail: ImageDetail::Original,
                },
                ToolOutputContent::EncryptedContent {
                    encrypted_content: "opaque-provider-payload".to_owned(),
                },
            ]),
        )];

        let request = serde_json::to_value(input).expect("tool output should serialize");

        assert!(request[0]["output"][1].get("detail").is_none());
        assert_eq!(
            request[0]["output"][2],
            json!({
                "type": "encrypted_content",
                "encrypted_content": "opaque-provider-payload",
            })
        );
    }
}
