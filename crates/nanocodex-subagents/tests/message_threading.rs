//! Public-tool journeys: only the external model provider is synthetic. Messages
//! run through installed tools, real agent turns, routing, and delivery events.
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use nanocodex_agent::{Nanocodex, OpenAi, ResponseError, transport::ResponsesTransport};
use nanocodex_oai_api::{
    responses::{ContentItem, MessageRole, ResponseItem},
    tower::{
        CodeCall, CodeCallKind, GenerationOutput, ResponsePipelineStats, ResponsesAttempt,
        ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
    },
};
use nanocodex_oai_tools::Tools;
use nanocodex_subagents::{
    AgentId, AgentMessage, AgentStatus, AgentTask, AgentUpdate, MessageDeliveryState,
    MessageDisposition, MessagePurpose, Registry, ScopedAgentUpdate, SubagentControl, channel,
    install_tools, start_agent,
};
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

// Agents are Code Mode only since eda4a21e3: the model reaches tools through exec.
fn exec_input(name: &str, arguments: &str) -> String {
    format!(
        r#"const value = await tools.{name}({arguments});
text(typeof value === "string" ? value : JSON.stringify(value));"#
    )
}

fn generation(call: Option<(&str, &str, Value)>) -> ResponsesOutput {
    let (id, output_items, code_calls) = match call {
        Some((id, name, arguments)) => {
            let input = exec_input(name, &arguments.to_string());
            (
                id.to_owned(),
                vec![
                    serde_json::from_value(json!({
                        "type": "custom_tool_call", "call_id": id, "name": "exec",
                        "input": input,
                    }))
                    .unwrap(),
                ],
                vec![CodeCall {
                    call_id: id.to_owned(),
                    name: "exec".to_owned(),
                    namespace: None,
                    input,
                    kind: CodeCallKind::Custom,
                }],
            )
        }
        None => (
            "final".to_owned(),
            vec![ResponseItem::message(
                MessageRole::Assistant,
                [ContentItem::output_text("Coordination complete.")],
            )],
            Vec::new(),
        ),
    };
    let finished = code_calls.is_empty();
    ResponsesOutput::Generation(GenerationOutput {
        id: format!("resp-{id}"),
        reported_model: None,
        status: "completed".to_owned(),
        end_turn: Some(finished),
        final_message: finished.then(|| "Coordination complete.".to_owned()),
        output_items,
        code_calls,
        usage: None,
        time_to_first_event_ns: 0,
        time_to_first_output_ns: None,
        pipeline_stats: ResponsePipelineStats::default(),
    })
}

struct Journey {
    parent: Nanocodex,
    registry: Arc<Registry>,
    control: SubagentControl,
    session: String,
    generations: mpsc::UnboundedReceiver<PendingGeneration>,
    updates: mpsc::UnboundedReceiver<ScopedAgentUpdate>,
    handle: nanocodex_agent::AgentHandle,
    references: Vec<(Value, Value)>,
}

impl Journey {
    fn new() -> Self {
        let (requests, generations) = mpsc::unbounded_channel();
        let openai = OpenAi::builder("synthetic-test-key")
            .transport(ResponsesTransport::Https)
            .service(move || ControlledProvider(requests.clone()))
            .build()
            .unwrap();
        let (registry, control, updates) = channel(4);
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
        Self {
            parent,
            registry,
            control,
            session,
            generations,
            updates,
            handle,
            references: Vec::new(),
        }
    }

    async fn child(&mut self, task: &str) -> (AgentId, PendingGeneration) {
        let child = start_agent(
            &self.handle,
            &self.registry,
            &self.session,
            AgentTask {
                role: task.to_owned(),
                task: task.to_owned(),
                output_schema: json!({"type":"string"}),
            },
        )
        .await
        .unwrap();
        let pending = self.next().await;
        assert!(serde_json::to_string(&pending.0).unwrap().contains(task));
        (child.agent_id, pending)
    }

    async fn next(&mut self) -> PendingGeneration {
        tokio::time::timeout(Duration::from_secs(5), self.generations.recv())
            .await
            .expect("next model request must arrive")
            .expect("provider remains connected")
    }

    // Other model requests stay held at their safe boundary, so the next request
    // is from the actor whose synthetic response we just released, not a timer.
    async fn tool(
        &mut self,
        pending: PendingGeneration,
        id: &str,
        name: &str,
        args: Value,
    ) -> PendingGeneration {
        println!("CALL {id} {name} {args}");
        pending
            .1
            .send(generation(Some((id, name, args))))
            .unwrap_or_else(|_| panic!("model receiver closed for {id}"));
        let next = self.next().await;
        println!("RESULT {id} {}", tool_output(&next.0, id));
        next
    }

    async fn send(
        &mut self,
        pending: PendingGeneration,
        id: &str,
        args: Value,
    ) -> (PendingGeneration, Value) {
        let target = args["agent_id"].clone();
        let reference = args["in_reply_to"].clone();
        let next = self.tool(pending, id, "send_agent_message", args).await;
        let receipt: Value = serde_json::from_str(&tool_output(&next.0, id)).unwrap();
        assert!(receipt["message_id"].is_number(), "{id}: {receipt}");
        assert_eq!(receipt["to_agent_id"], target);
        assert_eq!(receipt["disposition"], "steered");
        self.references
            .push((receipt["message_id"].clone(), reference));
        (next, receipt)
    }

    async fn reject(
        &mut self,
        pending: PendingGeneration,
        id: &str,
        args: Value,
        reason: &str,
    ) -> PendingGeneration {
        let next = self.tool(pending, id, "send_agent_message", args).await;
        let output = tool_output(&next.0, id);
        assert!(
            output.contains(reason),
            "{id}: expected {reason:?}, got {output}"
        );
        next
    }

    async fn finish_child(&mut self, pending: PendingGeneration, call_id: &str) {
        let next = self
            .tool(pending, call_id, "submit_result", json!({"output":"done"}))
            .await;
        let receipt: Value = serde_json::from_str(&tool_output(&next.0, call_id)).unwrap();
        assert_eq!(receipt, json!({"accepted":true, "status":"accepted"}));
        next.1
            .send(generation(None))
            .unwrap_or_else(|_| panic!("final receiver closed"));
    }

    async fn finish(mut self, ids: &[AgentId], expected: &[(&Value, MessagePurpose, &str)]) {
        for id in ids {
            let (summary, timed_out) = self
                .registry
                .wait(&self.session, &[*id], Duration::from_secs(5))
                .await
                .unwrap();
            assert!(!timed_out);
            assert_eq!(
                summary[0].status,
                AgentStatus::Completed {
                    output: json!("done")
                }
            );
        }
        // Join forwarding before collecting the full public event trace.
        self.control.close_all(&self.session).await.unwrap();
        // Urgent steering emits Admitted(Steered), not Delivered. Actual
        // recipient model inputs above establish consumption at a safe boundary.
        let mut admitted = Vec::<AgentMessage>::new();
        while let Ok(update) = self.updates.try_recv() {
            if let AgentUpdate::Message(message_update) = update.update {
                println!(
                    "MESSAGE {}",
                    serde_json::to_string(&message_update).unwrap()
                );
                match message_update.delivery {
                    MessageDeliveryState::Admitted { disposition } => {
                        assert_eq!(disposition, MessageDisposition::Steered);
                        admitted.push(
                            message_update
                                .thread
                                .messages
                                .into_iter()
                                .find(|message| message.id == message_update.message_id)
                                .unwrap(),
                        );
                    }
                    MessageDeliveryState::Delivered { .. } => {}
                    MessageDeliveryState::Failed { error } => {
                        panic!("unexpected delivery failure: {error}")
                    }
                }
            }
        }
        assert_eq!(
            admitted.len(),
            expected.len(),
            "only valid sends are admitted"
        );
        for (receipt, purpose, body) in expected {
            let message = admitted
                .iter()
                .find(|message| json!(message.id) == receipt["message_id"])
                .expect("accepted send must have a public admitted event");
            assert_eq!(json!(message.thread_id), receipt["thread_id"]);
            assert_eq!(json!(message.from), receipt["from"]);
            assert_eq!(json!(message.to), receipt["to_agent_id"]);
            assert_eq!(message.purpose, *purpose);
            assert_eq!(message.body, *body);
            let (_, reference) = self
                .references
                .iter()
                .find(|(id, _)| id == &receipt["message_id"])
                .unwrap();
            assert_eq!(json!(message.in_reply_to), *reference);
        }
        self.parent.shutdown().await.unwrap();
    }
}

fn tool_output(input: &[Value], id: &str) -> String {
    exec_text(
        input
            .iter()
            .find(|item| item["type"] == "custom_tool_call_output" && item["call_id"] == id)
            .unwrap_or_else(|| {
                panic!("{id}: tool output missing from actual model input: {input:?}")
            }),
    )
}
/// The text a Code Mode cell emitted, without its status/wall-time header.
fn exec_text(item: &Value) -> String {
    match &item["output"] {
        Value::String(text) => text
            .split_once("Output:\n")
            .map_or(text.as_str(), |(_, rest)| rest)
            .to_owned(),
        Value::Array(content) => content
            .iter()
            .skip(1)
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("unexpected exec output {other}"),
    }
}

fn assert_delivery_input(input: &[Value], bodies: &[&str]) {
    // Look only at real user messages, never assistant tool arguments/history.
    let user_messages: Vec<_> = input.iter().filter(|item| item["role"] == "user").collect();
    let text = serde_json::to_string(&user_messages).unwrap();
    for body in bodies {
        assert!(
            text.contains(body),
            "delivered body {body:?} missing from model input: {text}"
        );
        println!("MODEL received {body:?}");
    }
}

#[tokio::test]
async fn root_followup_keeps_default_coordinate_and_authorized_delegate_keeps_its_purpose() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut journey = Journey::new();
        let (child, child_pending) = journey.child("Audit the storage migration.").await;
        let root_turn = journey
            .parent
            .prompt("Coordinate the migration audit.")
            .await
            .unwrap();
        let root = journey.next().await;
        let (root, first) = journey
            .send(
                root,
                "root-initial",
                json!({
                    "agent_id":child, "priority":"urgent", "message":"Check migration ordering."
                }),
            )
            .await;
        let (root, followup) = journey
            .send(
                root,
                "root-followup",
                json!({
                    "agent_id":child, "priority":"urgent", "in_reply_to":first["message_id"],
                    "message":"Also check the rollback ordering."
                }),
            )
            .await;
        assert_eq!(followup["thread_id"], first["thread_id"]);
        let (root, delegated) = journey
            .send(
                root,
                "root-delegate",
                json!({
                    "agent_id":child, "priority":"urgent", "purpose":"delegate",
                    "in_reply_to":followup["message_id"], "message":"Audit rollback ordering only."
                }),
            )
            .await;
        assert_eq!(delegated["thread_id"], first["thread_id"]);
        let root = journey
            .reject(
                root,
                "root-unknown",
                json!({
                    "agent_id":child, "in_reply_to":999999, "message":"Not a known thread."
                }),
                "unknown in_reply_to",
            )
            .await;
        root.1
            .send(generation(None))
            .unwrap_or_else(|_| panic!("root receiver closed"));
        root_turn.result().await.unwrap();

        // Flush steering at the real model boundary before submitting a result.
        let child_pending = journey
            .tool(
                child_pending,
                "inspect-assignment",
                "list_agents",
                json!({"include_self":true}),
            )
            .await;
        assert_delivery_input(
            &child_pending.0,
            &[
                "Check migration ordering.",
                "Also check the rollback ordering.",
                "Audit rollback ordering only.",
            ],
        );
        let directory: Value =
            serde_json::from_str(&tool_output(&child_pending.0, "inspect-assignment")).unwrap();
        let entry = directory["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["agent_id"] == json!(child))
            .unwrap();
        assert_eq!(entry["task"], "Audit rollback ordering only.");
        journey.finish_child(child_pending, "audit-result").await;
        journey
            .finish(
                &[child],
                &[
                    (
                        &first,
                        MessagePurpose::Coordinate,
                        "Check migration ordering.",
                    ),
                    (
                        &followup,
                        MessagePurpose::Coordinate,
                        "Also check the rollback ordering.",
                    ),
                    (
                        &delegated,
                        MessagePurpose::Delegate,
                        "Audit rollback ordering only.",
                    ),
                ],
            )
            .await;
    })
    .await
    .expect("root journey must finish without hanging");
}

#[tokio::test]
async fn siblings_continue_a_question_with_findings_but_reply_and_authority_checks_remain() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut journey = Journey::new();
        let (engineer, engineer_pending) = journey.child("Implement the storage migration.").await;
        let (reviewer, reviewer_pending) = journey.child("Review migration safety.").await;
        let (observer, observer_pending) = journey.child("Observe an unrelated UI change.").await;
        let (engineer_pending, question) = journey.send(engineer_pending, "ask-reviewer", json!({
            "agent_id":reviewer, "priority":"urgent", "purpose":"question",
            "message":"Is the rollback safe?"
        })).await;
        let (reviewer_pending, finding) = journey.send(reviewer_pending, "reviewer-finding", json!({
            "agent_id":engineer, "priority":"urgent", "purpose":"finding",
            "in_reply_to":question["message_id"], "message":"Rollback must restore the old index."
        })).await;
        assert_delivery_input(&reviewer_pending.0, &["Is the rollback safe?"]);
        let (reviewer_pending, coordinate) = journey.send(reviewer_pending, "reviewer-coordinate", json!({
            "agent_id":engineer, "priority":"urgent", "purpose":"coordinate",
            "in_reply_to":question["message_id"], "message":"Keep the old index until verification."
        })).await;
        for receipt in [&finding, &coordinate] {
            assert_eq!(receipt["thread_id"], question["thread_id"]);
        }
        let (engineer_pending, reply) = journey.send(engineer_pending, "engineer-reply", json!({
            "agent_id":reviewer, "priority":"urgent", "purpose":"reply",
            "in_reply_to":finding["message_id"], "message":"I will preserve the old index."
        })).await;
        assert_eq!(reply["thread_id"], question["thread_id"]);
        assert_delivery_input(&engineer_pending.0, &[
            "Rollback must restore the old index.", "Keep the old index until verification."
        ]);
        let engineer_pending = journey.reject(engineer_pending, "wrong-reply-direction", json!({
            "agent_id":reviewer, "purpose":"reply", "in_reply_to":question["message_id"],
            "message":"Cannot reply to my own outgoing question."
        }), "only be answered by its recipient").await;
        let engineer_pending = journey.reject(engineer_pending, "missing-reply-reference", json!({
            "agent_id":reviewer, "purpose":"reply", "message":"Reply without a reference."
        }), "require an in_reply_to").await;
        let engineer_pending = journey.reject(engineer_pending, "different-recipient", json!({
            "agent_id":observer, "in_reply_to":question["message_id"],
            "message":"Must not redirect this thread to the UI observer."
        }), "different two-party thread").await;
        let observer_pending = journey.reject(observer_pending, "unrelated-sender", json!({
            "agent_id":reviewer, "purpose":"finding", "in_reply_to":question["message_id"],
            "message":"Must not join another pair's thread."
        }), "different two-party thread").await;
        let reviewer_pending = journey.reject(reviewer_pending, "sibling-delegate", json!({
            "agent_id":engineer, "priority":"urgent", "purpose":"delegate",
            "in_reply_to":question["message_id"], "message":"Replace the engineer's task without permission."
        }), "may only manage its descendants").await;
        assert_delivery_input(&reviewer_pending.0, &["I will preserve the old index."]);
        // The rejected delegate must not change the public assignment.
        let reviewer_pending = journey.tool(reviewer_pending, "inspect-after-rejection", "list_agents",
            json!({"include_self":true})).await;
        let directory: Value = serde_json::from_str(&tool_output(&reviewer_pending.0, "inspect-after-rejection")).unwrap();
        let entry = directory["agents"].as_array().unwrap().iter()
            .find(|entry| entry["agent_id"] == json!(engineer)).unwrap();
        assert_eq!(entry["task"], "Implement the storage migration.");

        journey.finish_child(engineer_pending, "engineer-result").await;
        journey.finish_child(reviewer_pending, "reviewer-result").await;
        journey.finish_child(observer_pending, "observer-result").await;
        journey.finish(&[engineer, reviewer, observer], &[
            (&question, MessagePurpose::Question, "Is the rollback safe?"),
            (&finding, MessagePurpose::Finding, "Rollback must restore the old index."),
            (&coordinate, MessagePurpose::Coordinate, "Keep the old index until verification."),
            (&reply, MessagePurpose::Reply, "I will preserve the old index."),
        ]).await;
    }).await.expect("sibling journey must finish without hanging");
}

// Use a persisted journal as the recovery input, then exercise the installed
// list_agents tool through real model/tool dispatch. Holding resume until after
// listing deterministically models the ready-before-resume window, without sleeps.
#[tokio::test]
async fn restored_children_remain_discoverable_before_and_after_failed_resume() {
    use nanocodex_subagents::{MemorySubagentStore, SubagentStore};
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut journey = Journey::new();
        let mut agents: Vec<Value> = (1..=6)
            .map(|id| {
                json!({
                    "descriptor": {"id": id, "session_id": format!("restored-{id}"),
                        "role": format!("worker-{id}"), "task": "retained task",
                        "parent": if id == 2 { Some(1) } else { None }},
                    "status": {"state":"running"}, "turn_in_flight": true,
                    "output_schema": {"type":"string"},
                    "native_checkpoint": {"model":"claude-haiku-4-5",
                        "session_id": format!("restored-{id}"), "thinking":"none",
                        "payload":"{\"messages\":[]}", "has_conversation":true}
                })
            })
            .collect();
        for (id, status) in [
            (7, json!({"state":"completed","output":"done"})),
            (8, json!({"state":"failed","error":"retained failure"})),
            (9, json!({"state":"closed"})),
            (10, json!({"state":"interrupted"})),
        ] {
            agents.push(json!({
                "descriptor":{"id":id,"session_id":format!("retained-{id}"),
                    "role":"retained","task":"retained task","parent":null},
                "status":status, "output_schema":{"type":"string"}
            }));
        }
        let store = MemorySubagentStore::new();
        store
            .save(
                &journey.session,
                json!({"version":1,"agents":agents}).to_string(),
                Vec::new(),
            )
            .await
            .unwrap();
        journey.registry.set_store(Arc::new(store));
        let report = journey.registry.restore(&journey.session).await.unwrap();
        assert_eq!(report.restored, 10);
        assert_eq!(report.interrupted.len(), 6);
        let turn = journey
            .parent
            .prompt("Inspect the recovered directory.")
            .await
            .unwrap();
        let pending = journey.next().await;
        let pending = journey
            .tool(
                pending,
                "restored-default",
                "list_agents",
                json!({"include_completed":false}),
            )
            .await;
        let listed: Value =
            serde_json::from_str(&tool_output(&pending.0, "restored-default")).unwrap();
        let entries = listed["agents"].as_array().unwrap();
        assert_eq!(entries.len(), 6);
        for (index, entry) in entries.iter().enumerate() {
            assert_eq!(entry["agent_id"], index + 1);
            assert_eq!(entry["status"]["state"], "interrupted");
            assert_eq!(entry["can_message"], true);
            assert_eq!(entry["can_manage"], true);
        }
        assert_eq!(entries[1]["parent_agent_id"], 1);
        let pending = journey
            .tool(
                pending,
                "restored-all",
                "list_agents",
                json!({"include_completed":true}),
            )
            .await;
        let all: Value = serde_json::from_str(&tool_output(&pending.0, "restored-all")).unwrap();
        assert_eq!(all["agents"].as_array().unwrap().len(), 10);
        assert_eq!(&all["agents"].as_array().unwrap()[..6], entries.as_slice());

        // Public directory permissions remain relative to the caller's tree.
        let child_view = journey
            .registry
            .directory("restored-1", false, false)
            .await
            .unwrap();
        assert_eq!(child_view.len(), 5);
        assert!(
            child_view
                .iter()
                .find(|e| e.agent_id.to_string() == "2")
                .unwrap()
                .can_manage
        );
        assert!(
            !child_view
                .iter()
                .find(|e| e.agent_id.to_string() == "3")
                .unwrap()
                .can_manage
        );
        let self_view = journey
            .registry
            .directory("restored-1", false, true)
            .await
            .unwrap();
        assert!(!self_view[0].can_message && !self_view[0].can_manage);
        assert!(
            journey
                .registry
                .directory("unrelated-root", true, true)
                .await
                .unwrap()
                .is_empty()
        );
        let sibling: AgentId = "3".parse().unwrap();
        assert!(journey.registry.close("restored-1", sibling).await.is_err());

        // A fresh registry has no root factory, so resume deterministically fails
        // before any provider call. No replacement children are started.
        let payload = json!({"version":1,"agents":agents}).to_string();
        let store = MemorySubagentStore::new();
        store
            .save("unavailable-root", payload, Vec::new())
            .await
            .unwrap();
        let (recovered, _, _) = channel(8);
        recovered.set_store(Arc::new(store));
        recovered.restore("unavailable-root").await.unwrap();
        let results = recovered.resume_interrupted("unavailable-root").await;
        assert_eq!(results.len(), 6);
        for (id, result) in results {
            let error = result.unwrap_err();
            println!("RESUME {id} failed: {error}");
        }
        let after = recovered
            .directory("unavailable-root", false, false)
            .await
            .unwrap();
        assert_eq!(after.len(), 6);
        assert!(after.iter().all(|e| e.status == AgentStatus::Interrupted));
        println!(
            "AFTER failed resume {}",
            serde_json::to_string(&after).unwrap()
        );
        pending
            .1
            .send(generation(None))
            .unwrap_or_else(|_| panic!("root closed"));
        turn.result().await.unwrap();
        journey.parent.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}
