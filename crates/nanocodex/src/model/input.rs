#[cfg(not(target_family = "wasm"))]
use chrono::{Local, Utc};
use nanocodex_core::{
    ContentItem, FunctionOutputBody, FunctionOutputContent, MessageRole, ResponseItem,
};
use nanocodex_tools::{ToolOutputBody, ToolOutputContent};

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
    user_content: Vec<ContentItem>,
    workspace: &str,
    shell: &str,
    project_instructions: Option<&str>,
) -> Vec<ResponseItem> {
    let (current_date, timezone) = local_time_context();
    task_input_with_time_context(
        user_content,
        workspace,
        shell,
        project_instructions,
        &current_date,
        &timezone,
    )
}

pub(in crate::model) fn task_context(
    workspace: &str,
    shell: &str,
    project_instructions: Option<&str>,
) -> ResponseItem {
    let (current_date, timezone) = local_time_context();
    task_context_with_time(
        workspace,
        shell,
        project_instructions,
        &current_date,
        &timezone,
    )
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

fn task_input_with_time_context(
    user_content: Vec<ContentItem>,
    workspace: &str,
    shell: &str,
    project_instructions: Option<&str>,
    current_date: &str,
    timezone: &str,
) -> Vec<ResponseItem> {
    vec![
        developer_context(),
        task_context_with_time(
            workspace,
            shell,
            project_instructions,
            current_date,
            timezone,
        ),
        ResponseItem::message(MessageRole::User, user_content),
    ]
}

pub(in crate::model) fn developer_context() -> ResponseItem {
    ResponseItem::message(
        MessageRole::Developer,
        [ContentItem::InputText {
            text: PERMISSIONS_INSTRUCTIONS.into(),
        }],
    )
}

fn task_context_with_time(
    workspace: &str,
    shell: &str,
    project_instructions: Option<&str>,
    current_date: &str,
    timezone: &str,
) -> ResponseItem {
    let mut context = Vec::with_capacity(2);
    if let Some(project_instructions) = project_instructions {
        context.push(ContentItem::InputText {
            text: format!(
                "# AGENTS.md instructions for {workspace}\n\n<INSTRUCTIONS>\n{project_instructions}\n</INSTRUCTIONS>"
            )
            .into_boxed_str(),
        });
    }
    context.push(ContentItem::InputText {
        text: environment_context(workspace, shell, current_date, timezone).into_boxed_str(),
    });
    ResponseItem::message(MessageRole::User, context)
}

#[cfg(not(target_family = "wasm"))]
fn local_time_context() -> (String, String) {
    match iana_time_zone::get_timezone() {
        Ok(timezone) => (Local::now().format("%Y-%m-%d").to_string(), timezone),
        Err(_) => (
            Utc::now().format("%Y-%m-%d").to_string(),
            "Etc/UTC".to_owned(),
        ),
    }
}

#[cfg(target_family = "wasm")]
fn local_time_context() -> (String, String) {
    let now = js_sys::Date::new_0();
    let current_date = format!(
        "{:04}-{:02}-{:02}",
        now.get_full_year(),
        now.get_month() + 1,
        now.get_date()
    );
    let formatter =
        js_sys::Intl::DateTimeFormat::new(&js_sys::Array::new(), &js_sys::Object::new());
    let timezone = js_sys::Reflect::get(
        &formatter.resolved_options(),
        &wasm_bindgen::JsValue::from_str("timeZone"),
    )
    .ok()
    .and_then(|value| value.as_string())
    .unwrap_or_else(|| "Etc/UTC".to_owned());
    (current_date, timezone)
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
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanocodex_core::ImageDetail;
    use nanocodex_tools::ToolOutputContent;
    use serde_json::json;

    #[test]
    fn task_input_matches_codex_context_shape() {
        let input = task_input_with_time_context(
            vec![ContentItem::InputText {
                text: "fix the bug".into(),
            }],
            "/workspace/a&b",
            "bash",
            Some("Follow the project formatter."),
            "2026-07-17",
            "America/Los_Angeles",
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
            ]),
        )];

        let request = serde_json::to_value(input).expect("tool output should serialize");

        assert!(request[0]["output"][1].get("detail").is_none());
    }
}
