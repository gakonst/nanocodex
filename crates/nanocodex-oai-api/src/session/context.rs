use std::collections::HashSet;

use crate::{
    ContentItem, FunctionOutputBody, FunctionOutputContent, MessageRole, ResponseItem,
    ResponseItemId, Usage, responses::ResponseHistory,
};

use super::compaction;

const TOOL_OUTPUT_TOKEN_LIMIT: usize = 12_000;
// Changing this value would change model-visible IDs and invalidate prompt caches.
const SYNTHETIC_OUTPUT_ID_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0x90d38d3e_6a5b_4d52_bfe2_2f1e634bfac4);

/// Typed model-visible transcript. The common prompt path shares its backing
/// allocation; prompt-only repairs allocate only for incomplete call pairs.
#[derive(Clone)]
pub struct ContextManager {
    items: ResponseHistory,
    last_token_usage: Option<Usage>,
    calls: CallIds,
}

#[derive(Clone, Default)]
struct CallIds {
    function_calls: HashSet<Box<str>>,
    function_outputs: HashSet<Box<str>>,
    custom_calls: HashSet<Box<str>>,
    custom_outputs: HashSet<Box<str>>,
    tool_search_calls: HashSet<Box<str>>,
    tool_search_outputs: HashSet<Box<str>>,
    non_server_tool_search_outputs: HashSet<Box<str>>,
}

impl ContextManager {
    #[must_use]
    pub fn new(items: Vec<ResponseItem>) -> Self {
        let mut context = Self {
            items: ResponseHistory::default(),
            last_token_usage: None,
            calls: CallIds::default(),
        };
        context.record_items(items);
        context
    }

    #[must_use]
    pub fn flattened_items(&self) -> Vec<ResponseItem> {
        self.items.iter().cloned().collect()
    }

    #[must_use]
    pub fn shared_items(&self) -> ResponseHistory {
        self.items.clone()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &ResponseItem> {
        self.items.iter()
    }

    pub fn record_items(&mut self, items: impl IntoIterator<Item = ResponseItem>) {
        for mut item in items
            .into_iter()
            .filter(is_api_item)
            .map(truncate_tool_output)
        {
            assign_missing_response_item_id(&mut item);
            self.calls.track(&item);
            self.items.push(item);
        }
    }

    pub fn commit_tail(&mut self) {
        self.items.commit_tail();
        if self.calls.is_balanced() {
            self.calls.clear();
        }
    }

    pub fn replace_and_recompute(&mut self, mut items: Vec<ResponseItem>, prefix: &[ResponseItem]) {
        assign_missing_response_item_ids(&mut items);
        self.items.replace(items);
        let total_tokens = prefix
            .iter()
            .chain(self.items.iter())
            .map(compaction::estimate_item_tokens)
            .fold(0, u64::saturating_add);
        self.last_token_usage = Some(Usage {
            total_tokens,
            ..Usage::default()
        });
        self.calls = CallIds::from_items(self.items.iter());
    }

    pub fn update_token_info(&mut self, usage: Option<&Usage>) {
        if let Some(usage) = usage {
            self.last_token_usage = Some(usage.clone());
        }
    }

    #[must_use]
    pub fn active_context_tokens(&self, server_reasoning_included: bool) -> u64 {
        let reported = self
            .last_token_usage
            .as_ref()
            .map_or(0, |usage| usage.total_tokens);
        let local_tail = self.items_after_last_model_generated_tokens();
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
    #[must_use]
    pub fn prompt_items(&self) -> ResponseHistory {
        self.prompt_items_with_repair().0
    }

    pub(crate) fn prompt_items_with_repair(&self) -> (ResponseHistory, bool) {
        let needs_repair = !self.calls.is_balanced();
        if !needs_repair {
            return (self.items.clone(), false);
        }

        let calls = CallIds::from_items(self.items.iter());
        let mut repaired = Vec::with_capacity(self.items.len() + 2);
        for item in &self.items {
            match item {
                ResponseItem::FunctionCall { id, call_id, .. }
                | ResponseItem::LocalShellCall {
                    id,
                    call_id: Some(call_id),
                    ..
                } => {
                    repaired.push(item.clone());
                    if !calls.function_outputs.contains(call_id.as_ref()) {
                        let mut output = ResponseItem::function_call_output(
                            call_id.to_string(),
                            FunctionOutputBody::Text("aborted".into()),
                        );
                        output.set_id(synthetic_output_id("fco", id.as_ref()));
                        repaired.push(output);
                    }
                }
                ResponseItem::CustomToolCall { id, call_id, .. } => {
                    repaired.push(item.clone());
                    if !calls.custom_outputs.contains(call_id.as_ref()) {
                        let mut output = ResponseItem::custom_tool_output(
                            call_id.to_string(),
                            None,
                            FunctionOutputBody::Text("aborted".into()),
                        );
                        output.set_id(synthetic_output_id("ctco", id.as_ref()));
                        repaired.push(output);
                    }
                }
                ResponseItem::FunctionCallOutput { call_id, .. }
                    if !calls.function_calls.contains(call_id.as_ref()) => {}
                ResponseItem::CustomToolCallOutput { call_id, .. }
                    if !calls.custom_calls.contains(call_id.as_ref()) => {}
                ResponseItem::ToolSearchCall {
                    id,
                    call_id: Some(call_id),
                    ..
                } => {
                    repaired.push(item.clone());
                    if !calls.tool_search_outputs.contains(call_id.as_ref()) {
                        repaired.push(ResponseItem::ToolSearchOutput {
                            id: synthetic_output_id("tso", id.as_ref()),
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
                    && !calls.tool_search_calls.contains(call_id.as_ref()) => {}
                _ => repaired.push(item.clone()),
            }
        }
        (ResponseHistory::new(repaired), true)
    }

    pub(crate) fn adopt_prompt_items(&mut self, items: ResponseHistory) {
        self.items = items;
        self.calls = CallIds::from_items(self.items.iter());
    }

    fn items_after_last_model_generated_tokens(&self) -> u64 {
        let mut tokens = 0_u64;
        for item in &self.items {
            if is_model_generated_item(item) {
                tokens = 0;
            } else {
                tokens = tokens.saturating_add(compaction::estimate_item_tokens(item));
            }
        }
        tokens
    }

    fn non_last_reasoning_tokens(&self) -> u64 {
        let mut reasoning = 0_u64;
        let mut before_last_user = None;
        for item in &self.items {
            if is_user_turn_boundary(item) {
                before_last_user = Some(reasoning);
            }
            if matches!(
                item,
                ResponseItem::Reasoning {
                    encrypted_content: Some(_),
                    ..
                }
            ) {
                reasoning = reasoning.saturating_add(compaction::estimate_item_tokens(item));
            }
        }
        before_last_user.unwrap_or_default()
    }
}

impl CallIds {
    fn from_items<'a>(items: impl IntoIterator<Item = &'a ResponseItem>) -> Self {
        let mut calls = Self::default();
        for item in items {
            calls.track(item);
        }
        calls
    }

    fn is_balanced(&self) -> bool {
        self.function_calls == self.function_outputs
            && self.custom_calls == self.custom_outputs
            && self.tool_search_calls.is_subset(&self.tool_search_outputs)
            && self
                .non_server_tool_search_outputs
                .is_subset(&self.tool_search_calls)
    }

    fn clear(&mut self) {
        self.function_calls.clear();
        self.function_outputs.clear();
        self.custom_calls.clear();
        self.custom_outputs.clear();
        self.tool_search_calls.clear();
        self.tool_search_outputs.clear();
        self.non_server_tool_search_outputs.clear();
    }

    fn track(&mut self, item: &ResponseItem) {
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
            ResponseItem::CustomToolCallOutput {
                call_id,
                name: None,
                ..
            } => {
                self.custom_outputs.insert(call_id.clone());
            }
            // Named outputs are progress notifications, not terminal tool results.
            ResponseItem::CustomToolCallOutput { .. } => {}
            ResponseItem::ToolSearchCall {
                call_id: Some(call_id),
                ..
            } => {
                self.tool_search_calls.insert(call_id.clone());
            }
            ResponseItem::ToolSearchOutput {
                call_id: Some(call_id),
                execution,
                ..
            } => {
                self.tool_search_outputs.insert(call_id.clone());
                if execution.as_ref() != "server" {
                    self.non_server_tool_search_outputs.insert(call_id.clone());
                }
            }
            _ => {}
        }
    }
}

pub fn assign_missing_response_item_ids(items: &mut [ResponseItem]) {
    for item in items {
        assign_missing_response_item_id(item);
    }
}

pub fn assign_missing_response_item_id(item: &mut ResponseItem) {
    if item.id().is_some_and(|id| !id.is_empty()) {
        return;
    }
    let Some(prefix) = item.id_prefix() else {
        return;
    };
    item.set_id(Some(new_response_item_id(prefix)));
}

fn new_response_item_id(prefix: &str) -> ResponseItemId {
    ResponseItemId::with_suffix(prefix, uuid::Uuid::now_v7())
}

fn synthetic_output_id(prefix: &str, source_id: Option<&ResponseItemId>) -> Option<ResponseItemId> {
    let source_id = source_id.filter(|id| !id.is_empty())?;
    let name = format!("{prefix}:{}", source_id.as_str());
    Some(ResponseItemId::with_suffix(
        prefix,
        uuid::Uuid::new_v5(&SYNTHETIC_OUTPUT_ID_NAMESPACE, name.as_bytes()),
    ))
}

#[must_use]
pub fn has_well_formed_tool_calls(items: &[ResponseItem]) -> bool {
    let mut function_calls = HashSet::new();
    let mut function_outputs = HashSet::new();
    let mut custom_calls = HashSet::new();
    let mut custom_outputs = HashSet::new();
    let mut search_calls = HashSet::new();
    let mut search_outputs = HashSet::new();
    let mut non_server_search_outputs = HashSet::new();
    for item in items {
        let valid = match item {
            ResponseItem::FunctionCall { call_id, .. }
            | ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => function_calls.insert(call_id.as_ref()),
            ResponseItem::FunctionCallOutput { call_id, .. } => {
                function_calls.contains(call_id.as_ref())
                    && function_outputs.insert(call_id.as_ref())
            }
            ResponseItem::CustomToolCall { call_id, .. } => custom_calls.insert(call_id.as_ref()),
            ResponseItem::CustomToolCallOutput { call_id, name, .. } => {
                custom_calls.contains(call_id.as_ref())
                    && (name.is_some() || custom_outputs.insert(call_id.as_ref()))
            }
            ResponseItem::ToolSearchCall {
                call_id: Some(call_id),
                ..
            } => search_calls.insert(call_id.as_ref()),
            ResponseItem::ToolSearchOutput {
                call_id: Some(call_id),
                execution,
                ..
            } => {
                search_outputs.insert(call_id.as_ref());
                execution.as_ref() == "server" || non_server_search_outputs.insert(call_id.as_ref())
            }
            ResponseItem::ToolSearchCall { .. } | ResponseItem::ToolSearchOutput { .. } => true,
            _ => true,
        };
        if !valid {
            return false;
        }
    }
    function_calls == function_outputs
        && custom_calls == custom_outputs
        && search_calls.is_subset(&search_outputs)
        && non_server_search_outputs.is_subset(&search_calls)
}

const fn is_model_generated_item(item: &ResponseItem) -> bool {
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

#[must_use]
pub fn is_contextual_user_message(item: &ResponseItem) -> bool {
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
                || matches_marked_text("<turn_aborted>", "</turn_aborted>", text)
        })
}

pub(crate) fn is_canonical_context_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message {
            role: MessageRole::Developer,
            ..
        } => true,
        ResponseItem::Message {
            role: MessageRole::User,
            content,
            ..
        } => content.iter().any(|content| {
            let ContentItem::InputText { text } = content else {
                return false;
            };
            matches_marked_text("# AGENTS.md instructions", "</INSTRUCTIONS>", text)
                || matches_marked_text("<environment_context>", "</environment_context>", text)
        }),
        _ => false,
    }
}

fn matches_marked_text(start: &str, end: &str, text: &str) -> bool {
    let text = text.trim();
    text.get(..start.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(start))
        && text
            .get(text.len().saturating_sub(end.len())..)
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(end))
}

const fn is_api_item(item: &ResponseItem) -> bool {
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
            FunctionOutputContent::InputAudio { .. } => {}
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
    fn complete_prompt_reuses_the_history_without_repair() {
        let context = ContextManager::new(vec![message("hello")]);
        let prompt = context.prompt_items();
        assert_eq!(prompt.len(), 1);
    }

    #[test]
    fn history_assigns_ids_once_and_preserves_them_across_checkpoints() {
        let mut context = ContextManager::new(vec![message("hello")]);
        let id = context
            .flattened_items()
            .into_iter()
            .next()
            .and_then(|item| item.id().cloned())
            .expect("history item should have an ID");
        assert!(id.as_str().starts_with("msg_"));

        context.commit_tail();
        let checkpoint = context.shared_items();
        assert_eq!(
            checkpoint.iter().next().and_then(ResponseItem::id),
            Some(&id)
        );
    }

    #[test]
    fn prompt_repairs_do_not_mutate_raw_history() {
        let call: ResponseItem = serde_json::from_str(
            r#"{"type":"custom_tool_call","id":"ctc_source","call_id":"missing","name":"exec","input":"code"}"#,
        )
        .unwrap();
        let orphan = ResponseItem::custom_tool_output(
            "orphan".to_owned(),
            None,
            FunctionOutputBody::Text("unused".into()),
        );
        let context = ContextManager::new(vec![call, orphan]);
        let prompt = context.prompt_items();
        let prompt: Vec<_> = prompt.iter().collect();
        assert_eq!(context.flattened_items().len(), 2);
        assert_eq!(prompt.len(), 2);
        assert!(matches!(
            &prompt[1],
            ResponseItem::CustomToolCallOutput {
                id: Some(id),
                call_id,
                output: FunctionOutputBody::Text(text),
                ..
            } if id.as_str() == "ctco_e63f89e2-4637-5644-bf21-01718d2de15e"
                && call_id.as_ref() == "missing"
                && text.as_ref() == "aborted"
        ));
    }

    #[test]
    fn notification_for_committed_custom_call_keeps_history_balanced() {
        let call: ResponseItem = serde_json::from_str(
            r#"{"type":"custom_tool_call","id":"ctc_source","call_id":"exec","name":"exec","input":"code"}"#,
        )
        .unwrap();
        let output = ResponseItem::custom_tool_output(
            "exec".to_owned(),
            None,
            FunctionOutputBody::Text("running".into()),
        );
        let mut context = ContextManager::new(vec![call, output]);
        context.commit_tail();

        context.record_items([ResponseItem::custom_tool_output(
            "exec".to_owned(),
            Some("exec".to_owned()),
            FunctionOutputBody::Text("progress".into()),
        )]);

        let (prompt, repaired) = context.prompt_items_with_repair();
        assert!(!repaired);
        assert_eq!(prompt.len(), 3);
        assert!(has_well_formed_tool_calls(
            &prompt.iter().cloned().collect::<Vec<_>>()
        ));

        let pending: ResponseItem = serde_json::from_str(
            r#"{"type":"custom_tool_call","id":"ctc_pending","call_id":"pending","name":"exec","input":"code"}"#,
        )
        .unwrap();
        context.record_items([pending]);

        let (prompt, repaired) = context.prompt_items_with_repair();
        let prompt = prompt.iter().cloned().collect::<Vec<_>>();
        assert!(repaired);
        assert!(has_well_formed_tool_calls(&prompt));
        assert!(prompt.iter().any(|item| matches!(
            item,
            ResponseItem::CustomToolCallOutput {
                call_id,
                name: None,
                output: FunctionOutputBody::Text(text),
                ..
            } if call_id.as_ref() == "exec" && text.as_ref() == "running"
        )));
        assert!(prompt.iter().any(|item| matches!(
            item,
            ResponseItem::CustomToolCallOutput {
                call_id,
                name: Some(name),
                output: FunctionOutputBody::Text(text),
                ..
            } if call_id.as_ref() == "exec"
                && name.as_ref() == "exec"
                && text.as_ref() == "progress"
        )));
    }

    #[test]
    fn orphan_server_tool_search_output_is_preserved_without_repeated_repair() {
        let mut context = ContextManager::new(vec![tool_search_output("orphan", "server")]);

        let (first, first_repaired) = context.prompt_items_with_repair();
        assert!(!first_repaired);
        assert_eq!(first.len(), 1);
        assert!(has_well_formed_tool_calls(&context.flattened_items()));

        context.commit_tail();
        let (second, second_repaired) = context.prompt_items_with_repair();
        assert!(!second_repaired);
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn server_tool_search_call_gets_a_deterministic_client_output() {
        let call = tool_search_call("missing", "server");
        let orphan = tool_search_output("other", "server");
        let mut context = ContextManager::new(vec![call, orphan]);

        let (first, first_repaired) = context.prompt_items_with_repair();
        assert!(first_repaired);
        assert_eq!(first.len(), 3);
        let synthetic_id = first
            .iter()
            .find_map(|item| match item {
                ResponseItem::ToolSearchOutput {
                    id,
                    call_id: Some(call_id),
                    execution,
                    ..
                } if call_id.as_ref() == "missing" && execution.as_ref() == "client" => id.as_ref(),
                _ => None,
            })
            .expect("missing server call should receive a client output")
            .clone();
        assert!(synthetic_id.as_str().starts_with("tso_"));

        let (repeated, repeated_repaired) = context.prompt_items_with_repair();
        assert!(repeated_repaired);
        assert_eq!(
            repeated.iter().find_map(|item| match item {
                ResponseItem::ToolSearchOutput {
                    id,
                    call_id: Some(call_id),
                    ..
                } if call_id.as_ref() == "missing" => id.as_ref(),
                _ => None,
            }),
            Some(&synthetic_id)
        );

        context.adopt_prompt_items(first);
        let (adopted, adopted_repaired) = context.prompt_items_with_repair();
        assert!(!adopted_repaired);
        assert_eq!(adopted.len(), 3);
        assert!(has_well_formed_tool_calls(&context.flattened_items()));
    }

    #[test]
    fn client_tool_search_call_can_be_paired_by_a_server_output() {
        let context = ContextManager::new(vec![
            tool_search_call("paired", "client"),
            tool_search_output("paired", "server"),
        ]);
        let (prompt, repaired) = context.prompt_items_with_repair();
        assert!(!repaired);
        assert_eq!(prompt.len(), 2);
        assert!(has_well_formed_tool_calls(&context.flattened_items()));
    }

    #[test]
    fn server_tool_search_call_can_be_paired_by_a_client_output() {
        let context = ContextManager::new(vec![
            tool_search_call("paired", "server"),
            tool_search_output("paired", "client"),
        ]);
        let (prompt, repaired) = context.prompt_items_with_repair();
        assert!(!repaired);
        assert_eq!(prompt.len(), 2);
        assert!(has_well_formed_tool_calls(&context.flattened_items()));
    }

    #[test]
    fn orphan_client_tool_search_output_is_removed() {
        let mut context = ContextManager::new(vec![tool_search_output("orphan", "client")]);
        assert!(!has_well_formed_tool_calls(&context.flattened_items()));

        let (prompt, repaired) = context.prompt_items_with_repair();
        assert!(repaired);
        assert!(prompt.is_empty());

        context.adopt_prompt_items(prompt);
        let (adopted, adopted_repaired) = context.prompt_items_with_repair();
        assert!(!adopted_repaired);
        assert!(adopted.is_empty());
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
        let history = context.flattened_items();
        let ResponseItem::CustomToolCallOutput {
            output: FunctionOutputBody::Content(output),
            ..
        } = &history[0]
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
        let agents =
            message("  # agents.md instructions\n\n<INSTRUCTIONS>\nnew\n</instructions>\n");
        assert!(is_contextual_user_message(&agents));
        assert!(is_canonical_context_item(&agents));
        assert!(!is_contextual_user_message(&message(
            "# AGENTS.md instructions are useful"
        )));
        let aborted = message("<turn_aborted>\ninterrupted\n</turn_aborted>");
        assert!(is_contextual_user_message(&aborted));
        assert!(!is_canonical_context_item(&aborted));
    }

    fn message(text: &str) -> ResponseItem {
        ResponseItem::message(
            MessageRole::User,
            [ContentItem::InputText { text: text.into() }],
        )
    }

    fn tool_search_call(call_id: &str, execution: &str) -> ResponseItem {
        ResponseItem::ToolSearchCall {
            id: Some(ResponseItemId::from_server(format!("tsc_{call_id}"))),
            call_id: Some(call_id.into()),
            status: Some("completed".into()),
            execution: execution.into(),
            arguments: serde_json::json!({ "query": "deferred" }).into(),
            internal_chat_message_metadata_passthrough: None,
        }
    }

    fn tool_search_output(call_id: &str, execution: &str) -> ResponseItem {
        ResponseItem::ToolSearchOutput {
            id: Some(ResponseItemId::from_server(format!("tso_{call_id}"))),
            call_id: Some(call_id.into()),
            status: "completed".into(),
            execution: execution.into(),
            tools: Vec::new(),
            internal_chat_message_metadata_passthrough: None,
        }
    }
}
