use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::Arc};

#[cfg(not(target_family = "wasm"))]
pub type HostFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
#[cfg(target_family = "wasm")]
pub type HostFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// Stable effect identity. Hosts must authorize the actual session and task,
/// and reconcile uncertain effects by this identity before allowing a retry.
#[derive(Clone, Debug)]
pub struct HostContext {
    pub model: String,
    pub session_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub instruction_revision: Option<u64>,
    pub host_context: Option<Arc<str>>,
}
#[derive(Clone, Debug)]
pub struct HostRequest {
    pub context: HostContext,
    pub tool: String,
    pub input: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}
/// Native Responses input media. Metadata and structured data are kept separate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    InputText {
        text: String,
    },
    InputImage {
        image_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    InputFile {
        #[serde(skip_serializing_if = "Option::is_none")]
        file_data: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolOutput {
    pub text: String,
    #[serde(default)]
    pub content: Vec<ToolContent>,
    pub is_error: bool,
    pub structured_result: Option<Value>,
    pub metadata: Option<Value>,
}
impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            content: vec![],
            is_error: false,
            structured_result: None,
            metadata: None,
        }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::text(text)
        }
    }
    pub fn with_metadata(mut self, value: Value) -> Self {
        self.metadata = Some(value);
        self
    }
    pub fn with_structured_result(mut self, value: Value) -> Self {
        self.structured_result = Some(value);
        self
    }
    pub fn wire_output(&self) -> Value {
        if self.content.is_empty() {
            return Value::String(self.text.clone());
        }
        let mut parts = vec![];
        if !self.text.is_empty() {
            parts.push(json!({"type":"input_text","text":self.text}));
        }
        parts.extend(
            self.content
                .iter()
                .map(|part| serde_json::to_value(part).expect("media serializes")),
        );
        Value::Array(parts)
    }
}
/// Caller-owned capability catalog. Never install a provider just to advertise
/// unsupported tools. A browser provider supplies its own exact schemas here.
pub trait XaiHost: Send + Sync + 'static {
    fn definitions(&self) -> Vec<ToolDefinition>;
    fn call(&self, request: HostRequest) -> HostFuture<Result<ToolOutput, String>>;
}
/// An explicit catalog plus a single host dispatcher, useful for browser APIs,
/// native runtimes and WASM external handlers. Does not itself grant authority.
pub struct XaiHostTools {
    definitions: Vec<ToolDefinition>,
    dispatch: Arc<dyn Fn(HostRequest) -> HostFuture<Result<ToolOutput, String>> + Send + Sync>,
}
impl XaiHostTools {
    pub fn new(
        definitions: Vec<ToolDefinition>,
        dispatch: impl Fn(HostRequest) -> HostFuture<Result<ToolOutput, String>> + Send + Sync + 'static,
    ) -> Result<Self, String> {
        validate_catalog(&definitions)?;
        Ok(Self {
            definitions,
            dispatch: Arc::new(dispatch),
        })
    }
}
impl XaiHost for XaiHostTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }
    fn call(&self, request: HostRequest) -> HostFuture<Result<ToolOutput, String>> {
        if let Err(error) = validate_request(&request).and_then(|()| {
            self.definitions
                .iter()
                .any(|d| d.name == request.tool)
                .then_some(())
                .ok_or("tool not installed".into())
        }) {
            return Box::pin(async { Err(error) });
        }
        (self.dispatch)(request)
    }
}
pub fn validate_catalog(definitions: &[ToolDefinition]) -> Result<(), String> {
    let mut names = std::collections::HashSet::new();
    for d in definitions {
        if d.name.is_empty()
            || d.name.len() > 64
            || !d
                .name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            || !d.parameters.is_object()
            || !names.insert(d.name.clone())
        {
            return Err(format!("invalid or duplicate tool definition: {}", d.name));
        }
    }
    Ok(())
}
pub fn validate_request(request: &HostRequest) -> Result<(), String> {
    if request.context.call_id.is_empty()
        || request.context.session_id.is_empty()
        || request.context.turn_id.is_empty()
    {
        return Err("host effects require session, turn and call identity".into());
    }
    if !request.input.is_object() || request.input.to_string().len() > 2 * 1024 * 1024 {
        return Err("tool input must be a bounded JSON object".into());
    }
    Ok(())
}
pub(crate) fn definition(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        parameters: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    }
}
pub(crate) fn fields(input: &Value, allowed: &[&str]) -> Result<(), String> {
    let object = input.as_object().ok_or("arguments must be an object")?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("unsupported argument: {key}"));
    }
    Ok(())
}
pub(crate) fn string<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input[key]
        .as_str()
        .ok_or_else(|| format!("missing or invalid {key}"))
}
pub(crate) fn number(input: &Value, key: &str, default: u64, max: u64) -> Result<u64, String> {
    let value = match input.get(key) {
        None => default,
        Some(v) => v
            .as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .ok_or_else(|| format!("invalid {key}"))?,
    };
    if value > max {
        return Err(format!("{key} exceeds {max}"));
    }
    Ok(value)
}
pub(crate) fn boolean(input: &Value, key: &str, default: bool) -> Result<bool, String> {
    match input.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_bool()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .ok_or_else(|| format!("invalid {key}")),
    }
}
