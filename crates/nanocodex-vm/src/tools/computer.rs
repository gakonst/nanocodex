use super::VmToolClient;
use nanocodex_computer::ComputerExecutor;
use nanocodex_tools::{ToolContext, ToolResult};
use std::sync::Arc;

pub(super) struct ComputerProxy(pub(super) Arc<dyn VmToolClient>);
#[async_trait::async_trait]
impl ComputerExecutor for ComputerProxy {
    async fn invoke_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
        context: ToolContext<'_>,
    ) -> ToolResult {
        self.0.computer(name, arguments, context).await
    }
}
