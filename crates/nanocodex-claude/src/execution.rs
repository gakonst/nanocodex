//! Optional execution-policy seam for Claude-native checkpoints and effects.
//!
//! The `nanocodex-durability` crate supplies the store-backed implementation.
//! Payloads retain provider-native blocks without translating signed content.
use nanocodex_agent::Result;
use serde_json::Value;
use std::{future::Future, pin::Pin};

/// Future returned by a host execution policy.
#[cfg(not(target_family = "wasm"))]
pub type PolicyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
/// Future returned by an isolate-local execution policy.
#[cfg(target_family = "wasm")]
pub type PolicyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;

/// Admission result from the authoritative store.
pub enum Admission {
    /// New operation.
    Execute,
    /// Previously accepted unfinished operation.
    Resume,
    /// Exact terminal receipt; does not rewind the current session.
    Completed { checkpoint: Value, output: Value },
    /// Previously failed operation.
    Failed { checkpoint: Value, error: String },
    /// Previously cancelled operation.
    Cancelled,
}
/// Admission of one external effect.
pub enum Step {
    /// Perform the admitted effect.
    Execute,
    /// Exact settled output, without invoking the handler.
    Replay(Value),
    /// An interrupted effect may have run and must not be repeated automatically.
    OutcomeUnknown,
}
/// Prepared native request plus opaque branch-local policy state.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RequestPreparation {
    /// Provider-native request; signed transcript blocks stay unmodified.
    pub request: Value,
    /// Policy history and routing receipt retained in the native checkpoint.
    pub state: Value,
}
/// Host-owned durable execution, sharing Nanocodex's store and fencing rules.
pub trait ClaudeExecutionPolicy: Send + Sync {
    fn state_id(&self) -> &str;
    /// Prepare and persist deterministic configuration/routing before dispatch.
    /// Defaults to the original native request for existing policy consumers.
    fn prepare_request(
        &self,
        _operation: String,
        _request_id: String,
        _continuation: bool,
        _state: Value,
        _request: Value,
    ) -> PolicyFuture<'_, Option<RequestPreparation>> {
        Box::pin(async { Ok(None) })
    }
    /// Supplies the authoritative native cell journal, if installed.
    #[cfg(all(feature = "code-mode", not(target_family = "wasm")))]
    fn code_mode_journal(
        &self,
    ) -> Option<std::sync::Arc<dyn nanocodex_oai_tools::code_mode::CodeModeJournal>> {
        None
    }
    fn admit(
        &self,
        id: String,
        input: Value,
        automatic: bool,
    ) -> PolicyFuture<'_, (String, Admission)>;
    fn begin_attempt(&self, id: String) -> PolicyFuture<'_, ()>;
    fn continuation(&self, id: String) -> PolicyFuture<'_, Option<Value>>;
    fn advance(&self, id: String, state: Value) -> PolicyFuture<'_, ()>;
    /// Foreground checkpoint retaining independently owned background effects.
    fn advance_retaining(
        &self,
        id: String,
        state: Value,
        retained_steps: Vec<String>,
    ) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            if !retained_steps.is_empty() {
                return Err(
                    nanocodex_agent::NanocodexError::ExecutionPolicyCapabilityUnsupported {
                        capability: "background effect retention",
                    },
                );
            }
            self.advance(id, state).await
        })
    }
    fn begin_step(
        &self,
        id: String,
        step_id: String,
        kind: String,
        input: Value,
    ) -> PolicyFuture<'_, Step>;
    /// Retains explicit replay permission with intent. Both old and current
    /// permissions must be safe before an interrupted effect can run again.
    fn begin_step_with_replay(
        &self,
        _id: String,
        _step_id: String,
        _kind: String,
        _input: Value,
        _replay_safety: nanocodex_agent::ReplaySafety,
    ) -> PolicyFuture<'_, Step> {
        Box::pin(async {
            Err(
                nanocodex_agent::NanocodexError::ExecutionPolicyCapabilityUnsupported {
                    capability: "effect replay safety",
                },
            )
        })
    }
    fn complete_step(&self, id: String, step_id: String, output: Value) -> PolicyFuture<'_, ()>;
    fn complete(&self, id: String, checkpoint: Value, output: Value) -> PolicyFuture<'_, ()>;
    fn fail(&self, id: String, checkpoint: Value, error: String) -> PolicyFuture<'_, ()>;
    fn cancel(&self, id: String, checkpoint: Value) -> PolicyFuture<'_, ()>;
    fn release(&self, id: String) -> PolicyFuture<'_, ()>;
    fn shutdown(&self) -> PolicyFuture<'_, ()>;
    fn checkpoint(&self, state: Value) -> PolicyFuture<'_, ()>;
}
