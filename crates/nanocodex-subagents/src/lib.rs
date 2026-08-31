//! Reusable, application-composed subagent tools and task-tree runtime.

mod capacity;
mod harness;
mod message;
mod model;
mod platform;
mod runtime;
mod task_tree;
mod tools;

pub use model::{
    AgentDescriptor, AgentId, AgentLineage, AgentMessage, AgentMessageUpdate, AgentStatus,
    AgentTerminalCompletion, AgentThread, AgentUpdate, AgentUsage, MessageDeliveryState,
    MessageDisposition, MessageId, MessagePriority, MessagePurpose, MessageSender,
    ScopedAgentUpdate, SubagentRuntimeId, ThreadId,
};
pub use runtime::{
    AgentDirectoryEntry, AgentSummary, MessageReceipt, Registry, SubagentControl, channel,
};
pub use tools::{
    AgentStartReport, AgentTask, AgentToolResult, install_tools, start_agent, start_agent_with,
    start_agents, start_agents_observed,
};

/// Default maximum number of active turns in one task tree.
pub const DEFAULT_MAX_SUBAGENTS: usize = 32;

/// Default maximum number of inactive, reusable subagent runtimes retained in memory.
///
/// Active turns may temporarily exceed this limit. Once turns reach a terminal
/// state, the least-recently-used inactive runtimes are unloaded until residency
/// returns to this bound. Their topology, status, and last output remain
/// inspectable.
pub const DEFAULT_MAX_RESIDENT_SUBAGENTS: usize = 16;
