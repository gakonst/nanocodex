//! Public lifecycle journeys over real HTTP and fragmented SSE. Only the
//! external model service is replaced; client transport and loop are production.
use axum::{Json, Router, body::Body, response::Response, routing::post};
use futures_util::{StreamExt, stream};
use nanocodex_agent::Nanocodex;
use nanocodex_xai::{Xai, XaiClient};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;

const DEADLINE: Duration = Duration::from_secs(5);

fn frame(value: Value) -> String {
    format!(
        "event: {}\r\ndata: {value}\r\n\r\n",
        value["type"].as_str().unwrap()
    )
}

fn completed(text: &str, ordinal: usize) -> String {
    let item_id = format!("message-{ordinal}");
    let response_id = format!("response-{ordinal}");
    let item = json!({"type":"message","id":item_id,"role":"assistant",
        "status":"completed","content":[{"type":"output_text","text":text,"annotations":[]}]});
    frame(
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,
        "item_id":item_id,"delta":text}),
    ) + &frame(
        json!({"type":"response.completed","response":{"id":response_id,
        "status":"completed","output":[item],"usage":{"input_tokens":8,
        "input_tokens_details":{"cached_tokens":3},"output_tokens":2,"total_tokens":10}}}),
    )
}

fn sse(text: String) -> Response {
    // Single-byte body chunks deliberately split UTF-8 and CRLF boundaries.
    let chunks = text
        .into_bytes()
        .into_iter()
        .map(|byte| Ok::<_, Infallible>(vec![byte]));
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream::iter(chunks)))
        .unwrap()
}

fn record(name: &str, requests: &[Value]) {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/xai");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(name),
        serde_json::to_vec_pretty(requests).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn failed_samples_do_not_commit_partial_assistant_history_and_next_turn_recovers() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = Arc::clone(&requests);
    let app = Router::new().route(
        "/v1/responses",
        post(move |Json(body): Json<Value>| {
            let received = Arc::clone(&received);
            async move {
                let ordinal = {
                    let mut log = received.lock().unwrap();
                    log.push(body);
                    log.len()
                };
                let partial = frame(json!({"type":"response.output_text.delta","output_index":0,
                "content_index":0,"item_id":"partial-item","delta":"uncommitted poison"}));
                match ordinal {
                    2 => sse(partial), // EOF without a terminal response is not success.
                    4 => sse(partial
                        + &frame(json!({"type":"response.failed","response":{
                    "id":"failed","status":"failed","output":[],"error":{
                        "code":"server_error","message":"synthetic failure"}}}))),
                    6 => sse(partial
                        + &frame(json!({"type":"response.incomplete","response":{
                    "id":"filtered","status":"incomplete","output":[],
                    "incomplete_details":{"reason":"content_filter"}}}))),
                    8 => sse(partial + "event: response.completed\r\ndata: {broken-json\r\n\r\n"),
                    _ => sse(completed("valid café 🦀", ordinal)),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = XaiClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/responses"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Xai::new(client, "grok-4.6"))
        .build()
        .unwrap();
    let first = tokio::time::timeout(
        DEADLINE,
        agent.prompt("remember the seed").await.unwrap().result(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(first.final_message(), "valid café 🦀");
    for failure in [
        "premature EOF",
        "provider failure",
        "content filter",
        "invalid JSON",
    ] {
        let failed = tokio::time::timeout(DEADLINE, agent.prompt(failure).await.unwrap().result())
            .await
            .expect("invalid provider response must terminate");
        assert!(failed.is_err(), "{failure} was incorrectly accepted");
        let recovered = tokio::time::timeout(
            DEADLINE,
            agent
                .prompt("recover using committed context")
                .await
                .unwrap()
                .result(),
        )
        .await
        .expect("recovery turn must terminate")
        .unwrap();
        assert_eq!(recovered.final_message(), "valid café 🦀");
    }
    agent.shutdown().await.unwrap();
    let log = requests.lock().unwrap();
    assert_eq!(
        log.len(),
        9,
        "failed samples must not be silently resubmitted"
    );
    for request in log.iter().skip(1) {
        let input = request["input"].to_string();
        assert!(
            input.contains("remember the seed"),
            "committed user context was lost: {input}"
        );
        assert!(
            input.contains("valid café 🦀"),
            "committed assistant context was lost: {input}"
        );
        assert!(
            !input.contains("uncommitted poison"),
            "failed partial text was committed: {input}"
        );
    }
    record("protocol-failure-requests.json", &log);
    println!(
        "xAI HTTP journey: fragmented UTF-8/CRLF success; EOF, failed, filtered and malformed SSE reject; four recovery turns retain committed context (9 requests)"
    );
    server.abort();
}

#[tokio::test]
async fn cancel_a_live_response_then_start_a_clean_followup() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = Arc::clone(&requests);
    let streaming = Arc::new(Notify::new());
    let stream_started = Arc::clone(&streaming);
    let app = Router::new().route("/v1/responses", post(move |Json(body): Json<Value>| {
        let received = Arc::clone(&received);
        let stream_started = Arc::clone(&stream_started);
        async move {
            let ordinal = { let mut log = received.lock().unwrap(); log.push(body); log.len() };
            if ordinal != 1 { return sse(completed("after cancellation", ordinal)); }
            let first = stream::once(async move {
                stream_started.notify_one();
                Ok::<_, Infallible>(frame(json!({"type":"response.output_text.delta",
                    "output_index":0,"content_index":0,"item_id":"cancelled-item","delta":"cancelled partial"})))
            });
            Response::builder().header("content-type", "text/event-stream")
                .body(Body::from_stream(first.chain(stream::pending()))).unwrap()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = XaiClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/responses"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Xai::new(client, "grok-4.6"))
        .build()
        .unwrap();
    let turn = agent.prompt("start a slow response").await.unwrap();
    tokio::time::timeout(DEADLINE, streaming.notified())
        .await
        .expect("provider stream did not start");
    tokio::time::timeout(DEADLINE, turn.cancel())
        .await
        .expect("cancellation did not stop the live transport")
        .unwrap();
    assert!(
        tokio::time::timeout(DEADLINE, turn.result())
            .await
            .unwrap()
            .is_err()
    );
    let next = tokio::time::timeout(
        DEADLINE,
        agent
            .prompt("continue after cancellation")
            .await
            .unwrap()
            .result(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(next.final_message(), "after cancellation");
    agent.shutdown().await.unwrap();
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 2);
    assert!(!log[1]["input"].to_string().contains("cancelled partial"));
    record("cancel-requests.json", &log);
    println!(
        "xAI HTTP journey: live hanging SSE cancelled; terminal failure observed; next turn succeeds without cancelled partial history (2 requests)"
    );
    server.abort();
}

fn tool_response(output: Vec<Value>, ordinal: usize) -> String {
    frame(
        json!({"type":"response.completed","response":{"id":format!("tools-{ordinal}"),
        "status":"completed","output":output,"usage":{"input_tokens":5,"output_tokens":4,"total_tokens":9}}}),
    )
}

fn call(id: &str, name: &str, arguments: &str) -> Value {
    json!({"type":"function_call","id":format!("item-{id}"),"call_id":id,
        "status":"completed","name":name,"arguments":arguments})
}

#[tokio::test]
async fn tool_errors_are_paired_and_incomplete_or_replayed_calls_have_no_effect() {
    use nanocodex_xai::ToolDefinition;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = Arc::clone(&requests);
    let app = Router::new().route("/v1/responses", post(move |Json(body): Json<Value>| {
        let received = Arc::clone(&received);
        async move {
            let ordinal = { let mut log = received.lock().unwrap(); log.push(body); log.len() };
            let text = match ordinal {
                1 => frame(json!({"type":"response.output_item.done","output_index":0,
                    "item":call("incomplete-call", "effect", "{}")})),
                2 => tool_response(vec![
                    json!({"type":"reasoning","id":"reasoning-2","status":"completed",
                        "encrypted_content":"opaque-test-reasoning","summary":[],"content":[{"text":"reasoning context"}]}),
                    call("effect-once", "effect", "{}"),
                    call("unknown-tool", "unknown", "{}"),
                    call("invalid-arguments", "effect", "{invalid"),
                    call("callback-error", "fail", "{}"),
                    json!({"type":"custom_tool_call","id":"hosted-search","call_id":"server-call",
                        "name":"effect","input":"provider hosted search","status":"completed"}),
                ], ordinal),
                3 => completed("paired error recovery", ordinal),
                4 => tool_response(vec![call("effect-once", "effect", "{}")], ordinal),
                5 => tool_response(vec![call("not-admitted", "effect", "{}"),
                    json!({"type":"function_call","id":"missing-call-id","name":"effect","arguments":"{}"})], ordinal),
                _ => completed("safe after invalid tools", ordinal),
            };
            sse(text)
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let effects = Arc::new(AtomicUsize::new(0));
    let executed = Arc::clone(&effects);
    let client = XaiClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/responses"),
        "synthetic",
    );
    let definition = |name: &str| ToolDefinition {
        name: name.into(),
        description: "Synthetic authorized callback".into(),
        parameters: json!({"type":"object"}),
    };
    let (agent, _) = Nanocodex::builder(Xai::new(client, "grok-4.6"))
        .tool(definition("effect"), move |_| {
            executed.fetch_add(1, Ordering::SeqCst);
            async { Ok("committed receipt".into()) }
        })
        .tool(definition("fail"), |_| async {
            Err("synthetic callback failure".into())
        })
        .build()
        .unwrap();
    assert!(
        tokio::time::timeout(
            DEADLINE,
            agent
                .prompt("incomplete tool stream")
                .await
                .unwrap()
                .result()
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(
        effects.load(Ordering::SeqCst),
        0,
        "a done item without a terminal response must never dispatch"
    );
    let result = tokio::time::timeout(
        DEADLINE,
        agent
            .prompt("perform one authorized effect")
            .await
            .unwrap()
            .result(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.final_message(), "paired error recovery");
    assert_eq!(
        effects.load(Ordering::SeqCst),
        1,
        "unknown, malformed and hosted calls must not dispatch effect"
    );
    for prompt in ["try repeated call identity", "try malformed batch identity"] {
        assert!(
            tokio::time::timeout(DEADLINE, agent.prompt(prompt).await.unwrap().result())
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(
            effects.load(Ordering::SeqCst),
            1,
            "rejected identity must fail before any batch callback"
        );
    }
    let recovered = tokio::time::timeout(
        DEADLINE,
        agent
            .prompt("recover without any tools")
            .await
            .unwrap()
            .result(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(recovered.final_message(), "safe after invalid tools");
    agent.shutdown().await.unwrap();
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 6);
    let input = log[2]["input"].as_array().unwrap();
    let results: Vec<_> = input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect();
    assert_eq!(
        results.len(),
        4,
        "each client function must receive one result"
    );
    for (id, expected) in [
        ("effect-once", "committed receipt"),
        ("unknown-tool", "Unknown tool"),
        ("invalid-arguments", "JSON object"),
        ("callback-error", "synthetic callback failure"),
    ] {
        let result = results.iter().find(|item| item["call_id"] == id).unwrap();
        assert!(result["output"].as_str().unwrap().contains(expected));
    }
    for item in input.iter().filter(|item| item["type"] == "function_call") {
        assert!(
            serde_json::from_str::<Value>(item["arguments"].as_str().unwrap()).is_ok(),
            "replayed function arguments must be valid JSON to avoid provider rejection: {item}"
        );
    }
    let reasoning = input
        .iter()
        .find(|item| item["type"] == "reasoning")
        .unwrap();
    assert!(reasoning.get("status").is_none());
    assert_eq!(reasoning["content"][0]["type"], "reasoning_text");
    assert_eq!(reasoning["encrypted_content"], "opaque-test-reasoning");
    assert!(input.iter().any(|item| item["id"] == "hosted-search"));
    assert!(
        !input
            .iter()
            .any(|item| item["call_id"] == "incomplete-call")
    );
    record("tool-error-requests.json", &log);
    println!(
        "xAI HTTP journey: incomplete tool never runs; four client calls get paired results; malformed/unknown errors recover; native reasoning and hosted calls replay; duplicate and malformed call identities execute no effects (6 requests, 1 effect)"
    );
    server.abort();
}

#[tokio::test]
async fn cancellation_acknowledges_a_started_tool_only_after_its_receipt_is_retained() {
    use nanocodex_xai::ToolDefinition;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = Arc::clone(&requests);
    let app = Router::new().route(
        "/v1/responses",
        post(move |Json(body): Json<Value>| {
            let received = Arc::clone(&received);
            async move {
                let ordinal = {
                    let mut log = received.lock().unwrap();
                    log.push(body);
                    log.len()
                };
                sse(if ordinal == 1 {
                    tool_response(vec![call("slow-effect", "effect", "{}")], ordinal)
                } else {
                    completed("after tool cancellation", ordinal)
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let started = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    let effects = Arc::new(AtomicUsize::new(0));
    let (callback_started, callback_finish, callback_effects) = (
        Arc::clone(&started),
        Arc::clone(&finish),
        Arc::clone(&effects),
    );
    let client = XaiClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/responses"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Xai::new(client, "grok-4.6"))
        .tool(
            ToolDefinition {
                name: "effect".into(),
                description: "Synthetic bounded effect".into(),
                parameters: json!({"type":"object"}),
            },
            move |_| {
                let (started, finish, effects) = (
                    Arc::clone(&callback_started),
                    Arc::clone(&callback_finish),
                    Arc::clone(&callback_effects),
                );
                async move {
                    started.notify_one();
                    finish.notified().await;
                    effects.fetch_add(1, Ordering::SeqCst);
                    Ok("retained slow-effect receipt".into())
                }
            },
        )
        .build()
        .unwrap();
    let turn = agent.prompt("perform the slow effect").await.unwrap();
    tokio::time::timeout(DEADLINE, started.notified())
        .await
        .unwrap();
    let control = turn.control();
    let cancel = control.cancel();
    tokio::pin!(cancel);
    let early_ack = tokio::time::timeout(Duration::from_millis(50), &mut cancel).await;
    finish.notify_one();
    if early_ack.is_err() {
        tokio::time::timeout(DEADLINE, &mut cancel)
            .await
            .unwrap()
            .unwrap();
    }
    let outcome = tokio::time::timeout(DEADLINE, turn.result()).await.unwrap();
    assert!(outcome.is_err());
    assert!(
        early_ack.is_err(),
        "cancel acknowledged while a host effect was still running"
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let recovered = tokio::time::timeout(
        DEADLINE,
        agent
            .prompt("continue after retained receipt")
            .await
            .unwrap()
            .result(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(recovered.final_message(), "after tool cancellation");
    agent.shutdown().await.unwrap();
    let log = requests.lock().unwrap();
    assert_eq!(
        log.len(),
        2,
        "cancelled turn must not resample after its effect"
    );
    let input = log[1]["input"].as_array().unwrap();
    assert_eq!(
        input
            .iter()
            .filter(
                |item| item["type"] == "function_call_output" && item["call_id"] == "slow-effect"
            )
            .count(),
        1
    );
    assert!(
        input
            .iter()
            .any(|item| item["output"] == "retained slow-effect receipt")
    );
    record("tool-cancel-requests.json", &log);
    println!(
        "xAI HTTP journey: cancellation waits for started effect; one committed receipt survives; cancelled turn never resamples and follow-up succeeds (2 requests, 1 effect)"
    );
    server.abort();
}

#[tokio::test]
async fn truncated_http_rejection_is_not_retried() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = requests.clone();
    let app = Router::new().route(
        "/v1/responses",
        post(move |Json(body): Json<Value>| {
            let received = received.clone();
            async move {
                let ordinal = {
                    let mut log = received.lock().unwrap();
                    log.push(body);
                    log.len()
                };
                if ordinal != 2 {
                    return sse(completed("committed response", ordinal));
                }
                let body = stream::once(async { Ok::<_, std::io::Error>("{\"error\":") }).chain(
                    stream::once(async {
                        // Ensure the status and partial body reach the actual HTTP client
                        // before the external provider disconnects mid-response.
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Err(std::io::Error::other("synthetic provider disconnect"))
                    }),
                );
                Response::builder()
                    .status(503)
                    .header("content-type", "application/json")
                    .body(Body::from_stream(body))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (agent, _) = Xai::new(
        XaiClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/responses"),
            "synthetic",
        ),
        "grok-4.6",
    )
    .max_retries(1)
    .build()
    .unwrap();
    agent
        .prompt("remember committed context")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let failure = tokio::time::timeout(
        DEADLINE,
        agent
            .prompt("interrupted rejection")
            .await
            .unwrap()
            .result(),
    )
    .await
    .unwrap();
    assert!(
        failure.is_err(),
        "partial HTTP rejection must not authorize retry"
    );
    assert_eq!(requests.lock().unwrap().len(), 2);
    assert_eq!(
        agent
            .prompt("explicit followup")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "committed response"
    );
    let log = requests.lock().unwrap().clone();
    assert_eq!(log.len(), 3);
    assert!(
        log[2]["input"]
            .to_string()
            .contains("remember committed context")
    );
    record("truncated-http-rejection.json", &log);
    agent.shutdown().await.unwrap();
    server.abort();
    println!(
        "HTTP journey: disconnected 503 body fails once; explicit followup retains committed context (3 requests)"
    );
}
