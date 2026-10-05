use crate::Registry;
use nanocodex_agent::{
    NanocodexError, Result,
    execution::{ExecutionFuture, TurnOwnership},
};
use std::sync::{Arc, Weak};

/// Native lifecycle barrier for a registry's foreground ownership boundaries.
pub struct RegistryOwnership(pub Arc<Registry>);

impl TurnOwnership for RegistryOwnership {
    fn prepare<'a>(&'a self, session_id: &'a str) -> ExecutionFuture<'a, Result<()>> {
        Box::pin(async move {
            self.0
                .recover_registered(session_id)
                .await
                .map_err(|error| NanocodexError::InvalidExecutionPolicy(error.to_string()))
        })
    }

    fn settle<'a>(&'a self, session_id: &'a str, success: bool) -> ExecutionFuture<'a, Result<()>> {
        Box::pin(async move {
            let result = if success {
                self.0.wait_foreground(session_id).await
            } else {
                self.0.abort_foreground(session_id).await
            };
            result.map_err(|error| NanocodexError::InvalidExecutionPolicy(error.to_string()))
        })
    }
}

impl RegistryOwnership {
    /// Idle managed drivers must not retain the registry that owns their harness.
    /// Direct caller handles acquire their own lease; the harness replaces it
    /// with ownership while executing or delivering committed mailbox work.
    pub fn child(registry: &Arc<Registry>) -> impl TurnOwnership + use<> {
        ChildOwnership(Arc::downgrade(registry))
    }
}

struct ChildOwnership(Weak<Registry>);

impl TurnOwnership for ChildOwnership {
    fn caller_ownership(&self) -> Option<Arc<dyn Send + Sync>> {
        self.0
            .upgrade()
            .map(|registry| registry as Arc<dyn Send + Sync>)
    }

    fn prepare<'a>(&'a self, session_id: &'a str) -> ExecutionFuture<'a, Result<()>> {
        Box::pin(async move {
            let registry = self.0.upgrade().ok_or(NanocodexError::AgentStopped)?;
            RegistryOwnership(registry).prepare(session_id).await
        })
    }

    fn settle<'a>(&'a self, session_id: &'a str, success: bool) -> ExecutionFuture<'a, Result<()>> {
        Box::pin(async move {
            let registry = self.0.upgrade().ok_or(NanocodexError::AgentStopped)?;
            RegistryOwnership(registry)
                .settle(session_id, success)
                .await
        })
    }
}
