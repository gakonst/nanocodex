use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use nanocodex_oai_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, contract::async_trait,
};

use super::*;

struct CatalogSearch {
    corrected: Arc<AtomicBool>,
    calls: Arc<AtomicU32>,
}

fn lookup_definition(corrected: bool) -> Value {
    json!({
        "type": "function",
        "name": "lookup",
        "strict": true,
        "parameters": {
            "type": "object",
            "properties": { "limit": { "type": ["integer", "null"] } },
            "required": if corrected { vec!["limit"] } else { vec![] },
            "additionalProperties": false
        }
    })
}

fn sibling_definition() -> Value {
    json!({
        "type": "function",
        "name": "list_keys",
        "parameters": { "type": "object", "properties": {} }
    })
}

#[async_trait]
impl Tool for CatalogSearch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::tool_search(
            "client",
            "Find lookup tools.",
            json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(ToolOutput::json(&json!([
            lookup_definition(self.corrected.load(Ordering::Relaxed)),
            sibling_definition()
        ])))
    }
}

#[tokio::test]
async fn invalid_tool_schema_is_removed_before_durable_resume() -> Result<()> {
    assert_schema_recovery(false).await
}

#[tokio::test]
async fn invalid_tool_schema_uses_the_request_after_checkpoint_loss() -> Result<()> {
    assert_schema_recovery(true).await
}

// Since eda4a21e3 agents expose only exec/wait to the provider. A discovered
// strict schema that the provider would reject (lookup's nullable "limit" is
// not required) must therefore never become a provider declaration: a stray
// native tool_search_call fails closed without running the catalog, Code Mode
// discovery returns schemas only as cell output, and neither checkpoint loss
// nor a durable resume can replay a declared schema.
async fn assert_schema_recovery(lose_checkpoint: bool) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server =
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let warmup = next_json(&mut socket).await?;
            assert_eq!(warmup["generate"], false);
            assert_eq!(declared_tool_names(&warmup), ["exec", "wait"]);
            send_warmup(&mut socket, "resp-warmup").await?;
            let _generation = next_json(&mut socket).await?;
            send_json(
                &mut socket,
                completed_response("resp-search", &[search_call("search-old")]),
            )
            .await?;

            let mut request = next_json(&mut socket).await?;
            assert_eq!(request["input"][0]["type"], "tool_search_output");
            assert_eq!(request["input"][0]["call_id"], "search-old");
            assert_eq!(request["previous_response_id"], "resp-search");
            if lose_checkpoint {
                send_json(
                    &mut socket,
                    json!({
                        "type": "error",
                        "error": { "code": "previous_response_not_found" }
                    }),
                )
                .await?;
                request = next_json(&mut socket).await?;
                assert!(request.get("previous_response_id").is_none());
                assert_eq!(declared_tool_names(&request), ["exec", "wait"]);
                assert!(request.to_string().contains("find a lookup tool"));
            }
            assert_no_declared_schemas(&request);
            let input = request["input"].as_array().unwrap();
            let index = input
                .iter()
                .position(|item| item["type"] == "tool_search_output")
                .unwrap();
            assert_eq!(index > 0, lose_checkpoint);
            send_json(
                &mut socket,
                completed_response("resp-discovery", &[exec_search("search-cell")]),
            )
            .await?;

            let discovered = next_json(&mut socket).await?;
            assert_eq!(discovered["input"][0]["type"], "custom_tool_call_output");
            assert_eq!(discovered["input"][0]["call_id"], "search-cell");
            let output = cell_text(&discovered["input"][0]["output"]);
            assert!(output.starts_with("Script completed\n"), "{output}");
            assert!(output.contains("\"required\":[]"), "{output}");
            assert!(output.contains("list_keys"), "{output}");
            assert_no_declared_schemas(&discovered);
            send_final(&mut socket, "resp-final").await?;

            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let replay = next_json(&mut socket).await?;
            assert!(replay.get("previous_response_id").is_none());
            assert_eq!(declared_tool_names(&replay), ["exec", "wait"]);
            assert!(replay.to_string().contains("find a lookup tool"));
            assert!(replay.to_string().contains("use the corrected catalog"));
            assert!(replay["input"].as_array().unwrap().iter().any(|item| {
                item["type"] == "tool_search_call" && item["call_id"] == "search-old"
            }));
            assert_no_declared_schemas(&replay);
            send_json(
                &mut socket,
                completed_response("resp-rediscovery", &[exec_search("search-new")]),
            )
            .await?;
            let corrected = next_json(&mut socket).await?;
            assert_eq!(corrected["input"][0]["call_id"], "search-new");
            let output = cell_text(&corrected["input"][0]["output"]);
            assert!(output.contains("\"required\":[\"limit\"]"), "{output}");
            assert_no_declared_schemas(&corrected);
            send_final(&mut socket, "resp-final").await
        });

    let workspace = tempfile::tempdir()?;
    let rollout_home = tempfile::tempdir()?;
    let corrected = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicU32::new(0));
    let tools = || {
        Tools::builder()
            .without_defaults()
            .tool(CatalogSearch {
                corrected: Arc::clone(&corrected),
                calls: Arc::clone(&calls),
            })
            .build()
    };
    let openai = || OpenAi::builder("test-key").websocket_url(&endpoint).build();
    let (agent, events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .workspace(workspace.path())
        .session_id(test_session_id())
        .tools(tools()?)
        .rollout(RolloutConfig::new(rollout_home.path()))
        .build()?;
    drop(events);
    assert_eq!(
        agent
            .prompt("find a lookup tool")
            .await?
            .await?
            .final_message(),
        "done"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "only Code Mode discovery may run the catalog"
    );
    agent.shutdown().await?;
    drop(agent);

    let durable = RolloutConfig::new(rollout_home.path()).load_session(TEST_SESSION_ID)?;
    let snapshot = serde_json::to_value(durable.snapshot())?;
    let history = snapshot["history"].as_array().unwrap();
    assert!(
        history
            .iter()
            .any(|item| item["type"] == "tool_search_output" && item["call_id"] == "search-old")
    );
    assert!(
        history.iter().all(|item| declared_schemas(item).is_empty()),
        "{snapshot}"
    );

    corrected.store(true, Ordering::Relaxed);
    let (thread_id, snapshot, rollout) = durable.into_parts();
    let (agent, events) = Nanocodex::builder(openai()?)
        .thinking(Thinking::Low)
        .session_id(thread_id.parse()?)
        .resume(snapshot)
        .tools(tools()?)
        .rollout(rollout)
        .build()?;
    drop(events);
    assert_eq!(
        agent
            .prompt("use the corrected catalog")
            .await?
            .await?
            .final_message(),
        "done"
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    agent.shutdown().await?;
    drop(agent);
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    Ok(())
}

fn search_call(call_id: &str) -> Value {
    json!({
        "type": "tool_search_call",
        "call_id": call_id,
        "execution": "client",
        "arguments": { "query": "lookup" }
    })
}

fn exec_search(call_id: &str) -> Value {
    json!({
        "type": "custom_tool_call",
        "call_id": call_id,
        "name": "exec",
        "input": "text(await tools.tool_search({query: 'lookup'}));"
    })
}

// Schemas a request item would declare to the provider.
fn declared_schemas(item: &Value) -> Vec<&Value> {
    match item["type"].as_str() {
        Some("tool_search_output") => item["tools"].as_array().into_iter().flatten().collect(),
        Some("additional_tools") => item["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|tool| !matches!(tool["name"].as_str(), Some("exec" | "wait")))
            .collect(),
        _ => Vec::new(),
    }
}

fn assert_no_declared_schemas(request: &Value) {
    let input = request["input"].as_array().unwrap();
    assert!(
        input.iter().all(|item| declared_schemas(item).is_empty()),
        "{request}"
    );
}

fn declared_tool_names(request: &Value) -> Vec<&str> {
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "additional_tools")
        .flat_map(|item| item["tools"].as_array().into_iter().flatten())
        .filter_map(|tool| tool["name"].as_str())
        .collect()
}

fn cell_text(output: &Value) -> String {
    output
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect()
}
