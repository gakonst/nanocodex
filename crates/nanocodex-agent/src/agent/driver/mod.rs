mod branch;
mod control;
mod telemetry;

use super::*;
pub(super) use branch::{AgentOrigin, BranchSpawner};
pub(super) use control::DriverShutdown;
use control::{
    begin_shutdown, cancel_queued_turn, handle_idle_command, mark_all_queued_turns_cancelled,
};
use telemetry::{ReasoningSettings, agent_compact_span, agent_turn_span};

/// Sole owner of mutable run state and the Responses service stack.
pub(super) struct AgentDriver<S> {
    pub(super) commands: mpsc::Receiver<Command>,
    pub(super) events: EventSink,
    pub(super) client: ResponsesClient<S>,
    pub(super) transport_stats: Arc<TransportStats>,
    pub(super) tools: Tools,
    pub(super) workspace: Option<Arc<str>>,
    pub(super) spawner: BranchSpawner<S>,
    pub(super) initial_model: Option<PreparedCheckpoint>,
    pub(super) origin: AgentOrigin,
    pub(super) durability: Durability,
}

impl<S> AgentDriver<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<ResponseError> + AgentSend + 'static,
    S::Future: AgentSend,
{
    /// Drives queued turns until explicit shutdown or every command handle is dropped.
    ///
    /// # Errors
    ///
    /// Returns an infrastructure error while receiving or starting a command.
    #[allow(clippy::too_many_lines)]
    pub(super) async fn run(mut self) -> Result<()> {
        let session_id = self.events.request_id().to_owned();
        let mut default_thinking = self.spawner.config.thinking;
        let mut default_fast_mode = self.spawner.config.fast_mode;
        let inherited_checkpoint = self.initial_model.as_ref().map(|initial| {
            Arc::new(CommittedSession::new(
                Arc::clone(&self.spawner.lineage_id),
                initial.checkpoint.clone(),
            ))
        });
        let prompt_cache_key = self
            .spawner
            .prompt_cache_key
            .as_ref()
            .map_or_else(|| Arc::clone(&self.spawner.lineage_id), Arc::clone);
        let prompt_cache =
            ModelPromptCache::new(prompt_cache_key, self.spawner.shared_prompt_cache.clone());
        let mut model = if let Some(initial) = self.initial_model.take() {
            ModelRun::from_checkpoint(
                self.events.clone(),
                Arc::clone(&self.spawner.config),
                self.client,
                Arc::clone(&self.transport_stats),
                self.tools.clone(),
                prompt_cache.clone(),
                initial,
            )
        } else {
            ModelRun::new(
                self.events.clone(),
                Arc::clone(&self.spawner.config),
                self.client,
                Arc::clone(&self.transport_stats),
                self.tools.clone(),
                prompt_cache.clone(),
                self.spawner.context_source.clone(),
            )
        };
        let mut turn_index = 0_u64;
        let mut logical_turn_index = 0_u64;
        let mut latest_fork_checkpoint = inherited_checkpoint;
        let mut queued_turns = VecDeque::new();
        let mut pending_compact = None;
        let mut commands_open = true;
        loop {
            let command = loop {
                if let Some((parent, result)) = pending_compact.take() {
                    break Command::Compact { parent, result };
                }
                if let Some(queued) = queued_turns.pop_front() {
                    match queued {
                        QueuedTurn::Pending {
                            key,
                            prompt,
                            thinking,
                            fast_mode,
                            parent,
                            events,
                            result,
                        } => {
                            break Command::Prompt {
                                key,
                                prompt,
                                thinking: Some(thinking),
                                fast_mode: Some(fast_mode),
                                parent,
                                events,
                                result,
                            };
                        }
                        QueuedTurn::Cancelled {
                            prompt,
                            thinking,
                            fast_mode,
                            parent,
                            events,
                            result,
                        } => {
                            turn_index += 1;
                            let prompt_content = tracing::enabled!(
                                target: "nanocodex",
                                tracing::Level::INFO
                            )
                            .then(|| serde_json::to_string(&prompt).ok())
                            .flatten();
                            let turn_span = agent_turn_span(
                                parent.as_ref(),
                                session_id.as_str(),
                                self.spawner.lineage_id.as_ref(),
                                &self.origin,
                                ReasoningSettings {
                                    mode: self.spawner.config.reasoning_mode,
                                    effort: thinking,
                                },
                                turn_index,
                                prompt.instruction.text_bytes(),
                            );
                            drop(parent);
                            turn_span.record("status", "cancelled");
                            turn_span.record("otel.status_code", "ERROR");
                            if let Some(prompt_content) = &prompt_content {
                                turn_span.in_scope(|| {
                                    info!(
                                        target: "nanocodex",
                                        content_kind = "prompt",
                                        content = prompt_content.as_str(),
                                        "turn content"
                                    );
                                });
                            }
                            let _guard = turn_span.enter();
                            model.set_events(events);
                            model.emit_cancelled_before_start(
                                &prompt,
                                self.workspace.as_deref(),
                                thinking,
                                fast_mode,
                            )?;
                            model.set_events(self.events.clone());
                            drop(result.send(Err(NanocodexError::TurnCancelled)));
                            continue;
                        }
                    }
                }
                if commands_open {
                    let Some(command) = self.commands.recv().await else {
                        commands_open = false;
                        continue;
                    };
                    if let Command::Shutdown = command {
                        begin_shutdown(
                            &mut self.commands,
                            &mut queued_turns,
                            default_thinking,
                            default_fast_mode,
                        )
                        .await;
                        commands_open = false;
                        continue;
                    }
                    break command;
                }
                model.shutdown().await;
                return Ok(());
            };
            let Command::Prompt {
                key,
                prompt,
                thinking,
                fast_mode,
                parent,
                events,
                result,
            } = command
            else {
                if let Command::SetThinking { thinking, result } = command {
                    default_thinking = thinking;
                    drop(result.send(Ok(())));
                    continue;
                }
                if let Command::SetFastMode { enabled, result } = command {
                    default_fast_mode = enabled;
                    drop(result.send(Ok(())));
                    continue;
                }
                if let Command::Compact { parent, result } = command {
                    logical_turn_index = logical_turn_index.saturating_add(1);
                    let span = agent_compact_span(
                        parent.as_ref(),
                        session_id.as_str(),
                        self.spawner.lineage_id.as_ref(),
                        &self.origin,
                    );
                    drop(parent);
                    let compact_started = web_time::Instant::now();
                    let durability_turn = self.durability.start_compaction();
                    let mut compact_replaced = false;
                    let (cancel_compaction, mut cancel_compaction_rx) = oneshot::channel();
                    let mut cancel_compaction = Some(cancel_compaction);
                    let mut execution = Box::pin(
                        model
                            .compact(
                                self.workspace.clone(),
                                default_thinking,
                                default_fast_mode,
                                logical_turn_index,
                                &mut cancel_compaction_rx,
                            )
                            .instrument(span.clone()),
                    );
                    let completed = loop {
                        if !commands_open {
                            break execution.as_mut().await;
                        }
                        tokio::select! {
                            biased;
                            outcome = &mut execution => break outcome,
                            command = self.commands.recv() => {
                                match command {
                                    Some(Command::Prompt {
                                        key,
                                        prompt,
                                        thinking: _,
                                        fast_mode: _,
                                        parent,
                                        events,
                                        result,
                                    }) => {
                                        queued_turns.push_back(QueuedTurn::Pending {
                                            key,
                                            prompt,
                                            thinking: default_thinking,
                                            fast_mode: default_fast_mode,
                                            parent,
                                            events,
                                            result,
                                        });
                                    }
                                    Some(Command::Compact { parent, result }) => {
                                        compact_replaced = true;
                                        pending_compact = Some((parent, result));
                                        if let Some(cancel) = cancel_compaction.take() {
                                            let _ = cancel.send(());
                                        }
                                        break execution.as_mut().await;
                                    }
                                    Some(Command::Cancel { key, result }) => {
                                        let outcome = if cancel_queued_turn(&mut queued_turns, key) {
                                            Ok(())
                                        } else {
                                            Err(NanocodexError::TurnNotCancellable)
                                        };
                                        drop(result.send(outcome));
                                    }
                                    Some(Command::Steer { result, .. }) => {
                                        drop(result.send(Err(NanocodexError::TurnNotSteerable)));
                                    }
                                    Some(command @ (Command::Fork { .. } | Command::Spawn { .. })) => {
                                        handle_idle_command(
                                            command,
                                            latest_fork_checkpoint.as_ref(),
                                            &self.spawner,
                                            default_thinking,
                                            default_fast_mode,
                                            session_id.as_str(),
                                            self.workspace.clone(),
                                        );
                                    }
                                    Some(Command::SetThinking { thinking, result }) => {
                                        default_thinking = thinking;
                                        drop(result.send(Ok(())));
                                    }
                                    Some(Command::SetFastMode { enabled, result }) => {
                                        default_fast_mode = enabled;
                                        drop(result.send(Ok(())));
                                    }
                                    Some(Command::Shutdown) => {
                                        if let Some(cancel) = cancel_compaction.take() {
                                            let _ = cancel.send(());
                                        }
                                        begin_shutdown(
                                            &mut self.commands,
                                            &mut queued_turns,
                                            default_thinking,
                                            default_fast_mode,
                                        )
                                        .await;
                                        commands_open = false;
                                        break execution.as_mut().await;
                                    }
                                    None => {
                                        commands_open = false;
                                        mark_all_queued_turns_cancelled(&mut queued_turns);
                                        if let Some(cancel) = cancel_compaction.take() {
                                            let _ = cancel.send(());
                                        }
                                        break execution.as_mut().await;
                                    }
                                }
                            }
                        }
                    };
                    drop(execution);
                    let outcome = match completed {
                        Ok(ModelCompactOutcome::Completed(checkpoint)) => {
                            let checkpoint = Arc::new(CommittedSession::new(
                                Arc::clone(&self.spawner.lineage_id),
                                checkpoint,
                            ));
                            // The installed in-memory boundary is authoritative.
                            // Rollout writes follow the same retry-on-flush
                            // contract as completed prompt turns and must not
                            // roll back or hide the safe fork checkpoint.
                            latest_fork_checkpoint = Some(Arc::clone(&checkpoint));
                            self.durability
                                .persist_compaction(
                                    &checkpoint,
                                    durability_turn.completed_without_message(),
                                )
                                .await;
                            Ok(())
                        }
                        Ok(ModelCompactOutcome::Cancelled(checkpoint)) => {
                            let checkpoint = Arc::new(CommittedSession::new(
                                Arc::clone(&self.spawner.lineage_id),
                                checkpoint,
                            ));
                            latest_fork_checkpoint = Some(Arc::clone(&checkpoint));
                            let durability_turn = if compact_replaced {
                                durability_turn.replaced()
                            } else {
                                durability_turn.interrupted()
                            };
                            self.durability
                                .persist(&checkpoint, durability_turn)
                                .instrument(span.clone())
                                .await;
                            model.replace_client(ResponsesClient::new((self
                                .spawner
                                .service_factory)(
                            )));
                            Err(NanocodexError::TurnCancelled)
                        }
                        Ok(ModelCompactOutcome::Failed { error, checkpoint }) => {
                            let checkpoint = Arc::new(CommittedSession::new(
                                Arc::clone(&self.spawner.lineage_id),
                                checkpoint,
                            ));
                            latest_fork_checkpoint = Some(Arc::clone(&checkpoint));
                            self.durability
                                .persist(&checkpoint, durability_turn.failed())
                                .instrument(span.clone())
                                .await;
                            Err(error)
                        }
                        Err(error) => Err(error),
                    };
                    span.record(
                        "status",
                        if matches!(&outcome, Err(NanocodexError::TurnCancelled)) {
                            "cancelled"
                        } else if outcome.is_ok() {
                            "completed"
                        } else {
                            "failed"
                        },
                    );
                    span.record(
                        "otel.status_code",
                        if outcome.is_ok() { "OK" } else { "ERROR" },
                    );
                    span.record(
                        "duration_ns",
                        u64::try_from(compact_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
                    drop(result.send(outcome));
                    continue;
                }
                handle_idle_command(
                    command,
                    latest_fork_checkpoint.as_ref(),
                    &self.spawner,
                    default_thinking,
                    default_fast_mode,
                    session_id.as_str(),
                    self.workspace.clone(),
                );
                continue;
            };
            let thinking = thinking.unwrap_or(default_thinking);
            let fast_mode = fast_mode.unwrap_or(default_fast_mode);
            turn_index += 1;
            logical_turn_index = logical_turn_index.saturating_add(1);
            let prompt_content = tracing::enabled!(
                target: "nanocodex",
                tracing::Level::INFO
            )
            .then(|| serde_json::to_string(&prompt).ok())
            .flatten();
            let turn_span = agent_turn_span(
                parent.as_ref(),
                session_id.as_str(),
                self.spawner.lineage_id.as_ref(),
                &self.origin,
                ReasoningSettings {
                    mode: self.spawner.config.reasoning_mode,
                    effort: thinking,
                },
                turn_index,
                prompt.instruction.text_bytes(),
            );
            drop(parent);
            if let Some(prompt_content) = &prompt_content {
                turn_span.in_scope(|| {
                    info!(
                        target: "nanocodex",
                        content_kind = "prompt",
                        content = prompt_content.as_str(),
                        "turn content"
                    );
                });
            }
            let durability_turn = self.durability.start_turn(&prompt);
            let (steers, steer_rx) = mpsc::channel(STEER_CAPACITY);
            let (cancel, cancel_rx) = oneshot::channel();
            let (fork_snapshots, mut fork_snapshot_rx) = watch::channel(None);
            let mut fork_snapshots_open = true;
            let mut cancel = Some(cancel);
            let mut cancel_result = None;
            model.set_events(events);
            let mut execution = Box::pin(
                model
                    .execute(
                        prompt,
                        self.workspace.clone(),
                        thinking,
                        fast_mode,
                        logical_turn_index,
                        steer_rx,
                        cancel_rx,
                        fork_snapshots,
                    )
                    .instrument(turn_span.clone()),
            );
            let completed = loop {
                if !commands_open {
                    break execution.as_mut().await;
                }
                tokio::select! {
                    biased;
                    changed = fork_snapshot_rx.changed(), if fork_snapshots_open => {
                        if changed.is_err() {
                            fork_snapshots_open = false;
                            continue;
                        }
                        let snapshot = fork_snapshot_rx.borrow_and_update().clone();
                        if let Some(snapshot) = snapshot {
                            latest_fork_checkpoint = Some(Arc::new(CommittedSession::new(
                                Arc::clone(&self.spawner.lineage_id),
                                snapshot,
                            )));
                        }
                    }
                    outcome = &mut execution => break outcome,
                    command = self.commands.recv() => {
                        match command {
                            Some(Command::Prompt {
                                key,
                                prompt,
                                thinking: _,
                                fast_mode: _,
                                parent,
                                events,
                                result,
                            }) => {
                                queued_turns.push_back(QueuedTurn::Pending {
                                    key,
                                    prompt,
                                    thinking: default_thinking,
                                    fast_mode: default_fast_mode,
                                    parent,
                                    events,
                                    result,
                                });
                            }
                            Some(Command::Steer { key: target, prompt, result }) => {
                                if target != key {
                                    drop(result.send(Err(NanocodexError::TurnNotSteerable)));
                                    continue;
                                }
                                let outcome = steers.try_send(prompt).map_err(|error| match error {
                                    mpsc::error::TrySendError::Full(_) => {
                                        NanocodexError::SteerQueueFull
                                    }
                                    mpsc::error::TrySendError::Closed(_) => {
                                        NanocodexError::TurnNotSteerable
                                    }
                                });
                                drop(result.send(outcome));
                            }
                            Some(Command::Cancel { key: target, result: cancellation }) => {
                                if target != key {
                                    if cancel_queued_turn(&mut queued_turns, target) {
                                        drop(cancellation.send(Ok(())));
                                    } else {
                                        drop(cancellation.send(Err(
                                            NanocodexError::TurnNotCancellable,
                                        )));
                                    }
                                    continue;
                                }
                                let Some(cancel) = cancel.take() else {
                                    drop(cancellation.send(Err(
                                        NanocodexError::TurnNotCancellable,
                                    )));
                                    continue;
                                };
                                let _ = cancel.send(());
                                cancel_result = Some(cancellation);
                                break execution.as_mut().await;
                            }
                            Some(command @ (Command::Fork { .. } | Command::Spawn { .. })) => {
                                if let Some(snapshot) =
                                    fork_snapshot_rx.borrow_and_update().clone()
                                {
                                    latest_fork_checkpoint =
                                        Some(Arc::new(CommittedSession::new(
                                            Arc::clone(&self.spawner.lineage_id),
                                            snapshot,
                                        )));
                                }
                                handle_idle_command(
                                    command,
                                    latest_fork_checkpoint.as_ref(),
                                    &self.spawner,
                                    default_thinking,
                                    default_fast_mode,
                                    session_id.as_str(),
                                    self.workspace.clone(),
                                );
                            }
                            Some(Command::SetThinking { thinking, result }) => {
                                default_thinking = thinking;
                                drop(result.send(Ok(())));
                            }
                            Some(Command::SetFastMode { enabled, result }) => {
                                default_fast_mode = enabled;
                                drop(result.send(Ok(())));
                            }
                            Some(Command::Compact { parent, result }) => {
                                pending_compact = Some((parent, result));
                                if let Some(cancel) = cancel.take() {
                                    let _ = cancel.send(());
                                }
                                break execution.as_mut().await;
                            }
                            Some(Command::Shutdown) => {
                                if let Some(cancel) = cancel.take() {
                                    let _ = cancel.send(());
                                }
                                begin_shutdown(
                                    &mut self.commands,
                                    &mut queued_turns,
                                    default_thinking,
                                    default_fast_mode,
                                )
                                .await;
                                commands_open = false;
                            }
                            None => {
                                commands_open = false;
                                mark_all_queued_turns_cancelled(&mut queued_turns);
                                if let Some(cancel) = cancel.take() {
                                    let _ = cancel.send(());
                                }
                            }
                        }
                    }
                }
            };
            drop(execution);
            model.set_events(self.events.clone());
            let (outcome, was_cancelled): (Result<TurnResult>, bool) = match completed {
                Ok(ModelTurnOutcome::Completed(completed)) => {
                    let CompletedModelTurn {
                        final_message,
                        usage,
                        checkpoint,
                    } = completed;
                    let checkpoint = Arc::new(CommittedSession::new(
                        Arc::clone(&self.spawner.lineage_id),
                        checkpoint,
                    ));
                    let durability_turn = durability_turn.completed(final_message.clone());
                    self.durability
                        .persist(&checkpoint, durability_turn)
                        .instrument(turn_span.clone())
                        .await;
                    latest_fork_checkpoint = Some(Arc::clone(&checkpoint));
                    (
                        Ok(TurnResult {
                            final_message,
                            usage,
                            checkpoint,
                        }),
                        false,
                    )
                }
                Ok(ModelTurnOutcome::Cancelled(checkpoint)) => {
                    let checkpoint = Arc::new(CommittedSession::new(
                        Arc::clone(&self.spawner.lineage_id),
                        checkpoint,
                    ));
                    let durability_turn = durability_turn.interrupted();
                    self.durability
                        .persist(&checkpoint, durability_turn)
                        .instrument(turn_span.clone())
                        .await;
                    latest_fork_checkpoint = Some(Arc::clone(&checkpoint));
                    model.replace_client(ResponsesClient::new((self.spawner.service_factory)()));
                    (Err(NanocodexError::TurnCancelled), true)
                }
                Ok(ModelTurnOutcome::Failed { error, checkpoint }) => {
                    let checkpoint = Arc::new(CommittedSession::new(
                        Arc::clone(&self.spawner.lineage_id),
                        checkpoint,
                    ));
                    let durability_turn = durability_turn.failed();
                    self.durability
                        .persist(&checkpoint, durability_turn)
                        .instrument(turn_span.clone())
                        .await;
                    latest_fork_checkpoint = Some(checkpoint);
                    (Err(error), false)
                }
                Err(error) => (Err(error), false),
            };
            turn_span.record(
                "status",
                if was_cancelled {
                    "cancelled"
                } else if outcome.is_ok() {
                    "completed"
                } else {
                    "failed"
                },
            );
            turn_span.record(
                "otel.status_code",
                if outcome.is_ok() { "OK" } else { "ERROR" },
            );
            drop(result.send(outcome));
            if let Some(cancel_result) = cancel_result {
                let outcome = if was_cancelled {
                    Ok(())
                } else {
                    Err(NanocodexError::TurnNotCancellable)
                };
                drop(cancel_result.send(outcome));
            }
        }
    }
}
