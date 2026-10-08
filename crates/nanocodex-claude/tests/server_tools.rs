//! The Anthropic-executed tools are not client callbacks and never get user tool_result blocks.
use axum::{Json, Router, response::IntoResponse, routing::post};
use futures_util::StreamExt;
use nanocodex_agent::Nanocodex;
use nanocodex_agent::events::AgentEventKind;
use nanocodex_claude::{Claude, ClaudeClient, ServerToolDefinition};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn stream(blocks: Vec<Value>, stop: &str) -> String {
    let mut data = String::new();
    let mut emit = |event: Value| data.push_str(&format!("data: {event}\n\n"));
    emit(
        json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"test","content":[],"usage":{"input_tokens":8,"output_tokens":0}}}),
    );
    for (index, block) in blocks.into_iter().enumerate() {
        emit(json!({"type":"content_block_start","index":index,"content_block":block}));
        emit(json!({"type":"content_block_stop","index":index}));
    }
    emit(json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}));
    emit(json!({"type":"message_stop"}));
    data
}

#[tokio::test]
async fn server_web_search_results_and_citations_replay_without_client_result() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = requests.clone();
    let app = Router::new().route("/v1/messages",post(move |Json(body): Json<Value>| {
        let received = received.clone();
        async move {
            let index = { let mut reqs = received.lock().unwrap(); reqs.push(body); reqs.len() };
            let blocks = if index == 1 {
                vec![
                    json!({"type":"server_tool_use","id":"srvtoolu_1","name":"web_search","input":{"query":"example"}}),
                    json!({"type":"web_search_tool_result","tool_use_id":"srvtoolu_1","content":[{"type":"web_search_result","url":"https://example.org","title":"Example","encrypted_content":"opaque"}]}),
                    json!({"type":"text","text":"An answer","citations":[{"type":"web_search_result_location","url":"https://example.org","encrypted_index":"opaque-index"}]}),
                ]
            } else { vec![json!({"type":"text","text":"next"})] };
            ([ ("content-type","text/event-stream") ], stream(blocks,"end_turn")).into_response()
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
    let (agent, mut events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .server_tool(ServerToolDefinition::web_search_basic(2))
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("search")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "An answer"
    );
    let citation_event = loop {
        let event = events.next().await.unwrap();
        if event.kind == AgentEventKind::AssistantMessage {
            break event;
        }
    };
    let event: Value = serde_json::from_str(citation_event.payload.get()).unwrap();
    assert_eq!(event["citations"][0]["url"], "https://example.org");
    agent
        .prompt("followup")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let reqs = requests.lock().unwrap();
    assert_eq!(
        reqs[0]["tools"][0],
        json!({"type":"web_search_20250305","name":"web_search","max_uses":2})
    );
    assert_eq!(
        reqs[1]["messages"][1]["content"][1]["content"][0]["encrypted_content"],
        "opaque"
    );
    assert_eq!(
        reqs[1]["messages"][1]["content"][2]["citations"][0]["encrypted_index"],
        "opaque-index"
    );
    assert_eq!(reqs[1]["messages"][2]["content"][0]["text"], "followup");
}

#[tokio::test]
async fn pause_turn_resends_server_tools_and_assistant_blocks_without_user_result() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = requests.clone();
    let app = Router::new().route("/v1/messages",post(move |Json(body): Json<Value>| {
        let received = received.clone();
        async move {
            let index = { let mut reqs = received.lock().unwrap(); reqs.push(body); reqs.len() };
            let (blocks,stop) = if index == 1 {
                (vec![json!({"type":"server_tool_use","id":"srvtoolu_1","name":"web_fetch","input":{"url":"https://example.org"}})],"pause_turn")
            } else { (vec![
                json!({"type":"web_fetch_tool_result","tool_use_id":"srvtoolu_1","content":{"type":"web_fetch_result","url":"https://example.org","content":"page"}}),
                json!({"type":"text","text":"fetched"}),
            ],"end_turn") };
            ([ ("content-type","text/event-stream") ], stream(blocks,stop)).into_response()
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
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .server_tool(ServerToolDefinition::web_fetch_basic(1))
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("fetch")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "fetched"
    );
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1]["tools"], reqs[0]["tools"]);
    assert_eq!(reqs[1]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(reqs[1]["messages"][1]["content"][0]["id"], "srvtoolu_1");
}

#[test]
fn deferred_client_and_native_tool_search_specs_have_claude_shape() {
    use nanocodex_claude::{ClaudeToolSpec, ToolDefinition};
    let deferred = ToolDefinition {
        name: "lookup".into(),
        description: "Lookup a record".into(),
        input_schema: json!({"type":"object","properties":{"key":{"type":"string"}}}),
        strict: Some(true),
        defer_loading: true,
    };
    let specs: Vec<ClaudeToolSpec> = vec![
        ServerToolDefinition::tool_search_bm25().into(),
        deferred.into(),
    ];
    let data = serde_json::to_value(specs).unwrap();
    assert_eq!(
        data[0],
        json!({"type":"tool_search_tool_bm25_20251119","name":"tool_search_tool_bm25"})
    );
    assert_eq!(data[1]["defer_loading"], true);
    assert_eq!(data[1]["input_schema"]["type"], "object");
}

#[tokio::test]
async fn server_tool_search_reference_and_discovered_client_tool_continue_without_server_result() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = received.clone();
    let app = Router::new().route("/v1/messages",post(move |Json(body): Json<Value>| {
        let requests=requests.clone();
        async move {
            let index={let mut r=requests.lock().unwrap();r.push(body);r.len()};
            let (blocks,stop)=if index==1 {
                (vec![
                    json!({"type":"server_tool_use","id":"srvtoolu_search","name":"tool_search_tool_bm25","input":{"query":"find lookup"}}),
                    json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_search","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"lookup"}]}}),
                    json!({"type":"tool_use","id":"toolu_lookup","name":"lookup","input":{"key":"x"}}),
                ],"tool_use")
            } else {(vec![json!({"type":"text","text":"done"})],"end_turn")};
            ([ ("content-type","text/event-stream") ],stream(blocks,stop)).into_response()
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
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .server_tool(ServerToolDefinition::tool_search_bm25())
        .tool(
            nanocodex_claude::ToolDefinition {
                name: "ping".into(),
                description: "Ping".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: false,
            },
            |_| async { Ok("pong".into()) },
        )
        .tool(
            nanocodex_claude::ToolDefinition {
                name: "lookup".into(),
                description: "Find a record".into(),
                input_schema: json!({"type":"object","properties":{"key":{"type":"string"}}}),
                strict: None,
                defer_loading: true,
            },
            |_| async { Ok("value x".into()) },
        )
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("find x")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "done"
    );
    let r = received.lock().unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[1]["tools"], r[0]["tools"]);
    assert_eq!(
        r[1]["messages"][1]["content"][1]["content"]["tool_references"][0]["tool_name"],
        "lookup"
    );
    assert_eq!(r[1]["messages"][2]["content"].as_array().unwrap().len(), 1);
    assert_eq!(
        r[1]["messages"][2]["content"][0]["tool_use_id"],
        "toolu_lookup"
    );
}

#[test]
fn additional_anthropic_server_results_and_mcp_listing_replay_opaque_payload() {
    use nanocodex_claude::ContentBlock;
    let cases = [
        json!({"type":"bash_code_execution_tool_result","tool_use_id":"s1","content":{"type":"bash_code_execution_result","stdout":"a","return_code":0,"content":[{"file_id":"opaque"}]}}),
        json!({"type":"text_editor_code_execution_tool_result","tool_use_id":"s2","content":{"type":"text_editor_code_execution_view_result","content":"hi"}}),
        json!({"type":"mcp_tool_use","id":"m1","name":"echo","server_name":"safe","input":{"text":"x"},"caller":{"type":"direct"}}),
        json!({"type":"mcp_tool_result","tool_use_id":"m1","is_error":false,"content":[{"type":"text","text":"x"}]}),
        json!({"type":"mcp_tool_listing","mcp_server_name":"safe","tools":[{"name":"echo","input_schema":{"type":"object"}}]}),
    ];
    for value in cases {
        let block: ContentBlock = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(block).unwrap(), value);
    }
}

#[tokio::test]
async fn provider_code_container_id_is_reused_on_next_turn_without_local_bash() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = received.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body):Json<Value>| {
        let requests=requests.clone();
        async move {
            let index={let mut log=requests.lock().unwrap();log.push(body);log.len()};
            let mut out=String::new();
            let mut emit=|frame:Value|out.push_str(&format!("data: {frame}\n\n"));
            emit(json!({"type":"message_start","message":{"id":"m","role":"assistant","model":"test","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}));
            emit(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}));
            emit(json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":if index==1 {"one"}else{"two"}}}));
            emit(json!({"type":"content_block_stop","index":0}));
            emit(json!({"type":"message_delta","delta":{"stop_reason":"end_turn","container":{"id":"container-fixture","expires_at":"synthetic"}},"usage":{"output_tokens":1}}));
            emit(json!({"type":"message_stop"}));
            ([ ("content-type","text/event-stream") ],out).into_response()
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
        .server_tool(ServerToolDefinition::code_execution_current())
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
        "one"
    );
    assert_eq!(
        agent
            .prompt("second")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "two"
    );
    let log = received.lock().unwrap();
    assert!(log[0].get("container").is_none());
    assert_eq!(log[0]["tools"][0]["type"], "code_execution_20260521");
    assert_eq!(log[1]["container"], "container-fixture");
    assert!(
        !log[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "Bash")
    );
    server.abort();
}

#[tokio::test]
async fn uncertain_pause_turn_continuation_preserves_opaque_boundary_as_recovery_data() {
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
                if index == 2 {
                    return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "synthetic transport failure".to_string()).into_response();
                }
                let (blocks, reason) = if index == 1 {
                    (vec![json!({"type":"server_tool_use","id":"srvtoolu_paused","name":"web_fetch","input":{"url":"https://example.org"}})], "pause_turn")
                } else {
                    (vec![json!({"type":"text","text":"resumed"})], "end_turn")
                };
                ([ ("content-type", "text/event-stream") ], stream(blocks, reason)).into_response()
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
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .server_tool(ServerToolDefinition::web_fetch_basic(1))
        .build()
        .unwrap();
    assert!(
        agent
            .prompt("fetch once")
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
        "resumed"
    );
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 3);
    assert_eq!(log[1]["messages"][1]["content"][0]["id"], "srvtoolu_paused");
    let messages = log[2]["messages"].as_array().unwrap();
    assert!(messages.iter().all(|message| message["role"] == "user"));
    let evidence = messages
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .filter_map(|block| block["text"].as_str())
        .find(|text| text.contains("srvtoolu_paused"))
        .expect("the original unresolved server call must survive as data");
    assert!(evidence.contains("outcome unknown"));
    assert!(evidence.contains("Do not automatically repeat"));
    assert_eq!(messages.last().unwrap()["content"][0]["text"], "continue");
    server.abort();
}
