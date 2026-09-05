use super::*;

mod environment;
mod panic;
mod parallel;

struct NativeToolSearch;
struct NamespacedEcho;

#[nanocodex_tools::contract::async_trait]
impl nanocodex_tools::Tool for NamespacedEcho {
    fn definition(&self) -> nanocodex_tools::ToolDefinition {
        nanocodex_tools::ToolDefinition::function(
            "test_namespace__echo",
            "Echo one value.",
            json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(
        &self,
        input: nanocodex_tools::ToolInput,
        _context: nanocodex_tools::ToolContext<'_>,
    ) -> nanocodex_tools::ToolResult {
        let arguments: Value = input.decode_json()?;
        Ok(nanocodex_tools::ToolOutput::text(
            arguments["value"].as_str().unwrap_or_default(),
        ))
    }
}

#[tokio::test]
async fn normal_code_mode_executes_direct_function_and_custom_tools() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;

        let warmup = next_json(&mut socket).await?;
        let visible_tools = warmup["input"][0]["tools"]
            .as_array()
            .ok_or_else(|| eyre!("warmup tools were not an array"))?;
        let names = visible_tools
            .iter()
            .filter_map(|definition| definition["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "exec",
                "wait",
                "exec_command",
                "write_stdin",
                "update_plan",
                "apply_patch",
                "view_image",
                "web",
                "image_gen",
                "test_namespace",
            ]
        );
        let exec = visible_tools
            .iter()
            .find(|definition| definition["name"] == "exec")
            .unwrap();
        assert!(
            !exec["description"]
                .as_str()
                .unwrap_or_default()
                .contains("### `update_plan`")
        );
        let update_plan = visible_tools
            .iter()
            .find(|definition| definition["name"] == "update_plan")
            .unwrap();
        assert!(
            update_plan["description"]
                .as_str()
                .unwrap_or_default()
                .contains("exec tool declaration:")
        );
        let exec_command = visible_tools
            .iter()
            .find(|definition| definition["name"] == "exec_command")
            .unwrap();
        assert!(exec_command.get("output_schema").is_none());
        let image_gen = visible_tools
            .iter()
            .find(|definition| definition["name"] == "image_gen")
            .unwrap();
        assert_eq!(image_gen["type"], "namespace");
        assert_eq!(
            image_gen["description"],
            "Tools in the image_gen namespace."
        );
        assert_eq!(image_gen["tools"][0]["name"], "imagegen");
        assert!(
            image_gen["tools"][0]
                .pointer("/parameters/properties/referenced_image_paths/maxItems")
                .is_none()
        );
        assert!(
            image_gen["tools"][0]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("image_gen__imagegen(args:")
        );
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
            completed_response(
                "resp-direct-tools",
                &[
                    json!({
                        "type": "function_call",
                        "call_id": "call-plan",
                        "name": "update_plan",
                        "arguments": "{\"plan\":[{\"step\":\"exercise direct tools\",\"status\":\"completed\"}]}"
                    }),
                    json!({
                        "type": "custom_tool_call",
                        "call_id": "call-patch",
                        "name": "apply_patch",
                        "input": "*** Begin Patch\n*** Add File: direct-tool.txt\n+direct dispatch worked\n*** End Patch"
                    }),
                    json!({
                        "type": "function_call",
                        "call_id": "call-namespaced",
                        "namespace": "test_namespace",
                        "name": "echo",
                        "arguments": "{\"value\":\"namespaced direct dispatch worked\"}"
                    }),
                    json!({
                        "type": "function_call",
                        "call_id": "call-shell",
                        "name": "exec_command",
                        "arguments": "{\"cmd\":\"printf direct-shell-dispatch-worked\",\"login\":false}"
                    }),
                ],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-direct-tools");
        let input = continuation["input"]
            .as_array()
            .ok_or_else(|| eyre!("continuation input was not an array"))?;
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "call-plan");
        assert_eq!(input[1]["type"], "custom_tool_call_output");
        assert_eq!(input[1]["call_id"], "call-patch");
        assert!(
            !input[1]["output"]
                .as_str()
                .unwrap_or_default()
                .contains("unsupported"),
            "{continuation}"
        );
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call-namespaced");
        assert_eq!(input[2]["output"], "namespaced direct dispatch worked");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["call_id"], "call-shell");
        let shell_output = input[3]["output"]
            .as_str()
            .ok_or_else(|| eyre!("direct shell output was not text"))?;
        assert!(shell_output.starts_with("Chunk ID: "), "{shell_output}");
        assert!(shell_output.contains("\nWall time: "), "{shell_output}");
        assert!(
            shell_output.contains("\nProcess exited with code 0\n"),
            "{shell_output}"
        );
        assert!(
            shell_output.ends_with("\nOutput:\ndirect-shell-dispatch-worked"),
            "{shell_output}"
        );
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("normal-code-mode-direct-tools")?;
    let tools = Tools::builder()
        .exposure(nanocodex_tools::ToolExposure::DirectAndCodeMode)
        .tool(NamespacedEcho)
        .build()?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(&endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools)
        .build()?;
    let turn = agent.prompt("Use the direct tools.").await?;
    drop(agent);
    let mut output = Vec::new();
    let (event_result, turn_result) = tokio::join!(events.write_jsonl(&mut output), turn.result());
    event_result?;
    turn_result?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert_eq!(
        std::fs::read_to_string(workspace.join("direct-tool.txt"))?,
        "direct dispatch worked\n"
    );
    let output = String::from_utf8(output)?;
    assert!(output.contains(r#""tool":"update_plan""#));
    assert!(output.contains(r#""tool":"apply_patch""#));
    assert!(output.contains(r#""tool":"test_namespace__echo""#));
    let events = output
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let direct_results = events
        .iter()
        .filter(|event| event["type"] == "tool.result")
        .collect::<Vec<_>>();
    assert_eq!(direct_results.len(), 4);
    assert!(
        direct_results
            .iter()
            .all(|event| event["payload"].get("structured_result").is_some())
    );
    let direct_shell_result = direct_results
        .into_iter()
        .find(|event| event["payload"]["tool"] == "exec_command")
        .ok_or_else(|| eyre!("direct shell result event was not emitted"))?;
    assert_eq!(
        direct_shell_result["payload"]["structured_result"]["exit_code"],
        0
    );
    assert_eq!(
        direct_shell_result["payload"]["structured_result"]["output"],
        "direct-shell-dispatch-worked"
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[nanocodex_tools::contract::async_trait]
impl nanocodex_tools::Tool for NativeToolSearch {
    fn definition(&self) -> nanocodex_tools::ToolDefinition {
        nanocodex_tools::ToolDefinition::tool_search(
            "client",
            "Search caller-configured deferred tools.",
            nanocodex_oai_api::responses::JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for deferred tools."
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of tools to return."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            })),
        )
    }

    async fn execute(
        &self,
        input: nanocodex_tools::ToolInput,
        _context: nanocodex_tools::ToolContext<'_>,
    ) -> nanocodex_tools::ToolResult {
        let arguments: Value = input.decode_json()?;
        assert_eq!(arguments, json!({"query": "calendar create", "limit": 1}));
        Ok(nanocodex_tools::ToolOutput::json(&json!([{
            "type": "function",
            "name": "calendar_create_event",
            "description": "Create a calendar event.",
            "defer_loading": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "title": {"type": "string"}
                },
                "required": ["title"],
                "additionalProperties": false
            }
        }])))
    }
}

#[tokio::test]
async fn configured_native_tool_search_round_trips_its_dedicated_items() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;

        let warmup = next_json(&mut socket).await?;
        let definitions = warmup["input"][0]["tools"]
            .as_array()
            .ok_or_else(|| eyre!("warmup tools were not an array"))?;
        let definition = definitions
            .iter()
            .find(|definition| definition["type"] == "tool_search")
            .ok_or_else(|| eyre!("native tool_search definition was missing"))?;
        assert_eq!(
            definition,
            &json!({
                "type": "tool_search",
                "execution": "client",
                "description": "Search caller-configured deferred tools.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query for deferred tools."
                        },
                        "limit": {
                            "type": "number",
                            "description": "Maximum number of tools to return."
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            })
        );
        let exec = definitions
            .iter()
            .find(|definition| definition["name"] == "exec")
            .ok_or_else(|| eyre!("Code Mode exec definition was missing"))?;
        assert!(
            !exec["description"]
                .as_str()
                .unwrap_or_default()
                .contains("Search caller-configured deferred tools."),
            "native tool_search must not also leak into Code Mode's nested surface"
        );
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
            completed_response(
                "resp-search",
                &[json!({
                    "type": "tool_search_call",
                    "call_id": "search-1",
                    "execution": "client",
                    "arguments": {
                        "query": "calendar create",
                        "limit": 1
                    }
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-search");
        let mut input = continuation["input"].clone();
        remove_client_item_id(&mut input[0], "tso");
        assert_eq!(
            input,
            json!([{
                "type": "tool_search_output",
                "call_id": "search-1",
                "status": "completed",
                "execution": "client",
                "tools": [{
                    "type": "function",
                    "name": "calendar_create_event",
                    "description": "Create a calendar event.",
                    "defer_loading": true,
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"}
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    }
                }]
            }])
        );
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("native-tool-search")?;
    let tools = Tools::builder()
        .without_defaults()
        .tool(NativeToolSearch)
        .build()?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(&endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools)
        .build()?;
    let turn = agent.prompt("Find the calendar creation tool.").await?;
    drop(agent);
    let mut output = Vec::new();
    let (event_result, turn_result) = tokio::join!(events.write_jsonl(&mut output), turn.result());
    event_result?;
    turn_result?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    let output = String::from_utf8(output)?;
    assert!(output.contains(r#""tool":"tool_search""#), "{output}");
    assert!(output.contains("\"run.completed\""), "{output}");
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn mcp_tool_search_exposes_and_dispatches_a_native_namespace() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;

        let warmup = next_json(&mut socket).await?;
        let definitions = warmup["input"][0]["tools"]
            .as_array()
            .ok_or_else(|| eyre!("warmup tools were not an array"))?;
        let search = definitions
            .iter()
            .find(|definition| definition["type"] == "tool_search")
            .ok_or_else(|| eyre!("MCP tool_search definition was missing"))?;
        assert_eq!(search["execution"], "client");
        assert!(
            search["description"]
                .as_str()
                .is_some_and(|description| description.contains("- fixture"))
        );
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
            completed_response(
                "resp-search",
                &[json!({
                    "type": "tool_search_call",
                    "call_id": "search-mcp",
                    "execution": "client",
                    "arguments": {
                        "query": "echo deterministic message",
                        "limit": 1
                    }
                })],
            ),
        )
        .await?;

        let searched = next_json(&mut socket).await?;
        let mut input = searched["input"].clone();
        remove_client_item_id(&mut input[0], "tso");
        assert_eq!(
            input,
            json!([{
                "type": "tool_search_output",
                "call_id": "search-mcp",
                "status": "completed",
                "execution": "client",
                "tools": [{
                    "type": "namespace",
                    "name": "mcp__fixture__",
                    "description": "Tools in the mcp__fixture__ namespace.",
                    "tools": [{
                        "type": "function",
                        "name": "echo",
                        "description": "Echo deterministic MCP fixture message 0.",
                        "strict": false,
                        "defer_loading": true,
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "message": { "type": "string" },
                                "delay_ms": {
                                    "type": "integer",
                                    "minimum": 0,
                                    "maximum": 1000
                                }
                            },
                            "required": ["message"],
                            "additionalProperties": false
                        }
                    }]
                }]
            }])
        );
        send_json(
            &mut socket,
            completed_response(
                "resp-tool",
                &[json!({
                    "type": "function_call",
                    "call_id": "call-mcp",
                    "namespace": "mcp__fixture__",
                    "name": "echo",
                    "arguments": "{\"message\":\"hello\"}"
                })],
            ),
        )
        .await?;

        let called = next_json(&mut socket).await?;
        assert_eq!(called["input"][0]["type"], "function_call_output");
        assert_eq!(called["input"][0]["call_id"], "call-mcp");
        assert!(
            called["input"][0]["output"]
                .as_str()
                .is_some_and(|output| output.contains("fixture:hello")),
            "{called}"
        );
        send_final(&mut socket, "resp-final").await
    });

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../nanocodex-tools/tests/fixtures/mcp-stdio-server.mjs");
    let mcp = nanocodex_tools::mcp::Mcp::builder()
        .server(
            "fixture",
            nanocodex_tools::mcp::McpServer::stdio("node").arg(fixture.to_string_lossy()),
        )
        .build()?;
    let workspace = temporary_workspace("mcp-native-tool-search")?;
    let tools = Tools::builder()
        .exposure(nanocodex_tools::ToolExposure::DirectAndCodeMode)
        .provider(mcp)
        .build()?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(&endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools)
        .build()?;
    let turn = agent.prompt("Find and call the MCP echo tool.").await?;
    drop(agent);
    let mut output = Vec::new();
    let (event_result, turn_result) = tokio::join!(events.write_jsonl(&mut output), turn.result());
    event_result?;
    turn_result?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    let output = String::from_utf8(output)?;
    assert!(output.contains(r#""tool":"tool_search""#), "{output}");
    assert!(
        output.contains(r#""tool":"mcp__fixture__echo""#),
        "{output}"
    );
    assert!(output.contains("\"run.completed\""), "{output}");
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn connection_local_response_code_mode_round_trip() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        assert_warmup(&warmup);
        send_json(
            &mut socket,
            json!({
                "type": "response.metadata",
                "headers": { "x-codex-turn-state": "sticky-test" }
            }),
        )
        .await?;
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        assert_eq!(generation["store"], false);
        assert!(generation.get("generate").is_none());
        assert_eq!(generation["input"].as_array().map(Vec::len), Some(3));
        assert_eq!(generation["input"][0]["role"], "developer");
        assert_eq!(generation["input"][1]["role"], "user");
        assert_eq!(generation["input"][2]["role"], "user");
        assert_eq!(
            generation["client_metadata"]["x-codex-turn-state"],
            "sticky-test"
        );
        send_json(
            &mut socket,
            completed_response(
                "resp-tool",
                &[json!({
                    "id": "item-exec",
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "const result = await tools.exec_command({cmd: \"printf hello\"}); text(result.output);"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-tool");
        assert_eq!(continuation["input"].as_array().map(Vec::len), Some(1));
        assert_eq!(continuation["input"][0]["type"], "custom_tool_call_output");
        assert_eq!(continuation["input"][0]["call_id"], "call-exec");
        assert!(continuation["input"][0].get("success").is_none());
        assert!(
            continuation["input"][0]["output"]
                .as_array()
                .is_some_and(|content| content.iter().any(|item| {
                    item["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("hello"))
                }))
        );
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("code-mode")?;
    let output = run_model(&endpoint, &workspace, "run a shell command").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"tool\":\"exec\""));
    let shell_result = output
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|event| event["type"] == "tool.result" && event["payload"]["tool"] == "exec_command")
        .ok_or_else(|| eyre!("nested shell result event was not emitted"))?;
    assert!(
        shell_result["payload"]["result"]
            .as_str()
            .is_some_and(|result| result.contains("Process exited with code 0"))
    );
    assert_eq!(shell_result["payload"]["structured_result"]["exit_code"], 0);
    assert_eq!(
        shell_result["payload"]["structured_result"]["output"],
        "hello"
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn unsupported_direct_tools_return_failed_results_to_the_model() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
            completed_response(
                "resp-unsupported",
                &[
                    json!({
                        "type": "custom_tool_call",
                        "call_id": "call-custom",
                        "name": "missing_custom",
                        "input": "raw input"
                    }),
                    json!({
                        "type": "function_call",
                        "call_id": "call-function",
                        "namespace": "example::",
                        "name": "missing_function",
                        "arguments": "not json"
                    }),
                ],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-unsupported");
        let input = continuation["input"]
            .as_array()
            .ok_or_else(|| eyre!("continuation input was not an array"))?;
        assert_eq!(input[0]["type"], "custom_tool_call_output");
        assert_eq!(input[0]["call_id"], "call-custom");
        assert_eq!(
            input[0]["output"],
            "unsupported custom tool call: missing_custom"
        );
        assert_client_item_id(&input[0], "ctco");
        assert_eq!(input[1]["type"], "function_call_output");
        assert_eq!(input[1]["call_id"], "call-function");
        assert_eq!(
            input[1]["output"],
            "unsupported call: example::__missing_function"
        );
        assert_client_item_id(&input[1], "fco");
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("unsupported-tools")?;
    let output = run_model(&endpoint, &workspace, "recover from unsupported tools").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert_eq!(
        output.matches(r#""status":"failed""#).count(),
        2,
        "{output}"
    );
    assert!(output.contains("\"tool_calls\":2"));
    assert!(output.contains("\"run.completed\""));
    assert!(!output.contains("\"run.failed\""));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn code_mode_notify_adds_a_named_exec_output_to_the_next_request() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
            completed_response(
                "resp-notify",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "notify({phase: \"working\"}); text(\"done\");"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], "resp-notify");
        let input = continuation["input"]
            .as_array()
            .ok_or_else(|| eyre!("continuation input was not an array"))?;
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "custom_tool_call_output");
        assert_eq!(input[0]["call_id"], "call-exec");
        assert!(input[0].get("name").is_none());
        assert!(input[0].to_string().contains("done"));
        assert_eq!(input[1]["type"], "custom_tool_call_output");
        assert_eq!(input[1]["call_id"], "call-exec");
        assert_eq!(input[1]["name"], "exec");
        assert_eq!(input[1]["output"], r#"{"phase":"working"}"#);
        assert!(input[1].get("success").is_none());
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("code-mode-notify")?;
    run_model(&endpoint, &workspace, "send a progress notification").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn prepares_images_and_stops_on_invalid_image_requests() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
            completed_response(
                "resp-image",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-image",
                    "name": "exec",
                    "input": "image(\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=\", \"original\");"
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        let output = continuation["input"][0]["output"]
            .as_array()
            .ok_or_else(|| eyre!("image tool output was not content"))?;
        let image = output
            .iter()
            .find(|item| item["type"] == "input_image")
            .ok_or_else(|| eyre!("prepared image was missing"))?;
        assert!(
            image["image_url"]
                .as_str()
                .is_some_and(|url| url.starts_with("data:image/png;base64,"))
        );
        assert!(image.get("detail").is_none());

        send_json(
            &mut socket,
            json!({
                "type": "response.failed",
                "response": {
                    "id": "resp-invalid-image",
                    "status": "failed",
                    "error": {
                        "code": "invalid_image",
                        "message": "The image data you provided does not represent a valid image"
                    }
                }
            }),
        )
        .await?;

        Ok::<(), eyre::Report>(())
    });

    let workspace = temporary_workspace("images")?;
    let error = run_model(&endpoint, &workspace, "inspect images")
        .await
        .expect_err("invalid tool image should fail the turn");
    let error = error
        .downcast_ref::<NanocodexError>()
        .ok_or_else(|| eyre!("invalid image returned the wrong error type"))?;
    assert!(matches!(
        error.responses_error(),
        Some(ResponsesError::InvalidImageRequest { .. })
    ));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn yielded_exec_cell_continues_through_direct_wait_tool() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        assert_warmup(&next_json(&mut socket).await?);
        send_warmup(&mut socket, "resp-warmup").await?;

        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], "resp-warmup");
        send_json(
            &mut socket,
            completed_response(
                "resp-exec",
                &[json!({
                    "type": "custom_tool_call",
                    "call_id": "call-exec",
                    "name": "exec",
                    "input": "text(\"before\"); await yield_control(); const result = await tools.exec_command({cmd: \"printf after\", login: false}); text(result.output);"
                })],
            ),
        )
        .await?;

        let yielded = next_json(&mut socket).await?;
        assert_eq!(yielded["previous_response_id"], "resp-exec");
        assert_eq!(yielded["input"][0]["type"], "custom_tool_call_output");
        assert!(
            yielded
                .to_string()
                .contains("Script running with cell ID 1")
        );
        send_json(
            &mut socket,
            completed_response(
                "resp-wait",
                &[json!({
                    "type": "function_call",
                    "call_id": "call-wait",
                    "name": "wait",
                    "arguments": "{\"cell_id\":\"1\",\"yield_time_ms\":30000}"
                })],
            ),
        )
        .await?;

        let completed = next_json(&mut socket).await?;
        assert_eq!(completed["previous_response_id"], "resp-wait");
        assert_eq!(completed["input"][0]["type"], "function_call_output");
        assert_eq!(completed["input"][0]["call_id"], "call-wait");
        assert!(completed.to_string().contains("after"));
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("code-mode-wait")?;
    let output = run_model(&endpoint, &workspace, "yield and wait").await?;
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    assert!(output.contains("\"tool\":\"wait\""));
    let nested_call = output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| {
            event["type"] == "tool.call" && event["payload"]["call_id"] == "call-exec/code-1"
        })
        .ok_or_else(|| eyre!("nested call did not retain its original exec lineage"))?;
    assert_eq!(nested_call["payload"]["model_call_index"], 1);
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
