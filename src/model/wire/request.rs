use serde::Serialize;
use sha2::{Digest, Sha256};

use super::response::Caller;
use crate::{model::ModelConfig, protocol::Task, shell::ShellCommandOutput};

const BASE_INSTRUCTIONS: &str = r"You are a coding agent running non-interactively inside an isolated evaluation container.

Complete the user's task autonomously. Local shell access is available only through Programmatic Tool Calling: write hosted JavaScript that invokes tools.shell. Continue until the requested change is implemented and checked; do not merely explain what should be done. You have full permission inside the container and must not ask for approval.

<tool_orchestration>
Treat each generated JavaScript program as one bounded semantic phase. Within a phase, continue through every mechanically predictable step before emitting text. Return control to the model only when the next action requires semantic judgment, the phase is complete, or the phase cannot proceed safely.

Run independent read-only shell actions concurrently with Promise.all. Sequence dependent actions and all mutations. Never mutate the same workspace concurrently. Put sequential commands that share one timeout and output budget in a single shell action. If an expected command fails, gather the diagnostics needed to decide what to do next in the same program. After a successful mutation, run its mechanically determined verification in the same program.

Process and reduce intermediate results in JavaScript instead of forwarding every raw result. Emit one compact JSON result containing the phase status, relevant evidence, verification, and any failure that needs model judgment. Do not repeat completed calls. Retry a transient failure at most once.
</tool_orchestration>

Shell actions always run from the task workspace. Keep commands scoped to the task. When finished, give a concise summary of the changes and verification.";
const CACHE_PROFILE_VERSION: &str = "openai-coding-v2";
const TOOL_CATALOG_SIGNATURE: &str = "shell:local:programmatic;programmatic_tool_calling";
const PROGRAMMATIC_CALLER: [&str; 1] = ["programmatic"];

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
        hasher.update(TOOL_CATALOG_SIGNATURE.as_bytes());
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
    ShellCallOutput(ShellCallOutput),
}

#[derive(Serialize)]
pub(in crate::model) struct MessageInput {
    #[serde(rename = "type")]
    kind: &'static str,
    role: &'static str,
    content: [InputText; 1],
}

impl MessageInput {
    fn developer() -> Self {
        Self {
            kind: "message",
            role: "developer",
            content: [InputText::cached(BASE_INSTRUCTIONS)],
        }
    }

    fn user(task: &Task, workspace: &str) -> Self {
        Self {
            kind: "message",
            role: "user",
            content: [InputText::new(format!(
                "{}\n\n<environment_context>\n<cwd>{workspace}</cwd>\n<shell>/bin/sh</shell>\n</environment_context>",
                task.instruction
            ))],
        }
    }
}

#[derive(Serialize)]
pub(in crate::model) struct ShellCallOutput {
    #[serde(rename = "type")]
    kind: &'static str,
    call_id: String,
    max_output_length: u64,
    output: Vec<ShellCommandOutput>,
    caller: Caller,
}

impl ShellCallOutput {
    pub(in crate::model) fn new(
        call_id: String,
        max_output_length: u64,
        output: Vec<ShellCommandOutput>,
        caller: Caller,
    ) -> Self {
        Self {
            kind: "shell_call_output",
            call_id,
            max_output_length,
            output,
            caller,
        }
    }

    pub(in crate::model) fn call_id(&self) -> &str {
        &self.call_id
    }
}

impl From<ShellCallOutput> for InputItem {
    fn from(output: ShellCallOutput) -> Self {
        Self::ShellCallOutput(output)
    }
}

impl InputItem {
    pub(in crate::model) fn for_task(task: &Task, workspace: &str) -> Vec<Self> {
        vec![
            Self::Message(MessageInput::developer()),
            Self::Message(MessageInput::user(task, workspace)),
        ]
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
}

impl<'a> ResponseCreate<'a> {
    pub(in crate::model) fn warmup(
        config: &'a ModelConfig,
        input: &'a [InputItem],
        profile: &'a RequestProfile,
    ) -> Self {
        Self::new(config, input, None, Some(false), profile)
    }

    pub(in crate::model) fn generated(
        config: &'a ModelConfig,
        input: &'a [InputItem],
        previous_response_id: &'a str,
        profile: &'a RequestProfile,
    ) -> Self {
        Self::new(config, input, Some(previous_response_id), None, profile)
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
            tools: ToolDefinitions(SHELL_TOOL, PROGRAMMATIC_TOOL),
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

    fn cached(text: &'static str) -> Self {
        Self {
            kind: "input_text",
            text: text.to_owned(),
            prompt_cache_breakpoint: Some(CacheBreakpoint { mode: "explicit" }),
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
struct ShellTool {
    #[serde(rename = "type")]
    kind: &'static str,
    environment: LocalEnvironment,
    allowed_callers: [&'static str; 1],
}

#[derive(Clone, Copy, Serialize)]
struct LocalEnvironment {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct ToolDefinitions(ShellTool, ProgrammaticTool);

#[derive(Clone, Copy, Serialize)]
struct ProgrammaticTool {
    #[serde(rename = "type")]
    kind: &'static str,
}

const SHELL_TOOL: ShellTool = ShellTool {
    kind: "shell",
    environment: LocalEnvironment { kind: "local" },
    allowed_callers: PROGRAMMATIC_CALLER,
};

const PROGRAMMATIC_TOOL: ProgrammaticTool = ProgrammaticTool {
    kind: "programmatic_tool_calling",
};
