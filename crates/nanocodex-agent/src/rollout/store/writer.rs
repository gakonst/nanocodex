use super::super::{load::validate_legacy_history_mode, wire::*};
use super::*;

pub(in crate::rollout) struct RolloutWriter {
    file: tokio::fs::File,
    pub(in crate::rollout) pending: Option<RolloutCommit>,
    written_revision: Option<u64>,
    written_len: usize,
    written_context_baseline: Option<ContextBaseline>,
    workspace: PathBuf,
    window_number: u64,
    first_window_id: String,
    current_window_id: String,
    #[cfg(test)]
    injected_write_failures: usize,
}

impl RolloutWriter {
    pub(in crate::rollout) fn new(
        file: tokio::fs::File,
        initial_window_id: String,
        workspace: PathBuf,
    ) -> Self {
        Self {
            file,
            pending: None,
            written_revision: None,
            written_len: 0,
            written_context_baseline: None,
            workspace,
            window_number: 0,
            first_window_id: initial_window_id.clone(),
            current_window_id: initial_window_id,
            #[cfg(test)]
            injected_write_failures: 0,
        }
    }

    pub(super) fn resumed(file: tokio::fs::File, state: ResumeWriterState) -> Self {
        Self {
            file,
            pending: None,
            written_revision: Some(0),
            written_len: state.written_len,
            written_context_baseline: state.context_baseline,
            workspace: state.workspace,
            window_number: state.window_number,
            first_window_id: state.first_window_id,
            current_window_id: state.current_window_id,
            #[cfg(test)]
            injected_write_failures: 0,
        }
    }

    pub(super) async fn run(
        mut self,
        mut commands: mpsc::Receiver<RolloutCommand>,
    ) -> (io::Result<()>, Option<oneshot::Sender<io::Result<()>>>) {
        while let Some(command) = commands.recv().await {
            match command {
                RolloutCommand::Commit { commit, result } => {
                    self.pending = Some(*commit);
                    drop(result.send(self.persist_pending().await));
                }
                RolloutCommand::Flush { result } => {
                    drop(result.send(self.flush().await));
                }
                RolloutCommand::Shutdown { result } => {
                    commands.close();
                    return (self.flush().await, Some(result));
                }
            }
        }
        (self.flush().await, None)
    }

    pub(in crate::rollout) async fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_some() {
            self.persist_pending().await
        } else {
            self.file.flush().await?;
            self.file.sync_data().await
        }
    }

    pub(in crate::rollout) async fn persist_pending(&mut self) -> io::Result<()> {
        let Some(commit) = self.pending.take() else {
            return Ok(());
        };
        let persisted = self.append_with_retry(&commit).await;
        match persisted {
            Ok(()) => Ok(()),
            Err(source) => {
                self.pending = Some(commit);
                Err(source)
            }
        }
    }

    async fn append_with_retry(&mut self, commit: &RolloutCommit) -> io::Result<()> {
        let prepared = self.prepare_append(commit)?;
        let original_len = self.file.metadata().await?.len();
        let first_error = match self.write_prepared(&prepared).await {
            Ok(()) => {
                self.apply_prepared(prepared);
                return Ok(());
            }
            Err(source) => source,
        };

        self.rollback(original_len).await?;
        match self.write_prepared(&prepared).await {
            Ok(()) => {
                self.apply_prepared(prepared);
                Ok(())
            }
            Err(second) => {
                self.rollback(original_len).await?;
                Err(io::Error::new(
                    second.kind(),
                    format!(
                        "failed to append Codex rollout after retry; first error: {first_error}; final error: {second}"
                    ),
                ))
            }
        }
    }

    fn prepare_append(&self, commit: &RolloutCommit) -> io::Result<PreparedAppend> {
        let len = commit.history.len();
        match self.written_revision {
            None if commit.revision > 0 => {
                let window_number = self.window_number.saturating_add(1);
                let window_id = uuid::Uuid::now_v7().to_string();
                Ok(PreparedAppend {
                    records: PreparedRecords::Compacted(CompactedItem {
                        message: String::new(),
                        replacement_history: commit.history.iter().cloned().collect(),
                        window_number,
                        first_window_id: self.first_window_id.clone(),
                        previous_window_id: self.current_window_id.clone(),
                        window_id: window_id.clone(),
                    }),
                    revision: commit.revision,
                    len,
                    window: Some(WindowAdvance {
                        number: window_number,
                        id: window_id,
                    }),
                    turn: commit.turn.clone(),
                    model: commit.model,
                    context_baseline: commit.context_baseline.clone(),
                    write_state: true,
                })
            }
            None => Ok(PreparedAppend {
                records: PreparedRecords::Items {
                    history: commit.history.clone(),
                    start: 0,
                },
                revision: commit.revision,
                len,
                window: None,
                turn: commit.turn.clone(),
                model: commit.model,
                context_baseline: commit.context_baseline.clone(),
                write_state: self.written_context_baseline.as_ref()
                    != Some(&commit.context_baseline),
            }),
            Some(revision) if revision == commit.revision => {
                if len < self.written_len {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "committed history shrank without a compaction revision",
                    ));
                }
                Ok(PreparedAppend {
                    records: PreparedRecords::Items {
                        history: commit.history.clone(),
                        start: self.written_len,
                    },
                    revision,
                    len,
                    window: None,
                    turn: commit.turn.clone(),
                    model: commit.model,
                    context_baseline: commit.context_baseline.clone(),
                    write_state: self.written_context_baseline.as_ref()
                        != Some(&commit.context_baseline),
                })
            }
            Some(_) => {
                let window_number = self.window_number.saturating_add(1);
                let window_id = uuid::Uuid::now_v7().to_string();
                Ok(PreparedAppend {
                    records: PreparedRecords::Compacted(CompactedItem {
                        message: String::new(),
                        replacement_history: commit.history.iter().cloned().collect(),
                        window_number,
                        first_window_id: self.first_window_id.clone(),
                        previous_window_id: self.current_window_id.clone(),
                        window_id: window_id.clone(),
                    }),
                    revision: commit.revision,
                    len,
                    window: Some(WindowAdvance {
                        number: window_number,
                        id: window_id,
                    }),
                    turn: commit.turn.clone(),
                    model: commit.model,
                    context_baseline: commit.context_baseline.clone(),
                    // A compaction starts a new history window, whose context
                    // baseline must be independently reconstructable.
                    write_state: true,
                })
            }
        }
    }

    async fn write_prepared(&mut self, prepared: &PreparedAppend) -> io::Result<()> {
        #[cfg(test)]
        if self.injected_write_failures > 0 {
            self.injected_write_failures -= 1;
            return Err(io::Error::other("injected rollout write failure"));
        }
        let turn = &prepared.turn;
        self.write_event(CodexEvent::TaskStarted {
            turn_id: &turn.turn_id,
            started_at: turn.started_at,
            model_context_window: None,
            collaboration_mode_kind: "default",
        })
        .await?;
        if let Some(user_message) = &turn.user_message {
            self.write_event(CodexEvent::UserMessage(user_message))
                .await?;
        }
        match &prepared.records {
            PreparedRecords::Items { history, start } => {
                self.write_turn_context(turn, prepared.model).await?;
                for item in history.iter_from(*start) {
                    write_async_line(
                        &mut self.file,
                        &RolloutLine {
                            timestamp: timestamp(),
                            item: RolloutItem::ResponseItem(item),
                        },
                    )
                    .await?;
                }
            }
            PreparedRecords::Compacted(compacted) => {
                write_async_line(
                    &mut self.file,
                    &RolloutLine {
                        timestamp: timestamp(),
                        item: RolloutItem::Compacted(compacted),
                    },
                )
                .await?;
                self.write_turn_context(turn, prepared.model).await?;
            }
        }
        if prepared.write_state {
            write_async_line(
                &mut self.file,
                &RolloutLine {
                    timestamp: timestamp(),
                    item: RolloutItem::WorldState(&WorldStateItem {
                        full: true,
                        state: PersistedContextState {
                            nanocodex_context: &prepared.context_baseline,
                        },
                    }),
                },
            )
            .await?;
        }
        if let Some(message) = turn.final_message.as_deref() {
            self.write_event(CodexEvent::AgentMessage {
                message,
                phase: "final_answer",
                memory_citation: None,
            })
            .await?;
        }
        match turn.status {
            RolloutTurnStatus::Completed => {
                self.write_event(CodexEvent::TaskComplete {
                    turn_id: &turn.turn_id,
                    last_agent_message: turn.final_message.as_deref(),
                    started_at: turn.started_at,
                    completed_at: turn.completed_at,
                    duration_ms: turn.duration_ms,
                    time_to_first_token_ms: None,
                })
                .await?;
            }
            RolloutTurnStatus::Interrupted => {
                self.write_event(CodexEvent::TurnAborted {
                    turn_id: &turn.turn_id,
                    reason: "interrupted",
                    started_at: turn.started_at,
                    completed_at: turn.completed_at,
                    duration_ms: turn.duration_ms,
                })
                .await?;
            }
            RolloutTurnStatus::Replaced => {
                self.write_event(CodexEvent::TurnAborted {
                    turn_id: &turn.turn_id,
                    reason: "replaced",
                    started_at: turn.started_at,
                    completed_at: turn.completed_at,
                    duration_ms: turn.duration_ms,
                })
                .await?;
            }
            RolloutTurnStatus::Failed => {
                self.write_event(CodexEvent::TaskComplete {
                    turn_id: &turn.turn_id,
                    last_agent_message: None,
                    started_at: turn.started_at,
                    completed_at: turn.completed_at,
                    duration_ms: turn.duration_ms,
                    time_to_first_token_ms: None,
                })
                .await?;
            }
            RolloutTurnStatus::InProgress => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot persist an unfinished Codex rollout turn",
                ));
            }
        }
        self.file.flush().await?;
        self.file.sync_data().await
    }

    async fn write_event(&mut self, event: CodexEvent<'_>) -> io::Result<()> {
        write_async_line(
            &mut self.file,
            &RolloutLine {
                timestamp: timestamp(),
                item: RolloutItem::Event(&event),
            },
        )
        .await
    }

    async fn write_turn_context(&mut self, turn: &RolloutTurn, model: Model) -> io::Result<()> {
        write_async_line(
            &mut self.file,
            &RolloutLine {
                timestamp: timestamp(),
                item: RolloutItem::TurnContext(&TurnContext {
                    cwd: &self.workspace,
                    approval_policy: "never",
                    sandbox_policy: SandboxPolicy {
                        kind: "danger-full-access",
                    },
                    model: model.as_str(),
                    effort: turn.effort.as_str(),
                    summary: "auto",
                }),
            },
        )
        .await
    }

    fn apply_prepared(&mut self, prepared: PreparedAppend) {
        self.written_revision = Some(prepared.revision);
        self.written_len = prepared.len;
        if prepared.write_state {
            self.written_context_baseline = Some(prepared.context_baseline);
        }
        if let Some(window) = prepared.window {
            self.window_number = window.number;
            self.current_window_id = window.id;
        }
    }

    async fn rollback(&mut self, len: u64) -> io::Result<()> {
        self.file.set_len(len).await?;
        self.file.seek(std::io::SeekFrom::Start(len)).await?;
        self.file.sync_data().await
    }

    #[cfg(test)]
    pub(in crate::rollout) const fn inject_write_failures(&mut self, count: usize) {
        self.injected_write_failures = count;
    }
}

struct PreparedAppend {
    records: PreparedRecords,
    revision: u64,
    len: usize,
    window: Option<WindowAdvance>,
    turn: RolloutTurn,
    model: Model,
    context_baseline: ContextBaseline,
    write_state: bool,
}

enum PreparedRecords {
    Items {
        history: ResponseHistory,
        start: usize,
    },
    Compacted(CompactedItem),
}

struct WindowAdvance {
    number: u64,
    id: String,
}

pub(super) struct ResumeWriterState {
    pub(super) written_len: usize,
    workspace: PathBuf,
    context_baseline: Option<ContextBaseline>,
    window_number: u64,
    first_window_id: String,
    current_window_id: String,
}

pub(super) fn read_resume_writer_state(
    path: &Path,
    thread_id: &str,
) -> io::Result<ResumeWriterState> {
    let mut first_window_id = None;
    let mut current_window_id = None;
    let mut window_number = 0;
    let mut written_len = 0;
    let mut workspace = None;
    let mut context_baseline = None;
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let value: serde_json::Value = serde_json::from_str(&line).map_err(io::Error::other)?;
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("session_meta") if first_window_id.is_none() => {
                let payload = &value["payload"];
                validate_legacy_history_mode(payload)?;
                if payload.get("id").and_then(serde_json::Value::as_str) != Some(thread_id) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Codex rollout thread ID does not match durable session",
                    ));
                }
                let window = payload["context_window"]["window_id"]
                    .as_str()
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Codex rollout is missing its initial context window",
                        )
                    })?
                    .to_owned();
                first_window_id = Some(window.clone());
                current_window_id = Some(window);
                workspace = Some(PathBuf::from(payload["cwd"].as_str().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Codex rollout is missing its workspace",
                    )
                })?));
            }
            Some("compacted") => {
                let payload = &value["payload"];
                written_len = payload["replacement_history"]
                    .as_array()
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Codex compaction is missing replacement history",
                        )
                    })?
                    .len();
                window_number = payload["window_number"].as_u64().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Codex compaction is missing its window number",
                    )
                })?;
                current_window_id = Some(
                    payload["window_id"]
                        .as_str()
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Codex compaction is missing its window ID",
                            )
                        })?
                        .to_owned(),
                );
                context_baseline = None;
            }
            Some("response_item") => written_len = written_len.saturating_add(1),
            Some("world_state") => {
                if let Some(state) = value["payload"]["state"].get("nanocodex_context") {
                    context_baseline =
                        Some(serde_json::from_value(state.clone()).map_err(io::Error::other)?);
                }
            }
            _ => {}
        }
    }
    let first_window_id = first_window_id.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex rollout is missing session metadata",
        )
    })?;
    let workspace = workspace.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex rollout is missing session metadata",
        )
    })?;
    Ok(ResumeWriterState {
        written_len,
        workspace,
        context_baseline,
        window_number,
        current_window_id: current_window_id.unwrap_or_else(|| first_window_id.clone()),
        first_window_id,
    })
}

pub(in crate::rollout) fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(in crate::rollout) fn write_line(
    output: &mut impl Write,
    line: &impl Serialize,
) -> io::Result<()> {
    serde_json::to_writer(&mut *output, line).map_err(io::Error::other)?;
    output.write_all(b"\n")
}

async fn write_async_line(output: &mut tokio::fs::File, line: &impl Serialize) -> io::Result<()> {
    let mut encoded = serde_json::to_vec(line).map_err(io::Error::other)?;
    encoded.push(b'\n');
    output.write_all(&encoded).await
}
