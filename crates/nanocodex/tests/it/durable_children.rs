//! Public native facade journeys. Only the HTTP model is a fixture: durable
//! builders, native descendants, tool transport and lifetime barriers are real.
use axum::{Json, Router, routing::post};
use nanocodex::{
    DurableAgentExt, Nanocodex, OpenAi, PromptRequest,
    durability::{DurableSession, SqliteStore},
    oai::transport::ResponsesTransport,
    tools::Tools,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;

const DEADLINE: Duration = Duration::from_secs(20);

struct Fixture {
    base: String,
    requests: Arc<Mutex<Vec<Value>>>,
    child_started: Arc<Notify>,
    child_release: Arc<Notify>,
    parent_finished: Arc<Notify>,
    child_finished: Arc<Notify>,
    claude: bool,
    preserve_workspace: bool,
    server: tokio::task::JoinHandle<()>,
    workspace: PathBuf,
}

fn latest_prompt(body: &Value) -> String {
    let messages = body.get("input").unwrap_or(&body["messages"]);
    messages
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|item| item["role"] == "user" && !item["content"].to_string().contains("tool_result"))
        .map(|item| item["content"].to_string())
        .unwrap_or_default()
}

fn function(name: &str, arguments: Value, ordinal: usize) -> Value {
    json!([{"type":"function_call", "call_id":format!("native-call-{ordinal}"),
        "name":name, "arguments":arguments.to_string()}])
}

fn message(text: impl Into<String>) -> Value {
    json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":text.into()}]}])
}

impl Fixture {
    async fn start_family(label: &str, claude: bool, path: Option<PathBuf>) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let child_started = Arc::new(Notify::new());
        let child_release = Arc::new(Notify::new());
        let parent_finished = Arc::new(Notify::new());
        let child_finished = Arc::new(Notify::new());
        let preserve_workspace = path.is_some();
        let workspace = path.unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "native-facade-{label}-{}",
                nanocodex::agent::session::SessionId::new()
            ))
        });
        std::fs::create_dir_all(&workspace).unwrap();
        let trace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../output/native-{label}-{}-{}.json",
            if claude { "claude" } else { "oai" },
            std::process::id()
        ));
        std::fs::create_dir_all(trace.parent().unwrap()).unwrap();
        println!("HTTP_TRANSCRIPT {}", trace.display());
        let handler = post({
            let requests = requests.clone();
            let started = child_started.clone();
            let release = child_release.clone();
            let parent_finished = parent_finished.clone();
            let child_finished = child_finished.clone();
            move |Json(body): Json<Value>| {
                let requests = requests.clone();
                let started = started.clone();
                let release = release.clone();
                let parent_finished = parent_finished.clone();
                let child_finished = child_finished.clone();
                let trace = trace.clone();
                async move {
                    let ordinal = {
                        let mut requests = requests.lock().unwrap();
                        requests.push(body.clone());
                        std::fs::write(&trace, serde_json::to_vec_pretty(&*requests).unwrap())
                            .unwrap();
                        requests.len()
                    };
                    let prompt = latest_prompt(&body);
                    let messages = body
                        .get("input")
                        .unwrap_or(&body["messages"])
                        .as_array()
                        .unwrap();
                    let last = messages.last().unwrap();
                    let continuation = last["type"] == "function_call_output"
                        || last["content"][0]["type"] == "tool_result";
                    let output = if continuation {
                        if prompt.contains("spawn-") {
                            parent_finished.notify_one();
                        }
                        if prompt.contains("gated-child") {
                            child_finished.notify_one();
                        }
                        let reply = last.get("output").unwrap_or(&last["content"][0]["content"]);
                        let reply = reply.as_str().map(str::to_owned).unwrap_or_else(|| {
                            reply
                                .as_array()
                                .map(|blocks| {
                                    blocks
                                        .iter()
                                        .filter_map(|block| block["text"].as_str())
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                                .unwrap_or_else(|| reply.to_string())
                        });
                        message(reply)
                    } else if prompt.contains("gated-child") {
                        started.notify_one();
                        release.notified().await;
                        function(
                            "submit_result",
                            json!({"output":"native-child-result"}),
                            ordinal,
                        )
                    } else if prompt.contains("spawn-foreground")
                        || prompt.contains("spawn-background")
                    {
                        let background = prompt.contains("spawn-background");
                        function(
                            "spawn_agent",
                            json!({
                                "role":"native specialist", "task":"gated-child",
                                "harness":null,"model":null,"thinking":null,
                                "lifetime":if background {"background"} else {"foreground"},
                                "output_contract":{"kind":"string"}
                            }),
                            ordinal,
                        )
                    } else if prompt.contains("queue-followup") {
                        function(
                            "send_agent_message",
                            json!({
                                "agent_id":1, "message":"gated-child-followup",
                                "priority":"deferred", "purpose":"coordinate"
                            }),
                            ordinal,
                        )
                    } else if prompt.contains("directory") {
                        function(
                            "list_agents",
                            json!({"include_completed":true,"include_self":false}),
                            ordinal,
                        )
                    } else if prompt.contains("wait-background") {
                        function(
                            "wait_agent",
                            json!({"agent_ids":[2],"timeout_ms":10000}),
                            ordinal,
                        )
                    } else {
                        message("durable native answer")
                    };
                    let stream = if claude {
                        let item = &output[0];
                        let (content, stop) = if item["type"] == "function_call" {
                            (
                                json!({"type":"tool_use","id":item["call_id"],"name":item["name"],"input":serde_json::from_str::<Value>(item["arguments"].as_str().unwrap()).unwrap()}),
                                "tool_use",
                            )
                        } else {
                            (
                                json!({"type":"text","text":item["content"][0]["text"]}),
                                "end_turn",
                            )
                        };
                        [
                            json!({"type":"message_start","message":{"id":format!("message-{ordinal}"),"role":"assistant","model":"claude-sonnet-5-5","content":[],"usage":{"input_tokens":5,"output_tokens":0}}}),
                            json!({"type":"content_block_start","index":0,"content_block":content}),
                            json!({"type":"content_block_stop","index":0}),
                            json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":3}}),
                            json!({"type":"message_stop"}),
                        ].into_iter().map(|frame| format!("data: {frame}\n\n")).collect()
                    } else {
                        let frame = json!({"type":"response.completed", "response":{"id":format!("response-{ordinal}"),"status":"completed","output":output}});
                        format!("data: {frame}\n\ndata: [DONE]\n\n")
                    };
                    ([("content-type", "text/event-stream")], stream)
                }
            }
        });
        let app = Router::new()
            .route("/responses", handler.clone())
            .route("/v1/messages", handler);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base: format!("http://{address}"),
            requests,
            child_started,
            child_release,
            parent_finished,
            child_finished,
            claude,
            preserve_workspace,
            server,
            workspace,
        }
    }

    fn openai(&self) -> OpenAi {
        OpenAi::builder("synthetic-key")
            .transport(ResponsesTransport::Https)
            .websocket_warmup(false)
            .store(false)
            .api_base_url(&self.base)
            .build()
            .unwrap()
    }

    async fn parent(&self) -> (Nanocodex, nanocodex::AgentEvents) {
        let state = DurableSession::open(
            SqliteStore::open(self.workspace.join("children.sqlite")).unwrap(),
            "configured-native-root",
        )
        .await
        .unwrap();
        self.parent_with_state(state).await
    }

    async fn parent_with_state(
        &self,
        state: DurableSession,
    ) -> (Nanocodex, nanocodex::AgentEvents) {
        self.parent_capturing_handle(state, None).await
    }

    async fn parent_capturing_handle(
        &self,
        state: DurableSession,
        capture: Option<tokio::sync::oneshot::Sender<nanocodex::agent::AgentHandle>>,
    ) -> (Nanocodex, nanocodex::AgentEvents) {
        let capture = Arc::new(Mutex::new(capture));
        #[cfg(feature = "claude")]
        if self.claude {
            let client = nanocodex::claude::ClaudeClient::new(
                reqwest::Client::new(),
                format!("{}/v1/messages", self.base),
                "synthetic-key",
            );
            return Nanocodex::builder(nanocodex::Claude::new(client, "claude-sonnet-5-5"))
                .map_tools_factory(move |handle, tools| {
                    if let Some(capture) = capture.lock().unwrap().take() {
                        let _ = capture.send(handle);
                    }
                    Ok(tools)
                })
                .durability(state)
                .await
                .unwrap()
                .build()
                .unwrap();
        }
        Nanocodex::builder(self.openai())
            .workspace(&self.workspace)
            .tools(Tools::builder().without_defaults().build().unwrap())
            .map_tools_factory(move |handle, tools| {
                if let Some(capture) = capture.lock().unwrap().take() {
                    let _ = capture.send(handle);
                }
                Ok(tools)
            })
            .durability(state)
            .await
            .unwrap()
            .build()
            .unwrap()
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        if !self.preserve_workspace {
            let _ = std::fs::remove_dir_all(&self.workspace);
        }
    }
}

async fn answer(agent: &Nanocodex, input: &str, id: &str) -> String {
    tokio::time::timeout(DEADLINE, async {
        agent
            .prompt(PromptRequest::new(input).request_id(id))
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message()
            .to_owned()
    })
    .await
    .expect("native facade prompt deadline")
}

#[tokio::test]
async fn configured_native_parent_own_spawn_and_descendant_replay_requests() {
    replay_journey(false).await;
}

#[cfg(feature = "claude")]
#[tokio::test]
async fn configured_claude_parent_owns_same_family_children_and_descendant_replay() {
    replay_journey(true).await;
}

async fn replay_journey(claude: bool) {
    let fixture = Fixture::start_family("facade-replay", claude, None).await;
    let (parent, parent_events) = fixture.parent().await;
    let root_session = parent.session_id().to_owned();
    let (child, child_events) = parent.spawn().await.unwrap();
    let (descendant, descendant_events) = child.spawn().await.unwrap();
    assert_ne!(parent.session_id(), child.session_id());
    assert_ne!(child.session_id(), descendant.session_id());
    for (agent, marker) in [
        (&parent, "root-own-request"),
        (&child, "child-own-request"),
        (&descendant, "descendant-own-request"),
    ] {
        assert_eq!(
            answer(agent, marker, "shared-request-id").await,
            "durable native answer"
        );
        let before = fixture.calls();
        assert_eq!(
            answer(agent, marker, "shared-request-id").await,
            "durable native answer"
        );
        assert_eq!(
            fixture.calls(),
            before,
            "{marker} replay must not call HTTP model"
        );
    }
    assert_eq!(
        fixture.calls(),
        3,
        "each session has an independent durable request identity"
    );
    descendant.shutdown().await.unwrap();
    child.shutdown().await.unwrap();
    parent.shutdown().await.unwrap();
    drop((
        parent,
        parent_events,
        child,
        child_events,
        descendant,
        descendant_events,
    ));
    // Fresh SQLite connection and facade builder; no live durable/session/registry
    // object is reused. The non-UUID host key must restore the native identity.
    let (reopened, reopened_events) = fixture.parent().await;
    assert_eq!(reopened.session_id(), root_session);
    let before = fixture.calls();
    assert_eq!(
        answer(&reopened, "root-own-request", "shared-request-id").await,
        "durable native answer"
    );
    assert_eq!(
        fixture.calls(),
        before,
        "cold root replay must not call HTTP model"
    );
    println!(
        "NATIVE_REPLAY parent.spawn + child.spawn: 3 independent HTTP requests, zero replay requests; SQLite cold root identity/receipt retained"
    );
    reopened.shutdown().await.unwrap();
    drop((reopened, reopened_events));
}

#[tokio::test]
async fn configured_native_registry_tools_hold_foreground_and_allow_background_then_reopen() {
    lifetime_journey(false).await;
}

#[cfg(feature = "claude")]
#[tokio::test]
async fn configured_claude_registry_holds_foreground_and_reopens_background_outputs() {
    lifetime_journey(true).await;
}

async fn lifetime_journey(claude: bool) {
    let fixture = Fixture::start_family("facade-lifetime", claude, None).await;
    let (parent, parent_events) = fixture.parent().await;
    let root_session = parent.session_id().to_owned();
    let foreground = parent
        .prompt(PromptRequest::new("spawn-foreground").request_id("foreground"))
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, fixture.child_started.notified())
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, fixture.parent_finished.notified())
        .await
        .unwrap();
    let mut foreground_result = Box::pin(foreground.result());
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut foreground_result)
            .await
            .is_err(),
        "parent model finished but foreground native child must hold completion"
    );
    fixture.child_release.notify_one();
    let receipt = tokio::time::timeout(DEADLINE, foreground_result)
        .await
        .unwrap()
        .unwrap();
    assert!(receipt.final_message().contains("agent_id"));
    let directory: Value =
        serde_json::from_str(&answer(&parent, "directory", "directory-before-background").await)
            .unwrap();
    assert_eq!(directory["agents"].as_array().unwrap().len(), 1);
    assert_eq!(directory["agents"][0]["status"]["state"], "completed");
    assert_eq!(
        directory["agents"][0]["status"]["output"],
        "native-child-result"
    );
    assert_eq!(directory["agents"][0]["lifetime"], "foreground");

    let background = parent
        .prompt(PromptRequest::new("spawn-background").request_id("background"))
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, fixture.child_started.notified())
        .await
        .unwrap();
    let receipt = tokio::time::timeout(DEADLINE, background.result())
        .await
        .expect("background child must allow parent completion")
        .unwrap();
    assert!(receipt.final_message().contains("agent_id"));
    let directory: Value =
        serde_json::from_str(&answer(&parent, "directory", "directory-running-background").await)
            .unwrap();
    let background = directory["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == 2)
        .unwrap();
    assert_eq!(background["lifetime"], "background");
    assert!(matches!(
        background["status"]["state"].as_str(),
        Some("running" | "pending")
    ));
    fixture.child_release.notify_one();
    let completed: Value =
        serde_json::from_str(&answer(&parent, "wait-background", "wait-background").await).unwrap();
    assert_eq!(completed["agents"][0]["status"]["state"], "completed");
    assert_eq!(
        completed["agents"][0]["status"]["output"],
        "native-child-result"
    );
    let calls_before = fixture.calls();
    let replay = answer(&parent, "spawn-background", "background").await;
    assert!(replay.contains("agent_id"));
    assert_eq!(
        fixture.calls(),
        calls_before,
        "spawn receipt request replay must not rerun HTTP or create a child"
    );
    parent.shutdown().await.unwrap();
    drop((parent, parent_events));
    let (reopened, reopened_events) = fixture.parent().await;
    assert_eq!(reopened.session_id(), root_session);
    let calls_before = fixture.calls();
    assert_eq!(
        answer(&reopened, "spawn-background", "background").await,
        replay
    );
    assert_eq!(
        fixture.calls(),
        calls_before,
        "cold parent spawn receipt replay is provider-free"
    );
    let restored: Value =
        serde_json::from_str(&answer(&reopened, "directory", "directory-after-reopen").await)
            .unwrap();
    assert_eq!(restored, directory_completed(&directory));
    println!(
        "NATIVE_LIFETIME real spawn_agent/list_agents/wait_agent/submit_result transport: foreground held after parent model terminal; background running after parent terminal; both completed outputs and topology retained on fresh SQLite/facade reopen"
    );
    reopened.shutdown().await.unwrap();
    drop((reopened, reopened_events));
}

#[tokio::test]
async fn native_background_child_survives_parent_shutdown() {
    background_departure(false).await;
}

#[cfg(feature = "claude")]
#[tokio::test]
async fn claude_background_child_survives_parent_shutdown() {
    background_departure(true).await;
}

async fn background_departure(claude: bool) {
    child_owner_departure(claude, true, true, false).await;
}

#[tokio::test]
async fn native_completed_child_drop_releases_owner() {
    child_owner_departure(false, false, false, false).await;
}
#[tokio::test]
async fn native_completed_child_shutdown_releases_owner() {
    child_owner_departure(false, true, false, false).await;
}
#[tokio::test]
async fn native_background_child_drop_releases_owner_after_settlement() {
    child_owner_departure(false, false, true, false).await;
}
#[cfg(feature = "claude")]
#[tokio::test]
async fn claude_completed_child_drop_releases_owner() {
    child_owner_departure(true, false, false, false).await;
}
#[cfg(feature = "claude")]
#[tokio::test]
async fn claude_completed_child_shutdown_releases_owner() {
    child_owner_departure(true, true, false, false).await;
}
#[cfg(feature = "claude")]
#[tokio::test]
async fn claude_background_child_drop_releases_owner_after_settlement() {
    child_owner_departure(true, false, true, false).await;
}

#[tokio::test]
async fn native_queued_child_work_retains_owner_until_drained() {
    child_owner_departure(false, true, true, true).await;
}
#[cfg(feature = "claude")]
#[tokio::test]
async fn claude_queued_child_work_retains_owner_until_drained() {
    child_owner_departure(true, true, true, true).await;
}

async fn child_owner_departure(claude: bool, shutdown: bool, background: bool, queued: bool) {
    let fixture = Fixture::start_family("child-owner-departure", claude, None).await;
    let owned = Arc::new(());
    let weak = Arc::downgrade(&owned);
    let state = DurableSession::open(
        super::durable_owner_drop::WitnessStore {
            inner: SqliteStore::open(fixture.workspace.join("children.sqlite")).unwrap(),
            _witness: owned,
        },
        "configured-native-root",
    )
    .await
    .unwrap();
    let (parent, events) = fixture.parent_with_state(state).await;
    let root_session = parent.session_id().to_owned();
    let turn = parent
        .prompt(
            PromptRequest::new(if background {
                "spawn-background"
            } else {
                "spawn-foreground"
            })
            .request_id("child"),
        )
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, fixture.child_started.notified())
        .await
        .unwrap();
    if !background {
        fixture.child_release.notify_one();
    }
    tokio::time::timeout(DEADLINE, turn.result())
        .await
        .unwrap()
        .unwrap();
    if !background {
        let directory: Value =
            serde_json::from_str(&answer(&parent, "directory", "before-departure").await).unwrap();
        assert_eq!(directory["agents"][0]["status"]["state"], "completed");
        assert_eq!(
            directory["agents"][0]["status"]["output"],
            "native-child-result"
        );
    }
    if queued {
        let receipt: Value =
            serde_json::from_str(&answer(&parent, "queue-followup", "queue-followup").await)
                .unwrap();
        assert_eq!(receipt["disposition"], "queued");
    }
    let retained = parent.clone();
    drop(parent);
    assert!(weak.upgrade().is_some(), "live root clone retains owner");
    if shutdown {
        retained.shutdown().await.unwrap();
    }
    drop((retained, events));
    if background {
        // No root facade remains; accepted work still owns its registry.
        assert!(
            weak.upgrade().is_some(),
            "pending background must retain owner"
        );
        fixture.child_release.notify_one();
        tokio::time::timeout(DEADLINE, fixture.child_finished.notified())
            .await
            .expect("background result submission must survive root departure");
        if queued {
            tokio::time::timeout(DEADLINE, fixture.child_started.notified())
                .await
                .expect("queued follow-up starts after first child settles");
            assert!(
                weak.upgrade().is_some(),
                "queued follow-up must retain registry"
            );
            fixture.child_release.notify_one();
            tokio::time::timeout(DEADLINE, fixture.child_finished.notified())
                .await
                .expect("queued follow-up result must settle after root departure");
        }
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed child tree must release its store after final root departure");
    let (reopened, events) = fixture.parent().await;
    reopened.ready().await.unwrap();
    assert_eq!(reopened.session_id(), root_session);
    let directory: Value =
        serde_json::from_str(&answer(&reopened, "directory", "after-departure").await).unwrap();
    assert_eq!(directory["agents"].as_array().unwrap().len(), 1);
    assert_eq!(directory["agents"][0]["status"]["state"], "completed");
    assert_eq!(
        directory["agents"][0]["status"]["output"],
        "native-child-result"
    );
    println!(
        "CHILD_OWNER_RELEASE claude={claude} shutdown={shutdown} background={background} queued={queued}: live clone/pending work retained, final settlement released store, completed result survived reopen"
    );
    reopened.shutdown().await.unwrap();
    drop((reopened, events));
}

fn directory_completed(directory: &Value) -> Value {
    let mut directory = directory.clone();
    for agent in directory["agents"].as_array_mut().unwrap() {
        agent["status"] = json!({"state":"completed", "output":"native-child-result"});
    }
    directory
}

#[test]
fn cold_build_resumes_background_children_without_submitting_root_work() {
    let path = std::env::temp_dir().join(format!(
        "native-cold-children-{}",
        nanocodex::agent::session::SessionId::new()
    ));
    std::fs::create_dir_all(&path).unwrap();
    let families = if cfg!(feature = "claude") {
        vec!["oai", "claude"]
    } else {
        vec!["oai"]
    };
    for family in families {
        let workspace = path.join(family);
        for stage in ["seed", "recover", "verify"] {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "durable_children::cold_child_process",
                    "--ignored",
                    "--nocapture",
                ])
                .env("NANOCODEX_CHILD_FAMILY", family)
                .env("NANOCODEX_CHILD_STAGE", stage)
                .env("NANOCODEX_CHILD_PATH", &workspace)
                .output()
                .unwrap();
            println!(
                "COLD_CHILD family={family} stage={stage}\n{}",
                String::from_utf8_lossy(&result.stdout)
            );
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
#[ignore = "subprocess entrypoint for abrupt native owner replacement"]
async fn cold_child_process() {
    let family = std::env::var("NANOCODEX_CHILD_FAMILY").unwrap();
    let stage = std::env::var("NANOCODEX_CHILD_STAGE").unwrap();
    let path = PathBuf::from(std::env::var("NANOCODEX_CHILD_PATH").unwrap());
    if stage == "verify" {
        let session = std::fs::read_to_string(path.join("root-session")).unwrap();
        let mut journal = nanocodex::durability::ChildJournal::open(
            SqliteStore::open(path.join("children.sqlite")).unwrap(),
            &session,
        )
        .await
        .unwrap();
        let tree = journal.load::<Value>().await.unwrap().unwrap();
        let children = tree["sessions"].as_object().unwrap();
        assert_eq!(
            children.len(),
            1,
            "cold recovery must not fabricate another child"
        );
        let child = children.values().next().unwrap();
        assert_eq!(
            child["submitted_output"], "native-child-result",
            "accepted child result survives a second abrupt restart"
        );
        println!(
            "COLD_CHILD_ACCEPTED family={family}: persisted one child/result after root-free startup recovery"
        );
        return;
    }
    let fixture = Fixture::start_family("cold-build", family == "claude", Some(path.clone())).await;
    let (parent, _events) = fixture.parent().await;
    if stage == "seed" {
        std::fs::write(path.join("root-session"), parent.session_id()).unwrap();
        let turn = parent
            .prompt(PromptRequest::new("spawn-background").request_id("completed-root"))
            .await
            .unwrap();
        tokio::time::timeout(DEADLINE, fixture.child_started.notified())
            .await
            .unwrap();
        let result = tokio::time::timeout(DEADLINE, turn.result())
            .await
            .unwrap()
            .unwrap();
        assert!(result.final_message().contains("agent_id"));
        println!(
            "COLD_CHILD_SEED family={family}: root terminal while background child model response gated"
        );
    } else {
        assert_eq!(stage, "recover");
        tokio::time::timeout(DEADLINE, parent.ready())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(DEADLINE, fixture.child_started.notified())
            .await
            .unwrap();
        assert_eq!(
            fixture.calls(),
            1,
            "cold build resumes only child's pending HTTP request"
        );
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|body| latest_prompt(body).contains("gated-child"))
        );
        fixture.child_release.notify_one();
        tokio::time::timeout(DEADLINE, fixture.child_finished.notified())
            .await
            .unwrap();
        let requests = fixture.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .all(|body| latest_prompt(body).contains("gated-child")),
            "startup must never fabricate a root prompt"
        );
        println!(
            "COLD_CHILD_RECOVER family={family}: {} child-only HTTP requests, accepted submitted result; no root.prompt call",
            requests.len()
        );
    }
    // Deliberately bypass destructors to reproduce abrupt process replacement.
    std::process::exit(0);
}

struct ObservedStore {
    inner: SqliteStore,
    acquisitions: Arc<Mutex<Vec<String>>>,
    fail_write: Arc<std::sync::atomic::AtomicBool>,
}
impl nanocodex::durability::StateStore for ObservedStore {
    fn read_record<'a>(
        &'a mut self,
        id: &'a str,
        key: &'a str,
    ) -> nanocodex::durability::StoreFuture<
        'a,
        Result<Option<String>, nanocodex::durability::StoreError>,
    > {
        self.inner.read_record(id, key)
    }
    fn acquire<'a>(
        &'a mut self,
        id: &'a str,
        owner: nanocodex::durability::OwnerId,
    ) -> nanocodex::durability::StoreFuture<
        'a,
        Result<nanocodex::durability::OwnedState, nanocodex::durability::StoreError>,
    > {
        self.acquisitions.lock().unwrap().push(id.to_owned());
        self.inner.acquire(id, owner)
    }
    fn replace<'a>(
        &'a mut self,
        id: &'a str,
        owner: &'a nanocodex::durability::OwnerToken,
        revision: u64,
        payload: &'a str,
        records: &'a [nanocodex::durability::StoreRecord],
    ) -> nanocodex::durability::StoreFuture<'a, Result<u64, nanocodex::durability::StoreError>>
    {
        Box::pin(async move {
            if self
                .fail_write
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(nanocodex::durability::StoreError::NotCommitted(
                    "synthetic startup child journal unavailable".into(),
                ));
            }
            self.inner
                .replace(id, owner, revision, payload, records)
                .await
        })
    }
}

struct CustomFactory;
impl nanocodex_agent::backend::AgentFactory for CustomFactory {
    fn spawn(
        &self,
        _: nanocodex_agent::AgentHandle,
        _: nanocodex::agent::SpawnOptions,
        _: Option<Arc<str>>,
    ) -> nanocodex_agent::backend::BackendFuture<
        nanocodex_agent::Result<(Nanocodex, nanocodex::AgentEvents)>,
    > {
        panic!("rejected custom factory must never be replaced or called")
    }
    fn restore(
        &self,
        _: nanocodex_agent::AgentHandle,
        _: nanocodex::agent::ChildSnapshot,
        _: Option<Arc<str>>,
    ) -> nanocodex_agent::backend::BackendFuture<
        nanocodex_agent::Result<(Nanocodex, nanocodex::AgentEvents)>,
    > {
        panic!("rejected custom factory must never be replaced or called")
    }
}

#[tokio::test]
async fn configured_spawn_factories_are_rejected_before_automatic_tree_mutation() {
    for claude in [false, true] {
        if claude && !cfg!(feature = "claude") {
            continue;
        }
        let fixture = Fixture::start_family("factory-authority", claude, None).await;
        let acquisitions = Arc::new(Mutex::new(Vec::new()));
        let state = DurableSession::open(
            ObservedStore {
                inner: SqliteStore::open(fixture.workspace.join("custom.sqlite")).unwrap(),
                acquisitions: acquisitions.clone(),
                fail_write: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            "custom-root",
        )
        .await
        .unwrap();
        let before = acquisitions.lock().unwrap().clone();
        let result = if claude {
            #[cfg(feature = "claude")]
            {
                let client = nanocodex::claude::ClaudeClient::new(
                    reqwest::Client::new(),
                    format!("{}/v1/messages", fixture.base),
                    "synthetic-key",
                );
                Nanocodex::builder(nanocodex::Claude::new(client, "claude-sonnet-5-5"))
                    .spawn_factory(Arc::new(CustomFactory))
                    .durability(state)
                    .await
                    .map(|_| ())
            }
            #[cfg(not(feature = "claude"))]
            {
                unreachable!()
            }
        } else {
            Nanocodex::builder(fixture.openai())
                .spawn_factory(Arc::new(CustomFactory))
                .durability(state)
                .await
                .map(|_| ())
        };
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot replace a configured spawn factory")
        );
        assert_eq!(
            *acquisitions.lock().unwrap(),
            before,
            "rejection precedes identity/tree acquisition"
        );
        assert_eq!(fixture.calls(), 0);
        println!(
            "NATIVE_FACTORY_AUTHORITY claude={claude}: custom factory rejected, zero child-store acquisitions or provider requests"
        );
    }
}

#[tokio::test]
async fn startup_recovery_errors_are_observable_without_root_model_work() {
    for claude in [false, true] {
        if claude && !cfg!(feature = "claude") {
            continue;
        }
        let fixture = Fixture::start_family("startup-error", claude, None).await;
        let fail_write = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let state = DurableSession::open(
            ObservedStore {
                inner: SqliteStore::open(fixture.workspace.join("error.sqlite")).unwrap(),
                acquisitions: Arc::new(Mutex::new(Vec::new())),
                fail_write: fail_write.clone(),
            },
            "error-root",
        )
        .await
        .unwrap();
        let (agent, _events) = if claude {
            #[cfg(feature = "claude")]
            {
                let client = nanocodex::claude::ClaudeClient::new(
                    reqwest::Client::new(),
                    format!("{}/v1/messages", fixture.base),
                    "synthetic-key",
                );
                let builder =
                    Nanocodex::builder(nanocodex::Claude::new(client, "claude-sonnet-5-5"))
                        .durability(state.clone())
                        .await
                        .unwrap();
                fail_write.store(true, std::sync::atomic::Ordering::SeqCst);
                builder.build().unwrap()
            }
            #[cfg(not(feature = "claude"))]
            {
                unreachable!()
            }
        } else {
            let builder = Nanocodex::builder(fixture.openai())
                .workspace(&fixture.workspace)
                .durability(state.clone())
                .await
                .unwrap();
            fail_write.store(true, std::sync::atomic::Ordering::SeqCst);
            builder.build().unwrap()
        };
        let error = tokio::time::timeout(DEADLINE, agent.ready())
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("synthetic startup child journal unavailable"),
            "{error}"
        );
        assert_eq!(
            agent.clone().ready().await.unwrap_err().to_string(),
            error.to_string()
        );
        assert!(agent.prompt("must not dispatch").await.is_err());
        assert!(
            state.state().await.unwrap().operations().is_empty(),
            "startup failure must not admit a fabricated root operation"
        );
        assert_eq!(fixture.calls(), 0);
        agent.shutdown().await.unwrap();
        println!(
            "NATIVE_STARTUP_ERROR claude={claude}: ready/clone/prompt report recovery failure, zero root operations or HTTP"
        );
    }
}

#[tokio::test]
async fn native_direct_spawn_survives_root_drop() {
    direct_spawn_departure(false, false).await;
}

#[tokio::test]
async fn native_direct_spawn_survives_root_shutdown() {
    direct_spawn_departure(false, true).await;
}

#[cfg(feature = "claude")]
#[tokio::test]
async fn claude_direct_spawn_survives_root_drop() {
    direct_spawn_departure(true, false).await;
}

#[cfg(feature = "claude")]
#[tokio::test]
async fn claude_direct_spawn_survives_root_shutdown() {
    direct_spawn_departure(true, true).await;
}

async fn direct_spawn_departure(claude: bool, shutdown: bool) {
    for capability in [false, true] {
        let fixture = Fixture::start_family(
            &format!("direct-spawn-{shutdown}-{capability}"),
            claude,
            None,
        )
        .await;
        let owned = Arc::new(());
        let weak = Arc::downgrade(&owned);
        let state = DurableSession::open(
            super::durable_owner_drop::WitnessStore {
                inner: SqliteStore::open(fixture.workspace.join("children.sqlite")).unwrap(),
                _witness: owned,
            },
            "configured-native-root",
        )
        .await
        .unwrap();
        let (capture, captured) = tokio::sync::oneshot::channel();
        let (root, events) = fixture.parent_capturing_handle(state, Some(capture)).await;
        let handle = captured.await.unwrap();
        root.ready().await.unwrap();
        let (child, child_events) = if capability {
            handle.spawn().await.unwrap()
        } else {
            root.spawn().await.unwrap()
        };
        child.ready().await.unwrap();
        assert_ne!(root.session_id(), child.session_id());
        assert_eq!(fixture.calls(), 0, "construction starts no model turn");
        if shutdown {
            root.shutdown().await.unwrap();
        }
        drop((root, events));
        tokio::time::timeout(DEADLINE, async {
            while handle.ensure_available().await.is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("root driver must stop independently of the sibling");
        assert!(
            handle.spawn().await.is_err(),
            "departed root capability stays weak"
        );
        drop(handle);
        let retained = child.clone();
        drop(child);
        assert_eq!(
            answer(&retained, "direct sibling", "direct").await,
            "durable native answer"
        );
        let (descendant, descendant_events) = retained.spawn().await.unwrap();
        descendant.ready().await.unwrap();
        retained.shutdown().await.unwrap();
        drop((retained, child_events));
        assert_eq!(
            answer(&descendant, "independent descendant", "descendant").await,
            "durable native answer"
        );
        descendant.shutdown().await.unwrap();
        drop((descendant, descendant_events));
        tokio::time::timeout(DEADLINE, async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("final direct caller departure must release its store");
        println!(
            "DIRECT_SPAWN claude={claude} shutdown={shutdown} capability={capability}: root stopped; cloned sibling and descendant answered independently; final caller released store"
        );
    }
}

#[tokio::test]
async fn native_direct_forks_survive_caller_departure() {
    // Durable operation policies already reject forks. Exercise the supported
    // native fork APIs with the same public registry ownership barrier instead.
    for fork_kind in ["latest", "completed", "side", "capability"] {
        let fixture = Fixture::start_family(&format!("direct-fork-{fork_kind}"), false, None).await;
        let (registry, _, _updates) = nanocodex_subagents::channel(usize::MAX);
        let weak = Arc::downgrade(&registry);
        let (capture, captured) = tokio::sync::oneshot::channel();
        let capture = Arc::new(Mutex::new(Some(capture)));
        let (child, child_events) = Nanocodex::builder(fixture.openai())
            .workspace(&fixture.workspace)
            .tools(Tools::builder().without_defaults().build().unwrap())
            .turn_ownership(Arc::new(nanocodex_subagents::RegistryOwnership::child(
                &registry,
            )))
            .map_tools_factory(move |handle, tools| {
                if let Some(capture) = capture.lock().unwrap().take() {
                    let _ = capture.send(handle);
                }
                Ok(tools)
            })
            .build()
            .unwrap();
        let handle = captured.await.unwrap();
        child.ready().await.unwrap();
        drop(registry);
        let completed = child
            .prompt("fork boundary")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        let (fork, fork_events) = match fork_kind {
            "latest" => child.fork().await.unwrap(),
            "completed" => child.fork_from(&completed).await.unwrap(),
            "side" => child.fork_side_conversation().await.unwrap(),
            _ => handle.fork().await.unwrap(),
        };
        fork.ready().await.unwrap();
        child.shutdown().await.unwrap();
        drop((child, child_events, completed, handle));
        assert_eq!(
            tokio::time::timeout(DEADLINE, async {
                fork.prompt("independent fork")
                    .await
                    .unwrap()
                    .result()
                    .await
                    .unwrap()
                    .final_message()
                    .to_owned()
            })
            .await
            .unwrap(),
            "durable native answer"
        );
        println!("DIRECT_FORK {fork_kind}: fork answered after the original caller departed");
        fork.shutdown().await.unwrap();
        drop((fork, fork_events));
        tokio::time::timeout(DEADLINE, async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("final fork caller must release its registry");
    }
}
