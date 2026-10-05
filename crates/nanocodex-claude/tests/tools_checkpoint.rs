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
    fn state_id(&self) -> &str {
        "tools-only-checkpoint"
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
    fn begin_step_with_replay(
        &self,
        id: String,
        step: String,
        kind: String,
        input: Value,
        safety: nanocodex_agent::ReplaySafety,
    ) -> PolicyFuture<'_, Step> {
        // Session-local task mutations are reconstructed with their checkpoint.
        assert_eq!(safety, nanocodex_agent::ReplaySafety::Safe);
        self.begin_step(id, step, kind, input)
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
        .join("../../outputs/provider-managed-20261001/crates/tools-checkpoint");
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
