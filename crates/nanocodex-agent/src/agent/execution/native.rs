use std::path::{Path, PathBuf};

use tracing::error;

use crate::{
    NanocodexError, Result,
    rollout::{
        RolloutConfig, RolloutCreate, RolloutInfo, RolloutOrigin, RolloutRecorder, RolloutTurn,
    },
    session::CommittedSession,
};

#[derive(Clone, Default)]
pub(super) struct Config {
    rollout: Option<RolloutConfig>,
}

impl Config {
    pub(super) fn set_rollout(&mut self, rollout: RolloutConfig) {
        self.rollout = Some(rollout);
    }

    pub(super) fn for_new_thread(&self) -> Self {
        // Every fork, side conversation and subagent is its own resumable
        // conversation and records its own rollout beside its parent.
        Self {
            rollout: self.rollout.as_ref().map(RolloutConfig::for_branch),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start(
        &self,
        session_id: &str,
        prompt_cache_key: &str,
        workspace: Option<&str>,
        instructions: &str,
        start: crate::session::SessionStart,
        lineage_origin: crate::Origin,
        parent_session_id: Option<&str>,
        root_session_id: &str,
        resume_history_len: Option<usize>,
    ) -> Result<Execution> {
        let Some(config) = &self.rollout else {
            return Ok(Execution::default());
        };
        // A restored child or a session resumed from durable state continues
        // the rollout it already recorded; the recorder appends only history
        // newer than the file, so the mirror never duplicates a turn.
        let reopened = if matches!(
            start,
            crate::session::SessionStart::Restore | crate::session::SessionStart::Resume
        ) {
            config
                .reopening(session_id)
                .map_err(|source| NanocodexError::InitializeRollout {
                    codex_home: config.codex_home().to_path_buf(),
                    source,
                })?
        } else {
            None
        };
        let resume_history_len = match &reopened {
            Some(_) => Some(resume_history_len.unwrap_or(0)),
            None => resume_history_len,
        };
        let config = reopened.as_ref().unwrap_or(config);
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| NanocodexError::TokioRuntimeUnavailable)?;
        let cwd =
            rollout_workspace(workspace).map_err(|source| NanocodexError::InitializeRollout {
                codex_home: config.codex_home().to_path_buf(),
                source,
            })?;
        let recorder = RolloutRecorder::create(
            &runtime,
            RolloutCreate {
                config,
                thread_id: session_id,
                prompt_cache_key,
                cwd: &cwd,
                instructions,
                origin: RolloutOrigin {
                    start: start.for_new_mirror(lineage_origin),
                    parent_thread_id: parent_session_id,
                    root_session_id: Some(root_session_id),
                },
                resume_history_len,
            },
        )
        .map_err(|source| NanocodexError::InitializeRollout {
            codex_home: config.codex_home().to_path_buf(),
            source,
        })?;
        Ok(Execution {
            recorder: Some(recorder),
        })
    }
}

#[derive(Clone, Default)]
pub(super) struct Execution {
    recorder: Option<RolloutRecorder>,
}

impl Execution {
    pub(super) const fn info(&self) -> Option<&RolloutInfo> {
        match &self.recorder {
            Some(recorder) => Some(recorder.info()),
            None => None,
        }
    }

    pub(super) fn start_turn(
        &self,
        prompt: &nanocodex_oai_api::Prompt,
        effort: nanocodex_oai_api::Thinking,
        turn_id: Option<&str>,
    ) -> Turn {
        Turn(self.recorder.as_ref().map(|_| {
            let mut turn = RolloutTurn::started(prompt, effort);
            if let Some(id) = turn_id {
                turn.set_id(id);
            }
            turn
        }))
    }

    pub(super) fn start_compaction(&self, effort: nanocodex_oai_api::Thinking) -> Turn {
        Turn(
            self.recorder
                .as_ref()
                .map(|_| RolloutTurn::compaction_started(effort)),
        )
    }

    pub(super) async fn accepted_input(
        &self,
        input: nanocodex_oai_api::events::AcceptedInput,
    ) -> Result<()> {
        if let Some(recorder) = &self.recorder {
            recorder.accepted_input(input).await.map_err(|source| {
                NanocodexError::PersistRollout {
                    path: recorder.info().path().to_path_buf(),
                    source,
                }
            })?;
        }
        Ok(())
    }

    pub(super) async fn persist(&self, checkpoint: &CommittedSession, turn: Turn) {
        let (Some(recorder), Some(turn)) = (&self.recorder, turn.0) else {
            return;
        };
        if let Err(source) = recorder.persist(checkpoint, turn).await {
            error!(target: "nanocodex", rollout_path = %recorder.info().path().display(), error = %source, "failed to persist Codex rollout");
        }
    }

    pub(super) async fn persist_compaction(&self, checkpoint: &CommittedSession, turn: Turn) {
        let (Some(recorder), Some(turn)) = (&self.recorder, turn.0) else {
            return;
        };
        if let Err(source) = recorder.persist_compaction(checkpoint, turn).await {
            error!(target: "nanocodex", rollout_path = %recorder.info().path().display(), error = %source, "failed to persist Codex compaction boundary");
        }
    }

    pub(super) async fn flush(&self) -> Result<()> {
        let Some(recorder) = &self.recorder else {
            return Ok(());
        };
        recorder
            .flush()
            .await
            .map_err(|source| NanocodexError::PersistRollout {
                path: recorder.info().path().to_path_buf(),
                source,
            })
    }

    pub(super) async fn shutdown(&self) -> Result<()> {
        let Some(recorder) = &self.recorder else {
            return Ok(());
        };
        recorder
            .shutdown()
            .await
            .map_err(|source| NanocodexError::PersistRollout {
                path: recorder.info().path().to_path_buf(),
                source,
            })
    }
}

pub(super) struct Turn(Option<RolloutTurn>);

impl Turn {
    pub(super) fn completed(self, final_message: String) -> Self {
        Self(self.0.map(|turn| turn.completed(final_message)))
    }

    pub(super) fn completed_without_message(self) -> Self {
        Self(self.0.map(RolloutTurn::completed_without_message))
    }

    pub(super) fn interrupted(self) -> Self {
        Self(self.0.map(RolloutTurn::interrupted))
    }

    pub(super) fn replaced(self) -> Self {
        Self(self.0.map(RolloutTurn::replaced))
    }

    pub(super) fn failed(self) -> Self {
        Self(self.0.map(RolloutTurn::failed))
    }
}

fn rollout_workspace(workspace: Option<&str>) -> std::io::Result<PathBuf> {
    let current = std::env::current_dir()?;
    let Some(workspace) = workspace else {
        return Ok(current);
    };
    let workspace = Path::new(workspace);
    if workspace.is_absolute() {
        Ok(workspace.to_path_buf())
    } else {
        Ok(current.join(workspace))
    }
}
