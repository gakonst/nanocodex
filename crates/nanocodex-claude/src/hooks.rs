//! Caller-owned tool lifecycle hooks. Hooks run inside the admitted tool effect;
//! committed durable receipts bypass them on replay. Hosts must reconcile hooks
//! with external effects using the supplied stable invocation identity.
use crate::{ClaudeToolInvocation, ClaudeToolReply};
use serde_json::Value;
use std::{future::Future, pin::Pin};

/// A hook future follows the target's transport threading contract.
#[cfg(not(target_family = "wasm"))]
pub type ClaudeHookFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_family = "wasm")]
pub type ClaudeHookFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// The host decides whether a client tool may execute. Replacements are still
/// validated by that tool; they do not change its name or grant capabilities.
#[derive(Clone, Debug)]
pub enum ClaudeToolDecision {
    Allow,
    UpdateInput(Value),
    Deny(String),
}

/// Explicit host authorization and observation at the native client tool boundary.
/// No commands, settings, or permissions are loaded implicitly. Server tools run
/// at the provider and are outside this interface; enable them independently.
pub trait ClaudeToolHooks: Send + Sync {
    /// Opt in only when this policy actually handles the supplied boundary.
    /// False skips durable admission entirely; tool-only policies cost no
    /// lifecycle effect writes. Matching must be deterministic for a turn.
    fn handles_lifecycle(&self, _event: &ClaudeLifecycleEvent) -> bool {
        false
    }

    /// Runs only at a real host lifecycle boundary. The runtime owns durable
    /// admission, cancellation and checkpoint ordering. The default keeps
    /// existing tool-only policies source compatible.
    fn lifecycle<'a>(
        &'a self,
        _invocation: &'a ClaudeLifecycleInvocation,
    ) -> ClaudeHookFuture<'a, Result<ClaudeLifecycleOutcome, String>> {
        Box::pin(async { Ok(ClaudeLifecycleOutcome::default()) })
    }

    /// Runs before any client handler. Failure prevents dispatch.
    fn before<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
        invocation: &'a ClaudeToolInvocation,
    ) -> ClaudeHookFuture<'a, Result<ClaudeToolDecision, String>>;

    /// Observe the exact result, including failures. A failure here is appended
    /// to the result; it never erases evidence of a completed tool effect.
    fn after<'a>(
        &'a self,
        _name: &'a str,
        _input: &'a Value,
        _invocation: &'a ClaudeToolInvocation,
        _reply: &'a ClaudeToolReply,
    ) -> ClaudeHookFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// An actual native lifecycle boundary. Hosts must not synthesize events for
/// unsupported product workflows (for example an implicit transcript export).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "hook_event_name")]
pub enum ClaudeLifecycleEvent {
    SessionStart {
        source: String,
    },
    UserPromptSubmit {
        prompt: String,
    },
    Stop {
        stop_hook_active: bool,
        last_assistant_message: String,
    },
    PreCompact {
        trigger: String,
        custom_instructions: String,
    },
    PostCompact {
        trigger: String,
        compact_summary: String,
    },
    StopFailure {
        error: String,
        error_details: String,
    },
    SubagentStart {
        agent_id: String,
        agent_type: String,
    },
    SubagentStop {
        agent_id: String,
        agent_type: String,
        stop_hook_active: bool,
        last_assistant_message: String,
    },
    SessionEnd {
        reason: String,
    },
}
impl ClaudeLifecycleEvent {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::SessionStart { .. } => "SessionStart",
            Self::UserPromptSubmit { .. } => "UserPromptSubmit",
            Self::Stop { .. } => "Stop",
            Self::PreCompact { .. } => "PreCompact",
            Self::PostCompact { .. } => "PostCompact",
            Self::StopFailure { .. } => "StopFailure",
            Self::SubagentStart { .. } => "SubagentStart",
            Self::SubagentStop { .. } => "SubagentStop",
            Self::SessionEnd { .. } => "SessionEnd",
        }
    }
    /// Value against which a configured event matcher is evaluated.
    pub fn matcher_value(&self) -> &str {
        match self {
            Self::SessionStart { source } => source,
            Self::PreCompact { trigger, .. } | Self::PostCompact { trigger, .. } => trigger,
            Self::StopFailure { error, .. } => error,
            Self::SubagentStart { agent_type, .. } | Self::SubagentStop { agent_type, .. } => {
                agent_type
            }
            Self::SessionEnd { reason } => reason,
            Self::UserPromptSubmit { .. } | Self::Stop { .. } => "",
        }
    }
    pub const fn can_block(&self) -> bool {
        matches!(
            self,
            Self::UserPromptSubmit { .. }
                | Self::Stop { .. }
                | Self::PreCompact { .. }
                | Self::SubagentStop { .. }
        )
    }
}

/// Identity belongs to the admitted runtime effect, not to the shell process.
/// A committed outcome must be replayed without invoking the hook again. A
/// crash before commit can leave external effects uncertain; event_id lets a
/// trusted command reconcile them, but does not make arbitrary shell atomic.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ClaudeLifecycleInvocation {
    pub session_id: String,
    pub turn_id: String,
    pub event_id: String,
    pub model: String,
    pub instruction_revision: Option<u64>,
    #[serde(flatten)]
    pub event: ClaudeLifecycleEvent,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum ClaudeLifecycleDecision {
    #[default]
    Continue,
    /// Reject a prompt/compaction, or request another round at Stop.
    Block(String),
    /// An explicit continue:false requests that processing stop immediately.
    Stop(String),
}

/// Kept separate from committed model/tool content. Observation failures are
/// diagnostics; they may never replace or discard the content being observed.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ClaudeLifecycleOutcome {
    pub decision: ClaudeLifecycleDecision,
    pub additional_context: Vec<String>,
    pub diagnostics: Vec<String>,
}

/// Execute a lifecycle event within an already admitted durable operation.
/// Each registered policy has its own receipt, so a later policy failure or
/// recovery does not repeat an earlier committed policy. The caller checkpoints
/// the resulting context/decision before retiring these receipts, and selects
/// cancellation around this future to drop any in-flight command process.
///
/// This function does not create lifecycle events. Embeddings must call it at
/// their actual boundaries and retain completed content before post events.
pub async fn run_lifecycle_hooks(
    hooks: &[std::sync::Arc<dyn ClaudeToolHooks>],
    invocation: &ClaudeLifecycleInvocation,
    policy: Option<&dyn crate::execution::ClaudeExecutionPolicy>,
) -> nanocodex_agent::Result<ClaudeLifecycleOutcome> {
    use crate::execution::Step;
    use nanocodex_agent::NanocodexError;
    let serialization_error =
        |error: serde_json::Error| NanocodexError::backend("Claude lifecycle receipt", error);
    let mut combined = ClaudeLifecycleOutcome::default();
    for (index, hook) in hooks.iter().enumerate() {
        if !hook.handles_lifecycle(&invocation.event) {
            continue;
        }
        let step_id = format!("lifecycle-{}-{index}", invocation.event_id);
        let admitted = match policy {
            Some(policy) => {
                policy
                    .begin_step(
                        invocation.turn_id.clone(),
                        step_id.clone(),
                        "claude_lifecycle_hook".to_owned(),
                        serde_json::to_value(invocation).map_err(serialization_error)?,
                    )
                    .await?
            }
            None => Step::Execute,
        };
        let outcome: ClaudeLifecycleOutcome = match admitted {
            Step::Replay(value) => serde_json::from_value(value).map_err(serialization_error)?,
            Step::Execute => {
                let uncertain = if let Some(policy) = policy {
                    let intent_id = format!("{step_id}-started");
                    match policy
                        .begin_step(
                            invocation.turn_id.clone(),
                            intent_id.clone(),
                            "claude_lifecycle_intent".into(),
                            serde_json::to_value(invocation).map_err(serialization_error)?,
                        )
                        .await?
                    {
                        Step::Replay(_) => true,
                        Step::Execute => {
                            policy
                                .complete_step(
                                    invocation.turn_id.clone(),
                                    intent_id,
                                    serde_json::json!({"started":true}),
                                )
                                .await?;
                            false
                        }
                    }
                } else {
                    false
                };
                let result = if uncertain {
                    Err(format!(
                        "{} hook outcome unknown after interruption; external effects may have occurred. Reconcile event {} before retrying; hook was not repeated.",
                        invocation.event.name(),
                        invocation.event_id
                    ))
                } else {
                    hook.lifecycle(invocation).await
                };
                let outcome = match result {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let mut outcome = ClaudeLifecycleOutcome::default();
                        if matches!(
                            invocation.event,
                            ClaudeLifecycleEvent::UserPromptSubmit { .. }
                                | ClaudeLifecycleEvent::PreCompact { .. }
                        ) {
                            outcome.decision = ClaudeLifecycleDecision::Block(error);
                        } else {
                            outcome.diagnostics.push(error);
                        }
                        outcome
                    }
                };
                if let Some(policy) = policy {
                    policy
                        .complete_step(
                            invocation.turn_id.clone(),
                            step_id,
                            serde_json::to_value(&outcome).map_err(serialization_error)?,
                        )
                        .await?;
                }
                outcome
            }
        };
        combined
            .additional_context
            .extend(outcome.additional_context);
        combined.diagnostics.extend(outcome.diagnostics);
        if !matches!(outcome.decision, ClaudeLifecycleDecision::Continue) {
            if invocation.event.can_block() {
                combined.decision = outcome.decision;
                break;
            }
            combined.diagnostics.push(format!(
                "{} is observational; lifecycle decision ignored",
                invocation.event.name()
            ));
        }
    }
    Ok(combined)
}
