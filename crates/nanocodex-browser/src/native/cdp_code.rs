use std::{collections::BTreeMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use chromiumoxide::{
    Connection, Method,
    types::{CallId, EventMessage, Message, MethodId},
};
use futures_util::StreamExt as _;
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, Tools,
    runtime::ToolRuntime,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{sync::Mutex, time::timeout};

use super::BrowserError;

const CDP_CALL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BROWSER_CODE_BYTES: usize = 64 * 1024;
const MAX_CDP_PARAMETER_BYTES: usize = 256 * 1024;
const MAX_CDP_RESULT_BYTES: usize = 4 * 1024 * 1024;
const ALLOWED_METHODS: [&str; 22] = [
    "Target.getTargets",
    "Target.createTarget",
    "Target.closeTarget",
    "Target.attachToTarget",
    "Page.enable",
    "Page.navigate",
    "Page.reload",
    "Page.stopLoading",
    "Page.captureScreenshot",
    "Page.getLayoutMetrics",
    "DOM.enable",
    "DOM.getDocument",
    "DOM.querySelector",
    "DOM.querySelectorAll",
    "DOM.getOuterHTML",
    "DOM.getAttributes",
    "DOM.getBoxModel",
    "DOM.focus",
    "DOM.scrollIntoView",
    "Input.dispatchMouseEvent",
    "Input.dispatchKeyEvent",
    "Input.insertText",
];

#[derive(Debug, Deserialize)]
struct RawCdpEvent {
    method: MethodId,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    params: Value,
}

impl Method for RawCdpEvent {
    fn identifier(&self) -> MethodId {
        self.method.clone()
    }
}

impl EventMessage for RawCdpEvent {
    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
}

struct CdpClient {
    connection: Mutex<Connection<RawCdpEvent>>,
}

impl CdpClient {
    async fn connect(websocket_address: &str) -> Result<Arc<Self>, BrowserError> {
        Ok(Arc::new(Self {
            connection: Mutex::new(Connection::connect(websocket_address).await?),
        }))
    }

    async fn send(
        &self,
        method: &str,
        params: Value,
        session_id: Option<String>,
    ) -> Result<Value, BrowserError> {
        validate_command(method, &params)?;
        let parameter_bytes = serde_json::to_vec(&params)?.len();
        if parameter_bytes > MAX_CDP_PARAMETER_BYTES {
            return Err(BrowserError::BrowserExecute {
                message: format!(
                    "CDP parameters are {parameter_bytes} bytes, above the {MAX_CDP_PARAMETER_BYTES}-byte limit"
                ),
            });
        }
        let mut connection = self.connection.lock().await;
        let call = connection.submit_command(
            method.to_owned().into(),
            session_id.map(Into::into),
            params,
        )?;
        let response = wait_for_response(&mut connection, call).await?;
        if let Some(error) = response.error {
            return Err(BrowserError::BrowserExecute {
                message: format!("CDP method {method} was rejected: {error}"),
            });
        }
        let result = response.result.unwrap_or_else(|| json!({}));
        let result = sanitize_value(result, None, 0);
        let result_bytes = serde_json::to_vec(&result)?.len();
        if result_bytes > MAX_CDP_RESULT_BYTES {
            return Err(BrowserError::BrowserExecute {
                message: format!(
                    "CDP result is {result_bytes} bytes, above the {MAX_CDP_RESULT_BYTES}-byte limit"
                ),
            });
        }
        Ok(result)
    }
}

async fn wait_for_response(
    connection: &mut Connection<RawCdpEvent>,
    call: CallId,
) -> Result<chromiumoxide::types::Response, BrowserError> {
    let deadline = tokio::time::Instant::now() + CDP_CALL_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(BrowserError::BrowserExecute {
                message: "CDP command timed out".to_owned(),
            });
        }
        let message = timeout(remaining, connection.next())
            .await
            .map_err(|_| BrowserError::BrowserExecute {
                message: "CDP command timed out".to_owned(),
            })?
            .ok_or_else(|| BrowserError::BrowserExecute {
                message: "the browser DevTools connection closed".to_owned(),
            })??;
        match message {
            Message::Response(response) if response.id == call => return Ok(response),
            Message::Event(event) => {
                let _ = (&event.method, &event.params);
            }
            Message::Response(_) => {}
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CdpSendInput {
    method: String,
    #[serde(default = "empty_object")]
    params: Value,
    session_id: Option<String>,
}

fn empty_object() -> Value {
    json!({})
}

struct CdpSendTool {
    client: Arc<CdpClient>,
}

#[async_trait]
impl Tool for CdpSendTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "browser_cdp_send",
            "Send one policy-filtered Chrome DevTools Protocol command.",
            json!({
                "type": "object",
                "properties": {
                    "method": { "type": "string" },
                    "params": { "type": "object" },
                    "sessionId": { "type": "string" }
                },
                "required": ["method"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let input = input.decode_json::<CdpSendInput>()?;
        Ok(ToolOutput::from_json(
            self.client
                .send(&input.method, input.params, input.session_id)
                .await?,
            true,
        ))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttachInput {
    target_id: String,
    flatten: Option<bool>,
}

struct CdpAttachTool {
    client: Arc<CdpClient>,
}

#[async_trait]
impl Tool for CdpAttachTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "browser_cdp_attach_to_target",
            "Attach to one page target and return its CDP session ID.",
            json!({
                "type": "object",
                "properties": {
                    "targetId": { "type": "string" },
                    "flatten": { "type": "boolean" }
                },
                "required": ["targetId"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let input = input.decode_json::<AttachInput>()?;
        Ok(ToolOutput::from_json(
            self.client
                .send(
                    "Target.attachToTarget",
                    json!({
                        "targetId": input.target_id,
                        "flatten": input.flatten.unwrap_or(true),
                    }),
                    None,
                )
                .await?,
            true,
        ))
    }
}

struct StaticTool {
    name: &'static str,
    description: &'static str,
    parameters: Value,
    result: Value,
}

#[async_trait]
impl Tool for StaticTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(self.name, self.description, self.parameters.clone())
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        Ok(ToolOutput::from_json(self.result.clone(), true))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryInput {
    query: String,
}

struct CdpSearchTool;

#[async_trait]
impl Tool for CdpSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "browser_cdp_search",
            "Search the browser connector method catalog.",
            json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let query = input
            .decode_json::<QueryInput>()?
            .query
            .to_ascii_lowercase();
        let methods = [
            ("cdp.send", "Send one allowlisted raw CDP command."),
            (
                "cdp.attachToTarget",
                "Attach to a target returned by Target.getTargets.",
            ),
            ("cdp.spec", "List the allowlisted CDP domains and commands."),
        ]
        .into_iter()
        .filter(|(name, description)| {
            query.trim().is_empty()
                || name.to_ascii_lowercase().contains(&query)
                || description.to_ascii_lowercase().contains(&query)
        })
        .map(|(name, description)| json!({ "name": name, "description": description }))
        .collect::<Vec<_>>();
        Ok(ToolOutput::json(&methods))
    }
}

struct CdpDescribeTool;

#[async_trait]
impl Tool for CdpDescribeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "browser_cdp_describe",
            "Describe one browser connector method.",
            json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let query = input.decode_json::<QueryInput>()?.query;
        let result = match query.as_str() {
            "cdp.send" => json!({
                "name": "cdp.send",
                "signature": "cdp.send({ method, params?, sessionId? })",
                "description": "Send one allowlisted raw CDP command. Attach to a page before Page, DOM, or Input commands."
            }),
            "cdp.attachToTarget" => json!({
                "name": "cdp.attachToTarget",
                "signature": "cdp.attachToTarget({ targetId, flatten? })",
                "description": "Attach to a target and return { sessionId }."
            }),
            "cdp.spec" => json!({
                "name": "cdp.spec",
                "signature": "cdp.spec({})",
                "description": "Return the exact allowlisted CDP commands."
            }),
            _ => json!({ "name": query, "found": false }),
        };
        Ok(ToolOutput::json(&result))
    }
}

pub(super) fn validate_code(code: &str) -> Result<(), BrowserError> {
    if code.len() > MAX_BROWSER_CODE_BYTES {
        return Err(BrowserError::BrowserExecute {
            message: format!(
                "browser code is {} bytes, above the {MAX_BROWSER_CODE_BYTES}-byte limit",
                code.len()
            ),
        });
    }
    let normalized = code
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let forbidden = [
        "cookie",
        "authorization",
        "credential",
        "password",
        "token",
        "secret",
        "live view",
        "getliveviewurl",
        "connecturl",
        "websocketdebuggerurl",
        "runtime.evaluate",
        "runtime.callfunctionon",
        "setextrahttpheaders",
    ];
    if forbidden.iter().any(|term| normalized.contains(term)) {
        return Err(BrowserError::BrowserExecute {
            message:
                "browser code requested a credential-bearing or unrestricted runtime capability"
                    .to_owned(),
        });
    }
    Ok(())
}

fn validate_command(method: &str, params: &Value) -> Result<(), BrowserError> {
    if !ALLOWED_METHODS.contains(&method) {
        return Err(BrowserError::BrowserExecute {
            message: format!("CDP method {method} is blocked by browser credential policy"),
        });
    }
    if matches!(method, "Page.navigate" | "Target.createTarget") {
        let url = params
            .as_object()
            .and_then(|params| params.get("url"))
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserError::BrowserExecute {
                message: format!("CDP method {method} requires an absolute HTTP(S) url"),
            })?;
        let url = url::Url::parse(url)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(BrowserError::UnsupportedUrlScheme {
                scheme: url.scheme().to_owned(),
            });
        }
    }
    Ok(())
}

pub(super) async fn execute(
    websocket_address: &str,
    source: &str,
    context: ToolContext<'_>,
) -> Result<ToolOutput, BrowserError> {
    validate_code(source)?;
    let client = CdpClient::connect(websocket_address).await?;
    let tools = Tools::builder()
        .without_defaults()
        .tool(CdpSendTool {
            client: Arc::clone(&client),
        })
        .tool(CdpAttachTool { client })
        .tool(CdpSearchTool)
        .tool(CdpDescribeTool)
        .tool(StaticTool {
            name: "browser_cdp_spec",
            description: "Return the exact allowlisted CDP command catalog.",
            parameters: json!({ "type": "object", "additionalProperties": false }),
            result: protocol_spec(),
        })
        .build()
        .map_err(|error| BrowserError::BrowserExecute {
            message: error.to_string(),
        })?;
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
    let source = wrapped_source(source);
    let execution = runtime
        .execute_code(&source, context)
        .await
        .map_err(|error| BrowserError::BrowserExecute {
            message: error.to_string(),
        })?;
    let value = execution_value(&execution.output).or_else(|| {
        execution
            .nested_calls
            .last()
            .map(|call| call.structured_result.clone())
    });
    match value {
        Some(value) => Ok(ToolOutput::from_json(
            sanitize_value(value, None, 0),
            execution.success,
        )),
        None if execution.success => Ok(ToolOutput::from_json(Value::Null, true)),
        None => Ok(ToolOutput::error("browser code execution failed")),
    }
}

fn wrapped_source(source: &str) -> String {
    let source = return_last_expression(source);
    format!(
        r#"
const cdp = Object.freeze({{
  send: (args) => tools.browser_cdp_send(args),
  attachToTarget: (args) => tools.browser_cdp_attach_to_target(args),
  spec: (args = {{}}) => tools.browser_cdp_spec(args),
}});
const codemode = Object.freeze({{
  search: (query) => tools.browser_cdp_search({{ query }}),
  describe: (query) => tools.browser_cdp_describe({{ query }}),
}});
const __browserResult = await (async (
  tools, ALL_TOOLS, toolSchema, text, image, audio, generatedImage, notify,
  store, load, yield_control, exit
) => {{
{source}
}})(
  undefined, undefined, undefined, undefined, undefined, undefined,
  undefined, undefined, undefined, undefined, undefined, undefined
);
text(__browserResult);
"#
    )
}

fn return_last_expression(source: &str) -> String {
    let trimmed = source.trim_end();
    if trimmed.is_empty()
        || trimmed
            .lines()
            .any(|line| line.trim_start().starts_with("return "))
    {
        return source.to_owned();
    }
    let Some((prefix, last)) = trimmed.rsplit_once('\n') else {
        let expression = trimmed.trim_end_matches(';').trim();
        return format!("return ({expression});");
    };
    let expression = last.trim().trim_end_matches(';').trim();
    let declaration = [
        "const ",
        "let ",
        "var ",
        "if ",
        "if(",
        "for ",
        "for(",
        "while ",
        "while(",
        "switch ",
        "switch(",
        "try ",
        "throw ",
        "class ",
        "function ",
        "{",
        "}",
    ]
    .iter()
    .any(|prefix| expression.starts_with(prefix));
    if expression.is_empty() || declaration {
        source.to_owned()
    } else {
        format!("{prefix}\nreturn ({expression});")
    }
}

fn execution_value(output: &nanocodex_tools::contract::ToolOutputBody) -> Option<Value> {
    match output {
        nanocodex_tools::contract::ToolOutputBody::Text(text) => parse_output_text(text),
        nanocodex_tools::contract::ToolOutputBody::Content(content) => {
            let values = content
                .iter()
                .filter_map(|item| match item {
                    nanocodex_tools::contract::ToolOutputContent::InputText { text } => {
                        parse_output_text(text)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            match values.as_slice() {
                [] => None,
                [value] => Some(value.clone()),
                _ => Some(Value::Array(values)),
            }
        }
    }
}

fn parse_output_text(text: &str) -> Option<Value> {
    if text == "undefined" || text.starts_with("Script error:") {
        return None;
    }
    Some(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned())))
}

fn protocol_spec() -> Value {
    let mut domains = BTreeMap::<&str, Vec<&str>>::new();
    for method in ALLOWED_METHODS {
        let (domain, command) = method.split_once('.').expect("fixed CDP method");
        domains.entry(domain).or_default().push(command);
    }
    Value::Object(
        domains
            .into_iter()
            .map(|(domain, commands)| {
                (
                    domain.to_owned(),
                    json!({
                        "commands": commands,
                        "credentialSafe": true,
                    }),
                )
            })
            .collect(),
    )
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized.contains("cookie")
        || normalized.contains("authorization")
        || normalized.contains("credential")
        || normalized.contains("password")
        || normalized.contains("apikey")
        || normalized.contains("accesstoken")
        || normalized.contains("refreshtoken")
        || normalized == "token"
        || normalized.contains("secret")
        || normalized.contains("connecturl")
        || normalized.contains("websocketdebuggerurl")
        || normalized.contains("signingkey")
        || normalized.contains("liveview")
}

fn sanitize_value(value: Value, key: Option<&str>, depth: usize) -> Value {
    if key.is_some_and(sensitive_key) {
        return Value::String("[redacted]".to_owned());
    }
    if depth >= 24 {
        return Value::String("[truncated]".to_owned());
    }
    match value {
        Value::String(value) => sanitize_string(value),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| sanitize_value(value, key, depth + 1))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let value = sanitize_value(value, Some(&key), depth + 1);
                    (key, value)
                })
                .collect(),
        ),
        value => value,
    }
}

fn sanitize_string(value: String) -> Value {
    let lower = value.to_ascii_lowercase();
    if lower.contains("browserbase.com") || lower.contains("browser.run") {
        Value::String("[redacted provider URL]".to_owned())
    } else if lower.contains("set-cookie:")
        || lower.contains("document.cookie:")
        || lower.contains("cookie:")
        || ["session=", "sid=", "token=", "auth="]
            .iter()
            .any(|marker| lower.contains(marker))
    {
        Value::String("[redacted cookie material]".to_owned())
    } else {
        Value::String(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_policy_matches_the_managed_cloudflare_surface() {
        assert!(validate_code("return cdp.send({ method: 'Page.navigate' })").is_ok());
        assert!(validate_code("return cdp.send({ method: 'Runtime.evaluate' })").is_err());
        assert!(validate_code("return cdp.getLiveViewUrl()").is_err());
        assert!(validate_command("DOM.getDocument", &json!({})).is_ok());
        assert!(validate_command("Network.getAllCookies", &json!({})).is_err());
        assert!(
            validate_command("Page.navigate", &json!({ "url": "file:///etc/passwd" })).is_err()
        );
    }

    #[test]
    fn cloudflare_style_final_expression_is_returned() {
        assert_eq!(
            return_last_expression("const value = 40 + 2;\nvalue"),
            "const value = 40 + 2;\nreturn (value);"
        );
        assert_eq!(
            return_last_expression("await cdp.send({ method: 'Target.getTargets' });"),
            "return (await cdp.send({ method: 'Target.getTargets' }));"
        );
    }

    #[test]
    fn credential_shaped_results_are_redacted() {
        assert_eq!(
            sanitize_value(
                json!({
                    "cookies": [{ "value": "private" }],
                    "page": "https://example.com",
                    "scalar": "sid=private; theme=dark"
                }),
                None,
                0,
            ),
            json!({
                "cookies": "[redacted]",
                "page": "https://example.com",
                "scalar": "[redacted cookie material]"
            })
        );
    }
}
