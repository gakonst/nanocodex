use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use nanocodex_tools::{
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

async fn assert_schema_recovery(lose_checkpoint: bool) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server =
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            assert_eq!(next_json(&mut socket).await?["generate"], false);
            send_warmup(&mut socket, "resp-warmup").await?;
            let _generation = next_json(&mut socket).await?;
            send_json(
                &mut socket,
                completed_response("resp-search", &[search_call("search-old")]),
            )
            .await?;

            let mut request = next_json(&mut socket).await?;
            assert_eq!(request["input"][0]["type"], "tool_search_output");
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
            }
            let input = request["input"].as_array().unwrap();
            let index = input
                .iter()
                .position(|item| item["type"] == "tool_search_output")
                .unwrap();
            assert_eq!(index > 0, lose_checkpoint);
            assert_eq!(input[index]["tools"][0], lookup_definition(false));
            let mut expected_output = input[index].clone();
            expected_output["tools"] = json!([sibling_definition()]);
            send_json(
                &mut socket,
                json!({
                    "type": "error",
                    "status": 400,
                    "error": {
                        "type": "invalid_request_error",
                        "code": "invalid_function_parameters",
                        "param": format!("input[{index}].tools[0].parameters"),
                        "message": "The required array must include limit."
                    }
                }),
            )
            .await?;

            // The rejected turn must finish without another request on this socket.
            let next = timeout(std::time::Duration::from_secs(5), socket.next()).await?;
            assert!(!matches!(next, Some(Ok(Message::Text(_)))));
            let (stream, _) = listener.accept().await?;
            let mut socket = accept_async(stream).await?;
            let replay = next_json(&mut socket).await?;
            assert!(replay.get("previous_response_id").is_none());
            let history = replay["input"].as_array().unwrap();
            assert!(replay.to_string().contains("find a lookup tool"));
            assert!(replay.to_string().contains("use the corrected catalog"));
            assert!(history.iter().any(|item| {
                item["type"] == "tool_search_call" && item["call_id"] == "search-old"
            }));
            assert_eq!(
                history
                    .iter()
                    .find(|item| item["type"] == "tool_search_output"),
                Some(&expected_output)
            );
            send_json(
                &mut socket,
                completed_response("resp-rediscovery", &[search_call("search-new")]),
            )
            .await?;
            let corrected = next_json(&mut socket).await?;
            assert_eq!(corrected["input"][0]["call_id"], "search-new");
            assert_eq!(
                corrected["input"][0]["tools"],
                json!([lookup_definition(true), sibling_definition()])
            );
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
    let error = agent
        .prompt("find a lookup tool")
        .await?
        .await
        .expect_err("invalid discovery must fail the original turn");
    assert!(error.to_string().contains("invalid_function_parameters"));
    assert!(
        error
            .to_string()
            .contains("The required array must include limit.")
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    agent.shutdown().await?;
    drop(agent);

    let durable = RolloutConfig::new(rollout_home.path()).load_session(TEST_SESSION_ID)?;
    let snapshot = serde_json::to_value(durable.snapshot())?;
    let discovery = snapshot["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "tool_search_output")
        .unwrap();
    assert_eq!(discovery["tools"], json!([sibling_definition()]));

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
