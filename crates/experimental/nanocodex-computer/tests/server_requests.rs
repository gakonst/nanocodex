//! Mock MCP wire tests; no desktop or installed provider is required.
#![cfg(unix)]
use nanocodex_computer::{ComputerConfig, ComputerTools};
use nanocodex_oai_api::tools::{Tool, ToolContext, ToolError, ToolInput};
use serde_json::{Value, json};

fn config(mode: &str, params: Value) -> ComputerConfig {
    let method = mode;
    let request = json!({"jsonrpc":"2.0","id":"approval-0","method":method,"params":params});
    // The process echoes the actual initialize request and elicitation response
    // inside structuredContent, keeping assertions on the wire contract.
    let script = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-06-18","capabilities":{{}}}}}}'
IFS= read -r initialized
IFS= read -r list
printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"js","inputSchema":{{"type":"object"}}}}]}}}}'
IFS= read -r call || exit 0
printf '%s\n' '{request}'
IFS= read -r answer || exit 0
printf '{{"jsonrpc":"2.0","id":3,"result":{{"content":[],"structuredContent":{{"initialize":%s,"answer":%s}}}}}}\n' "$initialize" "$answer"
IFS= read -r end
"#
    );
    let mut config = ComputerConfig::mcp("/bin/sh");
    config.args = vec!["-c".into(), script.into()];
    config
}

async fn invoke(tools: &ComputerTools) -> Result<Value, ToolError> {
    let result = tools
        .js()
        .execute(
            ToolInput::Function(serde_json::value::to_raw_value(&json!({})).unwrap()),
            ToolContext::new(
                "fixture-model",
                "fixture-session",
                "fixture-call",
                &[],
                16000,
            ),
        )
        .await?;
    Ok(result.structured_result()["structuredContent"].clone())
}

#[tokio::test]
async fn unsupported_server_requests_do_not_advertise_or_fabricate_consent() {
    for method in [
        "elicitation/create",
        "openai/elicitation/create",
        "unknown/request",
    ] {
        let tools = ComputerTools::connect(config(
            method,
            json!({
                "message":"Provider-owned form", "requestedSchema":{"type":"object"}
            }),
        ))
        .await
        .unwrap();
        let result = invoke(&tools).await.unwrap();
        assert_eq!(result["initialize"]["params"]["capabilities"], json!({}));
        assert_eq!(result["answer"]["id"], "approval-0");
        assert_eq!(result["answer"]["error"]["code"], -32601);
        assert!(result["answer"].get("result").is_none());
    }
}
