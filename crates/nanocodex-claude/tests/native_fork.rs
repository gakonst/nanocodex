//! Public native lifecycle + real loopback Messages/SSE. Only the provider is synthetic.
use axum::{Json, Router, routing::post};
use nanocodex_agent::Nanocodex;
use nanocodex_claude::{
    Claude, ClaudeClient, ClaudeToolReply, ClaudeTools, ToolDefinition, ToolResultContent,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

fn stream(blocks: Vec<Value>, stop: &str) -> String {
    let mut events = vec![
        json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"claude-sonnet-5-5","content":[],"usage":{"input_tokens":3,"output_tokens":0}}}),
    ];
    for (index, block) in blocks.into_iter().enumerate() {
        let (start, delta) = match block["type"].as_str().unwrap() {
            "text" => (
                json!({"type":"text","text":""}),
                json!({"type":"text_delta","text":block["text"]}),
            ),
            "thinking" => (
                json!({"type":"thinking","thinking":"","signature":""}),
                json!({"type":"thinking_delta","thinking":block["thinking"]}),
            ),
            _ => (
                json!({"type":"tool_use","id":block["id"],"name":block["name"],"input":{}}),
                json!({"type":"input_json_delta","partial_json":block["input"].to_string()}),
            ),
        };
        events.push(json!({"type":"content_block_start","index":index,"content_block":start}));
        events.push(json!({"type":"content_block_delta","index":index,"delta":delta}));
        if block["type"] == "thinking" {
            events.push(json!({"type":"content_block_delta","index":index,"delta":{"type":"signature_delta","signature":block["signature"]}}));
        }
        events.push(json!({"type":"content_block_stop","index":index}));
    }
    events.push(
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}),
    );
    events.push(json!({"type":"message_stop"}));
    events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}
fn tool(name: &str) -> ToolDefinition {
    serde_json::from_value(json!({"name":name,"description":"Synthetic host action","input_schema":{"type":"object","properties":{},"additionalProperties":false}})).unwrap()
}
#[tokio::test]
async fn native_fork_preserves_history_without_replay_or_parent_mutation() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let trace = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = trace.clone();
    let arrived = Arc::new(tokio::sync::Notify::new());
    let provider_arrived = arrived.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let requests = requests.clone();
        let arrived = provider_arrived.clone();
        async move {
            requests.lock().unwrap().push(body.clone());
            let messages = body["messages"].as_array().unwrap();
            let last = messages.last().unwrap()["content"].to_string();
            let wire = if last.contains("cancel-child") {
                arrived.notify_one();
                tokio::time::sleep(Duration::from_secs(30)).await;
                stream(vec![json!({"type":"text","text":"late"})], "end_turn")
            } else if last.contains("seed-parent") {
                stream(vec![json!({"type":"thinking","thinking":"native thought","signature":"signed-native-history"}),json!({"type":"tool_use","id":"prior-effect","name":"Effect","input":{}})], "tool_use")
            } else if last.contains("fork-in-handler") {
                stream(vec![json!({"type":"tool_use","id":"fork-call","name":"Fork","input":{}})], "tool_use")
            } else if last.contains("inspect-child") {
                let inherited = messages.iter().any(|m| m.to_string().contains("parent-only-marker-7d839"));
                stream(vec![json!({"type":"text","text":if inherited {"child-private-result"} else {"clean-no-marker"}})], "end_turn")
            } else { stream(vec![json!({"type":"text","text":"parent-ready"})], "end_turn") };
            ([("content-type", "text/event-stream")], wire)
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let effects = Arc::new(AtomicUsize::new(0));
    let handler_child = Arc::new(Mutex::new(None::<Nanocodex>));
    let child_slot = handler_child.clone();
    let counter = effects.clone();
    let workspaces = Arc::new(Mutex::new(std::collections::HashMap::from([(
        "synthetic-native-parent".to_string(),
        "/synthetic/original".to_string(),
    )])));
    let child_workspaces = workspaces.clone();
    let resolved_workspaces = workspaces.clone();
    let system_workspaces = workspaces.clone();
    let tool_workspaces = workspaces.clone();
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (parent, _) = Nanocodex::builder(Claude::new(client, "claude-sonnet-5-5"))
        .session_id("synthetic-native-parent")
        .keep_thinking()
        .child_workspace_init(move |parent, child| {
            assert_ne!(parent, child);
            let mut paths = child_workspaces.lock().unwrap();
            let current = paths.get(parent).expect("parent initialized").clone();
            assert!(
                paths.insert(child.to_owned(), current).is_none(),
                "child identity reused"
            );
            Ok(())
        })
        .workspace_resolver(move |id| {
            resolved_workspaces
                .lock()
                .unwrap()
                .get(id)
                .expect("workspace initialized before resolve")
                .clone()
        })
        .system_resolver(move |id| {
            format!(
                "Current workspace: {}",
                system_workspaces
                    .lock()
                    .unwrap()
                    .get(id)
                    .expect("workspace initialized before prompt")
            )
        })
        .tools_factory(move |handle| {
            assert!(
                tool_workspaces
                    .lock()
                    .unwrap()
                    .contains_key(handle.session_id()),
                "child initializer must run before tools_factory"
            );
            let counter = counter.clone();
            let child_slot = child_slot.clone();
            Ok(ClaudeTools::new()
                .tool_with_context(tool("Effect"), move |_, _| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async {
                        Ok(ClaudeToolReply::success(ToolResultContent::Text(
                            "effect-receipt".into(),
                        )))
                    }
                })
                .tool_with_context(tool("Fork"), move |_, _| {
                    let handle = handle.clone();
                    let slot = child_slot.clone();
                    async move {
                        let (child, _) = handle.fork().await.map_err(|e| e.to_string())?;
                        let answer = child
                            .prompt("inspect-child")
                            .await
                            .map_err(|e| e.to_string())?
                            .result()
                            .await
                            .map_err(|e| e.to_string())?;
                        assert_eq!(answer.final_message(), "child-private-result");
                        *slot.lock().unwrap() = Some(child);
                        Ok(ClaudeToolReply::success(ToolResultContent::Text(
                            "child-started-without-output".into(),
                        )))
                    }
                }))
        })
        .build()
        .unwrap();
    parent
        .prompt("seed-parent parent-only-marker-7d839")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let before = trace.lock().unwrap().last().unwrap()["messages"].clone();
    workspaces.lock().unwrap().insert(
        parent.session_id().to_owned(),
        "/synthetic/moved".to_string(),
    );
    let (idle_child, _) = parent.fork().await.unwrap();
    assert_ne!(idle_child.session_id(), parent.session_id());
    assert_eq!(
        idle_child
            .prompt("inspect-child")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "child-private-result"
    );
    let child_request = trace.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        &child_request["messages"].as_array().unwrap()[..before.as_array().unwrap().len()],
        before.as_array().unwrap()
    );
    assert!(child_request.to_string().contains("signed-native-history"));
    assert!(
        child_request["system"]
            .to_string()
            .contains("/synthetic/moved")
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let (fresh, _) = parent.spawn().await.unwrap();
    assert_eq!(
        fresh
            .prompt("inspect-child")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "clean-no-marker"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        parent
            .prompt("fork-in-handler")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
    })
    .await
    .expect("fork callback must not deadlock parent's conversation mutex");
    let child = handler_child.lock().unwrap().take().unwrap();
    let callback_request = trace
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|request| {
            request["messages"].as_array().unwrap().last().unwrap()["content"]
                .to_string()
                .contains("inspect-child")
        })
        .unwrap()
        .clone();
    assert!(
        !callback_request["messages"]
            .to_string()
            .contains("fork-call"),
        "fork must exclude incomplete dispatch call"
    );
    assert_ne!(child.session_id(), idle_child.session_id());
    let turn = child.prompt("cancel-child").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), arrived.notified())
        .await
        .unwrap();
    turn.cancel().await.unwrap();
    assert!(turn.result().await.is_err());
    parent
        .prompt("parent-continues")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let final_request = trace.lock().unwrap().last().unwrap().clone();
    assert!(
        !final_request["messages"]
            .to_string()
            .contains("child-private-result")
    );
    assert!(
        !final_request["messages"]
            .to_string()
            .contains("cancel-child")
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let evidence = std::path::Path::new("../../output/native-fork-api");
    std::fs::create_dir_all(evidence).unwrap();
    std::fs::write(
        evidence.join("provider.json"),
        serde_json::to_vec_pretty(&*trace.lock().unwrap()).unwrap(),
    )
    .unwrap();
    std::fs::write(evidence.join("outcome.json"), serde_json::to_vec_pretty(&json!({"success":true,"effect_count":effects.load(Ordering::SeqCst),"parent":parent.session_id(),"idle_fork":idle_child.session_id(),"callback_fork":child.session_id(),"clean":fresh.session_id(),"signed_thinking_preserved":true,"callback_deadlock":false,"parent_context_unchanged":true,"child_cancelled":true})).unwrap()).unwrap();
    parent.shutdown().await.unwrap();
    idle_child.shutdown().await.unwrap();
    child.shutdown().await.unwrap();
    fresh.shutdown().await.unwrap();
    server.abort();
}
