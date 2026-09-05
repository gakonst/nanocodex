#![cfg_attr(feature = "native", doc = include_str!("../README.md"))]
#![cfg_attr(
    all(not(feature = "native"), feature = "workspace-runtime"),
    doc = include_str!("../WORKSPACE_RUNTIME.md")
)]
#![cfg_attr(
    not(any(feature = "native", feature = "workspace-runtime")),
    doc = "Dependency-light model-visible tool contracts for Nanocodex."
)]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(target_family = "wasm", allow(clippy::module_name_repetitions))]

#[cfg(feature = "workspace-runtime")]
#[doc(hidden)]
pub mod apply_patch;
#[cfg(all(not(target_family = "wasm"), feature = "attachment"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub mod attachment;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub mod code_mode;
#[cfg(feature = "native")]
#[path = "code_mode/description.rs"]
mod code_mode_description;
#[cfg(feature = "native")]
mod code_mode_order;
#[cfg(feature = "native")]
pub mod embedded;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub mod image;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
mod image_generation;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub mod mcp;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
mod plan;
#[cfg(all(not(target_family = "wasm"), feature = "attachment"))]
mod prepared;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub mod runtime;
#[cfg(feature = "native")]
mod runtime_config;
#[cfg(any(
    feature = "native",
    all(not(target_family = "wasm"), feature = "attachment")
))]
#[path = "runtime/selection.rs"]
mod selection;
#[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
mod shell;
#[cfg(feature = "workspace-runtime")]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub mod standard;
#[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
mod view_image;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
mod web_search;
#[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub mod workspace_runtime;
#[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
pub use workspace_runtime::WorkspaceTools;

/// Model-visible tool definitions, inputs, outputs, and execution contracts.
pub mod contract {
    #[cfg(all(
        not(target_family = "wasm"),
        any(feature = "attachment", feature = "workspace-runtime")
    ))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    pub use async_trait::async_trait;
    pub use nanocodex_oai_api::tools::{
        DEFAULT_TOOL_OUTPUT_TOKENS, Tool, ToolContext, ToolDefinition, ToolError, ToolInput,
        ToolInputError, ToolOutput, ToolOutputBody, ToolOutputContent, ToolOutputWire,
        ToolProcessTraceWire, ToolResult,
    };
}

#[cfg(all(target_family = "wasm", feature = "native"))]
/// Code Mode results and observation contracts for the embedded WASM runtime.
pub mod code_mode {
    pub use crate::embedded::{
        CodeModeExecution, CodeModeNotification, CodeModeObserver, CodeModeUpdate, NestedToolCall,
    };
}

#[cfg(all(target_family = "wasm", feature = "native"))]
/// Image input and output preparation for the embedded WASM runtime.
pub mod image {
    pub use crate::embedded::{prepare_output_images, prepare_user_input};
    pub use nanocodex_oai_api::ImageDetail;
}

#[cfg(all(target_family = "wasm", feature = "native"))]
/// Embedded tool selection and execution runtime.
pub mod runtime {
    pub use crate::{
        embedded::{
            EmbeddedToolMode, EmbeddedToolRuntime as ToolRuntime,
            EmbeddedToolRuntimeControl as ToolRuntimeControl, OwnedToolContext,
        },
        runtime_config::{ImageGenerationConfig, WebSearchConfig},
        selection::{ToolExposure, ToolSource, Tools, ToolsBuildError, ToolsBuilder},
    };
}

#[cfg(all(
    not(target_family = "wasm"),
    any(feature = "native", all(test, feature = "workspace-runtime"))
))]
pub(crate) use contract::ToolOutputBody;
#[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
pub(crate) use contract::ToolOutputContent;
pub use contract::{Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult};
#[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
pub(crate) use nanocodex_oai_api::ImageDetail;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub use nanocodex_tools_macros::tool;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
pub(crate) use runtime::{DynamicToolProvider, ImageGenerationConfig, WebSearchConfig};
#[cfg(any(
    feature = "native",
    all(not(target_family = "wasm"), feature = "attachment")
))]
pub use selection::ToolExposure;
#[cfg(any(
    feature = "native",
    all(not(target_family = "wasm"), feature = "attachment")
))]
pub use selection::Tools;
#[cfg(any(
    feature = "native",
    all(not(target_family = "wasm"), feature = "attachment")
))]
pub use selection::{ToolSource, ToolsBuildError, ToolsBuilder};
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
#[cfg_attr(docsrs, doc(cfg(all(not(target_family = "wasm"), feature = "native"))))]
pub use shell::ambient_sensitive_environment;
#[cfg(feature = "workspace-runtime")]
pub(crate) use standard::StandardTool;

#[doc(hidden)]
pub mod __private {
    #[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
    pub use async_trait::async_trait;
    #[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
    pub use schemars;
    #[cfg(not(target_family = "wasm"))]
    pub use serde;

    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub use crate::runtime::schema_for;
    #[cfg(not(target_family = "wasm"))]
    pub use crate::{Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult};

    /// Builds the direct Responses tool prefix and the nested Code Mode name map together.
    #[cfg(feature = "native")]
    pub fn model_contract(
        runtime: &crate::runtime::ToolRuntime,
        session_id: &str,
    ) -> (
        Vec<nanocodex_oai_api::tools::ToolDefinition>,
        Vec<(String, String)>,
    ) {
        runtime.model_contract(session_id)
    }
}
