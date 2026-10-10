//! Claude Code-style client discovery and its independent server-backed web call.
use axum::{Json, Router, response::IntoResponse, routing::post};
use nanocodex_agent::Nanocodex;
use nanocodex_claude::{Claude, ClaudeClient};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn stream(blocks: Vec<Value>, stop: &str) -> String {
    let mut out = String::new();
    let mut emit = |event: Value| out.push_str(&format!("data: {event}\n\n"));
    emit(
        json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"test","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
    );
    for (index, block) in blocks.into_iter().enumerate() {
        if block["type"] == "tool_use" {
            emit(
                json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":block["id"],"name":block["name"],"input":{}}}),
            );
            emit(
                json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":block["input"].to_string()}}),
            );
        } else {
            emit(json!({"type":"content_block_start","index":index,"content_block":block}));
        }
        emit(json!({"type":"content_block_stop","index":index}));
    }
    emit(json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}));
    emit(json!({"type":"message_stop"}));
    out
}

#[tokio::test]
async fn client_tool_search_then_nested_web_search_then_compaction() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let log = log.clone();
        async move {
            let index = { let mut r = log.lock().unwrap(); r.push(body); r.len() };
            let (blocks, stop) = match index {
                1 => (vec![json!({"type":"tool_use","id":"find","name":"ToolSearch","input":{"query":"select:WebSearch","max_results":1}})],"tool_use"),
                2 => (vec![json!({"type":"tool_use","id":"search","name":"WebSearch","input":{"query":"latest example","allowed_domains":["example.org"]}})],"tool_use"),
                3 => (vec![
                    json!({"type":"server_tool_use","id":"srv","name":"web_search","input":{"query":"latest example"}}),
                    json!({"type":"web_search_tool_result","tool_use_id":"srv","content":[{"type":"web_search_result","url":"https://example.org/a","title":"Example source","encrypted_content":"opaque"}]}),
                    json!({"type":"text","text":"An answer.","citations":[{"type":"web_search_result_location","url":"https://example.org/a","encrypted_index":"opaque"}]}),
                ],"end_turn"),
                4 => (vec![json!({"type":"text","text":"Done"})],"end_turn"),
                5 => (vec![json!({"type":"text","text":"Summary of research"})],"end_turn"),
                _ => (vec![json!({"type":"text","text":"Again"})],"end_turn"),
            };
            ([ ("content-type","text/event-stream") ], stream(blocks,stop)).into_response()
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
        .client_tool_search()
        .message_diagnostics()
        .nested_web_search(true)
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("Research example")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "Done"
    );
    agent.compact().await.unwrap();
    assert_eq!(
        agent
            .prompt("Continue")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "Again"
    );
    let r = requests.lock().unwrap();
    assert_eq!(r.len(), 6);
    assert_eq!(r[0]["diagnostics"], json!({"previous_message_id":null}));
    assert_eq!(r[1]["diagnostics"], json!({"previous_message_id":"msg"}));
    assert_eq!(
        r[2].get("diagnostics"),
        None,
        "nested search is an independent request"
    );
    assert_eq!(r[5]["diagnostics"], json!({"previous_message_id":"msg"}));
    // Custom search returns references; definitions remain deferred in a
    // stable catalog so discovery does not invalidate the cached tool prefix.
    assert_eq!(r[0]["tools"].as_array().unwrap().len(), 3);
    assert_eq!(r[0]["tools"], r[1]["tools"]);
    assert_eq!(r[0]["tools"], r[3]["tools"]);
    assert_eq!(r[0]["tools"], r[5]["tools"]);
    assert_eq!(r[0]["tools"][0]["name"], "WebSearch");
    assert_eq!(r[0]["tools"][0]["defer_loading"], true);
    assert!(
        r[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "WebSearch")
    );
    assert_eq!(
        r[1]["messages"][2]["content"][0]["content"],
        json!([{"type":"tool_reference","tool_name":"WebSearch"}])
    );
    assert_eq!(
        r[2]["tools"],
        json!([{"type":"web_search_20250305","name":"web_search","allowed_domains":["example.org"]}])
    );
    assert_eq!(r[2]["tool_choice"], json!({"type":"auto"}));
    assert_eq!(r[2]["messages"].as_array().unwrap().len(), 1);
    assert_eq!(r[2]["messages"][0]["content"][0]["text"], "latest example");
    let result = r[3]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(result["content"][0]["tool_use_id"], "search");
    assert!(
        result["content"][0]["content"]
            .as_str()
            .unwrap()
            .contains("https://example.org/a")
    );
    assert!(
        r[4]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "WebSearch")
    );
    assert!(
        r[5]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "WebSearch")
    );
    assert!(
        r[5]["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Summary of research")
    );
    server.abort();
}

#[tokio::test]
async fn optional_context_profile_and_invalid_web_filters_are_bounded() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let received = Arc::new(Mutex::new(Vec::<(Value, Option<String>)>::new()));
    let log = received.clone();
    let app=Router::new().route("/v1/messages",post(move |headers:axum::http::HeaderMap,Json(body):Json<Value>| {
        let log=log.clone();
        async move {
            let index={let mut r=log.lock().unwrap();r.push((body,headers.get("anthropic-beta").and_then(|v|v.to_str().ok()).map(str::to_owned)));r.len()};
            let (blocks,stop)=match index {
                1=>(vec![json!({"type":"tool_use","id":"search","name":"WebSearch","input":{"query":"demo","allowed_domains":["example.org"],"blocked_domains":["else.org"]}})],"tool_use"),
                _=>(vec![json!({"type":"text","text":"Handled"})],"end_turn")
            };
            ([ ("content-type","text/event-stream") ],stream(blocks,stop)).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{addr}/v1/messages"),
        "synthetic",
    );
    let (agent,_)=Nanocodex::builder(Claude::new(client,"test")).max_tokens(128_000)
        .nested_web_search(false).adaptive_thinking().keep_thinking().cache_one_hour()
        .system_blocks(vec![json!({"type":"text","text":"Authorized system","cache_control":{"type":"ephemeral","ttl":"1h"}})])
        .build().unwrap();
    assert_eq!(
        agent
            .prompt("query")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "Handled"
    );
    let r = received.lock().unwrap();
    assert_eq!(
        r.len(),
        2,
        "invalid filters must not make a nested provider call"
    );
    assert_eq!(r[0].0["thinking"], json!({"type":"adaptive"}));
    assert_eq!(
        r[0].0["context_management"],
        json!({"edits":[{"type":"clear_thinking_20251015","keep":"all"}]})
    );
    assert_eq!(r[0].0["cache_control"]["ttl"], "1h");
    assert_eq!(r[0].0["system"][0]["text"], "Authorized system");
    assert_eq!(r[0].1.as_deref(), Some("context-management-2025-06-27"));
    assert_eq!(r[1].0["messages"][2]["content"][0]["is_error"], true);
    server.abort();
}

#[tokio::test]
async fn failed_nested_search_yields_one_error_result_without_retrying_it() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = seen.clone();
    let app=Router::new().route("/v1/messages",post(move |Json(body):Json<Value>| {
        let captured=captured.clone();
        async move {
            let index={let mut r=captured.lock().unwrap();r.push(body);r.len()};
            match index {
                1=>([("content-type","text/event-stream")],stream(vec![json!({"type":"tool_use","id":"failed","name":"WebSearch","input":{"query":"test failure"}})],"tool_use")).into_response(),
                2=>(axum::http::StatusCode::BAD_GATEWAY,"Synthetic upstream failure".to_owned()).into_response(),
                _=>([("content-type","text/event-stream")],stream(vec![json!({"type":"text","text":"Reported failure"})],"end_turn")).into_response(),
            }
        }
    }));
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
        .nested_web_search(false)
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
        "Reported failure"
    );
    let r = seen.lock().unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[2]["messages"][2]["content"][0]["is_error"], true);
    assert_eq!(
        r[2]["messages"][2]["content"][0]["content"],
        "nested search request failed"
    );
    server.abort();
}

#[cfg(feature = "tools")]
#[tokio::test]
async fn web_fetch_uses_approved_page_then_auxiliary_haiku_not_server_fetch() {
    assert_approved_fetch(
        "What is its title?\nInclude\tthe source.",
        "Fixture title".into(),
    )
    .await;
}

#[cfg(feature = "tools")]
#[tokio::test]
async fn web_fetch_long_answer_is_returned_whole_with_source() {
    assert_approved_fetch(
        "What is its title?",
        format!("Fixture title {}", "💡".repeat(10_000)),
    )
    .await;
}

#[cfg(feature = "tools")]
async fn assert_approved_fetch(prompt: &str, answer: String) {
    use nanocodex_claude_tools::web::{ApprovedPage, ApprovedWebFetchSource, WebFetchRequest};
    struct FixtureSource(Arc<Mutex<Vec<WebFetchRequest>>>);
    impl ApprovedWebFetchSource for FixtureSource {
        async fn fetch_source(&self, request: WebFetchRequest) -> Result<ApprovedPage, String> {
            self.0.lock().unwrap().push(request);
            Ok(ApprovedPage {
                final_url: "https://example.org/final".into(),
                content: "<h1>Fixture title</h1>".into(),
            })
        }
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let question = prompt.to_owned();
    let expected = answer.clone();
    let app=Router::new().route("/v1/messages",post(move |Json(body):Json<Value>| {
        let log=log.clone();
        let question=question.clone();
        let answer=answer.clone();
        async move {
            let index={let mut r=log.lock().unwrap();r.push(body);r.len()};
            let (blocks,stop)=match index {
                1=>(vec![json!({"type":"tool_use","id":"fetch","name":"WebFetch","input":{"url":"https://example.org/start","prompt":question}})],"tool_use"),
                2=>(vec![json!({"type":"text","text":answer})],"end_turn"),
                _=>(vec![json!({"type":"text","text":"Fetched"})],"end_turn"),
            };
            ([ ("content-type","text/event-stream") ],stream(blocks,stop)).into_response()
        }
    }));
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
        .web_fetch_with_source(Arc::new(FixtureSource(calls.clone())), false)
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
        "Fetched"
    );
    let call = calls.lock().unwrap();
    assert_eq!(call.len(), 1);
    assert_eq!(call[0].max_output_bytes, 128 * 1024);
    assert_eq!(call[0].prompt, prompt);
    let r = requests.lock().unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0]["tools"][0]["name"], "WebFetch");
    assert!(
        r[0]["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("fetch")
    );
    assert_eq!(r[1]["model"], "claude-haiku-4-5-20251001");
    assert_eq!(r[1]["thinking"], json!({"type":"disabled"}));
    assert!(r[1].get("tools").is_none());
    assert!(
        r[1]["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(prompt)
    );
    let result = &r[2]["messages"][2]["content"][0];
    assert_ne!(result["is_error"], true);
    let text = result["content"].as_str().unwrap();
    // The auxiliary summary is no longer cut at 32 KiB.
    assert_eq!(
        text,
        format!("{expected}\nSource: https://example.org/final")
    );
    assert!(
        r[1]["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Fixture title")
    );
    assert!(
        r[2]["messages"][2]["content"][0]["content"]
            .as_str()
            .unwrap()
            .contains("https://example.org/final")
    );
    assert!(
        r[2]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["name"] != "web_fetch")
    );
    server.abort();
}

#[tokio::test]
async fn rejected_discovery_options_do_not_activate_a_deferred_tool() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let log = log.clone();
        async move {
            let n = { let mut r = log.lock().unwrap(); r.push(body); r.len() };
            let (blocks, reason) = match n {
                1 => (vec![json!({"type":"tool_use","id":"bad-discovery","name":"ToolSearch","input":{"query":"select:effect","max_results":1,"unsupported":true}})], "tool_use"),
                2 => (vec![json!({"type":"tool_use","id":"unloaded-effect","name":"effect","input":{}})], "tool_use"),
                _ => (vec![json!({"type":"text","text":"done"})], "end_turn"),
            };
            ([("content-type", "text/event-stream")], stream(blocks, reason))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .client_tool_search()
        .tool(
            nanocodex_claude::ToolDefinition {
                name: "effect".into(),
                description: "Counted synthetic effect".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: true,
            },
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok("committed".into()) }
            },
        )
        .build()
        .unwrap();
    assert!(
        agent
            .prompt("discover")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["messages"][2]["content"][0]["is_error"], true);
    server.abort();
}

#[tokio::test]
async fn nested_web_search_long_answer_and_sources_are_returned_whole() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let log = log.clone();
        async move {
            let index = { let mut r = log.lock().unwrap(); r.push(body); r.len() };
            let (blocks, stop) = match index {
                1 => (vec![json!({"type":"tool_use","id":"search","name":"WebSearch","input":{"query":"bounded sources","allowed_domains":["example.org"]}})], "tool_use"),
                2 => (vec![
                    json!({"type":"text","text":format!("Answer {}", "💡".repeat(9_000)),"citations":[{"type":"web_search_result_location","url":"https://example.org/citation","encrypted_index":"opaque"}]}),
                    json!({"type":"server_tool_use","id":"srv","name":"web_search","input":{"query":"bounded sources"}}),
                    json!({"type":"web_search_tool_result","tool_use_id":"srv","content":[
                        {"type":"web_search_result","url":"https://example.org/result","title":"huge".repeat(2_500),"encrypted_content":"opaque"},
                        {"type":"web_search_result","url":"https://example.org/last","title":"Last source","encrypted_content":"opaque"}
                    ]}),
                ], "end_turn"),
                _ => (vec![json!({"type":"text","text":"Done"})], "end_turn"),
            };
            ([("content-type", "text/event-stream")], stream(blocks, stop)).into_response()
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
        .nested_web_search(false)
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
        "Done"
    );
    let r = requests.lock().unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[1]["tools"][0]["allowed_domains"], json!(["example.org"]));
    let result = &r[2]["messages"][2]["content"][0];
    assert_ne!(result["is_error"], true);
    let text = result["content"].as_str().unwrap();
    // Previously the answer was cut at 32 KiB, titles at 256 bytes, and a
    // source list above 8 KiB failed the whole search.
    assert!(text.len() > 32 * 1024);
    assert!(text.starts_with(&format!("Answer {}", "💡".repeat(9_000))));
    assert!(text.contains(&format!(
        "https://example.org/result — {}",
        "huge".repeat(2_500)
    )));
    for url in [
        "https://example.org/citation",
        "https://example.org/result",
        "https://example.org/last",
    ] {
        assert!(
            text.contains(&format!("\nSource: {url}")),
            "lost source {url}"
        );
    }
    server.abort();
}

#[tokio::test]
async fn mixed_discovery_reuses_server_references_until_compaction_then_client_search_recovers() {
    use nanocodex_claude::{ServerToolDefinition, ToolDefinition};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let log = log.clone();
        async move {
            let index = { let mut r = log.lock().unwrap(); r.push(body); r.len() };
            let call = |id| json!({"type":"tool_use","id":id,"name":"lookup","input":{}});
            let (blocks, stop) = match index {
                1 => (vec![
                    json!({"type":"server_tool_use","id":"srv-discovery","name":"tool_search_tool_bm25","input":{"query":"lookup"}}),
                    json!({"type":"tool_search_tool_result","tool_use_id":"srv-discovery","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"lookup"}]}}),
                    call("same-response"),
                ], "tool_use"),
                2 => (vec![call("retained-reference")], "tool_use"),
                4 => (vec![json!({"type":"text","text":"Lookup work completed."})], "end_turn"),
                5 => (vec![call("missing-reference")], "tool_use"),
                6 => (vec![json!({"type":"tool_use","id":"client-discovery","name":"ToolSearch","input":{"query":"select:lookup","max_results":1}})], "tool_use"),
                7 => (vec![call("client-loaded")], "tool_use"),
                _ => (vec![json!({"type":"text","text":"done"})], "end_turn"),
            };
            ([("content-type", "text/event-stream")], stream(blocks, stop))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .client_tool_search()
        .server_tool(ServerToolDefinition::tool_search_bm25())
        .tool(
            ToolDefinition {
                name: "lookup".into(),
                description: "Lookup synthetic records".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: true,
            },
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok("record".into()) }
            },
        )
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("lookup twice")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "done"
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    agent.compact().await.unwrap();
    assert!(
        agent
            .prompt("lookup again")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        2,
        "compaction removed the server reference"
    );
    assert_eq!(
        agent
            .prompt("rediscover")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "done"
    );
    assert_eq!(count.load(Ordering::SeqCst), 3);
    let r = requests.lock().unwrap();
    assert_eq!(r.len(), 8);
    assert_eq!(
        r[1]["messages"][1]["content"][1]["content"]["tool_references"][0]["tool_name"],
        "lookup"
    );
    assert_eq!(
        r[1]["messages"][2]["content"].as_array().unwrap().len(),
        1,
        "server search must not receive a client tool result"
    );
    assert_eq!(
        r[1]["messages"][2]["content"][0]["tool_use_id"],
        "same-response"
    );
    assert!(r.iter().all(|request| request["tools"] == r[0]["tools"]));
    assert!(!r[4]["messages"].to_string().contains("srv-discovery"));
    server.abort();
}

#[tokio::test]
async fn nested_search_preserves_sources_across_pause_and_bounds_the_combined_answer() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let log = log.clone();
        async move {
            let index = { let mut r = log.lock().unwrap(); r.push(body); r.len() };
            let (blocks, stop) = match index {
                1 => (vec![json!({"type":"tool_use","id":"research","name":"WebSearch","input":{"query":"synthetic research"}})], "tool_use"),
                2 => (vec![
                    json!({"type":"server_tool_use","id":"srv-search","name":"web_search","input":{"query":"synthetic research"}}),
                    json!({"type":"web_search_tool_result","tool_use_id":"srv-search","content":[{"type":"web_search_result","url":"https://example.org/paused","title":"Paused source","encrypted_content":"opaque"}]}),
                    json!({"type":"text","text":"Earlier finding. ","citations":[{"type":"web_search_result_location","url":"https://example.org/cited","encrypted_index":"opaque-index"}]}),
                ], "pause_turn"),
                3 => (vec![json!({"type":"text","text":"💡".repeat(12_000),"citations":[{"type":"web_search_result_location","url":"https://example.org/cited","encrypted_index":"opaque-index"}]} )], "end_turn"),
                _ => (vec![json!({"type":"text","text":"done"})], "end_turn"),
            };
            ([("content-type", "text/event-stream")], stream(blocks, stop))
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
        .nested_web_search(false)
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("research")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "done"
    );
    let r = requests.lock().unwrap();
    assert_eq!(r.len(), 4);
    assert_eq!(
        r[2]["messages"][1]["content"][1]["content"][0]["encrypted_content"],
        "opaque"
    );
    let receipt = &r[3]["messages"][2]["content"][0];
    assert_ne!(receipt["is_error"], true);
    let text = receipt["content"].as_str().unwrap();
    assert!(
        text.contains("https://example.org/paused"),
        "paused sources must survive the nested call"
    );
    assert_eq!(text.matches("https://example.org/cited").count(), 1);
    assert!(text.starts_with(&format!("Earlier finding. {}", "💡".repeat(12_000))));
    server.abort();
}

/// The nested search follows provider pause_turn continuations until end_turn,
/// not a fixed number of rounds (previously four), and sends no max_uses cap.
#[tokio::test]
async fn nested_search_continues_past_former_pause_cap_until_end_turn() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    const PAUSES: usize = 6;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let log = log.clone();
        async move {
            let index = { let mut r = log.lock().unwrap(); r.push(body); r.len() };
            let (blocks, stop) = match index {
                1 => (vec![json!({"type":"tool_use","id":"research","name":"WebSearch","input":{"query":"deep research"}})], "tool_use"),
                n if n <= PAUSES + 1 => (vec![
                    json!({"type":"server_tool_use","id":format!("srv-{n}"),"name":"web_search","input":{"query":"deep research"}}),
                    json!({"type":"web_search_tool_result","tool_use_id":format!("srv-{n}"),"content":[{"type":"web_search_result","url":format!("https://example.org/round-{n}"),"title":"Round","encrypted_content":"opaque"}]}),
                ], "pause_turn"),
                n if n == PAUSES + 2 => (vec![json!({"type":"text","text":"Final finding."})], "end_turn"),
                _ => (vec![json!({"type":"text","text":"done"})], "end_turn"),
            };
            ([("content-type", "text/event-stream")], stream(blocks, stop))
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
        .nested_web_search(false)
        .build()
        .unwrap();
    let result = agent
        .prompt("research")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "done");
    let r = requests.lock().unwrap();
    assert_eq!(r.len(), PAUSES + 3);
    assert_eq!(
        r[1]["tools"],
        json!([{"type":"web_search_20250305","name":"web_search"}])
    );
    let receipt = &r[PAUSES + 2]["messages"][2]["content"][0];
    assert_ne!(receipt["is_error"], true, "{receipt}");
    let text = receipt["content"].as_str().unwrap();
    assert!(text.starts_with("Final finding."));
    for n in 2..=PAUSES + 1 {
        assert!(
            text.contains(&format!("https://example.org/round-{n}")),
            "round {n}"
        );
    }
    server.abort();
}

/// A paused nested response with no content cannot make progress: the call
/// fails once instead of replaying the same request forever.
#[tokio::test]
async fn nested_search_empty_pause_fails_as_no_progress() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let log = log.clone();
        async move {
            let index = { let mut r = log.lock().unwrap(); r.push(body); r.len() };
            let (blocks, stop) = match index {
                1 => (vec![json!({"type":"tool_use","id":"research","name":"WebSearch","input":{"query":"stalled research"}})], "tool_use"),
                2 => (vec![], "pause_turn"),
                _ => (vec![json!({"type":"text","text":"done"})], "end_turn"),
            };
            ([("content-type", "text/event-stream")], stream(blocks, stop))
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
        .nested_web_search(false)
        .build()
        .unwrap();
    let result = agent
        .prompt("research")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "done");
    let r = requests.lock().unwrap();
    assert_eq!(r.len(), 3, "the empty pause is not replayed");
    let receipt = &r[2]["messages"][2]["content"][0];
    assert_eq!(receipt["is_error"], true);
    assert!(
        receipt.to_string().contains("paused without progress"),
        "{receipt}"
    );
    server.abort();
}

#[tokio::test]
async fn failed_tool_search_post_hook_does_not_authorize_deferred_effect() {
    use nanocodex_claude::{
        ClaudeHookFuture, ClaudeToolDecision, ClaudeToolHooks, ClaudeToolInvocation,
        ClaudeToolReply, ToolDefinition,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RejectDiscovery;
    impl ClaudeToolHooks for RejectDiscovery {
        fn before<'a>(
            &'a self,
            _name: &'a str,
            _input: &'a Value,
            _invocation: &'a ClaudeToolInvocation,
        ) -> ClaudeHookFuture<'a, Result<ClaudeToolDecision, String>> {
            Box::pin(async { Ok(ClaudeToolDecision::Allow) })
        }
        fn after<'a>(
            &'a self,
            name: &'a str,
            _input: &'a Value,
            _invocation: &'a ClaudeToolInvocation,
            _reply: &'a ClaudeToolReply,
        ) -> ClaudeHookFuture<'a, Result<(), String>> {
            Box::pin(async move {
                if name == "ToolSearch" {
                    Err("discovery rejected after handler".into())
                } else {
                    Ok(())
                }
            })
        }
    }

    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let log = log.clone();
        async move {
            let n = { let mut r = log.lock().unwrap(); r.push(body); r.len() };
            let (blocks, reason) = match n {
                1 => (vec![json!({"type":"tool_use","id":"discovery","name":"ToolSearch","input":{"query":"select:effect"}})], "tool_use"),
                2 => (vec![json!({"type":"tool_use","id":"forbidden-effect","name":"effect","input":{}})], "tool_use"),
                _ => (vec![json!({"type":"text","text":"done"})], "end_turn"),
            };
            ([("content-type", "text/event-stream")], stream(blocks, reason))
        }
    }));
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
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .client_tool_search()
        .tool_hooks(Arc::new(RejectDiscovery))
        .tool(
            ToolDefinition {
                name: "effect".into(),
                description: "Counted external effect".into(),
                input_schema: json!({"type":"object"}),
                strict: None,
                defer_loading: true,
            },
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Ok("committed".into()) }
            },
        )
        .build()
        .unwrap();
    let result = agent
        .prompt("discover then execute")
        .await
        .unwrap()
        .result()
        .await;
    assert!(
        result.is_err(),
        "failed discovery must reject deferred dispatch"
    );
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let receipt = &requests[1]["messages"][2]["content"][0];
    assert_eq!(receipt["is_error"], true);
    assert!(
        receipt["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["type"] == "text")
    );
    assert!(
        receipt
            .to_string()
            .contains("discovery rejected after handler")
    );
    // Retain the real transport inputs and result, not just the passing test name.
    let evidence =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/tool-search-posthook");
    std::fs::create_dir_all(&evidence).unwrap();
    std::fs::write(
        evidence.join("requests.json"),
        serde_json::to_vec_pretty(&*requests).unwrap(),
    )
    .unwrap();
    std::fs::write(
        evidence.join("outcome.json"),
        serde_json::to_vec_pretty(&json!({
            "provider_requests": requests.len(), "effect_count": effects.load(Ordering::SeqCst),
            "failed_discovery_receipt": receipt, "dispatch_error": result.unwrap_err().to_string()
        }))
        .unwrap(),
    )
    .unwrap();
    server.abort();
}
