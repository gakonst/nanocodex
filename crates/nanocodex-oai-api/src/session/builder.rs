use std::sync::Arc;

use ::tower::Service;
use tokio::sync::mpsc;

use crate::{
    ContentItem, EventSink, MessageRole, OpenAi, ResponseItem, ResponsesAttempt, ResponsesClient,
    ResponsesServiceResponse, Thinking, ToolDefinition, TransportStats,
    openai::{ResponsesServiceFactory, StandardServiceFactory},
    responses::RequestProfile,
};

use super::{
    context::assign_missing_response_item_id,
    response::{
        CompletedCompaction, Response, ResponseError, ResponseInput, run_compact, run_create,
    },
    state::{ManagedSessionState, SessionId},
};

const RESPONSE_EVENT_CAPACITY: usize = 64;

/// Builder for one instruction-bound managed Responses session.
#[derive(Clone)]
pub struct SessionBuilder<F = StandardServiceFactory> {
    openai: OpenAi<F>,
    instructions: Arc<str>,
    session_id: Option<SessionId>,
    prompt_cache_key: Option<String>,
    tools: Vec<ToolDefinition>,
}

impl<F> SessionBuilder<F>
where
    F: ResponsesServiceFactory,
{
    pub(crate) const fn new(openai: OpenAi<F>, instructions: Arc<str>) -> Self {
        Self {
            openai,
            instructions,
            session_id: None,
            prompt_cache_key: None,
            tools: Vec::new(),
        }
    }

    /// Sets the client-side session identity used for tracing and cache
    /// lineage.
    #[must_use]
    pub const fn session_id(mut self, session_id: SessionId) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Sets the stable cache key for the immutable instructions and tools.
    #[must_use]
    pub fn prompt_cache_key(mut self, prompt_cache_key: impl Into<String>) -> Self {
        self.prompt_cache_key = Some(prompt_cache_key.into());
        self
    }

    /// Installs the complete tool definitions sent to the model.
    ///
    /// This is the protocol-level API for callers that execute tool calls
    /// themselves. [`nanocodex-tools`](https://docs.rs/nanocodex-tools)
    /// provides a registry and concrete runtimes for applications that want
    /// Nanocodex to dispatch the calls.
    ///
    /// ```
    /// use nanocodex_oai_api::{
    ///     OpenAi,
    ///     responses::JsonSchema,
    ///     tools::ToolDefinition,
    /// };
    /// use serde_json::json;
    ///
    /// let openai = OpenAi::new("test-api-key")?;
    /// let session = openai
    ///     .instructions(
    ///         "Use lookup_region for deployment questions. Preserve exact identifiers.",
    ///     )
    ///     .tool_definitions([ToolDefinition::function(
    ///         "lookup_region",
    ///         "Return deployment metadata for one exact region identifier.",
    ///         JsonSchema::from(json!({
    ///             "type": "object",
    ///             "properties": {
    ///                 "region": { "type": "string" }
    ///             },
    ///             "required": ["region"],
    ///             "additionalProperties": false
    ///         })),
    ///     )])
    ///     .build()?;
    ///
    /// assert_eq!(session.history_len(), 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn tool_definitions(mut self, tools: impl IntoIterator<Item = ToolDefinition>) -> Self {
        self.tools = tools.into_iter().collect();
        self
    }

    /// Creates fresh service, context, and continuation state.
    ///
    /// # Errors
    ///
    /// Returns an error when instructions or the cache identity are empty.
    pub fn build(self) -> Result<Session<F::Service>, SessionBuildError> {
        if self.instructions.trim().is_empty() {
            return Err(SessionBuildError::EmptyInstructions);
        }
        let session_id = self.session_id.unwrap_or_default();
        let prompt_cache_key = self
            .prompt_cache_key
            .unwrap_or_else(|| session_id.to_string());
        if prompt_cache_key.trim().is_empty() {
            return Err(SessionBuildError::EmptyPromptCacheKey);
        }

        let mut prefix = [
            ResponseItem::additional_tools(self.tools),
            ResponseItem::message(
                MessageRole::Developer,
                [ContentItem::InputText {
                    text: self.instructions.to_string().into_boxed_str(),
                }],
            ),
        ];
        assign_request_prefix_ids(&mut prefix);
        let profile =
            RequestProfile::new(session_id.to_string(), prompt_cache_key, Arc::from(prefix));
        let config = self.openai.config().clone();
        let service = self.openai.make_service();

        Ok(Session {
            id: session_id,
            client: ResponsesClient::new(service),
            profile,
            state: ManagedSessionState::new(Vec::new()),
            canonical_context: Vec::new(),
            canonical_context_reinjection_pending: false,
            next_call_index: 1,
            next_logical_turn: 1,
            thinking: config.thinking,
            fast_mode: config.fast_mode,
            estimate_cost: config.estimate_cost,
            transport_stats: Arc::new(TransportStats::default()),
        })
    }
}

/// Invalid managed-session construction.
#[derive(Debug, thiserror::Error)]
pub enum SessionBuildError {
    /// Stable developer instructions were empty.
    #[error("OpenAI session instructions must not be empty")]
    EmptyInstructions,
    /// The explicit prompt-cache identity was empty.
    #[error("OpenAI prompt cache key must not be empty")]
    EmptyPromptCacheKey,
}

/// One managed `OpenAI` Responses conversation.
///
/// The session owns its concrete Tower service, persistent transport,
/// authoritative typed history, usage, and private continuation state.
pub struct Session<S> {
    pub(super) id: SessionId,
    pub(super) client: ResponsesClient<S>,
    pub(super) profile: RequestProfile,
    pub(super) state: ManagedSessionState,
    pub(super) canonical_context: Vec<ResponseItem>,
    pub(super) canonical_context_reinjection_pending: bool,
    pub(super) next_call_index: u32,
    next_logical_turn: u64,
    pub(super) thinking: Thinking,
    pub(super) fast_mode: bool,
    pub(super) estimate_cost: bool,
    pub(super) transport_stats: Arc<TransportStats>,
}

impl<S> Session<S> {
    /// Returns the client-side session identity.
    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// Starts one logical agent turn.
    ///
    /// Every `create` and `compact` call made through the returned value shares
    /// turn-scoped protocol state. A compaction before the first completed
    /// `create` is pre-turn; a compaction after one is mid-turn. Dropping the
    /// value ends the boundary.
    pub const fn turn(&mut self) -> ResponseTurn<'_, S> {
        let logical_turn = self.next_logical_turn;
        self.next_logical_turn = self.next_logical_turn.saturating_add(1);
        ResponseTurn {
            session: self,
            logical_turn,
            completed_generation: false,
        }
    }

    /// Returns the number of committed typed history items.
    #[must_use]
    pub fn history_len(&self) -> usize {
        self.state.history_len()
    }

    /// Iterates over committed authoritative history without exposing mutable
    /// access.
    pub fn history(&self) -> impl ExactSizeIterator<Item = &ResponseItem> {
        self.state.history()
    }

    /// Returns the best available estimate of tokens in the active provider
    /// context.
    ///
    /// The session combines completed provider usage with locally appended
    /// items and retained reasoning according to the transport metadata it has
    /// observed. Higher-level agents use this summary to decide *when* to call
    /// [`ResponseTurn::compact`].
    #[must_use]
    pub fn active_context_tokens(&self) -> u64 {
        self.state.active_context_tokens()
    }
}

/// Turn-scoped Responses operations borrowing one managed session.
pub struct ResponseTurn<'session, S> {
    pub(super) session: &'session mut Session<S>,
    pub(super) logical_turn: u64,
    pub(super) completed_generation: bool,
}

impl<S> ResponseTurn<'_, S> {
    /// Returns the session's best available active-context token estimate.
    #[must_use]
    pub fn active_context_tokens(&self) -> u64 {
        self.session.active_context_tokens()
    }
}

#[cfg(not(target_family = "wasm"))]
impl<S> ResponseTurn<'_, S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send,
    S::Error: Into<ResponseError> + Send,
    S::Future: Send,
{
    /// Starts one streamed `response.create` operation.
    pub fn create(&mut self, input: impl Into<ResponseInput>) -> Response<'_> {
        let (sink, raw_events) = EventSink::channel(self.session.profile.session_id().to_owned());
        drop(raw_events);
        let (response_events, events) = mpsc::channel(RESPONSE_EVENT_CAPACITY);
        let run = Box::pin(run_create(self, input.into(), sink, response_events));
        Response::new(events, run)
    }

    /// Executes `response.compact` and atomically installs its completed
    /// history replacement.
    ///
    /// Pre-turn compaction defers the session's last caller-supplied developer,
    /// `AGENTS.md`, and environment-context snapshot until the next normal
    /// `create`. Mid-turn compaction installs that snapshot immediately at the
    /// model-trained boundary before the last real user message. The standalone
    /// session never reads the filesystem to refresh this fallback.
    ///
    /// # Errors
    ///
    /// Returns a typed transport, protocol, or context error. Failed
    /// compaction leaves the prior history untouched.
    pub async fn compact(&mut self) -> Result<CompletedCompaction, ResponseError> {
        run_compact(self).await
    }
}

#[cfg(target_family = "wasm")]
impl<S> ResponseTurn<'_, S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse>,
    S::Error: Into<ResponseError>,
{
    /// Starts one streamed `response.create` operation.
    pub fn create(&mut self, input: impl Into<ResponseInput>) -> Response<'_> {
        let (sink, raw_events) = EventSink::channel(self.session.profile.session_id().to_owned());
        drop(raw_events);
        let (response_events, events) = mpsc::channel(RESPONSE_EVENT_CAPACITY);
        let run = Box::pin(run_create(self, input.into(), sink, response_events));
        Response::new(events, run)
    }

    /// Executes `response.compact` and atomically installs its completed
    /// history replacement.
    ///
    /// Pre-turn compaction defers the session's last caller-supplied developer,
    /// `AGENTS.md`, and environment-context snapshot until the next normal
    /// `create`. Mid-turn compaction installs that snapshot immediately at the
    /// model-trained boundary before the last real user message. The standalone
    /// session never reads the filesystem to refresh this fallback.
    ///
    /// # Errors
    ///
    /// Returns a typed transport, protocol, or context error. Failed
    /// compaction leaves the prior history untouched.
    pub async fn compact(&mut self) -> Result<CompletedCompaction, ResponseError> {
        run_compact(self).await
    }
}

fn assign_request_prefix_ids(prefix: &mut [ResponseItem]) {
    for item in prefix {
        if matches!(
            item,
            ResponseItem::AdditionalTools { .. }
                | ResponseItem::Message {
                    role: MessageRole::Developer,
                    ..
                }
        ) {
            item.strip_id();
            continue;
        }
        assign_missing_response_item_id(item);
    }
}
