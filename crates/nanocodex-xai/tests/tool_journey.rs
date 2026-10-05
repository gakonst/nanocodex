//! Exercise host tools and native reasoning/search replay over the real HTTP API.
use axum::{Json, Router, response::IntoResponse, routing::post};
use futures_util::StreamExt;
use nanocodex_agent::{Nanocodex, Thinking, events::AgentEventKind};
use nanocodex_xai::{ToolDefinition, Xai, XaiClient};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn complete(output: Vec<Value>) -> String {
    format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":{"id":"r","status":"completed","output":output,"usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":4},"output_tokens":3,"output_tokens_details":{"reasoning_tokens":1},"total_tokens":13}}})
    )
}
#[tokio::test]
async fn native_tool_call_id_replay_and_failed_continuation_do_not_repeat_effects() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let app=Router::new().route("/v1/responses",post(move|Json(body):Json<Value>|{let captured=captured.clone();async move{
        let n={let mut log=captured.lock().unwrap();log.push(body);log.len()};
        if n==2{return (axum::http::StatusCode::SERVICE_UNAVAILABLE,"synthetic downstream failure").into_response()}
        let output=if n==1{vec![
            json!({"type":"reasoning","id":"reason-1","status":"completed","content":[{"text":"private reasoning"}],"summary":[],"encrypted_content":"opaque-bound-reasoning"}),
            json!({"type":"web_search_call","id":"hosted-1","status":"completed","action":{"type":"search","query":"fixture"}}),
            json!({"type":"function_call","id":"item-identity","call_id":"call-identity","name":"write_note","arguments":"{\"text\":\"violet\"}","status":"completed"})
        ]}else{vec![json!({"type":"message","id":"answer","role":"assistant","status":"completed","content":[{"type":"output_text","text":"note written once","annotations":[]}]})]};
        ([("content-type","text/event-stream")],complete(output)).into_response()
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let calls = Arc::new(AtomicUsize::new(0));
    let called = calls.clone();
    // Exercise the caller-controlled fail-fast policy; bounded retries have their own journey.
    let (agent,mut events)=Nanocodex::builder(Xai::new(XaiClient::new(reqwest::Client::new(),format!("http://{addr}/v1/responses"),"fixture"),"grok-4.6"))
        .max_retries(0).web_search().tool(ToolDefinition{name:"write_note".into(),description:"Record a synthetic note".into(),parameters:json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]})},move|input|{let called=called.clone();async move{assert_eq!(input["text"],"violet");called.fetch_add(1,Ordering::SeqCst);Ok("saved violet".into())}}).build().unwrap();
    assert!(
        agent
            .prompt("write violet")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let result = agent
        .prompt("continue after service recovery")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "note written once");
    assert_eq!(result.usage().unwrap().input_tokens(), 10);
    assert_eq!(result.usage().unwrap().cached_input_tokens(), 4);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    agent.shutdown().await.unwrap();
    let mut terminal = 0;
    let mut tools = 0;
    while let Ok(Some(event)) =
        tokio::time::timeout(std::time::Duration::from_millis(20), events.next()).await
    {
        if event.kind == AgentEventKind::ToolCall {
            tools += 1;
            assert_eq!(
                event.decode_payload::<Value>().unwrap()["call_id"],
                "call-identity"
            );
        }
        if matches!(
            event.kind,
            AgentEventKind::RunCompleted | AgentEventKind::RunFailed
        ) {
            terminal += 1;
        }
        event.data().unwrap_or_else(|err| {
            panic!(
                "invalid {:?} shared event: {err}: {}",
                event.kind, event.payload
            )
        });
    }
    assert_eq!((tools, terminal), (1, 2));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for body in &requests[1..] {
        let input = body["input"].as_array().unwrap();
        let reasoning = input.iter().find(|i| i["type"] == "reasoning").unwrap();
        assert!(reasoning.get("status").is_none());
        assert_eq!(reasoning["content"][0]["type"], "reasoning_text");
        assert_eq!(reasoning["encrypted_content"], "opaque-bound-reasoning");
        assert!(input.iter().any(|i| i["type"] == "web_search_call"));
        assert!(input.iter().any(|i| i["type"] == "function_call_output"
            && i["call_id"] == "call-identity"
            && i["output"] == "saved violet"));
        assert!(body.get("previous_response_id").is_none());
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/xai");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tool-replay-requests.json"),
        serde_json::to_vec_pretty(&*requests).unwrap(),
    )
    .unwrap();
    println!(
        "tool journey: one host effect, call_id preserved, reasoning normalized, hosted search replayed, 503 failed without retry, next prompt resumed committed tool result; 3 HTTP requests"
    );
    server.abort();
}
#[tokio::test]
async fn unsupported_model_effort_fails_before_network() {
    let client = XaiClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1/v1/responses",
        "fixture",
    );
    assert!(
        Xai::new(client, "grok-4.5")
            .thinking(Thinking::Xhigh)
            .build()
            .is_err()
    );
}
