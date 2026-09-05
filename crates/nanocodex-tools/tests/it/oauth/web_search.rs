use eyre::{Result, eyre};
use nanocodex_oai_api::responses::ResponseItem;
use nanocodex_tools::{
    ToolContext, ToolInput, Tools,
    contract::{DEFAULT_TOOL_OUTPUT_TOKENS, ToolOutputBody},
    runtime::{ToolRuntime, WebSearchConfig},
};
use serde_json::{Value, json, value::to_raw_value};
use tokio::{net::TcpListener, task::JoinHandle};

use crate::support::{
    auth::RotatingChatGptAuth,
    http::{read_request, write_json, write_unauthorized},
};

#[tokio::test]
async fn chatgpt_auth_recovers_for_web_search() -> Result<()> {
    let _runtime_test = crate::TOOL_RUNTIME_TEST_LOCK.lock().await;
    let (endpoint, server) = spawn_search_server().await?;
    let (auth, auth_source) = RotatingChatGptAuth::shared();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-injected-client",
        reqwest::header::HeaderValue::from_static("true"),
    );
    nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()?;
    let tools = Tools::builder()
        .without_defaults()
        .web_search(true)
        .remote_http_client(client)
        .build()?;
    let runtime =
        ToolRuntime::new_with_tools(".", Some(WebSearchConfig { endpoint, auth }), None, &tools);
    let history = serde_json::from_value::<Vec<ResponseItem>>(json!([
        {
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "<environment_context>ignored</environment_context>"
            }]
        },
        {
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Search the web"}]
        },
    ]))?;

    let output = runtime
        .execute_tool(
            "web__run",
            ToolInput::Function(to_raw_value(&json!({
                "search_query": [{"q": "standalone web search"}],
            }))?),
            ToolContext::new(
                "gpt-5.6-sol",
                "search-session",
                "call-search",
                &history,
                DEFAULT_TOOL_OUTPUT_TOKENS,
            ),
        )
        .await;

    assert!(output.success);
    assert!(matches!(
        output.output,
        ToolOutputBody::Text(ref text) if text == "Search result with turn0search0"
    ));
    assert_eq!(
        output
            .metadata
            .as_deref()
            .map(|raw| serde_json::from_str::<Value>(raw.get()).unwrap()),
        Some(json!({
            "results": [{
                "type": "text_result",
                "ref_id": "turn0search0",
                "url": "https://example.com/result",
                "future_field": {"preserved": true}
            }]
        }))
    );

    let request = server.await??;
    assert_eq!(request["id"], "search-session");
    assert_eq!(request["model"], "gpt-5.6-sol");
    assert_eq!(
        request["commands"],
        json!({"search_query": [{"q": "standalone web search"}]})
    );
    assert_eq!(
        request["settings"],
        json!({"allowed_callers": ["direct"], "external_web_access": true})
    );
    assert_eq!(request["max_output_tokens"], 10_000);
    assert_eq!(
        request["input"],
        json!([{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Search the web"}]
        }])
    );
    assert!(request.get("reasoning").is_none());
    assert_eq!(auth_source.recoveries(), 1);
    Ok(())
}

async fn spawn_search_server() -> Result<(String, JoinHandle<Result<Value>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1/alpha/search", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (mut rejected, _) = listener.accept().await?;
        let request = read_request(&mut rejected).await?;
        assert_headers(&request.headers, "oauth-token-0")?;
        write_unauthorized(&mut rejected).await?;

        let (mut stream, _) = listener.accept().await?;
        let request = read_request(&mut stream).await?;
        assert_headers(&request.headers, "oauth-token-1")?;
        write_json(
            &mut stream,
            &json!({
                "encrypted_output": "ciphertext",
                "output": "Search result with turn0search0",
                "results": [{
                    "type": "text_result",
                    "ref_id": "turn0search0",
                    "url": "https://example.com/result",
                    "future_field": {"preserved": true}
                }]
            }),
        )
        .await?;
        Ok(serde_json::from_slice(&request.body)?)
    });
    Ok((endpoint, server))
}

fn assert_headers(headers: &str, token: &str) -> Result<()> {
    let headers = headers.to_ascii_lowercase();
    for expected in [
        format!("authorization: bearer {token}"),
        "chatgpt-account-id: account-test".to_owned(),
        "x-openai-fedramp: true".to_owned(),
        "x-injected-client: true".to_owned(),
    ] {
        if !headers.contains(&expected) {
            return Err(eyre!("search request omitted `{expected}`"));
        }
    }
    Ok(())
}
