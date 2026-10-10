// Public-boundary settle durability: a real parent runtime, Code Mode child,
// journal store and restoration. Only the model provider is controlled.
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use nanocodex_agent::transport::ResponsesTransport;
use nanocodex_agent::{Nanocodex, OpenAi, ResponseError};
use nanocodex_oai_api::{
    responses::{ContentItem, MessageRole, ResponseItem},
    tower::{
        CodeCall, CodeCallKind, GenerationOutput, ResponsePipelineStats, ResponsesAttempt,
        ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
    },
};
use nanocodex_oai_tools::Tools;
use nanocodex_subagents::{
    AgentStatus, AgentTask, MemorySubagentStore, SubagentStore, SubagentStoreFuture, channel,
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

const FINAL_TEXT: &str = "SETTLED_FINAL_TAIL_7f3c";

fn submit_generation(id: &str, result: &str) -> ResponsesOutput {
    let input = format!(
        "const value = await tools.submit_result({});\ntext(typeof value === \"string\" ? value : JSON.stringify(value));",
        json!({"output": result})
    );
    ResponsesOutput::Generation(GenerationOutput {
        id: format!("resp-{id}"),
        reported_model: None,
        status: "completed".to_owned(),
        end_turn: Some(false),
        final_message: None,
        output_items: vec![
            serde_json::from_value(json!({
                "type": "custom_tool_call", "call_id": id, "name": "exec", "input": input,
            }))
            .unwrap(),
        ],
        code_calls: vec![CodeCall {
            call_id: id.to_owned(),
            name: "exec".to_owned(),
            namespace: None,
            input,
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
        id: "resp-final".to_owned(),
        reported_model: None,
        status: "completed".to_owned(),
        end_turn: Some(true),
        final_message: Some(FINAL_TEXT.to_owned()),
        output_items: vec![ResponseItem::message(
            MessageRole::Assistant,
            [ContentItem::output_text(FINAL_TEXT)],
        )],
        code_calls: Vec::new(),
        usage: None,
        time_to_first_event_ns: 0,
        time_to_first_output_ns: None,
        pipeline_stats: ResponsePipelineStats::default(),
    })
}

/// The submit_result receipt the child's next request carries.
fn receipt(input: &[Value], call_id: &str) -> Value {
    let item = input
        .iter()
        .find(|item| item["type"] == "custom_tool_call_output" && item["call_id"] == call_id)
        .expect("the submission receipt reaches the next request");
    let text = match &item["output"] {
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
    };
    serde_json::from_str(text.trim()).unwrap()
}

/// A journal store that fails every save while armed.
#[derive(Default)]
struct Store {
    inner: MemorySubagentStore,
    failing: AtomicBool,
}

impl SubagentStore for Store {
    fn load<'a>(
        &'a self,
        root: &'a str,
    ) -> SubagentStoreFuture<'a, std::io::Result<Option<String>>> {
        self.inner.load(root)
    }
    fn save<'a>(
        &'a self,
        root: &'a str,
        payload: String,
        records: Vec<Arc<str>>,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>> {
        if self.failing.load(Ordering::SeqCst) {
            return Box::pin(async { Err(std::io::Error::other("synthetic store outage")) });
        }
        self.inner.save(root, payload, records)
    }
    fn load_record<'a>(
        &'a self,
        root: &'a str,
        key: &'a str,
    ) -> SubagentStoreFuture<'a, std::io::Result<String>> {
        self.inner.load_record(root, key)
    }
}

struct Journey {
    registry: Arc<nanocodex_subagents::Registry>,
    control: nanocodex_subagents::SubagentControl,
    parent: Nanocodex,
    session: String,
    child: nanocodex_subagents::AgentId,
    generations: mpsc::UnboundedReceiver<PendingGeneration>,
    _updates: mpsc::UnboundedReceiver<nanocodex_subagents::ScopedAgentUpdate>,
}

async fn journey(store: Arc<Store>) -> Journey {
    let (requests, generations) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test-key")
        .transport(ResponsesTransport::Https)
        .service(move || ControlledProvider(requests.clone()))
        .build()
        .unwrap();
    let (registry, control, updates) = channel(1);
    registry.set_store(store);
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
            role: "settle probe".into(),
            task: "Return the requested result".into(),
            output_schema: json!({"type": "string"}),
        },
    )
    .await
    .unwrap()
    .agent_id;
    Journey {
        registry,
        control,
        parent,
        session,
        child,
        generations,
        _updates: updates,
    }
}

// R3: shutdown racing a settled turn finishes promptly (far inside the 30 s
// stop timeout) and its frozen journal holds the turn's final checkpoint.
#[tokio::test]
async fn shutdown_racing_a_turn_settle_finishes_with_the_final_checkpoint() {
    tokio::time::timeout(Duration::from_secs(25), async {
        let store = Arc::new(Store::default());
        let mut journey = journey(Arc::clone(&store)).await;
        let (_, first) = journey.generations.recv().await.unwrap();
        first
            .send(submit_generation("submit", "DONE"))
            .unwrap_or_else(|_| panic!("provider receiver closed"));
        let (input, last) = journey.generations.recv().await.unwrap();
        let accepted = receipt(&input, "submit");
        assert_eq!(accepted["accepted"], json!(true));
        assert_eq!(
            accepted["durable"],
            json!(true),
            "a journaled acceptance is reported durable: {accepted}"
        );
        assert!(accepted.get("note").is_none(), "{accepted}");
        last.send(final_generation())
            .unwrap_or_else(|_| panic!("provider receiver closed"));
        // Completed is visible as soon as the turn settles; its final capture,
        // journal write and announcement are still running when shutdown starts.
        let (summaries, _) = journey
            .registry
            .wait(&journey.session, &[journey.child], Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(
            summaries[0].status,
            AgentStatus::Completed {
                output: json!("DONE")
            }
        );
        let started = Instant::now();
        journey.control.close_all(&journey.session).await.unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "shutdown racing a settle must not wait for the stop timeout: {elapsed:?}"
        );
        let payload: Value =
            serde_json::from_str(&store.load(&journey.session).await.unwrap().unwrap()).unwrap();
        let agent = &payload["agents"][0];
        assert_eq!(agent["status"]["state"], json!("completed"), "{agent}");
        let key = agent["checkpoint_ref"]
            .as_str()
            .unwrap_or_else(|| panic!("the frozen journal references a checkpoint: {agent}"));
        let record = store.load_record(&journey.session, key).await.unwrap();
        assert!(
            record.contains(FINAL_TEXT),
            "the frozen journal's checkpoint holds the settled turn's final tail"
        );
        // The completion is still unacknowledged (no host here): a restore
        // announces it again instead of losing it.
        let (restored, _, _) = channel(1);
        restored.set_store(Arc::clone(&store) as Arc<dyn SubagentStore>);
        let report = restored.restore(&journey.session).await.unwrap();
        assert_eq!(report.completed, vec![journey.child]);
        journey.parent.shutdown().await.unwrap();
    })
    .await
    .expect("shutdown racing a settle must finish without hanging");
}

// R1: an acceptance whose journal write failed stays accepted, tells the
// child plainly that it is not durable and must not be resubmitted, and the
// writer persists it once the store recovers.
#[tokio::test]
async fn unsaved_acceptance_receipt_reaches_the_child_without_resubmission() {
    tokio::time::timeout(Duration::from_secs(25), async {
        let store = Arc::new(Store::default());
        let mut journey = journey(Arc::clone(&store)).await;
        let (_, first) = journey.generations.recv().await.unwrap();
        store.failing.store(true, Ordering::SeqCst);
        first
            .send(submit_generation("submit", "DONE"))
            .unwrap_or_else(|_| panic!("provider receiver closed"));
        let (input, last) = journey.generations.recv().await.unwrap();
        let accepted = receipt(&input, "submit");
        assert_eq!(accepted["accepted"], json!(true), "{accepted}");
        assert_eq!(accepted["status"], json!("accepted"), "{accepted}");
        assert_eq!(accepted["durable"], json!(false), "{accepted}");
        let note = accepted["note"].as_str().unwrap_or_default();
        assert!(
            note.contains("not yet durable") && note.contains("Do not submit again"),
            "{accepted}"
        );
        store.failing.store(false, Ordering::SeqCst);
        last.send(final_generation())
            .unwrap_or_else(|_| panic!("provider receiver closed"));
        let (summaries, _) = journey
            .registry
            .wait(&journey.session, &[journey.child], Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(
            summaries[0].status,
            AgentStatus::Completed {
                output: json!("DONE")
            }
        );
        journey.control.close_all(&journey.session).await.unwrap();
        let payload: Value =
            serde_json::from_str(&store.load(&journey.session).await.unwrap().unwrap()).unwrap();
        assert_eq!(payload["agents"][0]["status"]["state"], json!("completed"));
        journey.parent.shutdown().await.unwrap();
    })
    .await
    .expect("unsaved acceptance journey must finish");
}
