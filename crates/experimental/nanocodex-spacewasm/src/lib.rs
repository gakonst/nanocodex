//! Synchronous, host-stepped Nanocodex execution for SpaceWasm.
//!
//! SpaceWasm deliberately exposes a fixed, synchronous host boundary. This
//! crate keeps the typed transcript and tool loop inside the guest while the
//! embedding host owns model transport, credentials, effects, storage, and
//! scheduling. The JSONL binary is one WASI Preview 1 adapter for this core.

use std::collections::{BTreeMap, BTreeSet};

use nanocodex_oai_api::responses::{
    ContentItem, FunctionOutputBody, MessageRole, ResponseItem, ResponseToolCallRef, ToolDefinition,
};
use serde::{Deserialize, Serialize};

const SNAPSHOT_VERSION: u32 = 1;

/// One host command accepted by the flight core.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    /// Initializes a fresh guest with immutable instructions and tool declarations.
    Init {
        /// Revision observed by the host before applying this command.
        expected_revision: u64,
        /// Stable developer instructions.
        instructions: String,
        /// Exact tools the host can execute.
        #[serde(default)]
        tools: Vec<ToolDefinition>,
    },
    /// Adds one user turn and requests a model call from the host.
    Prompt {
        /// Revision observed by the host before applying this command.
        expected_revision: u64,
        /// User text.
        text: String,
    },
    /// Returns one completed model output to the guest.
    ModelOutput {
        /// Revision observed by the host before applying this command.
        expected_revision: u64,
        /// Complete typed output items from one successful Responses call.
        items: Vec<ResponseItem>,
    },
    /// Returns one completed host tool effect to the guest.
    ToolOutput {
        /// Revision observed by the host before applying this command.
        expected_revision: u64,
        /// Exact provider call identity.
        call_id: String,
        /// Tool output returned to the model.
        output: FunctionOutputBody,
    },
    /// Reads a complete durable checkpoint without changing state.
    Snapshot,
    /// Replaces guest state from a previously returned checkpoint.
    Restore {
        /// Complete checkpoint.
        snapshot: Snapshot,
    },
    /// Ends the JSONL adapter cleanly.
    Shutdown,
}

/// One guest response for the embedding host.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostAction {
    /// The guest accepted initialization or restoration.
    Ready {
        /// Current guest revision.
        revision: u64,
    },
    /// The host must execute one Responses call using this authoritative input.
    ModelRequest {
        /// Current guest revision.
        revision: u64,
        /// Complete typed input, including stable prefix and committed history.
        input: Vec<ResponseItem>,
    },
    /// The host must execute every listed tool call and return each output.
    ToolCalls {
        /// Current guest revision.
        revision: u64,
        /// Pending calls in provider order.
        calls: Vec<HostToolCall>,
    },
    /// One tool result was accepted while other dispatched calls remain pending.
    AwaitingToolOutputs {
        /// Current guest revision.
        revision: u64,
        /// Number of dispatched tool calls still awaiting results.
        remaining: usize,
    },
    /// One agent turn completed without pending tools.
    Complete {
        /// Current guest revision.
        revision: u64,
        /// Terminal model output.
        items: Vec<ResponseItem>,
    },
    /// Complete durable state.
    Snapshot {
        /// Current checkpoint.
        snapshot: Snapshot,
    },
    /// The guest rejected a command without changing its state.
    Error {
        /// Current guest revision.
        revision: u64,
        /// Stable machine-readable category.
        code: ErrorCode,
        /// Human-readable detail safe for logs.
        detail: String,
    },
    /// The adapter should stop reading commands.
    Shutdown {
        /// Final guest revision.
        revision: u64,
    },
}

/// A model-requested effect that the embedding host must execute.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostToolCall {
    /// Function or custom tool kind.
    pub kind: ToolCallKind,
    /// Tool name.
    pub name: String,
    /// Optional Responses namespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// JSON function arguments or free-form custom input.
    pub input: String,
    /// Provider call identity.
    pub call_id: String,
}

/// Supported model tool-call kinds.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    /// JSON-schema function call.
    Function,
    /// Grammar-constrained custom tool call.
    Custom,
}

/// Stable command rejection categories.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The host used a stale or future revision.
    RevisionConflict,
    /// The command is invalid in the current phase.
    InvalidPhase,
    /// The guest has not been initialized.
    NotInitialized,
    /// The model returned no output items.
    EmptyModelOutput,
    /// Two pending calls used the same identity.
    DuplicateToolCall,
    /// A tool result did not match a pending call.
    UnknownToolCall,
    /// The model requested a tool outside the initialized declaration set.
    UndeclaredToolCall,
}

/// Complete serializable guest checkpoint.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Snapshot {
    version: u32,
    revision: u64,
    initialized: bool,
    instructions: String,
    tools: Vec<ToolDefinition>,
    history: Vec<ResponseItem>,
    phase: Phase,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Phase {
    #[default]
    Idle,
    AwaitingModel,
    AwaitingTools {
        pending: BTreeMap<String, PendingTool>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PendingTool {
    call: HostToolCall,
}

/// Deterministic, host-stepped Nanocodex guest state.
#[derive(Debug, Default)]
pub struct FlightCore {
    revision: u64,
    initialized: bool,
    instructions: String,
    tools: Vec<ToolDefinition>,
    history: Vec<ResponseItem>,
    phase: Phase,
}

impl FlightCore {
    /// Creates an empty guest waiting for `init` or `restore`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            revision: 0,
            initialized: false,
            instructions: String::new(),
            tools: Vec::new(),
            history: Vec::new(),
            phase: Phase::Idle,
        }
    }

    /// Applies one command atomically and returns the next host action.
    pub fn apply(&mut self, command: Command) -> HostAction {
        match command {
            Command::Init {
                expected_revision,
                instructions,
                tools,
            } => {
                if let Some(error) = self.check_revision(expected_revision) {
                    return error;
                }
                if self.initialized || !matches!(self.phase, Phase::Idle) {
                    return self.error(ErrorCode::InvalidPhase, "guest is already initialized");
                }
                self.instructions = instructions;
                self.tools = tools;
                self.initialized = true;
                self.bump();
                HostAction::Ready {
                    revision: self.revision,
                }
            }
            Command::Prompt {
                expected_revision,
                text,
            } => {
                if let Some(error) = self.check_mutation(expected_revision, true) {
                    return error;
                }
                if !matches!(self.phase, Phase::Idle) {
                    return self.error(ErrorCode::InvalidPhase, "a turn is already active");
                }
                self.history.push(ResponseItem::message(
                    MessageRole::User,
                    [ContentItem::input_text(text)],
                ));
                self.phase = Phase::AwaitingModel;
                self.bump();
                self.model_request()
            }
            Command::ModelOutput {
                expected_revision,
                items,
            } => {
                if let Some(error) = self.check_mutation(expected_revision, true) {
                    return error;
                }
                if !matches!(self.phase, Phase::AwaitingModel) {
                    return self.error(
                        ErrorCode::InvalidPhase,
                        "guest is not awaiting model output",
                    );
                }
                if items.is_empty() {
                    return self.error(
                        ErrorCode::EmptyModelOutput,
                        "model output contained no items",
                    );
                }

                let mut seen = BTreeSet::new();
                let mut calls = Vec::new();
                for item in &items {
                    let Some(call) = item.tool_call() else {
                        continue;
                    };
                    let (namespace, name) = match call {
                        ResponseToolCallRef::Function {
                            namespace, name, ..
                        }
                        | ResponseToolCallRef::Custom {
                            namespace, name, ..
                        } => (namespace, name),
                    };
                    if !self
                        .tools
                        .iter()
                        .any(|tool| tool.accepts_call(namespace, name))
                    {
                        return self.error(
                            ErrorCode::UndeclaredToolCall,
                            match namespace {
                                Some(namespace) => {
                                    format!("tool {namespace}.{name} was not declared")
                                }
                                None => format!("tool {name} was not declared"),
                            },
                        );
                    }
                    if !seen.insert(call.call_id().to_owned()) {
                        return self.error(
                            ErrorCode::DuplicateToolCall,
                            format!("duplicate call_id {}", call.call_id()),
                        );
                    }
                    calls.push(host_tool_call(call));
                }

                self.history.extend(items.iter().cloned());
                self.bump();
                if calls.is_empty() {
                    self.phase = Phase::Idle;
                    HostAction::Complete {
                        revision: self.revision,
                        items,
                    }
                } else {
                    let pending = calls
                        .iter()
                        .map(|call| (call.call_id.clone(), PendingTool { call: call.clone() }))
                        .collect();
                    self.phase = Phase::AwaitingTools { pending };
                    HostAction::ToolCalls {
                        revision: self.revision,
                        calls,
                    }
                }
            }
            Command::ToolOutput {
                expected_revision,
                call_id,
                output,
            } => {
                if let Some(error) = self.check_mutation(expected_revision, true) {
                    return error;
                }
                let (tool, remaining) = {
                    let Phase::AwaitingTools { pending } = &mut self.phase else {
                        return self
                            .error(ErrorCode::InvalidPhase, "guest is not awaiting tool output");
                    };
                    let Some(tool) = pending.remove(&call_id) else {
                        return self.error(
                            ErrorCode::UnknownToolCall,
                            format!("call_id {call_id} is not pending"),
                        );
                    };
                    let remaining = pending.len();
                    (tool, remaining)
                };
                let item = match tool.call.kind {
                    ToolCallKind::Function => ResponseItem::function_call_output(call_id, output),
                    ToolCallKind::Custom => {
                        ResponseItem::custom_tool_output(call_id, Some(tool.call.name), output)
                    }
                };
                self.history.push(item);
                let finished = remaining == 0;
                self.bump();
                if finished {
                    self.phase = Phase::AwaitingModel;
                    self.model_request()
                } else {
                    HostAction::AwaitingToolOutputs {
                        revision: self.revision,
                        remaining,
                    }
                }
            }
            Command::Snapshot => HostAction::Snapshot {
                snapshot: self.snapshot(),
            },
            Command::Restore { snapshot } => {
                if snapshot.version != SNAPSHOT_VERSION {
                    return self.error(
                        ErrorCode::InvalidPhase,
                        format!("unsupported snapshot version {}", snapshot.version),
                    );
                }
                self.revision = snapshot.revision;
                self.initialized = snapshot.initialized;
                self.instructions = snapshot.instructions;
                self.tools = snapshot.tools;
                self.history = snapshot.history;
                self.phase = snapshot.phase;
                HostAction::Ready {
                    revision: self.revision,
                }
            }
            Command::Shutdown => HostAction::Shutdown {
                revision: self.revision,
            },
        }
    }

    fn check_mutation(&self, expected_revision: u64, initialized: bool) -> Option<HostAction> {
        self.check_revision(expected_revision).or_else(|| {
            (initialized && !self.initialized)
                .then(|| self.error(ErrorCode::NotInitialized, "guest requires init or restore"))
        })
    }

    fn check_revision(&self, expected_revision: u64) -> Option<HostAction> {
        (expected_revision != self.revision).then(|| {
            self.error(
                ErrorCode::RevisionConflict,
                format!(
                    "expected revision {expected_revision}, current revision is {}",
                    self.revision
                ),
            )
        })
    }

    const fn bump(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    fn model_request(&self) -> HostAction {
        let mut input = Vec::with_capacity(self.history.len() + 2);
        input.push(ResponseItem::message(
            MessageRole::Developer,
            [ContentItem::input_text(self.instructions.clone())],
        ));
        if !self.tools.is_empty() {
            input.push(ResponseItem::additional_tools(self.tools.clone()));
        }
        input.extend(self.history.iter().cloned());
        HostAction::ModelRequest {
            revision: self.revision,
            input,
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            version: SNAPSHOT_VERSION,
            revision: self.revision,
            initialized: self.initialized,
            instructions: self.instructions.clone(),
            tools: self.tools.clone(),
            history: self.history.clone(),
            phase: self.phase.clone(),
        }
    }

    fn error(&self, code: ErrorCode, detail: impl Into<String>) -> HostAction {
        HostAction::Error {
            revision: self.revision,
            code,
            detail: detail.into(),
        }
    }
}

fn host_tool_call(call: ResponseToolCallRef<'_>) -> HostToolCall {
    match call {
        ResponseToolCallRef::Function {
            name,
            namespace,
            arguments,
            call_id,
        } => HostToolCall {
            kind: ToolCallKind::Function,
            name: name.to_owned(),
            namespace: namespace.map(str::to_owned),
            input: arguments.to_owned(),
            call_id: call_id.to_owned(),
        },
        ResponseToolCallRef::Custom {
            name,
            namespace,
            input,
            call_id,
        } => HostToolCall {
            kind: ToolCallKind::Custom,
            name: name.to_owned(),
            namespace: namespace.map(str::to_owned),
            input: input.to_owned(),
            call_id: call_id.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply_json(core: &mut FlightCore, json: &str) -> HostAction {
        core.apply(serde_json::from_str(json).expect("valid command"))
    }

    #[test]
    fn drives_multiple_turns_and_a_host_tool() {
        let mut core = FlightCore::new();
        assert!(matches!(
            apply_json(
                &mut core,
                r#"{"op":"init","expected_revision":0,"instructions":"Be terse","tools":[{"type":"function","name":"read_sensor","description":"Read one sensor","strict":false,"parameters":{"type":"object"}}]}"#
            ),
            HostAction::Ready { revision: 1 }
        ));
        assert!(matches!(
            apply_json(
                &mut core,
                r#"{"op":"prompt","expected_revision":1,"text":"inspect"}"#
            ),
            HostAction::ModelRequest { revision: 2, .. }
        ));
        let tool = apply_json(
            &mut core,
            r#"{"op":"model_output","expected_revision":2,"items":[{"type":"function_call","name":"read_sensor","arguments":"{\"channel\":7}","call_id":"call-1"}]}"#,
        );
        assert!(matches!(tool, HostAction::ToolCalls { revision: 3, .. }));
        assert!(matches!(
            apply_json(
                &mut core,
                r#"{"op":"tool_output","expected_revision":3,"call_id":"call-1","output":"nominal"}"#
            ),
            HostAction::ModelRequest { revision: 4, .. }
        ));
        assert!(matches!(
            apply_json(
                &mut core,
                r#"{"op":"model_output","expected_revision":4,"items":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"nominal"}]}]}"#
            ),
            HostAction::Complete { revision: 5, .. }
        ));
        assert!(matches!(
            apply_json(
                &mut core,
                r#"{"op":"prompt","expected_revision":5,"text":"again"}"#
            ),
            HostAction::ModelRequest { revision: 6, .. }
        ));
    }

    #[test]
    fn snapshot_restores_an_in_flight_effect_boundary() {
        let mut core = FlightCore::new();
        let _ = apply_json(
            &mut core,
            r#"{"op":"init","expected_revision":0,"instructions":"x","tools":[]}"#,
        );
        let _ = apply_json(
            &mut core,
            r#"{"op":"prompt","expected_revision":1,"text":"x"}"#,
        );
        let snapshot = match core.apply(Command::Snapshot) {
            HostAction::Snapshot { snapshot } => snapshot,
            other => panic!("unexpected action: {other:?}"),
        };
        let mut replacement = FlightCore::new();
        assert!(matches!(
            replacement.apply(Command::Restore { snapshot }),
            HostAction::Ready { revision: 2 }
        ));
        assert!(matches!(
            apply_json(
                &mut replacement,
                r#"{"op":"model_output","expected_revision":2,"items":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}]}"#
            ),
            HostAction::Complete { revision: 3, .. }
        ));
    }

    #[test]
    fn rejects_stale_replays_without_mutation() {
        let mut core = FlightCore::new();
        let _ = apply_json(
            &mut core,
            r#"{"op":"init","expected_revision":0,"instructions":"x","tools":[]}"#,
        );
        assert!(matches!(
            apply_json(
                &mut core,
                r#"{"op":"prompt","expected_revision":0,"text":"stale"}"#
            ),
            HostAction::Error {
                revision: 1,
                code: ErrorCode::RevisionConflict,
                ..
            }
        ));
        assert!(matches!(
            core.apply(Command::Snapshot),
            HostAction::Snapshot {
                snapshot: Snapshot { revision: 1, .. }
            }
        ));
    }

    #[test]
    fn rejects_tools_not_declared_by_the_host() {
        let mut core = FlightCore::new();
        let _ = apply_json(
            &mut core,
            r#"{"op":"init","expected_revision":0,"instructions":"x","tools":[]}"#,
        );
        let _ = apply_json(
            &mut core,
            r#"{"op":"prompt","expected_revision":1,"text":"x"}"#,
        );
        assert!(matches!(
            apply_json(
                &mut core,
                r#"{"op":"model_output","expected_revision":2,"items":[{"type":"function_call","name":"undeclared","arguments":"{}","call_id":"call-1"}]}"#
            ),
            HostAction::Error {
                revision: 2,
                code: ErrorCode::UndeclaredToolCall,
                ..
            }
        ));
    }
}
