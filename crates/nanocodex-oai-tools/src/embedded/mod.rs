//! Portable adapter for Code Mode runtimes owned by an embedding host.
//!
//! Embedded runtimes are useful when Rust owns the agent lifecycle but another
//! environment owns JavaScript execution and application-defined tools. The
//! adapter is independent of `wasm-bindgen`; a Node or browser binding can
//! implement [`CodeModeHost`] without leaking JavaScript types into this crate.
//!
//! ```
//! use nanocodex_oai_tools::{
//!     ToolContext, Tools,
//!     contract::ToolOutputBody,
//!     embedded::{
//!         CodeModeExecution, CodeModeHost, CodeModeHostError, HostFuture, bind_host,
//!     },
//! };
//!
//! struct ApplicationHost;
//!
//! impl CodeModeHost for ApplicationHost {
//!     fn tool_definitions(
//!         &self,
//!         _session_id: &str,
//!     ) -> Result<Vec<nanocodex_oai_tools::ToolDefinition>, CodeModeHostError> {
//!         Ok(Vec::new())
//!     }
//!
//!     fn execute<'a>(
//!         &'a self,
//!         source: &'a str,
//!         _context: ToolContext<'a>,
//!     ) -> HostFuture<'a, Result<CodeModeExecution, CodeModeHostError>> {
//!         Box::pin(async move {
//!             Ok(CodeModeExecution {
//!                 cell: None,
//!                 output: ToolOutputBody::Text(format!("evaluated: {source}")),
//!                 success: true,
//!                 nested_calls: Vec::new(),
//!                 notifications: Vec::new(),
//!             })
//!         })
//!     }
//! }
//!
//! # fn configure() -> Result<(), nanocodex_oai_tools::ToolsBuildError> {
//! let tools = Tools::builder().without_defaults().build()?;
//! let tools = bind_host(tools, ApplicationHost);
//! assert!(!tools.web_search_enabled());
//! # Ok(())
//! # }
//! ```

mod input;
mod runtime;
mod types;

use std::{error::Error, fmt, future::Future, pin::Pin};

use crate::{ToolContext, ToolDefinition, ToolInput, ToolOutput};

pub use input::{prepare_output_images, prepare_user_input};
pub use runtime::{EmbeddedToolRuntime, EmbeddedToolRuntimeControl};
pub use types::{
    CodeModeCell, CodeModeExecution, CodeModeNotification, CodeModeObserver, CodeModeUpdate,
    NestedToolCall, OwnedToolContext,
};

/// Binds a caller-owned Code Mode host to one immutable [`crate::Tools`] recipe.
///
/// This is an embedding seam for language bindings. Application tool policy
/// remains configured through the ordinary [`crate::ToolsBuilder`].
#[doc(hidden)]
#[must_use]
pub fn bind_host(mut tools: crate::Tools, host: impl CodeModeHost) -> crate::Tools {
    tools.embedded_host = Some(std::sync::Arc::new(host));
    tools
}

/// Future returned by an embedded Code Mode operation.
///
/// Native host futures must be sendable because a native agent driver may run
/// on a multithreaded executor.
#[cfg(not(target_family = "wasm"))]
pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Future returned by an embedded Code Mode operation.
///
/// Browser host futures may retain JavaScript values, which are local to the
/// browser thread.
#[cfg(target_family = "wasm")]
pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Error reported by an embedding host.
#[derive(Debug)]
pub struct CodeModeHostError {
    message: Box<str>,
}

impl CodeModeHostError {
    /// Creates an error with a complete host-provided diagnostic.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into().into_boxed_str(),
        }
    }

    /// Returns the complete host-provided diagnostic.
    #[must_use]
    pub const fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CodeModeHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CodeModeHostError {}

/// Model-visible dispatch policy selected by an embedding host.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EmbeddedToolMode {
    /// Expose one Code Mode `exec` tool and nest application tools below it.
    #[default]
    Code,
    /// Expose only Code Mode controls; workspace, discovery and local tools are nested.
    CodeOnly,
    /// Expose application tools directly without dynamic code evaluation.
    Direct,
}

/// Application-owned Code Mode and nested-tool execution boundary.
///
/// The host receives a read-only [`ToolContext`] for every cell and returns the
/// same typed execution contract used by the native runtime. Implementations
/// should preserve nested-call order and return a failed
/// [`CodeModeExecution`] for model-visible script failures. Reserve
/// [`CodeModeHostError`] for failures in the host bridge itself.
pub trait CodeModeHost: Send + Sync + 'static {
    /// Selects Code Mode or CSP-safe direct function dispatch.
    fn tool_mode(&self) -> EmbeddedToolMode {
        EmbeddedToolMode::Code
    }

    /// Whether the host implements resumable `exec`/`wait` cells and helpers.
    /// Existing complete-cell embeddings retain their original contract.
    fn supports_cells(&self) -> bool {
        false
    }

    /// Resumes a yielded cell while streaming newly observed nested work.
    fn wait_with_updates<'a>(
        &'a self,
        _input: &'a str,
        _context: ToolContext<'a>,
        _observer: &'a mut dyn CodeModeObserver,
    ) -> HostFuture<'a, Result<CodeModeExecution, CodeModeHostError>> {
        Box::pin(async {
            Err(CodeModeHostError::new(
                "embedded host does not support resumable cells",
            ))
        })
    }

    /// Returns the tools available to Code Mode for this session.
    ///
    /// The runtime calls this synchronously while building the model-visible
    /// `exec` definition.
    fn tool_definitions(&self, session_id: &str) -> Result<Vec<ToolDefinition>, CodeModeHostError>;

    /// Executes one complete Code Mode cell.
    fn execute<'a>(
        &'a self,
        source: &'a str,
        context: ToolContext<'a>,
    ) -> HostFuture<'a, Result<CodeModeExecution, CodeModeHostError>>;

    /// Executes one Code Mode cell while reporting nested-tool lifecycle
    /// updates at the boundary where the host observes them.
    ///
    /// The default preserves the complete execution contract for existing
    /// hosts. Hosts that can observe live nested work should override this and
    /// report updates at the actual start and completion boundaries.
    fn execute_with_updates<'a>(
        &'a self,
        source: &'a str,
        context: ToolContext<'a>,
        observer: &'a mut dyn CodeModeObserver,
    ) -> HostFuture<'a, Result<CodeModeExecution, CodeModeHostError>> {
        Box::pin(async move {
            let execution = self.execute(source, context).await?;
            for call in &execution.nested_calls {
                observer.update(CodeModeUpdate::NestedCallStarted {
                    call_id: &call.call_id,
                    name: &call.name,
                    input: &call.input,
                });
                observer.update(CodeModeUpdate::NestedCallCompleted(call));
            }
            Ok(execution)
        })
    }

    /// Executes a cell with Rust-owned tools pinned to its original context.
    /// Older hosts retain their behavior when no local tools are configured.
    fn execute_with_local_tools<'a>(
        &'a self,
        source: &'a str,
        context: ToolContext<'a>,
        tools: Vec<std::sync::Arc<dyn crate::Tool>>,
        observer: Option<&'a mut dyn CodeModeObserver>,
    ) -> HostFuture<'a, Result<CodeModeExecution, CodeModeHostError>> {
        Box::pin(async move {
            if !tools.is_empty() {
                return Err(CodeModeHostError::new(
                    "embedded host does not support nested local tools",
                ));
            }
            match observer {
                Some(observer) => self.execute_with_updates(source, context, observer).await,
                None => self.execute(source, context).await,
            }
        })
    }

    /// Executes one directly exposed application tool.
    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        _input: ToolInput,
        _context: ToolContext<'a>,
    ) -> HostFuture<'a, Result<ToolOutput, CodeModeHostError>> {
        Box::pin(async move {
            Err(CodeModeHostError::new(format!(
                "direct embedded tool `{name}` is unavailable"
            )))
        })
    }

    /// Starts a logical turn without cancelling cells retained by earlier turns.
    fn begin_turn(&self, _session_id: &str) {}

    /// Relays nested updates of cells a completed turn left running without an
    /// observer. For each such cell the host calls `observer(origin_call_id)` and
    /// reports that cell's nested starts and completions to the returned observer
    /// until the cell settles or a later wait takes over, delivering each update
    /// once. Must not block; hosts without a relay keep the default, and their
    /// cells report nested updates to the next wait.
    fn detach_turn(
        &self,
        _session_id: &str,
        _observer: &mut (dyn FnMut(&str) -> Box<dyn CodeModeObserver> + Send),
    ) {
    }

    /// Non-destructive early foreground yield. Custom hosts may conservatively
    /// retain timed observations by leaving this default implementation unchanged.
    fn preempt_turn<'a>(
        &'a self,
        _session_id: &'a str,
    ) -> HostFuture<'a, Result<(), CodeModeHostError>> {
        Box::pin(async { Ok(()) })
    }

    /// Cancels cells created or observed during the current logical turn.
    fn cancel_turn<'a>(
        &'a self,
        session_id: &'a str,
    ) -> HostFuture<'a, Result<(), CodeModeHostError>> {
        self.cancel(session_id)
    }

    /// Cancels the current logical turn's cells and reports nested calls they
    /// leave unfinished, including starts an earlier exec or wait delivered, to
    /// `observer` before returning. Each update is delivered once. Hosts without
    /// this keep the default, which cancels without reporting.
    fn cancel_turn_with_updates<'a>(
        &'a self,
        session_id: &'a str,
        _observer: &'a mut dyn CodeModeObserver,
    ) -> HostFuture<'a, Result<(), CodeModeHostError>> {
        self.cancel_turn(session_id)
    }

    /// Cancels host-owned Code Mode and nested-tool work for one agent session.
    fn cancel<'a>(&'a self, _session_id: &'a str) -> HostFuture<'a, Result<(), CodeModeHostError>> {
        Box::pin(async { Ok(()) })
    }
}
