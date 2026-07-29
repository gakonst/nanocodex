use super::*;

pub(super) struct ModelSessionState {
    pub(super) workspace: String,
    pub(super) tools: ToolRuntime,
    pub(super) factory: ResponsesAttemptFactory,
    pub(super) conversation: ConversationState,
    pub(super) context: ContextState,
    pub(super) preserve_inherited_delta: bool,
}

impl ModelSessionState {
    pub(super) fn validate_workspace(&self, requested: Option<&str>) -> Result<()> {
        let Some(requested) = requested else {
            return Ok(());
        };
        if requested != self.workspace {
            return Err(NanocodexError::WorkspaceChanged {
                current: self.workspace.clone(),
                requested: requested.to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct ContinuationPolicy {
    pub(super) thinking: Thinking,
    pub(super) fast_mode: bool,
}

#[derive(Clone)]
pub(super) struct ConversationState {
    pub(super) canonical_context: Arc<ResponseItem>,
    pub(super) managed: ManagedSessionState,
    pub(super) continuation_policy: Option<ContinuationPolicy>,
}

impl ConversationState {
    pub(super) fn empty(canonical_context: ResponseItem) -> Self {
        Self {
            canonical_context: Arc::new(canonical_context),
            managed: ManagedSessionState::new(Vec::new()),
            continuation_policy: None,
        }
    }

    pub(super) fn new(history: Vec<ResponseItem>) -> Result<Self> {
        let canonical_context = history
            .iter()
            .find(|item| item.is_user_message())
            .cloned()
            .ok_or(NanocodexError::MalformedResponse {
                detail: "task input did not include initial context",
            })?;
        Ok(Self {
            canonical_context: Arc::new(canonical_context),
            managed: ManagedSessionState::new(history),
            continuation_policy: None,
        })
    }

    pub(super) fn resume(
        mut canonical_context: ResponseItem,
        history: Vec<ResponseItem>,
    ) -> Result<Self> {
        if !canonical_context.is_user_message() {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "canonical context must be a user message".to_owned(),
            ));
        }
        assign_missing_response_item_id(&mut canonical_context);
        let managed = ManagedSessionState::resume(history)
            .map_err(|error| NanocodexError::InvalidSessionSnapshot(error.to_string()))?;
        Ok(Self {
            canonical_context: Arc::new(canonical_context),
            managed,
            continuation_policy: None,
        })
    }

    pub(super) fn flattened_history(&self) -> Vec<ResponseItem> {
        self.managed.flattened_history()
    }

    pub(super) fn clear_delta(&mut self) {
        self.managed.clear_delta();
    }

    pub(super) fn append(&mut self, items: impl IntoIterator<Item = ResponseItem>) {
        self.managed.append(items);
    }

    pub(super) fn update_token_info(&mut self, usage: Option<&Usage>) {
        self.managed.update_token_info(usage);
    }

    pub(super) const fn observe_server_reasoning(&mut self, included: bool) {
        self.managed.observe_server_reasoning(included);
    }

    pub(super) fn active_context_tokens(&self) -> u64 {
        self.managed.active_context_tokens()
    }

    pub(super) fn prompt_history(&self) -> nanocodex_oai_api::responses::ResponseHistory {
        self.managed.prompt_history()
    }

    pub(super) fn prompt_history_with_repair(
        &self,
    ) -> (nanocodex_oai_api::responses::ResponseHistory, bool) {
        self.managed.prompt_history_with_repair()
    }

    pub(super) fn adopt_prompt_history(
        &mut self,
        history: nanocodex_oai_api::responses::ResponseHistory,
    ) {
        self.managed.adopt_prompt_history(history);
    }

    pub(super) fn shared_history(&self) -> nanocodex_oai_api::responses::ResponseHistory {
        self.managed.shared_history()
    }

    pub(super) const fn delta_start(&self) -> usize {
        self.managed.delta_start()
    }

    pub(super) fn previous_response_id(&self) -> Option<&str> {
        self.managed.previous_response_id()
    }

    pub(super) fn set_previous_response_id(&mut self, response_id: impl Into<String>) {
        self.managed.set_previous_response_id(response_id);
    }

    #[allow(dead_code, reason = "consumed by the native durability boundary only")]
    pub(super) const fn history_revision(&self) -> u64 {
        self.managed.history_revision()
    }

    pub(super) fn install_pre_turn_compaction(
        &mut self,
        item: ResponseItem,
        request_prefix: &[ResponseItem],
    ) {
        self.managed.install_compaction(item, [], request_prefix);
    }

    pub(super) fn install_mid_turn_compaction(
        &mut self,
        item: ResponseItem,
        canonical_developer_context: ResponseItem,
        canonical_context: ResponseItem,
        request_prefix: &[ResponseItem],
    ) {
        self.canonical_context = Arc::new(canonical_context.clone());
        let initial_context = [canonical_developer_context, canonical_context];
        self.managed
            .install_compaction(item, initial_context, request_prefix);
    }

    pub(super) fn append_canonical_context(
        &mut self,
        canonical_developer_context: ResponseItem,
        canonical_context: ResponseItem,
    ) {
        self.canonical_context = Arc::new(canonical_context.clone());
        self.managed
            .append([canonical_developer_context, canonical_context]);
    }

    pub(super) fn set_canonical_context(&mut self, canonical_context: ResponseItem) {
        self.canonical_context = Arc::new(canonical_context);
    }

    pub(super) fn reset_for_full_request(&mut self) {
        self.managed.reset_for_full_request();
    }

    pub(super) fn prepare_request_policy(&mut self, policy: ContinuationPolicy) {
        if self
            .continuation_policy
            .is_some_and(|previous| previous != policy)
        {
            self.reset_for_full_request();
        }
        self.continuation_policy = Some(policy);
    }

    pub(super) fn commit(&mut self) -> Result<()> {
        self.managed
            .commit()
            .map_err(|_| NanocodexError::MalformedResponse {
                detail: "completed turn did not have a response ID",
            })
    }

    pub(super) fn commit_interrupted(&mut self) {
        self.managed.commit_interrupted();
    }

    pub(super) fn commit_tail(&mut self) {
        self.managed.commit_tail();
    }
}
