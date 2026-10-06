//! Public Messages + filesystem + SQLite journey. Only the model is synthetic.
#![cfg(all(feature = "claude", feature = "sqlite"))]
use axum::{Json, Router, routing::post};
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_claude::{
    Claude, ClaudeClient, ClaudeHookFuture, ClaudeToolDecision, ClaudeToolHooks,
    ClaudeToolInvocation, ClaudeToolReply, ToolDefinition,
};
use nanocodex_durability::{DurableAgentExt, DurableSession, SqliteStore};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct Policy {
    log: Arc<Mutex<Vec<Value>>>,
}
impl ClaudeToolHooks for Policy {
    fn before<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
        invocation: &'a ClaudeToolInvocation,
    ) -> ClaudeHookFuture<'a, Result<ClaudeToolDecision, String>> {
        Box::pin(async move {
            self.log.lock().unwrap().push(json!({"event":"before","name":name,"input":input,"session":invocation.session_id,"turn":invocation.turn_id,"call":invocation.call_id}));
            match invocation.call_id.as_str() {
                "denied" => Ok(ClaudeToolDecision::Deny("protected file".into())),
                "broken-pre" => Err("approval service unavailable".into()),
                "updated" => Ok(ClaudeToolDecision::UpdateInput(
                    json!({"content":"host approved content"}),
                )),
                _ => Ok(ClaudeToolDecision::Allow),
            }
        })
    }
    fn after<'a>(
        &'a self,
        _: &'a str,
        input: &'a Value,
        invocation: &'a ClaudeToolInvocation,
        reply: &'a ClaudeToolReply,
    ) -> ClaudeHookFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.log.lock().unwrap().push(json!({"event":"after","input":input,"call":invocation.call_id,"is_error":reply.is_error}));
            if invocation.call_id == "updated" {
                Err("observer unavailable".into())
            } else {
                Ok(())
            }
        })
    }
}
struct InnerAudit {
    log: Arc<Mutex<Vec<Value>>>,
}
impl ClaudeToolHooks for InnerAudit {
    fn before<'a>(
        &'a self,
        _: &'a str,
        input: &'a Value,
        invocation: &'a ClaudeToolInvocation,
    ) -> ClaudeHookFuture<'a, Result<ClaudeToolDecision, String>> {
        Box::pin(async move {
            self.log
                .lock()
                .unwrap()
                .push(json!({"event":"before","call":invocation.call_id,"input":input}));
            Ok(ClaudeToolDecision::Allow)
        })
    }
    fn after<'a>(
        &'a self,
        _: &'a str,
        _: &'a Value,
        invocation: &'a ClaudeToolInvocation,
        reply: &'a ClaudeToolReply,
    ) -> ClaudeHookFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.log
                .lock()
                .unwrap()
                .push(json!({"event":"after","call":invocation.call_id,"is_error":reply.is_error}));
            Ok(())
        })
    }
}
fn sse(blocks: Vec<Value>, stop: &str) -> String {
    let mut frames = vec![
        json!({"type":"message_start","message":{"id":"hook-journey","role":"assistant","model":"test","content":[],"usage":{"input_tokens":12,"output_tokens":0}}}),
    ];
    for (index, block) in blocks.into_iter().enumerate() {
        frames.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        frames.push(json!({"type":"content_block_stop","index":index}));
    }
    frames.push(
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":6}}),
    );
    frames.push(json!({"type":"message_stop"}));
    frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect()
}
#[tokio::test]
async fn tool_hooks_deny_rewrite_retain_receipts_and_do_not_rerun_after_reopen() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state.sqlite");
    let effects = dir.path().join("effects.jsonl");
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let received = received.clone();
        async move {
            let index = { let mut requests = received.lock().unwrap(); requests.push(body); requests.len() };
            let (blocks, stop) = if index == 1 {
                (vec!["denied","broken-pre","updated","tool-error"].into_iter().map(|id| json!({"type":"tool_use","id":id,"name":"WriteReceipt","input":{"content":"original content"}})).collect(), "tool_use")
            } else { (vec![json!({"type":"text","text":"Observed authorized receipt and errors"})], "end_turn") };
            ([("content-type","text/event-stream")], sse(blocks, stop))
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
    let hooks = Arc::new(Mutex::new(vec![]));
    let inner = Arc::new(Mutex::new(vec![]));
    for pass in 0..2 {
        let state = DurableSession::open(SqliteStore::open(&database).unwrap(), "hook-session")
            .await
            .unwrap();
        let target = effects.clone();
        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .tool_hooks(Arc::new(Policy { log: hooks.clone() }))
            .tool_hooks(Arc::new(InnerAudit { log: inner.clone() }))
            .tool_with_context(ToolDefinition { name:"WriteReceipt".into(),description:"Write one authorized receipt".into(),input_schema:json!({"type":"object","properties":{"content":{"type":"string"}},"required":["content"]}), strict:None,defer_loading:false }, move |input, invocation| {
                let target = target.clone();
                async move {
                    if invocation.call_id == "tool-error" { return Err("disk authorization refused".into()); }
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(target).map_err(|e| e.to_string())?;
                    writeln!(file,"{}",json!({"call":invocation.call_id,"input":input})).map_err(|e| e.to_string())?;
                    Ok(ClaudeToolReply::success(nanocodex_claude::ToolResultContent::Text("receipt-written".into())))
                }
            })
            .durability(state).await.unwrap().build().unwrap();
        let result = agent
            .prompt(
                PromptRequest::new("Run the authorized file workflow").request_id("hook-request"),
            )
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(
            result.final_message(),
            "Observed authorized receipt and errors"
        );
        agent.shutdown().await.unwrap();
        drop(agent);
        drop(events);
        assert_eq!(
            requests.lock().unwrap().len(),
            2,
            "pass {pass}: terminal replay must not call model"
        );
        assert_eq!(
            hooks.lock().unwrap().len(),
            6,
            "pass {pass}: hooks only run on initial dispatch"
        );
    }
    let inner = inner.lock().unwrap();
    assert_eq!(
        inner.len(),
        4,
        "outer denials and replays bypass inner hooks"
    );
    assert!(inner.iter().any(|h| h["event"] == "before"
        && h["call"] == "updated"
        && h["input"]["content"] == "host approved content"));
    assert!(
        inner
            .iter()
            .any(|h| h["event"] == "after" && h["call"] == "updated" && h["is_error"] == false),
        "inner post-hook sees completed result before outer observer failure"
    );
    let file = std::fs::read_to_string(&effects).unwrap();
    assert_eq!(
        file.lines().count(),
        1,
        "denied/failed/replayed calls must not write"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&file).unwrap()["input"]["content"],
        "host approved content"
    );
    let requests = requests.lock().unwrap();
    let results = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .filter(|b| b["type"] == "tool_result")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 4);
    assert!(results.iter().all(|r| r["is_error"] == true));
    let updated = results
        .iter()
        .find(|r| r["tool_use_id"] == "updated")
        .unwrap();
    let text = updated["content"].as_str().unwrap();
    assert!(
        text.contains("receipt-written")
            && text.contains("PostToolUse hook failed")
            && text.contains("already returned")
    );
    let hooks = hooks.lock().unwrap();
    assert!(
        hooks
            .iter()
            .filter(|h| h["event"] == "before")
            .all(|h| h["session"] == "hook-session" && !h["turn"].as_str().unwrap().is_empty())
    );
    assert!(
        hooks
            .iter()
            .any(|h| h["event"] == "after" && h["call"] == "tool-error" && h["is_error"] == true)
    );
    let evidence = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../output/claude-hooks");
    std::fs::create_dir_all(&evidence).unwrap();
    std::fs::write(evidence.join("journey.json"), serde_json::to_vec_pretty(&json!({"requests":*requests,"hooks":*hooks,"inner_hooks":*inner,"effects":file,"reopens":2,"terminal_replay_http":0,"terminal_replay_hooks":0})).unwrap()).unwrap();
    server.abort();
}
