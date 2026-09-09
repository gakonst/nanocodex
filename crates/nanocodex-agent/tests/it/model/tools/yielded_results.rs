use super::*;

use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, contract::async_trait,
};
use tokio::sync::Semaphore;

#[derive(Clone, Copy, PartialEq)]
enum Completion {
    Success,
    Failure,
    CancelAfterResult,
}

struct BlockedResult {
    release: Arc<Semaphore>,
    fails: bool,
}

#[async_trait]
impl Tool for BlockedResult {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "blocked_result",
            "Return a value after the test releases the tool.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        self.release.acquire().await?.forget();
        Ok(if self.fails {
            ToolOutput::error("nested-result-delivered")
        } else {
            ToolOutput::text("nested-result-delivered")
        })
    }
}

#[tokio::test]
async fn nested_tool_result_survives_code_mode_yield() -> Result<()> {
    nested_tool_result_round_trip(1, Completion::Success).await
}

#[tokio::test]
async fn nested_tool_result_without_code_mode_yield() -> Result<()> {
    nested_tool_result_round_trip(0, Completion::Success).await
}

#[tokio::test]
async fn nested_tool_result_survives_multiple_code_mode_yields() -> Result<()> {
    nested_tool_result_round_trip(2, Completion::Success).await
}

#[tokio::test]
async fn failed_nested_tool_result_survives_code_mode_yield() -> Result<()> {
    nested_tool_result_round_trip(1, Completion::Failure).await
}

#[tokio::test]
async fn cancellation_does_not_repeat_nested_tool_result_after_code_mode_yield() -> Result<()> {
    nested_tool_result_round_trip(1, Completion::CancelAfterResult).await
}

async fn nested_tool_result_round_trip(yield_count: u32, completion: Completion) -> Result<()> {
    let yields = yield_count > 0;
    let fails = completion == Completion::Failure;
    let cancels = completion == Completion::CancelAfterResult;
    let final_call_id = if yields {
        format!("call-wait-{yield_count}")
    } else {
        "call-exec".to_owned()
    };
    let release_call_id = if yields {
        final_call_id.clone()
    } else {
        "call-exec/code-1".to_owned()
    };
    timeout(std::time::Duration::from_secs(15), async {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("ws://{}", listener.local_addr()?);
        let server = async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let _warmup = next_json(&mut socket).await?;
            send_warmup(&mut socket, "resp-warmup").await?;
            let _generation = next_json(&mut socket).await?;
            let yield_statement = "await yield_control();".repeat(yield_count as usize);
            let tail = if cancels { "await new Promise(() => {});" } else { "" };
            send_json(
                &mut socket,
                completed_response("resp-exec", &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": format!("const pending = tools.blocked_result({{}}); {yield_statement} try {{ text(await pending); }} catch (error) {{ text(error); }} {tail}")
                })]),
            ).await?;

            let mut continuation = next_json(&mut socket).await?;
            assert_eq!(continuation["previous_response_id"], "resp-exec");
            for wait_index in 1..=yield_count {
                assert!(continuation.to_string().contains("Script running with cell ID 1"));
                assert!(!continuation.to_string().contains("nested-result-delivered"));
                send_json(
                    &mut socket,
                    completed_response(&format!("resp-wait-{wait_index}"), &[json!({
                        "type": "function_call",
                        "call_id": format!("call-wait-{wait_index}"),
                        "name": "wait",
                        "arguments": "{\"cell_id\":\"1\",\"yield_time_ms\":10000}"
                    })]),
                ).await?;
                if cancels && wait_index == yield_count {
                    return Ok(());
                }
                continuation = next_json(&mut socket).await?;
                assert_eq!(continuation["previous_response_id"], format!("resp-wait-{wait_index}"));
                assert_eq!(continuation["input"][0]["call_id"], format!("call-wait-{wait_index}"));
            }
            assert_eq!(continuation["input"][0]["call_id"], final_call_id);
            assert!(continuation["input"][0].to_string().contains("nested-result-delivered"), "{continuation}");
            assert!(!continuation.to_string().contains("Script running with cell ID"));
            send_final(&mut socket, "resp-final").await
        };

        let release = Arc::new(Semaphore::new(0));
        let tools = Tools::builder()
            .without_defaults()
            .tool(BlockedResult { release: Arc::clone(&release), fails })
            .build()?;
        let workspace = tempfile::tempdir()?;
        let openai = OpenAi::builder("test-key").websocket_url(&endpoint).build()?;
        let (agent, mut events) = Nanocodex::builder(openai)
            .thinking(Thinking::Low)
            .workspace(workspace.path())
            .session_id(test_session_id())
            .tools(tools)
            .build()?;
        let turn = agent.prompt("Run the nested tool.").await?;
        let mut control = cancels.then(|| turn.control());
        drop(agent);
        let collect = async {
            let mut captured = Vec::new();
            while let Some(event) = events.recv().await {
                let payload = event.decode_payload::<Value>()?;
                if event.kind == AgentEventKind::ToolCall
                    && payload["call_id"] == release_call_id
                {
                    release.add_permits(1);
                }
                if event.kind == AgentEventKind::ToolResult
                    && payload["call_id"] == "call-exec/code-1"
                    && let Some(control) = control.take()
                {
                    control.cancel().await?;
                }
                captured.push((event.kind, payload));
            }
            Ok::<_, eyre::Report>(captured)
        };
        let (server_result, turn_result, captured) = tokio::join!(server, turn.result(), collect);
        server_result?;
        if cancels {
            assert!(matches!(turn_result, Err(NanocodexError::TurnCancelled)));
        } else {
            assert_eq!(turn_result?.final_message(), "done");
        }
        let captured = captured?;
        let calls = captured.iter().enumerate().filter(|(_, (kind, payload))| {
            *kind == AgentEventKind::ToolCall && payload["call_id"] == "call-exec/code-1"
        }).collect::<Vec<_>>();
        assert_eq!(calls.len(), 1, "nested tool must start exactly once");
        let (nested_start, (_, call)) = calls[0];
        assert_eq!(call["tool"], "blocked_result");
        assert_eq!(call["model_call_index"], 1);
        if yields {
            let exec_result = captured.iter().position(|(kind, payload)| {
                *kind == AgentEventKind::ToolResult && payload["call_id"] == "call-exec"
            }).expect("exec must emit its yielded result");
            let wait_start = captured.iter().position(|(kind, payload)| {
                *kind == AgentEventKind::ToolCall && payload["call_id"] == release_call_id
            }).expect("wait must start before the nested tool is released");
            assert!(nested_start < exec_result, "nested start must be observed before exec yields");
            assert!(exec_result < wait_start);
        }
        let results = captured.iter().filter(|(kind, payload)| {
            *kind == AgentEventKind::ToolResult && payload["call_id"] == "call-exec/code-1"
        }).collect::<Vec<_>>();
        assert_eq!(results.len(), 1, "every nested ToolCall must have exactly one corresponding ToolResult");
        let (_, result) = results[0];
        assert_eq!(result["tool"], "blocked_result");
        assert_eq!(result["status"], if fails { "failed" } else { "completed" });
        assert_eq!(result["result"], "nested-result-delivered");
        assert_eq!(result["structured_result"], "nested-result-delivered");
        Ok(())
    }).await?
}
