//! Claude-native MCP tools over the retained host's real HTTP/stdio transport.
use super::*;
use nanocodex::tools::mcp::McpHandle;
use serde::Deserialize;

pub(super) fn install(mut tools: ClaudeTools, handle: McpHandle) -> ClaudeTools {
    let search = handle.clone();
    tools = tools.tool_with_context(definition("ToolSearch",
        "Find available MCP tools by purpose or select:name1,name2. Returns exact schemas; discovered tools are available on the next request.",
        json!({"query":{"type":"string"},"max_results":{"type":"integer","minimum":1,"maximum":32,"default":5}}), &["query"]), move |input, _| {
        let handle = search.clone();
        async move {
            let input: Search = decode(input)?;
            if input.query.trim().is_empty() || input.query.len() > 512 {
                return Err("query must contain 1–512 bytes".into());
            }
            let result = handle.native_search(&input.query, input.max_results).await?;
            let mut blocks = result["tools"].as_array().into_iter().flatten()
                .filter_map(|tool| tool["name"].as_str())
                .map(|name| json!({"type":"tool_reference","tool_name":name})).collect::<Vec<_>>();
            blocks.push(json!({"type":"text","text":serde_json::to_string(&result).map_err(|e|e.to_string())?}));
            let mut reply = ClaudeToolReply::success(ToolResultContent::Blocks(blocks));
            reply.structured_result = Some(result);
            Ok(reply)
        }
    });
    let wait = handle.clone();
    tools = tools.tool_with_context(definition("WaitForMcpServers",
        "Wait for MCP startup, reporting actual ready, pending and failed servers. A timeout does not mean readiness.",
        json!({"timeout_ms":{"type":"integer","minimum":0,"maximum":60000,"default":10000}}), &[]), move |input, _| {
        let handle = wait.clone();
        async move {
            let input: Wait = decode(input)?;
            if input.timeout_ms > 60_000 { return Err("timeout_ms must be 0–60000".into()); }
            json_reply(handle.native_wait(Duration::from_millis(input.timeout_ms)).await)
        }
    });
    let resources = handle.clone();
    tools = tools.tool_with_context(definition("ListMcpResourcesTool",
        "List resources and resource templates from configured MCP servers, including pagination and per-server failures.",
        json!({"server":{"type":"string"}}), &[]), move |input, _| {
        let handle = resources.clone();
        async move {
            let input: List = decode(input)?;
            json_reply(handle.native_list_resources(input.server.as_deref()).await?)
        }
    });
    let read = handle.clone();
    tools = tools.tool_with_context(definition("ReadMcpResourceTool",
        "Read a resource URI through its configured MCP server. The URI is never opened as a local path.",
        json!({"server":{"type":"string"},"uri":{"type":"string"}}), &["server", "uri"]), move |input, _| {
        let handle = read.clone();
        async move {
            let input: Read = decode(input)?;
            let result = handle.native_read_resource(&input.server, &input.uri).await?;
            // Preserve the complete resource response. Embedded image blobs use native
            // image blocks; non-image blobs retain their exact MIME type and encoding.
            let mut blocks = Vec::new();
            for item in result["contents"].as_array().into_iter().flatten() {
                if let (Some(mime), Some(blob)) = (item["mimeType"].as_str(), item["blob"].as_str())
                    && matches!(mime, "image/png" | "image/jpeg" | "image/gif" | "image/webp")
                {
                    blocks.push(json!({"type":"image","source":{"type":"base64","media_type":mime,"data":blob}}));
                    continue;
                }
                blocks.push(json!({"type":"text","text":serde_json::to_string(item).map_err(|e|e.to_string())?}));
            }
            let mut reply = ClaudeToolReply::success(ToolResultContent::Blocks(blocks));
            reply.metadata = result.get("_meta").cloned();
            reply.structured_result = Some(result);
            Ok(reply)
        }
    });
    tools.custom_tool_search().dynamic_tools(move || {
        let mut tools = ClaudeTools::new();
        for schema in handle.native_definitions() {
            // Transport owns canonical name normalization and the exact input schema.
            let Ok(mut definition) = serde_json::from_value::<ToolDefinition>(schema.clone())
            else {
                continue;
            };
            definition.defer_loading = true;
            let name = definition.name.clone();
            let handle = handle.clone();
            tools = tools.tool_with_context(definition, move |input, context| {
                let handle = handle.clone();
                let name = name.clone();
                let admitted = schema.clone();
                async move {
                    let metadata = json!({"model":context.model,"session_id":context.session_id,
                        "turn_id":context.turn_id,"call_id":context.call_id,
                        "instruction_revision":context.instruction_revision});
                    let result = handle
                        .native_call_admitted(&name, input, metadata, &admitted)
                        .await?;
                    match nanocodex::claude_tools::host::mcp_tool_output(result.clone())
                        .and_then(super::output_reply)
                    {
                        Ok(reply) => Ok(reply),
                        Err(error) => {
                            let mut reply =
                                ClaudeToolReply::success(ToolResultContent::Text(error));
                            reply.is_error = true;
                            reply.metadata = result.get("_meta").cloned();
                            reply.structured_result = Some(result);
                            Ok(reply)
                        }
                    }
                }
            });
        }
        tools
    })
}
fn definition(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        strict: None,
        defer_loading: false,
    }
}
fn json_reply(value: Value) -> std::result::Result<ClaudeToolReply, String> {
    let mut reply = ClaudeToolReply::success(ToolResultContent::Text(
        serde_json::to_string(&value).map_err(|e| e.to_string())?,
    ));
    reply.structured_result = Some(value);
    Ok(reply)
}
fn decode<T: serde::de::DeserializeOwned>(input: Value) -> std::result::Result<T, String> {
    serde_json::from_value(input).map_err(|e| format!("invalid MCP input: {e}"))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    query: String,
    #[serde(default = "five")]
    max_results: usize,
}
const fn five() -> usize {
    5
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wait {
    #[serde(default = "ten_seconds")]
    timeout_ms: u64,
}
const fn ten_seconds() -> u64 {
    10_000
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    #[serde(default)]
    server: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    server: String,
    uri: String,
}
