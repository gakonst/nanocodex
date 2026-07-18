mod history;
mod schema;
mod wire;

use reqwest::header::USER_AGENT;
use serde_json::{Value, json};

use self::{
    history::recent_input,
    schema::commands_schema,
    wire::{SearchCommands, SearchRequest, SearchResponse, SearchSettings},
};
use super::{ToolContext, ToolExecution, ToolFuture, ToolHandler, WebSearchConfig};

const DESCRIPTION: &str = include_str!("web_run_description.md");
const MAX_OUTPUT_TOKENS: u64 = 10_000;
const ERROR_BODY_LIMIT: usize = 4_096;

pub(super) struct WebSearchHandler {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl WebSearchHandler {
    pub(super) fn new(config: WebSearchConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: config.endpoint,
            api_key: config.api_key,
        }
    }

    async fn run(&self, input: &str, context: ToolContext<'_>) -> ToolExecution {
        let commands = if input.trim().is_empty() {
            SearchCommands::default()
        } else {
            match serde_json::from_str(input) {
                Ok(commands) => commands,
                Err(error) => {
                    return ToolExecution::error(format!(
                        "failed to parse web.run arguments: {error}"
                    ));
                }
            }
        };
        let request = SearchRequest {
            id: context.session_id,
            model: context.model,
            input: recent_input(context.history),
            commands: &commands,
            settings: SearchSettings {
                allowed_callers: ["direct"],
                external_web_access: true,
            },
            max_output_tokens: MAX_OUTPUT_TOKENS,
        };
        let response = match self
            .client
            .post(&self.endpoint)
            .header(USER_AGENT, concat!("harness/", env!("CARGO_PKG_VERSION")))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return ToolExecution::error(format!(
                    "standalone web search request failed: {error}"
                ));
            }
        };
        let status = response.status();
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => {
                return ToolExecution::error(format!(
                    "failed to read standalone web search response: {error}"
                ));
            }
        };
        if !status.is_success() {
            return ToolExecution::error(format!(
                "standalone web search returned HTTP {status}: {}",
                body_preview(&body)
            ));
        }
        let response = match serde_json::from_slice::<SearchResponse>(&body) {
            Ok(response) => response,
            Err(error) => {
                return ToolExecution::error(format!(
                    "failed to decode standalone web search response: {error}"
                ));
            }
        };
        let SearchResponse {
            output,
            results,
            _encrypted_output: _,
        } = response;
        let mut execution =
            ToolExecution::text(output.clone()).with_code_mode_value(Value::String(output));
        if let Some(results) = results {
            execution = execution.with_metadata(json!({ "results": results }));
        }
        execution
    }
}

impl ToolHandler for WebSearchHandler {
    fn name(&self) -> &'static str {
        "web__run"
    }

    fn spec(&self) -> Value {
        json!({
            "type": "function",
            "name": self.name(),
            "description": DESCRIPTION,
            "strict": false,
            "parameters": commands_schema(),
        })
    }

    fn execute<'a>(&'a self, input: String, context: ToolContext<'a>) -> ToolFuture<'a> {
        Box::pin(async move { self.run(&input, context).await })
    }
}

fn body_preview(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let mut end = text.len().min(ERROR_BODY_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let suffix = if end < text.len() { "…" } else { "" };
    format!("{}{suffix}", &text[..end])
}

#[cfg(test)]
mod tests {
    use eyre::{Result, eyre};
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };

    use super::{ToolContext, ToolHandler, WebSearchConfig, WebSearchHandler};
    use crate::tools::ToolOutputBody;

    #[tokio::test]
    async fn posts_codex_search_request_and_returns_plaintext_output() -> Result<()> {
        let (endpoint, server) = spawn_search_server().await?;
        let handler = WebSearchHandler::new(WebSearchConfig {
            endpoint,
            api_key: "test-key".to_owned(),
        });
        let history = vec![
            json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "<environment_context>ignored</environment_context>"
                }]
            }),
            json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Search the web"}]
            }),
        ];
        let execution = handler
            .run(
                r#"{"search_query":[{"q":"standalone web search"}]}"#,
                ToolContext {
                    model: "gpt-5.6-sol",
                    session_id: "search-session",
                    call_id: "call-search",
                    history: &history,
                },
            )
            .await;

        assert!(execution.success);
        assert!(matches!(
            execution.output,
            ToolOutputBody::Text(ref text) if text == "Search result with turn0search0"
        ));
        assert_eq!(
            execution.value(),
            Value::String("Search result with turn0search0".to_owned())
        );
        assert_eq!(
            execution.metadata,
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
        Ok(())
    }

    async fn spawn_search_server() -> Result<(String, JoinHandle<Result<Value>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/v1/alpha/search", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let (headers, body) = read_http_request(&mut stream).await?;
            if !headers
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key")
            {
                return Err(eyre!("search request did not contain bearer auth"));
            }
            let response = serde_json::to_vec(&json!({
                "encrypted_output": "ciphertext",
                "output": "Search result with turn0search0",
                "results": [{
                    "type": "text_result",
                    "ref_id": "turn0search0",
                    "url": "https://example.com/result",
                    "future_field": {"preserved": true}
                }]
            }))?;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await?;
            stream.write_all(&response).await?;
            Ok(body)
        });
        Ok((endpoint, server))
    }

    #[test]
    fn exposes_codex_web_run_schema_and_description() {
        let handler = WebSearchHandler::new(WebSearchConfig {
            endpoint: "http://127.0.0.1:1/v1/alpha/search".to_owned(),
            api_key: "test-key".to_owned(),
        });
        let spec = handler.spec();

        assert_eq!(spec["name"], "web__run");
        assert_eq!(spec["strict"], false);
        assert_eq!(
            spec.pointer("/parameters/properties/time/description"),
            Some(&json!("Get time for the given UTC offsets."))
        );
        assert!(
            spec["description"]
                .as_str()
                .is_some_and(|description| description.contains("turn2search5"))
        );
    }

    async fn read_http_request(stream: &mut TcpStream) -> Result<(String, Value)> {
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Err(eyre!("HTTP request ended before its headers"));
            }
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = std::str::from_utf8(&bytes[..header_end])?.to_owned();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .ok_or_else(|| eyre!("HTTP request omitted content-length"))?;
        while bytes.len() - header_end < content_length {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Err(eyre!("HTTP request body ended early"));
            }
            bytes.extend_from_slice(&chunk[..read]);
        }
        Ok((
            headers,
            serde_json::from_slice(&bytes[header_end..header_end + content_length])?,
        ))
    }
}
