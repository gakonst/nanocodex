use serde::{Deserialize, Serialize};

use super::ModelConfig;
use crate::protocol::Task;

const BASE_INSTRUCTIONS: &str = r"You are a coding agent running non-interactively inside an isolated evaluation container.

Complete the user's task autonomously. All exec_command access is through Programmatic Tool Calling: write hosted JavaScript that invokes tools.exec_command. Continue until the requested change is implemented and checked; do not merely explain what should be done. You have full permission inside the container and must not ask for approval.

<tool_orchestration>
Treat each generated JavaScript program as one bounded semantic phase, not as a wrapper around one exec_command call. Within a phase, continue through every mechanically predictable step before emitting text. Return control to the model only when the next action requires semantic judgment, the phase is complete, or the phase cannot proceed safely.

Run independent read-only calls concurrently with Promise.all. Sequence dependent calls and all mutations. Never mutate the same workspace concurrently. When shell composition is sufficient, batch related commands into one exec_command. If an expected command fails, gather the diagnostics needed to decide what to do next in the same program. After a successful mutation, run its mechanically determined verification in the same program.

Use only documented tool input and output fields. Process and reduce intermediate results in JavaScript instead of forwarding every raw result. Emit one compact JSON result containing the phase status, relevant evidence, verification, and any failure that needs model judgment. Do not repeat completed calls. Retry a transient failure at most once.
</tool_orchestration>

Set workdir explicitly on every exec_command call. Keep commands scoped to the task. When finished, give a concise summary of the changes and verification.";
const PROMPT_CACHE_KEY: &str = "harness-openai-coding-v1";
const TRANSPORT_INCLUDE: [&str; 1] = ["reasoning.encrypted_content"];

#[derive(Serialize)]
#[serde(tag = "type")]
pub(super) enum InputItem {
    #[serde(rename = "message")]
    Message {
        role: &'static str,
        content: [InputText; 1],
    },
    #[serde(rename = "function_call_output")]
    FunctionCallOutput {
        call_id: String,
        output: String,
        caller: Caller,
    },
}

#[derive(Serialize)]
pub(super) struct ResponseCreate<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    model: &'a str,
    instructions: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    input: &'a [InputItem],
    tools: ToolDefinitions,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: ReasoningControls,
    store: bool,
    stream: bool,
    include: [&'static str; 1],
    prompt_cache_key: &'static str,
    text: TextControls,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Usage {
    pub(super) input_tokens: u64,
    pub(super) input_tokens_details: InputTokenDetails,
    pub(super) output_tokens: u64,
    pub(super) output_tokens_details: OutputTokenDetails,
    pub(super) total_tokens: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct InputTokenDetails {
    pub(super) cached_tokens: u64,
    pub(super) cache_write_tokens: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct OutputTokenDetails {
    pub(super) reasoning_tokens: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(super) enum ServerEvent {
    #[serde(rename = "response.created")]
    Created { response: CreatedResponse },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary.delta")]
    ReasoningSummaryDelta { delta: String },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta,
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { item: OutputItem },
    #[serde(rename = "response.completed")]
    Completed { response: CompletedResponse },
    #[serde(rename = "response.failed")]
    Failed,
    #[serde(rename = "response.incomplete")]
    Incomplete,
    #[serde(rename = "error")]
    Error,
    #[serde(other)]
    Other,
}

impl ServerEvent {
    pub(super) const fn is_output_delta(&self) -> bool {
        matches!(
            self,
            Self::OutputTextDelta { .. }
                | Self::ReasoningSummaryTextDelta { .. }
                | Self::ReasoningSummaryDelta { .. }
                | Self::FunctionCallArgumentsDelta
        )
    }
}

#[derive(Deserialize)]
pub(super) struct CreatedResponse {
    pub(super) id: String,
}

#[derive(Deserialize)]
pub(super) struct CompletedResponse {
    pub(super) id: String,
    pub(super) status: String,
    pub(super) output: Vec<OutputItem>,
    pub(super) usage: Usage,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(super) enum OutputItem {
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
        caller: Caller,
    },
    #[serde(rename = "message")]
    Message {
        #[serde(default)]
        content: Vec<OutputContent>,
    },
    #[serde(rename = "program")]
    Program,
    #[serde(rename = "program_output")]
    ProgramOutput,
    #[serde(other)]
    Other,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub(super) enum Caller {
    #[serde(rename = "program")]
    Program { caller_id: String },
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(super) enum OutputContent {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(other)]
    Other,
}

#[derive(Clone, Serialize)]
pub(super) struct InputText {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

#[derive(Clone, Copy, Serialize)]
struct ReasoningControls {
    effort: &'static str,
    summary: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct TextControls {
    verbosity: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct FunctionTool {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'static str,
    description: &'static str,
    strict: bool,
    parameters: ParametersSchema,
    output_schema: OutputSchema,
    allowed_callers: [&'static str; 1],
}

#[derive(Clone, Copy, Serialize)]
struct ToolDefinitions(FunctionTool, ProgrammaticTool);

#[derive(Clone, Copy, Serialize)]
struct ProgrammaticTool {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct ParametersSchema {
    #[serde(rename = "type")]
    kind: &'static str,
    properties: ExecProperties,
    required: [&'static str; 1],
    #[serde(rename = "additionalProperties")]
    additional_properties: bool,
}

#[derive(Clone, Copy, Serialize)]
struct ExecProperties {
    cmd: PropertySchema,
    workdir: PropertySchema,
    tty: PropertySchema,
    yield_time_ms: PropertySchema,
    max_output_tokens: PropertySchema,
    shell: PropertySchema,
}

#[derive(Clone, Copy, Serialize)]
struct OutputSchema {
    #[serde(rename = "type")]
    kind: &'static str,
    properties: OutputProperties,
    required: [&'static str; 2],
    #[serde(rename = "additionalProperties")]
    additional_properties: bool,
}

#[derive(Clone, Copy, Serialize)]
struct OutputProperties {
    wall_time_seconds: PropertySchema,
    exit_code: PropertySchema,
    original_token_count: PropertySchema,
    output: PropertySchema,
}

#[derive(Clone, Copy, Serialize)]
struct PropertySchema {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'static str>,
}

const EXEC_COMMAND_TOOL: FunctionTool = FunctionTool {
    kind: "function",
    name: "exec_command",
    description: "Runs a shell command using plain pipes and returns when it exits. This is the completion path of Codex's exec_command tool; interactive sessions are not exposed in this milestone.",
    strict: false,
    parameters: ParametersSchema {
        kind: "object",
        properties: ExecProperties {
            cmd: property("string", "Shell command to execute."),
            workdir: property(
                "string",
                "Working directory for the command. Defaults to the task workspace.",
            ),
            tty: property(
                "boolean",
                "Must be false or omitted; this milestone uses plain pipes.",
            ),
            yield_time_ms: property(
                "number",
                "Accepted for compatibility with Codex's exec_command shape; completion-only commands wait until exit.",
            ),
            max_output_tokens: property(
                "number",
                "Output token budget. Defaults to 10000 tokens and is capped at 10000.",
            ),
            shell: property("string", "Shell binary to launch. Defaults to /bin/sh."),
        },
        required: ["cmd"],
        additional_properties: false,
    },
    output_schema: OutputSchema {
        kind: "object",
        properties: OutputProperties {
            wall_time_seconds: bare_property("number"),
            exit_code: bare_property("number"),
            original_token_count: bare_property("number"),
            output: bare_property("string"),
        },
        required: ["wall_time_seconds", "output"],
        additional_properties: false,
    },
    allowed_callers: ["programmatic"],
};

const PROGRAMMATIC_TOOL: ProgrammaticTool = ProgrammaticTool {
    kind: "programmatic_tool_calling",
};

pub(super) fn initial_input(task: &Task, workspace: &str) -> Vec<InputItem> {
    vec![InputItem::Message {
        role: "user",
        content: [InputText {
            kind: "input_text",
            text: format!(
                "{}\n\n<environment_context>\n<cwd>{workspace}</cwd>\n<shell>/bin/sh</shell>\n</environment_context>",
                task.instruction
            ),
        }],
    }]
}

pub(super) fn function_call_output(call_id: String, output: String, caller: Caller) -> InputItem {
    InputItem::FunctionCallOutput {
        call_id,
        output,
        caller,
    }
}

pub(super) fn response_create<'a>(
    config: &'a ModelConfig,
    input: &'a [InputItem],
    previous_response_id: Option<&'a str>,
) -> ResponseCreate<'a> {
    ResponseCreate {
        kind: "response.create",
        model: &config.model,
        instructions: BASE_INSTRUCTIONS,
        previous_response_id,
        input,
        tools: ToolDefinitions(EXEC_COMMAND_TOOL, PROGRAMMATIC_TOOL),
        tool_choice: "auto",
        parallel_tool_calls: true,
        reasoning: ReasoningControls {
            effort: config.effort.as_str(),
            summary: "auto",
        },
        store: false,
        stream: true,
        include: TRANSPORT_INCLUDE,
        prompt_cache_key: PROMPT_CACHE_KEY,
        text: TextControls { verbosity: "low" },
    }
}

const fn property(kind: &'static str, description: &'static str) -> PropertySchema {
    PropertySchema {
        kind,
        description: Some(description),
    }
}

const fn bare_property(kind: &'static str) -> PropertySchema {
    PropertySchema {
        kind,
        description: None,
    }
}
