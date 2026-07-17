use std::path::PathBuf;

use eyre::{Result, eyre};
use serde_json::Value;

use super::{CodeModeExecution, NestedToolCall};
use crate::tools::{ToolOutputBody, ToolOutputContent, ToolRuntime};

#[tokio::test]
async fn reuses_one_node_host_between_cells() -> Result<()> {
    let workspace = temporary_workspace("persistent-node-host")?;
    let tools = ToolRuntime::new(&workspace);

    let first = tools.execute_code("text(process.pid)").await;
    let second = tools.execute_code("text(process.pid)").await;

    assert!(first.success);
    assert!(second.success);
    assert_eq!(emitted_text(&first)?, emitted_text(&second)?);
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn promise_all_runs_nested_tools_concurrently() -> Result<()> {
    let workspace = temporary_workspace("parallel-nested-tools")?;
    let tools = ToolRuntime::new(&workspace);
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
async fn yielded_cell_completes_through_wait() -> Result<()> {
    let workspace = temporary_workspace("yielded-cell")?;
    let tools = ToolRuntime::new(&workspace);
    let execution = tools
        .execute_code(
            r#"
text("before");
await yield_control();
await new Promise((resolve) => setTimeout(resolve, 10));
text("after");
"#,
        )
        .await;

    assert!(execution.success);
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));
    assert!(execution_output(&execution).contains("before"));

    let completed = tools
        .wait_for_code(r#"{"cell_id":"1","yield_time_ms":1000}"#)
        .await;
    assert!(completed.success);
    assert!(execution_output(&completed).contains("Script completed"));
    assert!(execution_output(&completed).contains("after"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
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
