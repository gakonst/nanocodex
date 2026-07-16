use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::response::Caller;
use crate::{
    model::{MAX_CONCURRENT_SUBAGENTS, ModelConfig},
    protocol::Task,
    shell::ExecCommandResult,
};

const BASE_INSTRUCTIONS: &str = r"You are a coding agent running non-interactively inside an isolated evaluation container.

Complete the user's task autonomously. Continue until the requested change is implemented and checked; do not merely explain what should be done. You have full permission inside the container and must not ask for approval.

Preserve existing functionality and user-visible behavior unless the task explicitly requires a change. Inspect relevant files before editing. Do not weaken, delete, or bypass required behavior merely to make a check pass. Before finishing, run the most relevant available build, tests, or focused smoke checks and report concrete verification evidence. Exercise requested behavior at its real external boundary: when a task names signals, cancellation, process cleanup, concurrency limits, retries, or queued work, test the relevant boundary and combinations instead of only an internal happy-path approximation.

Follow repository instructions and existing project conventions. Preserve unrelated dirty work and keep edits scoped to the request. Prefer existing dependencies, scripts, and abstractions over one-off replacements. Search before assuming where behavior lives, read enough surrounding code to understand the data flow, and fix causes rather than symptoms. Treat command failures as evidence: inspect their output, adjust deliberately, and re-run the narrowest meaningful check. Do not modify benchmark tasks or verifier logic to manufacture success. Never print credentials or environment-file contents. Keep generated artifacts, caches, and build output out of source control.

<tool_orchestration>
Treat each tool phase as one bounded semantic unit. Continue through mechanically predictable steps before emitting text. Return control to the model only when the next action requires semantic judgment, the phase is complete, or the phase cannot proceed safely.

Sequence dependent actions and all mutations. Never mutate the same workspace concurrently. Put sequential commands that share one timeout and output budget in a single cmd. If an expected command fails, gather the diagnostics needed to decide what to do next in the same phase. After a successful mutation, run its mechanically determined verification in the same phase. Do not repeat completed calls. Retry a transient failure at most once.
</tool_orchestration>

exec_command defaults to the task workspace. Keep commands scoped to the task. When finished, give a concise summary of the changes and verification.";
const PTC_INSTRUCTIONS: &str = r"Programmatic Tool Calling is active. Use hosted JavaScript to invoke tools.exec_command. Treat each generated program as one bounded phase, use Promise.all for independent read-only calls, and reduce intermediate output in JavaScript before returning compact evidence to the model.";
const MULTI_AGENT_INSTRUCTIONS: &str = r"Hosted Multi-agent is active. Call exec_command directly so the client can inject each result into the waiting agent. Do not spawn subagents for routine or sequential work. Spawn them only when the user explicitly requests delegation, or when a genuinely difficult task has independent workstreams whose parallel execution would materially improve speed or quality. Run independent read-only calls in parallel, but never perform concurrent mutations in the shared workspace.";
const CACHE_PROFILE_VERSION: &str = "openai-coding-v7";

pub(in crate::model) struct RequestProfile {
    prompt_cache_key: String,
}

impl RequestProfile {
    pub(in crate::model) fn new(config: &ModelConfig) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(config.model.as_bytes());
        hasher.update([0]);
        hasher.update(CACHE_PROFILE_VERSION.as_bytes());
        hasher.update([0]);
        hasher.update(BASE_INSTRUCTIONS.as_bytes());
        hasher.update([0]);
        hasher.update(mode_instructions(config).as_bytes());
        hasher.update([0]);
        hasher.update(tool_catalog_signature(config).as_bytes());
        let mut digest = format!("{:x}", hasher.finalize());
        digest.truncate(48);
        Self {
            prompt_cache_key: format!("harness:{digest}"),
        }
    }

    pub(in crate::model) fn prompt_cache_key(&self) -> &str {
        &self.prompt_cache_key
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub(in crate::model) enum InputItem {
    Message(MessageInput),
    FunctionCallOutput(FunctionCallOutput),
}

#[derive(Serialize)]
pub(in crate::model) struct MessageInput {
    #[serde(rename = "type")]
    kind: &'static str,
    role: &'static str,
    content: Vec<InputText>,
}

impl MessageInput {
    fn developer(config: &ModelConfig) -> Self {
        Self {
            kind: "message",
            role: "developer",
            content: vec![
                InputText::stable(BASE_INSTRUCTIONS, false),
                InputText::stable(mode_instructions(config), true),
            ],
        }
    }

    fn user(task: &Task, workspace: &str) -> Self {
        Self {
            kind: "message",
            role: "user",
            content: vec![InputText::new(format!(
                "{}\n\n<environment_context>\n<cwd>{workspace}</cwd>\n<shell>/bin/sh</shell>\n</environment_context>",
                task.instruction
            ))],
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::model) struct FunctionCallOutput {
    #[serde(rename = "type")]
    kind: FunctionCallOutputKind,
    call_id: String,
    output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    caller: Option<Caller>,
}

impl FunctionCallOutput {
    pub(in crate::model) fn new(
        call_id: String,
        result: &ExecCommandResult,
        caller: Option<Caller>,
    ) -> serde_json::Result<Self> {
        Ok(Self {
            kind: FunctionCallOutputKind::FunctionCallOutput,
            call_id,
            output: serde_json::to_string(result)?,
            caller,
        })
    }

    pub(in crate::model) fn call_id(&self) -> &str {
        &self.call_id
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
enum FunctionCallOutputKind {
    #[serde(rename = "function_call_output")]
    FunctionCallOutput,
}

impl From<FunctionCallOutput> for InputItem {
    fn from(output: FunctionCallOutput) -> Self {
        Self::FunctionCallOutput(output)
    }
}

impl InputItem {
    pub(in crate::model) fn for_task(
        task: &Task,
        workspace: &str,
        config: &ModelConfig,
    ) -> Vec<Self> {
        vec![
            Self::Message(MessageInput::developer(config)),
            Self::Message(MessageInput::user(task, workspace)),
        ]
    }
}

fn mode_instructions(config: &ModelConfig) -> &'static str {
    if config.multi_agent {
        MULTI_AGENT_INSTRUCTIONS
    } else {
        PTC_INSTRUCTIONS
    }
}

fn tool_catalog_signature(config: &ModelConfig) -> &'static str {
    if config.multi_agent {
        "exec_command:function:direct;multi_agent:hosted:max3"
    } else {
        "exec_command:function:programmatic;programmatic_tool_calling"
    }
}

#[derive(Serialize)]
pub(in crate::model) struct ResponseCreate<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    input: &'a [InputItem],
    tools: ToolDefinitions,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: ReasoningControls,
    context_management: [CompactionControl; 1],
    store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    generate: Option<bool>,
    prompt_cache_key: &'a str,
    prompt_cache_options: PromptCacheOptions,
    text: TextControls,
    #[serde(skip_serializing_if = "Option::is_none")]
    multi_agent: Option<MultiAgentControls>,
}

impl<'a> ResponseCreate<'a> {
    pub(in crate::model) fn warmup(
        config: &'a ModelConfig,
        input: &'a [InputItem],
        profile: &'a RequestProfile,
    ) -> Self {
        Self::new(config, input, None, Some(false), profile)
    }

    pub(in crate::model) fn initial(
        config: &'a ModelConfig,
        input: &'a [InputItem],
        profile: &'a RequestProfile,
    ) -> Self {
        Self::new(config, input, None, Some(true), profile)
    }

    pub(in crate::model) fn continued(
        config: &'a ModelConfig,
        input: &'a [InputItem],
        previous_response_id: &'a str,
        profile: &'a RequestProfile,
    ) -> Self {
        Self::new(
            config,
            input,
            Some(previous_response_id),
            Some(true),
            profile,
        )
    }

    fn new(
        config: &'a ModelConfig,
        input: &'a [InputItem],
        previous_response_id: Option<&'a str>,
        generate: Option<bool>,
        profile: &'a RequestProfile,
    ) -> Self {
        Self {
            kind: "response.create",
            model: &config.model,
            previous_response_id,
            input,
            tools: ToolDefinitions::new(config),
            tool_choice: "auto",
            parallel_tool_calls: true,
            reasoning: ReasoningControls {
                effort: config.effort.as_str(),
                mode: "standard",
                context: "all_turns",
            },
            context_management: [CompactionControl {
                kind: "compaction",
                compact_threshold: config.compact_threshold,
            }],
            store: true,
            generate,
            prompt_cache_key: profile.prompt_cache_key(),
            prompt_cache_options: PromptCacheOptions {
                mode: "explicit",
                ttl: "30m",
            },
            text: TextControls { verbosity: "low" },
            multi_agent: config.multi_agent.then_some(MultiAgentControls {
                enabled: true,
                max_concurrent_subagents: MAX_CONCURRENT_SUBAGENTS,
            }),
        }
    }
}

#[derive(Serialize)]
pub(in crate::model) struct ResponseInject<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    response_id: &'a str,
    input: &'a [FunctionCallOutput],
}

impl<'a> ResponseInject<'a> {
    pub(in crate::model) const fn new(
        response_id: &'a str,
        input: &'a [FunctionCallOutput],
    ) -> Self {
        Self {
            kind: "response.inject",
            response_id,
            input,
        }
    }
}

#[derive(Clone, Serialize)]
struct InputText {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_breakpoint: Option<CacheBreakpoint>,
}

impl InputText {
    fn new(text: String) -> Self {
        Self {
            kind: "input_text",
            text,
            prompt_cache_breakpoint: None,
        }
    }

    fn stable(text: &'static str, breakpoint: bool) -> Self {
        Self {
            kind: "input_text",
            text: text.to_owned(),
            prompt_cache_breakpoint: breakpoint.then_some(CacheBreakpoint { mode: "explicit" }),
        }
    }
}

#[derive(Clone, Copy, Serialize)]
struct CacheBreakpoint {
    mode: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct PromptCacheOptions {
    mode: &'static str,
    ttl: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct ReasoningControls {
    effort: &'static str,
    mode: &'static str,
    context: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct CompactionControl {
    #[serde(rename = "type")]
    kind: &'static str,
    compact_threshold: u64,
}

#[derive(Clone, Copy, Serialize)]
struct TextControls {
    verbosity: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct MultiAgentControls {
    enabled: bool,
    max_concurrent_subagents: u32,
}

#[derive(Clone, Copy, Serialize)]
struct ExecCommandTool {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'static str,
    description: &'static str,
    strict: bool,
    parameters: ExecCommandParameters,
    output_schema: ExecCommandOutputSchema,
    allowed_callers: [AllowedCaller; 1],
}

impl ExecCommandTool {
    fn new(caller: AllowedCaller) -> Self {
        Self {
            allowed_callers: [caller],
            ..EXEC_COMMAND_TOOL
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum AllowedCaller {
    Direct,
    Programmatic,
}

#[derive(Clone, Copy, Serialize)]
struct ExecCommandParameters {
    #[serde(rename = "type")]
    kind: &'static str,
    properties: ExecCommandProperties,
    required: [&'static str; 1],
    #[serde(rename = "additionalProperties")]
    additional_properties: bool,
}

#[derive(Clone, Copy, Serialize)]
struct ExecCommandProperties {
    cmd: SchemaProperty,
    workdir: SchemaProperty,
    login: SchemaProperty,
    max_output_tokens: SchemaProperty,
    timeout_ms: SchemaProperty,
}

#[derive(Clone, Copy, Serialize)]
struct ExecCommandOutputSchema {
    #[serde(rename = "type")]
    kind: &'static str,
    properties: ExecCommandOutputProperties,
    required: [&'static str; 2],
    #[serde(rename = "additionalProperties")]
    additional_properties: bool,
}

#[derive(Clone, Copy, Serialize)]
struct ExecCommandOutputProperties {
    wall_time_seconds: SchemaProperty,
    exit_code: SchemaProperty,
    output: SchemaProperty,
}

#[derive(Clone, Copy, Serialize)]
struct SchemaProperty {
    #[serde(rename = "type")]
    kind: &'static str,
    description: &'static str,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ToolDefinition {
    ExecCommand(Box<ExecCommandTool>),
    Programmatic(ProgrammaticTool),
}

#[derive(Serialize)]
struct ToolDefinitions(Vec<ToolDefinition>);

impl ToolDefinitions {
    fn new(config: &ModelConfig) -> Self {
        if config.multi_agent {
            Self(vec![ToolDefinition::ExecCommand(Box::new(
                ExecCommandTool::new(AllowedCaller::Direct),
            ))])
        } else {
            Self(vec![
                ToolDefinition::ExecCommand(Box::new(ExecCommandTool::new(
                    AllowedCaller::Programmatic,
                ))),
                ToolDefinition::Programmatic(PROGRAMMATIC_TOOL),
            ])
        }
    }
}

#[derive(Clone, Copy, Serialize)]
struct ProgrammaticTool {
    #[serde(rename = "type")]
    kind: &'static str,
}

const EXEC_COMMAND_TOOL: ExecCommandTool = ExecCommandTool {
    kind: "function",
    name: "exec_command",
    description: "Runs a shell command to completion, returning bounded output and timing.",
    strict: false,
    parameters: ExecCommandParameters {
        kind: "object",
        properties: ExecCommandProperties {
            cmd: SchemaProperty {
                kind: "string",
                description: "Shell command to execute.",
            },
            workdir: SchemaProperty {
                kind: "string",
                description: "Working directory for the command. Defaults to the task workspace.",
            },
            login: SchemaProperty {
                kind: "boolean",
                description: "True runs with login-shell semantics; false disables them. Defaults to true.",
            },
            max_output_tokens: SchemaProperty {
                kind: "integer",
                description: "Approximate output token budget. Defaults to 1024 tokens.",
            },
            timeout_ms: SchemaProperty {
                kind: "integer",
                description: "Maximum command runtime. Defaults to 120000 ms.",
            },
        },
        required: ["cmd"],
        additional_properties: false,
    },
    output_schema: ExecCommandOutputSchema {
        kind: "object",
        properties: ExecCommandOutputProperties {
            wall_time_seconds: SchemaProperty {
                kind: "number",
                description: "Elapsed wall time spent executing the command.",
            },
            exit_code: SchemaProperty {
                kind: "integer",
                description: "Process exit code, omitted when the command timed out.",
            },
            output: SchemaProperty {
                kind: "string",
                description: "Combined stdout and stderr, possibly truncated.",
            },
        },
        required: ["wall_time_seconds", "output"],
        additional_properties: false,
    },
    allowed_callers: [AllowedCaller::Direct],
};

const PROGRAMMATIC_TOOL: ProgrammaticTool = ProgrammaticTool {
    kind: "programmatic_tool_calling",
};
