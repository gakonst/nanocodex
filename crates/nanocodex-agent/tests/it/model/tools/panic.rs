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
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolOutput, runtime::DynamicToolProvider,
};
use tower::Service;

struct PanickingProvider;

#[nanocodex_tools::contract::async_trait]
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
                assert_eq!(input[0]["type"], "function_call_output");
                assert_eq!(input[0]["call_id"], "call-panic");
                assert_eq!(input[0]["output"], "aborted");
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

fn panic_generation() -> ResponsesOutput {
    let item = serde_json::from_value(json!({
        "type": "function_call",
        "call_id": "call-panic",
        "namespace": "panic__",
        "name": "boom",
        "arguments": "{}"
    }))
    .expect("function call item decodes");
    ResponsesOutput::Generation(GenerationOutput {
        id: "resp-panic".to_owned(),
        status: "completed".to_owned(),
        end_turn: Some(false),
        final_message: None,
        output_items: vec![item],
        code_calls: vec![CodeCall {
            call_id: "call-panic".to_owned(),
            name: "boom".to_owned(),
            namespace: Some("panic__".to_owned()),
            input: "{}".to_owned(),
            kind: CodeCallKind::Function,
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
    assert_eq!(tool_results.len(), 1);
    assert_eq!(tool_results[0]["call_id"], "call-panic");
    assert_eq!(tool_results[0]["status"], "failed");
    assert_eq!(tool_results[0]["result"], "aborted");
    assert_eq!(completed_turns, 2);
    assert_eq!(failed_turns, 0);
    assert_eq!(calls.load(Ordering::Relaxed), 4);

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
