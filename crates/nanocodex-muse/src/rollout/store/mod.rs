pub(in crate::rollout) mod writer;

use super::wire::*;
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use writer::*;
// The internal file writer, not the public harness-neutral one.
use writer::RolloutWriter;

use crate::session::{Origin, SessionStart};

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
    pub(crate) start: SessionStart,
    pub(crate) parent_thread_id: Option<&'a str>,
    /// Root of the conversation tree; a fresh root is its own.
    pub(crate) root_session_id: Option<&'a str>,
}

impl SessionStart {
    /// Start recorded when this session's mirror file is created now. A
    /// reopened session that was never mirrored, such as a durable branch,
    /// records its persisted provenance rather than a root resume.
    pub(crate) const fn for_new_mirror(self, origin: Origin) -> Self {
        match (self, origin) {
            (Self::Resume | Self::Restore, Origin::Root) | (Self::New(_), _) => self,
            (Self::Resume | Self::Restore, origin) => Self::New(origin),
        }
    }
}

impl RolloutOrigin<'_> {
    /// Persisted `origin_kind`; side conversations are recorded as forks.
    const fn kind(self) -> &'static str {
        match self.start {
            SessionStart::New(Origin::Root) => "root",
            SessionStart::New(Origin::Fork | Origin::Branch | Origin::SideConversation) => "fork",
            SessionStart::New(Origin::Subagent) => "spawn",
            SessionStart::Resume => "resume",
            SessionStart::Restore => "restore",
        }
    }

    /// Persisted `conversation_role`.
    const fn conversation_role(self) -> &'static str {
        match self.start {
            SessionStart::New(Origin::Subagent) => "subagent",
            SessionStart::New(Origin::Fork | Origin::Branch) => "branch",
            SessionStart::New(Origin::SideConversation) => "side_conversation",
            SessionStart::New(Origin::Root) | SessionStart::Resume | SessionStart::Restore => {
                "root"
            }
        }
    }

    /// Whether the parent is also the conversation this one was copied from.
    const fn forked(self) -> bool {
        matches!(
            self.start,
            SessionStart::New(Origin::Fork | Origin::Branch | Origin::SideConversation)
        )
    }
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
    history: RolloutHistory,
    revision: u64,
    turn: RolloutTurn,
    model: &'static str,
    context_baseline: ContextBaseline,
    client_authored: std::collections::BTreeSet<String>,
}

impl RolloutCommit {
    #[cfg(feature = "openai")]
    fn from_session(session: &CommittedSession, turn: RolloutTurn) -> Self {
        Self {
            history: RolloutHistory::Shared(session.rollout_history()),
            revision: session.history_revision(),
            turn,
            model: session.selected_model().as_str(),
            context_baseline: session.context_baseline().clone(),
            client_authored: session.model().client_authored().clone(),
        }
    }

    #[cfg(feature = "openai")]
    fn compaction(session: &CommittedSession, turn: RolloutTurn) -> Self {
        Self {
            history: RolloutHistory::Shared(session.rollout_history()),
            revision: session.history_revision(),
            turn,
            model: session.selected_model().as_str(),
            context_baseline: session.context_baseline().clone(),
            client_authored: session.model().client_authored().clone(),
        }
    }

    pub(in crate::rollout) fn neutral(
        history: Vec<ResponseItem>,
        revision: u64,
        turn: RolloutTurn,
        model: crate::HarnessModel,
    ) -> Self {
        Self {
            history: RolloutHistory::Items(history.into()),
            revision,
            turn,
            model: model.as_str(),
            context_baseline: ContextBaseline::Missing,
            client_authored: std::collections::BTreeSet::new(),
        }
    }

    #[cfg(all(test, feature = "openai"))]
    pub(super) const fn from_history(
        history: ResponseHistory,
        revision: u64,
        turn: RolloutTurn,
    ) -> Self {
        Self {
            history: RolloutHistory::Shared(history),
            revision,
            turn,
            model: Model::Sol.as_str(),
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
                .get_or_init(|| origin.root_session_id.unwrap_or(thread_id).to_owned())
                .clone(),
            origin_kind: origin.kind().to_owned(),
            conversation_role: origin.conversation_role(),
            session_id: thread_id.to_owned(),
            id: thread_id.to_owned(),
            prompt_cache_key: prompt_cache_key.to_owned(),
            forked_from_id: origin.forked().then(|| parent_thread_id.clone()).flatten(),
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

    #[cfg(feature = "openai")]
    pub(crate) async fn persist(
        &self,
        session: &CommittedSession,
        turn: RolloutTurn,
    ) -> io::Result<()> {
        self.persist_commit(RolloutCommit::from_session(session, turn))
            .await
    }

    #[cfg(feature = "openai")]
    pub(crate) async fn persist_compaction(
        &self,
        session: &CommittedSession,
        turn: RolloutTurn,
    ) -> io::Result<()> {
        self.persist_commit(RolloutCommit::compaction(session, turn))
            .await
    }

    pub(in crate::rollout) async fn persist_neutral(
        &self,
        commit: RolloutCommit,
    ) -> io::Result<()> {
        self.persist_commit(commit).await
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

    #[cfg(all(test, feature = "openai"))]
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
