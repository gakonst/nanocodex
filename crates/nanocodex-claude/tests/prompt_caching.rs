//! Protocol failures are exercised through the HTTP boundary; provider cache hits
//! and billing cannot be established by these synthetic loopback journeys.
use axum::{Json, Router, http::HeaderMap, routing::post};
use nanocodex_agent::Nanocodex;
use nanocodex_claude::{
    CacheControl, CacheTtl, CacheType, Claude, ClaudeClient, ClaudeError, ContentBlock, Message,
    MessagesRequest, Role, ServerToolDefinition, ToolDefinition,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn request() -> MessagesRequest {
    MessagesRequest {
        model: "test".into(),
        max_tokens: 128,
        cache_control: Some(CacheControl::ephemeral()),
        output_config: None,
        speed: None,
        tool_choice: None,
        thinking: None,
        context_management: None,
        diagnostics: None,
        system: None,
        container: None,
        tools: vec![],
        messages: vec![Message::text(Role::User, "hello")],
    }
}

fn marked(ttl: &str) -> Value {
    json!({"type":"text","text":"stable instructions","cache_control":{"type":"ephemeral","ttl":ttl}})
}

fn block(value: Value) -> ContentBlock {
    serde_json::from_value(value).unwrap()
}

async fn fixture() -> (
    ClaudeClient,
    Arc<Mutex<Vec<(HeaderMap, Value)>>>,
    tokio::task::JoinHandle<()>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let log = Arc::new(Mutex::new(vec![]));
    let captured = log.clone();
    let app = Router::new().route("/v1/messages", post(move |headers: HeaderMap, Json(body): Json<Value>| {
        captured.lock().unwrap().push((headers, body));
        async { Json(json!({"id":"synthetic","role":"assistant","model":"test","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn","usage":{}})) }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        ),
        log,
        server,
    )
}

// Failure modes: malformed raw controls, >4 total markers, short-before-long
// TTLs, automatic/explicit conflicts, and forbidden signed/empty targets.
#[tokio::test]
async fn invalid_cache_controls_fail_before_http_without_mutating_history() {
    let (client, log, server) = fixture().await;
    let mut invalid = vec![];
    let mut r = request();
    r.system = Some(json!([marked("5m"), marked("1h")]));
    invalid.push(("TTL order", r));
    let mut r = request();
    r.system = Some(json!([
        marked("5m"),
        marked("5m"),
        marked("5m"),
        marked("5m")
    ]));
    invalid.push(("automatic fifth breakpoint", r));
    let mut r = request();
    r.cache_control = None;
    r.system = Some(json!(vec![marked("5m"); 5]));
    invalid.push(("explicit fifth breakpoint", r));
    let mut r = request();
    r.messages[0].content = vec![block(marked("1h"))];
    invalid.push(("last-block automatic TTL conflict", r));
    for value in [
        json!({"type":"text","text":"x","cache_control":{"type":"permanent"}}),
        json!({"type":"text","text":"x","cache_control":{"type":"ephemeral","ttl":"2h"}}),
        json!({"type":"text","text":"x","cache_control":null}),
        json!({"type":"text","text":"","cache_control":{"type":"ephemeral"}}),
        json!({"type":"thinking","thinking":"signed","signature":"opaque","cache_control":{"type":"ephemeral"}}),
        json!({"type":"redacted_thinking","data":"opaque","cache_control":{"type":"ephemeral"}}),
    ] {
        let mut r = request();
        r.messages[0].content = vec![block(value)];
        invalid.push(("malformed or forbidden block control", r));
    }
    let mut r = request();
    let mut tool = ServerToolDefinition::web_search_basic(1);
    tool.options.insert(
        "cache_control".into(),
        json!({"type":"ephemeral","ttl":"5m"}),
    );
    r.tools.push(tool.into());
    r.system = Some(json!([marked("1h")]));
    invalid.push(("tool before system TTL order", r));
    for (label, r) in invalid {
        let before = serde_json::to_value(&r).unwrap();
        let error = client.create(&r).await.expect_err(label);
        assert!(
            matches!(error, ClaudeError::Protocol(_)),
            "{label}: {error}"
        );
        assert_eq!(serde_json::to_value(&r).unwrap(), before, "{label}");
    }
    assert!(
        log.lock().unwrap().is_empty(),
        "invalid cache requests reached HTTP"
    );
    server.abort();
}

#[tokio::test]
async fn valid_mixed_ttls_and_automatic_fallback_preserve_signed_replay() {
    let (client, log, server) = fixture().await;
    let mut r = request();
    r.system = Some(json!([marked("1h"), marked("5m"), marked("5m")]));
    client.create(&r).await.unwrap(); // Three explicit plus automatic.
    let signed = json!({"type":"thinking","thinking":"exact bytes\n","signature":"opaque-signature","binding":"opaque"});
    r.messages = vec![Message {
        role: Role::Assistant,
        content: vec![
            block(marked("5m")),
            block(signed.clone()),
            block(json!({"type":"redacted_thinking","data":"opaque"})),
            ContentBlock::text(""),
        ],
    }];
    r.system = Some(json!([marked("1h"), marked("5m")]));
    let before = serde_json::to_value(&r).unwrap();
    client.create(&r).await.unwrap(); // Automatic falls back and deduplicates equal TTL.
    assert_eq!(serde_json::to_value(&r).unwrap(), before);
    let mut r = request();
    r.cache_control = None;
    r.system = Some(json!([
        marked("5m"),
        marked("5m"),
        marked("5m"),
        marked("5m")
    ]));
    // User data named cache_control is not a protocol marker.
    r.messages[0].content = vec![ContentBlock::tool_use(
        "t",
        "x",
        json!({"cache_control":{"type":"permanent"}}),
    )];
    client.create(&r).await.unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 3);
    assert_eq!(log[1].1["messages"][0]["content"][1], signed);
    assert!(
        log.iter()
            .all(|(headers, _)| !headers.contains_key("anthropic-beta"))
    );
    server.abort();
}

// Compaction replaces message history: a stable system marker must already have
// been written on the preceding requests, not invented only after compaction.
#[tokio::test]
async fn automatic_agent_cache_keeps_system_prefix_across_compaction() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let log = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = log.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        captured.lock().unwrap().push(body);
        async { ([("content-type", "text/event-stream")], concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg\",\"role\":\"assistant\",\"model\":\"test\",\"content\":[],\"usage\":{}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"Summary\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        )) }
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
        .cache_one_hour()
        .system("Stable system")
        .build()
        .unwrap();
    agent.prompt("first").await.unwrap().result().await.unwrap();
    agent.compact().await.unwrap();
    agent
        .prompt("continue")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 3);
    assert_eq!(
        log[0]["system"],
        json!([{"type":"text","text":"Stable system","cache_control":{"type":"ephemeral","ttl":"1h"}}])
    );
    assert_eq!(log[0]["system"], log[1]["system"]);
    assert_eq!(log[0]["system"], log[2]["system"]);
    assert_eq!(log[0]["tools"], log[2]["tools"]);
    assert_eq!(log[0]["cache_control"], log[2]["cache_control"]);
    assert_ne!(log[0]["messages"], log[2]["messages"]);
    server.abort();
}

#[tokio::test]
async fn stable_prefix_preparation_preserves_caller_policy_and_budget() {
    let (client, log, server) = fixture().await;
    let mut inputs = vec![];
    let mut r = request();
    r.system = Some(json!([{"type":"text","text":"system"}]));
    r.cache_control = None;
    inputs.push(r); // No opt-in: no new marker.
    let mut r = request();
    r.system = Some(json!([marked("1h"),{"type":"text","text":"changing suffix"}]));
    inputs.push(r); // Honor caller-selected stable prefix.
    let mut r = request();
    r.system = Some(json!([{"type":"text","text":"system"}]));
    r.messages[0].content = vec![
        block(marked("1h")),
        block(marked("5m")),
        block(marked("5m")),
        ContentBlock::text("tail"),
    ];
    inputs.push(r); // All four slots already allocated.
    let mut r = request();
    r.system = Some(json!([{"type":"text","text":"system"}]));
    r.messages[0].content = vec![block(marked("1h")), ContentBlock::text("tail")];
    inputs.push(r); // Adding the automatic 5m TTL before the 1h marker is illegal.
    for mut r in inputs {
        let before = serde_json::to_value(&r).unwrap();
        r.cache_system_prefix().unwrap();
        assert_eq!(serde_json::to_value(&r).unwrap(), before);
        client.create(&r).await.unwrap();
    }
    let mut r = request();
    r.system = Some(json!("system"));
    r.cache_system_prefix().unwrap();
    let before = serde_json::to_value(&r).unwrap();
    r.cache_system_prefix().unwrap();
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        before,
        "preparation must be idempotent"
    );
    client.create(&r).await.unwrap();
    assert_eq!(log.lock().unwrap().len(), 5);
    server.abort();
}

#[tokio::test]
async fn explicit_tool_result_breakpoint_survives_replay_and_is_validated() {
    let (client, log, server) = fixture().await;
    let result = json!({"type":"tool_result","tool_use_id":"lookup-1","content":[{"type":"text","text":"found"}],"cache_control":{"type":"ephemeral","ttl":"1h"}});
    let mut r = request();
    r.cache_control = None;
    r.messages[0].content = vec![block(result.clone())];
    client.create(&r).await.unwrap();
    assert_eq!(
        log.lock().unwrap()[0].1["messages"][0]["content"][0],
        result
    );
    r.cache_control = Some(CacheControl::ephemeral());
    assert!(matches!(
        client.create(&r).await,
        Err(ClaudeError::Protocol(_))
    ));
    assert_eq!(log.lock().unwrap().len(), 1);
    server.abort();
}

/// Every `cache_control` location in a captured wire body, in cache order.
fn marker_paths(body: &Value) -> Vec<String> {
    let mut paths = vec![];
    if body.get("cache_control").is_some() {
        paths.push("cache_control".to_owned());
    }
    for (index, tool) in body["tools"].as_array().into_iter().flatten().enumerate() {
        if tool.get("cache_control").is_some() {
            paths.push(format!("tools[{index}]"));
        }
    }
    for (index, block) in body["system"].as_array().into_iter().flatten().enumerate() {
        if block.get("cache_control").is_some() {
            paths.push(format!("system[{index}]"));
        }
    }
    for (m, message) in body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        for (b, block) in message["content"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            if block.get("cache_control").is_some() {
                paths.push(format!("messages[{m}][{b}]"));
            }
        }
    }
    paths
}

fn sse(blocks: &[Value], stop: &str) -> String {
    let mut frames = vec![
        json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"test","content":[],"usage":{}}}),
    ];
    for (index, block) in blocks.iter().enumerate() {
        frames.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        frames.push(json!({"type":"content_block_stop","index":index}));
    }
    frames.push(
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":1}}),
    );
    frames.push(json!({"type":"message_stop"}));
    frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect()
}

// Claude Code sends no top-level automatic cache field on the subscription
// wire: two system markers plus one marker that moves to the final cacheable
// block of each request, all with the 1h TTL, including compaction requests.
#[tokio::test]
async fn subscription_wire_moves_one_explicit_final_block_marker() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let log = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = log.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let index = {
                let mut log = captured.lock().unwrap();
                log.push(body);
                log.len()
            };
            let (blocks, stop) = match index {
                1 => (
                    vec![
                        json!({"type":"thinking","thinking":"plan","signature":"opaque-signature"}),
                        json!({"type":"tool_use","id":"lookup-1","name":"_lookup","input":{}}),
                    ],
                    "tool_use",
                ),
                4 => (
                    vec![json!({"type":"text","text":"Summary: lookup found."})],
                    "end_turn",
                ),
                _ => (vec![json!({"type":"text","text":"done"})], "end_turn"),
            };
            async move { ([("content-type", "text/event-stream")], sse(&blocks, stop)) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    )
    .subscription_compatibility();
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .cache_one_hour()
        .system("Stable system")
        .tool(
            ToolDefinition {
                name: "lookup".into(),
                description: "Synthetic lookup".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            |_| async { Ok("found".into()) },
        )
        .build()
        .unwrap();
    agent.prompt("first").await.unwrap().result().await.unwrap();
    agent
        .prompt("continue")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    agent.compact().await.unwrap();
    agent.prompt("after").await.unwrap().result().await.unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 5);
    let one_hour = json!({"type":"ephemeral","ttl":"1h"});
    for (index, body) in log.iter().enumerate() {
        let messages = body["messages"].as_array().unwrap();
        let last = messages.len() - 1;
        let tail = messages[last]["content"].as_array().unwrap().len() - 1;
        assert_eq!(
            marker_paths(body),
            vec![
                "system[1]".to_owned(),
                "system[2]".to_owned(),
                format!("messages[{last}][{tail}]"),
            ],
            "request {index}"
        );
        assert_eq!(body["system"][1]["cache_control"], one_hour);
        assert_eq!(body["system"][2]["text"], "Stable system");
        assert_eq!(body["system"][2]["cache_control"], one_hour);
        assert_eq!(messages[last]["content"][tail]["cache_control"], one_hour);
        // Stable marked prefix: identity, instructions and tools never change.
        assert_eq!(
            body["system"].as_array().unwrap()[1..],
            log[0]["system"].as_array().unwrap()[1..]
        );
        assert_eq!(body["tools"], log[0]["tools"]);
    }
    // The final block is the tool result, never the signed thinking block.
    assert_eq!(log[1]["messages"][2]["content"][0]["type"], "tool_result");
    assert_eq!(log[1]["messages"][1]["content"][0]["type"], "thinking");
    assert_eq!(
        log[1]["messages"][1]["content"][0]["signature"],
        "opaque-signature"
    );
    // The marker moved: the earlier user tail is replayed unmarked.
    assert_eq!(
        log[2]["messages"][2]["content"][0]["tool_use_id"],
        "lookup-1"
    );
    assert_eq!(log[2]["messages"][4]["content"][0]["text"], "continue");
    // Compaction marks its instruction tail; the next request follows the summary.
    assert_eq!(log[3]["tool_choice"], json!({"type":"none"}));
    assert!(log[4].to_string().contains("Summary: lookup found."));
    server.abort();
}

// Direct client requests apply the same conversion on the subscription wire,
// skip signed/empty tails, deduplicate an equal explicit tail, and leave the
// public API body's top-level automatic field unchanged.
#[tokio::test]
async fn explicit_tail_conversion_skips_ineligible_blocks_and_dedupes() {
    let (api, api_log, api_server) = fixture().await;
    let (subscription, log, server) = fixture().await;
    let subscription = subscription.subscription_compatibility();
    let mut r = request();
    r.cache_control = Some(CacheControl {
        kind: CacheType::Ephemeral,
        ttl: Some(CacheTtl::OneHour),
    });
    r.messages = vec![
        Message::text(Role::User, "question"),
        Message {
            role: Role::Assistant,
            content: vec![
                block(json!({"type":"text","text":"answer"})),
                block(json!({"type":"thinking","thinking":"signed","signature":"opaque"})),
                ContentBlock::text(""),
            ],
        },
    ];
    let before = serde_json::to_value(&r).unwrap();
    subscription.create(&r).await.unwrap();
    api.create(&r).await.unwrap();
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        before,
        "logical request unchanged"
    );
    let mut deduped = r.clone();
    deduped.messages[1].content[0] = block(
        json!({"type":"text","text":"answer","cache_control":{"type":"ephemeral","ttl":"1h"}}),
    );
    subscription.create(&deduped).await.unwrap();
    let log = log.lock().unwrap();
    for body in [&log[0].1, &log[1].1] {
        assert_eq!(marker_paths(body), ["system[1]", "messages[1][0]"],);
        assert_eq!(
            body["messages"][1]["content"][0]["cache_control"],
            json!({"type":"ephemeral","ttl":"1h"})
        );
    }
    let api_log = api_log.lock().unwrap();
    assert_eq!(marker_paths(&api_log[0].1), ["cache_control"]);
    assert_eq!(
        api_log[0].1["cache_control"],
        json!({"type":"ephemeral","ttl":"1h"})
    );
    server.abort();
    api_server.abort();
}
