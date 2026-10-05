use super::ExecutionFuture;
use crate::Result;
use std::sync::Arc;

/// Embedding-owned foreground work that must settle before a turn is terminal.
/// The driver remains cancellable while waiting for successful completion.
pub trait TurnOwnership: Send + Sync + 'static {
    /// Retains embedding resources for a caller-owned native handle. Drivers and
    /// factory recipes must not store this lease: a managing harness removes it
    /// from the returned handle and provides its own active-work ownership.
    fn caller_ownership(&self) -> Option<Arc<dyn Send + Sync>> {
        None
    }

    /// Reconstructs owned work before admitting a new model call.
    fn prepare<'a>(&'a self, _session_id: &'a str) -> ExecutionFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Waits for foreground work on success, or stops it before failure/abort.
    fn settle<'a>(&'a self, session_id: &'a str, success: bool) -> ExecutionFuture<'a, Result<()>>;
}
