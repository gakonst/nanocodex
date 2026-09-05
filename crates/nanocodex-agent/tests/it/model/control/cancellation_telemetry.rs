use super::*;

use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU32, Ordering},
    task::{Context, Poll},
};

use nanocodex_oai_api::{
    responses::{ContentItem, MessageRole, ResponseItem, WarmupResponse},
    tower::{
        CodeCall, CodeCallKind, GenerationOutput, ResponsePipelineStats, ResponsesAttempt,
        ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
    },
};
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, contract::async_trait,
};
use tower::Service;

struct CompletedNestedTool;

struct PendingNestedTool {
    started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Tool for CompletedNestedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "stats__completed",
            "Completes after a deterministic amount of work.",
            json!({
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
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        Ok(ToolOutput::text("completed"))
    }
}

#[async_trait]
impl Tool for PendingNestedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "stats__pending",
            "Remains active until its owning turn is cancelled.",
            json!({
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
        self.started.notify_one();
        std::future::pending().await
    }
}

#[derive(Clone)]
struct NestedCancellationService {
    calls: Arc<AtomicU32>,
}

impl Service<ResponsesAttempt> for NestedCancellationService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _context: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            let output = match (call, request.kind()) {
                (0, ResponsesAttemptKind::Warmup) => ResponsesOutput::Warmup(WarmupResponse {
                    id: "resp-warmup".to_owned(),
                    usage: None,
                }),
                (1, ResponsesAttemptKind::Generation) => nested_tool_generation(),
                (2, ResponsesAttemptKind::Generation) => recovered_nested_tool_generation(),
                (3, ResponsesAttemptKind::Generation) => {
                    assert!(request.input_items().any(|item| {
                        serde_json::to_string(item).is_ok_and(|item| {
                            item.contains("call-recovered") && item.contains("completed")
                        })
                    }));
                    final_generation()
                }
                _ => panic!("unexpected attempt {call}: {:?}", request.kind()),
            };
            Ok(ResponsesServiceResponse::new(output))
        })
    }
}

fn recovered_nested_tool_generation() -> ResponsesOutput {
    let input = r#"text(await tools.stats__completed({}));"#;
    let output_item = serde_json::from_value(json!({
        "type": "custom_tool_call",
        "call_id": "call-recovered",
        "name": "exec",
        "input": input
    }))
    .expect("custom tool call item decodes");
    ResponsesOutput::Generation(GenerationOutput {
        id: "resp-recovered-tools".to_owned(),
        status: "completed".to_owned(),
        end_turn: Some(false),
        final_message: None,
        output_items: vec![output_item],
        code_calls: vec![CodeCall {
            call_id: "call-recovered".to_owned(),
            name: "exec".to_owned(),
            namespace: None,
            input: input.to_owned(),
            kind: CodeCallKind::Custom,
        }],
        usage: None,
        time_to_first_event_ns: 0,
        time_to_first_output_ns: None,
        pipeline_stats: ResponsePipelineStats::default(),
    })
}

fn final_generation() -> ResponsesOutput {
    ResponsesOutput::Generation(GenerationOutput {
        id: "resp-recovered-final".to_owned(),
        status: "completed".to_owned(),
        end_turn: Some(true),
        final_message: Some("recovered".to_owned()),
        output_items: vec![ResponseItem::message(
            MessageRole::Assistant,
            [ContentItem::output_text("recovered")],
        )],
        code_calls: Vec::new(),
        usage: None,
        time_to_first_event_ns: 0,
        time_to_first_output_ns: None,
        pipeline_stats: ResponsePipelineStats::default(),
    })
}

fn nested_tool_generation() -> ResponsesOutput {
    let input = r#"
await tools.stats__completed({});
await tools.stats__pending({});
text("unreachable");
"#;
    let output_item = serde_json::from_value(json!({
        "type": "custom_tool_call",
        "call_id": "call-exec",
        "name": "exec",
        "input": input
    }))
    .expect("custom tool call item decodes");
    ResponsesOutput::Generation(GenerationOutput {
        id: "resp-tools".to_owned(),
        status: "completed".to_owned(),
        end_turn: Some(false),
        final_message: None,
        output_items: vec![output_item],
        code_calls: vec![CodeCall {
            call_id: "call-exec".to_owned(),
            name: "exec".to_owned(),
            namespace: None,
            input: input.to_owned(),
            kind: CodeCallKind::Custom,
        }],
        usage: None,
        time_to_first_event_ns: 0,
        time_to_first_output_ns: None,
        pipeline_stats: ResponsePipelineStats::default(),
    })
}

#[tokio::test]
async fn cancellation_retains_completed_nested_progress_in_terminal_metrics() -> Result<()> {
    let calls = Arc::new(AtomicU32::new(0));
    let pending_started = Arc::new(tokio::sync::Notify::new());
    let service_calls = Arc::clone(&calls);
    let openai = OpenAi::builder("test-key")
        .service(move || NestedCancellationService {
            calls: Arc::clone(&service_calls),
        })
        .build()?;
    let tools = Tools::builder()
        .without_defaults()
        .tool(CompletedNestedTool)
        .tool(PendingNestedTool {
            started: Arc::clone(&pending_started),
        })
        .build()?;
    let workspace = temporary_workspace("cancel-nested-metrics")?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools)
        .build()?;

    let turn = agent.prompt("Run nested tools.").await?;
    timeout(
        std::time::Duration::from_secs(5),
        pending_started.notified(),
    )
    .await
    .map_err(|_| eyre!("pending nested tool did not start"))?;
    turn.cancel().await?;
    assert!(matches!(
        turn.result().await,
        Err(NanocodexError::TurnCancelled)
    ));
    assert_eq!(
        agent
            .prompt("Run a healthy nested tool.")
            .await?
            .result()
            .await?
            .final_message(),
        "recovered"
    );
    agent.shutdown().await?;
    drop(agent);

    let mut completed_nested_duration_ns = None;
    let mut cancelled_nested = None;
    let mut terminal = None;
    let mut ordered_terminals = Vec::new();
    while let Some(event) = events.recv().await {
        if event.kind == AgentEventKind::ToolResult {
            let result = event.decode_payload::<Value>()?;
            ordered_terminals.push(format!(
                "{}:{}",
                result["call_id"].as_str().unwrap_or("missing"),
                result["status"].as_str().unwrap_or("missing")
            ));
            if result["call_id"] == "call-exec/code-1" && result["status"] == "completed" {
                completed_nested_duration_ns = result["duration_ns"].as_u64();
            } else if result["call_id"] == "call-exec/code-2" {
                cancelled_nested = Some(result);
            }
        } else if event.kind == AgentEventKind::RunFailed {
            let result = event.decode_payload::<Value>()?;
            ordered_terminals.push(format!(
                "run:{}",
                result["status"].as_str().unwrap_or("missing")
            ));
            terminal = Some(result);
        } else if event.kind == AgentEventKind::RunCompleted {
            let result = event.decode_payload::<Value>()?;
            ordered_terminals.push(format!(
                "run:{}",
                result["status"].as_str().unwrap_or("missing")
            ));
        }
    }
    let completed_nested_duration_ns = completed_nested_duration_ns
        .ok_or_else(|| eyre!("completed nested tool result was not emitted"))?;
    let cancelled_nested =
        cancelled_nested.ok_or_else(|| eyre!("cancelled nested tool result was not emitted"))?;
    assert_eq!(cancelled_nested["tool"], "stats__pending");
    assert_eq!(cancelled_nested["status"], "cancelled");
    assert!(cancelled_nested["duration_ns"].as_u64().is_some());
    assert!(cancelled_nested["started_after_ns"].as_u64().is_some());
    assert!(
        cancelled_nested["result"]
            .as_str()
            .is_some_and(|result| result.starts_with("aborted by user after "))
    );
    assert_eq!(
        cancelled_nested["structured_result"],
        cancelled_nested["result"]
    );
    let terminal = terminal.ok_or_else(|| eyre!("cancelled terminal event was not emitted"))?;
    assert_eq!(terminal["status"], "cancelled");
    assert_eq!(terminal["tool_calls"], 3);
    let work = terminal["tool_work_duration_ns"]
        .as_u64()
        .ok_or_else(|| eyre!("tool work duration was missing"))?;
    let wall = terminal["tool_wall_duration_ns"]
        .as_u64()
        .ok_or_else(|| eyre!("tool wall duration was missing"))?;
    assert!(work >= completed_nested_duration_ns);
    assert!(wall >= work);
    assert_eq!(calls.load(Ordering::Relaxed), 4);
    assert_eq!(
        ordered_terminals,
        [
            "call-exec/code-1:completed",
            "call-exec/code-2:cancelled",
            "call-exec:cancelled",
            "run:cancelled",
            "call-recovered/code-1:completed",
            "call-recovered:completed",
            "run:completed",
        ]
    );

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
