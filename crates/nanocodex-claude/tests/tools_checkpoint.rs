#![cfg(all(feature = "tools", not(target_family = "wasm")))]

//! Canonical tools-only regression: native task snapshots, sequential durable
//! dispatch and filesystem checkpoint reopen, without the workspace-files alias.
use axum::{Json, Router, routing::post};
use nanocodex_agent::Nanocodex;
use nanocodex_claude::execution::{Admission, ClaudeExecutionPolicy, PolicyFuture, Step};
use nanocodex_claude::{Claude, ClaudeClient};
use nanocodex_claude_tools::ClaudeTasks;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct CheckpointPolicy {
    path: PathBuf,
    cursors: Mutex<Vec<Value>>,
}
impl CheckpointPolicy {
    fn save(&self, value: Value) {
        std::fs::write(&self.path, serde_json::to_vec(&value).unwrap()).unwrap();
    }
}
// Test-only host policy. This tests the native seam, not store fencing or an
// exactly-once service; those remain the durability crate's integration tests.
impl ClaudeExecutionPolicy for CheckpointPolicy {
    /// Durable state (and so session) identity: one per checkpoint file.
    fn state_id(&self) -> &str {
        self.path
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("tools-only-checkpoint")
    }
    fn admit(&self, id: String, _: Value, _: bool) -> PolicyFuture<'_, (String, Admission)> {
        Box::pin(async move { Ok((id, Admission::Execute)) })
    }
    fn begin_attempt(&self, _: String) -> PolicyFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn continuation(&self, _: String) -> PolicyFuture<'_, Option<Value>> {
        Box::pin(async { Ok(None) })
    }
    fn advance(&self, _: String, state: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.cursors.lock().unwrap().push(state);
            Ok(())
        })
    }
    fn begin_step(&self, _: String, _: String, _: String, _: Value) -> PolicyFuture<'_, Step> {
        Box::pin(async { Ok(Step::Execute) })
    }
    fn complete_step(&self, _: String, _: String, _: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn complete(&self, _: String, checkpoint: Value, _: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.save(checkpoint);
            Ok(())
        })
    }
    fn fail(&self, _: String, checkpoint: Value, _: String) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.save(checkpoint);
            Ok(())
        })
    }
    fn cancel(&self, _: String, checkpoint: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.save(checkpoint);
            Ok(())
        })
    }
    fn release(&self, _: String) -> PolicyFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn shutdown(&self) -> PolicyFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn checkpoint(&self, state: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.save(state);
            Ok(())
        })
    }
}
fn sse(block: Value, stop: &str) -> String {
    let mut out = String::new();
    let mut emit = |v: Value| out.push_str(&format!("data: {v}\n\n"));
    emit(
        json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"test","content":[],"usage":{"input_tokens":2,"output_tokens":0}}}),
    );
    let (start, delta) = if block["type"] == "text" {
        (
            json!({"type":"text","text":""}),
            json!({"type":"text_delta","text":block["text"]}),
        )
    } else {
        (
            json!({"type":"tool_use","id":block["id"],"name":block["name"],"input":{}}),
            json!({"type":"input_json_delta","partial_json":block["input"].to_string()}),
        )
    };
    emit(json!({"type":"content_block_start","index":0,"content_block":start}));
    emit(json!({"type":"content_block_delta","index":0,"delta":delta}));
    emit(json!({"type":"content_block_stop","index":0}));
    emit(json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":4}}));
    emit(json!({"type":"message_stop"}));
    out
}

#[tokio::test]
async fn tools_only_task_checkpoint_reopens_and_retains_id_watermark() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("claude-checkpoint.json");
    let policy = Arc::new(CheckpointPolicy {
        path: path.clone(),
        cursors: Mutex::new(vec![]),
    });
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = calls.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let log = log.clone();
        async move {
            let index = { let mut requests = log.lock().unwrap(); requests.push(body); requests.len() };
            let (block, stop) = match index {
                1 => (json!({"type":"tool_use","id":"create-task","name":"TaskCreate","input":{"subject":"Preserved task","description":"from first session"}}), "tool_use"),
                2 => (json!({"type":"text","text":"saved"}), "end_turn"),
                3 => (json!({"type":"tool_use","id":"read-reopened-task","name":"TaskGet","input":{"taskId":"1"}}), "tool_use"),
                4 => (json!({"type":"tool_use","id":"create-next-task","name":"TaskCreate","input":{"subject":"next task","description":"watermark retained"}}), "tool_use"),
                5 => (json!({"type":"text","text":"reopened and continued"}), "end_turn"),
                _ => panic!("unexpected model request after completed journey"),
            };
            ([("content-type", "text/event-stream")], sse(block, stop))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let board = Arc::new(ClaudeTasks::new());
    let (agent, _) = Nanocodex::builder(Claude::new(client.clone(), "test"))
        .max_tokens(128_000)
        .tasks(board.clone())
        .parallel_tools(true)
        .execution_policy(policy.clone(), None)
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("create a task")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "saved"
    );
    assert_eq!(calls.lock().unwrap().len(), 2);
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        saved["tasks"].is_object(),
        "tools-only must checkpoint attached tasks"
    );
    assert!(!policy.cursors.lock().unwrap().is_empty());
    assert!(
        policy
            .cursors
            .lock()
            .unwrap()
            .iter()
            .all(|cursor| cursor["parallel"] == false),
        "task-bearing durable dispatch must stay sequential"
    );
    let saved_evidence = saved.clone();
    drop(agent);
    drop(board);
    drop(policy);
    let reopened_board = Arc::new(ClaudeTasks::new());
    let reopened_policy = Arc::new(CheckpointPolicy {
        path,
        cursors: Mutex::new(vec![]),
    });
    let (reopened, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tasks(reopened_board.clone())
        .execution_policy(reopened_policy, Some(saved))
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        reopened
            .prompt("read preserved task and create next task")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "reopened and continued"
    );
    let requests = calls.lock().unwrap();
    assert_eq!(requests.len(), 5);
    let tool_result = |request_index: usize, call_id: &str| -> Value {
        requests[request_index]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .find(|block| block["type"] == "tool_result" && block["tool_use_id"] == call_id)
            .expect("reopened runtime must return actual task receipt")
            .clone()
    };
    let read = tool_result(3, "read-reopened-task");
    assert_ne!(read["is_error"], true);
    assert!(read["content"].as_str().unwrap().contains("Preserved task"));
    let created = tool_result(4, "create-next-task");
    assert_ne!(created["is_error"], true);
    let created: Value = serde_json::from_str(created["content"].as_str().unwrap()).unwrap();
    assert_eq!(created["task"]["id"], "2");
    let artifact = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/provider-managed-20261001/crates/tools-checkpoint");
    std::fs::create_dir_all(&artifact).unwrap();
    std::fs::write(
        artifact.join("requests.json"),
        serde_json::to_vec_pretty(&*requests).unwrap(),
    )
    .unwrap();
    std::fs::write(
        artifact.join("checkpoint-before-reopen.json"),
        serde_json::to_vec_pretty(&saved_evidence).unwrap(),
    )
    .unwrap();
    std::fs::write(artifact.join("scenario.txt"), "Command: cargo test --locked -p nanocodex-claude --no-default-features --features tools --test tools_checkpoint\nSynthetic loopback Messages/SSE and caller-owned filesystem policy; not SQLite fencing proof. Expected/observed: TaskCreate -> saved checkpoint, new builder+board -> TaskGet retains task, TaskCreate uses ID 2, 5 actual HTTP requests.\n").unwrap();
    drop(reopened);
    server.abort();
}

/// Existing host policies need no new methods to keep plain steering working.
#[tokio::test]
async fn older_custom_policy_preserves_plain_steering() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temp = tempfile::tempdir().unwrap();
    let policy = Arc::new(CheckpointPolicy {
        path: temp.path().join("checkpoint.json"),
        cursors: Mutex::new(vec![]),
    });
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route(
        "/v1/messages",
        post({
            let started = started.clone();
            let release = release.clone();
            let requests = requests.clone();
            move |Json(body): Json<Value>| {
                let started = started.clone();
                let release = release.clone();
                let requests = requests.clone();
                async move {
                    let index = {
                        let mut log = requests.lock().unwrap();
                        log.push(body);
                        log.len()
                    };
                    if index == 1 {
                        started.notify_one();
                        release.notified().await;
                    }
                    (
                        [("content-type", "text/event-stream")],
                        sse(
                            json!({"type":"text","text":"plain steer completed"}),
                            "end_turn",
                        ),
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic");
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .execution_policy(policy, None)
        .unwrap()
        .build()
        .unwrap();
    let turn = agent.prompt("older policy task").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    turn.steer("plain correction on older policy")
        .await
        .unwrap();
    assert!(
        turn.steer_with_id("requires-receipts".into(), "identified correction")
            .await
            .is_err()
    );
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), turn.result())
            .await
            .unwrap()
            .unwrap()
            .final_message(),
        "plain steer completed"
    );
    let transcript = requests.lock().unwrap().clone();
    assert_eq!(transcript.len(), 2);
    assert!(
        transcript[1]["messages"]
            .to_string()
            .contains("plain correction on older policy")
    );
    eprintln!(
        "{}",
        json!({"scenario":"older-custom-policy-plain-steering","requests":transcript,"outcome":"plain steering retained; identified input not downgraded"})
    );
    agent.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn rewound_checkpoints_branch_from_their_source_session() {
    use nanocodex_agent::{Lineage, Origin};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temp = tempfile::tempdir().unwrap();
    let policy = |name: &str| {
        Arc::new(CheckpointPolicy {
            path: temp.path().join(name),
            cursors: Mutex::new(vec![]),
        })
    };
    let saved = |name: &str| -> Value {
        serde_json::from_slice(&std::fs::read(temp.path().join(name)).unwrap()).unwrap()
    };
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            async move {
                let mut log = log.lock().unwrap();
                log.push(body);
                let text = format!("reply {}", log.len());
                (
                    [("content-type", "text/event-stream")],
                    sse(json!({"type":"text","text":text}), "end_turn"),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic");
    let open = |name: &str, checkpoint: Option<Value>| {
        Nanocodex::builder(Claude::new(client.clone(), "claude-sonnet-4-6"))
            .max_tokens(128_000)
            .execution_policy(policy(name), checkpoint)
            .unwrap()
            .build()
            .unwrap()
            .0
    };

    let source = open("source.json", None);
    let source_id = source.session_id().to_owned();
    source
        .prompt("first")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let first = saved("source.json");
    source
        .prompt("second")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let latest = saved("source.json");
    source.shutdown().await.unwrap();

    // Rewinding to the first turn publishes a branch of the source session.
    let rewound = nanocodex_claude::rewind_checkpoint(&source_id, Some(first), latest).unwrap();
    let branch = open("branch.json", Some(rewound));
    let branch_id = branch.session_id().to_owned();
    assert_ne!(branch_id, source_id);
    let expected = Lineage {
        root_session_id: source_id.clone(),
        parent_session_id: Some(source_id.clone()),
        origin: Origin::Branch,
        depth: 1,
    };
    assert_eq!(branch.session().lineage, expected);
    // The branch continues from the selected boundary, not the later turn.
    branch
        .prompt("third")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let continued = requests.lock().unwrap().last().unwrap()["messages"].to_string();
    assert!(continued.contains("first") && continued.contains("third"));
    assert!(!continued.contains("second"), "{continued}");
    assert_eq!(branch.checkpoint().await.unwrap().lineage(), &expected);
    let branch_latest = saved("branch.json");
    branch.shutdown().await.unwrap();

    // Rewinding the branch stays in the same tree, one level deeper.
    let nested = open(
        "nested.json",
        Some(nanocodex_claude::rewind_checkpoint(&branch_id, None, branch_latest).unwrap()),
    );
    assert_eq!(
        nested.session().lineage,
        Lineage {
            root_session_id: source_id,
            parent_session_id: Some(branch_id),
            origin: Origin::Branch,
            depth: 2,
        }
    );
    nested.shutdown().await.unwrap();
    server.abort();
}
