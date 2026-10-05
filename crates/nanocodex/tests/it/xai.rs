//! Exercise the public xAI facade against the real HTTP/SSE boundary.
//! Only the external model provider is replaced with a loopback fixture.
use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use nanocodex::{
    HarnessFamily, XaiModel,
    prelude::{Nanocodex, Xai},
    xai::XaiClient,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

fn response(ordinal: usize) -> String {
    let id = format!("response-{ordinal}");
    let item_id = format!("message-{ordinal}");
    let text = format!("answer {ordinal}");
    let item = json!({"id":item_id,"type":"message","role":"assistant","status":"completed",
        "content":[{"type":"output_text","text":text,"annotations":[]}]});
    let frames = [
        json!({"type":"response.created","response":{"id":id,"status":"in_progress","output":[]}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"id":item_id,"type":"message","role":"assistant","status":"in_progress","content":[]}}),
        json!({"type":"response.content_part.added","output_index":0,"content_index":0,"item_id":item_id,"part":{"type":"output_text","text":"","annotations":[]}}),
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"item_id":item_id,"delta":text}),
        json!({"type":"response.output_text.done","output_index":0,"content_index":0,"item_id":item_id,"text":text}),
        json!({"type":"response.content_part.done","output_index":0,"content_index":0,"item_id":item_id,"part":item["content"][0]}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"id":id,"status":"completed","output":[item],"usage":{"input_tokens":4,"output_tokens":2,"total_tokens":6}}}),
    ];
    frames
        .into_iter()
        .map(|frame| {
            format!(
                "event: {}\ndata: {frame}\n\n",
                frame["type"].as_str().unwrap()
            )
        })
        .collect()
}

#[tokio::test]
async fn xai_conversation_and_failure_through_public_facade() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let observed = Arc::clone(&requests);
    let model = XaiModel::Grok46;
    let app = Router::new().route(
        "/v1/responses",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let observed = Arc::clone(&observed);
            async move {
                assert_eq!(headers["authorization"], "Bearer synthetic-xai-key");
                assert_eq!(body["model"], model.as_str());
                assert_eq!(body["stream"], true);
                assert_eq!(body["store"], false);
                let ordinal = {
                    let mut requests = observed.lock().unwrap();
                    requests.push(body);
                    requests.len()
                };
                if ordinal == 3 {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({"error":{"message":"synthetic provider rejection"}})),
                    )
                        .into_response();
                }
                ([("content-type", "text/event-stream")], response(ordinal)).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = XaiClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/responses"),
        "synthetic-xai-key",
    );
    let (agent, _events) = Nanocodex::builder(Xai::new(client, model.as_str()))
        .build()
        .unwrap();
    assert_eq!(agent.harness_family(), HarnessFamily::Xai);
    for (prompt, expected) in [("remember violet", "answer 1"), ("which word?", "answer 2")] {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            agent.prompt(prompt).await.unwrap().await.unwrap()
        })
        .await
        .expect("xAI public turn timed out");
        assert_eq!(result.final_message(), expected);
    }
    let failure = tokio::time::timeout(Duration::from_secs(10), async {
        agent
            .prompt("provider rejects this turn")
            .await
            .unwrap()
            .await
    })
    .await
    .expect("xAI rejection timed out")
    .expect_err("401 must fail the turn");
    assert!(failure.to_string().contains("401"), "{failure}");
    agent.shutdown().await.unwrap();
    assert!(agent.prompt("after shutdown").await.is_err());
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        3,
        "provider rejection must not be retried implicitly"
    );
    let follow_on = requests[1]["input"].to_string();
    assert!(follow_on.contains("remember violet"), "{follow_on}");
    assert!(follow_on.contains("answer 1"), "{follow_on}");
    assert!(follow_on.contains("which word?"), "{follow_on}");
    let evidence = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/xai");
    std::fs::create_dir_all(&evidence).unwrap();
    std::fs::write(
        evidence.join("facade-requests.json"),
        serde_json::to_vec_pretty(&*requests).unwrap(),
    )
    .unwrap();
    println!(
        "xAI facade: two retained-context turns, HTTP 401 surfaced, shutdown fenced; 3 requests recorded in output/xai/facade-requests.json"
    );
    server.abort();
}
