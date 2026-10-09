//! Claude sessions mirror Codex-compatible rollouts: real loopback Messages/SSE,
//! real files under a temporary CODEX_HOME. Only the provider is synthetic.
use axum::{Json, Router, routing::post};
use nanocodex_agent::{ForkRequest, Nanocodex, Origin, rollout::RolloutConfig};
use nanocodex_claude::{
    Claude, ClaudeClient, ClaudeToolReply, ClaudeTools, ToolDefinition, ToolResultContent,
};
use serde_json::{Value, json};

fn stream(block: Value, stop: &str) -> String {
    let (start, delta) = if block["type"] == "text" {
        (
            json!({"type":"text","text":""}),
            json!({"type":"text_delta","text":block["text"]}),
        )
    } else {
        (
            json!({"type":"tool_use","id":block["id"],"name":block["name"],"input":{}}),
            json!({"type":"input_json_delta","partial_json":block["input"].to_string()}),
        )
    };
    [
        json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"claude-sonnet-5-5","content":[],"usage":{"input_tokens":3,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":start}),
        json!({"type":"content_block_delta","index":0,"delta":delta}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":5}}),
        json!({"type":"message_stop"}),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect()
}

#[tokio::test]
async fn root_and_side_conversation_write_resumable_codex_rollouts() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let app = Router::new().route(
        "/v1/messages",
        post(|Json(body): Json<Value>| async move {
            let last = body["messages"].as_array().unwrap().last().unwrap()["content"].to_string();
            let wire = if last.contains("use-the-tool") {
                stream(
                    json!({"type":"tool_use","id":"call-1","name":"Lookup","input":{"key":"k"}}),
                    "tool_use",
                )
            } else if last.contains("lookup-receipt") {
                stream(json!({"type":"text","text":"root-answer"}), "end_turn")
            } else {
                stream(json!({"type":"text","text":"side-answer"}), "end_turn")
            };
            ([("content-type", "text/event-stream")], wire)
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    let lookup: ToolDefinition = serde_json::from_value(json!({"name":"Lookup","description":"Synthetic lookup","input_schema":{"type":"object","properties":{"key":{"type":"string"}},"additionalProperties":false}})).unwrap();
    let (root, _) = Nanocodex::builder(Claude::new(
        ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        ),
        "claude-sonnet-5-5",
    ))
    .workspace(home.path().to_str().unwrap())
    .rollout(RolloutConfig::new(home.path()))
    .tools_factory(move |_| {
        Ok(
            ClaudeTools::new().tool_with_context(lookup.clone(), |_, _| async {
                Ok(ClaudeToolReply::success(ToolResultContent::Text(
                    "lookup-receipt".into(),
                )))
            }),
        )
    })
    .build()
    .unwrap();
    let root_rollout = root
        .persistence()
        .and_then(|persistence| persistence.rollout)
        .expect("a configured Claude root reports its rollout mirror");
    assert_eq!(root_rollout.thread_id(), root.session_id());
    root.prompt("please use-the-tool")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let (side, _) = root
        .fork(ForkRequest::latest().side_conversation())
        .await
        .unwrap();
    assert_eq!(side.session().lineage.origin, Origin::SideConversation);
    side.prompt("a side question")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    root.flush().await.unwrap();
    side.flush().await.unwrap();
    let side_rollout = side
        .persistence()
        .and_then(|p| p.rollout)
        .expect("forks mirror too");

    let config = RolloutConfig::new(home.path());
    let listed = config
        .list_sessions()
        .unwrap()
        .into_iter()
        .map(|session| session.thread_id().to_owned())
        .collect::<Vec<_>>();
    assert!(listed.contains(&root.session_id().to_owned()), "{listed:?}");
    assert!(listed.contains(&side.session_id().to_owned()), "{listed:?}");

    let root_file = std::fs::read_to_string(root_rollout.path()).unwrap();
    let side_file = std::fs::read_to_string(side_rollout.path()).unwrap();
    let rows = |file: &str| {
        file.lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>()
    };
    let root_rows = rows(&root_file);
    let side_rows = rows(&side_file);
    assert_eq!(root_rows[0]["type"], "session_meta");
    assert_eq!(side_rows[0]["type"], "session_meta");
    assert!(
        side_file.contains(root.session_id()),
        "side rollout records its parent/root"
    );
    let items = |rows: &[Value], kind: &str| {
        rows.iter()
            .filter(|row| row["type"] == "response_item" && row["payload"]["type"] == kind)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        items(&root_rows, "function_call")[0]["payload"]["call_id"],
        "call-1"
    );
    assert_eq!(
        items(&root_rows, "function_call_output")[0]["payload"]["output"],
        "lookup-receipt"
    );
    assert!(root_file.contains("please use-the-tool") && root_file.contains("root-answer"));
    // The fork inherits the parent's committed history and adds only its own turn.
    assert!(side_file.contains("root-answer") && side_file.contains("side-answer"));
    assert!(!root_file.contains("side-answer"));

    let loaded = config.load_session(root.session_id());
    let evidence =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/claude-rollout-mirror");
    std::fs::create_dir_all(&evidence).unwrap();
    std::fs::write(evidence.join("root.jsonl"), &root_file).unwrap();
    std::fs::write(evidence.join("side.jsonl"), &side_file).unwrap();
    std::fs::write(
        evidence.join("outcome.json"),
        serde_json::to_vec_pretty(&json!({
            "root": root.session_id(), "side": side.session_id(), "listed": listed,
            "root_load": loaded.as_ref().map(|s| s.transcript().len()).map_err(ToString::to_string),
        }))
        .unwrap(),
    )
    .unwrap();
    let loaded = loaded.expect("Claude rollout loads through the shared loader");
    assert!(!loaded.transcript().is_empty());
    side.shutdown().await.unwrap();

    // Restoring the session continues its existing rollout instead of
    // starting a second file for the same thread.
    let checkpoint = root.checkpoint().await.unwrap();
    root.shutdown().await.unwrap();
    let (restored, _) = Nanocodex::builder(Claude::new(
        ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        ),
        "claude-sonnet-5-5",
    ))
    .workspace(home.path().to_str().unwrap())
    .rollout(RolloutConfig::new(home.path()))
    .resume(checkpoint)
    .unwrap()
    .build()
    .unwrap();
    assert_eq!(restored.session_id(), root_rollout.thread_id());
    let restored_rollout = restored.persistence().and_then(|p| p.rollout).unwrap();
    assert_eq!(
        restored_rollout.path().canonicalize().unwrap(),
        root_rollout.path().canonicalize().unwrap()
    );
    restored
        .prompt("restored follow-up")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    restored.flush().await.unwrap();
    let continued = std::fs::read_to_string(root_rollout.path()).unwrap();
    assert!(continued.starts_with(&root_file) && continued.contains("restored follow-up"));
    assert_eq!(
        config
            .list_sessions()
            .unwrap()
            .iter()
            .filter(|session| session.thread_id() == restored.session_id())
            .count(),
        1
    );
    std::fs::write(evidence.join("root-continued.jsonl"), &continued).unwrap();
    restored.shutdown().await.unwrap();
    server.abort();
}


/// Visible thinking and provider server tools reach the mirror as a reasoning
/// summary and paired function call/output; signatures, redacted thinking and
/// encrypted search content never do.
#[tokio::test]
async fn reasoning_and_server_tools_are_mirrored_without_opaque_payloads() {
    use nanocodex_claude::ServerToolDefinition;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let blocks = vec![
        json!({"type":"thinking","thinking":"weigh the sources","signature":"opaque-signature"}),
        json!({"type":"redacted_thinking","data":"opaque-redacted"}),
        json!({"type":"server_tool_use","id":"srv-1","name":"web_search","input":{"query":"nanocodex"}}),
        json!({"type":"web_search_tool_result","tool_use_id":"srv-1","content":[{"type":"web_search_result","title":"Nanocodex","url":"https://example.com/n","encrypted_content":"opaque-search"}]}),
        json!({"type":"text","text":"searched-answer"}),
    ];
    let mut frames = vec![
        json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"claude-sonnet-5-5","content":[],"usage":{"input_tokens":3,"output_tokens":0}}}),
    ];
    for (index, block) in blocks.into_iter().enumerate() {
        frames.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        frames.push(json!({"type":"content_block_stop","index":index}));
    }
    frames.push(json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}));
    frames.push(json!({"type":"message_stop"}));
    let wire: String = frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect();
    let app = Router::new().route(
        "/v1/messages",
        post(move || {
            let wire = wire.clone();
            async move { ([("content-type", "text/event-stream")], wire) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    let (agent, _) = Nanocodex::builder(Claude::new(
        ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        ),
        "claude-sonnet-5-5",
    ))
    .workspace(home.path().to_str().unwrap())
    .rollout(RolloutConfig::new(home.path()))
    .server_tool(ServerToolDefinition::web_search_basic(3))
    .build()
    .unwrap();
    let result = agent
        .prompt("search for nanocodex")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "searched-answer");
    agent.flush().await.unwrap();
    let path = agent.persistence().and_then(|p| p.rollout).unwrap();
    let file = std::fs::read_to_string(path.path()).unwrap();
    let items = file
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["type"] == "response_item")
        .map(|row| row["payload"].clone())
        .collect::<Vec<_>>();
    let of = |kind: &str| {
        items
            .iter()
            .filter(|item| item["type"] == kind)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(of("reasoning").len(), 1, "{items:?}");
    assert_eq!(of("reasoning")[0]["summary"][0]["text"], "weigh the sources");
    assert_eq!(of("function_call")[0]["call_id"], "srv-1");
    assert_eq!(of("function_call")[0]["name"], "web_search");
    assert!(
        of("function_call")[0]["arguments"]
            .as_str()
            .unwrap()
            .contains("nanocodex")
    );
    assert_eq!(of("function_call_output")[0]["call_id"], "srv-1");
    assert_eq!(
        of("function_call_output")[0]["output"],
        "Nanocodex <https://example.com/n>"
    );
    assert!(!file.contains("opaque"), "opaque provider payloads leaked");
    let loaded = RolloutConfig::new(home.path())
        .load_session(agent.session_id())
        .expect("the shared loader reads reasoning and server-tool items");
    assert!(!loaded.transcript().is_empty());
    let evidence =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/claude-rollout-mirror");
    std::fs::create_dir_all(&evidence).unwrap();
    std::fs::write(evidence.join("server-tools.jsonl"), &file).unwrap();
    agent.shutdown().await.unwrap();
    server.abort();
}

