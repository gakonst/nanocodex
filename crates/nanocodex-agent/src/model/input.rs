use nanocodex_oai_api::{
    Prompt, PromptMessageRole,
    responses::{
        ContentItem, FunctionOutputBody, FunctionOutputContent, MessageRole, ResponseItem,
    },
};
use nanocodex_tools::contract::{ToolOutputBody, ToolOutputContent};
use serde_json::Value;

use super::context::ContextSnapshot;

const PERMISSIONS_INSTRUCTIONS: &str = concat!(
    "<permissions instructions>\n",
    "Filesystem sandboxing defines which files can be read or written. `sandbox_mode` is ",
    "`danger-full-access`: No filesystem sandboxing - all commands are permitted. Network ",
    "access is enabled.\n",
    "Approval policy is currently never. Do not provide the `sandbox_permissions` for any ",
    "reason, commands will be rejected.\n",
    "</permissions instructions>",
);

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
            text: PERMISSIONS_INSTRUCTIONS.into(),
        }],
    )
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
    fn task_input_matches_codex_context_shape() {
        let context = ContextSnapshot::capture_at(
            "/workspace/a&b",
            "bash",
            Some("Follow the project formatter."),
            "2026-07-17",
            "America/Los_Angeles",
        );
        let prompt = Prompt::new("fix the bug");
        let input = task_input(
            &prompt,
            vec![ContentItem::InputText {
                text: "fix the bug".into(),
            }],
            &context,
        );
        assert_eq!(
            serde_json::to_value(input).unwrap(),
            json!([
                json!({
                    "type": "message",
                    "role": "developer",
                    "content": [
                        {
                            "type": "input_text",
                            "text": PERMISSIONS_INSTRUCTIONS,
                        },
                    ],
                }),
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
            ]),
        );
    }

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
    fn turn_aborted_matches_codex_context_shape() {
        assert_eq!(
            serde_json::to_value(turn_aborted()).unwrap(),
            json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": concat!(
                        "<turn_aborted>\n",
                        "The user interrupted the previous turn on purpose. Any running unified ",
                        "exec processes may still be running in the background. If any ",
                        "tools/commands were aborted, they may have partially executed.\n",
                        "</turn_aborted>"
                    ),
                }],
            }),
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
