use super::*;

type SharedShutdownResult = std::result::Result<(), Arc<NanocodexError>>;

#[derive(Default)]
enum ShutdownPhase {
    #[default]
    Running,
    Requested,
    Complete(SharedShutdownResult),
}

#[derive(Default)]
struct ShutdownState {
    phase: ShutdownPhase,
    waiters: Vec<oneshot::Sender<SharedShutdownResult>>,
}

#[derive(Clone, Default)]
pub(in crate::agent) struct DriverShutdown {
    state: Arc<std::sync::Mutex<ShutdownState>>,
    execution_policy_owned: Arc<std::sync::atomic::AtomicBool>,
}

impl DriverShutdown {
    pub(in crate::agent) fn set_execution_policy_owned(&self, owned: bool) {
        self.execution_policy_owned
            .store(owned, std::sync::atomic::Ordering::Release);
    }

    pub(in crate::agent) fn request(&self) -> (bool, oneshot::Receiver<SharedShutdownResult>) {
        let (result, receiver) = oneshot::channel();
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        match &state.phase {
            ShutdownPhase::Running => {
                state.phase = ShutdownPhase::Requested;
                state.waiters.push(result);
                (true, receiver)
            }
            ShutdownPhase::Requested => {
                state.waiters.push(result);
                (false, receiver)
            }
            ShutdownPhase::Complete(outcome) => {
                drop(result.send(outcome.clone()));
                (false, receiver)
            }
        }
    }

    pub(in crate::agent) fn complete(&self, outcome: Result<()>) {
        let outcome = outcome.map_err(Arc::new);
        let waiters = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.phase = ShutdownPhase::Complete(outcome.clone());
            std::mem::take(&mut state.waiters)
        };
        for waiter in waiters {
            drop(waiter.send(outcome.clone()));
        }
    }

    pub(in crate::agent) async fn stopped_error(&self) -> NanocodexError {
        let receiver = {
            let (result, receiver) = oneshot::channel();
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            match &state.phase {
                ShutdownPhase::Complete(outcome) => {
                    drop(result.send(outcome.clone()));
                }
                ShutdownPhase::Running | ShutdownPhase::Requested => {
                    state.waiters.push(result);
                }
            }
            receiver
        };
        let error = match receiver.await {
            Ok(Err(error))
                if matches!(
                    error.execution_policy_disposition(),
                    Some(crate::ExecutionPolicyDisposition::Reopen)
                ) =>
            {
                NanocodexError::Shutdown(error)
            }
            Ok(Ok(()) | Err(_)) | Err(_) => NanocodexError::AgentStopped,
        };
        if matches!(error, NanocodexError::AgentStopped)
            && self
                .execution_policy_owned
                .load(std::sync::atomic::Ordering::Acquire)
        {
            NanocodexError::ExecutionPolicyOwnerStopped
        } else {
            error
        }
    }
}

pub(super) fn queued_execution_operation(
    queued_turns: &VecDeque<QueuedTurn>,
    target: TurnKey,
) -> Option<(Option<String>, Prompt)> {
    queued_turns.iter().find_map(|queued| match queued {
        QueuedTurn::Pending {
            key,
            execution_operation,
            prompt,
            ..
        } if *key == target => Some((execution_operation.clone(), prompt.clone())),
        _ => None,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn queued_prompt(
    key: TurnKey,
    prompt: Prompt,
    execution_operation: Option<String>,
    cancel_on_admission: bool,
    thinking: Thinking,
    fast_mode: bool,
    parent: Option<tracing::Span>,
    events: EventSink,
    result: oneshot::Sender<Result<TurnResult>>,
) -> QueuedTurn {
    if cancel_on_admission {
        QueuedTurn::Cancelled {
            prompt,
            execution_operation,
            cancellation_committed: false,
            thinking,
            fast_mode,
            parent,
            events,
            result,
        }
    } else {
        QueuedTurn::Pending {
            key,
            prompt,
            execution_operation,
            thinking,
            fast_mode,
            parent,
            events,
            result,
        }
    }
}

pub(super) fn cancel_queued_turn(
    queued_turns: &mut VecDeque<QueuedTurn>,
    target: TurnKey,
    cancellation_committed: bool,
) -> bool {
    let Some(position) = queued_turns
        .iter()
        .position(|queued| matches!(queued, QueuedTurn::Pending { key, .. } if *key == target))
    else {
        return false;
    };
    let Some(queued) = queued_turns.remove(position) else {
        return false;
    };
    let QueuedTurn::Pending {
        prompt,
        execution_operation,
        thinking,
        fast_mode,
        parent,
        events,
        result,
        ..
    } = queued
    else {
        return false;
    };
    queued_turns.insert(
        position,
        QueuedTurn::Cancelled {
            prompt,
            execution_operation,
            cancellation_committed,
            thinking,
            fast_mode,
            parent,
            events,
            result,
        },
    );
    true
}

pub(super) fn mark_all_queued_turns_cancelled(queued_turns: &mut VecDeque<QueuedTurn>) {
    let accepted = std::mem::take(queued_turns);
    queued_turns.extend(accepted.into_iter().map(|queued| match queued {
        QueuedTurn::Pending {
            prompt,
            execution_operation,
            thinking,
            fast_mode,
            parent,
            events,
            result,
            ..
        } => QueuedTurn::Cancelled {
            prompt,
            execution_operation,
            cancellation_committed: false,
            thinking,
            fast_mode,
            parent,
            events,
            result,
        },
        queued @ QueuedTurn::Cancelled { .. } => queued,
    }));
}

pub(super) async fn begin_shutdown(
    commands: &mut mpsc::Receiver<Command>,
    queued_turns: &mut VecDeque<QueuedTurn>,
    default_thinking: Thinking,
    default_fast_mode: bool,
) {
    commands.close();
    while let Some(command) = commands.recv().await {
        match command {
            Command::Prompt {
                accepted: Some(accepted),
                result,
                ..
            } => {
                drop(accepted.send(Err(NanocodexError::AgentStopped)));
                drop(result);
            }
            Command::Prompt {
                key,
                prompt,
                execution_operation,
                accepted: None,
                cancel_on_admission,
                thinking,
                fast_mode,
                parent,
                events,
                result,
            } => {
                let execution_operation = execution_operation.map(ExecutionOperation::into_id);
                queued_turns.push_back(queued_prompt(
                    key,
                    prompt,
                    execution_operation,
                    cancel_on_admission,
                    thinking.unwrap_or(default_thinking),
                    fast_mode.unwrap_or(default_fast_mode),
                    parent,
                    events,
                    result,
                ));
            }
            Command::RoutePrompt {
                route_result,
                turn_result,
                ..
            } => {
                drop(route_result.send(Err(NanocodexError::AgentStopped)));
                drop(turn_result);
            }
            Command::Fork { result, .. } => {
                drop(result.send(Err(NanocodexError::AgentStopped)));
            }
            Command::Spawn { result, .. } => {
                drop(result.send(Err(NanocodexError::AgentStopped)));
            }
            Command::SpawnBatch { result, .. } => {
                drop(result.send(Err(NanocodexError::AgentStopped)));
            }
            Command::AppendDeveloperMessage { result, .. } => {
                drop(result.send(Err(NanocodexError::AgentStopped)));
            }
            Command::Context { result } => {
                drop(result.send(Err(NanocodexError::AgentStopped)));
            }
            Command::Steer { result, .. }
            | Command::Cancel { result, .. }
            | Command::SetModel { result, .. }
            | Command::SetThinking { result, .. }
            | Command::SetFastMode { result, .. }
            | Command::Compact { result, .. } => {
                drop(result.send(Err(NanocodexError::AgentStopped)));
            }
            Command::Shutdown => {}
        }
    }
    mark_all_queued_turns_cancelled(queued_turns);
}

#[derive(Clone, Copy)]
pub(super) struct TurnDefaults {
    pub(super) model: Model,
    pub(super) thinking: Thinking,
    pub(super) fast_mode: bool,
}

pub(super) fn handle_idle_command<S>(
    command: Command,
    latest: Option<&Arc<CommittedSession>>,
    spawner: &BranchSpawner<S>,
    defaults: TurnDefaults,
    session_id: &str,
    workspace: Option<Arc<str>>,
) where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<ResponseError> + AgentSend + 'static,
    S::Future: AgentSend,
{
    match command {
        Command::Fork { checkpoint, result } => {
            let checkpoint = checkpoint.or_else(|| latest.cloned());
            let outcome = checkpoint
                .ok_or(NanocodexError::ForkBeforeCompletedTurn)
                .and_then(|checkpoint| {
                    spawner.spawn_fork(
                        &checkpoint,
                        ForkTurns::All,
                        None,
                        session_id,
                        defaults,
                        spawner.host_context.as_ref().map(Arc::clone),
                    )
                });
            drop(result.send(outcome));
        }
        Command::Spawn {
            options,
            fork_turns,
            agent_name,
            host_context,
            result,
        } => {
            let model = options.model.unwrap_or(defaults.model);
            let thinking = options.thinking.unwrap_or(defaults.thinking);
            let child_defaults = TurnDefaults {
                model,
                thinking,
                fast_mode: defaults.fast_mode,
            };
            let outcome = validate_model_thinking(model, thinking).and_then(|()| {
                let host_context =
                    host_context.or_else(|| spawner.host_context.as_ref().map(Arc::clone));
                match fork_turns {
                    ForkTurns::None => spawner.spawn_clean(
                        agent_name,
                        workspace,
                        session_id,
                        child_defaults,
                        host_context,
                    ),
                    ForkTurns::All | ForkTurns::Last(_) => {
                        let checkpoint = latest.ok_or(NanocodexError::ForkBeforeCompletedTurn)?;
                        spawner.spawn_fork(
                            checkpoint,
                            fork_turns,
                            agent_name,
                            session_id,
                            child_defaults,
                            host_context,
                        )
                    }
                }
            });
            drop(result.send(outcome));
        }
        Command::SpawnBatch {
            count,
            observer,
            host_context,
            result,
        } => {
            let outcome = spawner.spawn_clean_many(
                workspace,
                session_id,
                defaults,
                count,
                observer.as_deref(),
                host_context,
            );
            drop(result.send(outcome));
        }
        Command::Steer { result, .. } => {
            drop(result.send(Err(NanocodexError::TurnNotSteerable)));
        }
        Command::RoutePrompt {
            route_result,
            turn_result,
            ..
        } => {
            drop(route_result.send(Err(NanocodexError::AgentStopped)));
            drop(turn_result);
        }
        Command::Cancel { result, .. } => {
            drop(result.send(Err(NanocodexError::TurnNotCancellable)));
        }
        Command::SetThinking { result, .. } | Command::SetFastMode { result, .. } => {
            drop(result.send(Ok(())));
        }
        Command::SetModel { result, .. } => {
            drop(result.send(Err(model_change_locked())));
        }
        Command::Shutdown => {}
        Command::Compact { result, .. } => {
            drop(result.send(Err(NanocodexError::AgentStopped)));
        }
        Command::AppendDeveloperMessage { result, .. } => {
            drop(result.send(Err(NanocodexError::AgentStopped)));
        }
        Command::Context { result } => {
            drop(result.send(super::agent_session_context(
                latest.map(AsRef::as_ref),
                workspace.as_deref(),
                &spawner.context_source,
            )));
        }
        Command::Prompt { .. } => {}
    }
}

pub(super) fn model_change_locked() -> NanocodexError {
    NanocodexError::InvalidRequest(
        "the model can only be changed before the first turn is accepted".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stopped_error_distinguishes_ordinary_and_policy_owned_agents() {
        let ordinary = DriverShutdown::default();
        ordinary.complete(Ok(()));
        assert!(matches!(
            ordinary.stopped_error().await,
            NanocodexError::AgentStopped
        ));

        let policy_owned = DriverShutdown::default();
        policy_owned.set_execution_policy_owned(true);
        policy_owned.complete(Ok(()));
        assert!(matches!(
            policy_owned.stopped_error().await,
            NanocodexError::ExecutionPolicyOwnerStopped
        ));
    }
}
