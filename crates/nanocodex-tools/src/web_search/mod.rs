mod history;
mod schema;
mod wire;

use std::time::Duration;

use nanocodex_core::{OpenAiAuth, OpenAiAuthMode, OpenAiAuthSnapshot, ToolDefinition};
use reqwest::header::{AUTHORIZATION, USER_AGENT};
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};

use self::{
    history::recent_input,
    schema::commands_schema,
    wire::{SearchCommands, SearchRequest, SearchResponse, SearchSettings},
};
use super::{Tool, ToolContext, ToolExecution, ToolInput, ToolResult, WebSearchConfig};

const DESCRIPTION: &str = include_str!("web_run_description.md");
const ERROR_BODY_LIMIT: usize = 4_096;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_ATTEMPTS: usize = 2;
const TOOL_TIMEOUT: Duration = Duration::from_secs(45);
const RETRY_DELAY: Duration = Duration::from_millis(200);

pub(super) struct WebSearchHandler {
    client: reqwest::Client,
    endpoint: String,
    auth: OpenAiAuth,
}

impl WebSearchHandler {
    pub(super) fn new(config: WebSearchConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: config.endpoint,
            auth: config.auth,
        }
    }

    async fn run(&self, input: &str, context: ToolContext<'_>) -> ToolExecution {
        match timeout(TOOL_TIMEOUT, self.run_inner(input, context)).await {
            Ok(execution) => execution,
            Err(_) => ToolExecution::error(format!(
                "standalone web search timed out after {} seconds",
                TOOL_TIMEOUT.as_secs()
            )),
        }
    }

    async fn run_inner(&self, input: &str, context: ToolContext<'_>) -> ToolExecution {
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
        if let Err(error) = commands.validate() {
            return ToolExecution::error(error);
        }

        let commands = commands.into_requests();
        let request_count = commands.len();
        let input = recent_input(context.history);
        let mut outputs = Vec::with_capacity(request_count);
        let mut failures = Vec::new();
        let mut results = Vec::new();
        let mut saw_results = false;

        for (index, commands) in commands.iter().enumerate() {
            let request = SearchRequest {
                id: context.session_id,
                model: context.model,
                input: input.as_deref(),
                commands,
                settings: SearchSettings {
                    allowed_callers: ["direct"],
                    external_web_access: true,
                },
                max_output_tokens: request_token_budget(
                    context.output_token_budget,
                    index,
                    request_count,
                ),
            };
            let response = match self.search(&request).await {
                Ok(response) => response,
                Err(error) => {
                    failures.push(format!("web search request {} failed: {error}", index + 1));
                    continue;
                }
            };
            let SearchResponse {
                output,
                results: response_results,
                _encrypted_output: _,
            } = response;
            if let Some(response_results) = response_results {
                saw_results = true;
                results.extend(response_results);
            }
            if has_semantic_error(&output) {
                failures.push(format!(
                    "web search request {} returned an API error in its output",
                    index + 1
                ));
            } else {
                let missing = commands.missing_specialized_results(&output);
                if !missing.is_empty() {
                    failures.push(format!(
                        "web search request {} omitted results for: {}",
                        index + 1,
                        missing.join(", ")
                    ));
                }
            }
            if !output.is_empty() {
                outputs.push(output);
            }
        }

        let output = outputs.join("\n");
        let mut execution = if failures.is_empty() {
            ToolExecution::text(output.clone()).with_code_mode_value(Value::String(output))
        } else {
            let mut error = failures.join("\n");
            if !output.is_empty() {
                error.push_str("\n\nWeb search output:\n");
                error.push_str(&output);
            }
            ToolExecution::error(error)
        };
        if saw_results {
            execution = execution.with_metadata(json!({ "results": results }));
        }
        execution
    }

    async fn search(&self, request: &SearchRequest<'_>) -> Result<SearchResponse, String> {
        for attempt in 1..=MAX_ATTEMPTS {
            let (status, body) = match self.send(request).await {
                Ok(response) => response,
                Err(error) => {
                    if error.retryable && attempt < MAX_ATTEMPTS {
                        sleep(RETRY_DELAY).await;
                        continue;
                    }
                    return Err(error.message);
                }
            };
            let retryable = status.is_server_error();
            if retryable && attempt < MAX_ATTEMPTS {
                sleep(RETRY_DELAY).await;
                continue;
            }
            if !status.is_success() {
                return Err(format!(
                    "standalone web search returned HTTP {status}: {}",
                    body_preview(&body)
                ));
            }
            return serde_json::from_slice(&body).map_err(|error| {
                format!("failed to decode standalone web search response: {error}")
            });
        }
        Err("standalone web search exhausted its retry attempts".to_owned())
    }

    async fn send(
        &self,
        request: &SearchRequest<'_>,
    ) -> Result<(reqwest::StatusCode, Vec<u8>), RequestFailure> {
        let auth = self
            .auth
            .snapshot()
            .await
            .map_err(|error| auth_failure(&error))?;
        let response = self.send_authorized(request, &auth).await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            && auth.mode() == OpenAiAuthMode::ChatGpt
        {
            self.auth
                .recover_unauthorized(&auth)
                .await
                .map_err(|error| auth_failure(&error))?;
            let refreshed = self
                .auth
                .snapshot()
                .await
                .map_err(|error| auth_failure(&error))?;
            return self
                .read_response(self.send_authorized(request, &refreshed).await?)
                .await;
        }
        self.read_response(response).await
    }

    async fn send_authorized(
        &self,
        body: &SearchRequest<'_>,
        auth: &OpenAiAuthSnapshot,
    ) -> Result<reqwest::Response, RequestFailure> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .header(USER_AGENT, concat!("nanocodex/", env!("CARGO_PKG_VERSION")))
            .header(AUTHORIZATION, format!("Bearer {}", auth.bearer()));
        if let Some(account_id) = auth.account_id() {
            request = request.header("ChatGPT-Account-ID", account_id);
        }
        if auth.is_fedramp() {
            request = request.header("X-OpenAI-Fedramp", "true");
        }
        request
            .json(body)
            .send()
            .await
            .map_err(|error| RequestFailure {
                message: format!("standalone web search request failed: {error}"),
                retryable: true,
            })
    }

    async fn read_response(
        &self,
        response: reqwest::Response,
    ) -> Result<(reqwest::StatusCode, Vec<u8>), RequestFailure> {
        let status = response.status();
        let body = read_response_body(response).await.map_err(|mut failure| {
            failure.retryable |= status.is_server_error();
            failure
        })?;
        Ok((status, body))
    }
}

fn auth_failure(error: &nanocodex_core::OpenAiAuthError) -> RequestFailure {
    RequestFailure {
        message: error.to_string(),
        retryable: false,
    }
}

#[async_trait::async_trait]
impl Tool for WebSearchHandler {
    fn name(&self) -> &'static str {
        "web__run"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(self.name(), DESCRIPTION, commands_schema())
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let input = input.function_json()?;
        Ok(self.run(input.get(), context).await)
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

struct RequestFailure {
    message: String,
    retryable: bool,
}

async fn read_response_body(mut response: reqwest::Response) -> Result<Vec<u8>, RequestFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(response_too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| RequestFailure {
        message: format!("failed to read standalone web search response: {error}"),
        retryable: true,
    })? {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(response_too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn response_too_large() -> RequestFailure {
    RequestFailure {
        message: format!(
            "standalone web search response exceeded the {MAX_RESPONSE_BYTES}-byte limit"
        ),
        retryable: false,
    }
}

fn request_token_budget(total: usize, index: usize, request_count: usize) -> u64 {
    let base = total / request_count;
    let remainder = total % request_count;
    u64::try_from(base + usize::from(index < remainder))
        .unwrap_or(u64::MAX)
        .max(1)
}

fn has_semantic_error(output: &str) -> bool {
    output.lines().any(|line| {
        let line = line.trim();
        line.starts_with("Error parsing function call:")
            || line.starts_with("Found no tool response.")
            || line == "Internal Error ()"
    })
}

#[cfg(test)]
mod tests {
    use std::{future::ready, sync::Arc};

    use eyre::{Result, eyre};
    use nanocodex_core::{
        OpenAiAuth, OpenAiAuthError, OpenAiAuthFuture, OpenAiAuthMode, OpenAiAuthSnapshot,
        OpenAiAuthSource,
    };
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };

    use super::{Tool, ToolContext, WebSearchConfig, WebSearchHandler};
    use crate::ToolOutputBody;

    #[tokio::test]
    async fn posts_codex_search_request_and_returns_plaintext_output() -> Result<()> {
        let (endpoint, server) = spawn_search_server().await?;
        let handler = WebSearchHandler::new(WebSearchConfig {
            endpoint,
            auth: subscription_auth(),
        });
        let history = serde_json::from_value::<Vec<nanocodex_core::ResponseItem>>(json!([
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
        ]))?;
        let execution = handler
            .run(
                r#"{"search_query":[{"q":"standalone web search"}]}"#,
                ToolContext {
                    model: "gpt-5.6-sol",
                    session_id: "search-session",
                    call_id: "call-search",
                    history: &history,
                    output_token_budget: crate::DEFAULT_TOOL_OUTPUT_TOKENS,
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
            execution
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
        Ok(())
    }

    async fn spawn_search_server() -> Result<(String, JoinHandle<Result<Value>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/v1/alpha/search", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let (headers, body) = read_http_request(&mut stream).await?;
            let headers = headers.to_ascii_lowercase();
            if !headers.contains("authorization: bearer subscription-token") {
                return Err(eyre!("search request did not contain bearer auth"));
            }
            if !headers.contains("chatgpt-account-id: account-test") {
                return Err(eyre!("search request did not contain the account ID"));
            }
            if !headers.contains("x-openai-fedramp: true") {
                return Err(eyre!("search request did not contain FedRAMP routing"));
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

    struct StaticSubscriptionAuth;

    impl OpenAiAuthSource for StaticSubscriptionAuth {
        fn validate(&self) -> std::result::Result<(), OpenAiAuthError> {
            Ok(())
        }

        fn snapshot(
            &self,
        ) -> OpenAiAuthFuture<'_, std::result::Result<OpenAiAuthSnapshot, OpenAiAuthError>>
        {
            Box::pin(ready(Ok(OpenAiAuthSnapshot::new(
                OpenAiAuthMode::ChatGpt,
                "subscription-token",
                Some("account-test"),
                true,
                1,
            ))))
        }

        fn recover_unauthorized(
            &self,
            _rejected: &OpenAiAuthSnapshot,
        ) -> OpenAiAuthFuture<'_, std::result::Result<(), OpenAiAuthError>> {
            Box::pin(ready(Ok(())))
        }
    }

    fn subscription_auth() -> OpenAiAuth {
        OpenAiAuth::managed_chatgpt(Arc::new(StaticSubscriptionAuth))
    }

    #[test]
    fn exposes_codex_web_run_schema_and_description() {
        let handler = WebSearchHandler::new(WebSearchConfig {
            endpoint: "http://127.0.0.1:1/v1/alpha/search".to_owned(),
            auth: nanocodex_core::OpenAiAuth::api_key("test-key"),
        });
        let spec = serde_json::to_value(handler.definition()).unwrap();

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
