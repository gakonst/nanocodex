use super::*;

pub(super) async fn drive_late_wake<S>(
    commands: &mut mpsc::Receiver<Command>,
    execution: &Execution,
    spawner: &BranchSpawner<S>,
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
    let (operation_id, operation_input, admission) = execution
        .admit_late_continuation(&spawner.lineage_id, &wake_id)
        .await?;
    match admission {
        AdmittedExecution::Execute | AdmittedExecution::Resume => {}
        AdmittedExecution::Completed { .. } => {
            // The policy may know a newer committed snapshot than this
            // driver. Never replay a completed operation from stale state.
            return Err(NanocodexError::InvalidSessionSnapshot(
                "late continuation already completed; restore the committed policy snapshot".into(),
            ));
        }
        AdmittedExecution::Failed { .. } | AdmittedExecution::Cancelled => {
            return Err(NanocodexError::InvalidExecutionPolicy(
                "late continuation is terminal but its checkpoint still requests a wake".into(),
            ));
        }
    }
    let turn =
        execution.start_late_continuation(defaults.thinking, operation_id.clone(), operation_input);
    let retained = match turn.begin().await {
        Ok(retained) => retained,
        Err(error) => {
            if let Some(operation_id) = &operation_id {
                execution.release_claim(operation_id).await;
            }
            return Err(error);
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
            model_call_index: Arc::new(tokio::sync::Mutex::new(1)),
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
                    Some(command) => accept_execution_command(&execution, &spawner.config, defaults.thinking, command, &mut reopen).await,
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
                            accept_idle_route(&execution, &spawner.config, defaults.thinking, command, &mut reopen).await {
                            queued.push_back(queued_prompt(key, prompt, execution_operation,
                                cancel_on_admission, defaults.thinking, defaults.fast_mode, parent, events, result));
                        }
                        if reopen {
                            if let Some(cancel) = cancel_tx.take() { let _ = cancel.send(()); }
                            commands_open = false;
                            shutdown_requested = true;
                        }
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
                        handle_idle_command(command, checkpoint.as_ref(), &spawner, defaults, session_id, workspace.clone());
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
            execution
                .persist(&committed, turn.completed(done.final_message, done.usage))
                .await?;
            *checkpoint = Some(committed);
            model.emit_terminal("completed")?;
            Ok(())
        }
        Ok(ModelTurnOutcome::Cancelled(snapshot)) => {
            let committed = Arc::new(CommittedSession::new(
                Arc::clone(&spawner.lineage_id),
                defaults.model,
                snapshot,
            ));
            execution.persist(&committed, turn.interrupted()).await?;
            *checkpoint = Some(committed);
            model.emit_terminal("cancelled")?;
            Err(NanocodexError::TurnCancelled)
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
                .await?;
            model.emit_terminal("failed")?;
            Err(error)
        }
        Err(error) => {
            execution.fail_without_checkpoint(turn).await?;
            Err(error)
        }
    };
    if shutdown_requested {
        begin_shutdown(commands, queued, defaults.thinking, defaults.fast_mode).await;
    }
    // A cancelled continuation remains journalled for recovery; closing this
    // driver must not turn an orderly shutdown into a reported failure.
    match persisted {
        Ok(()) | Err(NanocodexError::TurnCancelled) if shutdown_requested => Ok(false),
        Ok(()) => Ok(commands_open),
        Err(error) => Err(error),
    }
}
