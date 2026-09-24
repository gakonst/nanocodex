use nanocodex_tools::{ToolInput, contract::ToolOutputWire, standard::StandardTool};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum GuestTool {
    Standard(StandardTool),
    Computer { name: String },
}
impl From<StandardTool> for GuestTool {
    fn from(tool: StandardTool) -> Self {
        Self::Standard(tool)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub(crate) enum SessionRequest {
    Ready(ReadyRequest),
    ComputerCatalog(ComputerCatalogRequest),
    Tool(ToolRequest),
    WriteFile(WriteFileRequest),
    CreateDirectory(CreateDirectoryRequest),
    ReadFile(ReadFileRequest),
    Memory(MemoryRequest),
    Execute(ExecuteRequest),
    Cancel(CancelRequest),
    TerminateToolProcesses(TerminateToolProcessesRequest),
    Shutdown(ShutdownRequest),
}

impl SessionRequest {
    #[cfg(any(feature = "guest-runtime", test))]
    pub const fn id(&self) -> u64 {
        match self {
            Self::Ready(request) => request.id,
            Self::ComputerCatalog(request) => request.id,
            Self::Tool(request) => request.id,
            Self::WriteFile(request) => request.id,
            Self::CreateDirectory(request) => request.id,
            Self::ReadFile(request) => request.id,
            Self::Memory(request) => request.id,
            Self::Execute(request) => request.id,
            Self::Cancel(request) => request.id,
            Self::TerminateToolProcesses(request) => request.id,
            Self::Shutdown(request) => request.id,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub(crate) enum SessionResponse {
    Ready(ControlResponse),
    ComputerCatalog(ComputerCatalogResponse),
    Tool(ToolResponse),
    WriteFile(ControlResponse),
    CreateDirectory(ControlResponse),
    ReadFile(ReadFileResponse),
    Memory(MemoryResponse),
    Execute(ExecuteResponse),
    Output(OutputChunk),
    Cancel(ControlResponse),
    TerminateToolProcesses(ControlResponse),
    Shutdown(ControlResponse),
}

impl SessionResponse {
    pub const fn id(&self) -> u64 {
        match self {
            Self::Ready(response) => response.id,
            Self::ComputerCatalog(response) => response.id,
            Self::Tool(response) => response.id,
            Self::WriteFile(response)
            | Self::CreateDirectory(response)
            | Self::Cancel(response)
            | Self::TerminateToolProcesses(response)
            | Self::Shutdown(response) => response.id,
            Self::ReadFile(response) => response.id,
            Self::Memory(response) => response.id,
            Self::Execute(response) => response.id,
            Self::Output(response) => response.id,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComputerCatalogRequest {
    pub id: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComputerCatalogResponse {
    pub id: u64,
    pub tools: Option<Vec<nanocodex_computer::ProviderTool>>,
    pub error: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadyRequest {
    pub id: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminateToolProcessesRequest {
    pub id: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShutdownRequest {
    pub id: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteFileRequest {
    pub id: u64,
    pub path: String,
    #[serde(with = "wire_bytes")]
    pub contents: Vec<u8>,
    pub mode: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_seconds: Option<i64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateDirectoryRequest {
    pub id: u64,
    pub path: String,
    pub mode: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_seconds: Option<i64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadFileRequest {
    pub id: u64,
    pub path: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryRequest {
    pub id: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecuteRequest {
    pub id: u64,
    pub program: String,
    pub arguments: Vec<String>,
    pub current_directory: String,
    pub environment: Vec<(String, String)>,
    pub timeout_millis: u64,
    pub max_output_bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_mirror: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream_stdout: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_mirror: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CancelRequest {
    pub id: u64,
    pub target_id: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlResponse {
    pub id: u64,
    pub error: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadFileResponse {
    pub id: u64,
    #[serde(default, with = "optional_wire_bytes")]
    pub contents: Option<Vec<u8>>,
    pub error: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryResponse {
    pub id: u64,
    pub total_kib: Option<u64>,
    pub minimum_available_kib: Option<u64>,
    pub oom_kills: u64,
    pub error: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecuteResponse {
    pub id: u64,
    pub exit_code: Option<i32>,
    #[serde(default, with = "optional_wire_bytes")]
    pub stdout: Option<Vec<u8>>,
    #[serde(default, with = "optional_wire_bytes")]
    pub stderr: Option<Vec<u8>>,
    pub error: Option<String>,
    pub timed_out: bool,
    pub output_limit_exceeded: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputChunk {
    pub id: u64,
    #[serde(with = "wire_bytes")]
    pub data: Vec<u8>,
}

mod wire_bytes {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(D::Error::custom)
    }
}

mod optional_wire_bytes {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    #[allow(
        clippy::ref_option,
        reason = "serde's `with` module contract passes the field by reference"
    )]
    pub fn serialize<S>(bytes: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match bytes {
            Some(bytes) => serializer.serialize_some(&STANDARD.encode(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)?
            .map(|encoded| STANDARD.decode(encoded).map_err(D::Error::custom))
            .transpose()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolRequest {
    pub id: u64,
    pub tool: GuestTool,
    pub input: WireToolInput,
    pub context: WireToolContext,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WireToolInput {
    Function { arguments: Box<RawValue> },
    Freeform { input: String },
}

impl From<ToolInput> for WireToolInput {
    fn from(input: ToolInput) -> Self {
        match input {
            ToolInput::Function(arguments) => Self::Function { arguments },
            ToolInput::Freeform(input) => Self::Freeform { input },
        }
    }
}

impl From<WireToolInput> for ToolInput {
    fn from(input: WireToolInput) -> Self {
        match input {
            WireToolInput::Function { arguments } => Self::Function(arguments),
            WireToolInput::Freeform { input } => Self::Freeform(input),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireToolContext {
    pub model: String,
    pub session_id: String,
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub output_token_budget: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolResponse {
    pub id: u64,
    pub execution: Option<ToolOutputWire>,
    pub error: Option<String>,
}

impl ToolResponse {
    #[cfg(any(feature = "guest-runtime", test))]
    pub const fn completed(id: u64, execution: ToolOutputWire) -> Self {
        Self {
            id,
            execution: Some(execution),
            error: None,
        }
    }

    #[cfg(any(feature = "guest-runtime", test))]
    pub const fn failed(id: u64, error: String) -> Self {
        Self {
            id,
            execution: None,
            error: Some(error),
        }
    }
}
