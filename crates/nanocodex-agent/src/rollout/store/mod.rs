pub(in crate::rollout) mod writer;

use super::wire::*;
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use writer::*;

/// Stable identity and file location of a recorded Nanocodex thread.
#[derive(Clone, Debug)]
pub struct RolloutInfo {
    thread_id: String,
    path: PathBuf,
    committed_bytes: Arc<AtomicU64>,
}

impl PartialEq for RolloutInfo {
    fn eq(&self, other: &Self) -> bool {
        self.thread_id == other.thread_id && self.path == other.path
    }
}
impl Eq for RolloutInfo {}

impl RolloutInfo {
    /// Exclusive byte boundary of successfully flushed, complete rollout records.
    #[must_use]
    pub fn committed_bytes(&self) -> u64 {
        self.committed_bytes.load(Ordering::Acquire)
    }

    /// UUID accepted by `codex resume` and `codex exec resume`.
    #[must_use]
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Codex-compatible JSONL rollout path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RolloutRecorder {
    info: RolloutInfo,
    commands: mpsc::Sender<RolloutCommand>,
}

#[derive(Clone, Copy)]
pub(crate) struct RolloutOrigin<'a> {
    pub(crate) kind: &'a str,
    pub(crate) parent_thread_id: Option<&'a str>,
}

pub(crate) struct RolloutCreate<'a> {
    pub(crate) config: &'a RolloutConfig,
    pub(crate) thread_id: &'a str,
    pub(crate) prompt_cache_key: &'a str,
    pub(crate) cwd: &'a Path,
    pub(crate) instructions: &'a str,
    pub(crate) origin: RolloutOrigin<'a>,
    pub(crate) resume_history_len: Option<usize>,
}

enum RolloutCommand {
    Input {
        input: nanocodex_oai_api::events::AcceptedInput,
        result: oneshot::Sender<io::Result<()>>,
    },
    Commit {
        commit: Box<RolloutCommit>,
        result: oneshot::Sender<io::Result<()>>,
    },
    Flush {
        result: oneshot::Sender<io::Result<()>>,
    },
    Shutdown {
        result: oneshot::Sender<io::Result<()>>,
    },
}

pub(super) struct RolloutCommit {
    history: ResponseHistory,
    revision: u64,
    turn: RolloutTurn,
    model: Model,
    context_baseline: ContextBaseline,
    client_authored: std::collections::BTreeSet<String>,
}

impl RolloutCommit {
    fn from_session(session: &CommittedSession, turn: RolloutTurn) -> Self {
        Self {
            history: session.rollout_history(),
            revision: session.history_revision(),
            turn,
            model: session.selected_model(),
            context_baseline: session.context_baseline().clone(),
            client_authored: session.model().client_authored().clone(),
        }
    }

    fn compaction(session: &CommittedSession, turn: RolloutTurn) -> Self {
        Self {
            history: session.rollout_history(),
            revision: session.history_revision(),
            turn,
            model: session.selected_model(),
            context_baseline: session.context_baseline().clone(),
            client_authored: session.model().client_authored().clone(),
        }
    }

    #[cfg(test)]
    pub(super) const fn from_history(
        history: ResponseHistory,
        revision: u64,
        turn: RolloutTurn,
    ) -> Self {
        Self {
            history,
            revision,
            turn,
            model: Model::Sol,
            context_baseline: ContextBaseline::Missing,
            client_authored: std::collections::BTreeSet::new(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct RolloutTurn {
    turn_id: String,
    user_message: Option<UserMessage>,
    final_message: Option<String>,
    started_at: i64,
    completed_at: Option<i64>,
    duration_ms: Option<i64>,
    status: RolloutTurnStatus,
    effort: Thinking,
    timer: Option<Instant>,
}

#[derive(Clone, Copy)]
enum RolloutTurnStatus {
    InProgress,
    Completed,
    Interrupted,
    Replaced,
    Failed,
}

impl RolloutTurn {
    pub(crate) fn set_id(&mut self, id: &str) {
        self.turn_id = id.to_owned();
    }

    pub(crate) fn started(prompt: &Prompt, effort: Thinking) -> Self {
        Self {
            turn_id: uuid::Uuid::now_v7().to_string(),
            user_message: Some(UserMessage::from_prompt(prompt)),
            final_message: None,
            started_at: Utc::now().timestamp(),
            completed_at: None,
            duration_ms: None,
            status: RolloutTurnStatus::InProgress,
            effort,
            timer: Some(Instant::now()),
        }
    }

    pub(crate) fn compaction_started(effort: Thinking) -> Self {
        Self {
            turn_id: uuid::Uuid::now_v7().to_string(),
            user_message: None,
            final_message: None,
            started_at: Utc::now().timestamp(),
            completed_at: None,
            duration_ms: None,
            status: RolloutTurnStatus::InProgress,
            effort,
            timer: Some(Instant::now()),
        }
    }

    pub(crate) fn completed(mut self, final_message: String) -> Self {
        self.finish(RolloutTurnStatus::Completed);
        self.final_message = Some(final_message);
        self
    }

    pub(crate) fn completed_without_message(mut self) -> Self {
        self.finish(RolloutTurnStatus::Completed);
        self
    }

    pub(crate) fn interrupted(mut self) -> Self {
        self.finish(RolloutTurnStatus::Interrupted);
        self
    }

    pub(crate) fn replaced(mut self) -> Self {
        self.finish(RolloutTurnStatus::Replaced);
        self
    }

    pub(crate) fn failed(mut self) -> Self {
        self.finish(RolloutTurnStatus::Failed);
        self
    }

    fn finish(&mut self, status: RolloutTurnStatus) {
        self.status = status;
        self.completed_at = Some(Utc::now().timestamp());
        self.duration_ms = self
            .timer
            .take()
            .map(|started| i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX));
    }
}

impl RolloutRecorder {
    pub(crate) fn create(runtime: &Handle, request: RolloutCreate<'_>) -> io::Result<Self> {
        let RolloutCreate {
            config,
            thread_id,
            prompt_cache_key,
            cwd,
            instructions,
            origin,
            resume_history_len,
        } = request;
        if let Some(path) = &config.resume_path {
            if let Ok(file) = File::open(path)
                && let Some(Ok(line)) = BufReader::new(file).lines().next()
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
                && let Some(root) = value["payload"]["root_session_id"].as_str()
            {
                let _ = config.root_session_id.set(root.to_owned());
            }
            // A legacy recording starts the discovered lineage at its resumed owner.
            let _ = config.root_session_id.set(thread_id.to_owned());
            let history_len = resume_history_len.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "resumed rollout requires restored history",
                )
            })?;
            return Self::resume(runtime, thread_id, path, history_len);
        }
        let local = Local::now();
        let directory = config
            .codex_home
            .join("sessions")
            .join(local.format("%Y").to_string())
            .join(local.format("%m").to_string())
            .join(local.format("%d").to_string());
        std::fs::create_dir_all(&directory)?;
        let filename_timestamp = local.format("%Y-%m-%dT%H-%M-%S");
        let path = directory.join(format!("rollout-{filename_timestamp}-{thread_id}.jsonl"));
        let initial_window_id = uuid::Uuid::now_v7().to_string();
        let timestamp = timestamp();
        let parent_thread_id = origin.parent_thread_id.map(ToOwned::to_owned);
        let meta = SessionMeta {
            root_session_id: config
                .root_session_id
                .get_or_init(|| thread_id.to_owned())
                .clone(),
            origin_kind: if origin.kind == "side_conversation" {
                "fork"
            } else {
                origin.kind
            }
            .to_owned(),
            conversation_role: match origin.kind {
                "spawn" => "subagent",
                "fork" => "branch",
                "side_conversation" => "side_conversation",
                _ => "root",
            },
            session_id: thread_id.to_owned(),
            id: thread_id.to_owned(),
            prompt_cache_key: prompt_cache_key.to_owned(),
            forked_from_id: (matches!(origin.kind, "fork" | "side_conversation"))
                .then(|| parent_thread_id.clone())
                .flatten(),
            parent_thread_id,
            timestamp: timestamp.clone(),
            cwd: cwd.to_path_buf(),
            originator: "nanocodex".to_owned(),
            cli_version: env!("CARGO_PKG_VERSION").to_owned(),
            source: "cli",
            thread_source: "user",
            model_provider: "openai",
            base_instructions: BaseInstructions {
                text: instructions.to_owned(),
            },
            history_mode: "legacy",
            context_window: SessionContextWindow {
                window_id: initial_window_id.clone(),
            },
        };
        let mut file = File::options().write(true).create_new(true).open(&path)?;
        write_line(
            &mut file,
            &RolloutLine {
                timestamp,
                item: RolloutItem::SessionMeta(&meta),
            },
        )?;
        file.flush()?;
        file.sync_all()?;

        let writer = RolloutWriter::new(
            tokio::fs::File::from_std(file),
            initial_window_id,
            cwd.to_path_buf(),
        );
        Ok(Self::spawn(runtime, thread_id, path, writer))
    }

    fn resume(
        runtime: &Handle,
        thread_id: &str,
        path: &Path,
        history_len: usize,
    ) -> io::Result<Self> {
        let state = read_resume_writer_state(path, thread_id)?;
        if state.written_len > history_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Codex rollout contains history newer than the durable Nanocodex boundary",
            ));
        }
        let file = File::options().read(true).append(true).open(path)?;
        let writer = RolloutWriter::resumed(tokio::fs::File::from_std(file), state);
        Ok(Self::spawn(runtime, thread_id, path.to_path_buf(), writer))
    }

    fn spawn(runtime: &Handle, thread_id: &str, path: PathBuf, writer: RolloutWriter) -> Self {
        let committed_bytes = Arc::clone(&writer.committed_bytes);
        committed_bytes.store(
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
            Ordering::Release,
        );
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let writer_path = path.clone();
        drop(runtime.spawn(async move {
            let (outcome, shutdown) = writer.run(receiver).await;
            if let Err(source) = &outcome {
                error!(
                    target: "nanocodex",
                    rollout_path = %writer_path.display(),
                    error = %source,
                    "Codex rollout writer stopped"
                );
            }
            if let Some(result) = shutdown {
                drop(result.send(outcome));
            }
        }));
        Self {
            info: RolloutInfo {
                thread_id: thread_id.to_owned(),
                path,
                committed_bytes,
            },
            commands,
        }
    }

    pub(crate) const fn info(&self) -> &RolloutInfo {
        &self.info
    }

    pub(crate) async fn accepted_input(
        &self,
        input: nanocodex_oai_api::events::AcceptedInput,
    ) -> io::Result<()> {
        let (result, receive) = oneshot::channel();
        self.commands
            .send(RolloutCommand::Input { input, result })
            .await
            .map_err(|_| io::Error::other("rollout writer stopped"))?;
        receive
            .await
            .map_err(|_| io::Error::other("rollout writer stopped"))?
    }

    pub(crate) async fn persist(
        &self,
        session: &CommittedSession,
        turn: RolloutTurn,
    ) -> io::Result<()> {
        self.persist_commit(RolloutCommit::from_session(session, turn))
            .await
    }

    pub(crate) async fn persist_compaction(
        &self,
        session: &CommittedSession,
        turn: RolloutTurn,
    ) -> io::Result<()> {
        self.persist_commit(RolloutCommit::compaction(session, turn))
            .await
    }

    async fn persist_commit(&self, commit: RolloutCommit) -> io::Result<()> {
        let (result, receiver) = oneshot::channel();
        self.commands
            .send(RolloutCommand::Commit {
                commit: Box::new(commit),
                result,
            })
            .await
            .map_err(|_| io::Error::other("Codex rollout writer stopped"))?;
        receiver
            .await
            .map_err(|_| io::Error::other("Codex rollout writer stopped"))?
    }

    #[cfg(test)]
    pub(in crate::rollout) async fn persist_history(
        &self,
        history: ResponseHistory,
        revision: u64,
        turn: RolloutTurn,
    ) -> io::Result<()> {
        self.persist_commit(RolloutCommit::from_history(history, revision, turn))
            .await
    }

    pub(crate) async fn flush(&self) -> io::Result<()> {
        let (result, receiver) = oneshot::channel();
        self.commands
            .send(RolloutCommand::Flush { result })
            .await
            .map_err(|_| io::Error::other("Codex rollout writer stopped"))?;
        receiver
            .await
            .map_err(|_| io::Error::other("Codex rollout writer stopped"))?
    }

    pub(crate) async fn shutdown(&self) -> io::Result<()> {
        let (result, receiver) = oneshot::channel();
        if self
            .commands
            .send(RolloutCommand::Shutdown { result })
            .await
            .is_err()
        {
            return Ok(());
        }
        receiver
            .await
            .map_err(|_| io::Error::other("Codex rollout writer stopped during shutdown"))?
    }
}
