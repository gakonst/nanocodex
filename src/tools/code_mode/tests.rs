use std::path::PathBuf;

use eyre::{Result, eyre};
use serde_json::Value;

use super::{CodeModeExecution, NestedToolCall, parse_exec_source};
use crate::tools::{ToolContext, ToolOutputBody, ToolOutputContent, ToolRuntime, WebSearchConfig};

#[tokio::test]
async fn reuses_one_node_host_between_cells() -> Result<()> {
    let workspace = temporary_workspace("persistent-node-host")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let context = test_context(&history);

    let first = tools.execute_code("text(process.pid)", context).await;
    let second = tools.execute_code("text(process.pid)", context).await;

    assert!(first.success);
    assert!(second.success);
    assert_eq!(emitted_text(&first)?, emitted_text(&second)?);
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn promise_all_runs_nested_tools_concurrently() -> Result<()> {
    let workspace = temporary_workspace("parallel-nested-tools")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
const [first, second] = await Promise.all([
  tools.exec_command({
    cmd: "touch first.started; i=0; while [ \"$i\" -lt 100 ]; do [ -f second.started ] && exit 0; i=$((i + 1)); sleep 0.01; done; exit 91",
    login: false,
  }),
  tools.exec_command({
    cmd: "touch second.started; i=0; while [ \"$i\" -lt 100 ]; do [ -f first.started ] && exit 0; i=$((i + 1)); sleep 0.01; done; exit 92",
    login: false,
  }),
]);
text({ first: first.exit_code, second: second.exit_code });
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success);
    assert_eq!(call_ids(&execution.nested_calls), ["code-1", "code-2"]);
    let result = serde_json::from_str::<Value>(emitted_text(&execution)?)?;
    assert_eq!(result, serde_json::json!({ "first": 0, "second": 0 }));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn failed_nested_tool_rejects_its_javascript_promise() -> Result<()> {
    let workspace = temporary_workspace("nested-tool-rejection")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
try {
  await tools.view_image({ path: "missing.png" });
  text("unexpected success");
} catch (error) {
  text(error);
}
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success);
    assert!(emitted_text(&execution)?.contains("unable to locate image"));
    assert_eq!(execution.nested_calls.len(), 1);
    assert!(!execution.nested_calls[0].success);
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn image_helper_requires_data_urls() -> Result<()> {
    let workspace = temporary_workspace("code-mode-image-urls")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();

    let remote = tools
        .execute_code(
            r#"image("https://example.com/image.png");"#,
            test_context(&history),
        )
        .await;
    assert!(!remote.success);
    assert!(execution_output(&remote).contains(
        "Tool call failed: remote image URLs are not supported in tool outputs. Pass a base64 data URI instead"
    ));

    let invalid = tools
        .execute_code(r#"image("not-an-image");"#, test_context(&history))
        .await;
    assert!(!invalid.success);
    assert!(
        execution_output(&invalid)
            .contains("Tool call failed: invalid image output. Pass a base64 data URI instead")
    );

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn image_helper_normalizes_detail_and_honors_override() -> Result<()> {
    let workspace = temporary_workspace("code-mode-image-detail")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"image({ image_url: "data:image/png;base64,a", detail: "low" }, "ORIGINAL");"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let ToolOutputBody::Content(content) = &execution.output else {
        return Err(eyre!("code-mode execution did not emit content"));
    };
    assert!(matches!(
        content.last(),
        Some(ToolOutputContent::InputImage {
            image_url,
            detail: crate::tools::ImageDetail::Original,
        }) if image_url == "data:image/png;base64,a"
    ));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn yielded_cell_completes_through_wait() -> Result<()> {
    let workspace = temporary_workspace("yielded-cell")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
text("before");
await yield_control();
await new Promise((resolve) => setTimeout(resolve, 10));
text("after");
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success);
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));
    assert!(execution_output(&execution).contains("before"));

    let completed = tools
        .wait_for_code(
            r#"{"cell_id":"1","yield_time_ms":1000}"#,
            test_context(&history),
        )
        .await;
    assert!(completed.success);
    assert!(execution_output(&completed).contains("Script completed"));
    assert!(execution_output(&completed).contains("after"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn freeform_apply_patch_accepts_a_string() -> Result<()> {
    let workspace = temporary_workspace("freeform-apply-patch")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
await tools.apply_patch("*** Begin Patch\n*** Add File: created.txt\n+created by patch\n*** End Patch");
text("done");
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(execution.nested_calls.len(), 1);
    assert_eq!(
        execution.nested_calls[0].input,
        Value::String(
            "*** Begin Patch\n*** Add File: created.txt\n+created by patch\n*** End Patch"
                .to_owned()
        )
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("created.txt"))?,
        "created by patch\n"
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn exec_pragma_and_wait_limit_direct_output() -> Result<()> {
    let workspace = temporary_workspace("code-output-limits")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            "// @exec: {\"max_output_tokens\": 2}\ntext(\"abcdefghijklmnop\")",
            test_context(&history),
        )
        .await;
    assert!(execution.success);
    assert!(execution_output(&execution).contains("Warning: truncated output"));

    let yielded = tools
        .execute_code(
            r#"
await yield_control();
text("abcdefghijklmnop");
"#,
            test_context(&history),
        )
        .await;
    assert!(yielded.success);
    let completed = tools
        .wait_for_code(
            r#"{"cell_id":"2","yield_time_ms":1000,"max_tokens":2}"#,
            test_context(&history),
        )
        .await;
    assert!(completed.success);
    assert!(execution_output(&completed).contains("Warning: truncated output"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[test]
fn exec_pragma_rejects_unknown_fields() {
    let error = parse_exec_source("// @exec: {\"unknown\": 1}\ntext('hi')")
        .err()
        .expect("unknown pragma fields should fail");
    assert!(error.contains("only supports"));
}

#[test]
fn model_description_uses_codex_style_declarations() {
    let workspace = temporary_workspace("code-mode-description")
        .expect("temporary test workspace should be available");
    let tools = test_tools(&workspace);
    let specs = tools.model_specs();
    let description = specs[0]["description"]
        .as_str()
        .expect("exec should have a description");
    assert!(description.contains("// @exec:"));
    assert!(description.contains("must be a base64-encoded `data:` URL"));
    assert!(description.contains("apply_patch(input: string): Promise<unknown>"));
    assert!(description.contains("exec_command(args: {"));
    assert!(!description.contains("Input schema:"));
    assert_eq!(
        specs[1]["parameters"]["properties"]["max_tokens"]["type"],
        "number"
    );
    std::fs::remove_dir_all(workspace).expect("temporary workspace should be removable");
}

fn emitted_text(execution: &CodeModeExecution) -> Result<&str> {
    let ToolOutputBody::Content(content) = &execution.output else {
        return Err(eyre!("code-mode execution did not emit content"));
    };
    content
        .iter()
        .rev()
        .find_map(|item| match item {
            ToolOutputContent::InputText { text } => Some(text.as_str()),
            ToolOutputContent::InputImage { .. } => None,
        })
        .ok_or_else(|| eyre!("code-mode execution did not emit text"))
}

fn execution_output(execution: &CodeModeExecution) -> String {
    match &execution.output {
        ToolOutputBody::Text(text) => text.clone(),
        ToolOutputBody::Content(content) => content
            .iter()
            .filter_map(|item| match item {
                ToolOutputContent::InputText { text } => Some(text.as_str()),
                ToolOutputContent::InputImage { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn call_ids(calls: &[NestedToolCall]) -> Vec<&str> {
    calls.iter().map(|call| call.call_id.as_str()).collect()
}

fn test_tools(workspace: &std::path::Path) -> ToolRuntime {
    ToolRuntime::new(
        workspace,
        WebSearchConfig {
            endpoint: "http://127.0.0.1:1/v1/alpha/search".to_owned(),
            api_key: "test-key".to_owned(),
        },
    )
}

fn test_context(history: &[Value]) -> ToolContext<'_> {
    ToolContext {
        model: "test-model",
        session_id: "test-session",
        history,
    }
}

fn temporary_workspace(label: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "harness-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
