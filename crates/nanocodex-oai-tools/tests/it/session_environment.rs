//! Session identity reaches every subprocess a bound tool selection launches.
use std::time::Duration;

use nanocodex_oai_tools::{
    SessionEnvironment, ToolContext, ToolExposure, ToolInput, Tools,
    mcp::{Mcp, McpServer},
    runtime::ToolRuntime,
};
use serde_json::{Value, json};

const PRINT_IDENTITY: &str = r#"printf '%s|%s' "$CODEX_THREAD_ID" "$NANOCODEX_ROOT_SESSION_ID""#;

async fn shell_identity(runtime: &ToolRuntime, session_id: &str) -> String {
    let input = serde_json::value::to_raw_value(&json!({ "cmd": PRINT_IDENTITY })).unwrap();
    let output = runtime
        .execute_tool(
            "exec_command",
            ToolInput::Function(input),
            ToolContext::new("synthetic-model", session_id, "identity", &[], 1000),
        )
        .await
        .unwrap();
    assert!(output.success, "{}", output.structured_result());
    output.structured_result()["output"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A root, and a subagent of that root sharing the same tool selection, each
/// observe their own session id through shell commands; the shared MCP stdio
/// server observes the session that started it, overriding configured values.
#[tokio::test]
async fn bound_sessions_export_identity_to_shell_commands_and_mcp_stdio_servers() {
    let _runtime_lock = crate::TOOL_RUNTIME_TEST_LOCK.lock().await;
    let workspace = tempfile::tempdir().unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mcp-stdio-server.mjs");
    let mcp = Mcp::builder()
        .server(
            "fixture",
            McpServer::stdio("node")
                .arg(fixture.to_string_lossy())
                .env("CODEX_THREAD_ID", "configured-spoof"),
        )
        .build()
        .unwrap();
    let handle = mcp.handle();
    let tools = Tools::builder()
        .exposure(ToolExposure::DirectOnly)
        .process_environment([("NANOCODEX_ROOT_SESSION_ID", "caller-spoof")])
        .provider(mcp)
        .build()
        .unwrap();

    let root = SessionEnvironment::root("root-session");
    let child = SessionEnvironment::new("child-session", root.session_id());
    let root_runtime = ToolRuntime::new_with_tools(
        workspace.path(),
        None,
        None,
        &tools.clone().for_session(&root),
    );
    let child_runtime =
        ToolRuntime::new_with_tools(workspace.path(), None, None, &tools.for_session(&child));

    assert_eq!(
        shell_identity(&root_runtime, "root-session").await,
        "root-session|root-session"
    );
    assert_eq!(
        shell_identity(&child_runtime, "child-session").await,
        "child-session|root-session"
    );

    let readiness = handle.native_wait(Duration::from_secs(10)).await;
    assert_eq!(readiness["complete"], true, "{readiness}");
    let result = handle
        .native_call(
            "mcp__fixture__echo",
            json!({"message":"__environment__"}),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(
        result["structuredContent"]["environment"],
        json!({"CODEX_THREAD_ID":"root-session","NANOCODEX_ROOT_SESSION_ID":"root-session"}),
        "{result}"
    );
    root_runtime.control().cancel().await;
    child_runtime.control().cancel().await;
}
