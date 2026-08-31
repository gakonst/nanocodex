use std::{
    future::{Ready, ready},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    task::{Context, Poll},
};

use nanocodex_oai_api::{
    responses::{ContentItem, MessageRole, ResponseItem, ResponseItemId, WarmupResponse},
    tower::{
        GenerationOutput, ResponsePipelineStats, ResponsesAttempt, ResponsesAttemptKind,
        ResponsesOutput, ResponsesServiceResponse,
    },
};
use tower::Service;

use super::*;

#[derive(Default)]
struct ObservedRequests {
    warmup_prefix: Vec<Vec<u8>>,
    generations: Vec<ObservedGeneration>,
}

struct ObservedGeneration {
    previous_response_id: Option<String>,
    full_replay: bool,
    input: Vec<Value>,
    input_bytes: Vec<Vec<u8>>,
}

#[derive(Clone)]
struct UnmatchedToolCallService {
    observed: Arc<Mutex<ObservedRequests>>,
    generation_calls: Arc<AtomicU32>,
}

impl Service<ResponsesAttempt> for UnmatchedToolCallService {
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
        let input_bytes = request
            .input_items()
            .map(|item| serde_json::to_vec(item).expect("response items serialize"))
            .collect::<Vec<_>>();
        let output = match request.kind() {
            ResponsesAttemptKind::Warmup => {
                self.observed.lock().unwrap().warmup_prefix = input_bytes;
                ResponsesOutput::Warmup(WarmupResponse {
                    id: "resp-warmup".to_owned(),
                    usage: None,
                    usage_metadata: None,
                })
            }
            ResponsesAttemptKind::Generation => {
                let call = self.generation_calls.fetch_add(1, Ordering::Relaxed) + 1;
                self.observed
                    .lock()
                    .unwrap()
                    .generations
                    .push(ObservedGeneration {
                        previous_response_id: request.previous_response_id().map(str::to_owned),
                        full_replay: request.is_full_replay(),
                        input: request
                            .input_items()
                            .map(|item| {
                                serde_json::to_value(item).expect("response items serialize")
                            })
                            .collect(),
                        input_bytes,
                    });
                let (output_items, final_message, end_turn) = if call == 1 {
                    (
                        vec![ResponseItem::FunctionCall {
                            id: Some(ResponseItemId::from("fc-unmatched")),
                            name: "lookup".into(),
                            namespace: None,
                            arguments: r#"{"key":"region"}"#.into(),
                            encrypted_function_args: None,
                            call_id: "call-unmatched".into(),
                            caller: None,
                            status: None,
                            created_by: None,
                            internal_chat_message_metadata_passthrough: None,
                        }],
                        None,
                        Some(false),
                    )
                } else {
                    let answer = format!("answer-{call}");
                    (
                        vec![ResponseItem::message(
                            MessageRole::Assistant,
                            [ContentItem::output_text(answer.clone())],
                        )],
                        Some(answer),
                        Some(true),
                    )
                };
                ResponsesOutput::Generation(GenerationOutput {
                    id: format!("resp-{call}"),
                    status: "completed".to_owned(),
                    end_turn,
                    final_message,
                    output_items,
                    // A caller-supplied aggregate can retain the provider item
                    // while declining to dispatch it as an executable call.
                    code_calls: Vec::new(),
                    usage: None,
                    usage_metadata: None,
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            _ => panic!("the normalization regression must not compact"),
        };
        ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

#[tokio::test]
async fn agent_repairs_unmatched_tool_calls_before_continuing_and_restores_delta() -> Result<()> {
    let observed = Arc::new(Mutex::new(ObservedRequests::default()));
    let generation_calls = Arc::new(AtomicU32::new(0));
    let factory_observed = Arc::clone(&observed);
    let factory_generation_calls = Arc::clone(&generation_calls);
    let openai = OpenAi::builder("test-key")
        .service(move || UnmatchedToolCallService {
            observed: Arc::clone(&factory_observed),
            generation_calls: Arc::clone(&factory_generation_calls),
        })
        .build()?;
    let workspace = tempfile::tempdir()?;
    let tools = Tools::builder().without_defaults().build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .instructions("Repair incomplete tool calls before continuing.")
        .workspace(workspace.path())
        .tools(tools)
        .build()?;
    drop(events);

    let first = agent
        .prompt("Look up the deployment region.")
        .await?
        .await?;
    assert_eq!(first.final_message(), "answer-2");
    let second = agent.prompt("Now answer one more question.").await?.await?;
    assert_eq!(second.final_message(), "answer-3");

    let observed = observed.lock().unwrap();
    assert_eq!(observed.generations.len(), 3);
    let repaired = &observed.generations[1];
    assert!(repaired.full_replay);
    assert_eq!(repaired.previous_response_id, None);
    assert_eq!(
        repaired.input_bytes[..observed.warmup_prefix.len()],
        observed.warmup_prefix,
        "repair must preserve the byte-stable request prefix"
    );
    let call_index = repaired
        .input
        .iter()
        .position(|item| item["type"] == "function_call")
        .expect("the full replay retains the unmatched provider call");
    assert_eq!(repaired.input[call_index]["call_id"], "call-unmatched");
    assert_eq!(
        repaired.input[call_index + 1]["type"],
        "function_call_output"
    );
    assert_eq!(repaired.input[call_index + 1]["call_id"], "call-unmatched");
    assert_eq!(repaired.input[call_index + 1]["output"], "aborted");

    let healthy = &observed.generations[2];
    assert!(!healthy.full_replay);
    assert_eq!(healthy.previous_response_id.as_deref(), Some("resp-2"));
    assert_eq!(healthy.input.len(), 1);
    assert_eq!(healthy.input[0]["role"], "user");

    let snapshot = serde_json::to_value(
        second
            .snapshot()
            .expect("local turns always retain a snapshot"),
    )?;
    let repaired_outputs = snapshot["history"]
        .as_array()
        .expect("snapshot history is an array")
        .iter()
        .filter(|item| {
            item["type"] == "function_call_output"
                && item["call_id"] == "call-unmatched"
                && item["output"] == "aborted"
        })
        .count();
    assert_eq!(repaired_outputs, 1);
    drop((observed, agent));
    Ok(())
}
