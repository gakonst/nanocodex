//! Native xAI/Claude family routing through the public library's real recipes.
use axum::{Json, Router, routing::post};
use nanocodex::{
    Claude, ClaudeModel, Harness, HarnessFamily, Nanocodex, Xai, XaiModel,
    agent::SpawnOptions,
    claude::ClaudeClient,
    xai::{XaiClient, XaiTools},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn xai_routes_children_and_restores_its_native_checkpoint() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = requests.clone();
    let app = Router::new().route("/responses", post(move |Json(body): Json<Value>| {
        let capture = capture.clone(); async move {
            capture.lock().unwrap().push(body.clone());
            let output = json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"xai-result"}]}]);
            let event = json!({"type":"response.completed","response":{"id":"xai-test","status":"completed","output":output,"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}});
            ([("content-type","text/event-stream")], format!("data: {event}\n\n"))
        }
    })).route("/messages", post(move |Json(body): Json<Value>| async move {
        let frames = [
            json!({"type":"message_start","message":{"id":"claude-test","role":"assistant","model":body["model"],"content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"claude-result"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
            json!({"type":"message_stop"}),
        ];
        ([("content-type","text/event-stream")], frames.into_iter().map(|frame| format!("event: {}\ndata: {frame}\n\n", frame["type"].as_str().unwrap())).collect::<String>())
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let xai = XaiClient::new(
        reqwest::Client::new(),
        format!("http://{address}/responses"),
        "synthetic-key",
    );
    let claude = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/messages"),
        "synthetic-key",
    );
    let handles = Arc::new(Mutex::new(Vec::new()));
    let captured = handles.clone();
    let recipe = xai.clone();
    let harness = Harness::builder()
        .register(HarnessFamily::Xai, move |request| {
            let client = recipe.clone();
            let captured = captured.clone();
            async move {
                let mut builder = Nanocodex::builder(Xai::new(client, request.model.as_str()))
                    .thinking(request.thinking)
                    .spawn_factory(request.spawn_factory)
                    .host_context(request.host_context)
                    .tools_factory(move |handle| {
                        captured.lock().unwrap().push(handle);
                        Ok(XaiTools::new())
                    });
                if let Some(snapshot) = request.snapshot {
                    builder = builder.restore_runtime(snapshot)?;
                }
                builder.build()
            }
        })
        .register(HarnessFamily::Claude, move |request| {
            let client = claude.clone();
            async move {
                let mut builder = Nanocodex::builder(Claude::new(client, request.model.as_str()))
                    .thinking(request.thinking)?
                    .spawn_factory(request.spawn_factory)
                    .host_context(request.host_context);
                if let Some(snapshot) = request.snapshot {
                    builder = builder.restore_runtime(snapshot)?;
                }
                builder.build()
            }
        })
        .build();
    let (root, _) = harness.start(XaiModel::Grok46.into()).await.unwrap();
    assert_eq!(
        root.prompt("remember violet")
            .await
            .unwrap()
            .await
            .unwrap()
            .final_message(),
        "xai-result"
    );
    let owner = handles.lock().unwrap()[0].clone();
    let (child, _) = owner
        .spawn_with(
            SpawnOptions::new()
                .harness(HarnessFamily::Claude)
                .harness_model(ClaudeModel::Sonnet46.into()),
        )
        .await
        .unwrap();
    assert_eq!(child.harness_family(), HarnessFamily::Claude);
    assert_eq!(
        child
            .prompt("check child")
            .await
            .unwrap()
            .await
            .unwrap()
            .final_message(),
        "claude-result"
    );
    child.shutdown().await.unwrap();
    let snapshot = root.runtime_snapshot().await.unwrap();
    let session_id = root.session_id().to_owned();
    root.shutdown().await.unwrap();
    assert!(
        owner.spawn().await.is_err(),
        "stopped weak owners cannot construct children"
    );
    let (restored, _) = Nanocodex::builder(Xai::new(xai, "grok-4.6"))
        .restore_runtime(snapshot)
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(restored.session_id(), session_id);
    restored
        .prompt("recall violet")
        .await
        .unwrap()
        .await
        .unwrap();
    let followup = requests.lock().unwrap().last().unwrap()["input"].to_string();
    assert!(
        followup.contains("remember violet") && followup.contains("recall violet"),
        "{followup}"
    );
    restored.shutdown().await.unwrap();
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/xai");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("harness-routing.json"),
        serde_json::to_vec_pretty(&*requests.lock().unwrap()).unwrap(),
    )
    .unwrap();
    server.abort();
}
