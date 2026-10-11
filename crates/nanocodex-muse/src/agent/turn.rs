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
    pub(super) turn_id: String,
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
    /// Canonical turn identity shared by live events and saved native history.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.turn_id
    }

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

    /// Admits a steer with an identity for later withdrawal.
    ///
    /// # Errors
    /// See [`TurnControl::steer_with_id`].
    pub async fn steer_with_id(&self, id: String, prompt: impl Into<Prompt>) -> Result<()> {
        self.control.steer_with_id(id, prompt).await
    }

    /// Withdraws the latest steer before the next model boundary.
    ///
    /// # Errors
    /// See [`TurnControl::withdraw_steer`].
    pub async fn withdraw_steer(&self, id: String) -> Result<bool> {
        self.control.withdraw_steer(id).await
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

    /// Admits input with a caller-owned identity unique within this turn.
    /// Receipt-capable execution policies replay the same identity and input
    /// without adding a second instruction, including after recovery.
    ///
    /// # Errors
    /// Returns admission errors or an error if identified steering is unsupported.
    pub async fn steer_with_id(&self, id: String, prompt: impl Into<Prompt>) -> Result<()> {
        if id.is_empty() {
            return Err(NanocodexError::InvalidRequest(
                "steer identity must not be empty".into(),
            ));
        }
        let prompt = prompt.into();
        prompt.validate().map_err(steer_validation_error)?;
        self.backend.steer_with_id(self.key, id, prompt).await
    }

    /// Withdraws the latest accepted steer before its model boundary.
    /// Returns false if the identity is no longer latest or was already consumed.
    ///
    /// # Errors
    /// Returns an error if persistence fails, the driver stops, or withdrawal is unsupported.
    pub async fn withdraw_steer(&self, id: String) -> Result<bool> {
        self.backend.withdraw_steer(self.key, id).await
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
    pub(super) turn_id: Option<String>,
    pub(super) final_message: String,
    pub(super) usage: Option<TurnUsage>,
    pub(super) boundary: Option<TurnBoundary>,
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

    /// Canonical identity of the turn that produced this result, when known.
    #[must_use]
    pub fn turn_id(&self) -> Option<&str> {
        self.turn_id.as_deref()
    }

    /// Materializes the portable boundary this turn committed, when the
    /// backend retained one.
    ///
    /// The checkpoint contains the complete unredacted model-visible
    /// conversation, including reasoning payloads and tool inputs and outputs.
    /// Applications are responsible for protecting and retaining serialized
    /// checkpoints appropriately. Use [`crate::ForkRequest::at_turn`] to fork
    /// from this boundary without materializing it.
    #[must_use]
    pub fn checkpoint(&self) -> Option<SessionCheckpoint> {
        let checkpoint = self.boundary.as_ref()?.checkpoint().ok()?;
        Some(if checkpoint.turn_id().is_none() {
            checkpoint.with_turn_id(self.turn_id.clone())
        } else {
            checkpoint
        })
    }

    /// Backend-retained boundary used to fork at this turn.
    #[doc(hidden)]
    #[must_use]
    pub const fn boundary(&self) -> Option<&TurnBoundary> {
        self.boundary.as_ref()
    }

    /// Constructs a completed result for a backend, optionally retaining the
    /// boundary that [`Self::checkpoint`] and turn forks use.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_backend(
        request_id: Option<String>,
        final_message: String,
        usage: Option<TurnUsage>,
        boundary: Option<TurnBoundary>,
    ) -> Self {
        Self {
            request_id,
            turn_id: None,
            final_message,
            usage,
            boundary,
        }
    }

    /// Records the canonical turn identity assigned by the common handle.
    pub(super) fn with_turn_id(mut self, turn_id: String) -> Self {
        if self.turn_id.is_none() {
            self.turn_id = Some(turn_id);
        }
        self
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
    pub const fn cancel_on_admission(mut self) -> Self {
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

/// Optional model policy for a newly spawned clean agent.
///
/// Omitted values inherit the invoking agent's settings at the model boundary
/// where the spawn command is handled. Selecting another model or family uses
/// the destination model's default effort unless an effort is supplied.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpawnOptions {
    pub(super) model: Option<Model>,
    pub(super) harness: Option<crate::HarnessFamily>,
    pub(super) harness_model: Option<crate::HarnessModel>,
    pub(super) thinking: Option<Thinking>,
    pub(super) stateless_http: bool,
}

impl SpawnOptions {
    /// Starts an inherited spawn configuration.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            model: None,
            harness: None,
            harness_model: None,
            thinking: None,
            stateless_http: false,
        }
    }

    /// Overrides the model for the new agent without changing its parent.
    #[must_use]
    pub const fn model(mut self, model: Model) -> Self {
        self.model = Some(model);
        self.harness_model = Some(crate::HarnessModel::Codex(model));
        self
    }

    /// Selects the new child's native agent-loop family.
    #[must_use]
    pub const fn harness(mut self, family: crate::HarnessFamily) -> Self {
        self.harness = Some(family);
        self
    }

    /// Selects a model scoped to its native family.
    #[must_use]
    pub const fn harness_model(mut self, model: crate::HarnessModel) -> Self {
        self.harness_model = Some(model);
        self.model = match model {
            crate::HarnessModel::Codex(model) => Some(model),
            _ => None,
        };
        self
    }

    /// Returns the requested family, before parent inheritance.
    #[must_use]
    pub const fn selected_harness(&self) -> Option<crate::HarnessFamily> {
        self.harness
    }

    /// Returns the requested family-scoped model.
    #[must_use]
    pub const fn selected_harness_model(&self) -> Option<crate::HarnessModel> {
        self.harness_model
    }

    /// Rejects a model that belongs to a different explicitly selected family.
    pub fn validate_harness(&self) -> Result<()> {
        if let (Some(family), Some(model)) = (self.harness, self.harness_model)
            && family != model.family()
        {
            return Err(NanocodexError::InvalidRequest(
                "model does not belong to selected harness".into(),
            ));
        }
        if let (Some(model), Some(thinking)) = (self.harness_model, self.thinking) {
            model
                .capabilities(crate::ModelTransport::Native)
                .check_thinking(thinking)
                .map_err(crate::error::model_capability_error)?;
        }
        Ok(())
    }

    /// Resolves defaults within one family without inheriting another family's model.
    pub fn resolve(self, parent: crate::HarnessModel, parent_thinking: Thinking) -> Result<Self> {
        self.validate_harness()?;
        let family = self.harness.unwrap_or(parent.family());
        let model = self.harness_model.unwrap_or_else(|| {
            if family == parent.family() {
                parent
            } else {
                family.default_model()
            }
        });
        if model.family() != family {
            return Err(NanocodexError::InvalidRequest(
                "model does not belong to selected harness".into(),
            ));
        }
        let thinking = self.thinking.unwrap_or_else(|| {
            if family == parent.family() && model == parent {
                parent_thinking
            } else {
                model.default_thinking()
            }
        });
        let resolved = self.harness(family).harness_model(model).thinking(thinking);
        resolved.validate_harness()?;
        Ok(resolved)
    }

    /// Overrides the reasoning effort for the new agent without changing its parent.
    #[must_use]
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.thinking = Some(thinking);
        self
    }
    /// Uses full-history HTTP for this child without changing its parent transport.
    /// Intended for host-owned routes to providers without Responses WebSockets.
    #[doc(hidden)]
    #[must_use]
    pub const fn stateless_http(mut self) -> Self {
        self.stateless_http = true;
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
        service_tier: Option<ServiceTier>,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
    Steer {
        key: TurnKey,
        prompt: Prompt,
        result: oneshot::Sender<Result<()>>,
    },
    SteerWithId {
        key: TurnKey,
        id: String,
        prompt: Prompt,
        result: oneshot::Sender<Result<()>>,
    },
    WithdrawSteer {
        key: TurnKey,
        id: String,
        result: oneshot::Sender<Result<bool>>,
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
        origin: crate::Origin,
        point: ForkFrom,
        session_id: SessionId,
        policy: Option<Arc<dyn execution::ExecutionPolicy>>,
        result: oneshot::Sender<Result<(Nanocodex, AgentEvents)>>,
    },
    /// Captures this driver's identity and latest committed boundary without
    /// waiting for an active turn.
    ChildSnapshot {
        result: oneshot::Sender<Result<ChildState>>,
    },
    Spawn {
        restore: Option<ChildState>,
        options: SpawnOptions,
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
    SetServiceTier {
        service_tier: ServiceTier,
        result: oneshot::Sender<Result<()>>,
    },
    Compact {
        parent: Option<tracing::Span>,
        result: oneshot::Sender<Result<()>>,
    },
    AppendDeveloperMessage {
        text: String,
        result: oneshot::Sender<Result<AgentSessionContext>>,
    },
    Context {
        result: oneshot::Sender<Result<AgentSessionContext>>,
    },
    Shutdown,
}

/// Driver-resolved boundary a fork starts from.
#[cfg(feature = "openai")]
pub(super) enum ForkFrom {
    /// The latest committed boundary held by the driver.
    Latest,
    /// A live boundary retained by a completed turn of this conversation.
    Live(Arc<CommittedSession>),
    /// A decoded portable checkpoint of this conversation tree.
    Snapshot(Box<SessionSnapshot>),
}

#[cfg(feature = "openai")]
#[derive(Clone)]
pub(super) enum ExecutionOperation {
    Caller(String),
    Automatic(String),
    Admitted(String),
    Recovered(String),
}

#[cfg(feature = "openai")]
impl ExecutionOperation {
    pub(super) fn into_id(self) -> String {
        match self {
            Self::Caller(operation_id)
            | Self::Automatic(operation_id)
            | Self::Admitted(operation_id)
            | Self::Recovered(operation_id) => operation_id,
        }
    }

    pub(super) fn id(&self) -> &str {
        match self {
            Self::Caller(operation_id)
            | Self::Automatic(operation_id)
            | Self::Admitted(operation_id)
            | Self::Recovered(operation_id) => operation_id,
        }
    }

    pub(super) const fn is_recovered(&self) -> bool {
        matches!(self, Self::Recovered(_))
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
        execution_operation: Option<ExecutionOperation>,
        thinking: Thinking,
        service_tier: ServiceTier,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
    Cancelled {
        key: TurnKey,
        prompt: Prompt,
        execution_operation: Option<ExecutionOperation>,
        cancellation_committed: bool,
        thinking: Thinking,
        service_tier: ServiceTier,
        parent: Option<tracing::Span>,
        events: EventSink,
        result: oneshot::Sender<Result<TurnResult>>,
    },
}
