//! Public native MCP transport journey over real stdio and Streamable HTTP.
use nanocodex_oai_tools::{
    mcp::{Mcp, McpServer, McpToolExposure},
    runtime::DynamicToolProvider,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncWriteExt, net::TcpListener};

#[tokio::test]
async fn native_mcp_stdio_tools_resources_errors_and_policy() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mcp-stdio-server.mjs");
    let server = McpServer::stdio("node").arg(fixture.to_string_lossy());
    let mcp = Mcp::builder()
        .server("fixture", server.clone())
        .server(
            "hidden",
            server.clone().tool_exposure(McpToolExposure::Hidden),
        )
        .server(
            "codeonly",
            server.tool_exposure(McpToolExposure::CodeModeOnly),
        )
        .build()
        .unwrap();
    let handle = mcp.handle();
    mcp.start();
    let readiness = handle.native_wait(Duration::from_secs(10)).await;
    println!("native stdio readiness: {readiness}");
    assert_eq!(readiness["complete"], true);
    assert_eq!(handle.native_definitions().len(), 1);
    let search = handle.native_search("echo", 5).await.unwrap();
    println!("native ToolSearch: {search}");
    assert_eq!(search["tools"][0]["name"], "mcp__fixture__echo");
    assert_eq!(
        search["tools"][0]["input_schema"]["required"],
        json!(["message"])
    );
    assert!(
        handle
            .native_search("select:mcp__hidden__echo", 5)
            .await
            .is_err()
    );
    assert!(
        handle
            .native_call(
                "mcp__codeonly__echo",
                json!({"message":"blocked"}),
                Value::Null
            )
            .await
            .is_err()
    );
    let result = handle
        .native_call(
            "mcp__fixture__echo",
            json!({"message":"__metadata__"}),
            json!({"session_id":"synthetic-session","call_id":"native-call"}),
        )
        .await
        .unwrap();
    println!("native direct result: {result}");
    assert_eq!(
        result["structuredContent"]["request_meta"]["nanocodex/invocation"]["call_id"],
        "native-call"
    );
    let failure = handle
        .native_call(
            "mcp__fixture__echo",
            json!({"message":"__fail__"}),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(failure["isError"], true);
    let resources = handle.native_list_resources(None).await.unwrap();
    println!("native resources (two protocol pages + templates): {resources}");
    assert_eq!(resources["resources"].as_array().unwrap().len(), 2);
    assert_eq!(resources["resourceTemplates"].as_array().unwrap().len(), 1);
    assert_eq!(resources["resources"][1]["server"], "fixture");
    let content = handle
        .native_read_resource("fixture", "fixture://second")
        .await
        .unwrap();
    println!("native resource read: {content}");
    assert_eq!(content["contents"][0]["text"], "fixture resource body");
    assert!(
        handle
            .native_read_resource("hidden", "fixture://first")
            .await
            .is_err()
    );
    assert!(
        handle
            .native_read_resource("unknown", "file:///etc/passwd")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_mcp_http_resource_only_server_and_failed_read() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let trace = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_trace = trace.clone();
    let tools_enabled = Arc::new(AtomicBool::new(false));
    let server_tools = tools_enabled.clone();
    let changed_schema = Arc::new(AtomicBool::new(false));
    let server_schema = changed_schema.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let trace = server_trace.clone();
            let tools_enabled = server_tools.clone();
            let changed_schema = server_schema.clone();
            tokio::spawn(async move {
                let request = super::support::http::read_request(&mut stream)
                    .await
                    .unwrap();
                if !request.headers.starts_with("POST ") {
                    stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    return;
                }
                let request: Value = serde_json::from_slice(&request.body).unwrap();
                trace.lock().unwrap().push(request.clone());
                let Some(id) = request.get("id") else {
                    stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    return;
                };
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => json!({"protocolVersion":request["params"]["protocolVersion"],
                        "capabilities":if tools_enabled.load(Ordering::SeqCst) { json!({"resources":{},"tools":{}}) } else { json!({"resources":{}}) },"serverInfo":{"name":"synthetic-http","version":"1"}}),
                    "tools/list" => {
                        json!({"tools":[{"name":"lookup","description":"Synthetic native lookup","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":if changed_schema.load(Ordering::SeqCst) { json!(["id","extra"]) } else { json!(["id"]) }}}, {"name":"blocked","description":"Configured denied tool","inputSchema":{"type":"object"}}]})
                    }
                    "resources/list" => {
                        json!({"resources":[{"name":"HTTP resource","uri":"fixture://http"}]})
                    }
                    "resources/templates/list" => json!({"resourceTemplates":[]}),
                    "resources/read" => {
                        let body = json!({"jsonrpc":"2.0","id":id,"error":{"code":-32002,"message":"Synthetic missing resource"}}).to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        stream.write_all(response.as_bytes()).await.unwrap();
                        return;
                    }
                    unexpected => panic!("unexpected MCP request: {unexpected}"),
                };
                let body = json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    let mcp = Mcp::builder()
        .server(
            "http",
            McpServer::http(endpoint).disabled_tools(["blocked"]),
        )
        .build()
        .unwrap();
    mcp.start();
    let handle = mcp.handle();
    assert_eq!(
        handle.native_wait(Duration::from_secs(10)).await["complete"],
        true
    );
    assert!(handle.native_definitions().is_empty());
    let resources = handle.native_list_resources(Some("http")).await.unwrap();
    assert_eq!(resources["resources"][0]["uri"], "fixture://http");
    let failure = handle
        .native_read_resource("http", "fixture://missing")
        .await
        .unwrap_err();
    assert!(failure.contains("Synthetic missing resource"));
    println!(
        "native HTTP resource-only transcript: {}",
        serde_json::to_string_pretty(&*trace.lock().unwrap()).unwrap()
    );
    println!("native HTTP observed resource failure: {failure}");
    assert!(
        !trace
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["method"] == "tools/list")
    );
    tools_enabled.store(true, Ordering::SeqCst);
    assert_eq!(handle.reload("http").await.unwrap(), 1);
    assert_eq!(handle.native_definitions()[0]["name"], "mcp__http__lookup");
    assert_eq!(
        handle
            .native_search("select:mcp__http__lookup", 5)
            .await
            .unwrap()["tools"][0]["input_schema"]["required"],
        json!(["id"])
    );
    assert_eq!(
        handle.native_definitions().len(),
        1,
        "search must preserve configured disabled tools"
    );
    assert!(
        handle
            .native_search("select:mcp__http__blocked", 5)
            .await
            .is_err()
    );
    let admitted = handle.native_definitions()[0].clone();
    changed_schema.store(true, Ordering::SeqCst);
    handle.native_search("lookup", 5).await.unwrap();
    let calls_before = trace.lock().unwrap().len();
    let changed = handle
        .native_call_admitted(
            "mcp__http__lookup",
            json!({"id":"old"}),
            Value::Null,
            &admitted,
        )
        .await
        .unwrap_err();
    assert!(changed.contains("changed since admission"));
    assert_eq!(
        trace.lock().unwrap().len(),
        calls_before,
        "changed schema must fail before remote dispatch"
    );
    println!("native HTTP configured filter and frozen-schema rejection: {changed}");
    tools_enabled.store(false, Ordering::SeqCst);
    assert_eq!(handle.reload("http").await.unwrap(), 0);
    assert!(handle.native_definitions().is_empty());
    assert!(
        handle
            .native_call("mcp__http__lookup", json!({"id":"removed"}), Value::Null)
            .await
            .unwrap_err()
            .contains("unavailable")
    );
    println!(
        "native HTTP reload discovery/removal verified: {}",
        serde_json::to_string_pretty(&*trace.lock().unwrap()).unwrap()
    );
    server.abort();
}
