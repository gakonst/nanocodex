#![cfg_attr(target_family = "wasm", allow(clippy::module_name_repetitions))]

#[cfg(not(target_family = "wasm"))]
mod hashline;
#[cfg(not(target_family = "wasm"))]
mod code_mode;

#[cfg(not(target_family = "wasm"))]
mod hashline;
#[cfg(not(target_family = "wasm"))]
mod image;
#[cfg(not(target_family = "wasm"))]
mod image_generation;
#[cfg(not(target_family = "wasm"))]
mod plan;
#[cfg(not(target_family = "wasm"))]
mod runtime;
#[cfg(not(target_family = "wasm"))]
mod shell;
#[cfg(not(target_family = "wasm"))]
mod view_image;
#[cfg(target_family = "wasm")]
mod wasm;
#[cfg(not(target_family = "wasm"))]
mod web_search;

#[cfg(not(target_family = "wasm"))]
pub use code_mode::{CodeModeExecution, NestedToolCall};
#[cfg(not(target_family = "wasm"))]
pub use image::{prepare_output_images, prepare_user_input};
pub use nanocodex_core::ImageDetail;
#[cfg(not(target_family = "wasm"))]
pub use runtime::{
    DEFAULT_TOOL_OUTPUT_TOKENS, DynamicToolProvider, ImageGenerationConfig, OwnedToolContext, Tool,
    ToolContext, ToolError, ToolExecution, ToolInput, ToolInputError, ToolOutputBody,
    ToolOutputContent, ToolResult, ToolRuntime, ToolRuntimeControl, Tools, ToolsBuildError,
    ToolsBuilder, WebSearchConfig, schema_for,
};
#[cfg(target_family = "wasm")]
pub use wasm::*;
