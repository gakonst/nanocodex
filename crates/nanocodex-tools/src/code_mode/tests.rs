use std::{path::PathBuf, time::Duration};

use eyre::{Result, eyre};
use nanocodex_core::ResponseItem;
use serde_json::Value;

use super::{
    CellUpdate, CodeModeExecution, LiveCell, NestedToolCall, nested_tool_yield_after, observe_cell,
    parse_exec_source,
};
use crate::{ToolContext, ToolOutputBody, ToolOutputContent, ToolRuntime, WebSearchConfig};

#[tokio::test]
async fn prewarms_embedded_quickjs_host() -> Result<()> {
    let workspace = temporary_workspace("prewarmed-quickjs-host")?;
    let runtime = super::CodeModeRuntime::new(workspace.clone());

    assert!(runtime.host.lock().await.host.is_some());

    runtime.control().terminate_all().await;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn execution_globals_do_not_leak_across_quickjs_contexts() -> Result<()> {
    let workspace = temporary_workspace("isolated-quickjs-contexts")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let context = test_context(&history);

    let source = r"
const previous = globalThis.__nanocodexContextGeneration;
globalThis.__nanocodexContextGeneration = (previous || 0) + 1;
text({ previous: previous ?? null, current: globalThis.__nanocodexContextGeneration });
";
    let first = tools.execute_code(source, context).await;
    let second = tools.execute_code(source, context).await;

    assert!(first.success);
    assert!(second.success);
    assert_eq!(emitted_text(&first)?, r#"{"previous":null,"current":1}"#);
    assert_eq!(emitted_text(&second)?, r#"{"previous":null,"current":1}"#);
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn execution_prototype_mutations_do_not_leak_across_quickjs_contexts() -> Result<()> {
    let workspace = temporary_workspace("isolated-quickjs-prototypes")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();

    let first = tools
        .execute_code(
            r#"
Object.prototype.__nanocodexPoisoned = "yes";
text(({}).__nanocodexPoisoned);
"#,
            test_context(&history),
        )
        .await;
    let second = tools
        .execute_code(
            r#"text(({}).__nanocodexPoisoned ?? "clean");"#,
            test_context(&history),
        )
        .await;

    assert!(first.success, "{}", execution_output(&first));
    assert!(second.success, "{}", execution_output(&second));
    assert_eq!(emitted_text(&first)?, "yes");
    assert_eq!(emitted_text(&second)?, "clean");
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn execution_local_bindings_do_not_leak_across_quickjs_calls() -> Result<()> {
    let workspace = temporary_workspace("scoped-quickjs-bindings")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let context = test_context(&history);

    let source = r"
const executionLocal = 1;
text(executionLocal);
";
    let first = tools.execute_code(source, context).await;
    let second = tools.execute_code(source, context).await;

    assert!(first.success, "{}", execution_output(&first));
    assert!(second.success, "{}", execution_output(&second));
    assert_eq!(emitted_text(&first)?, "1");
    assert_eq!(emitted_text(&second)?, "1");
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn embedded_quickjs_does_not_expose_node_or_host_callback_globals() -> Result<()> {
    let workspace = temporary_workspace("embedded-quickjs-globals")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r"
text({
  process: typeof process,
  require: typeof require,
  hostCallback: typeof __nanocodexTool,
});
",
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(
        emitted_text(&execution)?,
        r#"{"process":"undefined","require":"undefined","hostCallback":"undefined"}"#
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn multiple_yielded_cells_continue_and_complete_independently() -> Result<()> {
    let workspace = temporary_workspace("multiple-live-cells")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let first = tools
        .execute_code(
            r#"
await yield_control();
const result = await tools.exec_command({ cmd: "sleep 0.04; printf 'first done'", login: false });
text(result.output);
"#,
            test_context_with_call(&history, "call-first"),
        )
        .await;
    let second = tools
        .execute_code(
            r#"
await yield_control();
const result = await tools.exec_command({ cmd: "sleep 0.01; printf 'second done'", login: false });
text(result.output);
"#,
            test_context_with_call(&history, "call-second"),
        )
        .await;

    assert!(execution_output(&first).contains("Script running with cell ID 1"));
    assert!(execution_output(&second).contains("Script running with cell ID 2"));

    let second = tools
        .wait_for_code(
            r#"{"cell_id":"2","yield_time_ms":1000}"#,
            test_context_with_call(&history, "call-wait-second"),
        )
        .await;
    let first = tools
        .wait_for_code(
            r#"{"cell_id":"1","yield_time_ms":1000}"#,
            test_context_with_call(&history, "call-wait-first"),
        )
        .await;

    assert!(second.success, "{}", execution_output(&second));
    assert!(execution_output(&second).contains("second done"));
    assert_eq!(second.nested_calls.len(), 1);
    assert!(first.success, "{}", execution_output(&first));
    assert!(execution_output(&first).contains("first done"));
    assert_eq!(first.nested_calls.len(), 1);
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
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
    assert_eq!(
        call_ids(&execution.nested_calls),
        ["call-exec/code-1", "call-exec/code-2"]
    );
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
    let remote_output = execution_output(&remote);
    assert!(remote_output.contains(
        "Script error:\nTool call failed: remote image URLs are not supported in tool outputs. Pass a base64 data URI instead"
    ));
    assert!(!remote_output.contains("at image"));

    let invalid = tools
        .execute_code(r#"image("not-an-image");"#, test_context(&history))
        .await;
    assert!(!invalid.success);
    let invalid_output = execution_output(&invalid);
    assert!(invalid_output.contains(
        "Script error:\nTool call failed: invalid image output. Pass a base64 data URI instead"
    ));
    assert!(!invalid_output.contains("at image"));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn failed_cell_preserves_accumulated_output() -> Result<()> {
    let workspace = temporary_workspace("failed-cell-output")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
text("before crash");
image("data:image/png;base64,a", "original");
throw new Error("boom");
"#,
            test_context(&history),
        )
        .await;

    assert!(!execution.success);
    let ToolOutputBody::Content(content) = &execution.output else {
        return Err(eyre!("code-mode execution did not emit content"));
    };
    assert!(matches!(
        content.get(1),
        Some(ToolOutputContent::InputText { text }) if text == "before crash"
    ));
    assert!(matches!(
        content.get(2),
        Some(ToolOutputContent::InputImage {
            image_url,
            detail: crate::ImageDetail::Original,
        }) if image_url == "data:image/png;base64,a"
    ));
    assert!(matches!(
        content.get(3),
        Some(ToolOutputContent::InputText { text })
            if text.starts_with("Script error:\nError: boom\n")
    ));

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
            detail: crate::ImageDetail::Original,
        }) if image_url == "data:image/png;base64,a"
    ));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn generated_image_helper_appends_high_detail_image_and_hint() -> Result<()> {
    let workspace = temporary_workspace("code-mode-generated-image")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
generatedImage({
  image_url: "data:image/png;base64,a",
  output_hint: "generated image save hint",
});
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let ToolOutputBody::Content(content) = &execution.output else {
        return Err(eyre!("code-mode execution did not emit content"));
    };
    assert!(matches!(
        content.get(1),
        Some(ToolOutputContent::InputImage {
            image_url,
            detail: crate::ImageDetail::High,
        }) if image_url == "data:image/png;base64,a"
    ));
    assert!(matches!(
        content.get(2),
        Some(ToolOutputContent::InputText { text }) if text == "generated image save hint"
    ));

    let invalid = tools
        .execute_code(
            r#"generatedImage({ image_url: "data:image/png;base64,a", output_hint: 1 });"#,
            test_context(&history),
        )
        .await;
    assert!(!invalid.success);
    assert!(
        execution_output(&invalid)
            .contains("generatedImage output_hint must be a string when provided")
    );

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn notify_serializes_values_and_rejects_empty_text() -> Result<()> {
    let workspace = temporary_workspace("code-mode-notify")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"notify({ phase: "working" }); text("done");"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(execution.notifications.len(), 1);
    assert_eq!(execution.notifications[0].call_id, "call-exec");
    assert_eq!(execution.notifications[0].text, r#"{"phase":"working"}"#);

    let empty = tools
        .execute_code(r#"notify("  ");"#, test_context(&history))
        .await;
    assert!(!empty.success);
    assert!(execution_output(&empty).contains("Script error:\nnotify expects non-empty text"));
    assert!(!execution_output(&empty).contains("at notify"));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn store_normalizes_json_values_and_coerces_keys() -> Result<()> {
    let workspace = temporary_workspace("code-mode-store-json")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let write = tools
        .execute_code(
            r"
const value = { kept: 1, dropped: undefined, array: [undefined, NaN] };
store(42, value);
value.kept = 99;
",
            test_context(&history),
        )
        .await;
    assert!(write.success, "{}", execution_output(&write));

    let read = tools
        .execute_code(
            r"text(load(42));",
            test_context_with_call(&history, "call-read"),
        )
        .await;
    assert!(read.success, "{}", execution_output(&read));
    assert_eq!(
        serde_json::from_str::<Value>(emitted_text(&read)?)?,
        serde_json::json!({ "kept": 1, "array": [null, null] })
    );

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn store_rejects_non_serializable_values_at_the_call_boundary() -> Result<()> {
    let workspace = temporary_workspace("code-mode-store-errors")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(r#"store("candidate", undefined);"#, test_context(&history))
        .await;

    assert!(!execution.success);
    let output = execution_output(&execution);
    assert!(output.contains(
        "Script error:\nUnable to store \"candidate\". Only plain serializable objects can be stored."
    ));
    assert!(!output.contains("at store"));

    let read = tools
        .execute_code(
            r#"text(load("candidate"));"#,
            test_context_with_call(&history, "call-read"),
        )
        .await;
    assert!(read.success, "{}", execution_output(&read));
    assert_eq!(emitted_text(&read)?, "undefined");

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
async fn running_shell_session_survives_output_only_javascript() -> Result<()> {
    let workspace = temporary_workspace("running-shell-session-output")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
const result = await tools.exec_command({ cmd: "sleep 5", yield_time_ms: 250 });
text(result.output);
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert!(
        execution_output(&execution)
            .contains("Nested shell process is still running with session ID 1")
    );
    tools.control().cancel().await;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn running_shell_session_notice_is_not_duplicated_for_full_results() -> Result<()> {
    let workspace = temporary_workspace("visible-running-shell-session")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
const result = await tools.exec_command({ cmd: "sleep 5", yield_time_ms: 250 });
text(result);
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let output = execution_output(&execution);
    assert!(output.contains(r#""session_id":1"#));
    assert!(!output.contains("Nested shell process is still running"));
    tools.control().cancel().await;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cancellation_terminates_yielded_code_cells() -> Result<()> {
    let workspace = temporary_workspace("cancelled-cell")?;
    let tools = test_tools(&workspace);
    let control = tools.control();
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r"
await yield_control();
await new Promise(() => {});
",
            test_context(&history),
        )
        .await;
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));

    tokio::time::timeout(std::time::Duration::from_secs(2), control.cancel()).await?;
    let missing = tools
        .wait_for_code(r#"{"cell_id":"1"}"#, test_context(&history))
        .await;
    assert!(!missing.success);
    assert!(execution_output(&missing).contains("exec cell 1 was not found"));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cancellation_interrupts_busy_javascript_and_recreates_the_host() -> Result<()> {
    let workspace = temporary_workspace("cancelled-busy-cell")?;
    let tools = test_tools(&workspace);
    let control = tools.control();
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r"
await yield_control();
while (true) {}
",
            test_context(&history),
        )
        .await;
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));

    tokio::time::timeout(std::time::Duration::from_secs(2), control.cancel()).await?;
    let recovered = tools
        .execute_code(r#"text("recovered")"#, test_context(&history))
        .await;

    assert!(recovered.success, "{}", execution_output(&recovered));
    assert_eq!(emitted_text(&recovered)?, "recovered");
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_drops_pending_tool_promises_before_recreating_the_host() -> Result<()> {
    let workspace = temporary_workspace("cancelled-pending-tool")?;
    let tools = test_tools(&workspace);
    let control = tools.control();
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"// @exec: {"yield_time_ms": 10}
await tools.exec_command({ cmd: "sleep 5", login: false });
"#,
            test_context(&history),
        )
        .await;
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));

    tokio::time::timeout(std::time::Duration::from_secs(2), control.cancel()).await?;
    let recovered = tools
        .execute_code(r#"text("recovered")"#, test_context(&history))
        .await;

    assert!(recovered.success, "{}", execution_output(&recovered));
    assert_eq!(emitted_text(&recovered)?, "recovered");
    tools.control().cancel().await;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn resumed_cell_notifications_keep_the_original_exec_call_id() -> Result<()> {
    let workspace = temporary_workspace("resumed-cell-notify")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
await yield_control();
notify("after yield");
text("done");
"#,
            test_context_with_call(&history, "call-original-exec"),
        )
        .await;

    assert!(execution.success);
    assert!(execution.notifications.is_empty());

    let completed = tools
        .wait_for_code(
            r#"{"cell_id":"1","yield_time_ms":1000}"#,
            test_context_with_call(&history, "call-wait"),
        )
        .await;
    assert!(completed.success, "{}", execution_output(&completed));
    assert_eq!(completed.notifications.len(), 1);
    assert_eq!(completed.notifications[0].call_id, "call-original-exec");
    assert_eq!(completed.notifications[0].text, "after yield");

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
fn nested_shell_yields_follow_the_handlers_bounds() {
    assert_eq!(
        nested_tool_yield_after(
            "exec_command",
            &serde_json::json!({ "yield_time_ms": 45_000 }),
        ),
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        nested_tool_yield_after(
            "write_stdin",
            &serde_json::json!({ "session_id": 1, "yield_time_ms": 120_000 }),
        ),
        Some(Duration::from_secs(120))
    );
    assert_eq!(
        nested_tool_yield_after(
            "write_stdin",
            &serde_json::json!({
                "session_id": 1,
                "chars": "x",
                "yield_time_ms": 120_000,
            }),
        ),
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        nested_tool_yield_after(
            "apply_patch",
            &serde_json::json!({ "yield_time_ms": 30_000 })
        ),
        None
    );
}

#[tokio::test]
async fn default_cell_yield_extends_for_a_longer_nested_shell_wait() {
    let (updates_tx, updates) = tokio::sync::mpsc::unbounded_channel();
    let (terminate, _terminate_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        updates_tx
            .send(CellUpdate::NestedCallStarted {
                name: "write_stdin".to_owned(),
                yield_after: Duration::from_millis(40),
            })
            .expect("observer should receive the nested call");
        tokio::time::sleep(Duration::from_millis(15)).await;
        updates_tx
            .send(CellUpdate::Completed {
                content: Vec::new(),
            })
            .expect("observer should receive cell completion");
    });
    let mut cell = LiveCell {
        id: 1,
        output_token_budget: crate::DEFAULT_TOOL_OUTPUT_TOKENS,
        updates,
        terminate: Some(terminate),
        task: Some(task),
    };

    let (execution, running) = observe_cell(
        &mut cell,
        std::time::Instant::now(),
        Duration::from_millis(5),
        None,
        true,
    )
    .await;

    assert!(!running);
    assert!(execution.success);
    assert!(execution_output(&execution).contains("Script completed"));
    cell.join().await;
}

#[tokio::test]
async fn explicit_cell_yield_is_not_extended_by_a_nested_shell_wait() {
    let (updates_tx, updates) = tokio::sync::mpsc::unbounded_channel();
    let (terminate, _terminate_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        updates_tx
            .send(CellUpdate::NestedCallStarted {
                name: "write_stdin".to_owned(),
                yield_after: Duration::from_millis(40),
            })
            .expect("observer should receive the nested call");
        tokio::time::sleep(Duration::from_millis(15)).await;
        let _ = updates_tx.send(CellUpdate::Completed {
            content: Vec::new(),
        });
    });
    let mut cell = LiveCell {
        id: 1,
        output_token_budget: crate::DEFAULT_TOOL_OUTPUT_TOKENS,
        updates,
        terminate: Some(terminate),
        task: Some(task),
    };

    let (execution, running) = observe_cell(
        &mut cell,
        std::time::Instant::now(),
        Duration::from_millis(5),
        None,
        false,
    )
    .await;

    assert!(running);
    assert!(execution.success);
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));
    cell.join().await;
}

#[test]
fn model_description_uses_codex_style_declarations() {
    let workspace = temporary_workspace("code-mode-description")
        .expect("temporary test workspace should be available");
    let tools = test_tools(&workspace);
    let specs = tools
        .model_specs()
        .into_iter()
        .map(|spec| serde_json::to_value(spec).unwrap())
        .collect::<Vec<_>>();
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
        Some(WebSearchConfig {
            endpoint: "http://127.0.0.1:1/v1/alpha/search".to_owned(),
            auth: nanocodex_core::OpenAiAuth::api_key("test-key"),
        }),
        Some(super::super::ImageGenerationConfig {
            api_base_url: "http://127.0.0.1:1/v1".to_owned(),
            auth: nanocodex_core::OpenAiAuth::api_key("test-key"),
            save_root: workspace.to_path_buf(),
        }),
    )
}

fn test_context(history: &[ResponseItem]) -> ToolContext<'_> {
    test_context_with_call(history, "call-exec")
}

fn test_context_with_call<'a>(history: &'a [ResponseItem], call_id: &'a str) -> ToolContext<'a> {
    ToolContext {
        model: "test-model",
        session_id: "test-session",
        call_id,
        history,
        output_token_budget: crate::DEFAULT_TOOL_OUTPUT_TOKENS,
    }
}

fn temporary_workspace(label: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "nanocodex-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
