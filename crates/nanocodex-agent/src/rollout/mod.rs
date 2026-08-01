mod load;
mod store;
mod wire;

#[cfg(test)]
mod tests;

pub use load::{DurableSession, DurableSessionSummary, RolloutTranscriptItem};
pub use store::RolloutInfo;
pub(crate) use store::{RolloutOrigin, RolloutRecorder, RolloutTurn};

use std::{
    fs::File,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use chrono::{Local, SecondsFormat, Utc};
use nanocodex_oai_api::{
    ImageDetail, Model, Prompt, PromptInput, Thinking, UserInput,
    responses::{ResponseHistory, ResponseItem},
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

    /// Lists compatible Codex and Nanocodex sessions beneath this Codex home,
    /// newest first.
    ///
    /// The returned summaries read only enough rollout data to support session
    /// discovery. Loading the selected durable boundary remains explicit through
    /// [`Self::load_session`].
    ///
    /// # Errors
    ///
    /// Returns an error when the session directories or compatible rollout
    /// files cannot be read. Malformed or unsupported rollouts are omitted.
    pub fn list_sessions(&self) -> io::Result<Vec<DurableSessionSummary>> {
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
