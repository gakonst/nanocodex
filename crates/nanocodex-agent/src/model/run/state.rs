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
    pub(super) context_window: nanocodex_oai_api::session::ContextWindow,
    pending_rollovers: Vec<(
        nanocodex_oai_api::session::ContextWindow,
        nanocodex_oai_api::responses::ResponseHistory,
    )>,
}

impl ConversationState {
    pub(super) fn empty(
        canonical_context: ResponseItem,
        session_id: nanocodex_oai_api::session::SessionId,
    ) -> Self {
        Self {
            canonical_context: Arc::new(canonical_context),
            managed: ManagedSessionState::new(Vec::new()),
            continuation_policy: None,
            context_window: nanocodex_oai_api::session::ContextWindow::initial_for_agent(
                session_id,
            ),
            pending_rollovers: Vec::new(),
        }
    }

    pub(super) fn new(
        history: Vec<ResponseItem>,
        session_id: nanocodex_oai_api::session::SessionId,
    ) -> Result<Self> {
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
            context_window: nanocodex_oai_api::session::ContextWindow::initial_for_agent(
                session_id,
            ),
            pending_rollovers: Vec::new(),
        })
    }

    pub(super) fn resume(
        mut canonical_context: ResponseItem,
        history: Vec<ResponseItem>,
        context_window: Option<nanocodex_oai_api::session::ContextWindow>,
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
            context_window: context_window
                .unwrap_or_else(nanocodex_oai_api::session::ContextWindow::new_for_agent),
            pending_rollovers: Vec::new(),
        })
    }

    pub(super) fn flattened_history(&self) -> Vec<ResponseItem> {
        self.managed.flattened_history()
    }

    pub(super) fn canonical_context(&self) -> &ResponseItem {
        &self.canonical_context
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

    pub(super) fn pending_rollovers(
        &self,
    ) -> &[(
        nanocodex_oai_api::session::ContextWindow,
        nanocodex_oai_api::responses::ResponseHistory,
    )] {
        &self.pending_rollovers
    }

    pub(super) fn advance_context_window(&mut self) {
        self.context_window.advance_for_agent();
        self.pending_rollovers
            .push((self.context_window.clone(), self.shared_history()));
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

    pub(super) fn commit_tail(&mut self) {
        self.managed.commit_tail();
    }

    pub(super) fn context_remaining(&self, config: &ModelConfig) -> Option<u64> {
        config.token_budget.as_ref().map(|_| {
            self.base_context_limit(config)
                .saturating_sub(self.active_context_tokens())
        })
    }

    pub(super) const fn base_context_limit(&self, config: &ModelConfig) -> u64 {
        compaction::auto_compact_token_limit(config.model, config.context_window_tokens)
    }

    pub(super) fn effective_context_limit(&self, config: &ModelConfig) -> u64 {
        let base = self.base_context_limit(config);
        let buffer = config
            .token_budget
            .as_ref()
            .and_then(|budget| {
                budget
                    .auto_compact_fallback_prompt
                    .as_ref()
                    .map(|_| budget.auto_compact_fallback_buffer_tokens.unwrap_or(0))
            })
            .unwrap_or(0);
        base.saturating_add(buffer)
            .min(config.context_window_tokens)
    }

    pub(super) fn take_token_budget_fragments(
        &mut self,
        config: &ModelConfig,
        allow_fallback: bool,
    ) -> Vec<ResponseItem> {
        let Some(budget) = config.token_budget.as_ref() else {
            return Vec::new();
        };
        let remaining = self
            .base_context_limit(config)
            .saturating_sub(self.active_context_tokens());
        let mut items = Vec::with_capacity(2);
        if budget
            .reminder_threshold_tokens
            .is_some_and(|threshold| remaining <= threshold)
            && self.context_window.claim_reminder_for_agent()
        {
            items.push(contextual_developer_message(
                budget
                    .reminder_message_template
                    .replace("{n_remaining}", &remaining.to_string()),
                "token_budget.reminder",
            ));
        }
        if allow_fallback
            && remaining == 0
            && let Some(prompt) = budget.auto_compact_fallback_prompt.as_deref()
            && self.context_window.claim_fallback_for_agent()
        {
            items.push(contextual_developer_message(
                prompt,
                "compaction.auto_fallback_prompt",
            ));
        }
        items
    }

    pub(super) fn token_budget_window_context(&self, config: &ModelConfig) -> Vec<ResponseItem> {
        let Some(budget) = config.token_budget.as_ref() else {
            return Vec::new();
        };
        let window = &self.context_window;
        let mut lines = vec![
            format!("First context window id: {}", window.first_id()),
            format!("Current context window id: {}", window.current_id()),
        ];
        if let Some(previous) = window.previous_id() {
            lines.push(format!("Previous context window id: {previous}"));
        }
        let mut items = vec![contextual_developer_message(
            format!("<context_window>\n{}\n</context_window>", lines.join("\n")),
            "token_budget.context_window",
        )];
        if let Some(guidance) = budget.guidance_message.as_deref() {
            items.push(contextual_developer_message(
                format!("<context_window_guidance>\n{guidance}\n</context_window_guidance>"),
                "token_budget.context_window_guidance",
            ));
        }
        items
    }

    pub(super) fn retained_developer_context(&self) -> Vec<ResponseItem> {
        const RETAINED_TOKEN_BUDGET: u64 = 64_000;
        let mut remaining = RETAINED_TOKEN_BUDGET;
        let mut retained = Vec::new();
        for item in self.flattened_history().into_iter().rev().filter(|item| {
            item.is_developer_message()
                && !item.message_content_item_kinds().is_some_and(|kinds| {
                    kinds.iter().any(|kind| {
                        kind.as_str().starts_with("token_budget.")
                            || kind.as_str() == "compaction.auto_fallback_prompt"
                    })
                })
        }) {
            let tokens = compaction::estimate_item_tokens(&item).max(1);
            if tokens > remaining {
                continue;
            }
            remaining = remaining.saturating_sub(tokens);
            retained.push(item);
        }
        retained.reverse();
        retained
    }

    pub(super) fn install_initial_token_budget_context(&mut self, config: &ModelConfig) {
        if config.token_budget.is_none() || !self.context_window.claim_context_for_agent() {
            return;
        }
        let context = self.token_budget_window_context(config);
        if context.is_empty() {
            return;
        }
        let mut history = self.flattened_history();
        let insertion = history
            .iter()
            .position(ResponseItem::is_user_message)
            .unwrap_or(history.len());
        history.splice(insertion..insertion, context);
        self.managed = ManagedSessionState::new(history);
    }

    pub(super) fn rebase_context_window(
        &mut self,
        session_id: nanocodex_oai_api::session::SessionId,
        config: &ModelConfig,
    ) {
        self.context_window =
            nanocodex_oai_api::session::ContextWindow::initial_for_agent(session_id);
        self.pending_rollovers.clear();
        if config.token_budget.is_none() {
            return;
        }
        let history = self
            .flattened_history()
            .into_iter()
            .filter(|item| {
                !item.message_content_item_kinds().is_some_and(|kinds| {
                    kinds.iter().any(|kind| {
                        kind.as_str().starts_with("token_budget.")
                            || kind.as_str() == "compaction.auto_fallback_prompt"
                    })
                })
            })
            .collect();
        self.managed = ManagedSessionState::new(history);
        self.install_initial_token_budget_context(config);
    }

    pub(super) const fn request_new_context(&mut self) {
        self.context_window.request_new_context_for_agent();
    }

    pub(super) fn take_new_context_request(&mut self) -> bool {
        self.context_window.take_new_context_request_for_agent()
    }

    pub(super) fn start_fresh_context(
        &mut self,
        canonical_context: ResponseItem,
        config: &ModelConfig,
    ) {
        let mut initial = self.retained_developer_context();
        if initial.is_empty() {
            initial.push(developer_context());
        }
        initial.push(canonical_context.clone());
        self.context_window.advance_for_agent();
        let _ = self.context_window.claim_context_for_agent();
        let context = self.token_budget_window_context(config);
        let insertion = initial
            .iter()
            .position(ResponseItem::is_user_message)
            .unwrap_or(initial.len());
        initial.splice(insertion..insertion, context);
        self.managed = ManagedSessionState::new(initial);
        self.managed.reset_for_full_request();
        self.canonical_context = Arc::new(canonical_context);
        self.pending_rollovers
            .push((self.context_window.clone(), self.shared_history()));
    }
}

fn contextual_developer_message(text: impl Into<Box<str>>, kind: &'static str) -> ResponseItem {
    let mut item = ResponseItem::message(MessageRole::Developer, [ContentItem::input_text(text)]);
    item.set_message_content_item_kind(kind);
    item
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_budget_reminder_and_fallback_are_one_shot_with_a_buffered_limit() {
        let mut config = ModelConfig {
            context_window_tokens: 100,
            token_budget: Some(nanocodex_oai_api::session::TokenBudgetConfig {
                reminder_threshold_tokens: Some(10),
                reminder_message_template: "Only {n_remaining} remain.".to_owned(),
                guidance_message: None,
                auto_compact_fallback_prompt: Some("Preserve state now.".to_owned()),
                auto_compact_fallback_buffer_tokens: Some(10),
            }),
            ..ModelConfig::default()
        };
        config.model = Model::Sol;
        let mut conversation = ConversationState::new(
            vec![ResponseItem::message(
                MessageRole::User,
                [ContentItem::input_text("task")],
            )],
            nanocodex_oai_api::session::SessionId::new(),
        )
        .expect("user context is valid");
        conversation.update_token_info(Some(&Usage {
            input_tokens: 90,
            total_tokens: 90,
            ..Usage::default()
        }));

        assert_eq!(conversation.base_context_limit(&config), 90);
        assert_eq!(conversation.effective_context_limit(&config), 100);
        let first = conversation.take_token_budget_fragments(&config, true);
        assert_eq!(first.len(), 2);
        let encoded = serde_json::to_string(&first).expect("fragments serialize");
        assert!(encoded.contains("Only 0 remain."));
        assert!(encoded.contains("Preserve state now."));
        conversation.append(first);
        conversation.append([ResponseItem::message(
            MessageRole::Developer,
            [ContentItem::input_text("caller-owned developer state")],
        )]);
        let retained = serde_json::to_string(&conversation.retained_developer_context())
            .expect("retained context serializes");
        assert!(retained.contains("caller-owned developer state"));
        assert!(!retained.contains("Only 0 remain."));
        assert!(!retained.contains("Preserve state now."));
        assert!(
            conversation
                .take_token_budget_fragments(&config, true)
                .is_empty()
        );
    }

    #[test]
    fn token_budget_fork_rebases_window_and_replaces_parent_marker() {
        let config = ModelConfig {
            token_budget: Some(nanocodex_oai_api::session::TokenBudgetConfig::default()),
            ..ModelConfig::default()
        };
        let parent = nanocodex_oai_api::session::SessionId::new();
        let child = nanocodex_oai_api::session::SessionId::new();
        let mut conversation = ConversationState::new(
            vec![ResponseItem::message(
                MessageRole::User,
                [ContentItem::input_text("inherited task")],
            )],
            parent,
        )
        .expect("user context is valid");
        conversation.install_initial_token_budget_context(&config);
        conversation.rebase_context_window(child, &config);

        assert_eq!(conversation.context_window.first_id(), child);
        assert_eq!(conversation.context_window.current_id(), child);
        assert_eq!(conversation.context_window.number(), 0);
        let encoded = serde_json::to_string(&conversation.flattened_history())
            .expect("serialize rebased history");
        assert!(encoded.contains("inherited task"));
        assert!(encoded.contains(&child.to_string()));
        assert!(!encoded.contains(&parent.to_string()));
        assert_eq!(encoded.matches("token_budget.context_window").count(), 1);
    }
}
