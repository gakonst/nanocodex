//! Harness-neutral writer for Codex-compatible rollouts.
//!
//! Codex records its own sessions through this same store. Other harnesses
//! describe their session and express each committed turn as Responses-shaped
//! items (messages, function calls and outputs, optional reasoning summaries),
//! producing files that Codex-compatible tooling lists, reads, and resumes.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use super::*;
use crate::{
    HarnessModel,
    session::{Lineage, Origin, SessionStart},
};

/// Session metadata written as a rollout's first record.
#[derive(Clone, Debug)]
pub struct RolloutSession {
    /// Stable session identity; a UUID used as the Codex thread ID.
    pub session_id: String,
    /// Provenance recorded as the root, parent, and conversation role.
    pub lineage: Lineage,
    /// Workspace the session runs in.
    pub cwd: PathBuf,
    /// Base instructions in effect for the session.
    pub instructions: String,
    /// Prompt-cache identity, when the harness has one; defaults to the session ID.
    pub prompt_cache_key: Option<String>,
}

/// One turn's metadata, recorded beside the history it committed.
#[derive(Clone)]
pub struct RolloutTurnRecord(pub(super) RolloutTurn);

impl RolloutTurnRecord {
    /// Starts a turn for the given user prompt; its duration is measured from now.
    #[must_use]
    pub fn started(turn_id: impl AsRef<str>, prompt: &Prompt, effort: Thinking) -> Self {
        let mut turn = RolloutTurn::started(prompt, effort);
        turn.set_id(turn_id.as_ref());
        Self(turn)
    }

    /// Marks the turn completed with its final assistant message.
    #[must_use]
    pub fn completed(self, final_message: impl Into<String>) -> Self {
        Self(self.0.completed(final_message.into()))
    }

    /// Marks the turn completed without a final assistant message.
    #[must_use]
    pub fn completed_without_message(self) -> Self {
        Self(self.0.completed_without_message())
    }

    /// Marks the turn interrupted before completion.
    #[must_use]
    pub fn interrupted(self) -> Self {
        Self(self.0.interrupted())
    }

    /// Marks the turn failed.
    #[must_use]
    pub fn failed(self) -> Self {
        Self(self.0.failed())
    }
}

/// Appends committed turns of one session to its Codex-compatible rollout.
///
/// Cloning shares the same ordered writer. Requires an active Tokio runtime.
#[derive(Clone)]
pub struct RolloutWriter {
    recorder: RolloutRecorder,
    revision: Arc<AtomicU64>,
}

impl RolloutWriter {
    /// Creates a new rollout beneath the configured Codex home.
    ///
    /// # Errors
    ///
    /// Returns an error without an active Tokio runtime, for a configuration
    /// returned by a resumed session (use [`Self::resume`]), or when the file
    /// cannot be created.
    pub fn create(config: &RolloutConfig, session: &RolloutSession) -> io::Result<Self> {
        if config.resume_path.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a resumed rollout configuration must be reopened with RolloutWriter::resume",
            ));
        }
        Self::open(config, session, None)
    }

    /// Reopens the rollout of a loaded session to append further turns.
    ///
    /// `config` is the configuration returned with the loaded session, and
    /// `history_len` the number of committed history items it restored.
    ///
    /// # Errors
    ///
    /// Returns an error without an active Tokio runtime or when the rollout
    /// cannot be reopened.
    pub fn resume(
        config: &RolloutConfig,
        session: &RolloutSession,
        history_len: usize,
    ) -> io::Result<Self> {
        Self::open(config, session, Some(history_len))
    }

    fn open(
        config: &RolloutConfig,
        session: &RolloutSession,
        resume_history_len: Option<usize>,
    ) -> io::Result<Self> {
        let runtime = Handle::try_current().map_err(io::Error::other)?;
        let start = if resume_history_len.is_some() {
            SessionStart::Resume
        } else {
            SessionStart::New(session.lineage.origin)
        };
        let parent_thread_id = match session.lineage.origin {
            Origin::Root => None,
            _ => session.lineage.parent_session_id.as_deref(),
        };
        let recorder = RolloutRecorder::create(
            &runtime,
            RolloutCreate {
                config,
                thread_id: &session.session_id,
                prompt_cache_key: session
                    .prompt_cache_key
                    .as_deref()
                    .unwrap_or(&session.session_id),
                cwd: &session.cwd,
                instructions: &session.instructions,
                origin: RolloutOrigin {
                    start,
                    parent_thread_id,
                    root_session_id: Some(&session.lineage.root_session_id),
                },
                resume_history_len,
            },
        )?;
        Ok(Self {
            recorder,
            revision: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Identity and location of the rollout file.
    #[must_use]
    pub const fn info(&self) -> &RolloutInfo {
        self.recorder.info()
    }

    /// Records one committed turn. `history` is the complete model-visible
    /// history after the turn; only items beyond the previously recorded
    /// history are appended.
    ///
    /// # Errors
    ///
    /// Returns an error when the history shrank without a compaction or the
    /// rollout cannot be written; the next commit or flush retries.
    pub async fn commit(
        &self,
        turn: RolloutTurnRecord,
        model: HarnessModel,
        history: Vec<ResponseItem>,
    ) -> io::Result<()> {
        let revision = self.revision.load(Ordering::Acquire);
        self.recorder
            .persist_neutral(RolloutCommit::neutral(history, revision, turn.0, model))
            .await
    }

    /// Records a compaction that replaced the complete history.
    ///
    /// # Errors
    ///
    /// Returns an error when the rollout cannot be written.
    pub async fn commit_compaction(
        &self,
        turn: RolloutTurnRecord,
        model: HarnessModel,
        history: Vec<ResponseItem>,
    ) -> io::Result<()> {
        let revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        self.recorder
            .persist_neutral(RolloutCommit::neutral(history, revision, turn.0, model))
            .await
    }

    /// Retries pending writes and waits for a durable file flush.
    ///
    /// # Errors
    ///
    /// Returns an error when the rollout cannot be written.
    pub async fn flush(&self) -> io::Result<()> {
        self.recorder.flush().await
    }

    /// Flushes and closes the writer; later commits fail.
    ///
    /// # Errors
    ///
    /// Returns an error when the final flush fails.
    pub async fn shutdown(&self) -> io::Result<()> {
        self.recorder.shutdown().await
    }
}
