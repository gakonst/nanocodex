use super::*;

#[cfg(unix)]
#[tokio::test]
async fn agent_session_id_overrides_the_caller_shell_environment() -> Result<()> {
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
                    "input": concat!(
                        "const result = await tools.exec_command({",
                        "cmd: \"printf '%s' \\\"$CODEX_THREAD_ID\\\"\", ",
                        "login: false",
                        "}); text(result.output);"
                    )
                })],
            ),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        let output = continuation["input"][0]["output"].to_string();
        assert!(output.contains(TEST_SESSION_ID), "{output}");
        assert!(!output.contains("caller-spoof"), "{output}");
        send_final(&mut socket, "resp-final").await
    });

    let workspace = temporary_workspace("session-shell-environment")?;
    let tools = Tools::builder()
        .process_environment([("CODEX_THREAD_ID", "caller-spoof")])
        .build()?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(endpoint)
        .build()?;
    let (agent, events) = Nanocodex::builder(openai)
        .tools(tools)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .build()?;

    assert_eq!(
        agent
            .prompt("print the session identity")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );

    drop((agent, events));
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
