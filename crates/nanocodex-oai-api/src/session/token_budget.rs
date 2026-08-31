use serde::{Deserialize, Serialize};

const DEFAULT_REMINDER_MESSAGE_TEMPLATE: &str = concat!(
    "Your context window is nearly exhausted (only {n_remaining} tokens remaining) and will be automatically reset for you soon. ",
    "Once reset, message items in the current context window will be cleared, while caller-owned world state remains available."
);
const MAX_PROMPT_BYTES: usize = 2_000;

/// Optional local policy for Codex-style fresh context windows.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TokenBudgetConfig {
    /// Emit a one-time reminder at or below this remaining-token count.
    pub reminder_threshold_tokens: Option<u64>,
    /// One-shot reminder template. `{n_remaining}` is replaced with the current estimate.
    pub reminder_message_template: String,
    /// Optional guidance injected into each fresh window.
    pub guidance_message: Option<String>,
    /// Optional one-shot prompt injected when the base budget is exhausted.
    pub auto_compact_fallback_prompt: Option<String>,
    /// Extra tokens reserved so the fallback prompt can receive one response.
    pub auto_compact_fallback_buffer_tokens: Option<u64>,
}

impl TokenBudgetConfig {
    /// Rejects unusable zero-sized thresholds.
    pub fn validate(&self) -> Result<(), TokenBudgetConfigError> {
        if self.reminder_threshold_tokens == Some(0) {
            return Err(TokenBudgetConfigError::ZeroReminderThreshold);
        }
        if self.reminder_message_template.trim().is_empty() {
            return Err(TokenBudgetConfigError::EmptyReminderMessage);
        }
        if self.reminder_message_template.len() > MAX_PROMPT_BYTES {
            return Err(TokenBudgetConfigError::PromptTooLong);
        }
        if self
            .guidance_message
            .as_ref()
            .is_some_and(|message| message.len() > MAX_PROMPT_BYTES)
            || self
                .auto_compact_fallback_prompt
                .as_ref()
                .is_some_and(|message| message.len() > MAX_PROMPT_BYTES)
        {
            return Err(TokenBudgetConfigError::PromptTooLong);
        }
        if self.auto_compact_fallback_prompt.is_some()
            && self.auto_compact_fallback_buffer_tokens.is_none()
        {
            return Err(TokenBudgetConfigError::MissingFallbackBuffer);
        }
        if self.auto_compact_fallback_buffer_tokens == Some(0) {
            return Err(TokenBudgetConfigError::ZeroFallbackBuffer);
        }
        Ok(())
    }
}

impl Default for TokenBudgetConfig {
    fn default() -> Self {
        Self {
            reminder_threshold_tokens: None,
            reminder_message_template: DEFAULT_REMINDER_MESSAGE_TEMPLATE.to_owned(),
            guidance_message: None,
            auto_compact_fallback_prompt: None,
            auto_compact_fallback_buffer_tokens: None,
        }
    }
}

/// Invalid fresh-window token budget configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TokenBudgetConfigError {
    /// Reminders must leave room for a request.
    #[error("token-budget reminder threshold must be greater than zero")]
    ZeroReminderThreshold,
    /// Reminder text is required when the feature is enabled.
    #[error("token-budget reminder message must not be empty")]
    EmptyReminderMessage,
    /// Model-visible policy fragments remain deliberately small.
    #[error("token-budget prompt fragments must not exceed 2000 bytes")]
    PromptTooLong,
    /// A fallback prompt needs an explicit response reserve.
    #[error("token-budget fallback buffer is required when a fallback prompt is configured")]
    MissingFallbackBuffer,
    /// The fallback response reserve must be usable.
    #[error("token-budget fallback buffer must be greater than zero")]
    ZeroFallbackBuffer,
}

/// UUIDv7 lineage for one local context window.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextWindow {
    first_id: super::SessionId,
    previous_id: Option<super::SessionId>,
    current_id: super::SessionId,
    number: u64,
    new_context_requested: bool,
    #[serde(default)]
    context_delivered: bool,
    reminder_delivered: bool,
    fallback_delivered: bool,
}

impl ContextWindow {
    #[doc(hidden)]
    #[must_use]
    pub fn new_for_agent() -> Self {
        Self::new()
    }
    #[doc(hidden)]
    #[must_use]
    pub const fn initial_for_agent(current_id: super::SessionId) -> Self {
        Self {
            first_id: current_id,
            previous_id: None,
            current_id,
            number: 0,
            new_context_requested: false,
            context_delivered: false,
            reminder_delivered: false,
            fallback_delivered: false,
        }
    }
    #[doc(hidden)]
    #[must_use]
    pub const fn restore_for_agent(
        first_id: super::SessionId,
        previous_id: Option<super::SessionId>,
        current_id: super::SessionId,
        number: u64,
    ) -> Self {
        Self {
            first_id,
            previous_id,
            current_id,
            number,
            new_context_requested: false,
            context_delivered: true,
            reminder_delivered: false,
            fallback_delivered: false,
        }
    }
    pub(crate) fn new() -> Self {
        let current_id = super::SessionId::new();
        Self {
            first_id: current_id,
            previous_id: None,
            current_id,
            number: 0,
            new_context_requested: false,
            context_delivered: false,
            reminder_delivered: false,
            fallback_delivered: false,
        }
    }
    /// First UUIDv7 window identity in this conversation.
    #[must_use]
    pub const fn first_id(&self) -> super::SessionId {
        self.first_id
    }
    /// Previous window identity, if this window has rolled over.
    #[must_use]
    pub const fn previous_id(&self) -> Option<super::SessionId> {
        self.previous_id
    }
    /// Current UUIDv7 window identity.
    #[must_use]
    pub const fn current_id(&self) -> super::SessionId {
        self.current_id
    }
    /// Completed fresh-window replacement count.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }
    pub(crate) const fn request_new_context(&mut self) {
        self.new_context_requested = true;
    }
    #[doc(hidden)]
    pub const fn request_new_context_for_agent(&mut self) {
        self.request_new_context();
    }
    pub(crate) fn take_new_context_request(&mut self) -> bool {
        std::mem::take(&mut self.new_context_requested)
    }
    #[doc(hidden)]
    pub fn take_new_context_request_for_agent(&mut self) -> bool {
        self.take_new_context_request()
    }
    pub(crate) fn advance(&mut self) {
        self.number = self.number.saturating_add(1);
        self.previous_id = Some(self.current_id);
        self.current_id = super::SessionId::new();
        self.new_context_requested = false;
        self.context_delivered = false;
        self.reminder_delivered = false;
        self.fallback_delivered = false;
    }
    #[doc(hidden)]
    pub fn advance_for_agent(&mut self) {
        self.advance();
    }
    #[doc(hidden)]
    pub const fn claim_context_for_agent(&mut self) -> bool {
        !std::mem::replace(&mut self.context_delivered, true)
    }
    pub(crate) const fn claim_reminder(&mut self) -> bool {
        !std::mem::replace(&mut self.reminder_delivered, true)
    }
    #[doc(hidden)]
    pub const fn claim_reminder_for_agent(&mut self) -> bool {
        self.claim_reminder()
    }
    pub(crate) const fn claim_fallback(&mut self) -> bool {
        !std::mem::replace(&mut self.fallback_delivered, true)
    }
    #[doc(hidden)]
    pub const fn claim_fallback_for_agent(&mut self) -> bool {
        self.claim_fallback()
    }
}

/// One per-window token-budget action due from local accounting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenBudgetAction {
    /// The configured remaining-token reminder is due once.
    Reminder,
    /// The configured fallback reserve is due once.
    Fallback,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_window_requests_are_one_shot_and_keep_uuidv7_lineage() {
        let mut window = ContextWindow::new();
        let first = window.current_id();
        assert_eq!(first, window.first_id());
        assert_eq!(first.as_uuid().get_version_num(), 7);
        window.request_new_context();
        assert!(window.take_new_context_request());
        assert!(!window.take_new_context_request());
        window.advance();
        assert_eq!(window.previous_id(), Some(first));
        assert_eq!(window.current_id().as_uuid().get_version_num(), 7);
        assert_ne!(window.current_id(), first);
        assert!(window.claim_reminder());
        assert!(!window.claim_reminder());
        assert!(window.claim_fallback());
        assert!(!window.claim_fallback());
    }
}
