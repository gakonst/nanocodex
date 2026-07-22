use nanocodex_core::{CustomToolFormat, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::json;

const APPLY_PATCH_GRAMMAR: &str = include_str!("apply_patch/apply_patch.lark");

/// Stable identities and model-visible contracts for Nanocodex's standard tools.
///
/// Application-owned runtimes can reuse these definitions while implementing
/// their effects somewhere other than the host process, such as inside a VM.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StandardTool {
    ExecCommand,
    WriteStdin,
    UpdatePlan,
    ApplyPatch,
    ViewImage,
}

impl StandardTool {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ExecCommand => "exec_command",
            Self::WriteStdin => "write_stdin",
            Self::UpdatePlan => "update_plan",
            Self::ApplyPatch => "apply_patch",
            Self::ViewImage => "view_image",
        }
    }

    #[must_use]
    pub fn definition(self) -> ToolDefinition {
        match self {
            Self::ExecCommand => exec_command_definition(self.name()),
            Self::WriteStdin => write_stdin_definition(self.name()),
            Self::UpdatePlan => update_plan_definition(self.name()),
            Self::ApplyPatch => ToolDefinition::custom(
                self.name(),
                "Use the `apply_patch` tool to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.",
                CustomToolFormat::grammar("lark", APPLY_PATCH_GRAMMAR),
            ),
            Self::ViewImage => view_image_definition(self.name()),
        }
    }
}

fn exec_command_definition(name: &'static str) -> ToolDefinition {
    ToolDefinition::function(
        name,
        "Runs a shell command, returning output or a session ID for ongoing interaction. Live sessions are terminated when the agent ends; detach services that must remain running afterward.",
        json!({
            "type": "object",
            "properties": {
                "cmd": { "type": "string", "description": "Shell command to execute." },
                "workdir": {
                    "type": "string",
                    "description": "Working directory for the command. Defaults to the task workspace."
                },
                "shell": {
                    "type": "string",
                    "description": "Shell binary to launch. Defaults to the user's default shell."
                },
                "login": {
                    "type": "boolean",
                    "description": "True runs with login-shell semantics; false disables them. Defaults to true."
                },
                "tty": {
                    "type": "boolean",
                    "description": "True allocates a PTY for the command; false or omitted uses plain pipes."
                },
                "yield_time_ms": {
                    "type": "integer",
                    "description": "Wait before yielding output. Defaults to 10000 ms; effective range is 250-30000 ms."
                },
                "max_output_tokens": {
                    "type": "integer",
                    "description": "Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy."
                }
            },
            "required": ["cmd"],
            "additionalProperties": false
        }),
    )
    .with_output_schema(unified_exec_output_schema())
}

fn write_stdin_definition(name: &'static str) -> ToolDefinition {
    ToolDefinition::function(
        name,
        "Writes characters to an existing exec session and returns recent output.",
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "integer",
                    "description": "Identifier of the running exec session."
                },
                "chars": {
                    "type": "string",
                    "description": "Bytes to write to stdin. Defaults to empty, which polls without writing."
                },
                "yield_time_ms": {
                    "type": "integer",
                    "description": "Wait before yielding output. Non-empty writes default to 250 ms and cap at 30000 ms; empty polls wait 5000-300000 ms by default."
                },
                "max_output_tokens": {
                    "type": "integer",
                    "description": "Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy."
                }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
    )
    .with_output_schema(unified_exec_output_schema())
}

fn update_plan_definition(name: &'static str) -> ToolDefinition {
    ToolDefinition::function(
        name,
        "Updates the task plan.\nProvide an optional explanation and a list of plan items, each with a step and status.\nAt most one step can be in_progress at a time.\n",
        json!({
            "type": "object",
            "properties": {
                "explanation": {
                    "type": "string",
                    "description": "Optional explanation for this plan update."
                },
                "plan": {
                    "type": "array",
                    "description": "The list of steps",
                    "items": {
                        "type": "object",
                        "properties": {
                            "step": { "type": "string", "description": "Task step text." },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Step status."
                            }
                        },
                        "required": ["step", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["plan"],
            "additionalProperties": false
        }),
    )
}

fn view_image_definition(name: &'static str) -> ToolDefinition {
    ToolDefinition::function(
        name,
        "View a local image file from the filesystem when visual inspection is needed. Use this for images already available on disk.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Local filesystem path to an image file."
                },
                "detail": {
                    "type": "string",
                    "enum": ["high", "original"],
                    "description": "Image detail level. Defaults to `high`; use `original` to preserve exact resolution."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
    .with_output_schema(json!({
        "type": "object",
        "properties": {
            "image_url": {
                "type": "string",
                "description": "Data URL for the loaded image."
            },
            "detail": {
                "type": "string",
                "enum": ["high", "original"],
                "description": "Image detail hint returned by view_image."
            }
        },
        "required": ["image_url", "detail"],
        "additionalProperties": false
    }))
}

fn unified_exec_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "chunk_id": {
                "type": "string",
                "description": "Chunk identifier included when the response reports one."
            },
            "wall_time_seconds": {
                "type": "number",
                "description": "Elapsed wall time spent waiting for output in seconds."
            },
            "exit_code": {
                "type": "number",
                "description": "Process exit code when the command finished during this call."
            },
            "session_id": {
                "type": "number",
                "description": "Session identifier to pass to write_stdin when the process is still running."
            },
            "original_token_count": {
                "type": "number",
                "description": "Approximate token count before output truncation."
            },
            "output": {
                "type": "string",
                "description": "Command output text, possibly truncated."
            }
        },
        "required": ["wall_time_seconds", "output"],
        "additionalProperties": false
    })
}
