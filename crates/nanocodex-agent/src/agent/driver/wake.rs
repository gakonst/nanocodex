use super::*;

#[allow(
    clippy::too_many_arguments,
    reason = "driver-owned queues and durable checkpoint boundaries remain explicit"
)]
pub(super) async fn drive_late_wake<S>(
    commands: &mut mpsc::Receiver<Command>,
    execution: &Execution,
    spawner: &BranchSpawner<S>,
    events: &EventSink,
    transport_stats: &Arc<TransportStats>,
    tools: &Tools,
    prompt_cache: &ModelPromptCache,
    workspace: Option<Arc<str>>,
    model: &mut ModelRun<S>,
    checkpoint: &mut Option<Arc<CommittedSession>>,
    queued: &mut VecDeque<QueuedTurn>,
    pending_compact: &mut Option<(Option<tracing::Span>, oneshot::Sender<Result<()>>)>,
    pending_developer: &mut Vec<(String, oneshot::Sender<Result<AgentSessionContext>>)>,
    pending_outputs: &mut Vec<(
        String,
        nanocodex_oai_api::responses::FunctionOutputBody,
        String,
        oneshot::Sender<Result<LateFunctionOutputReceipt>>,
    )>,
    defaults: TurnDefaults,
    session_id: &str,
    logical_turn: u64,
) -> Result<bool>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<ResponseError> + AgentSend + 'static,
    S::Future: AgentSend,
{
    let wake_id = model
        .current_checkpoint()
        .and_then(|snapshot| snapshot.late_wake_id().map(str::to_owned))
        .ok_or_else(|| {
            NanocodexError::InvalidSessionSnapshot("late wake has no pending marker".into())
        })?;
    let journal_id = format!("late-continuation:{}:{wake_id}", spawner.lineage_id);
    let jobs = model
        .current_checkpoint()
        .map_or_else(Vec::new, |checkpoint| checkpoint.late_wake_jobs().to_vec());
    let (operation_id, operation_input, admission) = execution
        .recover_failure(
            Some(&journal_id),
            execution
                .admit_late_continuation(&spawner.lineage_id, &wake_id, &jobs)
                .await,
        )
        .await?;
    match admission {
        AdmittedExecution::Execute | AdmittedExecution::Resume => {}
        AdmittedExecution::Completed { snapshot, .. } => {
            // The journal is authoritative after a crash between persistence and
            // driver checkpoint publication. Rebuild both the exposed boundary
            // and the live run before servicing queued commands or another wake.
            let replayed = snapshot.clone().into_replayed_checkpoint(
                &spawner.lineage_id,
                defaults.model,
                workspace.as_deref(),
            )?;
            if replayed.late_wake_id() == Some(wake_id.as_str()) {
                return Err(NanocodexError::InvalidSessionSnapshot(
                    "completed late continuation still requests its own wake".into(),
                ));
            }
            let committed = Arc::new(
                CommittedSession::new(Arc::clone(&spawner.lineage_id), defaults.model, replayed)
                    .with_retained_snapshot(Some(snapshot)),
            );
            *model = model_from_checkpoint(
                events,
                transport_stats,
                tools,
                spawner,
                prompt_cache,
                Some(&committed),
            );
            *checkpoint = Some(committed);
            return Ok(true);
        }
        AdmittedExecution::Failed { error } => {
            return Err(NanocodexError::ReplayedExecutionFailed(error));
        }
        AdmittedExecution::Cancelled => return Err(NanocodexError::TurnCancelled),
    }
    let turn =
        execution.start_late_continuation(defaults.thinking, operation_id.clone(), operation_input);
    let retained = match turn.begin().await {
        Ok(retained) => retained,
        Err(error) => {
            if let Some(operation_id) = &operation_id {
                execution.release_claim(operation_id).await;
            }
            return execution
                .recover_failure(Some(&journal_id), Err(error))
                .await;
        }
    };
    let steer_rx = Arc::new(tokio::sync::Mutex::new(VecDeque::new()));
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let mut cancel_tx = Some(cancel_tx);
    let (fork_tx, mut fork_rx) = watch::channel(None);
    let mut fork_open = true;
    let mut commands_open = true;
    let mut shutdown_requested = false;
    let steps = turn.steps();
    let mut run = Box::pin(model.continue_late(
        workspace.clone(),
        defaults.thinking,
        defaults.fast_mode,
        logical_turn,
        TurnSteering {
            receiver: steer_rx,
            retained,
            model_call_index: Arc::new(tokio::sync::Mutex::new(0)),
            boundary_outputs: Arc::new(tokio::sync::Mutex::new(VecDeque::new())),
            retained_boundary_outputs: Vec::new(),
        },
        cancel_rx,
        fork_tx,
        steps,
    ));
    let outcome = loop {
        tokio::select! {
            biased;
            changed = fork_rx.changed(), if fork_open => {
                if changed.is_err() { fork_open = false; }
                // A partial model snapshot is not a committed boundary.
            }
            outcome = &mut run => break outcome,
            command = commands.recv(), if commands_open => {
                let mut reopen = false;
                let command = match command {
                    Some(command) => accept_execution_command(execution, &spawner.config, defaults.thinking, command, &mut reopen).await,
                    None => None,
                };
                if reopen {
                    if let Some(cancel) = cancel_tx.take() { let _ = cancel.send(()); }
                    commands_open = false;
                    shutdown_requested = true;
                    continue;
                }
                match command {
                    Some(Command::Prompt { key, prompt, execution_operation, cancel_on_admission, parent, events, result, .. }) => {
                        queued.push_back(queued_prompt(key, prompt, execution_operation,
                            cancel_on_admission, defaults.thinking, defaults.fast_mode, parent, events, result));
                    }
                    Some(command @ Command::RoutePrompt { .. }) => {
                        let mut reopen = false;
                        if let Some(Command::Prompt { key, prompt, execution_operation, cancel_on_admission, parent, events, result, .. }) =
                            accept_idle_route(execution, &spawner.config, defaults.thinking, command, &mut reopen).await {
                            queued.push_back(queued_prompt(key, prompt, execution_operation,
                                cancel_on_admission, defaults.thinking, defaults.fast_mode, parent, events, result));
                        }
                        if reopen {
                            if let Some(cancel) = cancel_tx.take() { let _ = cancel.send(()); }
                            commands_open = false;
                            shutdown_requested = true;
                        }
                    }
                    Some(Command::SubmitLateFunctionOutputs { result, .. }) => {
                        drop(result.send(Err(NanocodexError::InvalidRequest(
                            "batch late outputs require an idle driver".into(),
                        ))));
                    }
                    Some(Command::SubmitLateFunctionOutput { call_id, output, operation_id, result }) => {
                        pending_outputs.push((call_id, output, operation_id, result));
                    }
                    Some(Command::AppendDeveloperMessage { text, result }) => pending_developer.push((text, result)),
                    Some(Command::Compact { parent, result }) => {
                        if let Some((_, previous)) = pending_compact.replace((parent, result)) {
                            drop(previous.send(Err(NanocodexError::TurnCancelled)));
                        }
                    }
                    Some(Command::Shutdown) | None => {
                        if let Some(cancel) = cancel_tx.take() { let _ = cancel.send(()); }
                        commands_open = false;
                        shutdown_requested = true;
                    }
                    Some(Command::Cancel { key, result }) => {
                        let outcome = if let Some((operation, prompt)) = queued_execution_operation(queued, key) {
                            let recovered = operation.as_ref().is_some_and(ExecutionOperation::is_recovered);
                            let persisted = match operation.as_ref() {
                                Some(operation) if !recovered => execution.cancel_operation(operation.id(), &prompt).await,
                                _ => Ok(()),
                            };
                            persisted.and_then(|()| if cancel_queued_turn(queued, key, !recovered) { Ok(()) } else { Err(NanocodexError::TurnNotCancellable) })
                        } else { Err(NanocodexError::TurnNotCancellable) };
                        let reopen = outcome_requires_reopen(&outcome);
                        drop(result.send(outcome));
                        if reopen {
                            if let Some(cancel) = cancel_tx.take() { let _ = cancel.send(()); }
                            commands_open = false;
                            shutdown_requested = true;
                        }
                    }
                    Some(command @ (Command::Snapshot { .. } | Command::ChildSnapshot { .. } | Command::Fork { .. } | Command::Spawn { .. } | Command::SpawnBatch { .. } | Command::Context { .. } | Command::Steer { .. } | Command::SteerWithId { .. } | Command::WithdrawSteer { .. } | Command::SetModel { .. })) => {
                        handle_idle_command(command, checkpoint.as_ref(), spawner, defaults, session_id, workspace.clone());
                    }
                    Some(Command::SetThinking { result, .. } | Command::SetFastMode { result, .. }) => {
                        drop(result.send(Err(model_change_locked())));
                    }
                }
            }
        }
    };
    drop(run);
    let persisted = match outcome {
        Ok(ModelTurnOutcome::Completed(done)) => {
            let committed = Arc::new(CommittedSession::new(
                Arc::clone(&spawner.lineage_id),
                defaults.model,
                done.checkpoint,
            ));
            let persisted = execution
                .persist(&committed, turn.completed(done.final_message, done.usage))
                .await;
            if persisted.is_ok() {
                *checkpoint = Some(committed);
            }
            persisted
        }
        Ok(ModelTurnOutcome::Cancelled(_snapshot)) => {
            // The wake owns the only delivery attempt for these late outputs.
            // A terminal cancellation would permanently fence its deterministic
            // journal ID even though no completed response acknowledged them.
            // Retain the previous staged checkpoint and the operation's steps;
            // reacquisition can resume this same wake without re-running tools.
            execution
                .fail_without_checkpoint(turn)
                .await
                .and(Err(NanocodexError::TurnCancelled))
        }
        Ok(ModelTurnOutcome::Failed {
            error,
            checkpoint: snapshot,
        }) => {
            let committed = Arc::new(CommittedSession::new(
                Arc::clone(&spawner.lineage_id),
                defaults.model,
                snapshot,
            ));
            execution
                .persist(&committed, turn.failed(error.to_string(), true))
                .await
                .and(Err(error))
        }
        Err(error) => {
            if !matches!(
                error.execution_policy_disposition(),
                Some(crate::ExecutionPolicyDisposition::Reopen)
            ) {
                execution
                    .fail_without_checkpoint(turn)
                    .await
                    .and(Err(error))
            } else {
                Err(error)
            }
        }
    };
    // A successful fail_attempt leaves this wake Pending. Its recovery policy
    // correctly classifies TurnCancelled as retryable, but an orderly owner
    // shutdown is not itself a failed shutdown just because the wake must be
    // reacquired by the next owner.
    let shutdown_retryable_wake =
        shutdown_requested && matches!(&persisted, Err(NanocodexError::TurnCancelled));
    let persisted = execution
        .recover_failure(operation_id.as_deref(), persisted)
        .await;
    // A retry/reopen is an unfinished attempt, not a terminal model run.
    let terminal = match &persisted {
        Ok(()) => Some("completed"),
        Err(NanocodexError::TurnCancelled) => Some("cancelled"),
        Err(error)
            if matches!(
                error.execution_policy_disposition(),
                Some(
                    crate::ExecutionPolicyDisposition::Retry
                        | crate::ExecutionPolicyDisposition::Reopen
                )
            ) =>
        {
            None
        }
        Err(_) => Some("failed"),
    };
    let persisted = match terminal {
        Some(status) => model.emit_terminal(status).and(persisted),
        None => persisted,
    };
    if shutdown_requested {
        begin_shutdown(commands, queued, defaults.thinking, defaults.fast_mode).await;
        mark_all_queued_turns_cancelled(queued);
        if let Some((_, result)) = pending_compact.take() {
            drop(result.send(Err(NanocodexError::AgentStopped)));
        }
        for (_, result) in pending_developer.drain(..) {
            drop(result.send(Err(NanocodexError::AgentStopped)));
        }
        for (_, _, _, result) in pending_outputs.drain(..) {
            drop(result.send(Err(NanocodexError::AgentStopped)));
        }
    }
    // A cancelled continuation remains journalled for recovery; closing this
    // driver must not turn an orderly shutdown into a reported failure.
    match persisted {
        Err(_) if shutdown_retryable_wake => Ok(false),
        Ok(()) | Err(NanocodexError::TurnCancelled) if shutdown_requested => Ok(false),
        Ok(()) => Ok(commands_open),
        Err(error) => Err(error),
    }
}
