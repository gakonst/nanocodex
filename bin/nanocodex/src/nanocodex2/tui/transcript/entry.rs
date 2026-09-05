// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use crate::config::ReasoningEffort;
use nanocodex_subagents::{AgentThread, MessageDeliveryState, MessageId, MessageSender};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct EntryId(usize);

impl EntryId {
    pub(super) const fn from_index(index: usize) -> Self {
        Self(index)
    }

    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TransientStatus {
    Thinking,
    Responding,
    Warming,
    WaitingForBackgroundWork,
    Tool(String),
    Compacting,
    Retrying(u64),
    Connecting,
    Reconnecting,
    Error(String),
}

#[derive(Clone, Debug)]
pub(crate) struct TranscriptEntry {
    pub(crate) id: EntryId,
    pub(crate) revision: u64,
    pub(crate) kind: EntryKind,
    pub(crate) hidden: bool,
    pub(crate) parent: Option<EntryId>,
    pub(crate) trailing_spacer: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum EntryKind {
    User { text: String },
    Assistant { text: String, complete: bool },
    Reasoning { text: String },
    Tool(ToolEntry),
    DirectedMessage(DirectedMessageEntry),
    ForkedFrom { session_id: String },
    EffortChanged { to: ReasoningEffort },
    FastModeChanged { enabled: bool },
    ReflectionStarted,
    Interrupted { count: usize },
    ContextCompacted { duration_ns: u64 },
    TurnCompleted { duration_ns: u64 },
    ContextCompactionFailed { message: String },
    Error { message: String },
}

#[derive(Clone, Debug)]
pub(crate) struct DirectedMessageEntry {
    pub(crate) perspective: MessageSender,
    pub(crate) thread: AgentThread,
    pub(crate) deliveries: Vec<MessageDelivery>,
}

impl DirectedMessageEntry {
    pub(crate) fn delivery(&self, message_id: MessageId) -> Option<&MessageDeliveryState> {
        self.deliveries
            .iter()
            .find(|delivery| delivery.message_id == message_id)
            .map(|delivery| &delivery.state)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MessageDelivery {
    pub(crate) message_id: MessageId,
    pub(crate) state: MessageDeliveryState,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum MessagePhase {
    Commentary,
    Final,
}

impl From<Option<&str>> for MessagePhase {
    fn from(phase: Option<&str>) -> Self {
        if phase == Some("commentary") {
            return Self::Commentary;
        }
        Self::Final
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ToolEntry {
    pub(crate) name: String,
    pub(crate) arguments: Value,
    pub(crate) started_at_unix_ms: u64,
    pub(crate) state: ToolState,
    pub(crate) duration_ns: Option<u64>,
    pub(crate) result: Option<Value>,
    pub(crate) metadata: Option<Value>,
    pub(crate) execution: ToolExecution,
    pub(crate) substeps: Vec<String>,
    pub(crate) child_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ToolExecution {
    Account,
    Runtime,
    Sandbox { cwd: Option<String> },
    Direct,
    WebClient,
    Machine { name: String, cwd: Option<String> },
    Mcp { server: String },
    Browser,
    Subagent { target: Option<String> },
}

impl ToolExecution {
    pub(crate) fn infer(name: &str, arguments: &Value, metadata: Option<&Value>) -> Self {
        let identity = ToolIdentity::decode(metadata.and_then(tool_name).unwrap_or(name));
        if let Some(machine) = metadata.and_then(machine_name).or(identity.machine) {
            return Self::Machine {
                name: machine.to_owned(),
                cwd: execution_cwd(arguments),
            };
        }
        if let Some(server) = metadata
            .and_then(|value| find_string(value, &["mcp_server"]))
            .or(identity.mcp_server)
        {
            return Self::Mcp {
                server: server.to_owned(),
            };
        }
        if matches!(identity.family, "exec_command" | "write_stdin" | "preview")
            && let Some(environment) = arguments.get("environment").and_then(Value::as_str)
        {
            if environment == "sandbox" {
                return Self::Sandbox {
                    cwd: execution_cwd(arguments),
                };
            }
            if let Some(machine) = environment
                .strip_prefix("user:")
                .filter(|id| !id.is_empty())
            {
                return Self::Machine {
                    name: machine.to_owned(),
                    cwd: execution_cwd(arguments),
                };
            }
        }
        if matches!(identity.family, "accountInfo" | "account_connectors") {
            return Self::Account;
        }
        if identity.family == "runtimeInfo" {
            return Self::Runtime;
        }
        if identity.family.starts_with("sandbox_") || metadata.is_some_and(reports_sandbox) {
            return Self::Sandbox {
                cwd: execution_cwd(arguments),
            };
        }
        if identity.family == "web__run" {
            return Self::WebClient;
        }
        if identity.family == "browser" || identity.family.starts_with("browser_") {
            return Self::Browser;
        }
        if is_subagent_tool(identity.family) {
            return Self::Subagent {
                target: subagent_target(arguments),
            };
        }
        Self::Direct
    }

    fn qualifier(&self) -> String {
        match self {
            Self::Account => "Account".to_owned(),
            Self::Runtime => "Runtime".to_owned(),
            Self::Sandbox { cwd: Some(cwd) } => format!("Sandbox · {cwd}"),
            Self::Sandbox { cwd: None } => "Sandbox".to_owned(),
            Self::Direct => "Local".to_owned(),
            Self::WebClient => "Web client".to_owned(),
            Self::Machine {
                name,
                cwd: Some(cwd),
            } => format!("Machine {name} · {cwd}"),
            Self::Machine { name, cwd: None } => format!("Machine {name}"),
            Self::Mcp { server } => format!("MCP · {server}"),
            Self::Browser => "Browser".to_owned(),
            Self::Subagent {
                target: Some(target),
            } => format!("Subagent · {target}"),
            Self::Subagent { target: None } => "Subagent".to_owned(),
        }
    }
}

impl ToolEntry {
    pub(crate) fn infer_execution(&mut self) {
        self.execution = ToolExecution::infer(&self.name, &self.arguments, self.metadata.as_ref());
    }

    pub(crate) fn execution_qualifier(&self) -> String {
        self.execution.qualifier()
    }

    pub(crate) fn family(&self) -> &str {
        ToolIdentity::decode(
            self.metadata
                .as_ref()
                .and_then(tool_name)
                .unwrap_or(&self.name),
        )
        .family
    }

    pub(crate) fn has_mcp_origin(&self) -> bool {
        ToolIdentity::decode(&self.name).mcp_server.is_some()
    }

    pub(crate) fn mcp_server(&self) -> Option<&str> {
        ToolIdentity::decode(&self.name).mcp_server
    }

    pub(crate) fn inferred_execution(
        name: &str,
        arguments: &Value,
        metadata: Option<&Value>,
    ) -> ToolExecution {
        ToolExecution::infer(name, arguments, metadata)
    }

    pub(crate) const fn local_execution() -> ToolExecution {
        ToolExecution::Direct
    }
}

#[derive(Clone, Copy)]
struct ToolIdentity<'a> {
    family: &'a str,
    machine: Option<&'a str>,
    mcp_server: Option<&'a str>,
}

impl<'a> ToolIdentity<'a> {
    fn decode(name: &'a str) -> Self {
        let (machine, qualified) = name
            .strip_prefix("user_")
            .and_then(split_machine_tool)
            .filter(|(machine, family)| !machine.is_empty() && !family.is_empty())
            .map_or((None, name), |(machine, family)| (Some(machine), family));
        let (mcp_server, family) = qualified
            .strip_prefix("mcp__")
            .and_then(|name| name.split_once("__"))
            .filter(|(server, family)| !server.is_empty() && !family.is_empty())
            .map_or((None, qualified), |(server, family)| (Some(server), family));
        Self {
            family,
            machine,
            mcp_server,
        }
    }
}

fn split_machine_tool(name: &str) -> Option<(&str, &str)> {
    if let Some((machine, mcp)) = name.split_once("_mcp__")
        && !machine.is_empty()
        && !mcp.is_empty()
    {
        return Some((machine, &name[machine.len() + 1..]));
    }
    const KNOWN_FAMILIES: &[&str] = &[
        "account_connectors",
        "image_gen__imagegen",
        "send_agent_message",
        "interrupt_agent",
        "browser_execute",
        "exec_command",
        "submit_result",
        "list_agents",
        "spawn_agent",
        "close_agent",
        "wait_agent",
        "update_plan",
        "write_stdin",
        "accountInfo",
        "runtimeInfo",
        "apply_patch",
        "view_image",
        "tool_search",
        "web__run",
        "browser",
        "memory",
        "preview",
        "exec",
        "wait",
    ];
    KNOWN_FAMILIES
        .iter()
        .find_map(|family| {
            name.strip_suffix(family)
                .and_then(|prefix| prefix.strip_suffix('_'))
                .filter(|machine| !machine.is_empty())
                .map(|machine| (machine, *family))
        })
        .or_else(|| name.split_once('_'))
}

fn execution_cwd(arguments: &Value) -> Option<String> {
    arguments
        .get("cwd")
        .or_else(|| arguments.get("workdir"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn reports_sandbox(value: &Value) -> bool {
    let Some(fields) = value.as_object() else {
        return false;
    };
    fields.get("sandbox").and_then(Value::as_bool) == Some(true)
        || ["execution", "origin", "environment", "kind"]
            .into_iter()
            .any(|key| fields.get(key).and_then(Value::as_str) == Some("sandbox"))
        || fields.values().any(reports_sandbox)
}

pub(crate) fn is_subagent_tool(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent"
            | "submit_result"
            | "send_agent_message"
            | "list_agents"
            | "wait_agent"
            | "interrupt_agent"
            | "close_agent"
    )
}

fn subagent_target(arguments: &Value) -> Option<String> {
    arguments
        .get("role")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            arguments
                .get("agent_id")
                .and_then(Value::as_u64)
                .map(|id| format!("agent {id}"))
        })
        .or_else(|| {
            let ids = arguments.get("agent_ids")?.as_array()?;
            let labels = ids
                .iter()
                .filter_map(Value::as_u64)
                .map(|id| format!("{id}"))
                .collect::<Vec<_>>();
            (!labels.is_empty()).then(|| format!("agents {}", labels.join(", ")))
        })
}

fn machine_name(value: &Value) -> Option<&str> {
    find_string(value, &["machine_name", "machineName"])
        .or_else(|| value.get("machine").and_then(machine_value_name))
        .or_else(|| value.get("executor").and_then(machine_value_name))
}

fn tool_name(value: &Value) -> Option<&str> {
    find_string(value, &["tool_name", "toolName"])
}

fn machine_value_name(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("name").and_then(Value::as_str))
}

fn find_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    let fields = value.as_object()?;
    keys.iter()
        .find_map(|key| fields.get(*key).and_then(Value::as_str))
        .or_else(|| fields.values().find_map(|value| find_string(value, keys)))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolState {
    Running,
    Succeeded,
    Failed,
}
