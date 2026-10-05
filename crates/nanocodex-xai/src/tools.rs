//! Native xAI host adapters and contextual callbacks.
use crate::Xai;
pub use nanocodex_xai_tools::{
    HostContext as XaiToolInvocation, ToolDefinition, ToolOutput as XaiToolReply,
};
use nanocodex_xai_tools::{HostRequest, XaiHost};
use serde_json::Value;
use std::{collections::HashMap, future::Future, sync::Arc};
pub(crate) type ToolFuture = nanocodex_xai_tools::HostFuture<Result<XaiToolReply, String>>;
pub(crate) type ToolHandler = Arc<dyn Fn(Value, XaiToolInvocation) -> ToolFuture + Send + Sync>;
#[derive(Clone, Default)]
pub struct XaiTools {
    pub(crate) tools: HashMap<String, (ToolDefinition, ToolHandler)>,
}
impl XaiTools {
    pub fn new() -> Self {
        Self::default()
    }
    #[cfg(not(target_family = "wasm"))]
    pub fn tool_with_context<F, Fut>(mut self, definition: ToolDefinition, callback: F) -> Self
    where
        F: Fn(Value, XaiToolInvocation) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<XaiToolReply, String>> + Send + 'static,
    {
        self.tools.insert(
            definition.name.clone(),
            (definition, Arc::new(move |v, c| Box::pin(callback(v, c)))),
        );
        self
    }
    #[cfg(target_family = "wasm")]
    pub fn tool_with_context<F, Fut>(mut self, definition: ToolDefinition, callback: F) -> Self
    where
        F: Fn(Value, XaiToolInvocation) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<XaiToolReply, String>> + 'static,
    {
        self.tools.insert(
            definition.name.clone(),
            (definition, Arc::new(move |v, c| Box::pin(callback(v, c)))),
        );
        self
    }
    pub fn host<H: XaiHost + ?Sized>(mut self, host: Arc<H>) -> Self {
        for definition in host.definitions() {
            let provider = host.clone();
            let name = definition.name.clone();
            self = self.tool_with_context(definition, move |input, context| {
                provider.call(HostRequest {
                    context,
                    tool: name.clone(),
                    input,
                })
            });
        }
        self
    }
}
impl Xai {
    /// Installs only the capabilities explicitly supplied by this host.
    pub fn host<H: XaiHost + ?Sized>(mut self, host: Arc<H>) -> Self {
        self.tools.extend(XaiTools::new().host(host).tools);
        self
    }
    pub fn host_tools<H: XaiHost + ?Sized>(self, host: Arc<H>) -> Self {
        self.host(host)
    }
    #[cfg(all(feature = "tools", not(target_family = "wasm")))]
    pub fn workspace_files(self, files: Arc<nanocodex_xai_tools::XaiWorkspaceFiles>) -> Self {
        self.host(files)
    }
    pub fn bash<H: XaiHost + ?Sized>(self, bash: Arc<H>) -> Self {
        self.host(bash)
    }
    pub fn mcp<H: XaiHost + ?Sized>(self, mcp: Arc<H>) -> Self {
        self.host(mcp)
    }
    pub fn approved_web<H: XaiHost + ?Sized>(self, web: Arc<H>) -> Self {
        self.host(web)
    }
    pub fn tasks<H: XaiHost + ?Sized>(self, tasks: Arc<H>) -> Self {
        self.host(tasks)
    }
}
