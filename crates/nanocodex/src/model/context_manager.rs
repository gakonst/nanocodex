use std::{collections::HashSet, sync::Arc};

use nanocodex_core::{
    ContentItem, FunctionOutputBody, FunctionOutputContent, MessageRole, ResponseItem, Usage,
};

use super::compaction;

const TOOL_OUTPUT_TOKEN_LIMIT: usize = 12_000;

/// Typed model-visible transcript. The common prompt path shares its backing
/// allocation; prompt-only repairs allocate only for incomplete call pairs.
pub(super) struct ContextManager {
    items: Arc<Vec<ResponseItem>>,
    last_token_usage: Option<Usage>,
    function_calls: HashSet<Box<str>>,
    function_outputs: HashSet<Box<str>>,
    custom_calls: HashSet<Box<str>>,
    custom_outputs: HashSet<Box<str>>,
    tool_search_calls: HashSet<Box<str>>,
    tool_search_outputs: HashSet<Box<str>>,
}

impl ContextManager {
    pub(super) fn new(items: Vec<ResponseItem>) -> Self {
        let capacity = items.len();
        let mut context = Self {
            items: Arc::new(Vec::with_capacity(capacity)),
            last_token_usage: None,
            function_calls: HashSet::new(),
            function_outputs: HashSet::new(),
            custom_calls: HashSet::new(),
            custom_outputs: HashSet::new(),
            tool_search_calls: HashSet::new(),
            tool_search_outputs: HashSet::new(),
        };
        context.record_items(items);
        context
    }

    pub(super) fn raw_items(&self) -> &[ResponseItem] {
        &self.items
    }

    pub(super) fn shared_items(&self) -> Arc<Vec<ResponseItem>> {
        Arc::clone(&self.items)
    }

    pub(super) fn len(&self) -> usize {
        self.items.len()
    }

    pub(super) fn record_items(&mut self, items: impl IntoIterator<Item = ResponseItem>) {
        for item in items
            .into_iter()
            .filter(is_api_item)
            .map(truncate_tool_output)
        {
            self.track_call_id(&item);
            Arc::make_mut(&mut self.items).push(item);
        }
    }

    pub(super) fn replace_and_recompute(
        &mut self,
        items: Vec<ResponseItem>,
        prefix: &[ResponseItem],
    ) {
        self.items = Arc::new(items);
        let total_tokens = prefix
            .iter()
            .chain(self.items.iter())
            .map(compaction::estimate_item_tokens)
            .fold(0, u64::saturating_add);
        self.last_token_usage = Some(Usage {
            total_tokens,
            ..Usage::default()
        });
        self.function_calls.clear();
        self.function_outputs.clear();
        self.custom_calls.clear();
        self.custom_outputs.clear();
        self.tool_search_calls.clear();
        self.tool_search_outputs.clear();
        for item in Arc::clone(&self.items).iter() {
            self.track_call_id(item);
        }
    }

    pub(super) fn update_token_info(&mut self, usage: Option<&Usage>) {
        if let Some(usage) = usage {
            self.last_token_usage = Some(usage.clone());
        }
    }

    pub(super) fn active_context_tokens(&self, server_reasoning_included: bool) -> u64 {
        let reported = self
            .last_token_usage
            .as_ref()
            .map_or(0, |usage| usage.total_tokens);
        let local_tail = self
            .items_after_last_model_generated_item()
            .iter()
            .map(compaction::estimate_item_tokens)
            .fold(0, u64::saturating_add);
        if server_reasoning_included {
            reported.saturating_add(local_tail)
        } else {
            reported
                .saturating_add(self.non_last_reasoning_tokens())
                .saturating_add(local_tail)
        }
    }

    /// Returns a shared prompt snapshot, allocating a repaired copy only for
    /// missing call outputs or orphan outputs.
    pub(super) fn prompt_items(&self) -> Arc<Vec<ResponseItem>> {
        let needs_repair = self.function_calls != self.function_outputs
            || self.custom_calls != self.custom_outputs
            || self.tool_search_calls != self.tool_search_outputs;
        if !needs_repair {
            return Arc::clone(&self.items);
        }

        let mut repaired = Vec::with_capacity(self.items.len() + 2);
        for item in self.items.iter() {
            match item {
                ResponseItem::FunctionCall { call_id, .. }
                | ResponseItem::LocalShellCall {
                    call_id: Some(call_id),
                    ..
                } => {
                    repaired.push(item.clone());
                    if !self.function_outputs.contains(call_id.as_ref()) {
                        repaired.push(ResponseItem::function_call_output(
                            call_id.to_string(),
                            FunctionOutputBody::Text("aborted".into()),
                        ));
                    }
                }
                ResponseItem::CustomToolCall { call_id, .. } => {
                    repaired.push(item.clone());
                    if !self.custom_outputs.contains(call_id.as_ref()) {
                        repaired.push(ResponseItem::custom_tool_output(
                            call_id.to_string(),
                            None,
                            FunctionOutputBody::Text("aborted".into()),
                        ));
                    }
                }
                ResponseItem::FunctionCallOutput { call_id, .. }
                    if !self.function_calls.contains(call_id.as_ref()) => {}
                ResponseItem::CustomToolCallOutput { call_id, .. }
                    if !self.custom_calls.contains(call_id.as_ref()) => {}
                ResponseItem::ToolSearchCall {
                    call_id: Some(call_id),
                    ..
                } => {
                    repaired.push(item.clone());
                    if !self.tool_search_outputs.contains(call_id.as_ref()) {
                        repaired.push(ResponseItem::ToolSearchOutput {
                            id: None,
                            call_id: Some(call_id.clone()),
                            status: "completed".into(),
                            execution: "client".into(),
                            tools: Vec::new(),
                            internal_chat_message_metadata_passthrough: None,
                        });
                    }
                }
                ResponseItem::ToolSearchOutput {
                    call_id: Some(call_id),
                    execution,
                    ..
                } if execution.as_ref() != "server"
                    && !self.tool_search_calls.contains(call_id.as_ref()) => {}
                _ => repaired.push(item.clone()),
            }
        }
        Arc::new(repaired)
    }

    pub(super) fn replace_last_turn_images(&mut self, placeholder: &str) -> bool {
        for item in Arc::make_mut(&mut self.items).iter_mut().rev() {
            if item.is_user_message() {
                return false;
            }
            let (ResponseItem::FunctionCallOutput {
                output: FunctionOutputBody::Content(output),
                ..
            }
            | ResponseItem::CustomToolCallOutput {
                output: FunctionOutputBody::Content(output),
                ..
            }) = item
            else {
                continue;
            };
            let mut replaced = false;
            for content in output {
                if matches!(content, FunctionOutputContent::InputImage { .. }) {
                    *content = FunctionOutputContent::InputText {
                        text: placeholder.into(),
                    };
                    replaced = true;
                }
            }
            if replaced {
                return true;
            }
        }
        false
    }

    fn items_after_last_model_generated_item(&self) -> &[ResponseItem] {
        let start = self
            .items
            .iter()
            .rposition(is_model_generated_item)
            .map_or(self.items.len(), |index| index.saturating_add(1));
        &self.items[start..]
    }

    fn non_last_reasoning_tokens(&self) -> u64 {
        let Some(last_user) = self.items.iter().rposition(is_user_turn_boundary) else {
            return 0;
        };
        self.items[..last_user]
            .iter()
            .filter(|item| {
                matches!(
                    item,
                    ResponseItem::Reasoning {
                        encrypted_content: Some(_),
                        ..
                    }
                )
            })
            .map(compaction::estimate_item_tokens)
            .fold(0, u64::saturating_add)
    }

    fn track_call_id(&mut self, item: &ResponseItem) {
        match item {
            ResponseItem::FunctionCall { call_id, .. }
            | ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => {
                self.function_calls.insert(call_id.clone());
            }
            ResponseItem::FunctionCallOutput { call_id, .. } => {
                self.function_outputs.insert(call_id.clone());
            }
            ResponseItem::CustomToolCall { call_id, .. } => {
                self.custom_calls.insert(call_id.clone());
            }
            ResponseItem::CustomToolCallOutput { call_id, .. } => {
                self.custom_outputs.insert(call_id.clone());
            }
            ResponseItem::ToolSearchCall {
                call_id: Some(call_id),
                ..
            } => {
                self.tool_search_calls.insert(call_id.clone());
            }
            ResponseItem::ToolSearchOutput {
                call_id: Some(call_id),
                ..
            } => {
                self.tool_search_outputs.insert(call_id.clone());
            }
            _ => {}
        }
    }
}

fn is_model_generated_item(item: &ResponseItem) -> bool {
    matches!(
        item,
        ResponseItem::Message {
            role: MessageRole::Assistant,
            ..
        } | ResponseItem::AgentMessage { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::FunctionCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::ContextCompaction { .. }
    )
}

fn is_user_turn_boundary(item: &ResponseItem) -> bool {
    item.is_user_message() && !is_contextual_user_message(item)
}

pub(super) fn is_contextual_user_message(item: &ResponseItem) -> bool {
    let ResponseItem::Message { content, .. } = item else {
        return false;
    };
    content
        .iter()
        .filter_map(|content| {
            let ContentItem::InputText { text } = content else {
                return None;
            };
            Some(text.as_ref())
        })
        .any(|text| {
            matches_marked_text("# AGENTS.md instructions", "</INSTRUCTIONS>", text)
                || matches_marked_text("<environment_context>", "</environment_context>", text)
        })
}

fn matches_marked_text(start: &str, end: &str, text: &str) -> bool {
    let text = text.trim();
    text.get(..start.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(start))
        && text
            .get(text.len().saturating_sub(end.len())..)
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(end))
}

fn is_api_item(item: &ResponseItem) -> bool {
    !matches!(
        item,
        ResponseItem::CompactionTrigger {} | ResponseItem::Other(_)
    )
}

fn truncate_tool_output(mut item: ResponseItem) -> ResponseItem {
    let (ResponseItem::FunctionCallOutput { output, .. }
    | ResponseItem::CustomToolCallOutput { output, .. }) = &mut item
    else {
        return item;
    };
    match output {
        FunctionOutputBody::Text(text) => {
            *text = compaction::truncate_middle_with_token_budget(text, TOOL_OUTPUT_TOKEN_LIMIT)
                .into_boxed_str();
        }
        FunctionOutputBody::Content(content) => {
            truncate_output_content(content, TOOL_OUTPUT_TOKEN_LIMIT);
        }
    }
    item
}

fn truncate_output_content(items: &mut Vec<FunctionOutputContent>, token_limit: usize) {
    let mut remaining = token_limit;
    let mut omitted_text_items = 0usize;
    let mut output = Vec::with_capacity(items.len());
    for mut item in std::mem::take(items) {
        match &mut item {
            FunctionOutputContent::InputText { text } => {
                if remaining == 0 {
                    omitted_text_items += 1;
                    continue;
                }
                let tokens = text.len().div_ceil(4);
                if tokens <= remaining {
                    remaining -= tokens;
                    output.push(item);
                } else {
                    *text = compaction::truncate_middle_with_token_budget(text, remaining)
                        .into_boxed_str();
                    if text.is_empty() {
                        omitted_text_items += 1;
                    } else {
                        output.push(item);
                    }
                    remaining = 0;
                }
            }
            FunctionOutputContent::InputImage { .. }
            | FunctionOutputContent::EncryptedContent { .. } => output.push(item),
        }
    }
    if omitted_text_items > 0 {
        output.push(FunctionOutputContent::InputText {
            text: format!("[omitted {omitted_text_items} text items ...]").into_boxed_str(),
        });
    }
    *items = output;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_prompt_reuses_the_history_allocation() {
        let context = ContextManager::new(vec![message("hello")]);
        let prompt = context.prompt_items();
        assert!(Arc::ptr_eq(&context.items, &prompt));
    }

    #[test]
    fn prompt_repairs_do_not_mutate_raw_history() {
        let call: ResponseItem = serde_json::from_str(
            r#"{"type":"custom_tool_call","call_id":"missing","name":"exec","input":"code"}"#,
        )
        .unwrap();
        let orphan = ResponseItem::custom_tool_output(
            "orphan".to_owned(),
            None,
            FunctionOutputBody::Text("unused".into()),
        );
        let context = ContextManager::new(vec![call, orphan]);
        let prompt = context.prompt_items();
        assert_eq!(context.raw_items().len(), 2);
        assert_eq!(prompt.len(), 2);
        assert!(matches!(
            &prompt[1],
            ResponseItem::CustomToolCallOutput { call_id, output: FunctionOutputBody::Text(text), .. }
                if call_id.as_ref() == "missing" && text.as_ref() == "aborted"
        ));
    }

    #[test]
    fn history_truncates_tool_text_but_preserves_images() {
        let context = ContextManager::new(vec![ResponseItem::custom_tool_output(
            "call".to_owned(),
            None,
            FunctionOutputBody::Content(vec![
                FunctionOutputContent::InputText {
                    text: "x".repeat(48_004).into_boxed_str(),
                },
                FunctionOutputContent::InputImage {
                    image_url: "data:image/png;base64,a".into(),
                    detail: None,
                },
                FunctionOutputContent::InputText {
                    text: "omitted".into(),
                },
            ]),
        )]);
        let ResponseItem::CustomToolCallOutput {
            output: FunctionOutputBody::Content(output),
            ..
        } = &context.raw_items()[0]
        else {
            panic!("expected content output")
        };
        assert!(
            matches!(&output[0], FunctionOutputContent::InputText { text } if text.contains("tokens truncated"))
        );
        assert!(matches!(
            &output[1],
            FunctionOutputContent::InputImage { .. }
        ));
        assert!(
            matches!(&output[2], FunctionOutputContent::InputText { text } if text.as_ref() == "[omitted 1 text items ...]")
        );
    }

    #[test]
    fn contextual_messages_require_start_and_end_markers() {
        assert!(is_contextual_user_message(&message(
            "  # agents.md instructions\n\n<INSTRUCTIONS>\nnew\n</instructions>\n"
        )));
        assert!(!is_contextual_user_message(&message(
            "# AGENTS.md instructions are useful"
        )));
    }

    fn message(text: &str) -> ResponseItem {
        ResponseItem::message(
            MessageRole::User,
            [ContentItem::InputText { text: text.into() }],
        )
    }
}
