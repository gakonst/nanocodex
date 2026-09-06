use super::backend::{BackendFuture, BackendTurnKey, LifecycleBackend};
use super::*;
use nanocodex_oai_api::PromptValidationError;

/// Completion handle for an accepted turn.
///
/// A turn is both a [`Future`] for its final typed result and a [`Stream`] of
/// optional per-turn events. Result readiness is independent from consuming or
/// closing that event stream.
///
/// Dropping this handle does not cancel the accepted turn. Use [`Self::cancel`]
/// before dropping it when the work should stop.
#[must_use = "a turn continues running when dropped; await result(), control it, or explicitly drop it"]
pub struct Turn {
    pub(super) control: TurnControl,
    pub(super) request_id: Option<String>,
    pub(super) events: AgentEvents,
    pub(super) result: BackendFuture<Result<TurnResult>>,
}

/// Outcome of routing live user input into an agent session.
///
/// Live input adapters normally want to steer the current regular turn when
/// one exists and start a new turn only when the agent is idle.
/// [`Nanocodex::route_prompt`](crate::Nanocodex::route_prompt) performs that
/// decision atomically in the agent driver and returns this outcome.
pub enum PromptRoute {
    /// The agent was idle, so the prompt started a new independently awaitable turn.
    Started(Turn),
    /// The prompt was admitted to the current turn's steering queue.
    Steered,
}

impl Turn {
    /// Returns the durable request identity selected during prompt admission.
    ///
    /// A caller-supplied [`PromptRequest::request_id`] is returned unchanged.
    /// When an execution policy generated the identity, this returns the
    /// generated or recovered journal operation ID. Agents without an attached
    /// execution policy do not assign request identities.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// Returns a cheap cloneable capability targeting this exact turn.
    #[must_use]
    pub fn control(&self) -> TurnControl {
        self.control.clone()
    }

    /// Injects additional input into this turn at its next safe model boundary.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty prompt, when this turn is queued or no
    /// longer active, when its steering queue is full, or if the driver stops.
    pub async fn steer(&self, prompt: impl Into<Prompt>) -> Result<()> {
        self.control.steer(prompt).await
    }

    /// Cancels this exact unfinished turn.
    ///
    /// A queued turn is removed before execution and acknowledged immediately;
    /// its result and terminal event retain their FIFO position behind earlier
    /// turns. An active turn waits for its model and tool resources to stop
    /// before cancellation is acknowledged.
    ///
    /// # Errors
    ///
    /// Returns an error when this turn has already finished or if the driver
    /// stops.
    pub async fn cancel(&self) -> Result<()> {
        self.control.cancel().await
    }

    /// Waits for and returns the final typed turn result.
    ///
    /// This is equivalent to awaiting the turn directly. It does not wait for
    /// the per-turn event stream to be consumed or closed. Applications that
    /// need every event should consume the independently returned
    /// [`AgentEvents`] stream.
    ///
    /// # Errors
    ///
    /// Returns the model-run failure or an error if the driver stopped early.
    pub async fn result(self) -> Result<TurnResult> {
        self.await
    }
}

impl Stream for Turn {
    type Item = AgentEvent;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.events).poll_next(context)
    }
}

impl Future for Turn {
    type Output = Result<TurnResult>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.result.as_mut().poll(context)
    }
}

/// Cheap cloneable control capability for one accepted turn.
#[derive(Clone)]
pub struct TurnControl {
    pub(super) key: BackendTurnKey,
    pub(super) backend: Arc<dyn LifecycleBackend>,
}

impl TurnControl {
    /// Injects additional input into the targeted turn.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty prompt, when the turn is not active, when
    /// its steering queue is full, or if the driver stops.
    pub async fn steer(&self, prompt: impl Into<Prompt>) -> Result<()> {
        let prompt = prompt.into();
        prompt.validate().map_err(steer_validation_error)?;
        self.backend.steer(self.key, prompt).await
    }

    /// Cancels the targeted unfinished turn.
    ///
    /// # Errors
    ///
    /// Returns an error when the turn has already finished or if the driver
    /// stops.
    pub async fn cancel(&self) -> Result<()> {
        self.backend.cancel(self.key).await
    }
}

fn steer_validation_error(error: PromptValidationError) -> NanocodexError {
    let message = match error {
        PromptValidationError::EmptyInstruction => "steer instruction must not be empty".to_owned(),
        error => error.to_string(),
    };
    NanocodexError::InvalidRequest(message)
}

#[derive(Clone, Copy, Eq, PartialEq)]
#[cfg(feature = "openai")]
pub(super) struct TurnKey(pub(super) u64);

/// Final result of a completed turn.
#[derive(Clone)]
#[non_exhaustive]
pub struct TurnResult {
    pub(super) request_id: Option<String>,
    pub(super) final_message: String,
    pub(super) usage: Option<TurnUsage>,
    #[cfg(feature = "openai")]
    pub(super) checkpoint: TurnCheckpoint,
}

#[derive(Clone)]
#[cfg(feature = "openai")]
pub(super) enum TurnCheckpoint {
    Live(Arc<CommittedSession>),
    Replayed(SessionSnapshot),
    Unavailable,
}

impl TurnResult {
    /// Returns the durable request identity selected during prompt admission.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// Returns the final assistant message for this completed turn.
    #[must_use]
    pub fn final_message(&self) -> &str {
        &self.final_message
    }

    /// Consumes the result and returns its final assistant message.
    #[must_use]
    pub fn into_final_message(self) -> String {
        self.final_message
    }

    /// Returns exact aggregate token usage when reported by the backend.
    #[must_use]
    pub const fn usage(&self) -> Option<&TurnUsage> {
        self.usage.as_ref()
    }

    /// Returns a serializable, caller-owned session snapshot when retained by the backend.
    ///
    /// The snapshot contains the complete unredacted model-visible conversation,
    /// including reasoning payloads and tool inputs and outputs. Applications are
    /// responsible for protecting and retaining serialized snapshots appropriately.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn snapshot(&self) -> Option<SessionSnapshot> {
        #[cfg(feature = "openai")]
        match &self.checkpoint {
            TurnCheckpoint::Live(checkpoint) => Some(checkpoint.snapshot()),
            TurnCheckpoint::Replayed(snapshot) => Some(snapshot.clone()),
            TurnCheckpoint::Unavailable => None,
        }
        #[cfg(not(feature = "openai"))]
        None
    }

    /// Constructs a completed result for a backend without a transferable
    /// local session checkpoint.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_backend(
        request_id: Option<String>,
        final_message: String,
        usage: Option<TurnUsage>,
    ) -> Self {
        Self {
            request_id,
            final_message,
            usage,
            #[cfg(feature = "openai")]
            checkpoint: TurnCheckpoint::Unavailable,
        }
    }
}

impl fmt::Debug for TurnResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TurnResult")
            .field("final_message", &self.final_message)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::TurnResult;

    #[test]
    fn backend_result_can_omit_usage_and_local_snapshot() {
        let result = TurnResult::from_backend(None, "done".to_owned(), None);

        assert_eq!(result.final_message(), "done");
        assert!(result.usage().is_none());
        assert!(result.snapshot().is_none());
    }
}

/// One prompt submission with an optional execution identity.
///
/// When an execution policy is attached, the agent automatically assigns an
/// operation ID to requests that omit one. Attach a caller-owned ID when an
/// external job, webhook, or host retry resubmits the same logical operation.
#[derive(Clone, Debug)]
pub struct PromptRequest {
    pub(super) prompt: Prompt,
    pub(super) request_id: Option<String>,
    pub(super) cancel_on_admission: bool,
}

impl PromptRequest {
    /// Creates a prompt submission without a caller-owned operation identity.
    ///
    /// A policy-enabled agent assigns a unique operation ID before accepting
    /// this request.
    #[must_use]
    pub fn new(prompt: impl Into<Prompt>) -> Self {
        Self {
            prompt: prompt.into(),
            request_id: None,
            cancel_on_admission: false,
        }
    }

    /// Supplies a stable caller-owned request identity.
    ///
    /// When omitted, an execution policy generates an identity before the
    /// prompt is accepted. Resubmitting the same request ID with the same
    /// prompt resumes or replays that durable operation; reusing it for a
    /// different prompt is rejected as a conflict.
    #[must_use]
    pub fn request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Cancels this prompt at its durable admission boundary before model or
    /// tool work can start.
    #[doc(hidden)]
    #[must_use]
    pub fn cancel_on_admission(mut self) -> Self {
        self.cancel_on_admission = true;
        self
    }
}

impl From<Prompt> for PromptRequest {
    fn from(prompt: Prompt) -> Self {
        Self::new(prompt)
    }
}

impl From<String> for PromptRequest {
    fn from(prompt: String) -> Self {
        Self::new(prompt)
    }
}

impl From<&str> for PromptRequest {
    fn from(prompt: &str) -> Self {
        Self::new(prompt)
    }
}

/// Conversation inherited by a model-directed child agent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ForkTurns {
    /// Start with only the delegated task.
    #[default]
    None,
    /// Inherit the complete safe conversation boundary.
    All,
    /// Inherit the most recent user turns, excluding startup context.
    Last(std::num::NonZeroUsize),
}

/// Optional model policy for a newly spawned clean agent.
///
/// Omitted values inherit the invoking agent's settings at the model boundary
/// where the spawn command is handled.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpawnOptions {
    pub(super) model: Option<Model>,
    pub(super) thinking: Option<Thinking>,
}

impl SpawnOptions {
    /// Starts an inherited spawn configuration.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            model: None,
            thinking: None,
        }
    }

    /// Overrides the model for the new agent without changing its parent.
    #[must_use]
    pub const fn model(mut self, model: Model) -> Self {
        self.model = Some(model);
        self
    }

    /// Overrides the reasoning effort for the new agent without changing its parent.
    #[must_use]
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.thinking = Some(thinking);
        self
    }
    /// Returns the requested model override, when supplied.
    #[doc(hidden)]
    #[must_use]
    pub const fn selected_model(&self) -> Option<Model> {
        self.model
    }

    /// Returns the requested reasoning-effort override, when supplied.
    #[doc(hidden)]
    #[must_use]
    pub const fn selected_thinking(&self) -> Option<Thinking> {
        self.thinking
    }
}

#[cfg(feature = "openai")]
pub(super) enum Command {
    Prompt {
        key: TurnKey,
        prompt: Prompt,
        execution_operation: Option<ExecutionOperation>,
        accepted: Option<oneshot::Sender<Result<String>>>,
        cancel_on_admission: bool,
        thinking: Option<Thinking>,
        fast_mode: Option<bool>,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
    Steer {
        key: TurnKey,
        prompt: Prompt,
        result: oneshot::Sender<Result<()>>,
    },
    RoutePrompt {
        key: TurnKey,
        prompt: Prompt,
        parent: Option<tracing::Span>,
        events: EventSink,
        turn_result: oneshot::Sender<Result<TurnResult>>,
        route_result: oneshot::Sender<Result<PromptRouteKind>>,
    },
    Cancel {
        key: TurnKey,
        result: oneshot::Sender<Result<()>>,
    },
    Fork {
        checkpoint: Option<Arc<CommittedSession>>,
        result: oneshot::Sender<Result<(Nanocodex, AgentEvents)>>,
    },
    Spawn {
        options: SpawnOptions,
        fork_turns: ForkTurns,
        agent_name: Option<Arc<str>>,
        host_context: Option<Arc<str>>,
        result: oneshot::Sender<Result<(Nanocodex, AgentEvents)>>,
    },
    SpawnBatch {
        count: usize,
        observer: Option<Arc<SpawnObserver>>,
        host_context: Option<Arc<str>>,
        result: oneshot::Sender<Result<Vec<(Nanocodex, AgentEvents)>>>,
    },
    SetModel {
        model: Model,
        result: oneshot::Sender<Result<()>>,
    },
    SetThinking {
        thinking: Thinking,
        result: oneshot::Sender<Result<()>>,
    },
    SetFastMode {
        enabled: bool,
        result: oneshot::Sender<Result<()>>,
    },
    Compact {
        parent: Option<tracing::Span>,
        result: oneshot::Sender<Result<()>>,
    },
    AppendDeveloperMessage {
        text: nanocodex_oai_api::responses::ResponseItem,
        steer_active: bool,
        result: oneshot::Sender<Result<AgentSessionContext>>,
    },
    Context {
        result: oneshot::Sender<Result<AgentSessionContext>>,
    },
    Shutdown,
}

#[cfg(feature = "openai")]
pub(super) enum ExecutionOperation {
    Caller(String),
    Automatic(String),
    Admitted(String),
}

#[cfg(feature = "openai")]
impl ExecutionOperation {
    pub(super) fn into_id(self) -> String {
        match self {
            Self::Caller(operation_id)
            | Self::Automatic(operation_id)
            | Self::Admitted(operation_id) => operation_id,
        }
    }
}

#[cfg(feature = "openai")]
pub(super) enum PromptRouteKind {
    Started { request_id: Option<String> },
    Steered,
}

#[cfg(feature = "openai")]
pub(super) enum QueuedTurn {
    Pending {
        key: TurnKey,
        prompt: Prompt,
        execution_operation: Option<String>,
        thinking: Thinking,
        fast_mode: bool,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
    Cancelled {
        prompt: Prompt,
        execution_operation: Option<String>,
        cancellation_committed: bool,
        thinking: Thinking,
        fast_mode: bool,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
}
