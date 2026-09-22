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
    pub(super) model: Model,
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
        let mut state = Self {
            canonical_context: Arc::new(canonical_context),
            managed,
            continuation_policy: None,
        };
        state.prepare_replay_images();
        Ok(state)
    }

    pub(super) fn prepare_replay_images(&mut self) -> bool {
        let mut history = self.managed.flattened_history();
        let history_changed = nanocodex_tools::image::prepare_history_images(&mut history);
        let context_changed = nanocodex_tools::image::prepare_history_images(std::slice::from_mut(
            Arc::make_mut(&mut self.canonical_context),
        ));
        let changed = history_changed || context_changed;
        if changed {
            self.managed.replace_prepared_history(history);
        }
        changed
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

    pub(super) fn append_client(&mut self, items: impl IntoIterator<Item = ResponseItem>) {
        self.managed.append_client(items);
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

    #[allow(dead_code, reason = "consumed by the native rollout boundary only")]
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

    pub(super) fn replace_rejected_images(&mut self) -> usize {
        self.managed.replace_rejected_images()
    }

    pub(super) fn remove_tool_definition(&mut self, definition: &Value) -> usize {
        self.managed.remove_tool_definition(definition)
    }

    pub(super) fn commit_tail(&mut self) {
        self.managed.commit_tail();
    }
}

#[cfg(test)]
mod image_replay_tests {
    use super::*;

    #[test]
    fn checkpoint_image_preparation_replaces_history_once() {
        let history: Vec<ResponseItem> = serde_json::from_value(serde_json::json!([
            {"type":"message", "role":"user", "content":[
                {"type":"input_text", "text":"context"},
                {"type":"input_image", "image_url":"data:image/png;base64,YQ=="}]}
        ]))
        .unwrap();
        // Exact in-memory checkpoints can bypass the serialized resume constructor.
        let mut state = ConversationState::new(history).unwrap();
        let revision = state.managed.history_revision();
        assert!(state.prepare_replay_images());
        assert_eq!(state.managed.history_revision(), revision + 1);
        assert!(
            !serde_json::to_string(&state.flattened_history())
                .unwrap()
                .contains("input_image")
        );
        assert!(
            !serde_json::to_string(&state.canonical_context)
                .unwrap()
                .contains("input_image")
        );
        assert!(!state.prepare_replay_images());
        assert_eq!(state.managed.history_revision(), revision + 1);
    }
}
