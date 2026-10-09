//! Optional execution-policy seam for Claude-native checkpoints and effects.
//!
//! The `nanocodex-durability` crate supplies the store-backed implementation.
//! Payloads retain provider-native blocks without translating signed content.
use nanocodex_agent::{NanocodexError, Result};
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
    /// Perform the effect. Unsettled effects may execute again after a crash.
    Execute,
    /// Exact settled output, without invoking the handler.
    Replay(Value),
}
/// One accepted steer retained by a Claude execution policy.
pub struct ClaudeSteer {
    /// Caller identity, when acceptance was identified.
    pub message_id: Option<String>,
    /// Stable one-based accepted position.
    pub index: u32,
    /// Model boundary current when accepted.
    pub accepted_after_model_call_index: u32,
    /// Model boundary at which this input was consumed, when bound.
    pub model_call_index: Option<u32>,
    /// Exact serialized original prompt used for receipt identity.
    pub input_json: String,
}
/// Host-owned durable execution, sharing Nanocodex's store and fencing rules.
pub trait ClaudeExecutionPolicy: Send + Sync {
    fn state_id(&self) -> &str;
    fn admit(
        &self,
        id: String,
        input: Value,
        automatic: bool,
    ) -> PolicyFuture<'_, (String, Admission)>;
    fn begin_attempt(&self, id: String) -> PolicyFuture<'_, ()>;
    /// Whether this policy persists steering inputs and identified receipts.
    /// Older policies retain their existing ephemeral plain steering behavior.
    fn supports_steering(&self) -> bool {
        false
    }
    /// Retains steering identity and input before acknowledging acceptance.
    fn accept_steer(
        &self,
        _id: String,
        _message_id: Option<String>,
        _after: u32,
        _input_json: String,
        _capacity: bool,
    ) -> PolicyFuture<'_, Option<u32>> {
        Box::pin(async {
            Err(NanocodexError::InvalidRequest(
                "Claude execution policy does not support accept_steer".into(),
            ))
        })
    }
    fn retained_steers(&self, _id: String) -> PolicyFuture<'_, Vec<ClaudeSteer>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn withdraw_steer(&self, _id: String, _index: u32) -> PolicyFuture<'_, ()> {
        Box::pin(async {
            Err(NanocodexError::InvalidRequest(
                "Claude execution policy does not support withdraw_steer".into(),
            ))
        })
    }
    fn bind_steer(&self, _id: String, _index: u32, _boundary: u32) -> PolicyFuture<'_, ()> {
        Box::pin(async {
            Err(NanocodexError::InvalidRequest(
                "Claude execution policy does not support bind_steer".into(),
            ))
        })
    }
    fn continuation(&self, id: String) -> PolicyFuture<'_, Option<Value>>;
    fn advance(&self, id: String, state: Value) -> PolicyFuture<'_, ()>;
    fn begin_step(
        &self,
        id: String,
        step_id: String,
        kind: String,
        input: Value,
    ) -> PolicyFuture<'_, Step>;
    fn complete_step(&self, id: String, step_id: String, output: Value) -> PolicyFuture<'_, ()>;
    fn complete(&self, id: String, checkpoint: Value, output: Value) -> PolicyFuture<'_, ()>;
    fn fail(&self, id: String, checkpoint: Value, error: String) -> PolicyFuture<'_, ()>;
    fn cancel(&self, id: String, checkpoint: Value) -> PolicyFuture<'_, ()>;
    /// Cancels a claimed operation whose attempt never started, without a
    /// checkpoint. Returns false when the policy cannot prove that no attempt
    /// began, so the caller keeps its ordinary retry path.
    fn cancel_unstarted(&self, _id: String) -> PolicyFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }
    fn release(&self, id: String) -> PolicyFuture<'_, ()>;
    fn shutdown(&self) -> PolicyFuture<'_, ()>;
    fn checkpoint(&self, state: Value) -> PolicyFuture<'_, ()>;
}
