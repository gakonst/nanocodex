mod branch;
mod control;
mod telemetry;

use super::execution::AdmittedExecution;
use super::*;
pub(super) use branch::{AgentOrigin, BranchSpawner};
pub(super) use control::DriverShutdown;
use control::{
    TurnDefaults, begin_shutdown, cancel_queued_turn, handle_idle_command,
    mark_all_queued_turns_cancelled, queued_execution_operation,
};
use telemetry::{ReasoningSettings, agent_compact_span, agent_turn_span, emit_replayed_terminal};

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
    pub(super) execution: Execution,
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
        let thread_model = self.spawner.config.model;
        let mut default_thinking = self.spawner.config.thinking;
        let mut default_fast_mode = self.spawner.config.fast_mode;
        let inherited_checkpoint = self.initial_model.as_ref().map(|initial| {
            Arc::new(CommittedSession::new(
                Arc::clone(&self.spawner.lineage_id),
                thread_model,
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
        let mut pending_developer_messages = Vec::new();
        let mut commands_open = true;
        let mut shutdown_failures = Vec::new();
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
                            execution_operation,
                            thinking,
                            fast_mode,
                            parent,
                            events,
                            result,
                        } => {
                            break Command::Prompt {
                                key,
                                prompt,
                                execution_operation: execution_operation
                                    .map(ExecutionOperation::Admitted),
                                accepted: None,
                                thinking: Some(thinking),
                                fast_mode: Some(fast_mode),
                                parent,
                                events,
                                result,
                            };
                        }
                        QueuedTurn::Cancelled {
                            prompt,
                            execution_operation,
                            cancellation_committed,
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
                                    model: thread_model,
                                    mode: self.spawner.config.reasoning_mode,
                                    effort: thinking,
                                },
                                turn_index,
                                prompt.text_bytes(),
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
                            let persisted = if cancellation_committed {
                                Ok(())
                            } else if let Some(operation_id) = execution_operation {
                                self.execution
                                    .cancel_operation(&operation_id, &prompt)
                                    .await
                            } else {
                                Ok(())
                            };
                            let outcome = match persisted {
                                Ok(()) => {
                                    model.set_events(events);
                                    let emitted = model.emit_cancelled_before_start(
                                        &prompt,
                                        self.workspace.as_deref(),
                                        thinking,
                                        fast_mode,
                                    );
                                    model.set_events(self.events.clone());
                                    emitted.and(Err(NanocodexError::TurnCancelled))
                                }
                                Err(error) => {
                                    model.set_events(events);
                                    let emitted = model.emit_failed_before_start(
                                        &prompt,
                                        self.workspace.as_deref(),
                                        thinking,
                                        fast_mode,
                                        &error,
                                    );
                                    model.set_events(self.events.clone());
                                    emitted.and(Err(error))
                                }
                            };
                            if !commands_open
                                && let Err(error) = &outcome
                                && !matches!(error, NanocodexError::TurnCancelled)
                            {
                                shutdown_failures.push(error.to_string());
                            }
                            if commands_open && outcome_requires_reopen(&outcome) {
                                begin_shutdown(
                                    &mut self.commands,
                                    &mut queued_turns,
                                    default_thinking,
                                    default_fast_mode,
                                )
                                .await;
                                commands_open = false;
                            }
                            drop(_guard);
                            drop(result.send(outcome));
                            continue;
                        }
                    }
                }
                if commands_open {
                    let Some(command) = self.commands.recv().await else {
                        commands_open = false;
                        continue;
                    };
                    let mut reopen = false;
                    let Some(command) = accept_execution_command(
                        &self.execution,
                        &self.spawner.config,
                        default_thinking,
                        command,
                        &mut reopen,
                    )
                    .await
                    else {
                        if reopen {
                            begin_shutdown(
                                &mut self.commands,
                                &mut queued_turns,
                                default_thinking,
                                default_fast_mode,
                            )
                            .await;
                            commands_open = false;
                        }
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
                return if shutdown_failures.is_empty() {
                    Ok(())
                } else {
                    Err(NanocodexError::InvalidExecutionPolicy(format!(
                        "shutdown could not durably settle accepted work: {}",
                        shutdown_failures.join("; ")
                    )))
                };
            };
            let command = if matches!(command, Command::RoutePrompt { .. }) {
                let mut reopen = false;
                let Some(command) = accept_idle_route(
                    &self.execution,
                    &self.spawner.config,
                    default_thinking,
                    command,
                    &mut reopen,
                )
                .await
                else {
                    if reopen {
                        begin_shutdown(
                            &mut self.commands,
                            &mut queued_turns,
                            default_thinking,
                            default_fast_mode,
                        )
                        .await;
                        commands_open = false;
                    }
                    continue;
                };
                command
            } else {
                command
            };
            let Command::Prompt {
                key,
                prompt,
                execution_operation,
                accepted: _,
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
                if let Command::AppendDeveloperMessage { text, result } = command {
                    if !pending_developer_messages.is_empty() {
                        pending_developer_messages.push((text, result));
                        continue;
                    }
                    let outcome =
                        match model.append_developer_message(text, self.workspace.as_deref()) {
                            Ok(snapshot) => {
                                let checkpoint = Arc::new(CommittedSession::new(
                                    Arc::clone(&self.spawner.lineage_id),
                                    thread_model,
                                    snapshot,
                                ));
                                match self.execution.commit_checkpoint(&checkpoint).await {
                                    Ok(()) => {
                                        latest_fork_checkpoint = Some(checkpoint);
                                        agent_session_context(
                                            latest_fork_checkpoint.as_deref(),
                                            self.workspace.as_deref(),
                                            &self.spawner.context_source,
                                        )
                                    }
                                    Err(error) => {
                                        model = model_from_checkpoint(
                                            &self.events,
                                            &self.transport_stats,
                                            &self.tools,
                                            &self.spawner,
                                            &prompt_cache,
                                            latest_fork_checkpoint.as_deref(),
                                        );
                                        Err(error)
                                    }
                                }
                            }
                            Err(error) => Err(error),
                        };
                    let reopen = outcome_requires_reopen(&outcome);
                    drop(result.send(outcome));
                    if reopen {
                        begin_shutdown(
                            &mut self.commands,
                            &mut queued_turns,
                            default_thinking,
                            default_fast_mode,
                        )
                        .await;
                        commands_open = false;
                    }
                    continue;
                }
                if let Command::Context { result } = command {
                    drop(result.send(agent_session_context(
                        latest_fork_checkpoint.as_deref(),
                        self.workspace.as_deref(),
                        &self.spawner.context_source,
                    )));
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
                    let compact_base_checkpoint = latest_fork_checkpoint.clone();
                    let admitted = self
                        .execution
                        .admit_compaction(
                            compact_base_checkpoint.as_deref(),
                            thread_model,
                            default_thinking,
                            default_fast_mode,
                            self.workspace.as_deref(),
                        )
                        .await;
                    let (compaction_operation_id, admission) = match admitted {
                        Ok(admitted) => admitted,
                        Err(error) => {
                            let reopen = matches!(
                                error.execution_policy_disposition(),
                                Some(crate::ExecutionPolicyDisposition::Reopen)
                            );
                            span.record("status", "failed");
                            span.record("otel.status_code", "ERROR");
                            span.record(
                                "duration_ns",
                                u64::try_from(compact_started.elapsed().as_nanos())
                                    .unwrap_or(u64::MAX),
                            );
                            drop(result.send(Err(error)));
                            if reopen {
                                begin_shutdown(
                                    &mut self.commands,
                                    &mut queued_turns,
                                    default_thinking,
                                    default_fast_mode,
                                )
                                .await;
                                commands_open = false;
                            }
                            continue;
                        }
                    };
                    if !matches!(admission, AdmittedExecution::Execute) {
                        let error = NanocodexError::InvalidExecutionPolicy(
                            "standalone compaction identity resolved to an unexpected terminal operation"
                                .to_owned(),
                        );
                        span.record("status", "failed");
                        span.record("otel.status_code", "ERROR");
                        span.record(
                            "duration_ns",
                            u64::try_from(compact_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                        );
                        drop(result.send(Err(error)));
                        continue;
                    }
                    let execution_turn = self
                        .execution
                        .start_compaction(default_thinking, compaction_operation_id.clone());
                    if let Err(error) = execution_turn.begin().await {
                        if let Some(operation_id) = &compaction_operation_id {
                            self.execution.release_claim(operation_id).await;
                        }
                        let reopen = matches!(
                            error.execution_policy_disposition(),
                            Some(crate::ExecutionPolicyDisposition::Reopen)
                        );
                        span.record("status", "failed");
                        span.record("otel.status_code", "ERROR");
                        span.record(
                            "duration_ns",
                            u64::try_from(compact_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                        );
                        drop(result.send(Err(error)));
                        if reopen {
                            begin_shutdown(
                                &mut self.commands,
                                &mut queued_turns,
                                default_thinking,
                                default_fast_mode,
                            )
                            .await;
                            commands_open = false;
                        }
                        continue;
                    }
                    let execution_steps = execution_turn.steps();
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
                                execution_steps,
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
                                let mut reopen = false;
                                let command = match command {
                                    Some(command) => accept_execution_command(
                                        &self.execution,
                                        &self.spawner.config,
                                        default_thinking,
                                        command,
                                        &mut reopen,
                                    ).await,
                                    None => None,
                                };
                                if reopen {
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
                                match command {
                                    Some(Command::Prompt {
                                        key,
                                        prompt,
                                        execution_operation,
                                        accepted: _,
                                        thinking: _,
                                        fast_mode: _,
                                        parent,
                                        events,
                                        result,
                                    }) => {
                                        let execution_operation =
                                            execution_operation.map(ExecutionOperation::into_id);
                                        queued_turns.push_back(QueuedTurn::Pending {
                                            key,
                                            prompt,
                                            execution_operation,
                                            thinking: default_thinking,
                                            fast_mode: default_fast_mode,
                                            parent,
                                            events,
                                            result,
                                        });
                                    }
                                    Some(command @ Command::RoutePrompt { .. }) => {
                                        let mut reopen = false;
                                        let Some(Command::Prompt {
                                            key,
                                            prompt,
                                            execution_operation,
                                            accepted: _,
                                            thinking: _,
                                            fast_mode: _,
                                            parent,
                                            events,
                                            result,
                                        }) = accept_idle_route(
                                            &self.execution,
                                            &self.spawner.config,
                                            default_thinking,
                                            command,
                                            &mut reopen,
                                        )
                                        .await
                                        else {
                                            if reopen {
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
                                            continue;
                                        };
                                        queued_turns.push_back(QueuedTurn::Pending {
                                            key,
                                            prompt,
                                            execution_operation:
                                                execution_operation.map(ExecutionOperation::into_id),
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
                                        let outcome = match queued_execution_operation(
                                            &queued_turns,
                                            key,
                                        ) {
                                            Some((operation_id, prompt)) => {
                                                let persisted = match operation_id {
                                                    Some(operation_id) => {
                                                        self.execution
                                                            .cancel_operation(
                                                                &operation_id,
                                                                &prompt,
                                                            )
                                                            .await
                                                    }
                                                    None => Ok(()),
                                                };
                                                persisted.and_then(|()| {
                                                    if cancel_queued_turn(
                                                        &mut queued_turns,
                                                        key,
                                                        true,
                                                    ) {
                                                        Ok(())
                                                    } else {
                                                        Err(NanocodexError::TurnNotCancellable)
                                                    }
                                                })
                                            }
                                            None => Err(NanocodexError::TurnNotCancellable),
                                        };
                                        let reopen = outcome_requires_reopen(&outcome);
                                        drop(result.send(outcome));
                                        if reopen {
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
                                    }
                                    Some(Command::Steer { result, .. }) => {
                                        drop(result.send(Err(NanocodexError::TurnNotSteerable)));
                                    }
                                    Some(command @ (Command::Fork { .. } | Command::Spawn { .. } | Command::SpawnBatch { .. })) => {
                                        handle_idle_command(
                                            command,
                                            latest_fork_checkpoint.as_ref(),
                                            &self.spawner,
                                            TurnDefaults {
                                                model: thread_model,
                                                thinking: default_thinking,
                                                fast_mode: default_fast_mode,
                                            },
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
                                    Some(Command::AppendDeveloperMessage { text, result }) => {
                                        pending_developer_messages.push((text, result));
                                    }
                                    Some(Command::Context { result }) => {
                                        drop(result.send(agent_session_context(
                                            latest_fork_checkpoint.as_deref(),
                                            self.workspace.as_deref(),
                                            &self.spawner.context_source,
                                        )));
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
                    let mut compact_checkpoint_committed = false;
                    let outcome = match completed {
                        Ok(ModelCompactOutcome::Completed(checkpoint)) => {
                            let checkpoint = Arc::new(CommittedSession::new(
                                Arc::clone(&self.spawner.lineage_id),
                                thread_model,
                                checkpoint,
                            ));
                            let persisted = self
                                .execution
                                .persist_compaction(
                                    &checkpoint,
                                    execution_turn.completed_without_message(),
                                )
                                .await;
                            if persisted.is_ok() {
                                compact_checkpoint_committed = true;
                                latest_fork_checkpoint = Some(checkpoint);
                            }
                            persisted
                        }
                        Ok(ModelCompactOutcome::Cancelled(checkpoint)) => {
                            let checkpoint = Arc::new(CommittedSession::new(
                                Arc::clone(&self.spawner.lineage_id),
                                thread_model,
                                checkpoint,
                            ));
                            let execution_turn = if compact_replaced {
                                execution_turn.replaced()
                            } else {
                                execution_turn.interrupted()
                            }
                            .retain_pending_attempt(
                                "standalone compaction was interrupted before durable settlement",
                            );
                            let persisted = self
                                .execution
                                .persist(&checkpoint, execution_turn)
                                .instrument(span.clone())
                                .await;
                            match persisted {
                                Ok(()) => {
                                    model.replace_client(ResponsesClient::new((self
                                        .spawner
                                        .service_factory)(
                                        Arc::clone(&self.spawner.config),
                                    )));
                                    Err(NanocodexError::TurnCancelled)
                                }
                                Err(error) => Err(error),
                            }
                        }
                        Ok(ModelCompactOutcome::Failed { error, checkpoint }) => {
                            let checkpoint = Arc::new(CommittedSession::new(
                                Arc::clone(&self.spawner.lineage_id),
                                thread_model,
                                checkpoint,
                            ));
                            let persisted = self
                                .execution
                                .persist(
                                    &checkpoint,
                                    execution_turn.failed(error.to_string(), true),
                                )
                                .instrument(span.clone())
                                .await;
                            match persisted {
                                Ok(()) => Err(error),
                                Err(error) => Err(error),
                            }
                        }
                        Err(error) => Err(error),
                    };
                    if !compact_checkpoint_committed && outcome.is_err() {
                        latest_fork_checkpoint = compact_base_checkpoint;
                        model = model_from_checkpoint(
                            &self.events,
                            &self.transport_stats,
                            &self.tools,
                            &self.spawner,
                            &prompt_cache,
                            latest_fork_checkpoint.as_deref(),
                        );
                    }
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
                    let mut reopen_after_compaction = outcome_requires_reopen(&outcome);
                    if compact_checkpoint_committed && !reopen_after_compaction {
                        let mut developer_messages = pending_developer_messages.drain(..);
                        while let Some((text, message_result)) = developer_messages.next() {
                            let committed = commit_developer_message(
                                &mut model,
                                &self.execution,
                                Arc::clone(&self.spawner.lineage_id),
                                thread_model,
                                text,
                                self.workspace.as_deref(),
                            )
                            .await;
                            let message_outcome = match committed {
                                Ok(checkpoint) => {
                                    if let Some(checkpoint) = checkpoint {
                                        latest_fork_checkpoint = Some(checkpoint);
                                    }
                                    agent_session_context(
                                        latest_fork_checkpoint.as_deref(),
                                        self.workspace.as_deref(),
                                        &self.spawner.context_source,
                                    )
                                }
                                Err(error) => {
                                    model = model_from_checkpoint(
                                        &self.events,
                                        &self.transport_stats,
                                        &self.tools,
                                        &self.spawner,
                                        &prompt_cache,
                                        latest_fork_checkpoint.as_deref(),
                                    );
                                    Err(error)
                                }
                            };
                            let message_reopens = outcome_requires_reopen(&message_outcome);
                            drop(message_result.send(message_outcome));
                            if message_reopens {
                                reopen_after_compaction = true;
                                for (_, pending_result) in developer_messages {
                                    drop(pending_result.send(Err(NanocodexError::AgentStopped)));
                                }
                                break;
                            }
                        }
                    } else if let Err(error) = &outcome {
                        let detail = error.to_string();
                        for (_, message_result) in pending_developer_messages.drain(..) {
                            drop(message_result.send(Err(
                                NanocodexError::InvalidExecutionPolicy(format!(
                                    "developer message was not committed because the preceding compaction failed: {detail}"
                                )),
                            )));
                        }
                    }
                    drop(result.send(outcome));
                    if commands_open && reopen_after_compaction {
                        begin_shutdown(
                            &mut self.commands,
                            &mut queued_turns,
                            default_thinking,
                            default_fast_mode,
                        )
                        .await;
                        commands_open = false;
                    }
                    continue;
                }
                handle_idle_command(
                    command,
                    latest_fork_checkpoint.as_ref(),
                    &self.spawner,
                    TurnDefaults {
                        model: thread_model,
                        thinking: default_thinking,
                        fast_mode: default_fast_mode,
                    },
                    session_id.as_str(),
                    self.workspace.clone(),
                );
                continue;
            };
            let execution_operation = execution_operation.map(ExecutionOperation::into_id);
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
                    model: thread_model,
                    mode: self.spawner.config.reasoning_mode,
                    effort: thinking,
                },
                turn_index,
                prompt.text_bytes(),
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
            let execution_turn =
                self.execution
                    .start_turn(&prompt, thinking, execution_operation.clone());
            if let Err(error) = execution_turn.begin().await {
                let reopen = matches!(
                    error.execution_policy_disposition(),
                    Some(crate::ExecutionPolicyDisposition::Reopen)
                );
                if let Some(operation_id) = &execution_operation {
                    self.execution.release_claim(operation_id).await;
                }
                model.set_events(events);
                let emitted = model.emit_failed_before_start(
                    &prompt,
                    self.workspace.as_deref(),
                    thinking,
                    fast_mode,
                    &error,
                );
                model.set_events(self.events.clone());
                drop(result.send(emitted.and(Err(error))));
                if reopen {
                    begin_shutdown(
                        &mut self.commands,
                        &mut queued_turns,
                        default_thinking,
                        default_fast_mode,
                    )
                    .await;
                    commands_open = false;
                }
                continue;
            }
            let execution_base_checkpoint = execution_operation
                .as_ref()
                .map(|_| latest_fork_checkpoint.clone());
            let execution_steps = execution_turn.steps();
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
                        execution_steps,
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
                                thread_model,
                                snapshot,
                            )));
                        }
                    }
                    outcome = &mut execution => break outcome,
                    command = self.commands.recv() => {
                        let mut reopen = false;
                        let command = match command {
                            Some(command) => accept_execution_command(
                                &self.execution,
                                &self.spawner.config,
                                default_thinking,
                                command,
                                &mut reopen,
                            ).await,
                            None => None,
                        };
                        if reopen {
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
                            break execution.as_mut().await;
                        }
                        match command {
                            Some(Command::Prompt {
                                key,
                                prompt,
                                execution_operation,
                                accepted: _,
                                thinking: _,
                                fast_mode: _,
                                parent,
                                events,
                                result,
                            }) => {
                                let execution_operation =
                                    execution_operation.map(ExecutionOperation::into_id);
                                queued_turns.push_back(QueuedTurn::Pending {
                                    key,
                                    prompt,
                                    execution_operation,
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
                            Some(Command::RoutePrompt {
                                prompt,
                                route_result,
                                ..
                            }) => {
                                let outcome = steers.try_send(prompt).map_or_else(
                                    |error| {
                                        Err(match error {
                                            mpsc::error::TrySendError::Full(_) => {
                                                NanocodexError::SteerQueueFull
                                            }
                                            mpsc::error::TrySendError::Closed(_) => {
                                                NanocodexError::TurnNotSteerable
                                            }
                                        })
                                    },
                                    |()| Ok(PromptRouteKind::Steered),
                                );
                                drop(route_result.send(outcome));
                            }
                            Some(Command::Cancel { key: target, result: cancellation }) => {
                                if target != key {
                                    let outcome = match queued_execution_operation(
                                        &queued_turns,
                                        target,
                                    ) {
                                        Some((operation_id, prompt)) => {
                                            let persisted = match operation_id {
                                                Some(operation_id) => {
                                                    self.execution
                                                        .cancel_operation(&operation_id, &prompt)
                                                        .await
                                                }
                                                None => Ok(()),
                                            };
                                            persisted.and_then(|()| {
                                                if cancel_queued_turn(
                                                    &mut queued_turns,
                                                    target,
                                                    true,
                                                ) {
                                                    Ok(())
                                                } else {
                                                    Err(NanocodexError::TurnNotCancellable)
                                                }
                                            })
                                        }
                                        None => Err(NanocodexError::TurnNotCancellable),
                                    };
                                    let reopen = outcome_requires_reopen(&outcome);
                                    drop(cancellation.send(outcome));
                                    if reopen {
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
                                        break execution.as_mut().await;
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
                            Some(command @ (Command::Fork { .. } | Command::Spawn { .. } | Command::SpawnBatch { .. })) => {
                                if let Some(snapshot) =
                                    fork_snapshot_rx.borrow_and_update().clone()
                                {
                                    latest_fork_checkpoint =
                                        Some(Arc::new(CommittedSession::new(
                                            Arc::clone(&self.spawner.lineage_id),
                                            thread_model,
                                            snapshot,
                                        )));
                                }
                                handle_idle_command(
                                    command,
                                    latest_fork_checkpoint.as_ref(),
                                    &self.spawner,
                                    TurnDefaults {
                                        model: thread_model,
                                        thinking: default_thinking,
                                        fast_mode: default_fast_mode,
                                    },
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
                            Some(Command::AppendDeveloperMessage { text, result }) => {
                                pending_developer_messages.push((text, result));
                            }
                            Some(Command::Context { result }) => {
                                let checkpoint = fork_snapshot_rx
                                    .borrow_and_update()
                                    .clone()
                                    .map(|checkpoint| {
                                        Arc::new(CommittedSession::new(
                                            Arc::clone(&self.spawner.lineage_id),
                                            thread_model,
                                            checkpoint,
                                        ))
                                    })
                                    .or_else(|| latest_fork_checkpoint.clone());
                                drop(result.send(agent_session_context(
                                    checkpoint.as_deref(),
                                    self.workspace.as_deref(),
                                    &self.spawner.context_source,
                                )));
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
            let mut terminal_failure_committed = false;
            let (outcome, was_cancelled, cancellation_persisted): (
                Result<TurnResult>,
                bool,
                Option<Result<()>>,
            ) = match completed {
                Ok(ModelTurnOutcome::Completed(completed)) => {
                    let CompletedModelTurn {
                        final_message,
                        usage,
                        response_completions,
                        checkpoint,
                    } = completed;
                    let checkpoint = Arc::new(CommittedSession::new(
                        Arc::clone(&self.spawner.lineage_id),
                        thread_model,
                        checkpoint,
                    ));
                    let execution_turn = execution_turn.completed(
                        final_message.clone(),
                        usage.clone(),
                        response_completions.clone(),
                    );
                    let committed = self
                        .execution
                        .persist(&checkpoint, execution_turn)
                        .instrument(turn_span.clone())
                        .await;
                    if committed.is_ok() {
                        terminal_failure_committed = true;
                        latest_fork_checkpoint = Some(Arc::clone(&checkpoint));
                    }
                    let persisted = match committed {
                        Ok(()) => model.emit_terminal("completed"),
                        Err(error) => model.emit_terminal("failed").and(Err(error)),
                    };
                    (
                        persisted.map(|()| TurnResult {
                            request_id: execution_operation.clone(),
                            final_message,
                            usage: Some(usage),
                            response_completions,
                            checkpoint: TurnCheckpoint::Live(checkpoint),
                        }),
                        false,
                        None,
                    )
                }
                Ok(ModelTurnOutcome::Cancelled(checkpoint)) => {
                    let checkpoint = Arc::new(CommittedSession::new(
                        Arc::clone(&self.spawner.lineage_id),
                        thread_model,
                        checkpoint,
                    ));
                    let execution_turn = execution_turn.interrupted();
                    let committed = self
                        .execution
                        .persist(&checkpoint, execution_turn)
                        .instrument(turn_span.clone())
                        .await;
                    if committed.is_ok() {
                        terminal_failure_committed = true;
                        latest_fork_checkpoint = Some(Arc::clone(&checkpoint));
                    }
                    let (persisted, cancellation_persisted) = match committed {
                        Ok(()) => (model.emit_terminal("cancelled"), Ok(())),
                        Err(error) => (
                            model
                                .emit_terminal("failed")
                                .and(Err(duplicate_policy_error(&error))),
                            Err(error),
                        ),
                    };
                    model.replace_client(ResponsesClient::new((self.spawner.service_factory)(
                        Arc::clone(&self.spawner.config),
                    )));
                    (
                        persisted.and(Err(NanocodexError::TurnCancelled)),
                        true,
                        Some(cancellation_persisted),
                    )
                }
                Ok(ModelTurnOutcome::Failed { error, checkpoint }) => {
                    let checkpoint = Arc::new(CommittedSession::new(
                        Arc::clone(&self.spawner.lineage_id),
                        thread_model,
                        checkpoint,
                    ));
                    match error.execution_policy_disposition() {
                        Some(crate::ExecutionPolicyDisposition::Reopen) => {
                            (model.emit_terminal("failed").and(Err(error)), false, None)
                        }
                        disposition => {
                            let retryable = error
                                .responses_error()
                                .is_some_and(|source| source.retry_advice().is_some())
                                || matches!(
                                    disposition,
                                    Some(crate::ExecutionPolicyDisposition::Retry)
                                );
                            let execution_turn =
                                execution_turn.failed(error.to_string(), retryable);
                            let committed = self
                                .execution
                                .persist(&checkpoint, execution_turn)
                                .instrument(turn_span.clone())
                                .await;
                            if committed.is_ok() && !retryable {
                                latest_fork_checkpoint = Some(checkpoint);
                                terminal_failure_committed = true;
                            }
                            let persisted = match committed {
                                Ok(()) => model.emit_terminal("failed"),
                                Err(error) => model.emit_terminal("failed").and(Err(error)),
                            };
                            (persisted.and(Err(error)), false, None)
                        }
                    }
                }
                Err(error) => match error.execution_policy_disposition() {
                    Some(crate::ExecutionPolicyDisposition::Reopen) => {
                        (model.emit_terminal("failed").and(Err(error)), false, None)
                    }
                    _ => {
                        let persisted = self
                            .execution
                            .fail_without_checkpoint(execution_turn)
                            .instrument(turn_span.clone())
                            .await;
                        let persisted = match persisted {
                            Ok(()) => model.emit_terminal("failed"),
                            Err(error) => model.emit_terminal("failed").and(Err(error)),
                        };
                        (persisted.and(Err(error)), false, None)
                    }
                },
            };
            if !commands_open
                && !terminal_failure_committed
                && let Err(error) = &outcome
                && !matches!(error, NanocodexError::TurnCancelled)
            {
                shutdown_failures.push(error.to_string());
            }
            if outcome.is_err()
                && !terminal_failure_committed
                && let Some(base_checkpoint) = execution_base_checkpoint
            {
                latest_fork_checkpoint = base_checkpoint.clone();
                model = model_from_checkpoint(
                    &self.events,
                    &self.transport_stats,
                    &self.tools,
                    &self.spawner,
                    &prompt_cache,
                    base_checkpoint.as_deref(),
                );
            }
            model.set_events(self.events.clone());
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
            let mut reopen_after_turn = outcome_requires_reopen(&outcome);
            if commands_open && reopen_after_turn {
                begin_shutdown(
                    &mut self.commands,
                    &mut queued_turns,
                    default_thinking,
                    default_fast_mode,
                )
                .await;
                commands_open = false;
            }
            drop(result.send(outcome));
            if reopen_after_turn {
                for (_, message_result) in pending_developer_messages.drain(..) {
                    drop(message_result.send(Err(NanocodexError::AgentStopped)));
                }
            } else if terminal_failure_committed || execution_operation.is_none() {
                let mut developer_messages = pending_developer_messages.drain(..);
                while let Some((text, message_result)) = developer_messages.next() {
                    let committed = commit_developer_message(
                        &mut model,
                        &self.execution,
                        Arc::clone(&self.spawner.lineage_id),
                        thread_model,
                        text,
                        self.workspace.as_deref(),
                    )
                    .await;
                    let message_outcome = match committed {
                        Ok(checkpoint) => {
                            if let Some(checkpoint) = checkpoint {
                                latest_fork_checkpoint = Some(checkpoint);
                            }
                            agent_session_context(
                                latest_fork_checkpoint.as_deref(),
                                self.workspace.as_deref(),
                                &self.spawner.context_source,
                            )
                        }
                        Err(error) => {
                            model = model_from_checkpoint(
                                &self.events,
                                &self.transport_stats,
                                &self.tools,
                                &self.spawner,
                                &prompt_cache,
                                latest_fork_checkpoint.as_deref(),
                            );
                            Err(error)
                        }
                    };
                    let message_reopens = outcome_requires_reopen(&message_outcome);
                    drop(message_result.send(message_outcome));
                    if message_reopens {
                        reopen_after_turn = true;
                        for (_, pending_result) in developer_messages {
                            drop(pending_result.send(Err(NanocodexError::AgentStopped)));
                        }
                        break;
                    }
                }
            }
            if commands_open && reopen_after_turn {
                begin_shutdown(
                    &mut self.commands,
                    &mut queued_turns,
                    default_thinking,
                    default_fast_mode,
                )
                .await;
                commands_open = false;
            }
            if let Some(cancel_result) = cancel_result {
                let outcome = cancellation_persisted.unwrap_or_else(|| {
                    if was_cancelled {
                        Ok(())
                    } else {
                        Err(NanocodexError::TurnNotCancellable)
                    }
                });
                drop(cancel_result.send(outcome));
            }
        }
    }
}

fn duplicate_policy_error(error: &NanocodexError) -> NanocodexError {
    match error {
        NanocodexError::ExecutionPolicy {
            layer,
            disposition,
            source,
        } => NanocodexError::ExecutionPolicy {
            layer,
            disposition: *disposition,
            source: Arc::clone(source),
        },
        error => NanocodexError::InvalidExecutionPolicy(error.to_string()),
    }
}

fn outcome_requires_reopen<T>(outcome: &Result<T>) -> bool {
    outcome.as_ref().is_err_and(|error| {
        matches!(
            error.execution_policy_disposition(),
            Some(crate::ExecutionPolicyDisposition::Reopen)
        )
    })
}

async fn commit_developer_message<S>(
    model: &mut ModelRun<S>,
    execution: &Execution,
    lineage_id: Arc<str>,
    model_name: Model,
    text: String,
    workspace: Option<&str>,
) -> Result<Option<Arc<CommittedSession>>>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<ResponseError>,
    S::Future: AgentSend,
{
    let snapshot = model.append_developer_message(text, workspace)?;
    let checkpoint = Arc::new(CommittedSession::new(lineage_id, model_name, snapshot));
    execution.commit_checkpoint(&checkpoint).await?;
    Ok(Some(checkpoint))
}

fn model_from_checkpoint<S>(
    events: &EventSink,
    transport_stats: &Arc<TransportStats>,
    tools: &Tools,
    spawner: &BranchSpawner<S>,
    prompt_cache: &ModelPromptCache,
    checkpoint: Option<&CommittedSession>,
) -> ModelRun<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<ResponseError>,
    S::Future: AgentSend,
{
    let client = ResponsesClient::new((spawner.service_factory)(Arc::clone(&spawner.config)));
    if let Some(checkpoint) = checkpoint {
        let prepared = prepare_checkpoint(
            checkpoint.model().clone(),
            &spawner.config,
            tools,
            spawner.context_source.clone(),
        );
        ModelRun::from_checkpoint(
            events.clone(),
            Arc::clone(&spawner.config),
            client,
            Arc::clone(transport_stats),
            tools.clone(),
            prompt_cache.clone(),
            prepared,
        )
    } else {
        ModelRun::new(
            events.clone(),
            Arc::clone(&spawner.config),
            client,
            Arc::clone(transport_stats),
            tools.clone(),
            prompt_cache.clone(),
            spawner.context_source.clone(),
        )
    }
}

async fn accept_execution_command(
    execution: &Execution,
    config: &ModelConfig,
    default_thinking: Thinking,
    command: Command,
    reopen: &mut bool,
) -> Option<Command> {
    let Command::Prompt {
        key,
        prompt,
        execution_operation: Some(operation),
        accepted: Some(accepted),
        thinking,
        fast_mode,
        parent,
        events,
        result,
    } = command
    else {
        return Some(command);
    };
    let replay_thinking = thinking.unwrap_or(default_thinking);
    let admission = match operation {
        ExecutionOperation::Caller(operation_id) => execution
            .admit(&operation_id, &prompt)
            .await
            .map(|admission| (operation_id, admission)),
        ExecutionOperation::Automatic(candidate_operation_id) => {
            execution
                .admit_automatic(candidate_operation_id, &prompt)
                .await
        }
        ExecutionOperation::Admitted(operation_id) => {
            Ok((operation_id, AdmittedExecution::Execute))
        }
    };
    match admission {
        Ok((operation_id, AdmittedExecution::Execute)) => {
            if accepted.send(Ok(operation_id.clone())).is_err() {
                execution.release_claim(&operation_id).await;
                return None;
            }
            Some(Command::Prompt {
                key,
                prompt,
                execution_operation: Some(ExecutionOperation::Admitted(operation_id)),
                accepted: None,
                thinking,
                fast_mode,
                parent,
                events,
                result,
            })
        }
        Ok((operation_id, AdmittedExecution::Completed { output, snapshot })) => {
            if accepted.send(Ok(operation_id.clone())).is_err() {
                return None;
            }
            if let Err(error) =
                emit_replayed_terminal(&events, config, replay_thinking, "completed", &output.usage)
            {
                drop(result.send(Err(error)));
                return None;
            }
            drop(result.send(Ok(TurnResult {
                request_id: Some(operation_id),
                final_message: output.final_message,
                usage: Some(output.usage),
                response_completions: output.response_completions,
                checkpoint: TurnCheckpoint::Replayed(snapshot),
            })));
            None
        }
        Ok((operation_id, AdmittedExecution::Failed { error })) => {
            if accepted.send(Ok(operation_id)).is_err() {
                return None;
            }
            if let Err(event_error) = emit_replayed_terminal(
                &events,
                config,
                replay_thinking,
                "failed",
                &TurnUsage::default(),
            ) {
                drop(result.send(Err(event_error)));
                return None;
            }
            drop(result.send(Err(NanocodexError::ReplayedExecutionFailed(error))));
            None
        }
        Ok((operation_id, AdmittedExecution::Cancelled)) => {
            if accepted.send(Ok(operation_id)).is_err() {
                return None;
            }
            if let Err(error) = emit_replayed_terminal(
                &events,
                config,
                replay_thinking,
                "cancelled",
                &TurnUsage::default(),
            ) {
                drop(result.send(Err(error)));
                return None;
            }
            drop(result.send(Err(NanocodexError::TurnCancelled)));
            None
        }
        Err(error) => {
            *reopen = matches!(
                error.execution_policy_disposition(),
                Some(crate::ExecutionPolicyDisposition::Reopen)
            );
            drop(accepted.send(Err(error)));
            None
        }
    }
}

async fn accept_idle_route(
    execution: &Execution,
    config: &ModelConfig,
    default_thinking: Thinking,
    command: Command,
    reopen: &mut bool,
) -> Option<Command> {
    let Command::RoutePrompt {
        key,
        prompt,
        parent,
        events,
        turn_result,
        route_result,
    } = command
    else {
        return Some(command);
    };
    if !execution.identifies_prompts() {
        drop(route_result.send(Ok(PromptRouteKind::Started { request_id: None })));
        return Some(Command::Prompt {
            key,
            prompt,
            execution_operation: None,
            accepted: None,
            thinking: None,
            fast_mode: None,
            parent,
            events,
            result: turn_result,
        });
    }

    let admission = execution
        .admit_automatic(SessionId::new().to_string(), &prompt)
        .await;
    match admission {
        Ok((operation_id, AdmittedExecution::Execute)) => {
            if route_result
                .send(Ok(PromptRouteKind::Started {
                    request_id: Some(operation_id.clone()),
                }))
                .is_err()
            {
                execution.release_claim(&operation_id).await;
                return None;
            }
            Some(Command::Prompt {
                key,
                prompt,
                execution_operation: Some(ExecutionOperation::Admitted(operation_id)),
                accepted: None,
                thinking: None,
                fast_mode: None,
                parent,
                events,
                result: turn_result,
            })
        }
        Ok((operation_id, AdmittedExecution::Completed { output, snapshot })) => {
            if route_result
                .send(Ok(PromptRouteKind::Started {
                    request_id: Some(operation_id.clone()),
                }))
                .is_err()
            {
                return None;
            }
            if let Err(error) = emit_replayed_terminal(
                &events,
                config,
                default_thinking,
                "completed",
                &output.usage,
            ) {
                drop(turn_result.send(Err(error)));
                return None;
            }
            drop(turn_result.send(Ok(TurnResult {
                request_id: Some(operation_id),
                final_message: output.final_message,
                usage: Some(output.usage),
                response_completions: output.response_completions,
                checkpoint: TurnCheckpoint::Replayed(snapshot),
            })));
            None
        }
        Ok((operation_id, AdmittedExecution::Failed { error })) => {
            if route_result
                .send(Ok(PromptRouteKind::Started {
                    request_id: Some(operation_id),
                }))
                .is_err()
            {
                return None;
            }
            if let Err(event_error) = emit_replayed_terminal(
                &events,
                config,
                default_thinking,
                "failed",
                &TurnUsage::default(),
            ) {
                drop(turn_result.send(Err(event_error)));
                return None;
            }
            drop(turn_result.send(Err(NanocodexError::ReplayedExecutionFailed(error))));
            None
        }
        Ok((operation_id, AdmittedExecution::Cancelled)) => {
            if route_result
                .send(Ok(PromptRouteKind::Started {
                    request_id: Some(operation_id),
                }))
                .is_err()
            {
                return None;
            }
            if let Err(error) = emit_replayed_terminal(
                &events,
                config,
                default_thinking,
                "cancelled",
                &TurnUsage::default(),
            ) {
                drop(turn_result.send(Err(error)));
                return None;
            }
            drop(turn_result.send(Err(NanocodexError::TurnCancelled)));
            None
        }
        Err(error) => {
            *reopen = matches!(
                error.execution_policy_disposition(),
                Some(crate::ExecutionPolicyDisposition::Reopen)
            );
            drop(route_result.send(Err(error)));
            None
        }
    }
}

pub(super) fn agent_session_context(
    checkpoint: Option<&CommittedSession>,
    configured_workspace: Option<&str>,
    context_source: &ContextSource,
) -> Result<AgentSessionContext> {
    let workspace = checkpoint
        .map(|checkpoint| checkpoint.model().workspace().to_owned())
        .or_else(|| configured_workspace.map(str::to_owned))
        .map_or_else(|| context_source.resolve_workspace(None), Ok)?;
    Ok(AgentSessionContext::new(checkpoint, workspace))
}
