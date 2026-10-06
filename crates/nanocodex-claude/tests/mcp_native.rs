#![cfg(all(feature = "tools", not(target_family = "wasm")))]
//! Caller-owned native MCP seam over loopback JSON-RPC, not an OpenAI bridge.
//! No automatic dynamic-catalog wiring into ClaudeBuilder is claimed.
use axum::{Json, Router, routing::post};
use nanocodex_claude_tools::{
    ClaudeMcp, ClaudeMcpProvider, HostContext, ImageSource, McpToolDefinition, ToolOutput,
    ToolResultBlock,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct HttpMcp {
    client: reqwest::Client,
    endpoint: String,
    catalog: Mutex<Vec<McpToolDefinition>>,
}
impl HttpMcp {
    async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let response: Value = self
            .client
            .post(&self.endpoint)
            .json(
                &json!({"jsonrpc":"2.0","id":"synthetic-request","method":method,"params":params}),
            )
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?
            .json()
            .await
            .map_err(|error| error.to_string())?;
        if response.get("error").is_some() {
            return Err(response["error"]["message"].as_str().unwrap().to_owned());
        }
        Ok(response["result"].clone())
    }
    async fn refresh(&self) {
        let result = self.rpc("tools/list", json!({})).await.unwrap();
        *self.catalog.lock().unwrap() = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| {
                McpToolDefinition::new(
                    tool["name"].as_str().unwrap(),
                    tool["description"].as_str().unwrap(),
                    tool["inputSchema"].clone(),
                )
            })
            .collect();
    }
}
impl ClaudeMcpProvider for HttpMcp {
    fn start(&self) {} // Caller explicitly refreshes at request boundaries.
    fn available_definitions(&self) -> Vec<McpToolDefinition> {
        self.catalog.lock().unwrap().clone()
    }
    async fn execute(
        &self,
        name: &str,
        input: Value,
        context: HostContext<'_>,
    ) -> Result<Option<ToolOutput>, String> {
        // Context is carried to the authorized host; it is not a model grant.
        let result = self
            .rpc(
                "tools/call",
                json!({"name":name,"arguments":input,"_meta":{
                    "model":context.model(),"session":context.session_id(),"turn":context.turn_id(),
                    "call":context.call_id(),"output_budget":context.max_output_tokens()
                }}),
            )
            .await?;
        if result["removed"] == true {
            return Ok(None);
        }
        let blocks = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| match item["type"].as_str().unwrap() {
                "text" => ToolResultBlock::Text {
                    text: item["text"].as_str().unwrap().to_owned(),
                },
                "image" => ToolResultBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: item["mimeType"].as_str().unwrap().to_owned(),
                        data: item["data"].as_str().unwrap().to_owned(),
                    },
                },
                _ => ToolResultBlock::UnsupportedMedia {
                    media_type: item["mimeType"].as_str().unwrap().to_owned(),
                },
            })
            .collect();
        let mut output = ToolOutput::content(blocks)
            .with_structured_result(result["structuredContent"].clone())
            .with_metadata(result["_meta"].clone());
        output.is_error = result["isError"] == true;
        Ok(Some(output))
    }
}

#[tokio::test]
async fn caller_native_mcp_preserves_transport_results_and_live_catalog_failures() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let schema = json!({"type":"object","properties":{"query":{"oneOf":[{"type":"string"},{"type":"array","items":{"type":"string"}}]}},"required":["query"],"additionalProperties":false});
    let catalog = Arc::new(Mutex::new(
        json!([{"name":"mcp__docs__search","description":"Search docs","inputSchema":schema}]),
    ));
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let remote_catalog = catalog.clone();
    let log = requests.clone();
    let app = Router::new().route("/mcp", post(move |Json(request): Json<Value>| {
        let catalog = remote_catalog.clone();
        let log = log.clone();
        async move {
            log.lock().unwrap().push(request.clone());
            let result = match request["method"].as_str().unwrap() {
                "tools/list" => json!({"tools":*catalog.lock().unwrap()}),
                "tools/call" => match request["params"]["arguments"]["mode"].as_str() {
                    Some("deny") => return Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32001,"message":"host authorization denied"}})),
                    Some("removed") => json!({"removed":true}),
                    _ => json!({"content":[{"type":"text","text":"remote failure"},{"type":"image","mimeType":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII="},{"type":"audio","mimeType":"audio/wav","data":"UklGRg=="}],"isError":true,"structuredContent":request["params"]["arguments"],"_meta":{"receipt":"actual-remote-receipt"}}),
                },
                _ => panic!("unexpected RPC method"),
            };
            Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let provider = Arc::new(HttpMcp {
        client: reqwest::Client::new(),
        endpoint: format!("http://{address}/mcp"),
        catalog: Mutex::new(vec![]),
    });
    let adapter = ClaudeMcp::new(provider.clone());
    adapter.start();
    provider.refresh().await;
    assert_eq!(adapter.definitions().unwrap()[0]["input_schema"], schema);
    let context =
        HostContext::new("claude", "session-a", "mcp-call", 4096).with_turn_id(Some("turn-a"));
    let input = json!({"query":["one","two"]});
    let output = adapter
        .execute("mcp__docs__search", input.clone(), context)
        .await
        .unwrap();
    assert!(output.is_error);
    assert_eq!(output.structured_result, Some(input.clone()));
    assert_eq!(
        output.metadata,
        Some(json!({"receipt":"actual-remote-receipt"}))
    );
    assert_eq!(output.content, nanocodex_claude_tools::ToolContent::Blocks(vec![
        ToolResultBlock::Text { text: "remote failure".into() },
        ToolResultBlock::Image { source: ImageSource::Base64 { media_type: "image/png".into(), data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=".into() } },
        ToolResultBlock::UnsupportedMedia { media_type: "audio/wav".into() },
    ]));
    let count = requests.lock().unwrap().len();
    assert!(
        adapter
            .execute("mcp__docs__search", json!([]), context)
            .await
            .is_err()
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        count,
        "invalid input must never reach remote"
    );
    assert_eq!(
        adapter
            .execute("mcp__docs__search", json!({"mode":"deny"}), context)
            .await
            .unwrap_err(),
        "host authorization denied"
    );
    assert!(
        adapter
            .execute("mcp__docs__search", json!({"mode":"removed"}), context)
            .await
            .unwrap_err()
            .contains("became unavailable")
    );
    catalog.lock().unwrap()[0]["inputSchema"] = json!({"type":"object","required":["changed"]});
    provider.refresh().await;
    assert_eq!(
        adapter.definitions().unwrap()[0]["input_schema"]["required"],
        json!(["changed"])
    );
    *catalog.lock().unwrap() = json!([]);
    provider.refresh().await;
    assert!(adapter.definitions().unwrap().is_empty());
    let count = requests.lock().unwrap().len();
    assert!(
        adapter
            .execute("mcp__docs__search", input, context)
            .await
            .unwrap_err()
            .contains("no longer available")
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        count,
        "removed tool must fail before remote dispatch"
    );
    for malformed in [
        json!([{"name":"exec_command","description":"not MCP","inputSchema":{}}]),
        json!([{"name":"mcp__docs__search","description":"bad schema","inputSchema":true}]),
        json!([{"name":"mcp__docs__search","description":"one","inputSchema":{}},{"name":"mcp__docs__search","description":"two","inputSchema":{}}]),
    ] {
        *catalog.lock().unwrap() = malformed;
        provider.refresh().await;
        assert!(adapter.definitions().is_err());
    }
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests[1]["params"]["_meta"],
        json!({"model":"claude","session":"session-a","turn":"turn-a","call":"mcp-call","output_budget":4096})
    );
    let artifact = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/provider-managed-20261001/crates/mcp-native");
    std::fs::create_dir_all(&artifact).unwrap();
    std::fs::write(
        artifact.join("requests.json"),
        serde_json::to_vec_pretty(&*requests).unwrap(),
    )
    .unwrap();
    std::fs::write(
        artifact.join("native-output.json"),
        serde_json::to_vec_pretty(&output).unwrap(),
    )
    .unwrap();
    std::fs::write(artifact.join("scenario.txt"), "Command: cargo test --locked -p nanocodex-claude --no-default-features --features tools --test mcp_native\nCaller implements native MCP provider over actual loopback JSON-RPC HTTP. Expected/observed: exact live schema, intact input/context, native ordered text/image/unsupported-media, true is_error, structured result+metadata, remote denial/racing removal, refreshed schema/removal, malformed catalogs fail closed. This is the caller adapter seam, not full MCP transport/product wiring.\n").unwrap();
    server.abort();
}
