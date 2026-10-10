// Without the Codex backend only the harness-neutral writer and loader are used.
#![cfg_attr(not(feature = "openai"), allow(dead_code))]

mod load;
mod record;
mod store;
mod wire;

#[cfg(all(test, feature = "openai"))]
mod tests;

pub use load::{DurableSession, RolloutSessionInfo};
pub use record::{RolloutSession, RolloutTurnRecord, RolloutWriter};
use store::RolloutCommit;
pub use store::RolloutInfo;
pub(crate) use store::{RolloutCreate, RolloutOrigin, RolloutRecorder, RolloutTurn};

use std::{
    fs::File,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use chrono::{Local, SecondsFormat, Utc};
#[cfg(feature = "openai")]
use nanocodex_oai_api::responses::ResponseHistory;
use nanocodex_oai_api::{
    ImageDetail, Model, Prompt, PromptInput, Thinking, UserInput, responses::ResponseItem,
};
use serde::Serialize;
use tokio::{
    io::{AsyncSeekExt, AsyncWriteExt},
    runtime::Handle,
    sync::{mpsc, oneshot},
};
use tracing::error;

#[cfg(feature = "openai")]
use crate::session::CommittedSession;
use crate::session::{ContextBaseline, SessionSnapshot};

/// Committed history handed to the writer: Codex's shared segments, or a
/// neutral item list recorded by another harness.
#[derive(Clone)]
pub(crate) enum RolloutHistory {
    #[cfg(feature = "openai")]
    Shared(ResponseHistory),
    Items(std::sync::Arc<[ResponseItem]>),
}

impl RolloutHistory {
    pub(crate) fn len(&self) -> usize {
        match self {
            #[cfg(feature = "openai")]
            Self::Shared(history) => history.len(),
            Self::Items(items) => items.len(),
        }
    }

    pub(crate) fn iter(&self) -> Box<dyn Iterator<Item = &ResponseItem> + Send + '_> {
        self.iter_from(0)
    }

    pub(crate) fn iter_from(
        &self,
        start: usize,
    ) -> Box<dyn Iterator<Item = &ResponseItem> + Send + '_> {
        match self {
            #[cfg(feature = "openai")]
            Self::Shared(history) => Box::new(history.iter_from(start)),
            Self::Items(items) => Box::new(items.iter().skip(start)),
        }
    }
}

const COMMAND_CAPACITY: usize = 8;

/// Configuration for writing a thread in Codex's resumable rollout layout.
#[derive(Debug)]
pub struct RolloutConfig {
    codex_home: PathBuf,
    resume_path: Option<PathBuf>,
    root_session_id: std::sync::Arc<std::sync::OnceLock<String>>,
}

impl Clone for RolloutConfig {
    fn clone(&self) -> Self {
        Self {
            codex_home: self.codex_home.clone(),
            resume_path: self.resume_path.clone(),
            root_session_id: std::sync::Arc::new((*self.root_session_id).clone()),
        }
    }
}

impl RolloutConfig {
    /// Writes rollouts beneath `<codex_home>/sessions/YYYY/MM/DD`.
    #[must_use]
    pub fn new(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
            resume_path: None,
            root_session_id: Default::default(),
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

    /// Configuration that reopens the rollout already recorded for
    /// `thread_id`, including one created before its first turn, so a
    /// resumed session appends to its own file instead of mirroring twice.
    ///
    /// # Errors
    ///
    /// Returns an error when the sessions directory cannot be read.
    #[doc(hidden)]
    pub fn recorded(&self, thread_id: &str) -> io::Result<Option<Self>> {
        // A configuration that already names the recorded file reopens it.
        if self.resume_path.is_some() {
            return Ok(Some(self.clone()));
        }
        self.reopening(thread_id)
    }

    /// A fresh configuration for a new conversation under the same Codex home.
    pub(crate) fn for_branch(&self) -> Self {
        Self::new(self.codex_home.clone())
    }

    /// Reopens the uncompressed rollout already recorded for `thread_id`
    /// beneath this Codex home, so a restored child keeps appending to it.
    pub(crate) fn reopening(&self, thread_id: &str) -> io::Result<Option<Self>> {
        if self.resume_path.is_some() {
            return Ok(None);
        }
        Ok(load::find_rollout_path(&self.codex_home, thread_id)?
            .filter(|path| path.extension().is_some_and(|extension| extension == "jsonl"))
            .map(|path| Self::new(self.codex_home.clone()).resumed(path)))
    }

    pub(crate) fn resumed(mut self, rollout_path: PathBuf) -> Self {
        self.resume_path = Some(rollout_path);
        self
    }
}
