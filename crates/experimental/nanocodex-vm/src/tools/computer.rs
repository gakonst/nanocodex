use super::VmToolClient;
use nanocodex_computer::{ComputerExecutor, ComputerRequest};
use nanocodex_tools::{ToolContext, ToolResult};
use std::sync::Arc;

pub(super) struct ComputerProxy(pub(super) Arc<dyn VmToolClient>);
#[async_trait::async_trait]
impl ComputerExecutor for ComputerProxy {
    async fn invoke(
        &self,
        request: Option<ComputerRequest>,
        context: ToolContext<'_>,
    ) -> ToolResult {
        self.0.computer(request, context).await
    }
}
