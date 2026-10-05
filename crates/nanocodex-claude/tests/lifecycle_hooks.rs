//! Public-runtime journeys; only Messages inference is replaced by loopback SSE.
use axum::{Json, Router, routing::post};
use nanocodex_agent::Nanocodex;
use nanocodex_claude::{
    Claude, ClaudeClient, ClaudeHookFuture, ClaudeLifecycleDecision, ClaudeLifecycleEvent,
    ClaudeLifecycleInvocation, ClaudeLifecycleOutcome, ClaudeToolDecision, ClaudeToolHooks,
    ClaudeToolInvocation,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
struct RecordedHooks {
    calls: Mutex<Vec<Value>>,
    deny_compact: AtomicBool,
}
impl ClaudeToolHooks for RecordedHooks {
    fn handles_lifecycle(&self, _: &ClaudeLifecycleEvent) -> bool {
        true
    }
    fn before<'a>(
        &'a self,
        _: &'a str,
        _: &'a Value,
        _: &'a ClaudeToolInvocation,
    ) -> ClaudeHookFuture<'a, Result<ClaudeToolDecision, String>> {
        Box::pin(async { Ok(ClaudeToolDecision::Allow) })
    }
    fn lifecycle<'a>(
        &'a self,
        call: &'a ClaudeLifecycleInvocation,
    ) -> ClaudeHookFuture<'a, Result<ClaudeLifecycleOutcome, String>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(serde_json::to_value(call).unwrap());
            match &call.event {
                ClaudeLifecycleEvent::PreCompact { .. }
                    if self.deny_compact.load(Ordering::SeqCst) =>
                {
                    Ok(ClaudeLifecycleOutcome {
                        decision: ClaudeLifecycleDecision::Block("retain-history".into()),
                        ..Default::default()
                    })
                }
                ClaudeLifecycleEvent::PostCompact { .. } => {
                    Err("post-compact-observation-failed".into())
                }
                _ => Ok(ClaudeLifecycleOutcome::default()),
            }
        })
    }
}
fn sse(text: &str) -> String {
    [json!({"type":"message_start","message":{"id":"synthetic","role":"assistant","model":"test","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
     json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":text}}),
     json!({"type":"content_block_stop","index":0}),
     json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}),
     json!({"type":"message_stop"})].iter().map(|v| format!("data: {v}\n\n")).collect()
}
#[tokio::test]
async fn public_compaction_gates_observation_and_fork_lifecycle() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            async move {
                let compact = body["messages"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()
                    .to_string()
                    .contains("Produce a concise text-only handoff");
                log.lock().unwrap().push(body);
                (
                    [("content-type", "text/event-stream")],
                    sse(if compact {
                        "summary-receipt-preserved"
                    } else {
                        "completed-response"
                    }),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let hooks = Arc::new(RecordedHooks::default());
    let (agent, _) = Nanocodex::builder(Claude::new(
        ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        ),
        "test",
    ))
    .auto_compact_window_tokens(100)
    .tool_hooks(hooks.clone())
    .build()
    .unwrap();
    let answer = agent
        .prompt("seed history")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(answer.final_message(), "completed-response");
    hooks.deny_compact.store(true, Ordering::SeqCst);
    assert!(
        agent
            .compact()
            .await
            .unwrap_err()
            .to_string()
            .contains("retain-history")
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "blocked compaction called provider"
    );
    hooks.deny_compact.store(false, Ordering::SeqCst);
    agent.compact().await.unwrap();
    let answer = agent
        .prompt("continue after summary")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(
        answer.final_message(),
        "completed-response",
        "post observer replaced result"
    );
    assert!(
        requests.lock().unwrap()[2]
            .to_string()
            .contains("summary-receipt-preserved")
    );
    assert!(
        requests.lock().unwrap()[2]
            .to_string()
            .contains("post-compact-observation-failed")
    );
    agent
        .prompt("automatic boundary ".repeat(100))
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let (child, _) = agent.fork().await.unwrap();
    child
        .prompt("child prompt")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    child.shutdown().await.unwrap();
    agent.shutdown().await.unwrap();
    let calls = hooks.calls.lock().unwrap();
    let names = calls
        .iter()
        .map(|v| v["hook_event_name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "SessionStart",
            "UserPromptSubmit",
            "Stop",
            "PreCompact",
            "PreCompact",
            "PostCompact",
            "UserPromptSubmit",
            "Stop",
            "UserPromptSubmit",
            "PreCompact",
            "PostCompact",
            "Stop",
            "SubagentStart",
            "UserPromptSubmit",
            "SubagentStop",
            "SessionEnd",
            "SessionEnd"
        ]
    );
    let triggers = calls
        .iter()
        .filter(|v| v["hook_event_name"] == "PreCompact")
        .map(|v| v["trigger"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(triggers.contains(&"manual") && triggers.contains(&"auto"));
    let post = calls
        .iter()
        .find(|v| v["hook_event_name"] == "PostCompact")
        .unwrap();
    assert_eq!(post["compact_summary"], "summary-receipt-preserved");
    let start = calls
        .iter()
        .find(|v| v["hook_event_name"] == "SubagentStart")
        .unwrap();
    let stop = calls
        .iter()
        .find(|v| v["hook_event_name"] == "SubagentStop")
        .unwrap();
    assert_eq!(start["agent_id"], stop["agent_id"]);
    assert_ne!(start["session_id"], calls[0]["session_id"]);
    let artifact = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/lifecycle-public-runtime.json");
    std::fs::write(&artifact, serde_json::to_vec_pretty(&json!({"hooks":*calls,"requests":*requests.lock().unwrap(),"observed":"gate prevents summary HTTP; post failure preserves summary; fork emits child identities"})).unwrap()).unwrap();
    eprintln!("lifecycle evidence: {}", artifact.display());
    server.abort();
}
