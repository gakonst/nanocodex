pub(crate) mod agent;
#[cfg(not(target_family = "wasm"))]
mod agents_md;
#[cfg(target_family = "wasm")]
#[path = "agents_md_wasm.rs"]
mod agents_md;
mod compaction;
mod context_manager;
mod input;
mod telemetry;

#[cfg(not(target_family = "wasm"))]
pub(crate) use agents_md::load_global_instructions;

use telemetry::{
    CompactionCompleted, CompactionFailed, CompactionStarted, ModelCallCompleted, ModelCallFailed,
    ModelCallStarted, RunError, RunStarted, RunStats, RunSteered, ToolCallArguments, ToolCallEvent,
    ToolResultEvent, WarmupCompleted, WarmupFailed, WarmupStarted, display_endpoint, elapsed_ns,
    resolve_workspace, terminal_payload,
};
