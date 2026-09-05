use crate::{Result, session::CommittedSession};

#[derive(Clone, Default)]
pub(super) struct Config;

impl Config {
    pub(super) const fn for_new_thread(&self) -> Self {
        Self
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) const fn start(
        &self,
        _session_id: &str,
        _prompt_cache_key: &str,
        _workspace: Option<&str>,
        _instructions: &str,
        _origin_kind: &'static str,
        _parent_session_id: Option<&str>,
        _resume_history_len: Option<usize>,
    ) -> Result<Execution> {
        Ok(Execution)
    }
}

#[derive(Clone)]
pub(super) struct Execution;

impl Execution {
    pub(super) const fn start_turn(
        &self,
        _prompt: &nanocodex_oai_api::Prompt,
        _effort: nanocodex_oai_api::Thinking,
    ) -> Turn {
        Turn
    }

    pub(super) const fn start_compaction(&self, _effort: nanocodex_oai_api::Thinking) -> Turn {
        Turn
    }

    pub(super) async fn persist(&self, _checkpoint: &CommittedSession, _turn: Turn) {}

    pub(super) async fn persist_compaction(&self, _checkpoint: &CommittedSession, _turn: Turn) {}

    pub(super) async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

pub(super) struct Turn;

impl Turn {
    pub(super) fn completed(self, _final_message: String) -> Self {
        self
    }

    pub(super) const fn completed_without_message(self) -> Self {
        self
    }

    pub(super) const fn interrupted(self) -> Self {
        self
    }

    pub(super) const fn replaced(self) -> Self {
        self
    }

    pub(super) const fn failed(self) -> Self {
        self
    }
}
