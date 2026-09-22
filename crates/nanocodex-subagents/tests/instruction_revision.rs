use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use nanocodex_agent::transport::ResponsesTransport;
use nanocodex_agent::{Nanocodex, OpenAi, ResponseError};
use nanocodex_oai_api::{
    events::AgentEventKind,
    responses::{ContentItem, MessageRole, ResponseItem},
    tower::{
        CodeCall, CodeCallKind, GenerationOutput, ResponsePipelineStats, ResponsesAttempt,
        ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
    },
};
use nanocodex_subagents::{
    AgentStatus, AgentTask, AgentUpdate, MessagePriority, MessagePurpose, channel, install_tools,
    start_agent,
};
use nanocodex_tools::Tools;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tower::Service;

type PendingGeneration = (Vec<Value>, oneshot::Sender<ResponsesOutput>);

#[derive(Clone)]
struct ControlledProvider(mpsc::UnboundedSender<PendingGeneration>);

impl Service<ResponsesAttempt> for ControlledProvider {
    type Response = ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        assert!(matches!(request.kind(), ResponsesAttemptKind::Generation));
        let input = request
            .input_items()
            .map(|item| serde_json::to_value(item).unwrap())
            .collect();
        let (reply, response) = oneshot::channel();
        self.0.send((input, reply)).unwrap();
        Box::pin(async move { Ok(ResponsesServiceResponse::new(response.await.unwrap())) })
    }
}

fn generation(call: Option<(&str, &str)>) -> ResponsesOutput {
    let (output_items, code_calls) = match call {
        Some((id, result)) => {
            let arguments = json!({"output": {"result": result}}).to_string();
            (
                vec![
                    serde_json::from_value(json!({
                        "type": "function_call", "call_id": id, "name": "submit_result",
                        "arguments": arguments,
                    }))
                    .unwrap(),
                ],
                vec![CodeCall {
                    call_id: id.to_owned(),
                    name: "submit_result".to_owned(),
                    namespace: None,
                    input: arguments,
                    kind: CodeCallKind::Function,
                }],
            )
        }
        None => (
            vec![ResponseItem::message(
                MessageRole::Assistant,
                [ContentItem::output_text("done")],
            )],
            Vec::new(),
        ),
    };
    ResponsesOutput::Generation(GenerationOutput {
        id: format!("resp-{}", call.map_or("final", |(id, _)| id)),
        status: "completed".to_owned(),
        end_turn: Some(call.is_none()),
        final_message: call.is_none().then(|| "done".to_owned()),
        output_items,
        code_calls,
        usage: None,
        time_to_first_event_ns: 0,
        time_to_first_output_ns: None,
        pipeline_stats: ResponsePipelineStats::default(),
    })
}

fn assert_receipt(input: &[Value], call_id: &str, accepted: bool, status: &str) {
    let item = input
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
        .expect("submission result must reach the next request");
    let output: Value =
        serde_json::from_str(item["output"].as_str().expect("text receipt")).unwrap();
    assert_eq!(output, json!({"accepted": accepted, "status": status}));
}

#[tokio::test]
async fn repeated_in_flight_steering_supersedes_old_submissions() {
    regression(false).await;
}

#[tokio::test]
async fn steering_after_acceptance_queues_another_turn() {
    regression(true).await;
}

async fn regression(steer_after_acceptance: bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        let (requests, mut generations) = mpsc::unbounded_channel();
        let openai = OpenAi::builder("test-key")
            .transport(ResponsesTransport::Https)
            .service(move || ControlledProvider(requests.clone()))
            .build()
            .unwrap();
        let (registry, control, mut updates) = channel(1);
        let tool_registry = Arc::clone(&registry);
        let root_handle = Arc::new(Mutex::new(None));
        let captured_handle = Arc::clone(&root_handle);
        let (parent, events) = Nanocodex::builder(openai)
            .tools_factory(move |handle| {
                captured_handle
                    .lock()
                    .unwrap()
                    .get_or_insert_with(|| handle.clone());
                install_tools(
                    Tools::builder().without_defaults().build()?,
                    handle,
                    Arc::clone(&tool_registry),
                )
            })
            .build()
            .unwrap();
        drop(events);
        let session = parent.session_id().to_string();
        let handle = root_handle.lock().unwrap().clone().unwrap();
        let child = start_agent(
            &handle,
            &registry,
            &session,
            AgentTask {
                role: "revision probe".into(),
                task: "Return the requested result".into(),
                output_schema: json!({"type":"object", "properties":{"result":{"type":"string"}},
                "required":["result"], "additionalProperties":false}),
            },
        )
        .await
        .unwrap();

        let (_, first) = generations.recv().await.unwrap();
        registry
            .send_message(
                &session,
                child.agent_id,
                MessagePriority::Urgent,
                MessagePurpose::Coordinate,
                None,
                "First steering: use NEWER".into(),
            )
            .await
            .unwrap();
        first
            .send(generation(Some(("old", "OLD"))))
            .unwrap_or_else(|_| panic!("provider response receiver closed"));

        let (input, second) = generations.recv().await.unwrap();
        assert!(
            serde_json::to_string(&input)
                .unwrap()
                .contains("First steering: use NEWER")
        );
        assert_receipt(&input, "old", false, "superseded");
        registry
            .send_message(
                &session,
                child.agent_id,
                MessagePriority::Urgent,
                MessagePurpose::Coordinate,
                None,
                "Second steering: use CURRENT".into(),
            )
            .await
            .unwrap();
        second
            .send(generation(Some(("newer", "NEWER"))))
            .unwrap_or_else(|_| panic!("provider response receiver closed"));

        let (input, current) = generations.recv().await.unwrap();
        assert!(
            serde_json::to_string(&input)
                .unwrap()
                .contains("Second steering: use CURRENT")
        );
        assert_receipt(&input, "newer", false, "superseded");
        current
            .send(generation(Some(("current", "CURRENT"))))
            .unwrap_or_else(|_| panic!("provider response receiver closed"));
        let (input, final_response) = generations.recv().await.unwrap();
        assert_receipt(&input, "current", true, "accepted");
        if steer_after_acceptance {
            let receipt = registry
                .send_message(
                    &session,
                    child.agent_id,
                    MessagePriority::Urgent,
                    MessagePurpose::Coordinate,
                    None,
                    "After acceptance: use FOLLOWUP".into(),
                )
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_value(receipt).unwrap()["disposition"],
                "queued"
            );
        }
        final_response
            .send(generation(None))
            .unwrap_or_else(|_| panic!("provider response receiver closed"));
        if steer_after_acceptance {
            let (input, followup) = generations.recv().await.unwrap();
            assert!(
                serde_json::to_string(&input)
                    .unwrap()
                    .contains("After acceptance: use FOLLOWUP")
            );
            followup
                .send(generation(Some(("followup", "FOLLOWUP"))))
                .unwrap_or_else(|_| panic!("provider response receiver closed"));
            let (input, finish) = generations.recv().await.unwrap();
            assert_receipt(&input, "followup", true, "accepted");
            finish
                .send(generation(None))
                .unwrap_or_else(|_| panic!("provider response receiver closed"));
        }
        let expected = if steer_after_acceptance {
            "FOLLOWUP"
        } else {
            "CURRENT"
        };

        let (summaries, timed_out) = registry
            .wait(&session, &[child.agent_id], Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!timed_out);
        assert_eq!(
            summaries[0].status,
            AgentStatus::Completed {
                output: json!({"result":expected})
            }
        );
        // Join event forwarding before inspecting the complete event stream.
        control.close_all(&session).await.unwrap();
        let mut results = 0;
        let expected_results = if steer_after_acceptance { 4 } else { 3 };
        while let Ok(update) = updates.try_recv() {
            if let AgentUpdate::Event { event, .. } = update.update {
                if event.kind == AgentEventKind::ToolResult {
                    let payload: Value = serde_json::from_str(event.payload.get()).unwrap();
                    assert_eq!(payload["status"], "completed", "{payload}");
                    results += 1;
                }
            }
        }
        assert_eq!(results, expected_results);
        parent.shutdown().await.unwrap();
    })
    .await
    .expect("controlled regression must finish without hanging");
}
