//! Reproducible mock HTTP journeys: run `cargo test -p nanocodex-claude --test messages`.
//! No network outside a loopback listener and no real provider credentials.
use std::sync::{Arc, Mutex};

use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
use futures_util::StreamExt;
use nanocodex_claude::{
    ClaudeClient, ClaudeError, ContentBlock, Message, MessagesRequest, Role, StopReason,
    StreamEvent, ToolDefinition, collect_stream, compact_history,
};
use serde_json::{Value, json};

fn http_client() -> reqwest::Client {
    static PROVIDER: std::sync::Once = std::sync::Once::new();
    PROVIDER.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("crypto provider");
    });
    reqwest::Client::new()
}

async fn server(
    handler: impl Fn(Value) -> (StatusCode, &'static str, String) + Send + Sync + 'static,
) -> String {
    let handler = Arc::new(handler);
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let handler = handler.clone();
            async move {
                let (status, media_type, response) = handler(body);
                (status, [("content-type", media_type)], response).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}/v1/messages")
}

fn request() -> MessagesRequest {
    MessagesRequest {
        model: "claude-test".into(),
        max_tokens: 128,
        cache_control: None,
        output_config: None,
        speed: None,
        tool_choice: None,
        thinking: None,
        context_management: None,
        diagnostics: None,
        system: Some("Use tools".into()),
        messages: vec![Message::text(Role::User, "What's the weather?")],
        container: None,
        tools: vec![
            ToolDefinition {
                name: "weather".into(),
                description: "Find current weather".into(),
                input_schema: json!({"type":"object","properties":{"city":{"type":"string"}}}),
                strict: None,
                defer_loading: false,
            }
            .into(),
        ],
    }
}

#[tokio::test]
async fn streams_a_tool_call_then_continues_with_matching_result() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let endpoint = server(move |body| {
        captured.lock().unwrap().push(body.clone());
        if body["stream"] == true {
            let frames = [
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-test\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
                ": heartbeat\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"Checking \"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"now\"}}\n\n",
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"weather\",\"input\":{}}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\\\"Athens\\\"}\"}}\n\n",
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":12}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            ];
            (StatusCode::OK, "text/event-stream", frames.concat())
        } else {
            (StatusCode::OK, "application/json", json!({"id":"msg_2","type":"message","role":"assistant","model":"claude-test","content":[{"type":"text","text":"Sunny"}],"stop_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":2}}).to_string())
        }
    }).await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
    let mut initial = request();
    let mut stream = client.stream(&initial).await.unwrap();
    let first = stream.next().await.unwrap().unwrap();
    assert!(matches!(first, StreamEvent::MessageStart { .. }));
    let streamed = collect_stream(first, stream).await.unwrap();
    assert_eq!(streamed.id, "msg_1");
    assert_eq!(streamed.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(streamed.usage.output_tokens, 12);
    assert_eq!(streamed.content[0], ContentBlock::text("Checking now"));
    assert_eq!(
        streamed.content[1],
        ContentBlock::tool_use("toolu_1", "weather", json!({"city":"Athens"}))
    );

    initial.messages.push(Message {
        role: Role::Assistant,
        content: streamed.content,
    });
    initial
        .messages
        .push(Message::tool_results(vec![ContentBlock::tool_result(
            "toolu_1", "Sunny", false,
        )]));
    let final_message = client.create(&initial).await.unwrap();
    assert_eq!(final_message.content, vec![ContentBlock::text("Sunny")]);
    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(
        captured[0]["tools"][0]["input_schema"]["properties"]["city"]["type"],
        "string"
    );
    assert_eq!(captured[1]["messages"][1]["content"][1]["type"], "tool_use");
    assert_eq!(captured[1]["messages"][2]["role"], "user");
    assert_eq!(
        captured[1]["messages"][2]["content"][0]["tool_use_id"],
        "toolu_1"
    );
    assert_eq!(captured[1]["stream"], false);
}

#[tokio::test]
async fn surfaces_http_and_truncated_stream_failures() {
    let endpoint = server(|body| {
        if body["stream"] == true {
            (StatusCode::OK, "text/event-stream", "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg\",\"role\":\"assistant\",\"model\":\"x\",\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n".into())
        } else {
            (StatusCode::UNAUTHORIZED, "application/json", json!({"type":"error","error":{"type":"authentication_error","message":"invalid key"}}).to_string())
        }
    }).await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
    assert!(matches!(
        client.create(&request()).await,
        Err(ClaudeError::Http { status: 401, .. })
    ));
    let mut stream = client.stream(&request()).await.unwrap();
    assert!(matches!(
        stream.next().await.unwrap(),
        Ok(StreamEvent::MessageStart { .. })
    ));
    assert!(matches!(
        stream.next().await.unwrap(),
        Err(ClaudeError::IncompleteStream)
    ));
}

#[test]
fn compaction_retains_tool_use_with_result_at_boundary() {
    let history = vec![
        Message::text(Role::User, "old"),
        Message::text(Role::Assistant, "old response"),
        Message::text(Role::User, "find weather"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::tool_use(
                "toolu_1",
                "weather",
                json!({"city":"Athens"}),
            )],
        },
        Message::tool_results(vec![ContentBlock::tool_result("toolu_1", "Sunny", false)]),
        Message::text(Role::Assistant, "Sunny"),
    ];
    let compacted = compact_history(&history, 2, "Earlier query resolved.");
    assert_eq!(compacted.dropped_messages, 2);
    assert_eq!(compacted.messages, history[2..]);
    assert_eq!(compacted.summary, "Earlier query resolved.");
    assert!(
        compacted
            .system_context("Use tools")
            .contains("Earlier query resolved.")
    );
}

#[tokio::test]
async fn reports_in_band_error_and_rejects_malformed_tool_json() {
    let endpoint = server(|body| {
        if body["model"] == "error" {
            (StatusCode::OK, "text/event-stream", "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"try later\"}}\n\n".into())
        } else {
            (StatusCode::OK, "text/event-stream", concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg\",\"role\":\"assistant\",\"model\":\"x\",\"content\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu\",\"name\":\"foo\",\"input\":{}}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{broken\"}}\n\n",
                "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
            ).into())
        }
    }).await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
    let mut req = request();
    req.model = "error".into();
    let mut events = client.stream(&req).await.unwrap();
    assert!(matches!(
        events.next().await.unwrap(),
        Err(ClaudeError::StreamError { .. })
    ));
    assert!(events.next().await.is_none());
    req.model = "malformed".into();
    let mut events = client.stream(&req).await.unwrap();
    let first = events.next().await.unwrap().unwrap();
    assert!(collect_stream(first, events).await.is_err());
}

#[tokio::test]
async fn sets_console_api_headers_without_exposing_key_in_request_body() {
    use axum::http::HeaderMap;
    let app = Router::new().route("/v1/messages", post(|headers: HeaderMap, Json(body): Json<Value>| async move {
        assert_eq!(headers.get("x-api-key").unwrap(), "synthetic-key");
        assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
        assert!(body.get("api_key").is_none());
        Json(json!({"id":"msg","role":"assistant","model":"x","content":[],"usage":{"input_tokens":1,"output_tokens":0}}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        http_client(),
        format!("http://{address}/v1/messages"),
        "synthetic-key",
    );
    assert_eq!(client.create(&request()).await.unwrap().id, "msg");
}

#[tokio::test]
async fn accepts_caller_supplied_auth_headers_without_forcing_api_key() {
    use axum::http::HeaderMap;
    let app = Router::new().route(
        "/v1/messages",
        post(|headers: HeaderMap| async move {
            assert_eq!(headers.get("authorization").unwrap(), "Bearer synthetic-token");
            assert!(headers.get("x-api-key").is_none());
            Json(json!({"id":"msg","role":"assistant","model":"x","content":[],"usage":{"input_tokens":1,"output_tokens":0}}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        "Bearer synthetic-token".parse().unwrap(),
    );
    let client = ClaudeClient::with_auth_headers(
        http_client(),
        format!("http://{address}/v1/messages"),
        headers,
    );
    assert_eq!(client.create(&request()).await.unwrap().id, "msg");
}

#[tokio::test]
async fn preserves_signed_thinking_and_cache_usage_across_stream_and_replay() {
    let endpoint = server(|_| {
        (StatusCode::OK, "text/event-stream", [
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_t\",\"role\":\"assistant\",\"model\":\"test\",\"content\":[],\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":5,\"cache_creation_input_tokens\":3,\"output_tokens\":0}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"check\",\"signature\":\"\",\"binding\":\"future-preserved\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\" result\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"signed-payload\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque-data\",\"binding\":\"redacted-preserved\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ].concat())
    }).await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
    let mut events = client.stream(&request()).await.unwrap();
    let first = events.next().await.unwrap().unwrap();
    let completed = collect_stream(first, events).await.unwrap();
    assert_eq!(completed.usage.cache_read_input_tokens, 5);
    assert_eq!(completed.usage.cache_creation_input_tokens, 3);
    assert_eq!(completed.usage.output_tokens, 4);
    assert_eq!(
        serde_json::to_value(&completed.content).unwrap(),
        json!([
            {"type":"thinking","thinking":"check result","signature":"signed-payload","binding":"future-preserved"},
            {"type":"redacted_thinking","data":"opaque-data","binding":"redacted-preserved"}
        ])
    );
    let replay = Message {
        role: Role::Assistant,
        content: completed.content,
    };
    assert_eq!(
        serde_json::to_value(replay).unwrap()["content"][0]["signature"],
        "signed-payload"
    );
}

#[test]
fn serializes_automatic_cache_strict_tools_and_multimodal_tool_results() {
    use nanocodex_claude::{CacheControl, Effort, OutputConfig, ToolResultContent};
    let mut req = request();
    req.cache_control = Some(CacheControl::ephemeral());
    req.output_config = Some(OutputConfig {
        effort: Effort::High,
    });
    let nanocodex_claude::ClaudeToolSpec::Client(tool) = &mut req.tools[0] else {
        panic!("client tool");
    };
    tool.strict = Some(true);
    req.messages.push(Message::tool_results(vec![ContentBlock::tool_result_blocks(
        "toolu_1",
        vec![
            json!({"type":"text","text":"The chart is attached"}),
            json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"cG5n"}}),
        ],
        false,
    )]));
    let serialized = serde_json::to_value(&req).unwrap();
    assert_eq!(serialized["cache_control"], json!({"type":"ephemeral"}));
    assert_eq!(serialized["output_config"], json!({"effort":"high"}));
    assert_eq!(serialized["tools"][0]["strict"], true);
    assert_eq!(
        serialized["messages"][1]["content"][0]["content"][1]["type"],
        "image"
    );
    let result =
        serde_json::from_value::<ContentBlock>(serialized["messages"][1]["content"][0].clone())
            .unwrap();
    assert!(matches!(
        result,
        ContentBlock::ToolResult {
            content: ToolResultContent::Blocks(_),
            ..
        }
    ));
}

#[tokio::test]
async fn stream_rejects_mismatched_event_name_and_oversize_multiline_frame() {
    let endpoint = server(|body| {
        if body["model"] == "mismatch" {
            (StatusCode::OK, "text/event-stream", "event: message_stop\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"x\",\"role\":\"assistant\",\"model\":\"x\",\"content\":[],\"usage\":{}}}\n\n".into())
        } else {
            let mut payload = String::from("event: ping\n");
            for _ in 0..34 { payload.push_str(&format!("data: {}\n", "x".repeat(1_000_000))); }
            payload.push('\n');
            (StatusCode::OK, "text/event-stream", payload)
        }
    }).await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
    let mut req = request();
    req.model = "mismatch".into();
    let mut stream = client.stream(&req).await.unwrap();
    assert!(matches!(
        stream.next().await.unwrap(),
        Err(ClaudeError::Protocol(_))
    ));
    req.model = "oversize".into();
    let mut stream = client.stream(&req).await.unwrap();
    assert!(matches!(
        stream.next().await.unwrap(),
        Err(ClaudeError::Protocol(_))
    ));
}

#[tokio::test]
async fn collector_rejects_non_object_tool_input_and_missing_stop_reason() {
    let endpoint = server(|body| {
        let input = if body["model"] == "invalid-input" { "[]" } else { "{}" };
        let stop = if body["model"] == "invalid-input" { "\"tool_use\"" } else { "null" };
        let payload = format!(concat!(
            "data: {{\"type\":\"message_start\",\"message\":{{\"id\":\"x\",\"role\":\"assistant\",\"model\":\"x\",\"content\":[],\"usage\":{{}}}}}}\n\n",
            "data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"toolu\",\"name\":\"lookup\",\"input\":{{}}}}}}\n\n",
            "data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}}}\n\n",
            "data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n",
            "data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":{}}},\"usage\":{{\"output_tokens\":1}}}}\n\n",
            "data: {{\"type\":\"message_stop\"}}\n\n"
        ),input,stop);
        (StatusCode::OK,"text/event-stream",payload)
    }).await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
    for model in ["invalid-input", "missing-stop"] {
        let mut req = request();
        req.model = model.into();
        let mut stream = client.stream(&req).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            collect_stream(first, stream).await,
            Err(ClaudeError::Protocol(_))
        ));
    }
}

#[tokio::test]
async fn collector_does_not_silently_drop_unknown_delta_on_known_block() {
    let endpoint = server(|_| (StatusCode::OK, "text/event-stream", concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"x\",\"role\":\"assistant\",\"model\":\"x\",\"content\":[],\"usage\":{}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"new_text_delta\",\"text\":\"lost\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    ).into())).await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
    let mut stream = client.stream(&request()).await.unwrap();
    let first = stream.next().await.unwrap().unwrap();
    assert!(matches!(
        collect_stream(first, stream).await,
        Err(ClaudeError::Protocol(_))
    ));
}

#[tokio::test]
async fn approved_oauth_header_provider_refreshes_before_each_request() {
    use nanocodex_claude::{ClaudeAuthProvider, ClaudeAuthUnavailable};
    use std::{
        future::Future,
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
    };
    struct SyntheticApprovedProvider {
        calls: AtomicUsize,
    }
    impl ClaudeAuthProvider for SyntheticApprovedProvider {
        fn headers(
            &self,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<reqwest::header::HeaderMap, ClaudeAuthUnavailable>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                let next = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer synthetic-{next}").parse().unwrap(),
                );
                Ok(headers)
            })
        }
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let app = Router::new().route("/v1/messages", post(move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
        let captured = captured.clone();
        async move {
            assert!(body.get("authorization").is_none());
            captured.lock().unwrap().push(headers.get("authorization").unwrap().to_str().unwrap().to_owned());
            Json(json!({"id":"msg","role":"assistant","model":"test","content":[],"usage":{}}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::with_auth_provider(
        http_client(),
        format!("http://{address}/v1/messages"),
        Arc::new(SyntheticApprovedProvider {
            calls: AtomicUsize::new(0),
        }),
    );
    client.create(&request()).await.unwrap();
    client.create(&request()).await.unwrap();
    assert_eq!(
        &*seen.lock().unwrap(),
        &["Bearer synthetic-1", "Bearer synthetic-2"]
    );
}

#[tokio::test]
async fn streaming_web_citation_delta_survives_followup_replay() {
    let endpoint = server(|_| (StatusCode::OK,"text/event-stream",concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg\",\"role\":\"assistant\",\"model\":\"test\",\"content\":[],\"usage\":{}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"source\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"citations_delta\",\"citation\":{\"type\":\"web_search_result_location\",\"url\":\"https://example.org\",\"encrypted_index\":\"opaque-index\",\"cited_text\":\"source\"}}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    ).into())).await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic");
    let mut stream = client.stream(&request()).await.unwrap();
    let first = stream.next().await.unwrap().unwrap();
    let result = collect_stream(first, stream).await.unwrap();
    let replay = serde_json::to_value(Message {
        role: Role::Assistant,
        content: result.content,
    })
    .unwrap();
    assert_eq!(
        replay["content"][0]["citations"][0]["encrypted_index"],
        "opaque-index"
    );
}

#[tokio::test]
async fn streamed_client_tool_preserves_opaque_caller_metadata() {
    let endpoint = server(|_| {
        let mut body = String::new();
        for event in [
            json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"test","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"weather","input":{},"caller":{"type":"code_execution_20260120","tool_id":"srvtoolu_parent"}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"Paris\"}"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}),
            json!({"type":"message_stop"}),
        ] {
            body.push_str(&format!("data: {event}\n\n"));
        }
        (StatusCode::OK, "text/event-stream", body)
    }).await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
    let mut events = client.stream(&request()).await.unwrap();
    let first = events.next().await.unwrap().unwrap();
    let reply = collect_stream(first, events).await.unwrap();
    let replay = serde_json::to_value(&reply.content).unwrap();
    assert_eq!(replay[0]["input"], json!({"city":"Paris"}));
    assert_eq!(replay[0]["caller"]["tool_id"], "srvtoolu_parent");
}

fn complete_text_stream() -> String {
    [
        json!({"type":"message_start","message":{"id":"framing","role":"assistant","model":"x","content":[],"usage":{}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"日本語 😀"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ].iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect()
}

#[tokio::test]
async fn accepts_all_sse_line_endings_and_one_leading_bom() {
    // A CR-only, fully terminated message_stop used to produce IncompleteStream
    // even though the provider sent the complete terminal event.
    for ending in ["\r", "\n", "\r\n"] {
        for bom in ["", "\u{feff}"] {
            let payload = format!("{bom}{}", complete_text_stream().replace('\n', ending));
            let endpoint = server(move |_| {
                (
                    StatusCode::OK,
                    "text/event-stream; charset=utf-8",
                    payload.clone(),
                )
            })
            .await;
            let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
            let mut events = client.stream(&request()).await.unwrap();
            let first = events.next().await.unwrap().unwrap();
            let result = collect_stream(first, events).await.unwrap();
            assert_eq!(result.content, vec![ContentBlock::text("日本語 😀")]);
            assert_eq!(result.stop_reason, Some(StopReason::EndTurn));
        }
    }
}

#[tokio::test]
async fn never_promotes_unterminated_or_missing_terminal_to_success() {
    let complete = complete_text_stream();
    let terminal = complete.rfind("event: message_stop").unwrap();
    for payload in [
        complete[..terminal].to_owned(),
        complete.trim_end_matches('\n').to_owned(),
        complete[..complete.len() - 1].to_owned(),
        format!(
            "{}event: message_stop\ndata: {{\"type\":\"message_st",
            &complete[..terminal]
        ),
    ] {
        let endpoint =
            server(move |_| (StatusCode::OK, "text/event-stream", payload.clone())).await;
        let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
        let mut events = client.stream(&request()).await.unwrap();
        let first = events.next().await.unwrap().unwrap();
        assert!(matches!(
            collect_stream(first, events).await,
            Err(ClaudeError::IncompleteStream)
        ));
    }
}

#[tokio::test]
async fn rejects_successful_non_sse_response_without_exposing_body() {
    let endpoint = server(|_| {
        (
            StatusCode::OK,
            "application/json",
            "private upstream diagnostic".into(),
        )
    })
    .await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
    let error = match client.stream(&request()).await {
        Ok(_) => panic!("non-SSE body must be rejected"),
        Err(error) => error,
    };
    assert!(
        matches!(&error, ClaudeError::Protocol(message) if message == "expected text/event-stream response")
    );
    assert!(!error.to_string().contains("private upstream diagnostic"));
}

#[tokio::test]
async fn event_name_whitespace_is_not_silently_normalized() {
    let payload = complete_text_stream().replace("event: message_stop", "event:  message_stop");
    let endpoint = server(move |_| (StatusCode::OK, "text/event-stream", payload.clone())).await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
    let mut events = client.stream(&request()).await.unwrap();
    let first = events.next().await.unwrap().unwrap();
    assert!(matches!(
        collect_stream(first, events).await,
        Err(ClaudeError::Protocol(_))
    ));
}

#[tokio::test]
async fn observed_terminal_with_open_block_is_protocol_error_not_missing_terminal() {
    let payload = complete_text_stream().replace(
        "event: content_block_stop\ndata: {\"index\":0,\"type\":\"content_block_stop\"}\n\n",
        "",
    );
    assert!(
        !payload.contains("content_block_stop"),
        "fixture must leave the text block open"
    );
    let endpoint = server(move |_| (StatusCode::OK, "text/event-stream", payload.clone())).await;
    let client = ClaudeClient::new(http_client(), endpoint, "synthetic-key");
    let mut events = client.stream(&request()).await.unwrap();
    let first = events.next().await.unwrap().unwrap();
    assert!(matches!(collect_stream(first, events).await,
        Err(ClaudeError::Protocol(message)) if message == "message_stop before content_block_stop"));
}

#[tokio::test]
async fn complete_cr_terminal_settles_without_waiting_for_transport_eof() {
    use axum::body::Body;
    use std::convert::Infallible;
    let app = Router::new().route(
        "/v1/messages",
        post(|| async {
            let payload = complete_text_stream().replace('\n', "\r");
            let chunks = futures_util::stream::once(async move { Ok::<_, Infallible>(payload) })
                .chain(futures_util::stream::pending());
            (
                [("content-type", "text/event-stream")],
                Body::from_stream(chunks),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        http_client(),
        format!("http://{address}/v1/messages"),
        "synthetic-key",
    );
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut events = client.stream(&request()).await.unwrap();
        let first = events.next().await.unwrap().unwrap();
        collect_stream(first, events).await.unwrap()
    })
    .await
    .expect("a fully framed terminal must settle while the connection remains open");
    assert_eq!(result.content, vec![ContentBlock::text("日本語 😀")]);
    server.abort();
}
