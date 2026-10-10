//! Optional execution-policy seam for Claude-native checkpoints and effects.
//!
//! The `nanocodex-durability` crate supplies the store-backed implementation.
//! Payloads retain provider-native blocks without translating signed content.
use nanocodex_agent::{NanocodexError, Result, SessionInfo};
use serde_json::{Value, value::RawValue};
use std::{future::Future, pin::Pin, sync::Arc};

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
    /// Advances with an already encoded cursor. A cursor carries the whole
    /// conversation, so stores should override this to avoid materializing an
    /// intermediate [`Value`] tree on every round.
    // RawValue is unsized; overriding stores take ownership of the encoded text.
    #[allow(clippy::boxed_local)]
    fn advance_encoded(&self, id: String, state: Box<RawValue>) -> PolicyFuture<'_, ()> {
        match serde_json::from_str(state.get()) {
            Ok(state) => self.advance(id, state),
            Err(error) => Box::pin(async move {
                Err(NanocodexError::InvalidRequest(format!(
                    "invalid encoded Claude execution state: {error}"
                )))
            }),
        }
    }
    fn begin_step(
        &self,
        id: String,
        step_id: String,
        kind: String,
        input: Value,
    ) -> PolicyFuture<'_, Step>;
    /// Begins a step whose input is already encoded, such as a whole model request.
    // RawValue is unsized; overriding stores take ownership of the encoded text.
    #[allow(clippy::boxed_local)]
    fn begin_step_encoded(
        &self,
        id: String,
        step_id: String,
        kind: String,
        input: Box<RawValue>,
    ) -> PolicyFuture<'_, Step> {
        match serde_json::from_str(input.get()) {
            Ok(input) => self.begin_step(id, step_id, kind, input),
            Err(error) => Box::pin(async move {
                Err(NanocodexError::InvalidRequest(format!(
                    "invalid encoded Claude execution state: {error}"
                )))
            }),
        }
    }
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
    /// Persists a just-created child's first checkpoint so it is listed and
    /// resumable before its first turn. A store that reopens a restored child
    /// must keep the checkpoint it already holds. The default persists nothing.
    fn initial_checkpoint(&self, _state: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    /// Retracts state written only by [`Self::initial_checkpoint`] when the
    /// child's creation is abandoned. The default keeps it.
    fn discard_initial(&self) -> PolicyFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    /// Opens independent durable state for a fork, side conversation or
    /// subagent of this session before it starts (or reopens it for a restored
    /// subagent), so the child is resumable on its own.
    ///
    /// The returned policy's `state_id` must equal `child.session_id`; a
    /// fork's inherited transcript is committed as its first checkpoint.
    /// `None` (the default) means the policy cannot persist children; the
    /// spawn or fork then fails with `NanocodexError::ExecutionPolicyBranchUnsupported`,
    /// exactly as for Codex, instead of silently creating an ephemeral child.
    fn branch(&self, _child: &SessionInfo) -> Result<Option<Arc<dyn ClaudeExecutionPolicy>>> {
        Ok(None)
    }
}
