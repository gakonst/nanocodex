use nanocodex_tools::{ToolInput, contract::ToolOutputWire, standard::StandardTool};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub(crate) enum SessionRequest {
    Ready(ReadyRequest),
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
    Tool(ToolResponse),
    WriteFile(ControlResponse),
    CreateDirectory(ControlResponse),
    ReadFile(ReadFileResponse),
    Memory(MemoryResponse),
    Execute(ExecuteResponse),
    Cancel(ControlResponse),
    TerminateToolProcesses(ControlResponse),
    Shutdown(ControlResponse),
}

impl SessionResponse {
    pub const fn id(&self) -> u64 {
        match self {
            Self::Ready(response) => response.id,
            Self::Tool(response) => response.id,
            Self::WriteFile(response)
            | Self::CreateDirectory(response)
            | Self::Cancel(response)
            | Self::TerminateToolProcesses(response)
            | Self::Shutdown(response) => response.id,
            Self::ReadFile(response) => response.id,
            Self::Memory(response) => response.id,
            Self::Execute(response) => response.id,
        }
    }
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
    pub tool: StandardTool,
    pub input: WireToolInput,
    pub context: WireToolContext,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WireToolInput {
    Function { arguments: Box<RawValue> },
    Freeform { input: String },
}

#[cfg(test)]
mod tests {
    use nanocodex_tools::{ToolInput, ToolOutput, standard::StandardTool};
    use serde_json::{json, value::to_raw_value};

    use super::{
        CancelRequest, ControlResponse, CreateDirectoryRequest, ExecuteRequest, ExecuteResponse,
        MemoryRequest, MemoryResponse, ReadFileRequest, ReadFileResponse, ReadyRequest,
        SessionRequest, SessionResponse, ShutdownRequest, TerminateToolProcessesRequest,
        ToolRequest, ToolResponse, WireToolContext, WireToolInput, WriteFileRequest,
    };

    #[test]
    fn readiness_request_has_a_stable_typed_shape() {
        let request = SessionRequest::Ready(ReadyRequest { id: 4 });
        let encoded = serde_json::to_string(&request).unwrap();

        assert_eq!(encoded, r#"{"kind":"ready","payload":{"id":4}}"#);
    }

    #[test]
    fn shutdown_request_has_a_stable_typed_shape() {
        let request = SessionRequest::Shutdown(ShutdownRequest { id: 9 });
        let encoded = serde_json::to_string(&request).unwrap();

        assert_eq!(encoded, r#"{"kind":"shutdown","payload":{"id":9}}"#);
    }

    #[test]
    fn tool_process_termination_has_a_stable_typed_shape() {
        let request =
            SessionRequest::TerminateToolProcesses(TerminateToolProcessesRequest { id: 8 });
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"kind":"terminate_tool_processes","payload":{"id":8}}"#
        );

        let response =
            SessionResponse::TerminateToolProcesses(ControlResponse { id: 8, error: None });
        assert_eq!(
            serde_json::to_string(&response).unwrap(),
            r#"{"kind":"terminate_tool_processes","payload":{"id":8,"error":null}}"#
        );
    }

    #[test]
    fn function_request_round_trips_opaque_arguments() {
        let request = ToolRequest {
            id: 7,
            tool: StandardTool::ExecCommand,
            input: WireToolInput::from(ToolInput::Function(
                to_raw_value(&json!({"cmd": "pwd"})).unwrap(),
            )),
            context: WireToolContext {
                model: "model".to_owned(),
                session_id: "session".to_owned(),
                call_id: "call".to_owned(),
                output_token_budget: 100,
            },
        };
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded = serde_json::from_str::<ToolRequest>(&encoded).unwrap();
        let ToolInput::Function(arguments) = ToolInput::from(decoded.input) else {
            panic!("function input changed variants");
        };
        assert_eq!(arguments.get(), r#"{"cmd":"pwd"}"#);
    }

    #[test]
    fn tool_request_and_response_have_stable_typed_shapes() {
        let request = SessionRequest::Tool(ToolRequest {
            id: 1,
            tool: StandardTool::ExecCommand,
            input: WireToolInput::from(ToolInput::Function(
                to_raw_value(&json!({"cmd": "pwd"})).unwrap(),
            )),
            context: WireToolContext {
                model: "gpt-5.6".to_owned(),
                session_id: "session-1".to_owned(),
                call_id: "call-1".to_owned(),
                output_token_budget: 10_000,
            },
        });
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"kind":"tool","payload":{"id":1,"tool":"exec_command","input":{"function":{"arguments":{"cmd":"pwd"}}},"context":{"model":"gpt-5.6","session_id":"session-1","call_id":"call-1","output_token_budget":10000}}}"#
        );

        let response = SessionResponse::Tool(ToolResponse::completed(
            1,
            ToolOutput::text("/app\n")
                .with_process_trace(Some(0), None, None, 5, 0.01)
                .into_wire()
                .unwrap(),
        ));
        assert_eq!(
            serde_json::to_string(&response).unwrap(),
            r#"{"kind":"tool","payload":{"id":1,"execution":{"output":"/app\n","success":true,"structured_result":null,"metadata":null,"process_trace":{"exit_code":0,"session_id":null,"original_token_count":null,"output_bytes":5,"wall_time_seconds":0.01}},"error":null}}"#
        );
    }

    #[test]
    fn execution_response_round_trips_opaque_values() {
        let response = ToolResponse {
            id: 8,
            execution: Some(
                ToolOutput::from_json(json!({"output": "ok"}), true)
                    .into_wire()
                    .unwrap(),
            ),
            error: None,
        };
        let encoded = serde_json::to_string(&response).unwrap();
        let decoded = serde_json::from_str::<ToolResponse>(&encoded).unwrap();
        assert_eq!(decoded.id, 8);
    }

    #[test]
    fn binary_control_payloads_use_bounded_base64_strings() {
        let request = SessionRequest::WriteFile(WriteFileRequest {
            id: 4,
            path: "/tmp/output".to_owned(),
            contents: vec![0, 127, 128, 255],
            mode: 0o600,
            modified_unix_seconds: None,
        });
        let encoded = serde_json::to_string(&request).unwrap();

        assert!(encoded.contains(r#""contents":"AH+A/w==""#));
        assert!(!encoded.contains(r#""contents":[0,127,128,255]"#));
        assert!(!encoded.contains("modified_unix_seconds"));
        let decoded = serde_json::from_str::<SessionRequest>(&encoded).unwrap();
        let SessionRequest::WriteFile(decoded) = decoded else {
            panic!("write-file request changed variants");
        };
        assert_eq!(decoded.contents, [0, 127, 128, 255]);
        assert_eq!(decoded.modified_unix_seconds, None);
    }

    #[test]
    fn filesystem_metadata_requests_have_stable_typed_shapes() {
        let write = SessionRequest::WriteFile(WriteFileRequest {
            id: 4,
            path: "/tmp/output".to_owned(),
            contents: Vec::new(),
            mode: 0o640,
            modified_unix_seconds: Some(0),
        });
        assert_eq!(
            serde_json::to_string(&write).unwrap(),
            r#"{"kind":"write_file","payload":{"id":4,"path":"/tmp/output","contents":"","mode":416,"modified_unix_seconds":0}}"#
        );

        let directory = SessionRequest::CreateDirectory(CreateDirectoryRequest {
            id: 5,
            path: "/tmp/output-dir".to_owned(),
            mode: 0o750,
            modified_unix_seconds: Some(0),
        });
        let encoded = serde_json::to_string(&directory).unwrap();
        assert_eq!(
            encoded,
            r#"{"kind":"create_directory","payload":{"id":5,"path":"/tmp/output-dir","mode":488,"modified_unix_seconds":0}}"#
        );
        let decoded = serde_json::from_str::<SessionRequest>(&encoded).unwrap();
        let SessionRequest::CreateDirectory(decoded) = decoded else {
            panic!("create-directory request changed variants");
        };
        assert_eq!(decoded.mode, 0o750);
        assert_eq!(decoded.modified_unix_seconds, Some(0));
    }

    #[test]
    fn host_control_envelopes_have_stable_typed_shapes() {
        let read = SessionRequest::ReadFile(ReadFileRequest {
            id: 4,
            path: "/tmp/results/out.txt".to_owned(),
        });
        assert_eq!(
            serde_json::to_string(&read).unwrap(),
            r#"{"kind":"read_file","payload":{"id":4,"path":"/tmp/results/out.txt"}}"#
        );
        let read = SessionResponse::ReadFile(ReadFileResponse {
            id: 4,
            contents: Some(b"ok\n".to_vec()),
            error: None,
        });
        assert_eq!(
            serde_json::to_string(&read).unwrap(),
            r#"{"kind":"read_file","payload":{"id":4,"contents":"b2sK","error":null}}"#
        );

        let execute = SessionRequest::Execute(ExecuteRequest {
            id: 5,
            program: "/bin/sh".to_owned(),
            arguments: vec!["-lc".to_owned(), "printf ok".to_owned()],
            current_directory: "/app".to_owned(),
            environment: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
            timeout_millis: 60_000,
            max_output_bytes: 8_388_608,
            stdout_mirror: None,
            stderr_mirror: None,
        });
        assert_eq!(
            serde_json::to_string(&execute).unwrap(),
            r#"{"kind":"execute","payload":{"id":5,"program":"/bin/sh","arguments":["-lc","printf ok"],"current_directory":"/app","environment":[["PATH","/usr/bin:/bin"]],"timeout_millis":60000,"max_output_bytes":8388608}}"#
        );
        let execute = SessionResponse::Execute(ExecuteResponse {
            id: 5,
            exit_code: Some(0),
            stdout: Some(b"ok".to_vec()),
            stderr: Some(Vec::new()),
            error: None,
            timed_out: false,
            output_limit_exceeded: false,
        });
        assert_eq!(
            serde_json::to_string(&execute).unwrap(),
            r#"{"kind":"execute","payload":{"id":5,"exit_code":0,"stdout":"b2s=","stderr":"","error":null,"timed_out":false,"output_limit_exceeded":false}}"#
        );

        let cancel = SessionRequest::Cancel(CancelRequest {
            id: 6,
            target_id: 5,
        });
        assert_eq!(
            serde_json::to_string(&cancel).unwrap(),
            r#"{"kind":"cancel","payload":{"id":6,"target_id":5}}"#
        );
        let cancel = SessionResponse::Cancel(ControlResponse { id: 6, error: None });
        assert_eq!(
            serde_json::to_string(&cancel).unwrap(),
            r#"{"kind":"cancel","payload":{"id":6,"error":null}}"#
        );

        let memory = SessionRequest::Memory(MemoryRequest { id: 7 });
        assert_eq!(
            serde_json::to_string(&memory).unwrap(),
            r#"{"kind":"memory","payload":{"id":7}}"#
        );
        let memory = SessionResponse::Memory(MemoryResponse {
            id: 7,
            total_kib: Some(524_288),
            minimum_available_kib: Some(131_072),
            oom_kills: 1,
            error: None,
        });
        assert_eq!(
            serde_json::to_string(&memory).unwrap(),
            r#"{"kind":"memory","payload":{"id":7,"total_kib":524288,"minimum_available_kib":131072,"oom_kills":1,"error":null}}"#
        );
    }
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
