//! Host-owned, task-aware routing before a clean child is constructed.
use async_trait::async_trait;
use nanocodex_agent::SpawnOptions;

/// A prepared choice. The opaque reference carries no provider credentials.
pub struct SpawnRoute {
    pub options: SpawnOptions,
    pub reference: String,
}

/// Implemented by the embedding host. Resolve must enforce the invoking child's
/// authority and explicit overrides; bind must durably pin the choice before any
/// child turn starts. Returning an error fails the spawn closed.
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
pub trait SpawnRouter: Send + Sync {
    async fn resolve(
        &self,
        parent_session_id: &str,
        role: &str,
        task: &str,
        options: SpawnOptions,
        host_context: Option<&str>,
    ) -> std::io::Result<SpawnRoute>;

    fn bind(
        &self,
        parent_session_id: &str,
        child_session_id: &str,
        reference: &str,
        host_context: Option<&str>,
    ) -> std::io::Result<()>;
}
