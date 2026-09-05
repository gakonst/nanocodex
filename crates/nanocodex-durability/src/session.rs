use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
};

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{mpsc, oneshot};

use crate::{
    DurableState, EncodedPayload, Error, OperationStatus, OwnerId, OwnerToken, Result, StateStore,
    SteerState, StepStatus, StoreError, StoredState, Transition, shared_store::SharedStore,
    state::RetainedCheckpoint,
};

const COMMAND_CAPACITY: usize = 64;
const RELEASE_BURST_LIMIT: usize = 32;

/// Result of submitting one idempotent operation.
#[derive(Clone, Debug)]
pub enum Admission<C = EncodedPayload, O = EncodedPayload> {
    /// This call durably accepted new work.
    Accepted,
    /// The same input was already accepted and remains unfinished.
    Pending,
    /// The operation already completed.
    Completed {
        /// Checkpoint committed with the result.
        checkpoint: C,
        /// Previously completed result.
        output: O,
    },
    /// The operation already failed after committing a safe checkpoint.
    Failed {
        /// Checkpoint committed with the failure.
        checkpoint: C,
        /// Previously retained failure detail.
        error: String,
    },
    /// The operation was explicitly cancelled.
    Cancelled,
}

/// One automatically identified operation and its admission result.
#[derive(Clone, Debug)]
pub struct AutomaticAdmission<C = EncodedPayload, O = EncodedPayload> {
    operation_id: String,
    admission: Admission<C, O>,
}

impl<C, O> AutomaticAdmission<C, O> {
    /// Identity assigned to the admitted operation.
    #[must_use]
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Splits the assigned operation identity from its admission result.
    #[must_use]
    pub fn into_parts(self) -> (String, Admission<C, O>) {
        (self.operation_id, self.admission)
    }
}

/// Result of beginning a replayable step.
#[derive(Clone, Debug)]
pub enum BeginStep<O = EncodedPayload> {
    /// The caller owns this execution attempt and may perform the step.
    Execute,
    /// A prior attempt completed; use this stored output instead of executing.
    Replay(O),
}

enum StoredAdmission {
    Accepted,
    Pending,
    Completed {
        checkpoint: EncodedPayload,
        output: EncodedPayload,
    },
    Failed {
        checkpoint: EncodedPayload,
        error: String,
    },
    Cancelled,
}

impl StoredAdmission {
    fn into_encoded(self) -> Admission {
        match self {
            Self::Accepted => Admission::Accepted,
            Self::Pending => Admission::Pending,
            Self::Completed { checkpoint, output } => Admission::Completed { checkpoint, output },
            Self::Failed { checkpoint, error } => Admission::Failed { checkpoint, error },
            Self::Cancelled => Admission::Cancelled,
        }
    }

    fn decode<C, O>(self) -> Result<Admission<C, O>>
    where
        C: DeserializeOwned,
        O: DeserializeOwned,
    {
        match self {
            Self::Accepted => Ok(Admission::Accepted),
            Self::Pending => Ok(Admission::Pending),
            Self::Completed { checkpoint, output } => Ok(Admission::Completed {
                checkpoint: checkpoint.decode()?,
                output: output.decode()?,
            }),
            Self::Failed { checkpoint, error } => Ok(Admission::Failed {
                checkpoint: checkpoint.decode()?,
                error,
            }),
            Self::Cancelled => Ok(Admission::Cancelled),
        }
    }
}

enum StoredBeginStep {
    Execute,
    Replay(EncodedPayload),
}

#[derive(Clone)]
pub(crate) struct StoredSteer {
    pub(crate) index: u32,
    pub(crate) state: SteerState,
}

#[derive(Clone, Eq, PartialEq)]
enum Caller {
    Direct(OwnerId),
    Agent(u64),
}

struct AgentAcquisition {
    generation: u64,
    checkpoint: Option<EncodedPayload>,
}

enum Command {
    RecoverFailure {
        caller: Caller,
        operation_id: String,
        result: oneshot::Sender<Result<Option<OperationStatus>>>,
    },
    State {
        result: oneshot::Sender<DurableState>,
    },
    LatestCheckpoint {
        result: oneshot::Sender<Option<EncodedPayload>>,
    },
    AcquireAgent {
        result: oneshot::Sender<Result<AgentAcquisition>>,
    },
    Admit {
        caller: Caller,
        operation_id: String,
        input: EncodedPayload,
        acknowledged: oneshot::Receiver<()>,
        release_commands: mpsc::Sender<Self>,
        result: oneshot::Sender<Result<StoredAdmission>>,
    },
    AdmitAutomatic {
        caller: Caller,
        candidate_operation_id: String,
        input: EncodedPayload,
        acknowledged: oneshot::Receiver<()>,
        release_commands: mpsc::Sender<Self>,
        result: oneshot::Sender<Result<(String, StoredAdmission)>>,
    },
    Release {
        caller: Caller,
        operation_id: String,
        result: oneshot::Sender<Result<()>>,
    },
    BeginAttempt {
        caller: Caller,
        operation_id: String,
        result: oneshot::Sender<Result<()>>,
    },
    AcceptSteer {
        caller: Caller,
        operation_id: String,
        accepted_after_model_call_index: u32,
        input: EncodedPayload,
        result: oneshot::Sender<Result<u32>>,
    },
    RetainedSteers {
        caller: Caller,
        operation_id: String,
        result: oneshot::Sender<Result<Vec<StoredSteer>>>,
    },
    BindSteer {
        caller: Caller,
        operation_id: String,
        steer_index: u32,
        model_call_index: u32,
        result: oneshot::Sender<Result<()>>,
    },
    RetainedStepInput {
        caller: Caller,
        operation_id: String,
        step_id: String,
        kind: String,
        result: oneshot::Sender<Result<Option<EncodedPayload>>>,
    },
    BeginStep {
        caller: Caller,
        operation_id: String,
        step_id: String,
        kind: String,
        input: EncodedPayload,
        result: oneshot::Sender<Result<StoredBeginStep>>,
    },
    CompleteStep {
        caller: Caller,
        operation_id: String,
        step_id: String,
        output: EncodedPayload,
        result: oneshot::Sender<Result<()>>,
    },
    Complete {
        caller: Caller,
        operation_id: String,
        checkpoint: EncodedPayload,
        output: EncodedPayload,
        result: oneshot::Sender<Result<()>>,
    },
    Fail {
        caller: Caller,
        operation_id: String,
        checkpoint: EncodedPayload,
        error: String,
        result: oneshot::Sender<Result<()>>,
    },
    FailAttempt {
        caller: Caller,
        operation_id: String,
        result: oneshot::Sender<Result<()>>,
    },
    Cancel {
        caller: Caller,
        operation_id: String,
        checkpoint: Option<EncodedPayload>,
        result: oneshot::Sender<Result<()>>,
    },
    PruneReceipts {
        caller: Caller,
        result: oneshot::Sender<Result<()>>,
    },
    CommitCheckpoint {
        caller: Caller,
        checkpoint: EncodedPayload,
        result: oneshot::Sender<Result<()>>,
    },
}

struct Driver {
    store: Box<dyn StateStore>,
    state_id: Arc<str>,
    state: DurableState,
    terminal_receipt_limit: Option<usize>,
    owner: OwnerToken,
    next_agent_generation: u64,
    active_agent_generation: Option<u64>,
    claimed: HashMap<String, Caller>,
    running: HashSet<String>,
    poisoned: bool,
    commands: mpsc::Receiver<Command>,
    releases: mpsc::UnboundedReceiver<ReleaseSignal>,
}

const OWNER_ACTIVE: u8 = 0;
const OWNER_RELEASING: u8 = 1;
const OWNER_RELEASED: u8 = 2;

struct OwnerReleaseState {
    state: AtomicU8,
    completed: tokio::sync::watch::Sender<bool>,
}

impl OwnerReleaseState {
    fn new() -> Self {
        let (completed, _) = tokio::sync::watch::channel(false);
        Self {
            state: AtomicU8::new(OWNER_ACTIVE),
            completed,
        }
    }

    fn finish(&self) {
        self.state.store(OWNER_RELEASED, Ordering::Release);
        self.completed.send_replace(true);
    }
}

struct AgentRelease {
    generation: u64,
    state: Arc<OwnerReleaseState>,
}

enum ReleaseSignal {
    Agent(AgentRelease),
    Direct(OwnerId),
}

impl Driver {
    async fn run(mut self) {
        loop {
            for _ in 0..RELEASE_BURST_LIMIT {
                let Ok(release) = self.releases.try_recv() else {
                    break;
                };
                self.handle_release(release);
            }
            let command = tokio::select! {
                biased;
                command = self.commands.recv() => command,
                Some(release) = self.releases.recv() => {
                    self.handle_release(release);
                    continue;
                }
            };
            let Some(command) = command else {
                break;
            };
            // A release may arrive after the pre-select drain but before a
            // simultaneously ready command wins the biased selection. Drain a
            // bounded second burst so a just-dropped claimant is reclaimed
            // before an exact-ID admission, without letting sustained release
            // traffic starve the command itself.
            for _ in 0..RELEASE_BURST_LIMIT {
                let Ok(release) = self.releases.try_recv() else {
                    break;
                };
                self.handle_release(release);
            }
            match command {
                Command::RecoverFailure {
                    caller,
                    operation_id,
                    result,
                } => {
                    let outcome = self.authorize(&caller).and_then(|()| {
                        let operation = self.state.operation(&operation_id);
                        if operation.is_some_and(|operation| !operation.status.is_terminal())
                            && let Some((pending_id, _)) = self.state.first_pending_operation()
                            && pending_id != operation_id
                        {
                            return Err(Error::OperationBlocked {
                                operation_id,
                                pending_id: pending_id.to_owned(),
                            });
                        }
                        Ok(operation.map(|operation| operation.status.clone()))
                    });
                    drop(result.send(outcome));
                }
                Command::State { result } => drop(result.send(self.state.clone())),
                Command::LatestCheckpoint { result } => {
                    drop(result.send(self.state.latest_checkpoint().cloned()));
                }
                Command::AcquireAgent { result } => {
                    let outcome = self.acquire_agent().await;
                    let generation = outcome.as_ref().ok().map(|owner| owner.generation);
                    if result.send(outcome).is_err()
                        && let Some(generation) = generation
                        && self.active_agent_generation == Some(generation)
                    {
                        self.active_agent_generation = None;
                        self.claimed.clear();
                        self.running.clear();
                    }
                }
                Command::Admit {
                    caller,
                    operation_id,
                    input,
                    acknowledged,
                    release_commands,
                    result,
                } => {
                    let admitted_id = operation_id.clone();
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => self.admit(&caller, operation_id, input).await,
                        Err(error) => Err(error),
                    };
                    let claimed = matches!(
                        &outcome,
                        Ok(StoredAdmission::Accepted | StoredAdmission::Pending)
                    );
                    if result.send(outcome).is_err() {
                        self.release_claim_if_owned(&caller, &admitted_id);
                    } else if claimed {
                        spawn_claim_ack(release_commands, acknowledged, caller, admitted_id);
                    }
                }
                Command::AdmitAutomatic {
                    caller,
                    candidate_operation_id,
                    input,
                    acknowledged,
                    release_commands,
                    result,
                } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => {
                            self.admit_automatic(&caller, candidate_operation_id, input)
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    let claimed_id = match &outcome {
                        Ok((
                            operation_id,
                            StoredAdmission::Accepted | StoredAdmission::Pending,
                        )) => Some(operation_id.clone()),
                        _ => None,
                    };
                    if let Err(Ok((operation_id, _))) = result.send(outcome) {
                        self.release_claim_if_owned(&caller, &operation_id);
                    } else if let Some(operation_id) = claimed_id {
                        spawn_claim_ack(release_commands, acknowledged, caller, operation_id);
                    }
                }
                Command::Release {
                    caller,
                    operation_id,
                    result,
                } => {
                    let outcome = self
                        .authorize(&caller)
                        .and_then(|()| self.release_claim(&caller, &operation_id));
                    drop(result.send(outcome));
                }
                Command::BeginAttempt {
                    caller,
                    operation_id,
                    result,
                } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => self.begin_attempt(&caller, operation_id).await,
                        Err(error) => Err(error),
                    };
                    drop(result.send(outcome));
                }
                Command::AcceptSteer {
                    caller,
                    operation_id,
                    accepted_after_model_call_index,
                    input,
                    result,
                } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => {
                            self.accept_steer(
                                &caller,
                                operation_id,
                                accepted_after_model_call_index,
                                input,
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    };
                    drop(result.send(outcome));
                }
                Command::RetainedSteers {
                    caller,
                    operation_id,
                    result,
                } => {
                    let outcome = self
                        .authorize(&caller)
                        .and_then(|()| self.retained_steers(&caller, &operation_id));
                    drop(result.send(outcome));
                }
                Command::BindSteer {
                    caller,
                    operation_id,
                    steer_index,
                    model_call_index,
                    result,
                } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => {
                            self.bind_steer(&caller, operation_id, steer_index, model_call_index)
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    drop(result.send(outcome));
                }
                Command::RetainedStepInput {
                    caller,
                    operation_id,
                    step_id,
                    kind,
                    result,
                } => {
                    let outcome = self.authorize(&caller).and_then(|()| {
                        self.require_claimed(&caller, &operation_id)?;
                        self.require_running(&operation_id)?;
                        self.state.operation(&operation_id)
                            .and_then(|operation| operation.steps.get(&step_id))
                            .map(|step| {
                                if step.kind != kind {
                                    return Err(Error::InvalidState(format!(
                                        "step `{step_id}` in operation `{operation_id}` changed kind"
                                    )));
                                }
                                Ok(step.input.clone())
                            })
                            .transpose()
                    });
                    drop(result.send(outcome));
                }
                Command::BeginStep {
                    caller,
                    operation_id,
                    step_id,
                    kind,
                    input,
                    result,
                } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => {
                            self.begin_step(&caller, operation_id, step_id, kind, input)
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    drop(result.send(outcome));
                }
                Command::CompleteStep {
                    caller,
                    operation_id,
                    step_id,
                    output,
                    result,
                } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => {
                            self.complete_step(&caller, operation_id, step_id, output)
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    drop(result.send(outcome));
                }
                Command::Complete {
                    caller,
                    operation_id,
                    checkpoint,
                    output,
                    result,
                } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => {
                            self.complete(&caller, operation_id, checkpoint, output)
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    drop(result.send(outcome));
                }
                Command::Fail {
                    caller,
                    operation_id,
                    checkpoint,
                    error,
                    result,
                } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => self.fail(&caller, operation_id, checkpoint, error).await,
                        Err(error) => Err(error),
                    };
                    drop(result.send(outcome));
                }
                Command::FailAttempt {
                    caller,
                    operation_id,
                    result,
                } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => {
                            let outcome = match self.require_claimed(&caller, &operation_id) {
                                Ok(()) => self.require_running(&operation_id),
                                Err(error) => Err(error),
                            };
                            if finishing_attempt_releases_claim(&outcome) {
                                self.release_claim_if_owned(&caller, &operation_id);
                            }
                            outcome
                        }
                        Err(error) => Err(error),
                    };
                    drop(result.send(outcome));
                }
                Command::Cancel {
                    caller,
                    operation_id,
                    checkpoint,
                    result,
                } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => {
                            let outcome = match self.require_claimed(&caller, &operation_id) {
                                Ok(()) => {
                                    let admissible = match checkpoint.as_ref() {
                                        Some(_) => self.require_running(&operation_id),
                                        None if self.running.contains(&operation_id) => {
                                            Err(Error::CancellationCheckpointRequired {
                                                operation_id: operation_id.clone(),
                                            })
                                        }
                                        None => Ok(()),
                                    }
                                    .and_then(|()| {
                                        self.state
                                            .operation(&operation_id)
                                            .filter(|operation| !operation.status.is_terminal())
                                            .ok_or_else(|| Error::OperationTerminal {
                                                operation_id: operation_id.clone(),
                                            })
                                            .map(|_| ())
                                    });
                                    match admissible {
                                        Ok(()) => {
                                            self.apply_terminal(Transition::OperationCancelled {
                                                operation_id: operation_id.clone(),
                                                checkpoint,
                                            })
                                            .await
                                        }
                                        Err(error) => Err(error),
                                    }
                                }
                                Err(error) => Err(error),
                            };
                            if finishing_attempt_releases_claim(&outcome) {
                                self.release_claim_if_owned(&caller, &operation_id);
                            }
                            outcome
                        }
                        Err(error) => Err(error),
                    };
                    drop(result.send(outcome));
                }
                Command::PruneReceipts { caller, result } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => self.prune_retained(true).await,
                        Err(error) => Err(error),
                    };
                    drop(result.send(outcome));
                }
                Command::CommitCheckpoint {
                    caller,
                    checkpoint,
                    result,
                } => {
                    let outcome = match self.authorize(&caller) {
                        Ok(()) => match self.state.first_pending_operation() {
                            Some((pending_id, _)) => Err(Error::OperationBlocked {
                                operation_id: "standalone-checkpoint".to_owned(),
                                pending_id: pending_id.to_owned(),
                            }),
                            None => {
                                self.apply_terminal(Transition::CheckpointCommitted { checkpoint })
                                    .await
                            }
                        },
                        Err(error) => Err(error),
                    };
                    drop(result.send(outcome));
                }
            }
            if self.poisoned {
                break;
            }
        }
    }

    fn handle_release(&mut self, release: ReleaseSignal) {
        match release {
            ReleaseSignal::Agent(release) => {
                if self.active_agent_generation == Some(release.generation) {
                    self.active_agent_generation = None;
                    self.claimed.clear();
                    self.running.clear();
                }
                release.state.finish();
            }
            ReleaseSignal::Direct(caller_id) => {
                let released = self
                    .claimed
                    .iter()
                    .filter(|(_, caller)| *caller == &Caller::Direct(caller_id.clone()))
                    .map(|(operation_id, _)| operation_id.clone())
                    .collect::<Vec<_>>();
                self.claimed
                    .retain(|_, caller| caller != &Caller::Direct(caller_id.clone()));
                for operation_id in released {
                    self.running.remove(&operation_id);
                }
            }
        }
    }

    const fn authorize(&self, caller: &Caller) -> Result<()> {
        match (caller, self.active_agent_generation) {
            (Caller::Direct(_), None) => Ok(()),
            (Caller::Agent(generation), Some(active)) if *generation == active => Ok(()),
            (Caller::Direct(_), Some(_)) => Err(Error::ModelOwnerActive),
            (Caller::Agent(_), _) => Err(Error::ModelOwnerFenced),
        }
    }

    async fn acquire_agent(&mut self) -> Result<AgentAcquisition> {
        let acquired = match self.store.acquire(&self.state_id, OwnerId::new()).await {
            Ok(acquired) => acquired,
            Err(error @ StoreError::NotCommitted(_)) => return Err(error.into()),
            Err(error) => {
                self.poisoned = true;
                return Err(error.into());
            }
        };
        let state = match reduce(acquired.state) {
            Ok(state) => state,
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        };
        let generation = match self.next_agent_generation.checked_add(1) {
            Some(generation) => generation,
            None => {
                self.poisoned = true;
                return Err(Error::InvalidState(
                    "model-owner generation exceeded the u64 range".to_owned(),
                ));
            }
        };
        self.owner = acquired.owner;
        self.state = state;
        self.claimed.clear();
        self.running.clear();
        self.next_agent_generation = generation;
        self.active_agent_generation = Some(generation);
        Ok(AgentAcquisition {
            generation,
            checkpoint: self.state.latest_checkpoint().cloned(),
        })
    }

    async fn admit(
        &mut self,
        caller: &Caller,
        operation_id: String,
        input: EncodedPayload,
    ) -> Result<StoredAdmission> {
        if let Some(operation) = self.state.operation(&operation_id) {
            if operation.input != input {
                return Err(Error::OperationConflict { operation_id });
            }
            if self.claimed.contains_key(&operation_id) {
                return Err(Error::OperationActive { operation_id });
            }
            let operation = self.state.operation(&operation_id).ok_or_else(|| {
                Error::InvalidState(format!("operation `{operation_id}` disappeared"))
            })?;
            return match &operation.status {
                OperationStatus::Pending => {
                    self.claimed.insert(operation_id.clone(), caller.clone());
                    Ok(StoredAdmission::Pending)
                }
                OperationStatus::Completed { checkpoint, output } => {
                    Ok(StoredAdmission::Completed {
                        checkpoint: checkpoint.clone(),
                        output: output.clone(),
                    })
                }
                OperationStatus::Failed { checkpoint, error } => Ok(StoredAdmission::Failed {
                    checkpoint: checkpoint.clone(),
                    error: error.clone(),
                }),
                OperationStatus::Cancelled { .. } => Ok(StoredAdmission::Cancelled),
            };
        }
        self.apply(Transition::OperationAccepted {
            operation_id: operation_id.clone(),
            input,
        })
        .await?;
        self.claimed.insert(operation_id, caller.clone());
        Ok(StoredAdmission::Accepted)
    }

    async fn admit_automatic(
        &mut self,
        caller: &Caller,
        candidate_operation_id: String,
        input: EncodedPayload,
    ) -> Result<(String, StoredAdmission)> {
        if let Some((pending_id, operation)) = self
            .state
            .first_pending_operation_where(|pending_id| !self.claimed.contains_key(pending_id))
        {
            if operation.input != input {
                return Err(Error::OperationBlocked {
                    operation_id: candidate_operation_id,
                    pending_id: pending_id.to_owned(),
                });
            }
            let recovered_operation_id = pending_id.to_owned();
            let admission = self
                .admit(caller, recovered_operation_id.clone(), input)
                .await?;
            return Ok((recovered_operation_id, admission));
        }

        let admission = self
            .admit(caller, candidate_operation_id.clone(), input)
            .await?;
        Ok((candidate_operation_id, admission))
    }

    async fn begin_attempt(&mut self, caller: &Caller, operation_id: String) -> Result<()> {
        self.require_claimed(caller, &operation_id)?;
        if let Some((pending_id, _)) = self.state.first_pending_operation()
            && pending_id != operation_id
        {
            return Err(Error::OperationBlocked {
                operation_id,
                pending_id: pending_id.to_owned(),
            });
        }
        let operation = self.state.operation(&operation_id).ok_or_else(|| {
            Error::InvalidState(format!("operation `{operation_id}` was not accepted"))
        })?;
        if operation.status.is_terminal() {
            return Err(Error::OperationTerminal { operation_id });
        }
        if self.running.contains(&operation_id) {
            return Err(Error::AttemptActive { operation_id });
        }
        self.running.insert(operation_id.clone());
        Ok(())
    }

    async fn accept_steer(
        &mut self,
        caller: &Caller,
        operation_id: String,
        accepted_after_model_call_index: u32,
        input: EncodedPayload,
    ) -> Result<u32> {
        self.require_claimed(caller, &operation_id)?;
        self.require_running(&operation_id)?;
        let steer_index = u32::try_from(
            self.state
                .operation(&operation_id)
                .ok_or_else(|| {
                    Error::InvalidState(format!("operation `{operation_id}` was not accepted"))
                })?
                .steers
                .len(),
        )
        .ok()
        .and_then(|length| length.checked_add(1))
        .ok_or_else(|| {
            Error::InvalidState(format!(
                "operation `{operation_id}` exceeded the steer counter range"
            ))
        })?;
        self.apply(Transition::SteerAccepted {
            operation_id,
            steer_index,
            accepted_after_model_call_index,
            input,
        })
        .await?;
        Ok(steer_index)
    }

    fn retained_steers(&self, caller: &Caller, operation_id: &str) -> Result<Vec<StoredSteer>> {
        self.require_claimed(caller, operation_id)?;
        self.require_running(operation_id)?;
        let operation = self.state.operation(operation_id).ok_or_else(|| {
            Error::InvalidState(format!("operation `{operation_id}` was not accepted"))
        })?;
        operation
            .steers
            .iter()
            .cloned()
            .enumerate()
            .map(|(offset, state)| {
                let index = u32::try_from(offset)
                    .ok()
                    .and_then(|offset| offset.checked_add(1))
                    .ok_or_else(|| {
                        Error::InvalidState(format!(
                            "operation `{operation_id}` exceeded the steer counter range"
                        ))
                    })?;
                Ok(StoredSteer { index, state })
            })
            .collect()
    }

    async fn bind_steer(
        &mut self,
        caller: &Caller,
        operation_id: String,
        steer_index: u32,
        model_call_index: u32,
    ) -> Result<()> {
        self.require_claimed(caller, &operation_id)?;
        self.require_running(&operation_id)?;
        let retained = self
            .state
            .operation(&operation_id)
            .and_then(|operation| {
                steer_index.checked_sub(1).and_then(|index| {
                    usize::try_from(index)
                        .ok()
                        .and_then(|index| operation.steers.get(index))
                })
            })
            .ok_or_else(|| {
                Error::InvalidState(format!(
                    "steer {steer_index} in operation `{operation_id}` was bound before acceptance"
                ))
            })?;
        match retained.model_call_index {
            Some(retained) if retained == model_call_index => return Ok(()),
            Some(retained) => {
                return Err(Error::InvalidState(format!(
                    "steer {steer_index} in operation `{operation_id}` changed model boundary from {retained} to {model_call_index}"
                )));
            }
            None => {}
        }
        self.apply(Transition::SteerBound {
            operation_id,
            steer_index,
            model_call_index,
        })
        .await
    }

    async fn begin_step(
        &mut self,
        caller: &Caller,
        operation_id: String,
        step_id: String,
        kind: String,
        input: EncodedPayload,
    ) -> Result<StoredBeginStep> {
        self.require_claimed(caller, &operation_id)?;
        if let Some((pending_id, _)) = self.state.first_pending_operation()
            && pending_id != operation_id
        {
            return Err(Error::OperationBlocked {
                operation_id,
                pending_id: pending_id.to_owned(),
            });
        }
        self.require_running(&operation_id)?;
        if let Some(step) = self
            .state
            .operation(&operation_id)
            .and_then(|operation| operation.steps.get(&step_id))
        {
            if step.kind != kind || step.input != input {
                return Err(Error::InvalidState(format!(
                    "step `{step_id}` in operation `{operation_id}` changed definition"
                )));
            }
            match &step.status {
                StepStatus::Completed(output) => {
                    return Ok(StoredBeginStep::Replay(output.clone()));
                }
                StepStatus::EffectPending => {}
            }
        }
        let entry = Transition::StepStarted {
            operation_id: operation_id.clone(),
            step_id,
            kind,
            input,
        };
        self.apply(entry).await?;
        Ok(StoredBeginStep::Execute)
    }

    async fn complete_step(
        &mut self,
        caller: &Caller,
        operation_id: String,
        step_id: String,
        output: EncodedPayload,
    ) -> Result<()> {
        self.require_claimed(caller, &operation_id)?;
        let Some(step) = self
            .state
            .operation(&operation_id)
            .and_then(|operation| operation.steps.get(&step_id))
        else {
            return Err(Error::StepNotStarted {
                operation_id,
                step_id,
            });
        };
        let status = step.status.clone();
        self.require_running(&operation_id)?;
        match status {
            StepStatus::Completed(_) => Err(Error::InvalidState(format!(
                "step `{step_id}` in operation `{operation_id}` already completed"
            ))),
            StepStatus::EffectPending => {
                self.apply(Transition::StepCompleted {
                    operation_id,
                    step_id,
                    output,
                })
                .await
            }
        }
    }

    async fn complete(
        &mut self,
        caller: &Caller,
        operation_id: String,
        checkpoint: EncodedPayload,
        output: EncodedPayload,
    ) -> Result<()> {
        self.require_claimed(caller, &operation_id)?;
        self.require_running(&operation_id)?;
        let entry = Transition::OperationCompleted {
            operation_id: operation_id.clone(),
            checkpoint,
            output,
        };
        let outcome = self.apply_terminal(entry).await;
        if finishing_attempt_releases_claim(&outcome) {
            self.release_claim_if_owned(caller, &operation_id);
        }
        outcome
    }

    async fn fail(
        &mut self,
        caller: &Caller,
        operation_id: String,
        checkpoint: EncodedPayload,
        error: String,
    ) -> Result<()> {
        self.require_claimed(caller, &operation_id)?;
        self.require_running(&operation_id)?;
        let entry = Transition::OperationFailed {
            operation_id: operation_id.clone(),
            checkpoint,
            error,
        };
        let outcome = self.apply_terminal(entry).await;
        if finishing_attempt_releases_claim(&outcome) {
            self.release_claim_if_owned(caller, &operation_id);
        }
        outcome
    }

    fn require_claimed(&self, caller: &Caller, operation_id: &str) -> Result<()> {
        if self.claimed.get(operation_id) == Some(caller) {
            Ok(())
        } else {
            Err(Error::OperationNotClaimed {
                operation_id: operation_id.to_owned(),
            })
        }
    }

    fn require_running(&self, operation_id: &str) -> Result<()> {
        let operation = self.state.operation(operation_id).ok_or_else(|| {
            Error::InvalidState(format!("operation `{operation_id}` was not accepted"))
        })?;
        if operation.status.is_terminal() {
            return Err(Error::OperationTerminal {
                operation_id: operation_id.to_owned(),
            });
        }
        if !self.running.contains(operation_id) {
            return Err(Error::AttemptNotStarted {
                operation_id: operation_id.to_owned(),
            });
        }
        Ok(())
    }

    fn release_claim(&mut self, caller: &Caller, operation_id: &str) -> Result<()> {
        self.require_claimed(caller, operation_id)?;
        self.claimed.remove(operation_id);
        self.running.remove(operation_id);
        Ok(())
    }

    fn release_claim_if_owned(&mut self, caller: &Caller, operation_id: &str) {
        if self.claimed.get(operation_id) == Some(caller) {
            self.claimed.remove(operation_id);
            self.running.remove(operation_id);
        }
    }

    async fn apply(&mut self, entry: Transition) -> Result<()> {
        let expected_revision = self.state.revision().checked_add(1).ok_or_else(|| {
            Error::InvalidState("state revision exceeded the u64 range".to_owned())
        })?;
        let mut next = self.state.clone();
        next.apply_transition(expected_revision, entry)?;
        if let Some(limit) = self.terminal_receipt_limit {
            let _ = next.retain_terminal_receipts(limit);
        }
        self.persist(next).await
    }

    async fn persist(&mut self, next: DurableState) -> Result<()> {
        let expected_revision = self.state.revision().checked_add(1).ok_or_else(|| {
            Error::InvalidState("state revision exceeded the u64 range".to_owned())
        })?;
        if next.revision() != expected_revision {
            return Err(Error::InvalidState(format!(
                "next current state has revision {}, expected {expected_revision}",
                next.revision()
            )));
        }
        let payload = next.checkpoint_payload()?;
        let revision = match self
            .store
            .replace(&self.state_id, &self.owner, self.state.revision(), &payload)
            .await
        {
            Ok(revision) => revision,
            Err(error @ StoreError::NotCommitted(_)) => return Err(error.into()),
            Err(error) => {
                self.poisoned = true;
                return Err(error.into());
            }
        };
        if revision != expected_revision {
            self.poisoned = true;
            return Err(Error::InvalidState(format!(
                "store returned revision {revision} after replacing expected revision {expected_revision}"
            )));
        }
        self.state = next;
        Ok(())
    }

    async fn apply_terminal(&mut self, entry: Transition) -> Result<()> {
        self.apply(entry).await
    }

    async fn prune_retained(&mut self, _explicit: bool) -> Result<()> {
        let Some(limit) = self.terminal_receipt_limit else {
            return Ok(());
        };
        let mut next = self.state.clone();
        if !next.retain_terminal_receipts(limit) {
            return Ok(());
        }
        let revision = self.state.revision().checked_add(1).ok_or_else(|| {
            Error::InvalidState("state revision exceeded the u64 range".to_owned())
        })?;
        next.advance_revision(revision)?;
        self.persist(next).await
    }
}

const fn finishing_attempt_releases_claim(outcome: &Result<()>) -> bool {
    matches!(
        outcome,
        Ok(()) | Err(Error::Store(StoreError::NotCommitted(_)))
    )
}

fn reduce(stored: StoredState) -> Result<DurableState> {
    let Some(payload) = stored.payload else {
        return if stored.revision == 0 {
            Ok(DurableState::default())
        } else {
            Err(Error::InvalidState(format!(
                "store reported revision {} without a payload",
                stored.revision
            )))
        };
    };
    if stored.revision == 0 {
        return Err(Error::InvalidState(
            "store retained a payload at revision zero".to_owned(),
        ));
    }
    let RetainedCheckpoint {
        nanocodex_durable_state,
    } = crate::encoding::decode(&payload).map_err(|source| Error::Decode {
        revision: stored.revision,
        source,
    })?;
    DurableState::from_checkpoint(stored.revision, nanocodex_durable_state)
}

/// Cheap command handle for an owned durable-state driver.
///
/// The spawned driver is the sole owner of the reduced state state and all
/// live operation claims. Clones only enqueue commands and await typed replies.
pub struct DurableSession {
    state_id: Arc<str>,
    store: SharedStore,
    terminal_receipt_limit: Option<usize>,
    commands: mpsc::Sender<Command>,
    releases: mpsc::UnboundedSender<ReleaseSignal>,
    caller_id: OwnerId,
    active_claims: AtomicUsize,
}

impl Clone for DurableSession {
    fn clone(&self) -> Self {
        Self {
            state_id: Arc::clone(&self.state_id),
            store: self.store.clone(),
            terminal_receipt_limit: self.terminal_receipt_limit,
            commands: self.commands.clone(),
            releases: self.releases.clone(),
            caller_id: OwnerId::new(),
            active_claims: AtomicUsize::new(0),
        }
    }
}

impl Drop for DurableSession {
    fn drop(&mut self) {
        if self.active_claims.load(Ordering::Acquire) > 0 {
            let _ = self
                .releases
                .send(ReleaseSignal::Direct(self.caller_id.clone()));
        }
    }
}

impl DurableSession {
    /// Loads and validates a durable session, then spawns its owning driver.
    pub async fn open<S>(store: S, state_id: impl Into<String>) -> Result<Self>
    where
        S: StateStore + 'static,
    {
        Self::open_inner(store, state_id.into(), None).await
    }

    /// Loads a durable session whose compacted checkpoint retains at most the
    /// newest `limit` terminal replay receipts.
    ///
    /// The embedding application must preserve older exact-ID results before
    /// selecting this policy. Unresolved operations and the latest resumable
    /// model checkpoint are always retained.
    pub async fn open_with_terminal_receipt_limit<S>(
        store: S,
        state_id: impl Into<String>,
        limit: usize,
    ) -> Result<Self>
    where
        S: StateStore + 'static,
    {
        Self::open_inner(store, state_id.into(), Some(limit)).await
    }

    /// Removes old terminal replay receipts from the complete retained state.
    ///
    /// This lets an embedding host reduce a large recovered state before it
    /// allocates the model and tool runtime that will consume the checkpoint.
    pub async fn prune_receipts(&self) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::PruneReceipts {
            caller: Caller::Direct(self.caller_id.clone()),
            result,
        })
        .await?;
        receive(receiver).await
    }

    async fn open_inner<S>(
        store: S,
        state_id: String,
        terminal_receipt_limit: Option<usize>,
    ) -> Result<Self>
    where
        S: StateStore + 'static,
    {
        let store = SharedStore::new(store)?;
        Self::open_shared(store, state_id, terminal_receipt_limit).await
    }

    async fn open_shared(
        mut store: SharedStore,
        state_id: String,
        terminal_receipt_limit: Option<usize>,
    ) -> Result<Self> {
        if state_id.trim().is_empty() {
            return Err(Error::InvalidState(
                "state identity must not be empty".to_owned(),
            ));
        }
        let acquired = store.acquire(&state_id, OwnerId::new()).await?;
        let state = reduce(acquired.state)?;
        let state_id = Arc::<str>::from(state_id);
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (releases, release_receiver) = mpsc::unbounded_channel();
        spawn_driver(Driver {
            store: Box::new(store.clone()),
            state_id: Arc::clone(&state_id),
            state,
            terminal_receipt_limit,
            owner: acquired.owner,
            next_agent_generation: 0,
            active_agent_generation: None,
            claimed: HashMap::new(),
            running: HashSet::new(),
            poisoned: false,
            commands: receiver,
            releases: release_receiver,
        })?;
        Ok(Self {
            state_id,
            store,
            terminal_receipt_limit,
            commands,
            releases,
            caller_id: OwnerId::new(),
            active_claims: AtomicUsize::new(0),
        })
    }

    /// Stable host-store state identity.
    #[must_use]
    pub fn state_id(&self) -> &str {
        &self.state_id
    }

    pub(crate) async fn open_agent_state(&self, state_id: String) -> Result<Self> {
        Self::open_shared(self.store.clone(), state_id, self.terminal_receipt_limit).await
    }

    /// Copies the current reduced state from the owning driver.
    pub async fn state(&self) -> Result<DurableState> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::State { result }).await?;
        receiver.await.map_err(|_| Error::DriverStopped)
    }

    /// Copies the latest terminal checkpoint from the owning driver without
    /// cloning the rest of the reduced state.
    pub async fn latest_checkpoint(&self) -> Result<Option<EncodedPayload>> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::LatestCheckpoint { result }).await?;
        receiver.await.map_err(|_| Error::DriverStopped)
    }

    pub(crate) async fn acquire_agent(&self) -> Result<(DurableOwner, Option<EncodedPayload>)> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::AcquireAgent { result }).await?;
        let acquired = receive(receiver).await?;
        let owner = DurableOwner {
            generation: acquired.generation,
            commands: self.commands.clone(),
            releases: self.releases.clone(),
            release: Arc::new(OwnerReleaseState::new()),
        };
        Ok((owner, acquired.checkpoint))
    }

    /// Durably accepts and claims an operation, retaining terminal payloads in
    /// their encoded state form.
    pub async fn admit<I>(&self, operation_id: impl Into<String>, input: &I) -> Result<Admission>
    where
        I: Serialize + ?Sized,
    {
        Ok(self
            .admit_encoded(operation_id.into(), EncodedPayload::encode(input)?)
            .await?
            .into_encoded())
    }

    /// Durably accepts and claims an operation with typed replay values.
    pub async fn admit_typed<I, C, O>(
        &self,
        operation_id: impl Into<String>,
        input: &I,
    ) -> Result<Admission<C, O>>
    where
        I: Serialize + ?Sized,
        C: DeserializeOwned,
        O: DeserializeOwned,
    {
        self.admit_encoded(operation_id.into(), EncodedPayload::encode(input)?)
            .await?
            .decode()
    }

    /// Durably admits automatically identified work, retaining terminal
    /// payloads in their encoded state form.
    ///
    /// The candidate identity is used for new work. If the oldest unclaimed
    /// pending operation has identical input, that operation is reclaimed and
    /// its previously stored identity is returned instead.
    pub async fn admit_automatic<I>(
        &self,
        candidate_operation_id: impl Into<String>,
        input: &I,
    ) -> Result<AutomaticAdmission>
    where
        I: Serialize + ?Sized,
    {
        let (operation_id, admission) = self
            .admit_automatic_encoded(
                candidate_operation_id.into(),
                EncodedPayload::encode(input)?,
            )
            .await?;
        Ok(AutomaticAdmission {
            operation_id,
            admission: admission.into_encoded(),
        })
    }

    /// Durably admits automatically identified work, reclaiming the oldest
    /// unclaimed pending operation when its input is identical.
    ///
    /// `candidate_operation_id` is used for new work. Recovered work retains
    /// its previously stored identity, which is returned with the admission.
    pub async fn admit_automatic_typed<I, C, O>(
        &self,
        candidate_operation_id: impl Into<String>,
        input: &I,
    ) -> Result<AutomaticAdmission<C, O>>
    where
        I: Serialize + ?Sized,
        C: DeserializeOwned,
        O: DeserializeOwned,
    {
        let (operation_id, admission) = self
            .admit_automatic_encoded(
                candidate_operation_id.into(),
                EncodedPayload::encode(input)?,
            )
            .await?;
        let admission = admission.decode()?;
        Ok(AutomaticAdmission {
            operation_id,
            admission,
        })
    }

    async fn admit_encoded(
        &self,
        operation_id: String,
        input: EncodedPayload,
    ) -> Result<StoredAdmission> {
        let (result, receiver) = oneshot::channel();
        let (acknowledge, acknowledged) = oneshot::channel();
        self.send(Command::Admit {
            caller: Caller::Direct(self.caller_id.clone()),
            operation_id,
            input,
            acknowledged,
            release_commands: self.commands.clone(),
            result,
        })
        .await?;
        let admission = receive(receiver).await?;
        if matches!(
            &admission,
            StoredAdmission::Accepted | StoredAdmission::Pending
        ) {
            self.active_claims.fetch_add(1, Ordering::AcqRel);
        }
        let _ = acknowledge.send(());
        Ok(admission)
    }

    async fn admit_automatic_encoded(
        &self,
        candidate_operation_id: String,
        input: EncodedPayload,
    ) -> Result<(String, StoredAdmission)> {
        let (result, receiver) = oneshot::channel();
        let (acknowledge, acknowledged) = oneshot::channel();
        self.send(Command::AdmitAutomatic {
            caller: Caller::Direct(self.caller_id.clone()),
            candidate_operation_id,
            input,
            acknowledged,
            release_commands: self.commands.clone(),
            result,
        })
        .await?;
        let admission = receive(receiver).await?;
        if matches!(
            &admission.1,
            StoredAdmission::Accepted | StoredAdmission::Pending
        ) {
            self.active_claims.fetch_add(1, Ordering::AcqRel);
        }
        let _ = acknowledge.send(());
        Ok(admission)
    }

    /// Releases a live claim without changing durable state state.
    pub async fn release(&self, operation_id: impl Into<String>) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::Release {
            caller: Caller::Direct(self.caller_id.clone()),
            operation_id: operation_id.into(),
            result,
        })
        .await?;
        let outcome = receive(receiver).await;
        if outcome.is_ok() {
            self.release_one_claim();
        }
        outcome
    }

    /// Claims an accepted operation for one live execution attempt.
    pub async fn begin_attempt(&self, operation_id: impl Into<String>) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::BeginAttempt {
            caller: Caller::Direct(self.caller_id.clone()),
            operation_id: operation_id.into(),
            result,
        })
        .await?;
        receive(receiver).await
    }

    /// Begins or replays one stable step, retaining replay output in its
    /// encoded state form.
    pub async fn begin_step<I>(
        &self,
        operation_id: impl Into<String>,
        step_id: impl Into<String>,
        kind: impl Into<String>,
        input: &I,
    ) -> Result<BeginStep>
    where
        I: Serialize + ?Sized,
    {
        match self
            .begin_step_encoded(
                operation_id.into(),
                step_id.into(),
                kind.into(),
                EncodedPayload::encode(input)?,
            )
            .await?
        {
            StoredBeginStep::Execute => Ok(BeginStep::Execute),
            StoredBeginStep::Replay(output) => Ok(BeginStep::Replay(output)),
        }
    }

    /// Begins or replays one stable step with a typed replay output.
    pub async fn begin_step_typed<I, O>(
        &self,
        operation_id: impl Into<String>,
        step_id: impl Into<String>,
        kind: impl Into<String>,
        input: &I,
    ) -> Result<BeginStep<O>>
    where
        I: Serialize + ?Sized,
        O: DeserializeOwned,
    {
        match self
            .begin_step_encoded(
                operation_id.into(),
                step_id.into(),
                kind.into(),
                EncodedPayload::encode(input)?,
            )
            .await?
        {
            StoredBeginStep::Execute => Ok(BeginStep::Execute),
            StoredBeginStep::Replay(output) => Ok(BeginStep::Replay(output.decode()?)),
        }
    }

    async fn begin_step_encoded(
        &self,
        operation_id: String,
        step_id: String,
        kind: String,
        input: EncodedPayload,
    ) -> Result<StoredBeginStep> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::BeginStep {
            caller: Caller::Direct(self.caller_id.clone()),
            operation_id,
            step_id,
            kind,
            input,
            result,
        })
        .await?;
        receive(receiver).await
    }

    /// Commits a step output for future replay.
    pub async fn complete_step<T: Serialize + ?Sized>(
        &self,
        operation_id: impl Into<String>,
        step_id: impl Into<String>,
        output: &T,
    ) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::CompleteStep {
            caller: Caller::Direct(self.caller_id.clone()),
            operation_id: operation_id.into(),
            step_id: step_id.into(),
            output: EncodedPayload::encode(output)?,
            result,
        })
        .await?;
        receive(receiver).await
    }

    /// Atomically terminalizes an operation with its checkpoint and result.
    pub async fn complete<C: Serialize + ?Sized, O: Serialize + ?Sized>(
        &self,
        operation_id: impl Into<String>,
        checkpoint: &C,
        output: &O,
    ) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::Complete {
            caller: Caller::Direct(self.caller_id.clone()),
            operation_id: operation_id.into(),
            checkpoint: EncodedPayload::encode(checkpoint)?,
            output: EncodedPayload::encode(output)?,
            result,
        })
        .await?;
        let outcome = receive(receiver).await;
        if finishing_attempt_releases_claim(&outcome) {
            self.release_one_claim();
        }
        outcome
    }

    /// Atomically terminalizes a failed operation with its safe checkpoint.
    pub async fn fail<C: Serialize + ?Sized>(
        &self,
        operation_id: impl Into<String>,
        checkpoint: &C,
        error: impl Into<String>,
    ) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::Fail {
            caller: Caller::Direct(self.caller_id.clone()),
            operation_id: operation_id.into(),
            checkpoint: EncodedPayload::encode(checkpoint)?,
            error: error.into(),
            result,
        })
        .await?;
        let outcome = receive(receiver).await;
        if finishing_attempt_releases_claim(&outcome) {
            self.release_one_claim();
        }
        outcome
    }

    /// Ends the live attempt while leaving the durable operation pending.
    ///
    /// The diagnostic is intentionally not persisted: attempts are live
    /// scheduling state, while accepted input, effects, and terminals are the
    /// durable protocol.
    pub async fn fail_attempt(
        &self,
        operation_id: impl Into<String>,
        _error: impl Into<String>,
    ) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::FailAttempt {
            caller: Caller::Direct(self.caller_id.clone()),
            operation_id: operation_id.into(),
            result,
        })
        .await?;
        let outcome = receive(receiver).await;
        if finishing_attempt_releases_claim(&outcome) {
            self.release_one_claim();
        }
        outcome
    }

    /// Explicitly terminalizes an operation as cancelled.
    ///
    /// This is valid only before an attempt starts. Active Agent cancellation
    /// commits its safe interrupted checkpoint through the execution policy.
    pub async fn cancel(&self, operation_id: impl Into<String>) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::Cancel {
            caller: Caller::Direct(self.caller_id.clone()),
            operation_id: operation_id.into(),
            checkpoint: None,
            result,
        })
        .await?;
        let outcome = receive(receiver).await;
        if finishing_attempt_releases_claim(&outcome) {
            self.release_one_claim();
        }
        outcome
    }

    async fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .await
            .map_err(|_| Error::DriverStopped)
    }

    fn release_one_claim(&self) {
        let _ = self
            .active_claims
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |claims| {
                claims.checked_sub(1)
            });
    }
}

pub(crate) struct DurableOwner {
    generation: u64,
    commands: mpsc::Sender<Command>,
    releases: mpsc::UnboundedSender<ReleaseSignal>,
    release: Arc<OwnerReleaseState>,
}

impl DurableOwner {
    pub(crate) async fn recover_failure(
        &self,
        operation_id: String,
    ) -> Result<Option<OperationStatus>> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::RecoverFailure {
            caller: self.caller()?,
            operation_id,
            result,
        })
        .await?;
        receiver.await.map_err(|_| Error::DriverStopped)?
    }

    fn caller(&self) -> Result<Caller> {
        if self.release.state.load(Ordering::Acquire) != OWNER_ACTIVE {
            Err(Error::ModelOwnerFenced)
        } else {
            Ok(Caller::Agent(self.generation))
        }
    }

    async fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .await
            .map_err(|_| Error::DriverStopped)
    }

    pub(crate) async fn admit_typed<I, C, O>(
        &self,
        operation_id: String,
        input: &I,
    ) -> Result<Admission<C, O>>
    where
        I: Serialize + ?Sized,
        C: DeserializeOwned,
        O: DeserializeOwned,
    {
        let (result, receiver) = oneshot::channel();
        let (acknowledge, acknowledged) = oneshot::channel();
        self.send(Command::Admit {
            caller: self.caller()?,
            operation_id,
            input: EncodedPayload::encode(input)?,
            acknowledged,
            release_commands: self.commands.clone(),
            result,
        })
        .await?;
        let admission = receive(receiver).await?;
        let _ = acknowledge.send(());
        admission.decode()
    }

    pub(crate) async fn admit_automatic_typed<I, C, O>(
        &self,
        candidate_operation_id: String,
        input: &I,
    ) -> Result<AutomaticAdmission<C, O>>
    where
        I: Serialize + ?Sized,
        C: DeserializeOwned,
        O: DeserializeOwned,
    {
        let (result, receiver) = oneshot::channel();
        let (acknowledge, acknowledged) = oneshot::channel();
        self.send(Command::AdmitAutomatic {
            caller: self.caller()?,
            candidate_operation_id,
            input: EncodedPayload::encode(input)?,
            acknowledged,
            release_commands: self.commands.clone(),
            result,
        })
        .await?;
        let (operation_id, admission) = receive(receiver).await?;
        let _ = acknowledge.send(());
        Ok(AutomaticAdmission {
            operation_id,
            admission: admission.decode()?,
        })
    }

    pub(crate) async fn release_claim(&self, operation_id: String) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::Release {
            caller: self.caller()?,
            operation_id,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn begin_attempt(&self, operation_id: String) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::BeginAttempt {
            caller: self.caller()?,
            operation_id,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn accept_steer<I: Serialize + ?Sized>(
        &self,
        operation_id: String,
        accepted_after_model_call_index: u32,
        input: &I,
    ) -> Result<u32> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::AcceptSteer {
            caller: self.caller()?,
            operation_id,
            accepted_after_model_call_index,
            input: EncodedPayload::encode(input)?,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn retained_steers(&self, operation_id: String) -> Result<Vec<StoredSteer>> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::RetainedSteers {
            caller: self.caller()?,
            operation_id,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn bind_steer(
        &self,
        operation_id: String,
        steer_index: u32,
        model_call_index: u32,
    ) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::BindSteer {
            caller: self.caller()?,
            operation_id,
            steer_index,
            model_call_index,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn retained_step_input(
        &self,
        operation_id: String,
        step_id: String,
        kind: String,
    ) -> Result<Option<EncodedPayload>> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::RetainedStepInput {
            caller: self.caller()?,
            operation_id,
            step_id,
            kind,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn begin_step<I>(
        &self,
        operation_id: String,
        step_id: String,
        kind: String,
        input: &I,
    ) -> Result<BeginStep>
    where
        I: Serialize + ?Sized,
    {
        let (result, receiver) = oneshot::channel();
        self.send(Command::BeginStep {
            caller: self.caller()?,
            operation_id,
            step_id,
            kind,
            input: EncodedPayload::encode(input)?,
            result,
        })
        .await?;
        match receive(receiver).await? {
            StoredBeginStep::Execute => Ok(BeginStep::Execute),
            StoredBeginStep::Replay(output) => Ok(BeginStep::Replay(output)),
        }
    }

    pub(crate) async fn complete_step<T: Serialize + ?Sized>(
        &self,
        operation_id: String,
        step_id: String,
        output: &T,
    ) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::CompleteStep {
            caller: self.caller()?,
            operation_id,
            step_id,
            output: EncodedPayload::encode(output)?,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn complete<C: Serialize + ?Sized, O: Serialize + ?Sized>(
        &self,
        operation_id: String,
        checkpoint: &C,
        output: &O,
    ) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::Complete {
            caller: self.caller()?,
            operation_id,
            checkpoint: EncodedPayload::encode(checkpoint)?,
            output: EncodedPayload::encode(output)?,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn fail<C: Serialize + ?Sized>(
        &self,
        operation_id: String,
        checkpoint: &C,
        error: String,
    ) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::Fail {
            caller: self.caller()?,
            operation_id,
            checkpoint: EncodedPayload::encode(checkpoint)?,
            error,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn fail_attempt(&self, operation_id: String, _error: String) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::FailAttempt {
            caller: self.caller()?,
            operation_id,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn cancel<C: Serialize + ?Sized>(
        &self,
        operation_id: String,
        checkpoint: Option<&C>,
    ) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::Cancel {
            caller: self.caller()?,
            operation_id,
            checkpoint: checkpoint.map(EncodedPayload::encode).transpose()?,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn commit_checkpoint<C: Serialize + ?Sized>(
        &self,
        checkpoint: &C,
    ) -> Result<()> {
        let (result, receiver) = oneshot::channel();
        self.send(Command::CommitCheckpoint {
            caller: self.caller()?,
            checkpoint: EncodedPayload::encode(checkpoint)?,
            result,
        })
        .await?;
        receive(receiver).await
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        match self.release.state.compare_exchange(
            OWNER_ACTIVE,
            OWNER_RELEASING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(OWNER_ACTIVE) => {
                if self
                    .releases
                    .send(ReleaseSignal::Agent(AgentRelease {
                        generation: self.generation,
                        state: Arc::clone(&self.release),
                    }))
                    .is_err()
                {
                    self.release.finish();
                    return Err(Error::DriverStopped);
                }
            }
            Err(OWNER_RELEASED) => return Ok(()),
            Err(OWNER_RELEASING) => {}
            Ok(_) | Err(_) => {
                return Err(Error::InvalidState(
                    "durable owner entered an invalid release state".to_owned(),
                ));
            }
        }
        let mut completed = self.release.completed.subscribe();
        if *completed.borrow() {
            return Ok(());
        }
        tokio::select! {
            changed = completed.changed() => changed.map_err(|_| Error::DriverStopped),
            () = self.commands.closed() => {
                self.release.finish();
                Err(Error::DriverStopped)
            }
        }
    }
}

impl Drop for DurableOwner {
    fn drop(&mut self) {
        if self
            .release
            .state
            .compare_exchange(
                OWNER_ACTIVE,
                OWNER_RELEASING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            let _ = self.releases.send(ReleaseSignal::Agent(AgentRelease {
                generation: self.generation,
                state: Arc::clone(&self.release),
            }));
        }
    }
}

async fn receive<T>(receiver: oneshot::Receiver<Result<T>>) -> Result<T> {
    receiver.await.map_err(|_| Error::DriverStopped)?
}

#[cfg(not(target_family = "wasm"))]
fn spawn_claim_ack(
    commands: mpsc::Sender<Command>,
    acknowledged: oneshot::Receiver<()>,
    caller: Caller,
    operation_id: String,
) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        drop(runtime.spawn(async move {
            if acknowledged.await.is_err() {
                let (result, _receiver) = oneshot::channel();
                drop(
                    commands
                        .send(Command::Release {
                            caller,
                            operation_id,
                            result,
                        })
                        .await,
                );
            }
        }));
    }
}

#[cfg(target_family = "wasm")]
fn spawn_claim_ack(
    commands: mpsc::Sender<Command>,
    acknowledged: oneshot::Receiver<()>,
    caller: Caller,
    operation_id: String,
) {
    wasm_bindgen_futures::spawn_local(async move {
        if acknowledged.await.is_err() {
            let (result, _receiver) = oneshot::channel();
            drop(
                commands
                    .send(Command::Release {
                        caller,
                        operation_id,
                        result,
                    })
                    .await,
            );
        }
    });
}

#[cfg(not(target_family = "wasm"))]
fn spawn_driver(driver: Driver) -> Result<()> {
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| Error::RuntimeUnavailable)?;
    drop(runtime.spawn(driver.run()));
    Ok(())
}

#[cfg(target_family = "wasm")]
fn spawn_driver(driver: Driver) -> Result<()> {
    wasm_bindgen_futures::spawn_local(driver.run());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, future::Future, task::Poll};

    use super::*;
    use crate::{MemoryStore, OperationState};

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn owner_drop_releases_without_a_runtime_or_bounded_command_capacity() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (session, owner) = runtime.block_on(async {
            let store = MemoryStore::new().unwrap();
            let session = DurableSession::open(store, "drop-release-lane")
                .await
                .unwrap();
            let (owner, _) = session.acquire_agent().await.unwrap();
            (session, owner)
        });

        let mut abandoned_results = Vec::new();
        for _ in 0..COMMAND_CAPACITY {
            let (result, receiver) = oneshot::channel();
            session
                .commands
                .try_send(Command::State { result })
                .unwrap();
            abandoned_results.push(receiver);
        }
        drop(owner);

        runtime.block_on(async {
            let (successor, _) = session.acquire_agent().await.unwrap();
            successor.shutdown().await.unwrap();
        });
        drop(abandoned_results);
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn cancelled_shutdown_keeps_the_drop_release_lane_armed() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (session, owner) = runtime.block_on(async {
            let store = MemoryStore::new().unwrap();
            let session = DurableSession::open(store, "cancelled-shutdown-release")
                .await
                .unwrap();
            let (owner, _) = session.acquire_agent().await.unwrap();
            (session, owner)
        });

        let mut abandoned_results = Vec::new();
        for _ in 0..COMMAND_CAPACITY {
            let (result, receiver) = oneshot::channel();
            session
                .commands
                .try_send(Command::State { result })
                .unwrap();
            abandoned_results.push(receiver);
        }

        let mut shutdown = Box::pin(owner.shutdown());
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        assert!(matches!(
            shutdown.as_mut().poll(&mut context),
            Poll::Pending
        ));
        drop(shutdown);
        drop(owner);

        runtime.block_on(async {
            assert!(matches!(
                session
                    .admit("turn-after-cancelled-shutdown", &"prompt")
                    .await,
                Ok(Admission::Accepted)
            ));
        });
        drop(abandoned_results);
    }

    #[tokio::test]
    async fn stale_agent_capability_cannot_mutate_or_release_its_successor() {
        let store = MemoryStore::new().unwrap();
        let session = DurableSession::open(store, "local-owner-aba")
            .await
            .unwrap();
        let (older, _) = session.acquire_agent().await.unwrap();
        assert!(matches!(
            older
                .admit_typed::<_, u32, String>("turn-1".to_owned(), &"prompt")
                .await,
            Ok(Admission::Accepted)
        ));
        older.begin_attempt("turn-1".to_owned()).await.unwrap();

        let (newer, _) = session.acquire_agent().await.unwrap();
        let revision = session.state().await.unwrap().revision();
        assert!(matches!(
            older.complete("turn-1".to_owned(), &1, &"stale").await,
            Err(Error::ModelOwnerFenced)
        ));
        assert!(matches!(
            older
                .fail_attempt("turn-1".to_owned(), "stale".to_owned())
                .await,
            Err(Error::ModelOwnerFenced)
        ));
        assert!(matches!(
            older.cancel("turn-1".to_owned(), None::<&u32>).await,
            Err(Error::ModelOwnerFenced)
        ));
        assert_eq!(session.state().await.unwrap().revision(), revision);

        older.shutdown().await.unwrap();
        assert!(matches!(
            newer
                .admit_typed::<_, u32, String>("turn-1".to_owned(), &"prompt")
                .await,
            Ok(Admission::Pending)
        ));
        newer.begin_attempt("turn-1".to_owned()).await.unwrap();
        newer
            .complete("turn-1".to_owned(), &2, &"authoritative")
            .await
            .unwrap();
        newer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn direct_mutation_cannot_bypass_a_live_model_owner() {
        let store = MemoryStore::new().unwrap();
        let session = DurableSession::open(store, "direct-owner-bypass")
            .await
            .unwrap();
        let (owner, _) = session.acquire_agent().await.unwrap();
        assert!(matches!(
            session.admit("turn-1", &"prompt").await,
            Err(Error::ModelOwnerActive)
        ));
        assert_eq!(session.state().await.unwrap().revision(), 0);
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn abandoned_admission_handoff_releases_the_exact_claim() {
        let store = MemoryStore::new().unwrap();
        let session = DurableSession::open(store, "abandoned-admission")
            .await
            .unwrap();
        let caller = Caller::Direct(session.caller_id.clone());
        let (result, receiver) = oneshot::channel();
        let (acknowledge, acknowledged) = oneshot::channel();
        session
            .send(Command::Admit {
                caller,
                operation_id: "turn-1".to_owned(),
                input: EncodedPayload::encode(&"prompt").unwrap(),
                acknowledged,
                release_commands: session.commands.clone(),
                result,
            })
            .await
            .unwrap();
        assert!(matches!(
            receive(receiver).await,
            Ok(StoredAdmission::Accepted)
        ));

        drop(acknowledge);
        let mut reclaimed = false;
        for _ in 0..16 {
            tokio::task::yield_now().await;
            match session.admit("turn-1", &"prompt").await {
                Ok(Admission::Pending) => {
                    reclaimed = true;
                    break;
                }
                Err(Error::OperationActive { .. }) => {}
                outcome => panic!("unexpected reclaim outcome: {outcome:?}"),
            }
        }
        assert!(reclaimed, "the abandoned handoff must release its claim");
    }

    #[tokio::test]
    async fn duplicate_admission_cannot_release_a_live_attempt() {
        let store = MemoryStore::new().unwrap();
        let session = DurableSession::open(store, "duplicate-live-attempt")
            .await
            .unwrap();
        let claimant = session.clone();
        let duplicate = session.clone();
        assert!(matches!(
            claimant.admit("turn-1", &"prompt").await,
            Ok(Admission::Accepted)
        ));
        claimant.begin_attempt("turn-1").await.unwrap();
        let revision = session.state().await.unwrap().revision();

        assert!(matches!(
            duplicate.admit("turn-1", &"prompt").await,
            Err(Error::OperationActive { .. })
        ));
        let state = session.state().await.unwrap();
        assert_eq!(state.revision(), revision);
        claimant.complete("turn-1", &1, &"done").await.unwrap();
    }

    #[tokio::test]
    async fn owner_shutdown_observes_a_dead_state_driver() {
        let (commands, receiver) = mpsc::channel(1);
        drop(receiver);
        let (releases, _release_receiver) = mpsc::unbounded_channel();
        let owner = DurableOwner {
            generation: 1,
            commands,
            releases,
            release: Arc::new(OwnerReleaseState::new()),
        };

        assert!(matches!(owner.shutdown().await, Err(Error::DriverStopped)));
    }

    #[tokio::test]
    async fn active_cancellation_advances_the_safe_checkpoint() {
        let store = MemoryStore::new().unwrap();
        let session = DurableSession::open(store, "cancel-checkpoint")
            .await
            .unwrap();
        let (owner, _) = session.acquire_agent().await.unwrap();
        owner
            .admit_typed::<_, u32, String>("turn-1".to_owned(), &"prompt")
            .await
            .unwrap();
        owner.begin_attempt("turn-1".to_owned()).await.unwrap();
        owner
            .cancel("turn-1".to_owned(), Some(&41_u32))
            .await
            .unwrap();
        assert_eq!(
            session
                .latest_checkpoint()
                .await
                .unwrap()
                .unwrap()
                .decode::<u32>()
                .unwrap(),
            41
        );
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn queued_cancellation_cannot_publish_an_unexecuted_checkpoint() {
        let store = MemoryStore::new().unwrap();
        let session = DurableSession::open(store, "queued-cancel-checkpoint")
            .await
            .unwrap();
        let (owner, _) = session.acquire_agent().await.unwrap();
        owner
            .admit_typed::<_, u32, String>("turn-1".to_owned(), &"one")
            .await
            .unwrap();
        owner
            .admit_typed::<_, u32, String>("turn-2".to_owned(), &"two")
            .await
            .unwrap();
        owner.begin_attempt("turn-1".to_owned()).await.unwrap();
        assert!(matches!(
            owner.cancel("turn-2".to_owned(), Some(&99_u32)).await,
            Err(Error::AttemptNotStarted { .. })
        ));
        owner
            .cancel("turn-2".to_owned(), None::<&u32>)
            .await
            .unwrap();
        owner
            .complete("turn-1".to_owned(), &1_u32, &"done")
            .await
            .unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn terminal_boundary_compacts_the_prefix_without_rewinding_revision() {
        let store = MemoryStore::new().unwrap();
        let session = DurableSession::open(store.clone(), "bounded-prefix")
            .await
            .unwrap();
        let (owner, _) = session.acquire_agent().await.unwrap();
        for index in 0..22_u32 {
            let operation_id = format!("turn-{index}");
            let prompt = format!("prompt-{index}");
            assert!(matches!(
                owner
                    .admit_typed::<_, u32, String>(operation_id.clone(), &prompt)
                    .await,
                Ok(Admission::Accepted)
            ));
            owner.begin_attempt(operation_id.clone()).await.unwrap();
            owner
                .complete(operation_id, &index, &format!("output-{index}"))
                .await
                .unwrap();
        }
        owner.shutdown().await.unwrap();

        let mut inspector = store.clone();
        let compacted = inspector
            .acquire("bounded-prefix", OwnerId::new())
            .await
            .unwrap();
        assert_eq!(compacted.state.revision, 44);
        assert!(compacted.state.payload.is_some());

        let reopened = DurableSession::open(store, "bounded-prefix").await.unwrap();
        assert!(matches!(
            reopened
                .admit_typed::<_, u32, String>("turn-0", &"prompt-0")
                .await,
            Ok(Admission::Completed {
                checkpoint: 0,
                output
            }) if output == "output-0"
        ));
        assert_eq!(reopened.state().await.unwrap().revision(), 44);
    }

    #[tokio::test]
    async fn bounded_terminal_receipt_policy_keeps_only_the_newest_compacted_receipts() {
        let store = MemoryStore::new().unwrap();
        let session = DurableSession::open_with_terminal_receipt_limit(
            store.clone(),
            "bounded-terminal-receipts",
            3,
        )
        .await
        .unwrap();
        let (owner, _) = session.acquire_agent().await.unwrap();
        for index in 0..22_u32 {
            let operation_id = format!("turn-{index}");
            assert!(matches!(
                owner
                    .admit_typed::<_, u32, String>(operation_id.clone(), &index)
                    .await,
                Ok(Admission::Accepted)
            ));
            owner.begin_attempt(operation_id.clone()).await.unwrap();
            owner
                .complete(operation_id, &index, &format!("output-{index}"))
                .await
                .unwrap();
        }
        let live = session.state().await.unwrap();
        assert_eq!(live.operations().len(), 3);
        assert!(live.operation("turn-19").is_some());
        assert!(live.operation("turn-18").is_none());
        owner.shutdown().await.unwrap();

        let reopened = DurableSession::open(store, "bounded-terminal-receipts")
            .await
            .unwrap();
        let state = reopened.state().await.unwrap();
        assert_eq!(state.operations().len(), 3);
        assert!(state.operation("turn-19").is_some());
        assert!(state.operation("turn-20").is_some());
        assert!(state.operation("turn-21").is_some());
        assert!(state.operation("turn-18").is_none());
        assert_eq!(
            state.latest_checkpoint().unwrap().decode::<u32>().unwrap(),
            21
        );
    }

    #[tokio::test]
    async fn zero_retention_is_atomic_with_terminal_state() {
        let store = MemoryStore::new().unwrap();
        let session =
            DurableSession::open_with_terminal_receipt_limit(store.clone(), "zero-retention", 0)
                .await
                .unwrap();
        assert!(matches!(
            session.admit("turn-1", &"prompt").await,
            Ok(Admission::Accepted)
        ));
        session.begin_attempt("turn-1").await.unwrap();
        session.complete("turn-1", &1_u32, &"done").await.unwrap();

        assert!(session.state().await.unwrap().operation("turn-1").is_none());
        let mut inspector = store;
        let retained = inspector
            .acquire("zero-retention", OwnerId::new())
            .await
            .unwrap();
        assert_eq!(retained.state.revision, 2);
        assert!(retained.state.payload.is_some());
    }

    #[tokio::test]
    async fn explicit_compaction_rewrites_one_checkpoint_for_a_lower_receipt_limit() {
        let store = MemoryStore::new().unwrap();
        let session = DurableSession::open(store.clone(), "rewrite-one-checkpoint")
            .await
            .unwrap();
        assert!(matches!(
            session.admit("turn-1", &"prompt").await,
            Ok(Admission::Accepted)
        ));
        session.begin_attempt("turn-1").await.unwrap();
        session.complete("turn-1", &1_u32, &"done").await.unwrap();
        session.prune_receipts().await.unwrap();
        drop(session);

        let reopened = DurableSession::open_with_terminal_receipt_limit(
            store.clone(),
            "rewrite-one-checkpoint",
            0,
        )
        .await
        .unwrap();
        assert!(
            reopened
                .state()
                .await
                .unwrap()
                .operation("turn-1")
                .is_some()
        );
        reopened.prune_receipts().await.unwrap();
        assert!(
            reopened
                .state()
                .await
                .unwrap()
                .operation("turn-1")
                .is_none()
        );
        drop(reopened);

        let mut inspector = store;
        let retained = inspector
            .acquire("rewrite-one-checkpoint", OwnerId::new())
            .await
            .unwrap();
        assert_eq!(retained.state.revision, 3);
        assert!(retained.state.payload.is_some());
    }

    #[tokio::test]
    async fn standalone_checkpoint_supersedes_the_latest_terminal_boundary() {
        let store = MemoryStore::new().unwrap();
        let session = DurableSession::open(store.clone(), "standalone-checkpoint")
            .await
            .unwrap();
        let (owner, _) = session.acquire_agent().await.unwrap();
        owner
            .admit_typed::<_, u32, String>("turn-1".to_owned(), &"prompt")
            .await
            .unwrap();
        owner.begin_attempt("turn-1".to_owned()).await.unwrap();
        owner
            .complete("turn-1".to_owned(), &1_u32, &"done")
            .await
            .unwrap();
        owner.commit_checkpoint(&2_u32).await.unwrap();
        owner.shutdown().await.unwrap();

        let reopened = DurableSession::open(store, "standalone-checkpoint")
            .await
            .unwrap();
        assert_eq!(
            reopened
                .latest_checkpoint()
                .await
                .unwrap()
                .unwrap()
                .decode::<u32>()
                .unwrap(),
            2
        );
    }

    #[test]
    fn compacted_steer_state_rejects_impossible_boundaries_and_terminal_shapes() {
        fn steer(accepted_after: u32, bound_to: Option<u32>) -> SteerState {
            SteerState {
                input: EncodedPayload::encode(&"steer").unwrap(),
                accepted_after_model_call_index: accepted_after,
                model_call_index: bound_to,
            }
        }

        fn operation(status: OperationStatus, steers: Vec<SteerState>) -> OperationState {
            OperationState {
                input: EncodedPayload::encode(&"prompt").unwrap(),
                status,
                steps: BTreeMap::new(),
                steers,
                accepted_order: 1,
            }
        }

        fn checkpoint(operation: OperationState) -> StoredState {
            StoredState {
                revision: 10,
                payload: Some(
                    serde_json::json!({
                        "nanocodex_durable_state": {
                            "format": 2,
                            "operations": BTreeMap::from([("turn".to_owned(), operation)]),
                            "latest_checkpoint": null
                        }
                    })
                    .to_string(),
                ),
            }
        }

        let interleaved = operation(
            OperationStatus::Pending,
            vec![steer(1, Some(4)), steer(2, None), steer(2, Some(4))],
        );
        assert!(matches!(
            reduce(checkpoint(interleaved)),
            Err(Error::InvalidState(message)) if message.contains("bound after an unbound steer")
        ));

        let cancelled = operation(
            OperationStatus::Cancelled { checkpoint: None },
            vec![steer(1, None)],
        );
        assert!(matches!(
            reduce(checkpoint(cancelled)),
            Err(Error::InvalidState(message)) if message.contains("cancelled without a checkpoint")
        ));

        let completed = operation(
            OperationStatus::Completed {
                checkpoint: EncodedPayload::encode(&"checkpoint").unwrap(),
                output: EncodedPayload::encode(&"output").unwrap(),
            },
            vec![steer(1, Some(2))],
        );
        assert!(matches!(
            reduce(checkpoint(completed)),
            Err(Error::InvalidState(message)) if message.contains("before steer 1 was consumed")
        ));
    }
}
