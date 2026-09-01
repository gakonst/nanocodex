use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use eyre::{Result, eyre};
use nanocodex_oai_api::{responses::ResponseItem, tools::ToolDefinition};
use serde_json::Value;
use tokio::sync::Semaphore;

use super::{
    CellError, CellLifecycle, CellObservationState, CellUpdate, CodeModeExecution,
    CodeModeObserver, CodeModeUpdate, LiveCell, NestedToolCall, ObservationBuffer, ObservationMode,
    observe_cell, observer_yield_timeout, parse_exec_source,
};
use crate::{
    Tool, ToolContext, ToolInput, ToolOutput, ToolOutputBody, ToolOutputContent, ToolResult, Tools,
    runtime::{ToolRuntime, WebSearchConfig},
};

struct ConcurrencyProbe {
    state: Arc<ConcurrencyProbeState>,
}

struct SerialConcurrencyProbe {
    state: Arc<ConcurrencyProbeState>,
}

struct ConcurrencyProbeState {
    active: AtomicUsize,
    maximum: AtomicUsize,
    release: Semaphore,
}

#[async_trait::async_trait]
impl Tool for ConcurrencyProbe {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "concurrency_probe",
            "Waits until released by the test.",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let active = self.state.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.maximum.fetch_max(active, Ordering::SeqCst);
        let permit = self
            .state
            .release
            .acquire()
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        permit.forget();
        self.state.active.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::text("released"))
    }
}

#[async_trait::async_trait]
impl Tool for SerialConcurrencyProbe {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "serial_concurrency_probe",
            "Records whether default tool execution overlaps.",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let active = self.state.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.maximum.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(25)).await;
        self.state.active.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::text("completed"))
    }
}

#[tokio::test]
async fn nested_tool_calls_are_serial_by_default() -> Result<()> {
    let workspace = temporary_workspace("serial-nested-tools")?;
    let state = Arc::new(ConcurrencyProbeState {
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
        release: Semaphore::new(0),
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(SerialConcurrencyProbe {
            state: Arc::clone(&state),
        })
        .build()?;
    let runtime = ToolRuntime::new_with_tools(&workspace, None, None, &tools);
    let history = Vec::new();
    let execution = runtime
        .execute_code(
            r"
await Promise.all([
  tools.serial_concurrency_probe({}),
  tools.serial_concurrency_probe({}),
]);
",
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(state.maximum.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn nested_tool_calls_are_bounded_at_128() -> Result<()> {
    const CALLS: usize = super::MAX_CONCURRENT_NESTED_CALLS + 1;

    let workspace = temporary_workspace("bounded-nested-tools")?;
    let state = Arc::new(ConcurrencyProbeState {
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
        release: Semaphore::new(0),
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(ConcurrencyProbe {
            state: Arc::clone(&state),
        })
        .build()?;
    let runtime = ToolRuntime::new_with_tools(&workspace, None, None, &tools);
    let execution = tokio::spawn(async move {
        let history = Vec::new();
        runtime
            .execute_code(
                &format!(
                    "await Promise.all(Array.from({{ length: {CALLS} }}, () => \
                     tools.concurrency_probe({{}})));"
                ),
                test_context(&history),
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        while state.active.load(Ordering::SeqCst) < super::MAX_CONCURRENT_NESTED_CALLS {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(
        state.active.load(Ordering::SeqCst),
        super::MAX_CONCURRENT_NESTED_CALLS
    );
    assert_eq!(
        state.maximum.load(Ordering::SeqCst),
        super::MAX_CONCURRENT_NESTED_CALLS
    );

    state.release.add_permits(CALLS);
    let execution = execution.await?;
    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(execution.nested_calls.len(), CALLS);
    assert_eq!(
        state.maximum.load(Ordering::SeqCst),
        super::MAX_CONCURRENT_NESTED_CALLS
    );

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[test]
fn long_observer_yields_include_completion_grace() {
    assert_eq!(
        observer_yield_timeout(Duration::from_secs(10)),
        Duration::from_secs(11)
    );
    assert_eq!(
        observer_yield_timeout(Duration::from_millis(9_999)),
        Duration::from_millis(9_999)
    );
}

#[tokio::test]
async fn prewarms_embedded_quickjs_host() -> Result<()> {
    let workspace = temporary_workspace("prewarmed-quickjs-host")?;
    let runtime = super::CodeModeRuntime::new(workspace.clone());

    assert!(runtime.host.lock().await.host.is_some());

    drop(runtime.control().terminate_all().await);
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
            r#"
text({
  process: typeof process,
  require: typeof require,
  hostCallback: typeof __nanocodexTool,
  console: Object.hasOwn(globalThis, "console"),
  atomics: Object.hasOwn(globalThis, "Atomics"),
  sharedArrayBuffer: Object.hasOwn(globalThis, "SharedArrayBuffer"),
  webAssembly: Object.hasOwn(globalThis, "WebAssembly"),
});
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(
        emitted_text(&execution)?,
        r#"{"process":"undefined","require":"undefined","hostCallback":"undefined","console":false,"atomics":false,"sharedArrayBuffer":false,"webAssembly":false}"#
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn all_tools_matches_codex_metadata_shape() -> Result<()> {
    let workspace = temporary_workspace("all-tools-metadata")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
text(ALL_TOOLS.map((tool) => ({
  keys: Object.keys(tool).sort(),
  frozen: Object.isFrozen(tool),
  hasInputSchema: typeof tool.input_schema === "object",
  hasOutputSchema: typeof tool.output_schema === "object",
  serializedSchema: JSON.stringify(tool).includes("input_schema"),
})));
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let metadata = serde_json::from_str::<Value>(emitted_text(&execution)?)?;
    let tools = metadata
        .as_array()
        .ok_or_else(|| eyre!("ALL_TOOLS output was not an array"))?;
    assert!(!tools.is_empty());
    assert!(tools.iter().all(|tool| {
        tool["keys"] == serde_json::json!(["description", "name"])
            && tool["frozen"] == true
            && tool["hasInputSchema"] == false
            && tool["hasOutputSchema"] == false
            && tool["serializedSchema"] == false
    }));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn function_tools_receive_an_empty_object_when_called_without_arguments() -> Result<()> {
    let workspace = temporary_workspace("empty-function-arguments")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r"
try {
  await tools.update_plan();
} catch (_) {}
",
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(execution.nested_calls.len(), 1);
    assert_eq!(execution.nested_calls[0].input, serde_json::json!({}));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn text_propagates_json_stringification_errors() -> Result<()> {
    let workspace = temporary_workspace("text-stringification-error")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r"
const value = {};
value.self = value;
text(value);
",
            test_context(&history),
        )
        .await;

    assert!(!execution.success);
    assert!(execution_output(&execution).contains("Script error:"));
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
            r#"{"cell_id":"2","yield_time_ms":5000}"#,
            test_context_with_call(&history, "call-wait-second"),
        )
        .await;
    let first = tools
        .wait_for_code(
            r#"{"cell_id":"1","yield_time_ms":5000}"#,
            test_context_with_call(&history, "call-wait-first"),
        )
        .await;

    assert!(second.success, "{}", execution_output(&second));
    assert!(
        execution_output(&second).contains("second done"),
        "{}",
        execution_output(&second)
    );
    assert_eq!(second.nested_calls.len(), 1);
    assert!(first.success, "{}", execution_output(&first));
    assert!(
        execution_output(&first).contains("first done"),
        "{}",
        execution_output(&first)
    );
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

#[cfg(unix)]
#[tokio::test]
async fn nested_call_updates_stream_in_start_and_resolution_order() -> Result<()> {
    #[derive(Default)]
    struct Timeline(Vec<String>);

    impl CodeModeObserver for Timeline {
        fn update(&mut self, update: CodeModeUpdate<'_>) {
            match update {
                CodeModeUpdate::NestedCallStarted { call_id, .. } => {
                    self.0.push(format!("start:{call_id}"));
                }
                CodeModeUpdate::NestedCallCompleted(call) => {
                    self.0.push(format!("done:{}", call.call_id));
                }
            }
        }
    }

    let workspace = temporary_workspace("streaming-nested-tools")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let mut timeline = Timeline::default();
    let execution = tools
        .execute_code_with_updates(
            r#"
await Promise.all([
  tools.exec_command({
    cmd: "i=0; while [ \"$i\" -lt 500 ]; do [ -f second.done ] && exit 0; i=$((i + 1)); sleep 0.01; done; exit 91",
    login: false,
  }),
  tools.exec_command({ cmd: "touch second.done", login: false }),
]);
"#,
            test_context(&history),
            &mut timeline,
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(
        timeline.0,
        [
            "start:call-exec/code-1",
            "start:call-exec/code-2",
            "done:call-exec/code-2",
            "done:call-exec/code-1",
        ]
    );
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

#[cfg(unix)]
#[tokio::test]
async fn shell_runtime_failures_reject_but_nonzero_exits_resolve() -> Result<()> {
    let workspace = temporary_workspace("shell-runtime-failures")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let missing_workdir = workspace.join("missing");
    let execution = tools
        .execute_code(
            &format!(
                r#"
const observed = [];
try {{
  await tools.exec_command({{
    cmd: "true",
    workdir: {},
    login: false,
  }});
  observed.push("spawn-resolved");
}} catch (error) {{
  observed.push(String(error).startsWith("exec_command failed for `true`: CreateProcess"));
}}
try {{
  await tools.write_stdin({{ session_id: 999999, chars: "" }});
  observed.push("unknown-resolved");
}} catch (error) {{
  observed.push(String(error) === "write_stdin failed: Unknown process id 999999");
}}
const nonzero = await tools.exec_command({{ cmd: "exit 17", login: false }});
observed.push(nonzero.exit_code);
text(observed);
"#,
                serde_json::to_string(&missing_workdir.to_string_lossy())?
            ),
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(
        serde_json::from_str::<Value>(emitted_text(&execution)?)?,
        serde_json::json!([true, true, 17])
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn non_tty_stdin_is_closed_but_interrupt_is_supported() -> Result<()> {
    let workspace = temporary_workspace("non-tty-stdin")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
const command = await tools.exec_command({
  cmd: "sleep 30",
  login: false,
  tty: false,
  yield_time_ms: 250,
});
let closed = false;
try {
  await tools.write_stdin({
    session_id: command.session_id,
    chars: "hello\n",
  });
} catch (error) {
  closed = String(error) === "write_stdin failed: stdin is closed for this session; rerun exec_command with tty=true to keep stdin open";
}
const interrupted = await tools.write_stdin({
  session_id: command.session_id,
  chars: "\u0003",
  yield_time_ms: 1000,
});
text({ closed, exit_code: interrupted.exit_code });
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(
        serde_json::from_str::<Value>(emitted_text(&execution)?)?,
        serde_json::json!({ "closed": true, "exit_code": 130 })
    );
    assert_eq!(execution.nested_calls[0].structured_result["session_id"], 1);
    assert_eq!(
        execution.nested_calls[2].structured_result["exit_code"],
        130
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn shell_results_always_report_original_token_count() -> Result<()> {
    let workspace = temporary_workspace("shell-original-token-count")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
const result = await tools.exec_command({ cmd: "printf hi", login: false });
text({
  present: Object.hasOwn(result, "original_token_count"),
  count: result.original_token_count,
});
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let result = serde_json::from_str::<Value>(emitted_text(&execution)?)?;
    assert_eq!(result["present"], true);
    assert_eq!(result["count"], 1);
    assert_eq!(
        execution.nested_calls[0].structured_result["original_token_count"],
        1
    );
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
async fn view_image_rejects_unsupported_detail_with_codex_diagnostics() -> Result<()> {
    let workspace = temporary_workspace("view-image-detail-error")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
try {
  await tools.view_image({ path: "missing.png", detail: "low" });
} catch (error) {
  text(error);
}
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(
        emitted_text(&execution)?,
        "view_image.detail only supports `high` or `original`; omit `detail` for default high resized behavior, got `low`"
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn output_helpers_accept_raw_mcp_image_and_audio_blocks() -> Result<()> {
    let workspace = temporary_workspace("code-mode-mcp-media")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
const returnsUndefined = [
  image({
    type: "image",
    data: "a",
    mimeType: "image/png",
    _meta: { "codex/imageDetail": "original" },
  }),
  audio({
    type: "audio",
    data: "UklGRiQAAABXQVZFZm10IBAAAAABAAEAQB8AAIA+AAACABAAZGF0YQAAAAA=",
    mimeType: "audio/wav",
  }),
].map((value) => value === undefined);
text(returnsUndefined);
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
            detail: crate::ImageDetail::Original,
        }) if image_url == "data:image/png;base64,a"
    ));
    assert!(matches!(
        content.get(2),
        Some(ToolOutputContent::InputAudio { audio_url })
            if audio_url == "data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEAQB8AAIA+AAACABAAZGF0YQAAAAA="
    ));
    assert_eq!(emitted_text(&execution)?, "[true,true]");
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
            r#"{"cell_id":"1","yield_time_ms":5000}"#,
            test_context(&history),
        )
        .await;
    assert!(completed.success);
    assert!(execution_output(&completed).contains("Script completed"));
    assert!(execution_output(&completed).contains("after"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn yield_control_does_not_require_pending_tools_to_finish() -> Result<()> {
    let workspace = temporary_workspace("yield-with-pending-tool")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
const pending = tools.exec_command({
  cmd: "sleep 0.05; printf 'finished'",
  login: false,
});
text("before");
yield_control();
const result = await pending;
text(result.output);
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));
    assert!(execution_output(&execution).contains("before"));
    let completed = tools
        .wait_for_code(
            r#"{"cell_id":"1","yield_time_ms":5000}"#,
            test_context(&history),
        )
        .await;
    assert!(completed.success, "{}", execution_output(&completed));
    assert!(execution_output(&completed).contains("finished"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn pending_timeouts_do_not_keep_a_cell_alive() -> Result<()> {
    let workspace = temporary_workspace("unawaited-timeout")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let completed = tokio::time::timeout(
        Duration::from_secs(2),
        tools.execute_code(
            r#"
setTimeout(() => text("late"), 60_000);
text("done");
"#,
            test_context(&history),
        ),
    )
    .await?;

    assert!(completed.success, "{}", execution_output(&completed));
    assert_eq!(emitted_text(&completed)?, "done");
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn pending_timeouts_do_not_spawn_one_thread_each() -> Result<()> {
    let workspace = temporary_workspace("bounded-timeout-scheduler")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let threads_before = std::fs::read_dir("/proc/self/task")?.count();
    let yielded = tools
        .execute_code(
            r#"
for (let timer = 0; timer < 64; timer += 1) {
  setTimeout(() => {}, 60_000);
}
yield_control();
await new Promise(() => {});
"#,
            test_context(&history),
        )
        .await;
    assert!(yielded.success, "{}", execution_output(&yielded));
    let threads_with_timers = std::fs::read_dir("/proc/self/task")?.count();
    let terminated = tools
        .wait_for_code(
            r#"{"cell_id":"1","terminate":true}"#,
            test_context(&history),
        )
        .await;
    std::fs::remove_dir_all(workspace)?;

    assert!(terminated.success, "{}", execution_output(&terminated));
    assert!(
        threads_with_timers <= threads_before + 8,
        "64 pending timers created {} additional OS threads",
        threads_with_timers.saturating_sub(threads_before)
    );
    Ok(())
}

#[tokio::test]
async fn termination_returns_unobserved_output_since_the_last_yield() -> Result<()> {
    let workspace = temporary_workspace("terminated-cell-output")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let after_ready = workspace.join("after-ready");
    let yielded = tools
        .execute_code(
            r#"
text("before");
yield_control();
text("after");
await tools.exec_command({cmd: "touch after-ready", login: false});
await new Promise(() => {});
"#,
            test_context(&history),
        )
        .await;

    assert!(yielded.success, "{}", execution_output(&yielded));
    assert!(execution_output(&yielded).contains("before"));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !after_ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let terminated = tools
        .wait_for_code(
            r#"{"cell_id":"1","terminate":true}"#,
            test_context(&history),
        )
        .await;
    assert!(terminated.success, "{}", execution_output(&terminated));
    assert!(execution_output(&terminated).contains("Script terminated"));
    assert!(execution_output(&terminated).contains("after"));
    assert!(!execution_output(&terminated).contains("was terminated"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn natural_completion_wins_over_a_late_termination_request() -> Result<()> {
    let workspace = temporary_workspace("completion-before-termination")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let yielded = tools
        .execute_code(
            r#"
yield_control();
text("done");
"#,
            test_context(&history),
        )
        .await;
    assert!(yielded.success, "{}", execution_output(&yielded));

    tokio::time::sleep(Duration::from_millis(20)).await;
    let completed = tools
        .wait_for_code(
            r#"{"cell_id":"1","terminate":true}"#,
            test_context(&history),
        )
        .await;
    assert!(completed.success, "{}", execution_output(&completed));
    assert!(execution_output(&completed).contains("Script completed"));
    assert!(execution_output(&completed).contains("done"));
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
async fn completed_shell_session_is_not_reported_as_running() -> Result<()> {
    let workspace = temporary_workspace("completed-shell-session")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
const command = await tools.exec_command({
  cmd: "read value; printf 'received:%s' \"$value\"",
  login: false,
  tty: true,
  yield_time_ms: 250,
});
const completed = await tools.write_stdin({
  session_id: command.session_id,
  chars: "parity\n",
  yield_time_ms: 5000,
});
text(completed.output);
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let output = execution_output(&execution);
    assert!(output.contains("received:parity"));
    assert!(!output.contains("Nested shell process is still running"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_a_turn_stops_its_cell_but_preserves_its_shell_until_shutdown() -> Result<()> {
    let workspace = temporary_workspace("runtime-owned-shell-cancellation")?;
    let tools = test_tools(&workspace);
    let control = tools.control();
    let history = Vec::new();

    control.begin_turn();
    let started = tools
        .execute_code(
            r#"
const command = await tools.exec_command({
  cmd: "sleep 30",
  login: false,
  tty: true,
  yield_time_ms: 250,
});
text(command.session_id);
await yield_control();
await new Promise(() => {});
"#,
            test_context(&history),
        )
        .await;
    assert!(started.success, "{}", execution_output(&started));
    let started = execution_output(&started);
    assert!(
        started.contains("Script running with cell ID 1"),
        "{started}"
    );
    assert_eq!(started.lines().last(), Some("1"), "{started}");

    control.cancel_turn().await;
    let missing_cell = tools
        .wait_for_code(r#"{"cell_id":"1"}"#, test_context(&history))
        .await;
    assert!(!missing_cell.success);
    assert!(
        execution_output(&missing_cell).contains("exec cell 1 not found"),
        "{}",
        execution_output(&missing_cell)
    );

    let resumed = tools
        .execute_tool(
            "write_stdin",
            ToolInput::Function(serde_json::value::to_raw_value(&serde_json::json!({
                "session_id": 1,
                "chars": "\n",
                "yield_time_ms": 250
            }))?),
            test_context(&history),
        )
        .await;
    assert!(resumed.success);
    let ToolOutputBody::Text(resumed) = resumed.output else {
        return Err(eyre!("write_stdin returned non-text output"));
    };
    assert!(
        resumed.contains("Process running with session ID 1"),
        "{resumed}"
    );

    control.cancel().await;
    let missing = tools
        .execute_tool(
            "write_stdin",
            ToolInput::Function(serde_json::value::to_raw_value(&serde_json::json!({
                "session_id": 1,
                "chars": ""
            }))?),
            test_context(&history),
        )
        .await;
    assert!(!missing.success);
    let ToolOutputBody::Text(missing) = missing.output else {
        return Err(eyre!("missing-session failure returned non-text output"));
    };
    assert!(missing.contains("Unknown process id 1"), "{missing}");

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_during_initial_shell_observation_preserves_the_registered_session() -> Result<()>
{
    let workspace = temporary_workspace("cancelled-initial-shell-observation")?;
    let source = r#"
const command = await tools.exec_command({
  cmd: "sleep 30",
  login: false,
  tty: true,
  yield_time_ms: 30000,
});
text(command.session_id);
"#;
    let tools = Arc::new(test_tools(&workspace));
    let control = tools.control();

    control.begin_turn();
    let execution = {
        let tools = Arc::clone(&tools);
        tokio::spawn(async move {
            let history = Vec::new();
            tools.execute_code(source, test_context(&history)).await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while !tools.has_shell_session(1).await {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    tokio::time::timeout(Duration::from_secs(2), control.cancel_turn()).await?;
    let cancelled = tokio::time::timeout(Duration::from_secs(2), execution).await??;
    assert!(cancelled.success, "{}", execution_output(&cancelled));
    assert!(
        execution_output(&cancelled).contains("Script terminated"),
        "{}",
        execution_output(&cancelled)
    );

    let history = Vec::new();
    let resumed = tools
        .execute_tool(
            "write_stdin",
            ToolInput::Function(serde_json::value::to_raw_value(&serde_json::json!({
                "session_id": 1,
                "chars": "\n",
                "yield_time_ms": 250
            }))?),
            test_context(&history),
        )
        .await;
    assert!(resumed.success);
    let ToolOutputBody::Text(resumed) = resumed.output else {
        return Err(eyre!("write_stdin returned non-text output"));
    };
    assert!(
        resumed.contains("Process running with session ID 1"),
        "{resumed}"
    );

    control.cancel().await;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn full_shutdown_quiesces_a_shell_producing_cell_before_draining_sessions() -> Result<()> {
    let workspace = temporary_workspace("shutdown-shell-producing-cell")?;
    let tools = Arc::new(test_tools(&workspace));
    let control = tools.control();
    let source = r#"
const command = await tools.exec_command({
  cmd: "sleep 30",
  login: false,
  tty: true,
  yield_time_ms: 30000,
});
text(command.session_id);
"#;

    control.begin_turn();
    let execution = {
        let tools = Arc::clone(&tools);
        tokio::spawn(async move {
            let history = Vec::new();
            tools.execute_code(source, test_context(&history)).await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while !tools.has_shell_session(1).await {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    tokio::time::timeout(Duration::from_secs(2), control.cancel()).await?;
    let terminated = tokio::time::timeout(Duration::from_secs(2), execution).await??;
    assert!(terminated.success, "{}", execution_output(&terminated));
    assert!(
        execution_output(&terminated).contains("Script terminated"),
        "{}",
        execution_output(&terminated)
    );

    let history = Vec::new();
    let missing = tools
        .execute_tool(
            "write_stdin",
            ToolInput::Function(serde_json::value::to_raw_value(&serde_json::json!({
                "session_id": 1,
                "chars": ""
            }))?),
            test_context(&history),
        )
        .await;
    assert!(!missing.success);
    let ToolOutputBody::Text(missing) = missing.output else {
        return Err(eyre!("missing-session failure returned non-text output"));
    };
    assert!(missing.contains("Unknown process id 1"), "{missing}");

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn full_shutdown_invalidates_code_cells_already_waiting_for_admission() -> Result<()> {
    let workspace = temporary_workspace("shutdown-pending-code-admission")?;
    let tools = Arc::new(test_tools(&workspace));
    let control = tools.control();
    let admission = tools.hold_code_mode_admission().await;

    let cancellation = {
        let control = control.clone();
        tokio::spawn(async move { control.cancel().await })
    };
    tools.wait_for_code_mode_admission_attempt().await;

    let execution = {
        let tools = Arc::clone(&tools);
        tokio::spawn(async move {
            let history = Vec::new();
            tools
                .execute_code(
                    r#"
const command = await tools.exec_command({
  cmd: "sleep 30",
  login: false,
  tty: true,
  yield_time_ms: 250,
});
text(command.session_id);
"#,
                    test_context(&history),
                )
                .await
        })
    };
    tools.wait_for_code_mode_admission_attempt().await;
    drop(admission);

    tokio::time::timeout(Duration::from_secs(2), cancellation).await??;
    let rejected = tokio::time::timeout(Duration::from_secs(2), execution).await??;
    let leaked = tools.has_shell_session(1).await;
    control.cancel().await;

    assert!(rejected.success, "{}", execution_output(&rejected));
    assert!(
        execution_output(&rejected).contains("Script terminated"),
        "{}",
        execution_output(&rejected)
    );
    assert!(
        !leaked,
        "a pre-cancellation Code Mode admission escaped shutdown"
    );

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn turn_cancellation_invalidates_code_cells_already_waiting_for_admission() -> Result<()> {
    let workspace = temporary_workspace("turn-cancel-pending-code-admission")?;
    let tools = Arc::new(test_tools(&workspace));
    let control = tools.control();
    control.begin_turn();
    let admission = tools.hold_code_mode_admission().await;

    let cancellation = {
        let control = control.clone();
        tokio::spawn(async move { control.cancel_turn().await })
    };
    tools.wait_for_code_mode_admission_attempt().await;

    let execution = {
        let tools = Arc::clone(&tools);
        tokio::spawn(async move {
            let history = Vec::new();
            tools
                .execute_code(r#"text("escaped");"#, test_context(&history))
                .await
        })
    };
    tools.wait_for_code_mode_admission_attempt().await;
    drop(admission);

    tokio::time::timeout(Duration::from_secs(2), cancellation).await??;
    let rejected = tokio::time::timeout(Duration::from_secs(2), execution).await??;
    control.cancel().await;

    assert!(rejected.success, "{}", execution_output(&rejected));
    assert!(
        execution_output(&rejected).contains("Script terminated"),
        "{}",
        execution_output(&rejected)
    );
    assert!(!execution_output(&rejected).contains("escaped"));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cancellation_terminates_a_cell_during_its_initial_observation() -> Result<()> {
    let workspace = temporary_workspace("cancelled-initial-observation")?;
    let state = Arc::new(ConcurrencyProbeState {
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
        release: Semaphore::new(0),
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(ConcurrencyProbe {
            state: Arc::clone(&state),
        })
        .build()?;
    let runtime = Arc::new(ToolRuntime::new_with_tools(&workspace, None, None, &tools));
    let control = runtime.control();
    let execution_runtime = Arc::clone(&runtime);
    let execution = tokio::spawn(async move {
        let history = Vec::new();
        execution_runtime
            .execute_code(
                r#"// @exec: {"yield_time_ms": 60000}
await tools.concurrency_probe({});
"#,
                test_context(&history),
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        while state.active.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    tokio::time::timeout(Duration::from_secs(2), control.cancel()).await?;
    state.release.add_permits(1);
    let terminated = tokio::time::timeout(Duration::from_secs(2), execution).await??;

    assert!(terminated.success, "{}", execution_output(&terminated));
    assert!(
        execution_output(&terminated).contains("Script terminated"),
        "{}",
        execution_output(&terminated)
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn active_wait_is_busy_and_a_dropped_observer_can_resume() -> Result<()> {
    struct ObservationSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl CodeModeObserver for ObservationSignal {
        fn update(&mut self, update: CodeModeUpdate<'_>) {
            if matches!(update, CodeModeUpdate::NestedCallStarted { .. })
                && let Some(signal) = self.0.take()
            {
                let _ = signal.send(());
            }
        }
    }

    let workspace = temporary_workspace("dropped-code-mode-observer")?;
    let state = Arc::new(ConcurrencyProbeState {
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
        release: Semaphore::new(0),
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(ConcurrencyProbe {
            state: Arc::clone(&state),
        })
        .build()?;
    let runtime = Arc::new(ToolRuntime::new_with_tools(&workspace, None, None, &tools));
    let history = Vec::new();
    let yielded = runtime
        .execute_code(
            r#"
await yield_control();
const result = await tools.concurrency_probe({});
text(result);
"#,
            test_context(&history),
        )
        .await;
    assert!(
        execution_output(&yielded).contains("Script running with cell ID 1"),
        "{}",
        execution_output(&yielded)
    );

    let waiting_runtime = Arc::clone(&runtime);
    let (observing_tx, observing_rx) = tokio::sync::oneshot::channel();
    let waiting = tokio::spawn(async move {
        let history = Vec::new();
        let mut observer = ObservationSignal(Some(observing_tx));
        waiting_runtime
            .wait_for_code_with_updates(
                r#"{"cell_id":"1","yield_time_ms":60000}"#,
                test_context(&history),
                &mut observer,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), observing_rx).await??;
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.active.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    let busy = runtime
        .wait_for_code(
            r#"{"cell_id":"1","yield_time_ms":0}"#,
            test_context(&history),
        )
        .await;
    assert!(!busy.success);
    assert!(
        execution_output(&busy).contains("exec cell 1 already has an active observer"),
        "{}",
        execution_output(&busy)
    );

    waiting.abort();
    let cancellation = match waiting.await {
        Ok(_) => panic!("aborted observer task completed"),
        Err(cancellation) => cancellation,
    };
    assert!(cancellation.is_cancelled());
    state.release.add_permits(1);
    let completed = runtime
        .wait_for_code(
            r#"{"cell_id":"1","yield_time_ms":5000}"#,
            test_context(&history),
        )
        .await;

    assert!(completed.success, "{}", execution_output(&completed));
    assert!(
        execution_output(&completed).contains("Script completed"),
        "{}",
        execution_output(&completed)
    );
    assert_eq!(completed.nested_calls.len(), 1);
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
const result = await tools.apply_patch("*** Begin Patch\n*** Add File: created.txt\n+created by patch\n*** End Patch");
text(result);
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(emitted_text(&execution)?, "{}");
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
async fn freeform_apply_patch_rejects_with_codex_diagnostics() -> Result<()> {
    let workspace = temporary_workspace("freeform-apply-patch-error")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
try {
  await tools.apply_patch("*** Begin Patch\n*** Update File: missing.txt\n@@\n-before\n+after\n*** End Patch");
} catch (error) {
  text(error);
}
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert!(
        emitted_text(&execution)?
            .starts_with("apply_patch verification failed: Failed to read file to update "),
        "{}",
        execution_output(&execution)
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn update_plan_matches_codex_handler_acceptance() -> Result<()> {
    let workspace = temporary_workspace("update-plan-acceptance")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
const result = await tools.update_plan({
  plan: [
    { step: "", status: "in_progress" },
    { step: "also active", status: "in_progress" },
  ],
});
text(result);
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(emitted_text(&execution)?, "{}");
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

#[tokio::test]
async fn negative_shell_limits_fail_during_codex_compatible_argument_parsing() -> Result<()> {
    let workspace = temporary_workspace("negative-shell-limits")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"
try {
  await tools.exec_command({ cmd: "true", yield_time_ms: -1 });
} catch (error) {
  text(error);
}
"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert!(
        emitted_text(&execution)?
            .starts_with("failed to parse function arguments: invalid value: integer `-1`"),
        "{}",
        execution_output(&execution)
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[test]
fn termination_claim_prevents_completion_and_store_commit() {
    let lifecycle = CellLifecycle::new();
    assert!(lifecycle.request_termination());

    let mut committed = false;
    if lifecycle.claim_completion() {
        committed = true;
    }

    assert!(!committed);
}

#[tokio::test]
async fn active_observation_reports_busy_without_consuming_the_cell() {
    let (_updates_tx, updates) = tokio::sync::mpsc::unbounded_channel();
    let (terminate, terminate_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = terminate_rx.await;
    });
    let cell = test_live_cell(1, updates, terminate, task);
    let observation = cell
        .begin_observation()
        .expect("first observer should acquire the cell");

    assert!(matches!(cell.begin_observation(), Err(CellError::Busy)));

    drop(observation);
    let resumed = cell
        .begin_observation()
        .expect("dropping an observer should release the cell");
    drop(resumed);
    cell.request_terminate();
    cell.join().await;
}

#[tokio::test]
async fn dropped_observer_preserves_consumed_output_for_the_next_observation() -> Result<()> {
    struct StartSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl CodeModeObserver for StartSignal {
        fn update(&mut self, update: CodeModeUpdate<'_>) {
            if matches!(update, CodeModeUpdate::NestedCallStarted { .. })
                && let Some(signal) = self.0.take()
            {
                let _ = signal.send(());
            }
        }
    }

    let (updates_tx, updates) = tokio::sync::mpsc::unbounded_channel();
    let (terminate, terminate_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = terminate_rx.await;
    });
    let cell = test_live_cell(1, updates, terminate, task);
    let observation = cell
        .begin_observation()
        .expect("first observer should acquire the cell");
    let observed_cell = Arc::clone(&cell);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let observer_task = tokio::spawn(async move {
        let mut observer = StartSignal(Some(started_tx));
        observe_cell(
            &observed_cell,
            observation,
            std::time::Instant::now(),
            ObservationMode::YieldAfter(Duration::from_secs(60)),
            None,
            &mut observer,
        )
        .await
    });
    updates_tx
        .send(CellUpdate::Content(ToolOutputContent::InputText {
            text: "survives dropped observer".to_owned(),
        }))
        .expect("test cell should receive output");
    updates_tx
        .send(CellUpdate::NestedCallStarted {
            call_id: "call/code-1".to_owned(),
            name: "test".to_owned(),
            input: Value::Null,
        })
        .expect("test cell should receive the observation barrier");
    tokio::time::timeout(Duration::from_secs(1), started_rx)
        .await?
        .map_err(|error| eyre!(error))?;

    observer_task.abort();
    let cancellation = match observer_task.await {
        Ok(_) => panic!("aborted observer task completed"),
        Err(cancellation) => cancellation,
    };
    assert!(cancellation.is_cancelled());

    updates_tx
        .send(CellUpdate::Completed)
        .expect("test cell should receive completion");
    let observation = cell
        .begin_observation()
        .expect("dropping an observer should release the cell");
    let (completed, running) = observe_cell(
        &cell,
        observation,
        std::time::Instant::now(),
        ObservationMode::YieldAfter(Duration::from_secs(1)),
        None,
        &mut super::IgnoreCodeModeUpdates,
    )
    .await;

    assert!(!running);
    assert!(
        execution_output(&completed).contains("survives dropped observer"),
        "{}",
        execution_output(&completed)
    );
    cell.request_terminate();
    cell.join().await;
    Ok(())
}

#[tokio::test]
async fn yield_deadline_preempts_already_buffered_runtime_output() {
    let (updates_tx, updates) = tokio::sync::mpsc::unbounded_channel();
    let (terminate, terminate_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = terminate_rx.await;
    });
    let cell = test_live_cell(1, updates, terminate, task);
    updates_tx
        .send(CellUpdate::Content(ToolOutputContent::InputText {
            text: "queued output".to_owned(),
        }))
        .expect("test cell should receive output");
    let observation = cell
        .begin_observation()
        .expect("test cell should not already have an observer");

    let (yielded, running) = observe_cell(
        &cell,
        observation,
        std::time::Instant::now(),
        ObservationMode::YieldAfter(Duration::ZERO),
        None,
        &mut super::IgnoreCodeModeUpdates,
    )
    .await;

    assert!(running);
    assert!(!execution_output(&yielded).contains("queued output"));

    updates_tx
        .send(CellUpdate::Completed)
        .expect("test cell should receive completion");
    let observation = cell
        .begin_observation()
        .expect("yielded observer should release the cell");
    let (completed, running) = observe_cell(
        &cell,
        observation,
        std::time::Instant::now(),
        ObservationMode::YieldAfter(Duration::from_secs(1)),
        None,
        &mut super::IgnoreCodeModeUpdates,
    )
    .await;

    assert!(!running);
    assert!(execution_output(&completed).contains("queued output"));
    cell.request_terminate();
    cell.join().await;
}

#[tokio::test]
async fn nested_tool_start_does_not_extend_the_outer_yield() {
    let (updates_tx, updates) = tokio::sync::mpsc::unbounded_channel();
    let (terminate, _terminate_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        updates_tx
            .send(CellUpdate::NestedCallStarted {
                call_id: "call/code-1".to_owned(),
                name: "write_stdin".to_owned(),
                input: serde_json::Value::Null,
            })
            .expect("observer should receive the nested call");
        tokio::time::sleep(Duration::from_millis(15)).await;
        updates_tx
            .send(CellUpdate::Completed)
            .expect("observer should receive cell completion");
    });
    let cell = test_live_cell(1, updates, terminate, task);
    let observation = cell
        .begin_observation()
        .expect("test cell should not already have an observer");

    let (execution, running) = observe_cell(
        &cell,
        observation,
        std::time::Instant::now(),
        ObservationMode::YieldAfter(Duration::from_millis(5)),
        None,
        &mut super::IgnoreCodeModeUpdates,
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
        .model_specs("test-session")
        .into_iter()
        .map(|spec| serde_json::to_value(spec).unwrap())
        .collect::<Vec<_>>();
    let description = specs[0]["description"]
        .as_str()
        .expect("exec should have a description");
    assert!(description.contains("// @exec:"));
    assert!(description.contains("should be a base64-encoded `data:` URL"));
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
            ToolOutputContent::InputImage { .. }
            | ToolOutputContent::InputAudio { .. }
            | ToolOutputContent::EncryptedContent { .. } => None,
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
                ToolOutputContent::InputImage { .. }
                | ToolOutputContent::InputAudio { .. }
                | ToolOutputContent::EncryptedContent { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn call_ids(calls: &[NestedToolCall]) -> Vec<&str> {
    calls.iter().map(|call| call.call_id.as_str()).collect()
}

fn test_live_cell(
    id: u64,
    updates: tokio::sync::mpsc::UnboundedReceiver<CellUpdate>,
    terminate: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
) -> Arc<LiveCell> {
    Arc::new(LiveCell {
        id,
        turn_id: AtomicU64::new(0),
        output_token_budget: crate::contract::DEFAULT_TOOL_OUTPUT_TOKENS,
        observation: Arc::new(tokio::sync::Mutex::new(CellObservationState {
            updates,
            buffered: ObservationBuffer::default(),
        })),
        lifecycle: Arc::new(CellLifecycle::new()),
        terminate: std::sync::Mutex::new(Some(terminate)),
        task: tokio::sync::Mutex::new(Some(task)),
    })
}

fn test_tools(workspace: &std::path::Path) -> ToolRuntime {
    let selected = Tools::builder()
        .tool(crate::standard::UpdatePlanTool::new())
        .build()
        .unwrap();
    ToolRuntime::new_with_tools(
        workspace,
        Some(WebSearchConfig {
            endpoint: "http://127.0.0.1:1/v1/alpha/search".to_owned(),
            auth: nanocodex_oai_api::auth::OpenAiAuth::api_key("test-key"),
        }),
        Some(super::super::ImageGenerationConfig {
            api_base_url: "http://127.0.0.1:1/v1".to_owned(),
            auth: nanocodex_oai_api::auth::OpenAiAuth::api_key("test-key"),
            save_root: workspace.to_path_buf(),
        }),
        &selected,
    )
}

fn test_context(history: &[ResponseItem]) -> ToolContext<'_> {
    test_context_with_call(history, "call-exec")
}

fn test_context_with_call<'a>(history: &'a [ResponseItem], call_id: &'a str) -> ToolContext<'a> {
    ToolContext::new(
        "test-model",
        "test-session",
        call_id,
        history,
        crate::contract::DEFAULT_TOOL_OUTPUT_TOKENS,
    )
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
