use super::*;

use std::{
    future::{Ready, ready},
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
use nanocodex_oai_tools::{
    Tool, ToolContext, ToolDefinition, ToolOutput, runtime::DynamicToolProvider,
};
use tower::Service;

struct PanickingProvider;

#[nanocodex_oai_tools::contract::async_trait]
impl DynamicToolProvider for PanickingProvider {
    fn start(&self) {}

    fn direct_tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn available_definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::function(
            "panic__boom",
            "Panics to verify the public runtime boundary.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )]
    }

    async fn execute(
        &self,
        name: &str,
        _input: Value,
        _context: ToolContext<'_>,
    ) -> Option<ToolOutput> {
        assert_eq!(name, "panic__boom");
        panic!("provider panic payload retained only in tracing")
    }
}

#[derive(Clone)]
struct PanicRecoveryService {
    calls: Arc<AtomicU32>,
}

impl Service<ResponsesAttempt> for PanicRecoveryService {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let output = match (call, request.kind()) {
            (0, ResponsesAttemptKind::Warmup) => ResponsesOutput::Warmup(WarmupResponse {
                id: "resp-warmup".to_owned(),
                usage: None,
            }),
            (1, ResponsesAttemptKind::Generation) => panic_generation(),
            (2, ResponsesAttemptKind::Generation) => {
                let input = request
                    .input_items()
                    .map(|item| serde_json::to_value(item).expect("input item serializes"))
                    .collect::<Vec<_>>();
                assert_eq!(input.len(), 1, "{input:?}");
                assert_eq!(input[0]["type"], "custom_tool_call_output");
                assert_eq!(input[0]["call_id"], "call-panic");
                // The repaired nested panic surfaces as the cell's script error.
                let output = input[0]["output"].as_array().expect("exec content output");
                assert_eq!(output.len(), 2, "{input:?}");
                assert!(
                    output[0]["text"]
                        .as_str()
                        .is_some_and(|header| header.starts_with("Script failed\n")),
                    "{input:?}"
                );
                assert_eq!(output[1]["text"], "Script error:\naborted");
                final_generation("resp-recovered", "recovered")
            }
            (3, ResponsesAttemptKind::Generation) => {
                assert!(request.input_items().any(|item| {
                    serde_json::to_string(item)
                        .is_ok_and(|item| item.contains("Run another prompt."))
                }));
                final_generation("resp-later", "later")
            }
            _ => panic!("unexpected attempt {call}: {:?}", request.kind()),
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

// Agents require Code Mode since eda4a21e3; the provider runs as a nested tool.
const PANIC_CELL: &str = "text(await tools.panic__boom({}));";

fn panic_generation() -> ResponsesOutput {
    let item = serde_json::from_value(json!({
        "type": "custom_tool_call",
        "call_id": "call-panic",
        "name": "exec",
        "input": PANIC_CELL
    }))
    .expect("exec call item decodes");
    ResponsesOutput::Generation(GenerationOutput {
        id: "resp-panic".to_owned(),
        reported_model: None,
        status: "completed".to_owned(),
        end_turn: Some(false),
        final_message: None,
        output_items: vec![item],
        code_calls: vec![CodeCall {
            call_id: "call-panic".to_owned(),
            name: "exec".to_owned(),
            namespace: None,
            input: PANIC_CELL.to_owned(),
            kind: CodeCallKind::Custom,
        }],
        usage: None,
        time_to_first_event_ns: 0,
        time_to_first_output_ns: None,
        pipeline_stats: ResponsePipelineStats::default(),
    })
}

fn final_generation(response_id: &str, message: &str) -> ResponsesOutput {
    ResponsesOutput::Generation(GenerationOutput {
        id: response_id.to_owned(),
        reported_model: None,
        status: "completed".to_owned(),
        end_turn: Some(true),
        final_message: Some(message.to_owned()),
        output_items: vec![ResponseItem::message(
            MessageRole::Assistant,
            [ContentItem::output_text(message)],
        )],
        code_calls: Vec::new(),
        usage: None,
        time_to_first_event_ns: 0,
        time_to_first_output_ns: None,
        pipeline_stats: ResponsePipelineStats::default(),
    })
}

#[tokio::test]
async fn provider_panic_is_repaired_and_the_private_driver_remains_usable() -> Result<()> {
    let calls = Arc::new(AtomicU32::new(0));
    let service_calls = Arc::clone(&calls);
    let openai = OpenAi::builder("test-key")
        .service(move || PanicRecoveryService {
            calls: Arc::clone(&service_calls),
        })
        .build()?;
    let tools = Tools::builder()
        .without_defaults()
        .provider(PanickingProvider)
        .build()?;
    let workspace = temporary_workspace("provider-panic")?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools)
        .build()?;

    assert_eq!(
        agent
            .prompt("Trigger the provider.")
            .await?
            .result()
            .await?
            .final_message(),
        "recovered"
    );
    assert_eq!(
        agent
            .prompt("Run another prompt.")
            .await?
            .result()
            .await?
            .final_message(),
        "later"
    );
    agent.shutdown().await?;
    drop(agent);

    let mut tool_results = Vec::new();
    let mut completed_turns = 0;
    let mut failed_turns = 0;
    while let Some(event) = events.recv().await {
        match event.kind {
            AgentEventKind::ToolResult => {
                tool_results.push(event.decode_payload::<Value>()?);
            }
            AgentEventKind::RunCompleted => completed_turns += 1,
            AgentEventKind::RunFailed => failed_turns += 1,
            _ => {}
        }
    }
    eprintln!("Provider panic tool results: {tool_results:?}");
    assert_eq!(tool_results.len(), 2, "{tool_results:?}");
    let nested = tool_results
        .iter()
        .find(|result| result["tool"] == "panic__boom")
        .expect("nested provider result");
    assert!(
        nested["call_id"]
            .as_str()
            .is_some_and(|call_id| call_id.starts_with("call-panic/")),
        "{nested}"
    );
    assert_eq!(nested["status"], "failed");
    assert_eq!(nested["result"], "aborted");
    let cell = tool_results
        .iter()
        .find(|result| result["tool"] == "exec")
        .expect("exec cell result");
    assert_eq!(cell["call_id"], "call-panic");
    assert_eq!(cell["status"], "failed");
    assert_eq!(completed_turns, 2);
    assert_eq!(failed_turns, 0);
    assert_eq!(calls.load(Ordering::Relaxed), 4);

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
