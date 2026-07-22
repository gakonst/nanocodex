#![cfg_attr(target_family = "wasm", allow(clippy::module_name_repetitions))]

#[cfg(not(target_family = "wasm"))]
mod apply_patch;
#[cfg(all(not(target_family = "wasm"), feature = "code-mode"))]
mod code_mode;
#[cfg(not(target_family = "wasm"))]
mod image;
#[cfg(all(not(target_family = "wasm"), feature = "remote-tools"))]
mod image_generation;
#[cfg(not(target_family = "wasm"))]
mod plan;
#[cfg(not(target_family = "wasm"))]
mod runtime;
#[cfg(not(target_family = "wasm"))]
mod shell;
#[cfg(not(target_family = "wasm"))]
mod standard;
#[cfg(not(target_family = "wasm"))]
mod view_image;
#[cfg(target_family = "wasm")]
mod wasm;
#[cfg(all(not(target_family = "wasm"), feature = "remote-tools"))]
mod web_search;

#[cfg(all(not(target_family = "wasm"), feature = "code-mode"))]
pub use code_mode::{CodeModeExecution, NestedToolCall};
#[cfg(not(target_family = "wasm"))]
pub use image::{prepare_output_images, prepare_user_input};
pub use nanocodex_core::{ImageDetail, ToolDefinition};
#[cfg(not(target_family = "wasm"))]
pub use plan::UpdatePlanTool;
#[cfg(not(target_family = "wasm"))]
pub use runtime::{
    DEFAULT_TOOL_OUTPUT_TOKENS, DynamicToolProvider, ImageGenerationConfig, OwnedToolContext,
    ProcessTraceWire, Tool, ToolContext, ToolError, ToolExecution, ToolExecutionWire, ToolInput,
    ToolInputError, ToolOutputBody, ToolOutputContent, ToolResult, ToolRuntime, ToolRuntimeControl,
    Tools, ToolsBuildError, ToolsBuilder, WebSearchConfig, schema_for,
};
#[cfg(not(target_family = "wasm"))]
pub use standard::StandardTool;
#[cfg(target_family = "wasm")]
pub use wasm::*;
