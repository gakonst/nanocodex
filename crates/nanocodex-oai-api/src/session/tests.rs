use std::{
    error::Error as _,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    task::Poll,
};

use ::tower::Service;
use futures_util::TryStreamExt;

use crate::{
    responses::{ContentItem, MessageRole, Usage},
    session::SessionId,
    tower::{
        CompactionOutput, GenerationOutput, ResponsePipelineStats, ResponsesAttemptKind,
        ResponsesOutput, ResponsesServiceResponse,
    },
};

use crate::{OpenAi, ResponseEvent};

use super::{ResponseError, ResponseErrorKind, response::estimate_cost};

#[derive(Clone)]
struct Scripted {
    calls: Arc<AtomicU32>,
}

impl Service<crate::ResponsesAttempt> for Scripted {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: crate::ResponsesAttempt) -> Self::Future {
        let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        let item = crate::ResponseItem::message(
            MessageRole::Assistant,
            [ContentItem::OutputText {
                text: format!("answer-{call}").into(),
                annotations: None,
                logprobs: None,
            }],
        );
        Box::pin(async move {
            request
                .emit(ResponseEvent::OutputTextDelta(format!("answer-{call}")))
                .await
                .expect("emit response delta");
            Ok(
                ResponsesServiceResponse::new(ResponsesOutput::Generation(GenerationOutput {
                    id: format!("resp-{call}"),
                    status: "completed".to_owned(),
                    end_turn: None,
                    final_message: Some(format!("answer-{call}")),
                    output_items: vec![item],
                    code_calls: Vec::new(),
                    usage: Some(crate::Usage {
                        input_tokens: 12,
                        output_tokens: 5,
                        total_tokens: 17,
                        ..crate::Usage::default()
                    }),
                    time_to_first_event_ns: 1,
                    time_to_first_output_ns: Some(1),
                    pipeline_stats: ResponsePipelineStats::default(),
                }))
                .with_connection_generation(1)
                .with_server_reasoning_included(true),
            )
        })
    }
}

#[tokio::test]
async fn response_stream_and_future_share_one_completed_operation() {
    let calls = Arc::new(AtomicU32::new(0));
    let factory_calls = Arc::clone(&calls);
    let openai = OpenAi::builder("test-key")
        .service(move || Scripted {
            calls: Arc::clone(&factory_calls),
        })
        .build()
        .unwrap();
    let mut session = openai
        .instructions("Answer only from supplied facts.")
        .build()
        .unwrap();
    let completed = {
        let mut turn = session.turn();
        let mut response = turn.create("The region is us-west-2.");

        let event = response.try_next().await.unwrap().unwrap();
        assert!(matches!(event, ResponseEvent::OutputTextDelta(delta) if delta == "answer-1"));
        let event = response.try_next().await.unwrap().unwrap();
        assert!(matches!(event, ResponseEvent::Completed { .. }));
        assert!(response.try_next().await.unwrap().is_none());
        response.await.unwrap()
    };

    assert_eq!(completed.output_text(), "answer-1");
    let estimated_cost = completed
        .estimated_cost()
        .expect("provider usage should produce an estimate");
    assert_eq!(estimated_cost.amount().decimal(), "0.00021");
    assert_eq!(
        completed.cost_status(),
        crate::CostStatus::EstimatedFromUsage
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(session.history_len(), 2);
    assert_eq!(session.active_context_tokens(), 17);
}

#[test]
fn missing_usage_never_becomes_a_zero_cost_estimate() {
    let (estimate, status) = estimate_cost(None, false, true);
    assert!(estimate.is_none());
    assert_eq!(status, crate::CostStatus::UsageNotReported);
}

#[test]
fn reported_usage_can_explicitly_skip_the_openai_price_table() {
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 5,
        total_tokens: 15,
        ..Usage::default()
    };
    let (estimate, status) = estimate_cost(Some(&usage), false, false);
    assert!(estimate.is_none());
    assert_eq!(status, crate::CostStatus::NotEstimated);
}

#[tokio::test]
async fn custom_service_cost_policy_reaches_managed_sessions() {
    let calls = Arc::new(AtomicU32::new(0));
    let factory_calls = Arc::clone(&calls);
    let openai = OpenAi::builder("test-key")
        .estimate_cost(false)
        .service(move || Scripted {
            calls: Arc::clone(&factory_calls),
        })
        .build()
        .unwrap();
    let mut session = openai
        .instructions("Answer only from supplied facts.")
        .build()
        .unwrap();
    let completed = session
        .turn()
        .create("The region is us-west-2.")
        .await
        .unwrap();

    assert!(completed.estimated_cost().is_none());
    assert_eq!(completed.cost_status(), crate::CostStatus::NotEstimated);
}

#[derive(Debug)]
struct AttemptObservation {
    previous_response_id: Option<String>,
    full_replay: bool,
    input: Vec<serde_json::Value>,
    input_bytes: Vec<Vec<u8>>,
}

#[derive(Clone)]
struct RecordingScripted {
    calls: Arc<AtomicU32>,
    observations: Arc<Mutex<Vec<AttemptObservation>>>,
    unmatched_first_call: bool,
    fail_on_second: bool,
}

impl Service<crate::ResponsesAttempt> for RecordingScripted {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: crate::ResponsesAttempt) -> Self::Future {
        let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        self.observations.lock().unwrap().push(AttemptObservation {
            previous_response_id: request.previous_response_id().map(str::to_owned),
            full_replay: request.is_full_replay(),
            input: request
                .input_items()
                .map(|item| serde_json::to_value(item).unwrap())
                .collect(),
            input_bytes: request
                .input_items()
                .map(|item| serde_json::to_vec(item).unwrap())
                .collect(),
        });
        if self.fail_on_second && call == 2 {
            return std::future::ready(Err(ResponseError::service(std::io::Error::other(
                "repaired request failed",
            ))));
        }
        let (output_items, final_message) = if self.unmatched_first_call && call == 1 {
            (
                vec![crate::ResponseItem::FunctionCall {
                    id: Some(crate::ResponseItemId::from("fc_1")),
                    name: "lookup".into(),
                    namespace: None,
                    arguments: r#"{"key":"region"}"#.into(),
                    call_id: "call_1".into(),
                    caller: None,
                    status: None,
                    created_by: None,
                    internal_chat_message_metadata_passthrough: None,
                }],
                None,
            )
        } else {
            (
                vec![crate::ResponseItem::message(
                    MessageRole::Assistant,
                    [ContentItem::OutputText {
                        text: format!("answer-{call}").into(),
                        annotations: None,
                        logprobs: None,
                    }],
                )],
                Some(format!("answer-{call}")),
            )
        };
        std::future::ready(Ok(ResponsesServiceResponse::new(
            ResponsesOutput::Generation(GenerationOutput {
                id: format!("resp-{call}"),
                status: "completed".to_owned(),
                end_turn: Some(call == 2),
                final_message,
                output_items,
                code_calls: Vec::new(),
                usage: None,
                time_to_first_event_ns: 1,
                time_to_first_output_ns: Some(1),
                pipeline_stats: ResponsePipelineStats::default(),
            }),
        )))
    }
}

#[tokio::test]
async fn sequential_creates_send_only_the_new_delta_after_completion() {
    let calls = Arc::new(AtomicU32::new(0));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_observations = Arc::clone(&observations);
    let openai = OpenAi::builder("test-key")
        .service(move || RecordingScripted {
            calls: Arc::clone(&factory_calls),
            observations: Arc::clone(&factory_observations),
            unmatched_first_call: false,
            fail_on_second: false,
        })
        .build()
        .unwrap();
    let mut session = openai
        .instructions("Remember deployment facts between calls.")
        .build()
        .unwrap();

    {
        let mut turn = session.turn();
        assert_eq!(
            turn.create("The region is us-west-2.")
                .await
                .unwrap()
                .output_text(),
            "answer-1"
        );
        assert_eq!(
            turn.create("What region did I give you?")
                .await
                .unwrap()
                .output_text(),
            "answer-2"
        );
    }

    let observations = observations.lock().unwrap();
    assert_eq!(observations.len(), 2);
    assert!(observations[0].full_replay);
    assert_eq!(observations[0].previous_response_id, None);
    assert_eq!(observations[0].input.len(), 3);
    assert!(!observations[1].full_replay);
    assert_eq!(
        observations[1].previous_response_id.as_deref(),
        Some("resp-1")
    );
    assert_eq!(observations[1].input.len(), 1);
    assert_eq!(observations[1].input[0]["role"], "user");
    assert_eq!(session.history_len(), 4);
}

#[tokio::test]
async fn unmatched_tool_call_forces_repaired_full_replay_then_restores_incremental_baseline() {
    let calls = Arc::new(AtomicU32::new(0));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_observations = Arc::clone(&observations);
    let openai = OpenAi::builder("test-key")
        .service(move || RecordingScripted {
            calls: Arc::clone(&factory_calls),
            observations: Arc::clone(&factory_observations),
            unmatched_first_call: true,
            fail_on_second: false,
        })
        .build()
        .unwrap();
    let mut session = openai
        .instructions("Repair incomplete tool calls before continuing.")
        .build()
        .unwrap();

    {
        let mut turn = session.turn();
        let first = turn.create("Look up the deployment region.").await.unwrap();
        assert_eq!(first.tool_calls().count(), 1);
        turn.create("Continue without running that tool.")
            .await
            .unwrap();
        turn.create("Now answer one more question.").await.unwrap();
    }

    let observations = observations.lock().unwrap();
    assert_eq!(observations.len(), 3);
    assert_eq!(
        (
            observations[1].full_replay,
            observations[1].previous_response_id.as_deref(),
            observations[1].input.len(),
        ),
        (true, None, 6),
        "prompt normalization must not be bypassed by a healthy incremental suffix"
    );
    assert_eq!(
        observations[0].input_bytes[..2],
        observations[1].input_bytes[..2],
        "repair must preserve the byte-stable request prefix"
    );
    assert_eq!(observations[1].input[3]["type"], "function_call");
    assert_eq!(observations[1].input[4]["type"], "function_call_output");
    assert_eq!(observations[1].input[4]["call_id"], "call_1");
    assert_eq!(observations[1].input[4]["output"], "aborted");
    assert_eq!(observations[1].input[5]["role"], "user");

    assert!(!observations[2].full_replay);
    assert_eq!(
        observations[2].previous_response_id.as_deref(),
        Some("resp-2")
    );
    assert_eq!(observations[2].input.len(), 1);
    assert_eq!(observations[2].input[0]["role"], "user");
    assert_eq!(session.history_len(), 7);
    assert_eq!(
        session
            .history()
            .filter(|item| matches!(
                item,
                crate::ResponseItem::FunctionCallOutput {
                    call_id,
                    output: crate::FunctionOutputBody::Text(output),
                    ..
                } if call_id.as_ref() == "call_1" && output.as_ref() == "aborted"
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn failed_repaired_replay_does_not_commit_normalized_history() {
    let calls = Arc::new(AtomicU32::new(0));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_observations = Arc::clone(&observations);
    let openai = OpenAi::builder("test-key")
        .service(move || RecordingScripted {
            calls: Arc::clone(&factory_calls),
            observations: Arc::clone(&factory_observations),
            unmatched_first_call: true,
            fail_on_second: true,
        })
        .build()
        .unwrap();
    let mut session = openai
        .instructions("Repair incomplete tool calls before continuing.")
        .build()
        .unwrap();

    {
        let mut turn = session.turn();
        turn.create("Look up the deployment region.").await.unwrap();
        assert!(
            turn.create("Continue without running that tool.")
                .await
                .is_err()
        );
    }

    let observations = observations.lock().unwrap();
    assert_eq!(observations.len(), 2);
    assert!(observations[1].full_replay);
    assert!(observations[1].previous_response_id.is_none());
    assert_eq!(observations[1].input[4]["output"], "aborted");
    assert_eq!(session.history_len(), 2);
    assert!(
        session
            .history()
            .all(|item| !matches!(item, crate::ResponseItem::FunctionCallOutput { .. }))
    );
}

#[derive(Clone)]
struct CompactingScripted {
    calls: Arc<AtomicU32>,
    observations: Arc<Mutex<Vec<AttemptObservation>>>,
    fail_compaction: bool,
}

impl Service<crate::ResponsesAttempt> for CompactingScripted {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: crate::ResponsesAttempt) -> Self::Future {
        let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        let is_compaction = matches!(request.kind(), ResponsesAttemptKind::Compaction);
        self.observations.lock().unwrap().push(AttemptObservation {
            previous_response_id: request.previous_response_id().map(str::to_owned),
            full_replay: request.is_full_replay(),
            input: request
                .input_items()
                .map(|item| serde_json::to_value(item).unwrap())
                .collect(),
            input_bytes: request
                .input_items()
                .map(|item| serde_json::to_vec(item).unwrap())
                .collect(),
        });
        if self.fail_compaction && is_compaction {
            return std::future::ready(Err(ResponseError::service(std::io::Error::other(
                "compaction failed",
            ))));
        }
        let output = if is_compaction {
            ResponsesOutput::Compaction(CompactionOutput {
                id: format!("resp-{call}"),
                status: "completed".to_owned(),
                item: crate::ResponseItem::Compaction {
                    id: None,
                    encrypted_content: "encrypted-summary".into(),
                    created_by: None,
                    internal_chat_message_metadata_passthrough: None,
                },
                usage: None,
                time_to_first_event_ns: 1,
                time_to_first_output_ns: Some(1),
                pipeline_stats: ResponsePipelineStats::default(),
            })
        } else {
            let item = crate::ResponseItem::message(
                MessageRole::Assistant,
                [ContentItem::OutputText {
                    text: format!("answer-{call}").into(),
                    annotations: None,
                    logprobs: None,
                }],
            );
            ResponsesOutput::Generation(GenerationOutput {
                id: format!("resp-{call}"),
                status: "completed".to_owned(),
                end_turn: None,
                final_message: Some(format!("answer-{call}")),
                output_items: vec![item],
                code_calls: Vec::new(),
                usage: None,
                time_to_first_event_ns: 1,
                time_to_first_output_ns: Some(1),
                pipeline_stats: ResponsePipelineStats::default(),
            })
        };
        std::future::ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

#[tokio::test]
async fn compaction_atomically_replaces_history_and_forces_one_full_replay() {
    let calls = Arc::new(AtomicU32::new(0));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_observations = Arc::clone(&observations);
    let openai = OpenAi::builder("test-key")
        .service(move || CompactingScripted {
            calls: Arc::clone(&factory_calls),
            observations: Arc::clone(&factory_observations),
            fail_compaction: false,
        })
        .build()
        .unwrap();
    let mut session = openai
        .instructions("Retain user facts across explicit compaction.")
        .build()
        .unwrap();

    {
        let mut turn = session.turn();
        turn.create("The deployment region is us-west-2.")
            .await
            .unwrap();
        turn.compact().await.unwrap();
    }
    assert_eq!(session.history_len(), 2);
    assert!(session.history().any(crate::ResponseItem::is_user_message));
    assert!(
        session
            .history()
            .any(|item| matches!(item, crate::ResponseItem::Compaction { .. }))
    );

    session
        .turn()
        .create("Recall the deployment region.")
        .await
        .unwrap();

    let observations = observations.lock().unwrap();
    assert_eq!(observations.len(), 3);
    assert_eq!(
        observations[1].previous_response_id.as_deref(),
        Some("resp-1")
    );
    assert!(observations[2].full_replay);
    assert_eq!(observations[2].previous_response_id, None);
    assert_eq!(observations[2].input.len(), 5);
}

#[tokio::test]
async fn compaction_phase_controls_exact_canonical_context_ordering() {
    let (mut session, observations) = compacting_session(false);
    {
        let mut turn = session.turn();
        turn.create(canonical_test_input("initial task"))
            .await
            .unwrap();
    }
    {
        let mut turn = session.turn();
        turn.compact().await.unwrap();
    }

    assert_eq!(
        session
            .history()
            .map(history_item_shape)
            .collect::<Vec<_>>(),
        ["user:initial task", "compaction"],
        "pre-turn compaction must install only retained user history and the summary"
    );

    session.turn().create("next normal turn").await.unwrap();
    {
        let observations = observations.lock().unwrap();
        assert_eq!(
            observations[2]
                .input
                .iter()
                .map(observed_item_shape)
                .collect::<Vec<_>>(),
            [
                "additional_tools",
                "developer:stable instructions",
                "user:initial task",
                "compaction",
                "developer:fresh permissions",
                "user:# AGENTS.md instructions for /workspace\n\n<INSTRUCTIONS>\nfresh rules\n</INSTRUCTIONS>|<environment_context>\n<cwd>/workspace</cwd>\n</environment_context>",
                "user:next normal turn",
            ],
            "the next normal turn must reinject the canonical snapshot after the summary"
        );
        assert_eq!(
            observations[0].input_bytes[2..4],
            observations[2].input_bytes[4..6],
            "an unchanged standalone snapshot must preserve its exact request bytes"
        );
    }

    let (mut session, _) = compacting_session(false);
    {
        let mut turn = session.turn();
        turn.create(canonical_test_input("mid-turn task"))
            .await
            .unwrap();
        turn.compact().await.unwrap();
    }
    assert_eq!(
        session
            .history()
            .map(history_item_shape)
            .collect::<Vec<_>>(),
        [
            "developer:fresh permissions",
            "user:# AGENTS.md instructions for /workspace\n\n<INSTRUCTIONS>\nfresh rules\n</INSTRUCTIONS>|<environment_context>\n<cwd>/workspace</cwd>\n</environment_context>",
            "user:mid-turn task",
            "compaction",
        ],
        "mid-turn compaction must inject canonical context before the last real user message"
    );
}

#[tokio::test]
async fn failed_compaction_leaves_history_and_continuation_state_unchanged() {
    let (mut session, observations) = compacting_session(true);
    {
        let mut turn = session.turn();
        turn.create(canonical_test_input("initial task"))
            .await
            .unwrap();
    }
    let before = session
        .history()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert!(session.turn().compact().await.is_err());
    assert_eq!(
        session
            .history()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        before
    );

    session.turn().create("retry normally").await.unwrap();
    let observations = observations.lock().unwrap();
    assert!(!observations[2].full_replay);
    assert_eq!(
        observations[2].previous_response_id.as_deref(),
        Some("resp-1")
    );
    assert_eq!(
        observations[2]
            .input
            .iter()
            .map(observed_item_shape)
            .collect::<Vec<_>>(),
        ["user:retry normally"]
    );
}

fn compacting_session(
    fail_compaction: bool,
) -> (
    super::Session<CompactingScripted>,
    Arc<Mutex<Vec<AttemptObservation>>>,
) {
    let calls = Arc::new(AtomicU32::new(0));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_observations = Arc::clone(&observations);
    let openai = OpenAi::builder("test-key")
        .service(move || CompactingScripted {
            calls: Arc::clone(&factory_calls),
            observations: Arc::clone(&factory_observations),
            fail_compaction,
        })
        .build()
        .unwrap();
    (
        openai.instructions("stable instructions").build().unwrap(),
        observations,
    )
}

fn canonical_test_input(task: &str) -> super::ResponseInput {
    super::ResponseInput::items([
        crate::ResponseItem::message(
            MessageRole::Developer,
            [ContentItem::input_text("fresh permissions")],
        ),
        crate::ResponseItem::message(
            MessageRole::User,
            [
                ContentItem::input_text(
                    "# AGENTS.md instructions for /workspace\n\n<INSTRUCTIONS>\nfresh rules\n</INSTRUCTIONS>",
                ),
                ContentItem::input_text(
                    "<environment_context>\n<cwd>/workspace</cwd>\n</environment_context>",
                ),
            ],
        ),
        crate::ResponseItem::message(
            MessageRole::User,
            [ContentItem::input_text(
                "<turn_aborted>\ninterrupted\n</turn_aborted>",
            )],
        ),
        crate::ResponseItem::message(MessageRole::User, [ContentItem::input_text(task)]),
    ])
}

fn history_item_shape(item: &crate::ResponseItem) -> String {
    observed_item_shape(&serde_json::to_value(item).unwrap())
}

fn observed_item_shape(item: &serde_json::Value) -> String {
    let item_type = item["type"].as_str().unwrap();
    if item_type != "message" {
        return item_type.to_owned();
    }
    let content = item["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|content| content["text"].as_str())
        .collect::<Vec<_>>()
        .join("|");
    format!("{}:{content}", item["role"].as_str().unwrap())
}

#[derive(Clone)]
struct FailingScripted;

impl Service<crate::ResponsesAttempt> for FailingScripted {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: crate::ResponsesAttempt) -> Self::Future {
        Box::pin(async move {
            request
                .emit(ResponseEvent::OutputTextDelta("uncommitted".to_owned()))
                .await
                .expect("emit response delta");
            Err(ResponseError::service(std::io::Error::other(
                "scripted failure",
            )))
        })
    }
}

#[tokio::test]
async fn failed_partial_response_never_commits_input_or_output() {
    let openai = OpenAi::builder("test-key")
        .service(|| FailingScripted)
        .build()
        .unwrap();
    let mut session = openai
        .instructions("Commit only complete Responses operations.")
        .build()
        .unwrap();
    {
        let mut turn = session.turn();
        let mut response = turn.create("This input must remain uncommitted.");

        assert!(matches!(
            response.try_next().await.unwrap(),
            Some(ResponseEvent::OutputTextDelta(delta)) if delta == "uncommitted"
        ));
        let error = response.try_next().await.unwrap_err();
        assert_eq!(error.to_string(), "scripted failure");
        assert!(response.await.is_err());
    }

    assert_eq!(session.history_len(), 0);
}

#[test]
fn boxed_tower_errors_preserve_context_window_classification() {
    let service_error =
        crate::ResponsesServiceError::from(crate::ResponsesError::ContextWindowExceeded {
            event: r#"{"error":{"code":"context_length_exceeded"}}"#.to_owned(),
        });
    let error = ResponseError::from(Box::new(service_error) as ::tower::BoxError);

    assert!(error.is_context_window_exceeded());
    assert_eq!(error.kind(), ResponseErrorKind::ContextWindowExceeded);
    assert!(matches!(
        error.responses_error(),
        Some(crate::ResponsesError::ContextWindowExceeded { .. })
    ));
    assert!(error.source().is_some());
}

#[tokio::test]
async fn dropping_an_unpolled_response_performs_no_work() {
    let calls = Arc::new(AtomicU32::new(0));
    let factory_calls = Arc::clone(&calls);
    let openai = OpenAi::builder("test-key")
        .service(move || Scripted {
            calls: Arc::clone(&factory_calls),
        })
        .build()
        .unwrap();
    let mut session = openai
        .instructions("Do not run abandoned operations.")
        .build()
        .unwrap();
    {
        let mut turn = session.turn();
        drop(turn.create("abandoned"));
    }

    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(session.history_len(), 0);
}

#[test]
fn session_ids_are_serializable_uuid_v7_values() {
    let id = SessionId::new();
    assert_eq!(id.as_uuid().get_version_num(), 7);

    let encoded = serde_json::to_string(&id).unwrap();
    assert_eq!(serde_json::from_str::<SessionId>(&encoded).unwrap(), id);
    assert!(
        "550e8400-e29b-41d4-a716-446655440000"
            .parse::<SessionId>()
            .is_err()
    );
}
