// SPDX-License-Identifier: Apache-2.0

//! Private managed-history state and transcript projection.

use super::{
    session::RecentPrompt,
    transcript::{LocalEvent, TranscriptRecord, TurnId},
};
use nanocodex_agent::events::AgentEvent;
use nanocodex_managed::{
    EventHistoryPage, ManagedError, ManagedEvent, ManagedEventData, PromptContent, PromptInput,
};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) type HistoryProjection = (Vec<Arc<TranscriptRecord>>, u64, Vec<RecentPrompt>);
pub(super) type LiveManagedProjection = (Arc<TranscriptRecord>, Option<RecentPrompt>);

const PREFETCHED_HISTORY_PAGES: usize = 4;

#[derive(Default)]
pub(super) struct HistoryPrefetch {
    active_before: Option<String>,
    pages: VecDeque<(String, EventHistoryPage)>,
    replay_requested: bool,
}

impl HistoryPrefetch {
    pub(super) fn claim(&mut self, history: &HistoryWindow) -> Option<String> {
        if self.active_before.is_some() || self.pages.len() >= PREFETCHED_HISTORY_PAGES {
            return None;
        }
        let before = if let Some((_, page)) = self.pages.back() {
            page.has_more
                .then(|| page.data.first().map(|event| event.cursor.clone()))
                .flatten()
        } else {
            history.has_more.then(|| history.before.clone()).flatten()
        }?;
        self.active_before = Some(before.clone());
        Some(before)
    }

    pub(super) fn owns(&self, before: &str) -> bool {
        self.active_before.as_deref() == Some(before)
    }

    pub(super) fn fail(&mut self, before: &str) -> bool {
        if !self.owns(before) {
            return false;
        }
        self.active_before = None;
        true
    }

    pub(super) fn store(
        &mut self,
        before: &str,
        page: EventHistoryPage,
    ) -> Result<(), ManagedError> {
        if !self.owns(before) {
            return Err(ManagedError::InvalidResponse(
                "managed history prefetch lost cursor ownership",
            ));
        }
        if page.data.is_empty() && page.has_more {
            return Err(ManagedError::InvalidResponse(
                "managed history reports an empty nonterminal page",
            ));
        }
        self.active_before = None;
        self.pages.push_back((before.to_owned(), page));
        Ok(())
    }

    pub(super) fn request_replay(&mut self) {
        self.replay_requested = true;
    }

    pub(super) fn take_requested(
        &mut self,
        current_before: Option<&str>,
    ) -> Option<(String, EventHistoryPage)> {
        if !self.replay_requested
            || !self
                .pages
                .front()
                .is_some_and(|(before, _)| Some(before.as_str()) == current_before)
        {
            return None;
        }
        self.replay_requested = false;
        self.pages.pop_front()
    }

    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Default)]
pub(super) struct HistoryWindow {
    pub(super) events: Vec<ManagedEvent>,
    pub(super) before: Option<String>,
    pub(super) has_more: bool,
}

impl HistoryWindow {
    pub(super) fn retry_from(before: String) -> Self {
        Self {
            events: Vec::new(),
            before: Some(before),
            has_more: true,
        }
    }

    pub(super) fn from_page(
        requested_before: String,
        page: EventHistoryPage,
    ) -> Result<Self, ManagedError> {
        let mut window = Self::retry_from(requested_before);
        window.prepend(page)?;
        Ok(window)
    }

    pub(super) fn prepend(&mut self, page: EventHistoryPage) -> Result<(), ManagedError> {
        if page.data.is_empty() && page.has_more {
            return Err(ManagedError::InvalidResponse(
                "managed history reports an empty nonterminal page",
            ));
        }
        self.before = page.data.first().map(|event| event.cursor.clone());
        self.has_more = page.has_more;
        let mut events = page.data;
        events.append(&mut self.events);
        self.events = events;
        Ok(())
    }

    pub(super) fn prepend_window(&mut self, mut older: Self) {
        older.events.append(&mut self.events);
        self.events = older.events;
        self.before = older.before;
        self.has_more = older.has_more;
    }
}

pub(super) fn live_managed_projection(
    event: ManagedEvent,
    agent_id: &str,
    workspace: &Path,
    next_sequence: &mut u64,
) -> Result<Option<LiveManagedProjection>, ManagedError> {
    let timestamp = managed_timestamp(event.created_at, 0);
    let (record, prompt) = match event.data {
        ManagedEventData::TurnAccepted { input, .. } => {
            let text = prompt_input_text(&input);
            if text.is_empty() {
                return Ok(None);
            }
            let record = TranscriptRecord::from_local(
                *next_sequence,
                timestamp,
                LocalEvent::UserSubmitted {
                    id: TurnId::new(*next_sequence),
                    text: text.clone(),
                },
            )
            .map_err(|error| {
                ManagedError::Configuration(format!("TUI managed event error: {error}"))
            })?;
            let prompt = RecentPrompt {
                text,
                recorded_at_unix_ms: timestamp,
                session_id: agent_id.to_owned(),
                workspace: workspace.to_path_buf(),
            };
            (record, Some(prompt))
        }
        ManagedEventData::Event {
            event: nested,
            agent_id: child,
        } => (
            project_agent_record(&nested, &event.cursor, *next_sequence, timestamp)?
                .with_managed_turn_id(event.turn_id.as_deref())
                .with_managed_agent_id(child),
            None,
        ),
        ManagedEventData::TurnCompleted {
            id, final_message, ..
        } => (
            final_message_record(*next_sequence, timestamp, id, final_message)?,
            None,
        ),
        ManagedEventData::TurnFailed { id, error } => (
            stopped_turn_record(*next_sequence, timestamp, id, Some(error))?,
            None,
        ),
        ManagedEventData::TurnCancelled { id } => (
            stopped_turn_record(*next_sequence, timestamp, id, None)?,
            None,
        ),
        ManagedEventData::TurnRetryable { error, .. } => {
            let record = TranscriptRecord::from_local(
                *next_sequence,
                timestamp,
                LocalEvent::WorkerTurnFinished {
                    id: TurnId::new(*next_sequence),
                    error: Some(error),
                },
            )
            .map_err(|error| {
                ManagedError::Configuration(format!("TUI managed event error: {error}"))
            })?;
            (record, None)
        }
        ManagedEventData::AgentCreated { .. }
        | ManagedEventData::TurnCancelling { .. }
        | ManagedEventData::StreamFailed { .. } => return Ok(None),
    };
    *next_sequence = next_sequence.saturating_add(1);
    Ok(Some((Arc::new(record), prompt)))
}

pub(super) fn history_projection(
    history: Vec<ManagedEvent>,
    agent_id: &str,
    workspace: &Path,
) -> Result<HistoryProjection, ManagedError> {
    let mut sequences = HashMap::new();
    let mut next_sequence = 1;
    let (records, recent) = history_projection_with_sequences(
        &history,
        agent_id,
        workspace,
        &mut sequences,
        &mut next_sequence,
    )?;
    Ok((records, next_sequence, recent))
}

pub(super) fn history_projection_with_sequences(
    history: &[ManagedEvent],
    agent_id: &str,
    workspace: &Path,
    sequences: &mut HashMap<String, u64>,
    next_sequence: &mut u64,
) -> Result<(Vec<Arc<TranscriptRecord>>, Vec<RecentPrompt>), ManagedError> {
    let mut records = Vec::new();
    let mut recent = Vec::new();
    let initial_next_sequence = *next_sequence;
    let mut inserted_cursors = Vec::new();
    // Page boundaries can split a turn anywhere. Retain every event so a later
    // prepend can reconnect its prompt, tool call, streamed text, and terminal.
    for (index, event) in history.iter().enumerate() {
        let sequence = if let Some(sequence) = sequences.get(&event.cursor) {
            *sequence
        } else {
            let sequence = *next_sequence;
            *next_sequence = next_sequence.saturating_add(1);
            sequences.insert(event.cursor.clone(), sequence);
            inserted_cursors.push(event.cursor.clone());
            sequence
        };
        let timestamp = managed_timestamp(event.created_at, index);
        let projected = (|| -> Result<_, ManagedError> {
            match &event.data {
                ManagedEventData::TurnAccepted { input, .. } => {
                    let text = prompt_input_text(input);
                    if text.is_empty() {
                        return Ok(None);
                    }
                    let record = TranscriptRecord::from_local(
                        sequence,
                        timestamp,
                        LocalEvent::UserSubmitted {
                            id: TurnId::new(sequence),
                            text: text.clone(),
                        },
                    )
                    .map_err(|error| {
                        ManagedError::Configuration(format!("TUI history error: {error}"))
                    })?;
                    let prompt = RecentPrompt {
                        text,
                        recorded_at_unix_ms: timestamp,
                        session_id: agent_id.to_owned(),
                        workspace: workspace.to_path_buf(),
                    };
                    Ok(Some((Arc::new(record), Some(prompt))))
                }
                ManagedEventData::Event {
                    event: nested,
                    agent_id: child,
                } => Ok(Some((
                    Arc::new(
                        project_agent_record(nested, &event.cursor, sequence, timestamp)?
                            .with_managed_turn_id(event.turn_id.as_deref())
                            .with_managed_agent_id(*child),
                    ),
                    None,
                ))),
                ManagedEventData::TurnCompleted {
                    id, final_message, ..
                } => Ok(Some((
                    Arc::new(final_message_record(
                        sequence,
                        timestamp,
                        id.clone(),
                        final_message.clone(),
                    )?),
                    None,
                ))),
                ManagedEventData::TurnFailed { id, error } => Ok(Some((
                    Arc::new(stopped_turn_record(
                        sequence,
                        timestamp,
                        id.clone(),
                        Some(error.clone()),
                    )?),
                    None,
                ))),
                ManagedEventData::TurnCancelled { id } => Ok(Some((
                    Arc::new(stopped_turn_record(sequence, timestamp, id.clone(), None)?),
                    None,
                ))),
                ManagedEventData::TurnRetryable { error, .. } => {
                    let record = TranscriptRecord::from_local(
                        sequence,
                        timestamp,
                        LocalEvent::WorkerTurnFinished {
                            id: TurnId::new(sequence),
                            error: Some(error.clone()),
                        },
                    )
                    .map_err(|error| {
                        ManagedError::Configuration(format!("TUI history error: {error}"))
                    })?;
                    Ok(Some((Arc::new(record), None)))
                }
                ManagedEventData::AgentCreated { .. }
                | ManagedEventData::TurnCancelling { .. }
                | ManagedEventData::StreamFailed { .. } => Ok(None),
            }
        })();
        match projected {
            Ok(Some((record, prompt))) => {
                records.push(record);
                if let Some(prompt) = prompt {
                    recent.push(prompt);
                }
            }
            Ok(None) => {}
            Err(error) => {
                for cursor in inserted_cursors {
                    sequences.remove(&cursor);
                }
                *next_sequence = initial_next_sequence;
                return Err(error);
            }
        }
    }
    recent.reverse();
    Ok((records, recent))
}

fn stopped_turn_record(
    sequence: u64,
    timestamp: u64,
    turn_id: String,
    error: Option<String>,
) -> Result<TranscriptRecord, ManagedError> {
    TranscriptRecord::from_local(
        sequence,
        timestamp,
        LocalEvent::ManagedTurnStopped { turn_id, error },
    )
    .map_err(|error| ManagedError::Configuration(format!("TUI managed event error: {error}")))
}

fn final_message_record(
    sequence: u64,
    timestamp: u64,
    turn_id: String,
    text: String,
) -> Result<TranscriptRecord, ManagedError> {
    TranscriptRecord::from_local(
        sequence,
        timestamp,
        LocalEvent::ManagedFinalMessage { turn_id, text },
    )
    .map_err(|error| ManagedError::Configuration(format!("TUI final message error: {error}")))
}

fn project_agent_record(
    nested: &serde_json::value::RawValue,
    cursor: &str,
    sequence: u64,
    timestamp: u64,
) -> Result<TranscriptRecord, ManagedError> {
    match serde_json::from_str::<AgentEvent>(nested.get()) {
        Ok(event) => Ok(TranscriptRecord::from_agent(sequence, timestamp, event)),
        Err(error) => TranscriptRecord::from_local(
            sequence,
            timestamp,
            LocalEvent::DisplayError {
                message: format!("Could not display session update {cursor}: {error}"),
            },
        )
        .map_err(|error| ManagedError::Configuration(format!("TUI display error: {error}"))),
    }
}

fn prompt_input_text(input: &PromptInput) -> String {
    let text = match input {
        PromptInput::Text(text) => text.clone(),
        PromptInput::Content(content) => content
            .iter()
            .map(|item| match item {
                PromptContent::Text { text } => text.as_str(),
                PromptContent::Image { .. } => "[image attachment]",
                PromptContent::Audio { .. } => "[audio attachment]",
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    if let Some(receipt) = super::vault::receipt_summary(&text) {
        return receipt;
    }
    match nanocodex_voice_protocol::project_transcript(&text, false) {
        None => text,
        Some(entries) if entries.is_empty() => String::new(),
        Some(entries) => entries
            .into_iter()
            .map(|entry| {
                let speaker = if entry.role == "user" {
                    "You"
                } else {
                    "Assistant"
                };
                format!("Voice · {speaker}: {}", entry.text)
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn managed_timestamp(created_at: Option<f64>, fallback_offset: usize) -> u64 {
    let Some(mut timestamp) = created_at.filter(|timestamp| timestamp.is_finite()) else {
        return unix_ms().saturating_add(u64::try_from(fallback_offset).unwrap_or(u64::MAX));
    };
    if timestamp < 10_000_000_000.0 {
        timestamp *= 1_000.0;
    }
    timestamp.max(0.0) as u64
}

pub(super) fn unix_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{HistoryPrefetch, HistoryWindow};
    use nanocodex_managed::{EventHistoryPage, ManagedEvent, ManagedEventData};
    use serde_json::json;

    #[test]
    fn vault_receipts_are_readable_in_live_and_replayed_prompt_history() {
        let receipt = json!({"type":"vault_intake_receipt", "operation":"authorize_origin",
            "status":"saved", "id":"abcdefghijklmnopqrstuv", "kind":"login",
            "name":"Example", "browser_origin":"https://example.com", "password":"hidden-secret"});
        let expected = "Website approved for Example\nhttps://example.com\nVault ID: abcdefghijklmnopqrstuv\nSaved to Vault. Password stayed in Vault.";
        for input in [
            json!(receipt.to_string()),
            json!([{"type":"text", "text":receipt.to_string()}]),
        ] {
            let event: ManagedEvent =
                serde_json::from_value(json!({"cursor":"1", "turn_id":"turn",
                "type":"turn_accepted", "id":"turn", "input":input, "replayed":false}))
                .unwrap();
            let mut sequence = 1;
            let (live_record, live_prompt) = super::live_managed_projection(
                event.clone(),
                "agent",
                std::path::Path::new("/workspace"),
                &mut sequence,
            )
            .unwrap()
            .unwrap();
            let (records, _, prompts) =
                super::history_projection(vec![event], "agent", std::path::Path::new("/workspace"))
                    .unwrap();
            assert_eq!(live_prompt.unwrap().text, expected);
            assert_eq!(prompts[0].text, expected);
            for record in [&live_record, &records[0]] {
                let payload: serde_json::Value = record.decode_payload().unwrap();
                assert_eq!(payload["text"], expected);
                assert!(!payload["text"].as_str().unwrap().contains("hidden-secret"));
                assert!(
                    !payload["text"]
                        .as_str()
                        .unwrap()
                        .contains("vault_intake_receipt")
                );
                assert!(!payload["text"].as_str().unwrap().contains("\\n"));
            }
        }
    }

    #[test]
    fn invalid_vault_receipt_history_is_safe_and_ordinary_json_is_preserved() {
        let invalid =
            json!({"type":"vault_intake_receipt", "status":"saved", "password":"hidden-secret"})
                .to_string();
        assert_eq!(
            super::prompt_input_text(&nanocodex_managed::PromptInput::Text(invalid)),
            "Vault receipt could not be verified."
        );
        let ordinary = json!({"type":"example", "text":"ordinary JSON"}).to_string();
        assert_eq!(
            super::prompt_input_text(&nanocodex_managed::PromptInput::Text(ordinary.clone())),
            ordinary
        );
    }

    #[test]
    fn voice_history_projects_shared_transcript_without_internal_instructions() {
        let text = nanocodex_voice_protocol::realtime_tail_delegation(&[
            nanocodex_voice_protocol::TranscriptEntry::new("user", "check the desktop"),
            nanocodex_voice_protocol::TranscriptEntry::new("assistant", "Linux"),
        ])
        .unwrap();
        let display = super::prompt_input_text(&nanocodex_managed::PromptInput::Text(text));
        assert_eq!(
            display,
            "Voice · You: check the desktop\nVoice · Assistant: Linux"
        );
        assert_eq!(
            super::prompt_input_text(&nanocodex_managed::PromptInput::Text(
                "ordinary <code>".into()
            )),
            "ordinary <code>"
        );
        assert_eq!(
            super::prompt_input_text(&nanocodex_managed::PromptInput::Text(
                "<realtime_conversation>internal</realtime_conversation>".into()
            )),
            ""
        );
    }

    #[test]
    fn durable_stop_projection_preserves_other_work_and_keeps_retries_active() {
        use crate::tui::transcript::{EntryKind, ToolState, TranscriptModel, TranscriptRecord};
        for kind in ["turn_failed", "turn_cancelled", "turn_retryable"] {
            for retained in [false, true] {
                let nested = |cursor: u64,
                              turn: &str,
                              child: Option<u64>,
                              kind: &str,
                              payload: serde_json::Value| {
                    serde_json::from_value::<ManagedEvent>(json!({
                        "cursor": cursor.to_string(), "turn_id": turn, "type": "event", "agent_id": child,
                        "event": {"protocol_version": 1, "request_id": "agent", "seq": cursor, "type": kind, "payload": payload}
                    })).unwrap()
                };
                let mut history = Vec::new();
                for (index, turn, child) in
                    [(0, "root", None), (1, "root", Some(7)), (2, "other", None)]
                {
                    history.push(nested(index * 2 + 1, turn, child, "run.started", json!({})));
                    history.push(nested(index * 2 + 2, turn, child, "tool.call", json!({"call_id": format!("call-{index}"), "tool": "read_file", "arguments": {"path": "file"}})));
                }
                history.push(nested(
                    7,
                    "root",
                    None,
                    "assistant.delta",
                    json!({"model_call_index": 1, "phase": "final_answer", "text": "partial text"}),
                ));
                history.push(serde_json::from_value(json!({"cursor": "8", "turn_id": "root", "type": kind, "id": "root", "error": "retained failure reason"})).unwrap());
                let records = if retained {
                    super::history_projection(history, "agent", std::path::Path::new("/workspace"))
                        .unwrap()
                        .0
                } else {
                    let mut sequence = 1;
                    history
                        .into_iter()
                        .filter_map(|event| {
                            super::live_managed_projection(
                                event,
                                "agent",
                                std::path::Path::new("/workspace"),
                                &mut sequence,
                            )
                            .unwrap()
                            .map(|(record, _)| record)
                        })
                        .collect()
                };
                let mut model = TranscriptModel::default();
                // Exercise the on-disk record representation as well as both projection paths.
                for record in &records {
                    let roundtrip: TranscriptRecord =
                        serde_json::from_str(&serde_json::to_string(record).unwrap()).unwrap();
                    model.apply(&roundtrip);
                }
                model.apply(records.last().unwrap());
                let states = model
                    .entries()
                    .iter()
                    .filter_map(|entry| match &entry.kind {
                        EntryKind::Tool(tool) => Some(tool.state),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    states,
                    [
                        if kind == "turn_retryable" {
                            ToolState::Running
                        } else {
                            ToolState::Failed
                        },
                        ToolState::Running,
                        ToolState::Running
                    ],
                    "{kind}, retained={retained}"
                );
                let errors = model.entries().iter().filter(|entry| matches!(&entry.kind, EntryKind::Error { message } if message == "retained failure reason")).count();
                assert_eq!(errors, usize::from(kind == "turn_failed"));
                assert!(model.entries().iter().any(|entry| matches!(&entry.kind, EntryKind::Assistant { text, .. } if text == "partial text")));
                let mut sequence = 9;
                for (cursor, turn, child) in [(9, "root", Some(7)), (10, "other", None)] {
                    let (record, _) = super::live_managed_projection(
                        nested(cursor, turn, child, "run.completed", json!({})),
                        "agent",
                        std::path::Path::new("/workspace"),
                        &mut sequence,
                    )
                    .unwrap()
                    .unwrap();
                    model.apply(&record);
                }
                assert_eq!(
                    model.is_active(),
                    kind == "turn_retryable",
                    "{kind}, retained={retained}"
                );
            }
        }
    }

    #[test]
    fn retained_completion_preserves_the_answer_without_a_nested_final_message() {
        use crate::tui::transcript::{EntryKind, TranscriptModel};
        let history = vec![
            serde_json::from_value::<ManagedEvent>(json!({"cursor": "1", "turn_id": "turn", "type": "turn_accepted", "id": "turn", "input": "question", "replayed": false})).unwrap(),
            serde_json::from_value::<ManagedEvent>(json!({"cursor": "2", "turn_id": "turn", "type": "turn_completed", "id": "turn", "final_message": "retained final answer", "usage": null, "citations": []})).unwrap(),
        ];
        let (records, _, _) =
            super::history_projection(history, "agent", std::path::Path::new("/workspace"))
                .unwrap();
        let mut model = TranscriptModel::default();
        for record in records {
            model.apply(&record);
        }
        assert!(model.entries().iter().any(|entry| matches!(&entry.kind, EntryKind::Assistant { text, complete: true, agent_id: None } if text == "retained final answer")));
    }

    #[test]
    fn retained_answers_keep_their_turn_scope_when_call_numbers_restart() {
        use crate::tui::transcript::{EntryKind, TranscriptModel};
        let mut history = Vec::new();
        for (turn, text) in [("first", "first answer"), ("second", "second answer")] {
            let cursor = history.len() + 1;
            history.push(serde_json::from_value::<ManagedEvent>(json!({"cursor": cursor.to_string(), "turn_id": turn, "type": "turn_accepted", "id": turn, "input": turn, "replayed": false})).unwrap());
            history.push(serde_json::from_value::<ManagedEvent>(json!({"cursor": (cursor + 1).to_string(), "turn_id": turn, "type": "event", "event": {"protocol_version": 1, "request_id": "agent", "seq": cursor, "type": "assistant.message", "payload": {"model_call_index": 1, "item_id": null, "phase": "final_answer", "text": text}}})).unwrap());
        }
        let (records, _, _) =
            super::history_projection(history, "agent", std::path::Path::new("/workspace"))
                .unwrap();
        let mut model = TranscriptModel::default();
        for record in records {
            model.apply(&record);
        }
        let answers = model
            .entries()
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Assistant { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(answers, ["first answer", "second answer"]);
    }

    #[test]
    fn unrecognized_nested_update_keeps_surrounding_history_and_stable_sequences() {
        let history: Vec<ManagedEvent> = [
            json!({"cursor": "1", "turn_id": "turn-1", "type": "turn_accepted", "id": "turn-1", "input": "original", "replayed": false}),
            json!({"cursor": "2", "turn_id": "turn-1", "type": "event", "event": {"protocol_version": 1, "request_id": "agent-1", "seq": 1, "type": "unrecognized.session.update", "payload": {}}}),
            json!({"cursor": "3", "turn_id": "turn-1", "type": "event", "event": {"protocol_version": 1, "request_id": "agent-1", "seq": 2, "type": "assistant.message", "payload": {"model_call_index": 0, "item_id": "final", "phase": "final_answer", "text": "retained result"}}}),
        ].into_iter().map(|event| serde_json::from_value(event).unwrap()).collect();
        let mut sequences = std::collections::HashMap::new();
        let mut next_sequence = 1;
        for _ in 0..2 {
            let (records, prompts) = super::history_projection_with_sequences(
                &history,
                "agent-1",
                std::path::Path::new("/workspace"),
                &mut sequences,
                &mut next_sequence,
            )
            .unwrap();
            assert_eq!(
                records
                    .iter()
                    .map(|record| record.kind())
                    .collect::<Vec<_>>(),
                ["user.submitted", "display.error", "assistant.message"]
            );
            assert_eq!(prompts.len(), 1);
            assert_eq!(prompts[0].text, "original");
            assert_eq!(records[1].sequence(), 2);
            assert_eq!(next_sequence, 4);
        }
        let (record, prompt) = super::live_managed_projection(
            history[1].clone(),
            "agent-1",
            std::path::Path::new("/workspace"),
            &mut next_sequence,
        )
        .unwrap()
        .unwrap();
        assert_eq!(record.kind(), "display.error");
        assert!(prompt.is_none());
    }

    #[test]
    fn prefetch_fetches_ahead_without_replaying_until_requested() {
        let mut prefetch = HistoryPrefetch::default();
        let history = HistoryWindow::retry_from("9".to_owned());

        assert_eq!(prefetch.claim(&history).as_deref(), Some("9"));
        assert!(prefetch.owns("9"));
        assert!(prefetch.claim(&history).is_none());
        assert!(!prefetch.fail("8"));
        prefetch
            .store("9", page("7", true))
            .expect("valid page should buffer");

        assert_eq!(prefetch.claim(&history).as_deref(), Some("7"));
        prefetch.request_replay();
        let (requested, buffered) = prefetch
            .take_requested(history.before.as_deref())
            .expect("the oldest matching buffered page should replay");
        assert_eq!(requested, "9");
        assert_eq!(buffered.data[0].cursor, "7");
    }

    #[test]
    fn prefetch_stops_at_exhaustion_and_reset_drops_stale_ownership() {
        let mut prefetch = HistoryPrefetch::default();
        let history = HistoryWindow::retry_from("9".to_owned());
        assert_eq!(prefetch.claim(&history).as_deref(), Some("9"));

        prefetch.reset();
        assert!(!prefetch.owns("9"));
        assert_eq!(prefetch.claim(&history).as_deref(), Some("9"));
        prefetch
            .store("9", page("7", false))
            .expect("terminal page should buffer");
        assert!(prefetch.claim(&history).is_none());

        prefetch.reset();
        assert_eq!(prefetch.claim(&history).as_deref(), Some("9"));
    }

    fn page(cursor: &str, has_more: bool) -> EventHistoryPage {
        EventHistoryPage {
            data: vec![ManagedEvent {
                cursor: cursor.to_owned(),
                created_at: None,
                turn_id: None,
                data: ManagedEventData::AgentCreated {
                    agent_id: "agent-1".to_owned(),
                    capabilities: json!({}),
                },
            }],
            has_more,
            latest_cursor: "9".to_owned(),
        }
    }
}
