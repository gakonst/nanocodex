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
        Self {
            rollout: self.rollout.as_ref().map(RolloutConfig::for_new_thread),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start(
        &self,
        session_id: &str,
        prompt_cache_key: &str,
        workspace: Option<&str>,
        instructions: &str,
        origin_kind: &'static str,
        parent_session_id: Option<&str>,
        resume_history_len: Option<usize>,
    ) -> Result<Execution> {
        let Some(config) = &self.rollout else {
            return Ok(Execution::default());
        };
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
                    kind: origin_kind,
                    parent_thread_id: parent_session_id,
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
    ) -> Turn {
        Turn(
            self.recorder
                .as_ref()
                .map(|_| RolloutTurn::started(prompt, effort)),
        )
    }

    pub(super) fn start_compaction(&self, effort: nanocodex_oai_api::Thinking) -> Turn {
        Turn(
            self.recorder
                .as_ref()
                .map(|_| RolloutTurn::compaction_started(effort)),
        )
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
