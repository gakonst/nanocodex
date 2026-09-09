use super::*;

/// The current conversation and execution position. This replaces completed
/// effect receipts; it is not a collection of historical model requests.
#[derive(Deserialize, Serialize)]
struct CurrentExecution {
    phase: ExecutionPhase,
    workspace: String,
    history: Vec<ResponseItem>,
    canonical_context: ResponseItem,
    context_baseline: ContextBaseline,
    context_usage: Option<Usage>,
    server_reasoning_included: bool,
    prefix: Vec<ResponseItem>,
    prompt_cache_key: String,
    model: String,
    effort: Thinking,
    fast_mode: bool,
    reasoning_mode: String,
    model_id_prefix: Option<String>,
    store_responses: bool,
    stats: RunStats,
    usage_reported: bool,
    usage_cost: Option<nanocodex_oai_api::pricing::EstimatedUsdCost>,
    warmup_reported: bool,
    warmup_cost: Option<nanocodex_oai_api::pricing::EstimatedUsdCost>,
    context_window_tokens: u64,
    force_compaction: bool,
    tool_call_indices: HashMap<Box<str>, u32>,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub(super) enum ExecutionPhase {
    Warmup,
    PrepareTurn,
    Generate,
    Compact,
}

impl<S> ModelRun<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<nanocodex_oai_api::ResponseError>,
    S::Future: AgentSend,
{
    pub(super) async fn restore_execution(
        &mut self,
        requested_workspace: Option<&str>,
        logical_turn: u64,
    ) -> Result<Option<(ModelSessionState, ExecutionPhase)>> {
        let Some(steps) = &self.execution_steps else {
            return Ok(None);
        };
        let Some(mut saved) = steps.continuation::<CurrentExecution>().await? else {
            return Ok(None);
        };
        if saved.stats.model_calls == u32::MAX {
            return Err(NanocodexError::InvalidExecutionPolicy(
                "invalid execution continuation".into(),
            ));
        }
        let mut session = self.empty_session(Some(&saved.workspace))?;
        session.validate_workspace(requested_workspace)?;
        self.model = saved
            .model
            .parse()
            .map_err(|error| NanocodexError::InvalidExecutionPolicy(format!("{error}")))?;
        self.thinking = saved.effort;
        self.fast_mode = saved.fast_mode;
        let config = Arc::make_mut(&mut self.config);
        config.reasoning_mode = saved
            .reasoning_mode
            .parse()
            .map_err(|error| NanocodexError::InvalidExecutionPolicy(format!("{error}")))?;
        config.model_id_prefix = saved.model_id_prefix.clone().map(Arc::from);
        config.store_responses = saved.store_responses;
        config.context_window_tokens = saved.context_window_tokens;
        self.force_compaction = saved.force_compaction;
        session.factory = session
            .factory
            .with_request_content(
                saved.prompt_cache_key,
                saved.prefix.into(),
                saved.model_id_prefix,
                saved
                    .reasoning_mode
                    .parse()
                    .map_err(|error| NanocodexError::InvalidExecutionPolicy(format!("{error}")))?,
                saved.store_responses,
            )
            .for_logical_turn(logical_turn);
        session.conversation = if saved.history.is_empty() && saved.phase == ExecutionPhase::Compact
        {
            ConversationState::empty(saved.canonical_context)
        } else {
            ConversationState::resume(saved.canonical_context, saved.history)?
        };
        session
            .conversation
            .update_token_info(saved.context_usage.as_ref());
        session
            .conversation
            .observe_server_reasoning(saved.server_reasoning_included);
        // Provider response IDs are connection-local; only the conversation is durable.
        session.conversation.reset_for_full_request();
        session.context = ContextState::new(
            self.context_source
                .project_instructions(&saved.workspace)
                .map(Arc::<str>::from),
            saved.context_baseline,
        );
        saved.stats.last_response_id = None;
        saved.stats.usage.reported = saved.usage_reported;
        saved.stats.usage.estimated_cost = saved.usage_cost;
        saved.stats.warmup_usage.reported = saved.warmup_reported;
        saved.stats.warmup_usage.estimated_cost = saved.warmup_cost;
        self.stats = saved.stats;
        self.tool_call_indices = saved.tool_call_indices;
        self.active_tools
            .as_ref()
            .expect("restored tools")
            .begin_turn();
        Ok(Some((session, saved.phase)))
    }

    pub(super) fn restore_runtime(
        &mut self,
        configured: (Arc<ModelConfig>, Model),
        logical_turn: u64,
    ) -> Result<()> {
        let recovered_settings = !Arc::ptr_eq(&self.config, &configured.0);
        self.config = configured.0;
        self.model = configured.1;
        if !recovered_settings {
            return Ok(());
        }
        // Saved request settings belong to the unfinished operation. Future
        // turns use the current runtime's instructions and tool catalog.
        if let Some(mut session) = self.session.take() {
            let current = self.attempt_factory(&session.tools)?;
            session.factory = session
                .factory
                .with_request_content(
                    current.profile().prompt_cache_key().to_owned(),
                    current.profile().shared_prefix(),
                    self.config.model_id_prefix.as_deref().map(str::to_owned),
                    self.config.reasoning_mode,
                    self.config.store_responses,
                )
                .for_logical_turn(logical_turn);
            self.session = Some(session);
        }
        Ok(())
    }

    pub(super) fn record_transport(&mut self) {
        self.stats
            .apply_transport(self.transport_stats.since(self.transport_baseline));
        self.transport_baseline = self.transport_stats.snapshot();
    }

    pub(super) async fn retain_execution(
        &mut self,
        session: &ModelSessionState,
        phase: ExecutionPhase,
    ) -> Result<()> {
        if self.execution_steps.is_none() {
            return Ok(());
        }
        self.record_transport();
        let steps = self.execution_steps.as_ref().expect("durable execution");
        let saved = CurrentExecution {
            phase,
            workspace: session.workspace.clone(),
            history: session.conversation.flattened_history(),
            canonical_context: (*session.conversation.canonical_context).clone(),
            context_baseline: session.context.baseline(),
            context_usage: session.conversation.managed.context_usage().0.cloned(),
            server_reasoning_included: session.conversation.managed.context_usage().1,
            prefix: session.factory.profile().prefix().to_vec(),
            prompt_cache_key: session.factory.profile().prompt_cache_key().to_owned(),
            model: self.model.as_str().to_owned(),
            effort: self.thinking,
            fast_mode: self.fast_mode,
            reasoning_mode: self.config.reasoning_mode.as_str().to_owned(),
            model_id_prefix: self.config.model_id_prefix.as_deref().map(str::to_owned),
            store_responses: self.config.store_responses,
            stats: self.stats.clone(),
            usage_reported: self.stats.usage.reported,
            usage_cost: self.stats.usage.estimated_cost.clone(),
            warmup_reported: self.stats.warmup_usage.reported,
            warmup_cost: self.stats.warmup_usage.estimated_cost.clone(),
            context_window_tokens: self.config.context_window_tokens,
            force_compaction: self.force_compaction,
            tool_call_indices: self.tool_call_indices.clone(),
        };
        steps.advance(&saved).await?;
        Ok(())
    }
}
