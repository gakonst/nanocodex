mod load;
mod store;
mod wire;

#[cfg(test)]
mod tests;

pub use load::{DurableSession, RolloutSessionInfo, RolloutTranscriptItem};
pub use store::RolloutInfo;
pub(crate) use store::{RolloutCreate, RolloutOrigin, RolloutRecorder, RolloutTurn};

use std::{
    fs::File,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use chrono::{Local, SecondsFormat, Utc};
use nanocodex_oai_api::{
    ImageDetail, Model, Prompt, PromptInput, Thinking, UserInput,
    responses::{ContentItem, MessageRole, ResponseHistory, ResponseItem},
};
use serde::Serialize;
use tokio::{
    io::{AsyncSeekExt, AsyncWriteExt},
    runtime::Handle,
    sync::{mpsc, oneshot},
};
use tracing::error;

use crate::{
    model::context::ContextBaseline,
    session::{CommittedSession, SessionSnapshot},
};

const COMMAND_CAPACITY: usize = 8;

#[derive(Clone, Debug)]
struct ReplayedContextWindow {
    fallback_id: String,
    first_id: String,
    previous_id: Option<String>,
    current_id: String,
    number: u64,
    compaction_count: u64,
    explicit_lineage: bool,
}

impl ReplayedContextWindow {
    fn new(thread_id: &str) -> Self {
        Self {
            fallback_id: thread_id.to_owned(),
            first_id: thread_id.to_owned(),
            previous_id: None,
            current_id: thread_id.to_owned(),
            number: 0,
            compaction_count: 0,
            explicit_lineage: false,
        }
    }

    fn observe_session_meta(
        &mut self,
        payload: &serde_json::Value,
        thread_id: &str,
    ) -> io::Result<()> {
        let context_window = match payload.get("context_window") {
            None | Some(serde_json::Value::Null) => return Ok(()),
            Some(context_window) => context_window,
        };
        let window_id = context_window
            .get("window_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Codex rollout session metadata has an invalid context window",
                )
            })?;
        self.fallback_id = thread_id.to_owned();
        self.first_id = window_id.to_owned();
        self.previous_id = None;
        self.current_id = window_id.to_owned();
        self.number = 0;
        self.explicit_lineage = true;
        Ok(())
    }

    fn observe_compaction(&mut self, payload: &serde_json::Value) -> io::Result<()> {
        self.compaction_count = self.compaction_count.saturating_add(1);
        let mut number = optional_u64(payload, "window_number")?;
        let first = optional_nonempty_string(payload, "first_window_id")?;
        let previous = optional_nonempty_string(payload, "previous_window_id")?;
        let current = match payload.get("window_id") {
            Some(serde_json::Value::Number(legacy)) => {
                let legacy = legacy.as_u64().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Codex compaction has an invalid window_id",
                    )
                })?;
                number.get_or_insert(legacy);
                None
            }
            _ => optional_nonempty_string(payload, "window_id")?,
        };

        if let (Some(number), Some(first), Some(previous), Some(current)) =
            (number, first, previous, current)
        {
            if self.explicit_lineage {
                let (number, current) = validate_context_window_transition(
                    payload,
                    &self.first_id,
                    &self.current_id,
                    self.number,
                )?;
                self.previous_id = Some(self.current_id.clone());
                self.number = number;
                self.current_id = current;
            } else {
                if number == 0 || current == previous {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Codex compaction has an invalid context window transition",
                    ));
                }
                self.first_id = first.to_owned();
                self.previous_id = Some(previous.to_owned());
                self.current_id = current.to_owned();
                self.number = number;
            }
            self.explicit_lineage = true;
            return Ok(());
        }

        // Codex keeps these fields optional so rollouts written before context
        // lineage was introduced remain resumable. Its reconstruction uses the
        // compaction count and the session's fresh fallback identity whenever a
        // checkpoint does not carry a complete lineage.
        let current = current.unwrap_or(&self.fallback_id).to_owned();
        self.first_id = first.unwrap_or(&current).to_owned();
        self.previous_id = previous.map(str::to_owned);
        self.current_id = current;
        self.number = number.unwrap_or(self.compaction_count);
        self.explicit_lineage = false;
        Ok(())
    }
}

fn replay_compacted_history(
    payload: &serde_json::Value,
    history: &[ResponseItem],
) -> io::Result<Vec<ResponseItem>> {
    match payload.get("replacement_history") {
        None | Some(serde_json::Value::Null) => {
            let summary = payload
                .get("message")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "legacy Codex compaction is missing its summary",
                    )
                })?;
            let summary = if summary.is_empty() {
                "(no summary available)"
            } else {
                summary
            };
            let mut rebuilt = history
                .iter()
                .filter(|item| item.is_user_message())
                .cloned()
                .collect::<Vec<_>>();
            rebuilt.push(ResponseItem::message(
                MessageRole::User,
                [ContentItem::input_text(summary)],
            ));
            Ok(rebuilt)
        }
        Some(replacement) => serde_json::from_value(replacement.clone()).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to decode Codex replacement history: {error}"),
            )
        }),
    }
}

fn optional_u64(payload: &serde_json::Value, field: &str) -> io::Result<Option<u64>> {
    match payload.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Codex compaction has an invalid {field}"),
            )
        }),
    }
}

fn optional_nonempty_string<'a>(
    payload: &'a serde_json::Value,
    field: &str,
) -> io::Result<Option<&'a str>> {
    match payload.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .map(Some)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Codex compaction has an invalid {field}"),
                )
            }),
    }
}

fn validate_context_window_transition(
    payload: &serde_json::Value,
    first_window_id: &str,
    previous_window_id: &str,
    previous_window_number: u64,
) -> io::Result<(u64, String)> {
    let next_window_number = payload["window_number"].as_u64().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex compaction is missing its window number",
        )
    })?;
    if next_window_number != previous_window_number.saturating_add(1) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex compaction context window number is not contiguous",
        ));
    }
    if payload["first_window_id"].as_str() != Some(first_window_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex compaction changed its first context window ID",
        ));
    }
    if payload["previous_window_id"].as_str() != Some(previous_window_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex compaction previous context window does not match lineage",
        ));
    }
    let next_window_id = payload["window_id"]
        .as_str()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Codex compaction is missing its window ID",
            )
        })?
        .to_owned();
    if next_window_id == previous_window_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex compaction did not advance its context window ID",
        ));
    }
    Ok((next_window_number, next_window_id))
}

/// Configuration for writing a thread in Codex's resumable rollout layout.
#[derive(Clone, Debug)]
pub struct RolloutConfig {
    codex_home: PathBuf,
    resume_path: Option<PathBuf>,
}

impl RolloutConfig {
    /// Writes rollouts beneath `<codex_home>/sessions/YYYY/MM/DD`.
    #[must_use]
    pub fn new(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
            resume_path: None,
        }
    }

    /// Returns the Codex state directory used for this rollout policy.
    #[must_use]
    pub fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    /// Loads a Codex or Nanocodex session recorded beneath this Codex home.
    ///
    /// # Errors
    ///
    /// Returns an error when the thread ID is not a UUID, the session does not
    /// exist, or its rollout is malformed or incompatible.
    pub fn load_session(&self, thread_id: &str) -> io::Result<DurableSession> {
        DurableSession::load(&self.codex_home, thread_id)
    }

    /// Lists resumable Codex and Nanocodex sessions beneath this Codex home.
    ///
    /// Active and archived uncompressed JSONL rollouts are returned newest
    /// first. Files without recognizable session metadata are ignored so a
    /// stale or partially written unrelated file cannot prevent discovery.
    ///
    /// # Errors
    ///
    /// Returns an error when a session directory exists but cannot be read.
    pub fn list_sessions(&self) -> io::Result<Vec<RolloutSessionInfo>> {
        load::list_sessions(&self.codex_home)
    }

    pub(crate) fn resumed(mut self, rollout_path: PathBuf) -> Self {
        self.resume_path = Some(rollout_path);
        self
    }

    pub(crate) fn for_new_thread(&self) -> Self {
        Self::new(self.codex_home.clone())
    }
}
