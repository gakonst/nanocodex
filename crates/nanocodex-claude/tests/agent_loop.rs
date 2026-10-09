//! End-to-end behavioral contract against a synthetic loopback Messages API.
use axum::{Json, Router, response::IntoResponse, routing::post};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use image::{DynamicImage, ImageFormat};
use nanocodex_agent::{Nanocodex, events::AgentEventKind};
use nanocodex_claude::{Claude, ClaudeClient, Effort, ToolDefinition};
use serde_json::{Value, json};
use std::{
    io::Cursor,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

fn stream(blocks: Vec<Value>, stop: &str) -> String {
    let mut out = String::new();
    let mut emit = |event: Value| out.push_str(&format!("data: {event}\n\n"));
    emit(
        json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"test","content":[],"usage":{"input_tokens":3,"cache_read_input_tokens":2,"cache_creation_input_tokens":1,"output_tokens":0}}}),
    );
    for (i, block) in blocks.iter().enumerate() {
        let start = match block["type"].as_str() {
            Some("text") => json!({"type":"text","text":""}),
            Some("thinking") => {
                json!({"type":"thinking","thinking":"","signature":block["signature"],"binding":block["binding"]})
            }
            _ => json!({"type":"tool_use","id":block["id"],"name":block["name"],"input":{}}),
        };
        emit(json!({"type":"content_block_start","index":i,"content_block":start}));
        let delta = match block["type"].as_str() {
            Some("text") => json!({"type":"text_delta","text":block["text"]}),
            Some("thinking") => json!({"type":"thinking_delta","thinking":block["thinking"]}),
            _ => json!({"type":"input_json_delta","partial_json":block["input"].to_string()}),
        };
        emit(json!({"type":"content_block_delta","index":i,"delta":delta}));
        emit(json!({"type":"content_block_stop","index":i}));
    }
    emit(json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}));
    emit(json!({"type":"message_stop"}));
    out
}

fn png(width: u32, height: u32) -> String {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(width, height)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    STANDARD.encode(bytes.into_inner())
}

fn image_dimensions(block: &Value) -> Option<(u32, u32)> {
    let bytes = STANDARD.decode(block["source"]["data"].as_str()?).ok()?;
    let image = image::load_from_memory(&bytes).ok()?;
    Some((image.width(), image.height()))
}

/// A PNG of pseudo-random pixels, which lossless encodings cannot compress.
fn noise_png(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let noise = image::RgbImage::from_fn(width, height, |_, _| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let bytes = state.to_le_bytes();
        image::Rgb([bytes[0], bytes[1], bytes[2]])
    });
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(noise)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

/// A 1x1 JPEG whose application segments alone exceed the 5 MiB per-image
/// limit, so it cannot shrink to fit.
fn bloated_jpeg() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(1, 1)
        .write_to(&mut bytes, ImageFormat::Jpeg)
        .unwrap();
    let mut bytes = bytes.into_inner();
    let segment = [&[0xff, 0xef, 0xff, 0xff][..], &[0; 65_533]].concat();
    bytes.splice(2..2, segment.repeat(100));
    bytes
}

#[tokio::test]
async fn calls_outside_the_catalog_get_paired_errors_and_the_turn_continues() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let unknown_name = "unknown界".repeat(10_000);
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let app = Router::new().route(
        "/v1/messages",
        post({
            let requests = requests.clone();
            let unknown_name = unknown_name.clone();
            move |Json(body): Json<Value>| {
                let requests = requests.clone();
                let unknown_name = unknown_name.clone();
                async move {
                    let index = {
                        let mut log = requests.lock().unwrap();
                        log.push(body);
                        log.len()
                    };
                    let (blocks, stop) = match index {
                        1 => (vec![
                            json!({"type":"tool_use","id":"denied-plan","name":"update_plan","input":{}}),
                            json!({"type":"tool_use","id":"denied-long-name","name":unknown_name,"input":{}}),
                        ], "tool_use"),
                        2 => (vec![json!({"type":"tool_use","id":"corrected","name":"exec","input":{}})], "tool_use"),
                        3 => (vec![json!({"type":"text","text":"corrected and completed"})], "end_turn"),
                        _ => (vec![json!({"type":"tool_use","id":"denied-plan","name":"exec","input":{}})], "tool_use"),
                    };
                    ([("content-type", "text/event-stream")], stream(blocks, stop))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let (agent, _) = Nanocodex::builder(Claude::new(
        ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic"),
        "test",
    ))
    .max_tokens(128_000)
    .tool(
        ToolDefinition {
            name: "exec".into(),
            description: "Synthetic admitted effect".into(),
            input_schema: json!({"type":"object"}),
            strict: None,
            defer_loading: false,
        },
        move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok("committed once".into()) }
        },
    )
    .build()
    .unwrap();
    let outcome = agent
        .prompt("complete using the admitted tool")
        .await
        .unwrap()
        .result()
        .await;
    assert_eq!(outcome.unwrap().final_message(), "corrected and completed");
    let log = requests.lock().unwrap().clone();
    assert_eq!(log.len(), 3);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let denied = &log[1]["messages"];
    for (index, (id, name)) in [
        ("denied-plan", "update_plan"),
        ("denied-long-name", unknown_name.as_str()),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(denied[1]["content"][index]["id"], id);
        let result = &denied[2]["content"][index];
        assert_eq!(result["tool_use_id"], id);
        assert_eq!(result["is_error"], true);
        let reason = result["content"].as_str().unwrap();
        assert!(reason.len() <= 256, "denial must have a bounded reason");
        assert!(
            !reason.contains(name),
            "denial must not echo the requested name"
        );
    }
    let success = &log[2]["messages"][4]["content"][0];
    assert_eq!(success["tool_use_id"], "corrected");
    assert_eq!(success["content"], "committed once");

    let reused = agent
        .prompt("reuse the denied identity")
        .await
        .unwrap()
        .result()
        .await
        .unwrap_err();
    assert!(
        reused
            .to_string()
            .contains("reused an admitted tool_use id")
    );
    assert_eq!(
        effects.load(Ordering::SeqCst),
        1,
        "a denied identity cannot later authorize an effect"
    );
    agent.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn million_token_models_use_their_window_before_compacting() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for model in ["claude-sonnet-5-5", "claude-haiku-5-5"] {
        let calls = Arc::new(AtomicUsize::new(0));
        let received = calls.clone();
        let app = Router::new().route(
            "/v1/messages",
            post(move |Json(body): Json<Value>| {
                let received = received.clone();
                async move {
                    assert_eq!(body["model"], model);
                    received.fetch_add(1, Ordering::SeqCst);
                    let response = stream(vec![json!({"type":"text","text":"answer"})], "end_turn")
                        .replace("\"input_tokens\":3", "\"input_tokens\":300000");
                    ([("content-type", "text/event-stream")], response)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        );
        let (agent, _) = Nanocodex::builder(Claude::new(client, model))
            .build()
            .unwrap();
        for prompt in ["first", "second"] {
            assert_eq!(
                agent
                    .prompt(prompt)
                    .await
                    .unwrap()
                    .result()
                    .await
                    .unwrap()
                    .final_message(),
                "answer"
            );
        }
        agent.shutdown().await.unwrap();
        server.abort();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "300K input must not trigger compaction in {model}'s 1M window"
        );
    }
}

#[tokio::test]
async fn stream_tool_once_compact_and_failed_turn_preserves_history() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = received.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let requests = requests.clone();
        async move {
            let index = { let mut r = requests.lock().unwrap(); r.push(body.clone()); r.len() };
            if index == 5 { return (axum::http::StatusCode::BAD_REQUEST, "synthetic failure".to_string()).into_response(); }
            let (blocks, reason) = match index {
                1 => (vec![json!({"type":"text","text":"Hello "}),json!({"type":"text","text":"world"})], "end_turn"),
                2 => (vec![json!({"type":"thinking","thinking":"","signature":"signed-tool-turn","binding":"opaque"}),json!({"type":"tool_use","id":"tool-1","name":"lookup","input":{"key":"x"}})], "tool_use"),
                3 => (vec![json!({"type":"text","text":"found value"})], "end_turn"),
                4 => (vec![json!({"type":"text","text":"SUMMARY: user greeting and lookup x = value"})], "end_turn"),
                _ => (vec![json!({"type":"text","text":"after failure"})], "end_turn"),
            };
            ([("content-type","text/event-stream")], stream(blocks,reason)).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let claude = Claude::new(client, "test");
    let (agent, mut events) = Nanocodex::builder(claude)
        .max_tokens(128)
        .automatic_cache(true)
        .effort(Effort::High)
        .tool(
            ToolDefinition {
                name: "lookup".into(),
                description: "Test lookup".into(),
                input_schema: json!({"type":"object","properties":{"key":{"type":"string"}}}),
                strict: None,
                defer_loading: false,
            },
            move |input| {
                counter.fetch_add(1, Ordering::SeqCst);
                async move { Ok(format!("value for {}", input["key"])) }
            },
        )
        .build()
        .unwrap();
    let first = agent.prompt("hello").await.unwrap().result().await.unwrap();
    assert_eq!(first.final_message(), "Hello world");
    assert_eq!(first.usage().unwrap().cached_input_tokens(), 2);
    assert_eq!(first.usage().unwrap().cache_write_input_tokens(), 1);
    assert_eq!(first.usage().unwrap().total_tokens(), 11);
    assert_eq!(
        first.usage().unwrap().cost_status(),
        nanocodex_agent::CostStatus::Other
    );
    assert_eq!(
        agent
            .prompt("lookup x")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "found value"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // /compact is a normal summary request. The old history is swapped only after success.
    agent.compact().await.unwrap();
    assert!(
        agent
            .prompt("fail now")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(
        agent
            .prompt("continue")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "after failure"
    );
    let mut kinds = Vec::new();
    let mut next_seq = 1u64;
    while kinds
        .iter()
        .filter(|kind| {
            matches!(
                kind,
                AgentEventKind::RunCompleted | AgentEventKind::RunFailed
            )
        })
        .count()
        < 4
    {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.seq, next_seq, "lost or misordered backend event");
        next_seq += 1;
        kinds.push(event.kind);
    }
    assert!(kinds.contains(&AgentEventKind::AssistantDelta));
    assert!(kinds.contains(&AgentEventKind::ToolCall));
    assert!(kinds.contains(&AgentEventKind::ToolResult));
    let log = received.lock().unwrap();
    assert_eq!(log.len(), 6);
    assert_eq!(log[0]["stream"], true);
    assert_eq!(log[0]["cache_control"], json!({"type":"ephemeral"}));
    assert_eq!(log[0]["output_config"], json!({"effort":"high"}));
    assert_eq!(log[3]["cache_control"], json!({"type":"ephemeral"}));
    assert_eq!(log[2]["messages"][3]["role"], "assistant");
    assert_eq!(
        log[2]["messages"][3]["content"][0]["signature"],
        "signed-tool-turn"
    );
    assert_eq!(log[2]["messages"][3]["content"][0]["binding"], "opaque");
    assert_eq!(log[2]["messages"][4]["role"], "user");
    assert_eq!(log[2]["messages"][4]["content"][0]["type"], "tool_result");
    assert_eq!(log[2]["messages"][4]["content"][0]["tool_use_id"], "tool-1");
    assert_eq!(
        log[2]["messages"][4]["content"][0]["content"],
        "value for \"x\""
    );
    assert!(log[3]["messages"].as_array().unwrap().len() > 4);
    // The observed Claude Code continuation installs the summary as USER
    // context, not as an API system prompt or a fabricated signed block.
    assert_eq!(log[4]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(log[5]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        log[4]["messages"][1],
        json!({"role":"user","content":[{"type":"text","text":"fail now"}]})
    );
    assert_eq!(
        log[5]["messages"][1],
        json!({"role":"user","content":[{"type":"text","text":"continue"}]})
    );
    assert_eq!(
        log[4]["messages"][0], log[5]["messages"][0],
        "a failed turn must preserve the prior summary"
    );
    let resumed = log[5]["messages"][0]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(log[5]["messages"][0]["role"], "user");
    assert!(resumed.contains("SUMMARY: user greeting"));
    assert!(
        !resumed.contains("fail now"),
        "failed input must not enter retained history"
    );
    assert!(log[5].get("system").is_none());
    server.abort();
}

#[tokio::test]
async fn failed_compaction_and_cancelled_turn_keep_previous_context() {
    use nanocodex_agent::PromptRequest;
    use tokio::sync::Notify;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let arrived = Arc::new(Notify::new());
    let app = Router::new().route(
        "/v1/messages",
        post({
            let requests = requests.clone();
            let arrived = arrived.clone();
            move |Json(body): Json<Value>| {
                let requests = requests.clone();
                let arrived = arrived.clone();
                async move {
                    let index = {
                        let mut log = requests.lock().unwrap();
                        log.push(body);
                        log.len()
                    };
                    match index {
                        2 => (
                            axum::http::StatusCode::BAD_REQUEST,
                            "synthetic compact failure".to_string(),
                        )
                            .into_response(),
                        4 => {
                            arrived.notify_one();
                            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                            (
                                [("content-type", "text/event-stream")],
                                stream(
                                    vec![json!({"type":"text","text":"do not commit"})],
                                    "end_turn",
                                ),
                            )
                                .into_response()
                        }
                        _ => (
                            [("content-type", "text/event-stream")],
                            stream(
                                vec![json!({"type":"text","text":format!("ok-{index}")})],
                                "end_turn",
                            ),
                        )
                            .into_response(),
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{addr}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .build()
        .unwrap();
    agent.prompt("first").await.unwrap().result().await.unwrap();
    assert!(agent.compact().await.is_err());
    agent
        .prompt("after failed compact")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let notified = arrived.notified();
    let turn = agent.prompt("cancel me").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), notified)
        .await
        .unwrap();
    turn.cancel().await.unwrap();
    assert!(turn.result().await.is_err());
    assert!(
        agent
            .prompt(PromptRequest::new("cancel on admission").cancel_on_admission())
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(
        agent
            .prompt("after cancel")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "ok-5"
    );
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 5);
    assert_eq!(log[2]["messages"][0]["content"][0]["text"], "first");
    assert_eq!(log[4]["messages"].as_array().unwrap().len(), 5);
    assert_eq!(log[4]["messages"][4]["content"][0]["text"], "after cancel");
    server.abort();
}

#[tokio::test]
async fn auto_compacts_at_usage_threshold_before_next_prompt() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let index = {
                    let mut log = seen.lock().unwrap();
                    log.push(body);
                    log.len()
                };
                let text = match index {
                    1 => "first answer",
                    2 => "carry first answer",
                    _ => "second answer",
                };
                (
                    [("content-type", "text/event-stream")],
                    stream(vec![json!({"type":"text","text":text})], "end_turn"),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{addr}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .context_window_tokens(10)
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("first")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "first answer"
    );
    let second = agent
        .prompt("second")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(second.final_message(), "second answer");
    // Both the automatic summary and the answer belong to this turn's usage.
    let usage = second.usage().unwrap();
    assert_eq!(usage.input_tokens(), 6);
    assert_eq!(usage.cached_input_tokens(), 4);
    assert_eq!(usage.cache_write_input_tokens(), 2);
    assert_eq!(usage.output_tokens(), 10);
    assert_eq!(usage.total_tokens(), 22);
    let log = requests.lock().unwrap();
    assert_eq!(
        log.len(),
        3,
        "one summary generation before the second prompt"
    );
    assert!(log[1]["messages"].as_array().unwrap().len() >= 3);
    let next = log[2]["messages"][0]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(next.contains("carry first answer"));
    assert_eq!(log[2]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        log[2]["messages"][1],
        json!({"role":"user","content":[{"type":"text","text":"second"}]})
    );
    assert!(!next.contains("first answer\n\nfirst answer"));
    server.abort();
}

#[tokio::test]
async fn latest_model_sends_opus_5_5_without_legacy_thinking_parameters() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let received = Arc::new(Mutex::new(None::<Value>));
    let captured = received.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                *captured.lock().unwrap() = Some(body);
                (
                    [("content-type", "text/event-stream")],
                    stream(vec![json!({"type":"text","text":"ok"})], "end_turn"),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::latest(client)).build().unwrap();
    assert_eq!(
        agent
            .prompt("hello")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "ok"
    );
    let body = received.lock().unwrap().clone().unwrap();
    assert_eq!(body["model"], "claude-opus-5-5");
    assert!(body.get("thinking").is_none());
    assert!(body.get("tool_choice").is_none());
    assert!(
        body.get("tools").is_none(),
        "no Codex tools are implicitly exposed"
    );
}

#[tokio::test]
async fn latest_model_does_not_compact_at_legacy_200k_window() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let received = Arc::new(AtomicUsize::new(0));
    let count = received.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(_): Json<Value>| {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                let output = stream(vec![json!({"type":"text","text":"ok"})], "end_turn")
                    .replace("\"input_tokens\":3", "\"input_tokens\":250000");
                ([("content-type", "text/event-stream")], output).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::latest(client)).build().unwrap();
    let first = agent.prompt("one").await.unwrap().result().await.unwrap();
    assert_eq!(first.usage().unwrap().input_tokens(), 250_000);
    agent.prompt("two").await.unwrap().result().await.unwrap();
    assert_eq!(
        received.load(Ordering::SeqCst),
        2,
        "unexpected early compaction"
    );
}

#[tokio::test]
async fn independent_tools_can_execute_concurrently_but_results_remain_one_ordered_user_message() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = captured.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let received = received.clone();
        async move {
            let index = { let mut requests = received.lock().unwrap(); requests.push(body); requests.len() };
            let blocks = if index == 1 {
                vec![json!({"type":"tool_use","id":"toolu_a","name":"lookup","input":{"key":"a"}}),json!({"type":"tool_use","id":"toolu_b","name":"lookup","input":{"key":"b"}})]
            } else { vec![json!({"type":"text","text":"done"})] };
            ([ ("content-type","text/event-stream") ], stream(blocks, if index == 1 {"tool_use"} else {"end_turn"})).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .parallel_tools(true) // caller asserts the registered tool invocations are independent
        .tool(
            ToolDefinition {
                name: "lookup".into(),
                description: "Read a key".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            move |input| {
                let barrier = barrier.clone();
                async move {
                    barrier.wait().await;
                    if input["key"] == "b" {
                        Err("not found".into())
                    } else {
                        Ok("value a".into())
                    }
                }
            },
        )
        .build()
        .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        agent
            .prompt("read both")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
    })
    .await
    .expect("tools did not overlap");
    assert_eq!(result.final_message(), "done");
    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let returned = &requests[1]["messages"][2];
    assert_eq!(returned["role"], "user");
    assert_eq!(returned["content"].as_array().unwrap().len(), 2);
    assert_eq!(returned["content"][0]["tool_use_id"], "toolu_a");
    assert_eq!(returned["content"][1]["tool_use_id"], "toolu_b");
    assert_eq!(returned["content"][1]["is_error"], true);
}

#[tokio::test]
async fn compaction_accepts_latest_model_thinking_before_text_summary() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = captured.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let requests = requests.clone();
        async move {
            let index = { let mut r = requests.lock().unwrap(); r.push(body); r.len() };
            let output = if index == 2 {
                let mut text = String::new();
                for event in [
                    json!({"type":"message_start","message":{"id":"summary","role":"assistant","model":"claude-opus-5-5","content":[],"usage":{"input_tokens":25,"output_tokens":0}}}),
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":"sig"}}),
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"Summary of first turn"}}),
                    json!({"type":"content_block_stop","index":1}),
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}),
                    json!({"type":"message_stop"}),
                ] { text.push_str(&format!("data: {event}\n\n")); }
                text
            } else { stream(vec![json!({"type":"text","text":"ok"})],"end_turn") };
            ([ ("content-type","text/event-stream") ], output).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::latest(client)).build().unwrap();
    agent.prompt("first").await.unwrap().result().await.unwrap();
    agent.compact().await.unwrap();
    agent
        .prompt("second")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        requests[2]["messages"][1],
        json!({"role":"user","content":[{"type":"text","text":"second"}]})
    );
    assert!(
        requests[2]["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Summary of first turn")
    );
}

#[tokio::test]
async fn claude_client_tool_can_return_multimodal_blocks_without_codex_result_shape() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app=Router::new().route("/v1/messages",post(move |Json(body):Json<Value>| {
        let log=log.clone();
        async move {
            let index={let mut r=log.lock().unwrap();r.push(body);r.len()};
            let (blocks,reason)=if index==1 {(vec![json!({"type":"tool_use","id":"toolu_media","name":"ReadImage","input":{"path":"illustration.png"}})],"tool_use")}
                else {(vec![json!({"type":"text","text":"seen"})],"end_turn")};
            ([ ("content-type","text/event-stream") ],stream(blocks,reason)).into_response()
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
    let (agent,_)=Nanocodex::builder(Claude::new(client,"test")).max_tokens(128_000)
        .tool_blocks(ToolDefinition { name:"ReadImage".into(), description:"Test image".into(), input_schema:json!({"type":"object"}),strict:None,defer_loading:false }, |_| async {
            Ok(vec![json!({"type":"text","text":"image follows"}),json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":png(1, 1)}})])
        }).build().unwrap();
    assert_eq!(
        agent
            .prompt("read image")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "seen"
    );
    let log = requests.lock().unwrap();
    assert_eq!(
        log[1]["messages"][2]["content"][0]["content"][1]["source"]["data"],
        png(1, 1)
    );
    assert_eq!(log[1]["messages"][2]["content"][0]["type"], "tool_result");
    server.abort();
}

#[tokio::test]
async fn tool_images_fit_many_image_limit_before_history_crosses_twenty() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            async move {
                // Mirror the Messages API: every image must decode, and a request
                // with more than twenty images applies a stricter dimension limit.
                let images: Vec<Value> = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|message| message["content"].as_array().unwrap())
                    .flat_map(|block| block["content"].as_array().into_iter().flatten())
                    .filter(|block| block["type"] == "image")
                    .cloned()
                    .collect();
                let limit = if images.len() > 20 { 3000 } else { 8000 };
                let index = {
                    let mut log = log.lock().unwrap();
                    log.push(body);
                    log.len()
                };
                if !images.iter().all(|image| {
                    image_dimensions(image).is_some_and(|(width, height)| width.max(height) <= limit)
                }) {
                    return (
                        axum::http::StatusCode::BAD_REQUEST,
                        "invalid or oversized image",
                    )
                        .into_response();
                }
                let (blocks, reason) = if index < 3 {
                    (
                        vec![json!({"type":"tool_use","id":format!("capture-{index}"),"name":"Capture","input":{"batch":index}})],
                        "tool_use",
                    )
                } else {
                    (vec![json!({"type":"text","text":"seen all"})], "end_turn")
                };
                ([("content-type", "text/event-stream")], stream(blocks, reason)).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let capture = ToolDefinition {
        name: "Capture".into(),
        description: "Capture synthetic screenshots".into(),
        input_schema: json!({"type":"object"}),
        strict: None,
        defer_loading: false,
    };
    let image = |data: String| json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":data}});
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tool_blocks(capture, move |input| {
            let blocks = if input["batch"] == 1 {
                vec![
                    json!({"type":"text","text":"captured"}),
                    image(png(4000, 200)),
                    image("cG5n".into()),
                ]
            } else {
                vec![image(png(16, 16)); 20]
            };
            async move { Ok(blocks) }
        })
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("capture twice")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "seen all"
    );
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 3);
    let first = &log[1]["messages"][2];
    let receipt = &first["content"][0];
    assert_ne!(
        receipt["is_error"], true,
        "the completed effect is not an error"
    );
    assert_eq!(receipt["content"][0]["text"], "captured");
    // The standard tier's native size, which is within the many-image limit.
    assert_eq!(image_dimensions(&receipt["content"][1]), Some((1568, 78)));
    assert_eq!(
        receipt["content"][2]["type"], "text",
        "unprocessable image is omitted"
    );
    assert_eq!(
        log[2]["messages"][2], *first,
        "earlier tool images stay byte-identical once history exceeds twenty images"
    );
    server.abort();
}

/// Tool-result images, such as screenshots, reach Claude at most at the
/// model's native resolution and within the per-image byte limit. An image
/// that cannot be prepared, because it cannot shrink to fit, its header claims
/// more pixels than can be decoded, or it is a remote URL, becomes an omission
/// inside the same successful tool result.
#[tokio::test]
async fn tool_images_are_prepared_for_the_models_native_resolution() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    // Accept request bodies up to the Messages API's 32 MB limit.
    let app = Router::new()
        .route(
            "/v1/messages",
            post(move |Json(body): Json<Value>| {
                let log = log.clone();
                async move {
                    let prompted = body["messages"].as_array().unwrap().len() == 1;
                    log.lock().unwrap().push(body);
                    let (blocks, stop) = if prompted {
                        (
                            vec![json!({"type":"tool_use","id":"capture","name":"Capture","input":{}})],
                            "tool_use",
                        )
                    } else {
                        (vec![json!({"type":"text","text":"Seen."})], "end_turn")
                    };
                    ([("content-type", "text/event-stream")], stream(blocks, stop))
                }
            }),
        )
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let image = |media_type: &str, data: String| json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":data}});
    let captured = vec![
        image("image/png", png(3840, 2160)),
        image("image/png", STANDARD.encode(noise_png(2000, 1000))),
        image("image/jpeg", STANDARD.encode(bloated_jpeg())),
        image(
            "image/x-portable-pixmap",
            STANDARD.encode(b"P6\n4294967295 1\n255\n"),
        ),
        json!({"type":"image","source":{"type":"url","url":"https://example.com/screenshot.png"}}),
    ];

    // Sizes from Anthropic's resolution examples for each tier; the noise
    // image fits the high-resolution tier but not the byte limit.
    for (model, screenshot, noise) in [
        ("claude-opus-5-5", (2576, 1449), (1500, 750)),
        ("claude-haiku-4-5", (1456, 819), (1568, 784)),
    ] {
        let client = ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        );
        let blocks = captured.clone();
        let (agent, _) = Nanocodex::builder(Claude::new(client, model))
            .max_tokens(4096)
            .tool_blocks(
                ToolDefinition {
                    name: "Capture".into(),
                    description: "Capture a synthetic screenshot".into(),
                    input_schema: json!({"type":"object"}),
                    strict: None,
                    defer_loading: false,
                },
                move |_| {
                    let blocks = blocks.clone();
                    async move { Ok(blocks) }
                },
            )
            .build()
            .unwrap();
        let result = agent
            .prompt("capture the screen")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(result.final_message(), "Seen.");
        agent.shutdown().await.unwrap();

        let request = requests.lock().unwrap().last().unwrap().clone();
        let receipt = &request["messages"][2]["content"][0];
        assert_ne!(receipt["is_error"], true, "{model}");
        let content = &receipt["content"];
        assert_eq!(image_dimensions(&content[0]), Some(screenshot), "{model}");
        let sent = STANDARD
            .decode(content[1]["source"]["data"].as_str().unwrap())
            .unwrap();
        assert!(
            sent.len() <= 5 * 1024 * 1024,
            "{model}: {} bytes",
            sent.len()
        );
        assert_eq!(image_dimensions(&content[1]), Some(noise), "{model}");
        assert_eq!(content[2]["type"], "text", "{model}");
        assert!(
            content[2]["text"].as_str().unwrap().contains("size limit"),
            "{model}: {}",
            content[2]
        );
        assert_eq!(content[3]["type"], "text", "{model}");
        assert!(
            content[4]["text"]
                .as_str()
                .unwrap()
                .contains("remote image URLs"),
            "{model}: {}",
            content[4]
        );
    }
    server.abort();
}

/// Tool lifecycle events through the end of one run, plus any immediate stragglers.
/// Each entry is `(is_result, payload)`; unrelated events are ignored.
async fn tool_lifecycle_until_run_end(
    events: &mut nanocodex_agent::AgentEvents,
) -> Vec<(bool, Value)> {
    use std::time::Duration;
    let mut seen = Vec::new();
    let mut observe = |event: nanocodex_agent::events::AgentEvent| {
        let is_result = match event.kind {
            AgentEventKind::ToolCall => false,
            AgentEventKind::ToolResult => true,
            _ => return false,
        };
        seen.push((
            is_result,
            serde_json::from_str::<Value>(event.payload.get()).unwrap(),
        ));
        false
    };
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("run end event")
            .expect("event stream stays open");
        let ended = matches!(
            event.kind,
            AgentEventKind::RunFailed | AgentEventKind::RunCompleted
        );
        observe(event);
        if ended {
            break;
        }
    }
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(100), events.recv()).await
    {
        observe(event);
    }
    seen
}

async fn cancelled_tool_batch_retains_completed_and_unknown_results(parallel: bool) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = requests.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let received = received.clone();
            async move {
                let index = {
                    let mut log = received.lock().unwrap();
                    log.push(body);
                    log.len()
                };
                let (blocks, reason) = if index == 1 {
                    (
                        vec![
                            json!({"type":"tool_use","id":"toolu_a","name":"effect","input":{"key":"a"}}),
                            json!({"type":"tool_use","id":"toolu_b","name":"effect","input":{"key":"b"}}),
                        ],
                        "tool_use",
                    )
                } else {
                    (vec![json!({"type":"text","text":"recovered"})], "end_turn")
                };
                ([ ("content-type", "text/event-stream") ], stream(blocks, reason))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let a_done = Arc::new(tokio::sync::Notify::new());
    let b_started = Arc::new(tokio::sync::Notify::new());
    let effect_count = Arc::new(AtomicUsize::new(0));
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, mut events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .parallel_tools(parallel)
        .tool(
            ToolDefinition {
                name: "effect".into(),
                description: "synthetic effect".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            {
                let a_done = a_done.clone();
                let b_started = b_started.clone();
                let effect_count = effect_count.clone();
                move |input| {
                    let a_done = a_done.clone();
                    let b_started = b_started.clone();
                    let effect_count = effect_count.clone();
                    async move {
                        if input["key"] == "a" {
                            effect_count.fetch_add(1, Ordering::SeqCst);
                            a_done.notify_one();
                            Ok("effect a committed".to_string())
                        } else {
                            a_done.notified().await;
                            b_started.notify_one();
                            std::future::pending().await
                        }
                    }
                }
            },
        )
        .build()
        .unwrap();
    let started = b_started.notified();
    let turn = agent.prompt("perform two effects").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), started)
        .await
        .unwrap();
    turn.cancel().await.unwrap();
    assert!(turn.result().await.is_err());
    assert_eq!(effect_count.load(Ordering::SeqCst), 1);
    // Both handlers began, so each call must close exactly once: the committed
    // effect normally, the pending one as cancelled with an unknown outcome.
    let lifecycle = tool_lifecycle_until_run_end(&mut events).await;
    for id in ["toolu_a", "toolu_b"] {
        let of = |result: bool| {
            lifecycle
                .iter()
                .filter(|(is_result, payload)| *is_result == result && payload["call_id"] == id)
                .map(|(_, payload)| payload)
                .collect::<Vec<_>>()
        };
        assert_eq!(of(false).len(), 1, "{id} must publish one tool.call");
        let results = of(true);
        assert_eq!(
            results.len(),
            1,
            "{id} must publish exactly one tool.result"
        );
        assert_eq!(results[0]["turn_id"], of(false)[0]["turn_id"]);
        if id == "toolu_a" {
            assert_eq!(results[0]["status"], "completed");
            assert!(results[0].get("outcome_unknown").is_none());
        } else {
            assert_eq!(results[0]["status"], "cancelled");
            assert_eq!(results[0]["outcome_unknown"], true);
            assert!(results[0]["duration_ns"].is_u64());
            assert!(
                results[0]["result"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("outcome unknown")
            );
        }
    }
    assert_eq!(lifecycle.len(), 4, "no duplicate or extra lifecycle events");
    assert_eq!(
        agent
            .prompt("continue without repeating the interrupted operation")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "recovered"
    );
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 2);
    let blocks = log[1]["messages"][2]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0]["tool_use_id"], "toolu_a");
    assert_eq!(blocks[0]["content"], "effect a committed");
    assert_eq!(blocks[1]["tool_use_id"], "toolu_b");
    assert_eq!(blocks[1]["is_error"], true);
    assert!(
        blocks[1]["content"]
            .as_str()
            .unwrap()
            .contains("outcome unknown")
    );
    assert_eq!(effect_count.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn sequential_cancel_keeps_completed_tool_receipt() {
    cancelled_tool_batch_retains_completed_and_unknown_results(false).await;
}

#[tokio::test]
async fn parallel_cancel_keeps_completed_tool_receipt() {
    cancelled_tool_batch_retains_completed_and_unknown_results(true).await;
}

#[tokio::test]
async fn configured_auto_window_reserves_model_output_not_request_max_tokens() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for (input_tokens, expected_requests) in [(66_990, 2usize), (66_992, 3)] {
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let app = Router::new().route(
            "/v1/messages",
            post(move |Json(_): Json<Value>| {
                let calls = calls.clone();
                async move {
                    let index = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    let answer = if index == 2 {
                        "summary or answer"
                    } else {
                        "ok"
                    };
                    let mut body = stream(vec![json!({"type":"text","text":answer})], "end_turn");
                    if index == 1 {
                        body = body.replace(
                            "\"input_tokens\":3",
                            &format!("\"input_tokens\":{input_tokens}"),
                        );
                    }
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{addr}/v1/messages"),
            "synthetic",
        );
        let (agent, _) = Nanocodex::builder(Claude::latest(client))
            .max_tokens(4_096)
            .auto_compact_window_tokens(100_000)
            .build()
            .unwrap();
        agent.prompt("first").await.unwrap().result().await.unwrap();
        agent.prompt("next").await.unwrap().result().await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), expected_requests);
        server.abort();
    }
}

async fn tool_result_compaction(receipt: String, should_compact: bool) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = requests.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let received = received.clone();
            async move {
                let index = {
                    let mut log = received.lock().unwrap();
                    log.push(body);
                    log.len()
                };
                let (blocks, reason) = match index {
                    1 => (vec![json!({"type":"tool_use","id":"toolu_effect","name":"effect","input":{}})], "tool_use"),
                    2 if should_compact => (vec![json!({"type":"text","text":"Effect completed once; continue task."})], "end_turn"),
                    _ => (vec![json!({"type":"text","text":"done"})], "end_turn"),
                };
                let mut body = stream(blocks, reason);
                if index == 1 {
                    body = body.replace("\"input_tokens\":3", "\"input_tokens\":66500");
                }
                ([ ("content-type", "text/event-stream") ], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let expected_receipt = receipt.clone();
    let (agent, _) = Nanocodex::builder(Claude::latest(client))
        .auto_compact_window_tokens(100_000)
        .tool(
            ToolDefinition {
                name: "effect".into(),
                description: "Synthetic side effect".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                let receipt = receipt.clone();
                async move { Ok(receipt) }
            },
        )
        .build()
        .unwrap();
    let result = agent
        .prompt("perform once")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "done");
    let model_calls = if should_compact { 3 } else { 2 };
    assert_eq!(
        result.usage().unwrap().input_tokens(),
        66_500 + 3 * (model_calls - 1)
    );
    assert_eq!(result.usage().unwrap().output_tokens(), 5 * model_calls);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), model_calls as usize);
    let continuation = if should_compact { 2 } else { 1 };
    assert_eq!(
        log[continuation]["messages"][2]["content"][0]["content"],
        expected_receipt
    );
    if should_compact {
        assert_eq!(log[1]["messages"].as_array().unwrap().len(), 2);
        assert!(!log[1]["messages"].to_string().contains("toolu_effect"));
        assert_eq!(log[2]["messages"].as_array().unwrap().len(), 3);
        assert_eq!(log[2]["messages"][1]["content"][0]["id"], "toolu_effect");
        assert!(
            log[2]["messages"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Effect completed once")
        );
    }
    server.abort();
}

// Usage alone is below the threshold; the newly returned receipt crosses it.
#[tokio::test]
async fn large_tool_result_compacts_within_turn_without_rerunning_effect() {
    tool_result_compaction("x".repeat(2_200), true).await;
}

// UTF-8 byte length would incorrectly trigger compaction for this receipt.
#[tokio::test]
async fn unicode_tool_result_uses_same_text_estimate_as_queued_prompt() {
    tool_result_compaction("😀".repeat(600), false).await;
}

#[tokio::test]
async fn queued_user_text_counts_toward_next_compaction_decision() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = received.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let requests = requests.clone();
            async move {
                let index = {
                    let mut log = requests.lock().unwrap();
                    log.push(body);
                    log.len()
                };
                let mut output = stream(
                    vec![json!({"type":"text","text":if index==2 {"summary"} else {"answer"}})],
                    "end_turn",
                );
                if index == 1 {
                    output = output.replace("\"input_tokens\":3", "\"input_tokens\":66500");
                }
                ([("content-type", "text/event-stream")], output)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::latest(client))
        .auto_compact_window_tokens(100_000)
        .build()
        .unwrap();
    agent.prompt("one").await.unwrap().result().await.unwrap();
    let second = "synthetic text ".repeat(150); // 2250 UTF-16 units, crosses 67k
    assert_eq!(
        agent
            .prompt(second.clone())
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "answer"
    );
    let log = received.lock().unwrap();
    assert_eq!(log.len(), 3, "summary precedes the next main request");
    assert!(log[1]["messages"].as_array().unwrap().len() >= 2);
    assert_eq!(log[2]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        log[2]["messages"][1],
        json!({"role":"user","content":[{"type":"text","text":second}]})
    );
    server.abort();
}

// A completed handler can observe cancellation before returning its receipt.
// Preserve that receipt, but do not start the following side effect.
async fn cancellation_at_completed_handler_boundary(parallel: bool) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (control_tx, control) = tokio::sync::watch::channel(None::<nanocodex_agent::TurnControl>);
    let request_control = control.clone();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let received = received.clone();
        let mut request_control = request_control.clone();
        async move {
            // Publish the control before any tool handler can be polled.
            request_control.wait_for(Option::is_some).await.unwrap();
            let index = { let mut log = received.lock().unwrap(); log.push(body); log.len() };
            let (blocks, reason) = if index == 1 {
                (vec![
                    json!({"type":"tool_use","id":"a","name":"effect","input":{"first":true}}),
                    json!({"type":"tool_use","id":"b","name":"effect","input":{"first":false}}),
                ], "tool_use")
            } else { (vec![json!({"type":"text","text":"recovered"})], "end_turn") };
            ([("content-type", "text/event-stream")], stream(blocks, reason))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let effects = Arc::new(AtomicUsize::new(0));
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, mut events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .parallel_tools(parallel)
        .tool(
            ToolDefinition {
                name: "effect".into(),
                description: "Synthetic effect".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            {
                let control = control.clone();
                let effects = effects.clone();
                move |input| {
                    let mut control = control.clone();
                    let effects = effects.clone();
                    async move {
                        effects.fetch_add(1, Ordering::SeqCst);
                        if input["first"] == true {
                            let turn = control
                                .wait_for(Option::is_some)
                                .await
                                .unwrap()
                                .clone()
                                .unwrap();
                            turn.cancel().await.unwrap();
                        }
                        Ok("committed".into())
                    }
                }
            },
        )
        .build()
        .unwrap();
    let turn = agent.prompt("perform effects").await.unwrap();
    control_tx.send(Some(turn.control())).unwrap();
    assert!(turn.result().await.is_err());
    assert_eq!(
        effects.load(Ordering::SeqCst),
        1,
        "second side effect must not start after cancellation"
    );
    // The unstarted call has no tool.call, so it must not get a lone tool.result.
    let lifecycle = tool_lifecycle_until_run_end(&mut events).await;
    assert_eq!(
        lifecycle
            .iter()
            .map(|(is_result, payload)| (*is_result, payload["call_id"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        [(false, "a"), (true, "a")]
    );
    assert_eq!(lifecycle[1].1["status"], "completed");
    agent
        .prompt("recover")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = requests.lock().unwrap();
    assert_eq!(
        log.len(),
        2,
        "cancelled turn must not send another provider request"
    );
    let results = log[1]["messages"][2]["content"].as_array().unwrap();
    assert_eq!(results[0]["content"], "committed");
    assert_eq!(results[1]["tool_use_id"], "b");
    assert_eq!(results[1]["is_error"], true);
    server.abort();
}

#[tokio::test]
async fn cancellation_at_completed_handler_boundary_stops_next_effect() {
    cancellation_at_completed_handler_boundary(false).await;
}

#[tokio::test]
async fn parallel_cancellation_at_completed_handler_boundary_stops_unstarted_effect() {
    cancellation_at_completed_handler_boundary(true).await;
}

#[tokio::test]
async fn queued_ephemeral_cancellation_retires_without_aborting_active_model_or_tool() {
    use nanocodex_agent::{NanocodexError, PromptRequest};
    use std::time::Duration;
    use tokio::sync::Notify;
    let _ = rustls::crypto::ring::default_provider().install_default();
    for held_tool in [false, true] {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route("/v1/messages", post({
            let started = started.clone();
            let release = release.clone();
            let requests = requests.clone();
            move |Json(_body): Json<Value>| {
                let started = started.clone();
                let release = release.clone();
                let requests = requests.clone();
                async move {
                    let index = requests.fetch_add(1, Ordering::SeqCst);
                    if index == 0 && !held_tool {
                        started.notify_one();
                        release.notified().await;
                    }
                    let (blocks, stop) = if index == 0 && held_tool {
                        (vec![json!({"type":"tool_use","id":"held-effect","name":"effect","input":{}})], "tool_use")
                    } else {
                        (vec![json!({"type":"text","text":"ACTIVE_FINISHED"})], "end_turn")
                    };
                    ([("content-type", "text/event-stream")], stream(blocks, stop))
                }
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
        let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
            .max_tokens(128_000)
            .tool(
                ToolDefinition {
                    name: "effect".into(),
                    description: "Held synthetic effect".into(),
                    input_schema: json!({"type":"object"}),
                    strict: None,
                    defer_loading: false,
                },
                {
                    let started = started.clone();
                    let release = release.clone();
                    move |_input| {
                        let started = started.clone();
                        let release = release.clone();
                        async move {
                            started.notify_one();
                            release.notified().await;
                            Ok("effect completed".into())
                        }
                    }
                },
            )
            .build()
            .unwrap();
        let active = agent.prompt("hold active A").await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        let queued = agent.prompt("cancel queued B only").await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), queued.cancel())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), queued.result())
                .await
                .unwrap(),
            Err(NanocodexError::TurnCancelled)
        ));
        let cancelled_on_admission = agent
            .prompt(PromptRequest::new("never dispatch C").cancel_on_admission())
            .await
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), cancelled_on_admission.result())
                .await
                .unwrap(),
            Err(NanocodexError::TurnCancelled)
        ));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        release.notify_one();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), active.result())
                .await
                .unwrap()
                .unwrap()
                .final_message(),
            "ACTIVE_FINISHED"
        );
        assert_eq!(
            requests.load(Ordering::SeqCst),
            if held_tool { 2 } else { 1 }
        );
        agent.shutdown().await.unwrap();
        server.abort();
    }
}

#[tokio::test]
async fn fast_mode_applies_per_accepted_turn_on_supported_models() {
    use tokio::sync::Semaphore;
    let _ = rustls::crypto::ring::default_provider().install_default();
    // Records each request's speed field and whether the fast-mode beta was sent.
    let received = Arc::new(Mutex::new(Vec::<(Option<String>, bool)>::new()));
    let first_response = Arc::new(Semaphore::new(0));
    let (requests, gate) = (received.clone(), first_response.clone());
    let app = Router::new().route(
        "/v1/messages",
        post(
            move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                let (requests, gate) = (requests.clone(), gate.clone());
                async move {
                    let beta = headers
                        .get("anthropic-beta")
                        .and_then(|value| value.to_str().ok())
                        .is_some_and(|value| {
                            value.split(',').any(|beta| beta == "fast-mode-2026-02-01")
                        });
                    let speed = body["speed"].as_str().map(str::to_owned);
                    let index = {
                        let mut requests = requests.lock().unwrap();
                        requests.push((speed, beta));
                        requests.len()
                    };
                    if index == 1 {
                        drop(gate.acquire().await.unwrap());
                    }
                    let text = json!({"type":"text","text":"ok"});
                    (
                        [("content-type", "text/event-stream")],
                        stream(vec![text], "end_turn"),
                    )
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let agent = |model: &str| {
        let client = ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        );
        Nanocodex::builder(Claude::new(client, model))
            .fast_mode(true)
            .build()
            .unwrap()
            .0
    };

    let opus = agent("claude-opus-5-5");
    let active = opus.prompt("first").await.unwrap();
    while received.lock().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    // Accepted behind the active turn, so it keeps fast mode after the toggle.
    let queued = opus.prompt("queued").await.unwrap();
    opus.set_fast_mode(false).await.unwrap();
    first_response.add_permits(1);
    active.result().await.unwrap();
    queued.result().await.unwrap();
    opus.prompt("standard")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();

    let sonnet = agent("claude-sonnet-4-6");
    sonnet
        .prompt("unsupported")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();

    let fast = (Some("fast".to_owned()), true);
    let standard = (None, false);
    assert_eq!(
        *received.lock().unwrap(),
        [fast.clone(), fast, standard.clone(), standard]
    );
    server.abort();
}

#[tokio::test]
async fn response_usage_arrives_before_tool_completion_and_excludes_summary() {
    use nanocodex_agent::events::{AgentEventData, ModelEvent};
    use std::time::Duration;
    use tokio::sync::Notify;

    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(_): Json<Value>| {
            let count = count.clone();
            async move {
                let index = count.fetch_add(1, Ordering::SeqCst);
                let (blocks, stop) = if index == 0 {
                    (
                        vec![json!({"type":"tool_use","id":"held","name":"hold","input":{}})],
                        "tool_use",
                    )
                } else {
                    (vec![json!({"type":"text","text":"done"})], "end_turn")
                };
                let body = stream(blocks, stop).replace(
                    "\"input_tokens\":3",
                    &format!("\"input_tokens\":{}", 3 + index * 10),
                );
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let release = Arc::new(Notify::new());
    let finished = Arc::new(AtomicUsize::new(0));
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, mut events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tool(
            ToolDefinition {
                name: "hold".into(),
                description: "Held tool".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            {
                let release = release.clone();
                let finished = finished.clone();
                move |_| {
                    let release = release.clone();
                    let finished = finished.clone();
                    async move {
                        release.notified().await;
                        finished.fetch_add(1, Ordering::SeqCst);
                        Ok("released".into())
                    }
                }
            },
        )
        .build()
        .unwrap();
    let turn = agent.prompt("hold then answer").await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = events.next().await.unwrap();
            assert_ne!(event.kind, AgentEventKind::RunCompleted);
            if event.kind == AgentEventKind::ModelCallCompleted
                && let AgentEventData::Model(ModelEvent::CallCompleted(call)) =
                    event.data().unwrap()
            {
                break call;
            }
        }
    })
    .await
    .expect("completed response must publish usage while the tool is blocked");
    assert_eq!(finished.load(Ordering::SeqCst), 0);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(first.call_index, 1);
    assert_eq!(first.tool_calls, 1);
    let usage = first.usage.unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (6, 5, 11)
    );
    let details = usage.input_tokens_details.unwrap();
    assert_eq!((details.cached_tokens, details.cache_write_tokens), (2, 1));
    release.notify_one();
    let result = turn.result().await.unwrap();
    assert_eq!(result.usage().unwrap().total_tokens(), 32);
    agent.compact().await.unwrap();
    agent
        .prompt("after summary")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let mut completions = Vec::new();
    let mut terminals = 0;
    while terminals < 2 {
        let event = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .unwrap()
            .unwrap();
        if event.kind == AgentEventKind::RunCompleted {
            terminals += 1;
        }
        if event.kind == AgentEventKind::ModelCallCompleted
            && let AgentEventData::Model(ModelEvent::CallCompleted(call)) = event.data().unwrap()
        {
            completions.push(call);
        }
    }
    assert_eq!(
        completions.len(),
        2,
        "summary usage is not an active response"
    );
    assert_eq!(completions[0].call_index, 2);
    let continued_usage = completions[0].usage.as_ref().unwrap();
    assert_eq!(
        (continued_usage.input_tokens, continued_usage.total_tokens),
        (16, 21)
    );
    let continued_cache = continued_usage.input_tokens_details.as_ref().unwrap();
    assert_eq!(
        (
            continued_cache.cached_tokens,
            continued_cache.cache_write_tokens
        ),
        (2, 1)
    );
    assert_eq!(completions[1].usage.as_ref().unwrap().total_tokens, 41);
    assert_eq!(requests.load(Ordering::SeqCst), 4);
    agent.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn steering_acknowledges_consumption_at_tool_and_terminal_boundaries() {
    use nanocodex_agent::events::{AgentEventData, RunEvent};
    use std::time::Duration;
    use tokio::sync::Notify;

    let _ = rustls::crypto::ring::default_provider().install_default();
    for held_tool in [true, false] {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
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
                        if index == 1 && !held_tool {
                            started.notify_one();
                            release.notified().await;
                        }
                        let (blocks, stop) = if index == 1 && held_tool {
                            (
                                vec![
                                    json!({"type":"tool_use","id":"held","name":"hold","input":{}}),
                                ],
                                "tool_use",
                            )
                        } else {
                            (vec![json!({"type":"text","text":"done"})], "end_turn")
                        };
                        (
                            [("content-type", "text/event-stream")],
                            stream(blocks, stop),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        );
        let (agent, mut events) = Nanocodex::builder(Claude::new(client, "test"))
            .max_tokens(128_000)
            .tool(
                ToolDefinition {
                    name: "hold".into(),
                    description: "Held tool".into(),
                    input_schema: json!({"type":"object"}),
                    strict: None,
                    defer_loading: false,
                },
                {
                    let started = started.clone();
                    let release = release.clone();
                    move |_| {
                        let started = started.clone();
                        let release = release.clone();
                        async move {
                            started.notify_one();
                            release.notified().await;
                            Ok("released".into())
                        }
                    }
                },
            )
            .build()
            .unwrap();
        let turn = agent.prompt("begin").await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        turn.steer("first steer").await.unwrap();
        // A retried identity is admitted and reported once.
        for _ in 0..2 {
            turn.steer_with_id("second".into(), "second é")
                .await
                .unwrap();
        }
        let mut accepted = Vec::new();
        while let Some(event) = events.try_recv_timed() {
            assert_ne!(
                event.event.kind,
                AgentEventKind::RunSteered,
                "admission must not acknowledge consumption"
            );
            let payload: Value = serde_json::from_str(event.event.payload.get()).unwrap();
            if event.event.kind == AgentEventKind::InputAccepted && payload["kind"] == "steer" {
                accepted.push(payload);
            }
        }
        // Admission reports each steer's accepted input.
        assert_eq!(
            accepted
                .iter()
                .map(|payload| payload["input"].clone())
                .collect::<Vec<_>>(),
            [json!("first steer"), json!("second é")]
        );
        assert_eq!(accepted[0]["request_id"], Value::Null);
        assert_eq!(accepted[1]["request_id"], "second");
        for payload in &accepted {
            let turn_id = payload["turn_id"].as_str().unwrap();
            assert!(
                payload["item_id"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("{turn_id}:steer:")),
                "{payload}"
            );
        }
        assert!(
            accepted[1]["item_id"]
                .as_str()
                .unwrap()
                .ends_with(":steer:second")
        );
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), turn.result())
            .await
            .unwrap()
            .unwrap();
        let mut acknowledged = Vec::new();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(2), events.next())
                .await
                .unwrap()
                .unwrap();
            if event.kind == AgentEventKind::RunSteered
                && let AgentEventData::Run(RunEvent::Steered(steer)) = event.data().unwrap()
            {
                acknowledged.push((steer.steer_index, steer.instruction_bytes));
            }
            if event.kind == AgentEventKind::ModelCallCompleted {
                let payload: Value = serde_json::from_str(event.payload.get()).unwrap();
                if payload["call_index"] == 2 {
                    assert_eq!(
                        acknowledged.len(),
                        2,
                        "both steers must be acknowledged before their response completes"
                    );
                }
            }
            if event.kind == AgentEventKind::RunCompleted {
                break;
            }
        }
        assert_eq!(
            acknowledged,
            vec![(1, "first steer".len()), (2, "second é".len())]
        );
        {
            let log = requests.lock().unwrap();
            assert_eq!(log.len(), 2);
            let messages = log[1]["messages"].as_array().unwrap();
            assert_eq!(
                messages[messages.len() - 2]["content"][0]["text"],
                "first steer"
            );
            assert_eq!(
                messages[messages.len() - 1]["content"][0]["text"],
                "second é"
            );
        }
        agent.shutdown().await.unwrap();
        server.abort();
    }
}

/// Regression: a streamed Claude answer must not render twice. Clients (iOS
/// InboxCore and nanocodex-react) fold the final `assistant.message` into the
/// streamed `assistant.delta` row only when both carry the same response
/// identity (`model_call_index`, `item_id`, `phase`). The delta used to publish
/// `item_id: null` beside a concrete final message ID, so every answer was
/// appended as a second, duplicate row.
#[tokio::test]
async fn streamed_text_and_final_message_share_one_response_identity() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let calls = Arc::new(AtomicUsize::new(0));
    let received = calls.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(_body): Json<Value>| {
            let received = received.clone();
            async move {
                let call = received.fetch_add(1, Ordering::SeqCst) + 1;
                let (blocks, stop) = if call == 1 {
                    (
                        vec![
                            json!({"type":"text","text":"Checking "}),
                            json!({"type":"text","text":"the lookup."}),
                            json!({"type":"tool_use","id":"tool-1","name":"lookup","input":{"key":"x"}}),
                        ],
                        "tool_use",
                    )
                } else {
                    (vec![json!({"type":"text","text":"Found the value."})], "end_turn")
                };
                let response =
                    stream(blocks, stop).replace("\"id\":\"msg\"", &format!("\"id\":\"msg_{call}\""));
                ([("content-type", "text/event-stream")], response)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, mut events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tool(
            ToolDefinition {
                name: "lookup".into(),
                description: "Test lookup".into(),
                input_schema: json!({"type":"object","properties":{"key":{"type":"string"}}}),
                strict: None,
                defer_loading: false,
            },
            |_input| async move { Ok("value".to_owned()) },
        )
        .build()
        .unwrap();
    let result = agent
        .prompt("look up x")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "Found the value.");

    // Project the event stream exactly like a transcript client: deltas with
    // the same identity append to one streaming row; a final message replaces
    // the row with a matching identity, or appends a new row otherwise.
    type Identity = (Value, Value, Value);
    let identity = |payload: &Value| -> Identity {
        (
            payload["model_call_index"].clone(),
            payload["item_id"].clone(),
            payload["phase"].clone(),
        )
    };
    let mut rows: Vec<(Identity, String, bool)> = Vec::new();
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.next())
            .await
            .unwrap()
            .unwrap();
        let payload: Value = serde_json::from_str(event.payload.get()).unwrap();
        match event.kind {
            AgentEventKind::AssistantDelta => {
                assert!(
                    payload["item_id"].is_string(),
                    "streamed Claude text must identify its provider message: {payload}"
                );
                let key = identity(&payload);
                match rows.last_mut() {
                    Some((last, text, false)) if *last == key => {
                        text.push_str(payload["text"].as_str().unwrap());
                    }
                    _ => rows.push((key, payload["text"].as_str().unwrap().to_owned(), false)),
                }
            }
            AgentEventKind::AssistantMessage => {
                let key = identity(&payload);
                let text = payload["text"].as_str().unwrap().to_owned();
                match rows
                    .iter_mut()
                    .rev()
                    .find(|(row, _, done)| *row == key && !done)
                {
                    Some(row) => {
                        assert_eq!(row.1, text, "final text must equal its streamed text");
                        row.2 = true;
                    }
                    None => rows.push((key, text, true)),
                }
            }
            AgentEventKind::RunCompleted | AgentEventKind::RunFailed => break,
            _ => {}
        }
    }
    agent.shutdown().await.unwrap();
    server.abort();
    let texts = rows
        .iter()
        .map(|(_, text, _)| text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        texts,
        ["Checking the lookup.", "Found the value."],
        "each Claude response must render exactly once"
    );
    assert!(
        rows.iter().all(|(_, _, done)| *done),
        "every streamed row is finalized"
    );
    assert_ne!(
        rows[0].0, rows[1].0,
        "separate model calls keep separate identities"
    );
}

/// The run publishes streamed text while the caller admits steers on another
/// thread, and every event of both reaches the session stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn steers_admitted_during_streaming_keep_every_event() {
    use std::{sync::atomic::AtomicBool, time::Duration};

    const BATCHES: usize = 300;
    const BATCH: usize = 100;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let streaming = Arc::new(tokio::sync::Notify::new());
    let streamed = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/messages",
        post({
            let streaming = streaming.clone();
            let streamed = streamed.clone();
            let requests = requests.clone();
            move |Json(_): Json<Value>| {
                let streaming = streaming.clone();
                let streamed = streamed.clone();
                let requests = requests.clone();
                async move {
                    if requests.fetch_add(1, Ordering::SeqCst) > 0 {
                        return (
                            [("content-type", "text/event-stream")],
                            stream(vec![json!({"type":"text","text":"done"})], "end_turn"),
                        )
                            .into_response();
                    }
                    let sse = |events: &[Value]| -> String {
                        events
                            .iter()
                            .map(|event| format!("data: {event}\n\n"))
                            .collect()
                    };
                    let head = sse(&[
                        json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"test","content":[],"usage":{"input_tokens":3,"output_tokens":0}}}),
                        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                    ]);
                    let batch = sse(&[json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}})])
                        .repeat(BATCH);
                    let tail = sse(&[
                        json!({"type":"content_block_stop","index":0}),
                        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}),
                        json!({"type":"message_stop"}),
                    ]);
                    streaming.notify_one();
                    let batches = futures_util::stream::iter(0..BATCHES).then(move |_| {
                        let batch = batch.clone();
                        async move {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                            Ok::<_, std::io::Error>(batch)
                        }
                    });
                    let body = futures_util::stream::once(async move { Ok(head) })
                        .chain(batches)
                        .chain(futures_util::stream::once(async move {
                            streamed.store(true, Ordering::SeqCst);
                            Ok(tail)
                        }));
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from_stream(body))
                        .unwrap()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic");
    let (agent, mut events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .build()
        .unwrap();
    let turn = agent.prompt("begin").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), streaming.notified())
        .await
        .unwrap();
    // Withdrawing each steer keeps the steering queue from filling.
    let mut steers = 0;
    while !streamed.load(Ordering::SeqCst) {
        let id = format!("steer-{steers}");
        if turn.steer_with_id(id.clone(), "steer").await.is_err() {
            break;
        }
        steers += 1;
        let _ = turn.withdraw_steer(id).await;
    }
    tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .unwrap()
        .unwrap();
    let (mut deltas, mut accepted) = (0, 0);
    loop {
        let event = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .expect("every published event reaches the session stream")
            .unwrap();
        let payload: Value = serde_json::from_str(event.payload.get()).unwrap();
        match event.kind {
            AgentEventKind::AssistantDelta => deltas += 1,
            AgentEventKind::InputAccepted if payload["kind"] == "steer" => accepted += 1,
            AgentEventKind::RunCompleted => break,
            _ => {}
        }
    }
    assert!(steers > 0);
    assert_eq!(accepted, steers);
    assert!(deltas >= BATCHES * BATCH);
    agent.shutdown().await.unwrap();
    server.abort();
}

/// A turn retires even when its run panics, so the agent can shut down.
#[tokio::test]
async fn a_panicking_tool_does_not_block_shutdown() {
    use std::time::Duration;

    let _ = rustls::crypto::ring::default_provider().install_default();
    let app = Router::new().route(
        "/v1/messages",
        post(|Json(_): Json<Value>| async {
            (
                [("content-type", "text/event-stream")],
                stream(
                    vec![json!({"type":"tool_use","id":"panic","name":"panic","input":{}})],
                    "tool_use",
                ),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic");
    let (agent, _events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tool(
            ToolDefinition {
                name: "panic".into(),
                description: "Panicking tool".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            |_| -> std::future::Ready<Result<String, String>> { panic!("synthetic tool panic") },
        )
        .build()
        .unwrap();
    let turn = agent.prompt("begin").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), turn.result())
            .await
            .unwrap()
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(2), agent.shutdown())
        .await
        .expect("a panicked turn must not block shutdown")
        .unwrap();
    server.abort();
}

/// A realtime voice frontend delegates through live routing: idle input
/// starts a Claude turn, and input while that turn is working steers it
/// instead of admitting a concurrent operation.
#[tokio::test]
async fn live_route_starts_idle_turn_and_steers_active_turn() {
    use nanocodex_agent::PromptRoute;
    use std::time::Duration;
    use tokio::sync::Notify;

    let _ = rustls::crypto::ring::default_provider().install_default();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let app = Router::new().route(
        "/v1/messages",
        post({
            let requests = requests.clone();
            move |Json(body): Json<Value>| {
                let requests = requests.clone();
                async move {
                    let index = {
                        let mut log = requests.lock().unwrap();
                        log.push(body);
                        log.len()
                    };
                    let (blocks, stop) = match index {
                        1 => (
                            vec![json!({"type":"tool_use","id":"held","name":"hold","input":{}})],
                            "tool_use",
                        ),
                        2 => (
                            vec![json!({"type":"text","text":"Booked and noted the window seat."})],
                            "end_turn",
                        ),
                        _ => (
                            vec![json!({"type":"text","text":"Second turn."})],
                            "end_turn",
                        ),
                    };
                    (
                        [("content-type", "text/event-stream")],
                        stream(blocks, stop),
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _events) = Nanocodex::builder(Claude::new(client, "claude-opus-5-5"))
        .tool(
            ToolDefinition {
                name: "hold".into(),
                description: "Held tool".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            {
                let started = started.clone();
                let release = release.clone();
                move |_| {
                    let started = started.clone();
                    let release = release.clone();
                    async move {
                        started.notify_one();
                        release.notified().await;
                        Ok("booked".into())
                    }
                }
            },
        )
        .build()
        .unwrap();

    let PromptRoute::Started(turn) = agent.route_prompt("book the flight").await.unwrap() else {
        panic!("idle live input must start a turn");
    };
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    assert!(
        matches!(
            agent
                .route_prompt("and ask for a window seat")
                .await
                .unwrap(),
            PromptRoute::Steered
        ),
        "live input during an active turn must steer it"
    );
    release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(2), turn.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.final_message(), "Booked and noted the window seat.");
    {
        let log = requests.lock().unwrap();
        assert_eq!(
            log.len(),
            2,
            "steering must not admit a concurrent model call"
        );
        let continuation = log[1]["messages"].to_string();
        assert!(
            continuation.contains("and ask for a window seat"),
            "steered voice input reaches the active turn: {continuation}"
        );
    }

    let PromptRoute::Started(next) = agent.route_prompt("anything else?").await.unwrap() else {
        panic!("input after the active turn finished must start a new turn");
    };
    assert_eq!(next.result().await.unwrap().final_message(), "Second turn.");
    agent.shutdown().await.unwrap();
    server.abort();
}

/// Managed attachments reach Claude as native content blocks: inline images
/// become image blocks and inline PDFs/text become document blocks, with
/// explicit errors for unsupported or mislabeled media before any request.
#[tokio::test]
async fn prompt_images_and_documents_become_native_claude_blocks() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use nanocodex_agent::input::{Prompt, UserInput};

    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(body);
                (
                    [("content-type", "text/event-stream")],
                    stream(vec![json!({"type":"text","text":"Read both."})], "end_turn"),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "claude-opus-5-5"))
        .build()
        .unwrap();
    let png = png(1, 1);
    let pdf = STANDARD.encode(b"%PDF-1.7\nsynthetic invoice\n%%EOF");
    let notes = STANDARD.encode("Quarterly notes: revenue up.".as_bytes());
    let prompt = Prompt::content([
        UserInput::Text {
            text: "Compare the chart with the invoice.".into(),
        },
        UserInput::Image {
            image_url: format!("data:image/png;base64,{png}"),
            detail: None,
        },
        UserInput::File {
            file_data: format!("data:application/pdf;base64,{pdf}"),
            filename: Some("invoice.pdf".into()),
        },
        UserInput::File {
            file_data: format!("data:text/plain;base64,{notes}"),
            filename: None,
        },
    ]);
    let result = agent.prompt(prompt).await.unwrap().result().await.unwrap();
    assert_eq!(result.final_message(), "Read both.");
    {
        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(
            log[0]["messages"][0]["content"],
            json!([
                {"type":"text","text":"Compare the chart with the invoice."},
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":png}},
                {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":pdf},"title":"invoice.pdf"},
                {"type":"document","source":{"type":"text","media_type":"text/plain","data":"Quarterly notes: revenue up."}},
            ])
        );
    }

    // Subsequent turns replay the admitted content rather than flattening media.
    agent
        .prompt("Recall the attachments")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    {
        let log = requests.lock().unwrap();
        assert_eq!(
            log[1]["messages"][0]["content"],
            log[0]["messages"][0]["content"]
        );
    }
    // JPEG and WebP images that fit are sent unchanged; a GIF becomes a PNG of
    // its first frame, the only frame Claude reads.
    for (format, sent) in [
        (ImageFormat::Jpeg, "image/jpeg"),
        (ImageFormat::Gif, "image/png"),
        (ImageFormat::WebP, "image/webp"),
    ] {
        let mut bytes = std::io::Cursor::new(Vec::new());
        DynamicImage::new_rgb8(1, 1)
            .write_to(&mut bytes, format)
            .unwrap();
        let data = STANDARD.encode(bytes.into_inner());
        let mime = format.to_mime_type();
        agent
            .prompt(Prompt::content([UserInput::Image {
                image_url: format!("data:{mime};base64,{data}"),
                detail: None,
            }]))
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        let log = requests.lock().unwrap();
        let messages = log.last().unwrap()["messages"].as_array().unwrap();
        let block = &messages.last().unwrap()["content"][0];
        assert_eq!(block["source"]["media_type"], sent);
        assert_eq!(image_dimensions(block), Some((1, 1)));
        if sent == mime {
            assert_eq!(block["source"]["data"], data);
        }
    }

    let rejected = |file_data: String, filename: Option<&str>| {
        Prompt::content([UserInput::File {
            file_data,
            filename: filename.map(str::to_owned),
        }])
    };
    for (prompt, expected) in [
        (
            rejected("data:video/mp4;base64,AAAAAGZ0eXA=".into(), None),
            "application/pdf and text/plain",
        ),
        (
            rejected("data:audio/wav;base64,UklGRg==".into(), None),
            "application/pdf and text/plain",
        ),
        (
            rejected(format!("data:application/zip;base64,{pdf}"), None),
            "application/pdf and text/plain",
        ),
        (
            rejected(format!("data:application/pdf;base64,{png}"), None),
            "does not match",
        ),
        (
            rejected("https://example.com/invoice.pdf".into(), None),
            "base64 data URL",
        ),
        (
            rejected(
                format!("data:application/pdf;base64,{pdf}"),
                Some("../etc/passwd"),
            ),
            "filename",
        ),
        (
            Prompt::content((0..6).map(|_| UserInput::File {
                file_data: format!("data:application/pdf;base64,{pdf}"),
                filename: None,
            })),
            "5 documents",
        ),
    ] {
        let error = match agent.prompt(prompt).await {
            Err(error) => error.to_string(),
            Ok(turn) => turn
                .result()
                .await
                .expect_err("invalid media must fail")
                .to_string(),
        };
        assert!(
            error.contains(expected),
            "{error} should mention {expected}"
        );
    }
    assert_eq!(
        requests.lock().unwrap().len(),
        5,
        "invalid media never reaches the provider"
    );
    let artifact = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/claude-host-integration");
    std::fs::create_dir_all(&artifact).unwrap();
    std::fs::write(
        artifact.join("prompt-media-requests.json"),
        serde_json::to_vec_pretty(&*requests.lock().unwrap()).unwrap(),
    )
    .unwrap();
    agent.shutdown().await.unwrap();
    server.abort();
}

/// Prompt images are prepared like tool-result images: converted to a format
/// Claude accepts, reduced at most to the model's native resolution (the size
/// the Messages API would otherwise reduce them to) and the per-image byte
/// limit, or replaced by a note when they cannot be.
#[tokio::test]
async fn prompt_images_are_prepared_for_the_models_native_resolution() {
    use nanocodex_agent::input::{Prompt, UserInput};

    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    // Accept request bodies up to the Messages API's 32 MB limit.
    let app = Router::new()
        .route(
            "/v1/messages",
            post(move |Json(body): Json<Value>| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push(body);
                    (
                        [("content-type", "text/event-stream")],
                        stream(vec![json!({"type":"text","text":"Seen."})], "end_turn"),
                    )
                }
            }),
        )
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let agent = |model: &str| {
        let client = ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        );
        Nanocodex::builder(Claude::new(client, model))
            .build()
            .unwrap()
            .0
    };
    let image = |image_url: String| {
        Prompt::content([UserInput::Image {
            image_url,
            detail: None,
        }])
    };
    let send = async |model: &str, image_url: String| {
        let agent = agent(model);
        let result = agent
            .prompt(image(image_url))
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(result.final_message(), "Seen.");
        agent.shutdown().await.unwrap();
        requests.lock().unwrap().last().unwrap()["messages"][0]["content"][0].clone()
    };

    // Sizes from Anthropic's resolution examples for each tier.
    for (model, (width, height), native) in [
        ("claude-opus-5-5", (3840, 2160), (2576, 1449)),
        ("claude-haiku-4-5", (1920, 1080), (1456, 819)),
        ("claude-haiku-4-5", (1075, 1520), (924, 1307)),
    ] {
        let block = send(
            model,
            format!("data:image/png;base64,{}", png(width, height)),
        )
        .await;
        assert_eq!(block["source"]["media_type"], "image/png");
        assert_eq!(
            image_dimensions(&block),
            Some(native),
            "{model} {width}x{height}"
        );
    }

    let native = png(1920, 1080);
    let block = send("claude-opus-5-5", format!("data:image/png;base64,{native}")).await;
    assert_eq!(
        block["source"]["data"], native,
        "an image within the native resolution is sent unchanged"
    );

    let mut bitmap = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(4, 3)
        .write_to(&mut bitmap, ImageFormat::Bmp)
        .unwrap();
    let bitmap = STANDARD.encode(bitmap.into_inner());
    let block = send("claude-opus-5-5", format!("data:image/bmp;base64,{bitmap}")).await;
    assert_eq!(block["source"]["media_type"], "image/png");
    assert_eq!(image_dimensions(&block), Some((4, 3)));

    let mut jpeg = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(4, 3)
        .write_to(&mut jpeg, ImageFormat::Jpeg)
        .unwrap();
    let jpeg = STANDARD.encode(jpeg.into_inner());
    let block = send("claude-opus-5-5", format!("DATA:image/png;BASE64,{jpeg}")).await;
    assert_eq!(
        block["source"],
        json!({"type":"base64","media_type":"image/jpeg","data":jpeg}),
        "the scheme and encoding are case-insensitive, and the media type comes from the bytes"
    );

    // Rounding the shared resizer's short edge to 1093 would cost 1600 visual
    // tokens, over the standard tier's 1568.
    let block = send(
        "claude-haiku-4-5",
        format!("data:image/png;base64,{}", png(1152, 1140)),
    )
    .await;
    let (width, height) = image_dimensions(&block).unwrap();
    assert!(
        width.div_ceil(28) * height.div_ceil(28) <= 1568,
        "{width}x{height}"
    );
    assert!(width >= 1100, "{width}x{height}");

    let noisy = noise_png(2000, 1000);
    assert!(
        noisy.len() > 5 * 1024 * 1024,
        "the source exceeds the image limit"
    );
    let block = send(
        "claude-opus-5-5",
        format!("data:image/png;base64,{}", STANDARD.encode(noisy)),
    )
    .await;
    let sent = STANDARD
        .decode(block["source"]["data"].as_str().unwrap())
        .unwrap();
    assert!(sent.len() <= 5 * 1024 * 1024, "{} bytes", sent.len());
    assert_eq!(image_dimensions(&block), Some((1500, 750)));

    // A PNG cut off partway through its pixel data, and an image that cannot
    // shrink within the byte limit, each become a note.
    let complete = STANDARD.decode(png(4000, 3000)).unwrap();
    let truncated = STANDARD.encode(&complete[..complete.len() / 2]);
    for image_url in [
        format!("data:image/png;base64,{truncated}"),
        format!("data:image/jpeg;base64,{}", STANDARD.encode(bloated_jpeg())),
    ] {
        let block = send("claude-opus-5-5", image_url).await;
        assert_eq!(block["type"], "text", "{block}");
        assert!(
            block["text"]
                .as_str()
                .unwrap()
                .starts_with("image content omitted"),
            "{block}"
        );
    }
    server.abort();
}

#[tokio::test]
async fn truncated_complete_tool_block_is_never_dispatched_or_finalized() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = requests.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let received = received.clone();
            async move {
                let call = {
                    let mut log = received.lock().unwrap();
                    log.push(body);
                    log.len()
                };
                let response = if call == 1 {
                    let complete = stream(
                        vec![
                            json!({"type":"text","text":"partial progress"}),
                            json!({"type":"tool_use","id":"unsafe","name":"mutate","input":{}}),
                        ],
                        "tool_use",
                    );
                    complete[..complete.rfind("data: {\"type\":\"message_stop\"}").unwrap()]
                        .to_owned()
                } else {
                    stream(
                        vec![json!({"type":"text","text":"explicit recovery"})],
                        "end_turn",
                    )
                };
                ([("content-type", "text/event-stream")], response)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let tools = Arc::new(AtomicUsize::new(0));
    let counter = tools.clone();
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, mut events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tool(
            ToolDefinition {
                name: "mutate".into(),
                description: "Synthetic mutation".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok("must not execute".to_owned()) }
            },
        )
        .build()
        .unwrap();
    let failed = agent
        .prompt("perform one mutation")
        .await
        .unwrap()
        .result()
        .await;
    assert!(failed.is_err());
    assert_eq!(
        tools.load(Ordering::SeqCst),
        0,
        "missing terminal cannot authorize dispatch"
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "accepted incomplete request must not be retried"
    );
    loop {
        let event = events.next().await.unwrap();
        assert_ne!(
            event.kind,
            AgentEventKind::AssistantMessage,
            "no canonical success from partial stream"
        );
        assert_ne!(
            event.kind,
            AgentEventKind::ToolCall,
            "no tools from partial stream"
        );
        if event.kind == AgentEventKind::RunFailed {
            break;
        }
    }
    assert_eq!(
        agent
            .prompt("continue explicitly")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "explicit recovery"
    );
    {
        let log = requests.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert!(
            !log[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["role"] == "assistant"),
            "partial text and complete-looking tool block cannot enter replay history"
        );
    }
    agent.shutdown().await.unwrap();
    server.abort();
}

// Per-tool gating (Claude Code scheduling): consecutive declared parallel-safe
// calls overlap, while an undeclared call waits for them and runs alone before
// any later call starts. Results stay in response order in one user message.
#[tokio::test]
async fn parallel_safe_tools_overlap_only_in_runs_and_unsafe_calls_stay_ordered() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = captured.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let received = received.clone();
            async move {
                let index = {
                    let mut requests = received.lock().unwrap();
                    requests.push(body);
                    requests.len()
                };
                let blocks = if index == 1 {
                    vec![
                        json!({"type":"tool_use","id":"r1","name":"read","input":{}}),
                        json!({"type":"tool_use","id":"r2","name":"read","input":{}}),
                        json!({"type":"tool_use","id":"w1","name":"write","input":{}}),
                        json!({"type":"tool_use","id":"r3","name":"read","input":{}}),
                    ]
                } else {
                    vec![json!({"type":"text","text":"done"})]
                };
                (
                    [("content-type", "text/event-stream")],
                    stream(blocks, if index == 1 { "tool_use" } else { "end_turn" }),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let trace = Arc::new(Mutex::new(Vec::<String>::new()));
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let definition = |name: &str| ToolDefinition {
        name: name.into(),
        description: "Synthetic".into(),
        input_schema: json!({"type":"object"}),
        strict: None,
        defer_loading: false,
    };
    let handler = |kind: &'static str| {
        let (trace, active, peak) = (trace.clone(), active.clone(), peak.clone());
        move |_input: Value, invocation: nanocodex_claude::ClaudeToolInvocation| {
            let (trace, active, peak) = (trace.clone(), active.clone(), peak.clone());
            async move {
                let id = invocation.call_id;
                trace.lock().unwrap().push(format!("start {id}"));
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                trace.lock().unwrap().push(format!("end {id}"));
                Ok::<_, String>(nanocodex_claude::ClaudeToolReply::success(
                    nanocodex_claude::ToolResultContent::Text(format!("{kind} {id}")),
                ))
            }
        }
    };
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .parallel_safe_tools(["read"])
        .tool_with_context(definition("read"), handler("read"))
        .tool_with_context(definition("write"), handler("write"))
        .build()
        .unwrap();
    let result = agent.prompt("go").await.unwrap().result().await.unwrap();
    assert_eq!(result.final_message(), "done");
    let trace = trace.lock().unwrap().clone();
    let at = |entry: &str| trace.iter().position(|e| e == entry).unwrap();
    // r1 and r2 overlap; w1 starts only after both finish; r3 only after w1.
    assert!(
        at("start r2") < at("end r1") && at("start r1") < at("end r2"),
        "{trace:?}"
    );
    assert!(
        at("end r1") < at("start w1") && at("end r2") < at("start w1"),
        "{trace:?}"
    );
    assert!(at("end w1") < at("start r3"), "{trace:?}");
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    let requests = captured.lock().unwrap();
    let returned = &requests[1]["messages"][2]["content"];
    let ids: Vec<_> = returned
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["tool_use_id"].clone())
        .collect();
    assert_eq!(ids, [json!("r1"), json!("r2"), json!("w1"), json!("r3")]);
}

/// Actual Messages SSE -> public agent events, with the provider paused after
/// progress so a completed-response-only implementation cannot pass.
#[tokio::test]
async fn progress_updates_stream_before_tools_and_preserve_private_continuation() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for (model, updates, subscription) in [
        ("claude-opus-5-5", true, false),
        ("claude-opus-5-5", true, true),
        ("claude-fable-5-1", true, false),
        ("claude-sonnet-5-5", true, false),
        ("claude-haiku-5-5", false, false),
        ("claude-sonnet-4-6", false, false),
    ] {
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let release = Arc::new(tokio::sync::Notify::new());
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route("/v1/messages", post({
            let requests = requests.clone();
            let release = release.clone();
            move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                let requests = requests.clone();
                let release = release.clone();
                async move {
                    assert_eq!(body["output_config"]["effort"], "high");
                    assert_eq!(body["thinking"]["display"], if updates { json!("updates") } else { Value::Null });
                    let betas = headers.get("anthropic-beta").and_then(|v| v.to_str().ok()).unwrap_or("");
                    assert_eq!(betas.split(',').filter(|beta| *beta == "thinking-display-updates-2026-08-18").count(), usize::from(updates));
                    let count = { let mut log = requests.lock().unwrap(); log.push(body); log.len() };
                    if count > 1 {
                        return ([("content-type", "text/event-stream")], stream(vec![json!({"type":"text","text":"Lookup complete."})], "end_turn")).into_response();
                    }
                    let events = [
                        json!({"type":"message_start","message":{"id":"progress-message","role":"assistant","model":model,"content":[],"usage":{"input_tokens":3,"output_tokens":0}}}),
                        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}),
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"PRIVATE_REASONING_SIGNATURE"}}),
                        json!({"type":"content_block_stop","index":0}),
                        json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"PRIVATE_REDACTED_BLOCK"}}),
                        json!({"type":"content_block_stop","index":1}),
                        json!({"type":"content_block_start","index":2,"content_block":{"type":"thinking","thinking":"","signature":""}}),
                        json!({"type":"content_block_delta","index":2,"delta":{"type":"thinking_delta","thinking":"Found the record. "}}),
                        json!({"type":"content_block_delta","index":2,"delta":{"type":"thinking_delta","thinking":"Checking café details."}}),
                        json!({"type":"content_block_delta","index":2,"delta":{"type":"signature_delta","signature":"PRIVATE_UPDATE_SIGNATURE"}}),
                        json!({"type":"content_block_stop","index":2}),
                    ];
                    let prefix: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
                    let tail: String = [
                        json!({"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"lookup-1","name":if subscription {"_lookup"} else {"lookup"},"input":{}}}),
                        json!({"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
                        json!({"type":"content_block_stop","index":3}),
                        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}}),
                        json!({"type":"message_stop"}),
                    ].iter().map(|e| format!("data: {e}\n\n")).collect();
                    let chunks = futures_util::stream::once(async move { Ok::<_, std::io::Error>(prefix) })
                        .chain(futures_util::stream::once(async move { release.notified().await; Ok::<_, std::io::Error>(tail) }));
                    axum::response::Response::builder().header("content-type", "text/event-stream")
                        .body(axum::body::Body::from_stream(chunks)).unwrap()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ClaudeClient::new(reqwest::Client::new(), endpoint, "synthetic");
        let client = if subscription {
            client.subscription_compatibility()
        } else {
            client
        };
        let counter = tool_calls.clone();
        let (agent, mut events) = Nanocodex::builder(Claude::new(client, model))
            .thinking(nanocodex_agent::Thinking::High)
            .unwrap()
            .tool(
                ToolDefinition {
                    name: "lookup".into(),
                    description: "Read a synthetic record".into(),
                    input_schema: json!({"type":"object"}),
                    strict: None,
                    defer_loading: false,
                },
                move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { Ok("record verified".into()) }
                },
            )
            .build()
            .unwrap();
        let turn = agent
            .prompt("Check the record and report progress.")
            .await
            .unwrap();
        let mut visible = String::new();
        let mut evidence = Vec::new();
        while visible != "Found the record. Checking café details." {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
                .await
                .unwrap()
                .unwrap();
            let payload: Value = serde_json::from_str(event.payload.get()).unwrap();
            if event.kind == AgentEventKind::ReasoningSummaryDelta {
                assert_eq!(payload["item_id"], "progress-message:thinking:2");
                let text = payload["text"].as_str().unwrap();
                assert!(!text.is_empty());
                visible.push_str(text);
            }
            evidence.push(json!({"kind":format!("{:?}",event.kind),"payload":payload}));
        }
        assert_eq!(
            tool_calls.load(Ordering::SeqCst),
            0,
            "progress must be delivered while provider is still streaming"
        );
        release.notify_one();
        assert_eq!(
            turn.result().await.unwrap().final_message(),
            "Lookup complete."
        );
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
                .await
                .unwrap()
                .unwrap();
            let payload: Value = serde_json::from_str(event.payload.get()).unwrap();
            evidence.push(json!({"kind":format!("{:?}",event.kind),"payload":payload}));
            if event.kind == AgentEventKind::RunCompleted {
                break;
            }
        }
        assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
        let display: Vec<_> = evidence
            .iter()
            .filter(|e| {
                matches!(
                    e["kind"].as_str(),
                    Some("ReasoningSummaryDelta" | "AssistantDelta" | "AssistantMessage")
                )
            })
            .collect();
        assert!(
            !serde_json::to_string(&display)
                .unwrap()
                .contains("PRIVATE_")
        );
        let requests = requests.lock().unwrap();
        let continuation = &requests[1]["messages"][1]["content"];
        assert_eq!(continuation[0]["thinking"], "");
        assert_eq!(continuation[0]["signature"], "PRIVATE_REASONING_SIGNATURE");
        assert_eq!(continuation[1]["data"], "PRIVATE_REDACTED_BLOCK");
        assert_eq!(continuation[2]["thinking"], visible);
        assert_eq!(continuation[2]["signature"], "PRIVATE_UPDATE_SIGNATURE");
        let dir = std::path::Path::new("../../output/claude-progress");
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{model}-subscription-{subscription}.json")), serde_json::to_vec_pretty(&json!({"model":model,"subscription":subscription,"updates":updates,"progress_before_tool":true,"private_continuation_preserved":true,"events":evidence})).unwrap()).unwrap();
        server.abort();
    }
}
