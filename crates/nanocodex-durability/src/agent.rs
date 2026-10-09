use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use nanocodex_agent::{
    ExecutionPolicyDisposition, NanocodexBuilder, NanocodexError, Result as AgentResult,
    execution::{
        ExecutionAdmission, ExecutionContinuation, ExecutionFuture, ExecutionOutput,
        ExecutionPolicy, ExecutionSteer, ExecutionStepAdmission, IdentifiedExecutionSteer,
    },
    session::{SessionId, SessionSnapshot},
};
use serde_json::value::RawValue;

use crate::{
    Admission, BeginStep, DurableSession, Error, OperationStatus, SessionRecord,
    session::DurableOwner, shared_store::SharedStore,
};

/// Fluent builder extension that attaches portable durability to an agent.
pub trait DurableAgentExt: Sized {
    /// Restores the state's latest checkpoint and installs its execution
    /// policy at the agent's neutral lifecycle seam.
    fn durability(self, state: DurableSession) -> impl Future<Output = AgentResult<Self>>;
}

impl<F> DurableAgentExt for NanocodexBuilder<F> {
    async fn durability(self, state: DurableSession) -> AgentResult<Self> {
        let state_id = state.state_id().to_owned();
        let record = state.record().await.map_err(agent_error)?;
        let mut builder = self.child_journal(state.child_journal());
        // A durable Codex session is identified by its state; events,
        // persistence, and resume all report the same identity.
        if let Ok(session_id) = state_id.parse::<SessionId>() {
            builder = builder.session_id(session_id);
        }
        if let Some(record) = &record {
            builder = builder.lineage(record.lineage.clone());
        }
        let branches = Branches {
            store: state.shared_store(),
            record,
        };
        let (owner, checkpoint) = state.acquire_agent().await.map_err(agent_error)?;
        let mut known_records = HashSet::new();
        if let Some(checkpoint) = checkpoint {
            let (restored, keys) = crate::context::load_snapshot_with_keys(
                (&owner).into(),
                checkpoint.decode().map_err(agent_error)?,
            )
            .await
            .map_err(agent_error)?;
            if let Some(configured) = builder.resume_snapshot()
                && serde_json::to_string(configured)
                    .map_err(|error| NanocodexError::InvalidCheckpoint(error.to_string()))?
                    != serde_json::to_string(&restored)
                        .map_err(|error| NanocodexError::InvalidCheckpoint(error.to_string()))?
            {
                return Err(NanocodexError::InvalidCheckpoint(
                    "configured resume snapshot does not match the durability state".to_owned(),
                ));
            }
            known_records = keys;
            builder = builder.resume_native_snapshot(restored);
        } else if builder.resume_snapshot().is_none() {
            // A fork's explicitly supplied completed snapshot owns its cache
            // lineage. A fresh durable root alone defaults to its state ID.
            builder = builder.default_prompt_cache_key(state_id.clone());
        }
        let owner = Arc::new(Mutex::new(Some((owner, known_records, branches))));
        Ok(builder
            .execution_policy_factory(move || {
                let (owner, keys, branches) = owner
                    .lock()
                    .map_err(|_| {
                        NanocodexError::InvalidExecutionPolicy(
                            "the durability-attached builder owner lock was poisoned".to_owned(),
                        )
                    })?
                    .take()
                    .ok_or_else(|| {
                        NanocodexError::InvalidExecutionPolicy(
                            "a durability-attached builder can build only one agent; attach durability again to reopen the state"
                                .to_owned(),
                        )
                    })?;
                let policy = DurableExecution::ready(owner, state_id.clone(), Some(branches));
                policy.remember(keys)?;
                let policy: Arc<dyn ExecutionPolicy> = Arc::new(policy);
                Ok(policy)
            }) )
    }
}

/// Where forks of a durable session persist their own resumable state.
#[derive(Clone)]
pub(crate) struct Branches {
    pub(crate) store: SharedStore,
    pub(crate) record: Option<SessionRecord>,
}

impl Branches {
    /// Describes a child of this session; inherits model and workspace.
    pub(crate) fn child_record(
        &self,
        child: &nanocodex_agent::SessionInfo,
    ) -> AgentResult<SessionRecord> {
        let parent = self.record.as_ref().ok_or_else(|| {
            NanocodexError::InvalidExecutionPolicy(
                "a durable session without a recorded model cannot persist forks; open it through SessionStore"
                    .to_owned(),
            )
        })?;
        Ok(parent
            .derive(child.session_id.clone(), child.lineage.origin)
            .with_lineage(child.lineage.clone()))
    }
}

struct DurableExecution {
    owner: DurableOwner,
    state_id: String,
    branches: Option<Branches>,
    context_records: Mutex<HashSet<String>>,
}

impl DurableExecution {
    fn ready(owner: DurableOwner, state_id: String, branches: Option<Branches>) -> Self {
        Self {
            owner,
            state_id,
            branches,
            context_records: Mutex::new(HashSet::new()),
        }
    }

    fn prepare_snapshot(&self, snapshot: SessionSnapshot) -> AgentResult<crate::context::Prepared> {
        let known = self
            .context_records
            .lock()
            .map_err(|_| NanocodexError::InvalidExecutionPolicy("context cache poisoned".into()))?;
        crate::context::prepare_snapshot(snapshot, &known).map_err(agent_error)
    }

    fn remember(&self, keys: HashSet<String>) -> AgentResult<()> {
        *self.context_records.lock().map_err(|_| {
            NanocodexError::InvalidExecutionPolicy("context cache poisoned".into())
        })? = keys;
        Ok(())
    }
}

impl ExecutionPolicy for DurableExecution {
    fn durable_state_id(&self) -> Option<String> {
        Some(self.state_id.clone())
    }

    fn branch(
        &self,
        child: &nanocodex_agent::SessionInfo,
    ) -> AgentResult<Option<Arc<dyn ExecutionPolicy>>> {
        let Some(branches) = &self.branches else {
            return Ok(None);
        };
        let record = branches.child_record(child)?;
        Ok(Some(Arc::new(LazyExecution::new(
            branches.store.clone(),
            record,
        ))))
    }

    fn recover_failure<'a>(
        &'a self,
        operation_id: String,
        error: NanocodexError,
    ) -> ExecutionFuture<'a, NanocodexError> {
        Box::pin(async move {
            if matches!(
                error.execution_policy_disposition(),
                Some(ExecutionPolicyDisposition::Reopen | ExecutionPolicyDisposition::Fatal)
            ) {
                return error;
            }
            let owner = &self.owner;
            match owner.recover_failure(operation_id).await {
                Ok(Some(OperationStatus::Failed { error, .. })) => {
                    NanocodexError::ReplayedExecutionFailed(error)
                }
                Ok(Some(OperationStatus::Cancelled { .. })) => NanocodexError::TurnCancelled,
                // Completed work can still fail in event/result delivery. Its
                // exact ID must replay the receipt, never invent a failed turn.
                Ok(Some(OperationStatus::Pending | OperationStatus::Completed { .. })) => {
                    if error.execution_policy_disposition()
                        == Some(ExecutionPolicyDisposition::Retry)
                    {
                        return error;
                    }
                    NanocodexError::execution_policy_with_disposition(
                        "durable operation recovery",
                        ExecutionPolicyDisposition::Retry,
                        error,
                    )
                }
                // Admission may fail before acceptance, or retention may have
                // pruned a terminal receipt. Neither proves pending work.
                Ok(None) => error,
                Err(error) => agent_error(error),
            }
        })
    }

    fn shutdown<'a>(&'a self) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move { self.owner.shutdown().await.map_err(agent_error) })
    }

    fn commit_checkpoint<'a>(
        &'a self,
        snapshot: SessionSnapshot,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            let prepared = self.prepare_snapshot(snapshot)?;
            self.owner
                .commit_checkpoint(prepared.payload)
                .await
                .map_err(agent_error)?;
            self.remember(prepared.keys)
        })
    }

    fn admit<'a>(
        &'a self,
        operation_id: String,
        input_json: String,
    ) -> ExecutionFuture<'a, AgentResult<ExecutionAdmission>> {
        Box::pin(async move {
            let input = raw(input_json)?;
            let owner = &self.owner;
            let admission = owner
                .admit_typed::<_, crate::context::Snapshot, ExecutionOutput>(operation_id, &input)
                .await
                .map_err(agent_error)?;
            map_admission(owner, admission).await
        })
    }

    fn admit_automatic<'a>(
        &'a self,
        candidate_operation_id: String,
        input_json: String,
    ) -> ExecutionFuture<'a, AgentResult<(String, ExecutionAdmission)>> {
        Box::pin(async move {
            let input = raw(input_json)?;
            let admission = self
                .owner
                .admit_automatic_typed::<_, crate::context::Snapshot, ExecutionOutput>(
                    candidate_operation_id,
                    &input,
                )
                .await
                .map_err(agent_error)?;
            let (operation_id, admission) = admission.into_parts();
            Ok((operation_id, map_admission(&self.owner, admission).await?))
        })
    }

    fn release<'a>(&'a self, operation_id: String) -> ExecutionFuture<'a, ()> {
        Box::pin(async move {
            let _ = self.owner.release_claim(operation_id).await;
        })
    }

    fn cancel<'a>(
        &'a self,
        operation_id: String,
        snapshot: Option<SessionSnapshot>,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            let prepared = snapshot
                .map(|snapshot| self.prepare_snapshot(snapshot))
                .transpose()?;
            let (checkpoint, keys) = match prepared {
                Some(value) => (Some(value.payload), Some(value.keys)),
                None => (None, None),
            };
            self.owner
                .cancel(operation_id, checkpoint)
                .await
                .map_err(agent_error)?;
            if let Some(keys) = keys {
                self.remember(keys)?;
            }
            Ok(())
        })
    }

    fn begin_attempt<'a>(&'a self, operation_id: String) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.owner
                .begin_attempt(operation_id)
                .await
                .map(|_| ())
                .map_err(agent_error)
        })
    }

    fn accept_steer<'a>(
        &'a self,
        operation_id: String,
        accepted_after_model_call_index: u32,
        input_json: String,
    ) -> ExecutionFuture<'a, AgentResult<u32>> {
        Box::pin(async move {
            let input = raw(input_json)?;
            self.owner
                .accept_steer(
                    operation_id,
                    accepted_after_model_call_index,
                    &input,
                    None,
                    true,
                )
                .await
                .map(|index| index.expect("unidentified steer is new"))
                .map_err(agent_error)
        })
    }

    fn supports_steer_receipts(&self) -> bool {
        true
    }

    fn accept_identified_steer<'a>(
        &'a self,
        operation_id: String,
        message_id: String,
        accepted_after_model_call_index: u32,
        input_json: String,
        capacity_available: bool,
    ) -> ExecutionFuture<'a, AgentResult<Option<u32>>> {
        Box::pin(async move {
            let input = raw(input_json)?;
            self.owner
                .accept_steer(
                    operation_id,
                    accepted_after_model_call_index,
                    &input,
                    Some(message_id),
                    capacity_available,
                )
                .await
                .map_err(agent_error)
        })
    }

    fn retained_steers<'a>(
        &'a self,
        operation_id: String,
    ) -> ExecutionFuture<'a, AgentResult<Vec<ExecutionSteer>>> {
        Box::pin(async move {
            self.retained_identified_steers(operation_id)
                .await
                .map(|steers| steers.into_iter().map(|(_, steer)| steer).collect())
        })
    }

    fn retained_identified_steers<'a>(
        &'a self,
        operation_id: String,
    ) -> ExecutionFuture<'a, AgentResult<Vec<IdentifiedExecutionSteer>>> {
        Box::pin(async move {
            self.owner
                .retained_steers(operation_id)
                .await
                .and_then(|steers| {
                    steers
                        .into_iter()
                        .map(|steer| {
                            Ok((
                                steer.state.message_id,
                                ExecutionSteer {
                                    index: steer.index,
                                    accepted_after_model_call_index: steer
                                        .state
                                        .accepted_after_model_call_index,
                                    model_call_index: steer.state.model_call_index,
                                    input_json: steer.state.input.json()?.to_owned(),
                                },
                            ))
                        })
                        .collect()
                })
                .map_err(agent_error)
        })
    }

    fn withdraw_steer<'a>(
        &'a self,
        operation_id: String,
        steer_index: u32,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.owner
                .withdraw_steer(operation_id, steer_index)
                .await
                .map_err(agent_error)
        })
    }

    fn bind_steer<'a>(
        &'a self,
        operation_id: String,
        steer_index: u32,
        model_call_index: u32,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.owner
                .bind_steer(operation_id, steer_index, model_call_index)
                .await
                .map_err(agent_error)
        })
    }

    fn continuation<'a>(
        &'a self,
        operation_id: String,
    ) -> ExecutionFuture<'a, AgentResult<Option<ExecutionContinuation>>> {
        Box::pin(async move {
            let owner = &self.owner;
            match owner
                .continuation(operation_id)
                .await
                .map_err(agent_error)?
            {
                Some(value) => {
                    let (continuation, keys) =
                        crate::context::load_continuation(owner.into(), value)
                            .await
                            .map_err(agent_error)?;
                    self.remember(keys)?;
                    Ok(Some(continuation))
                }
                None => Ok(None),
            }
        })
    }

    fn advance<'a>(
        &'a self,
        operation_id: String,
        continuation: ExecutionContinuation,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            let prepared = {
                let known = self.context_records.lock().map_err(|_| {
                    NanocodexError::InvalidExecutionPolicy("context cache poisoned".into())
                })?;
                crate::context::prepare_continuation(continuation, &known).map_err(agent_error)?
            };
            self.owner
                .advance(operation_id, prepared.payload)
                .await
                .map_err(agent_error)?;
            self.remember(prepared.keys)
        })
    }

    fn begin_step<'a>(
        &'a self,
        operation_id: String,
        step_id: String,
        kind: String,
        input_json: String,
    ) -> ExecutionFuture<'a, AgentResult<ExecutionStepAdmission>> {
        Box::pin(async move {
            let input = raw(input_json)?;
            match self
                .owner
                .begin_step(operation_id, step_id, kind, &input)
                .await
            {
                Ok(BeginStep::Execute) => Ok(ExecutionStepAdmission::Execute),
                Ok(BeginStep::Replay(output)) => Ok(ExecutionStepAdmission::Replay(
                    output.json().map_err(agent_error)?.to_owned(),
                )),
                Err(error) => Err(agent_error(error)),
            }
        })
    }

    fn complete_step<'a>(
        &'a self,
        operation_id: String,
        step_id: String,
        output_json: String,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            let output = raw(output_json)?;
            self.owner
                .complete_step(operation_id, step_id, &output)
                .await
                .map_err(agent_error)
        })
    }

    fn complete<'a>(
        &'a self,
        operation_id: String,
        snapshot: SessionSnapshot,
        output: ExecutionOutput,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            let prepared = self.prepare_snapshot(snapshot)?;
            self.owner
                .complete(operation_id, prepared.payload, &output)
                .await
                .map_err(agent_error)?;
            self.remember(prepared.keys)
        })
    }

    fn fail_attempt<'a>(
        &'a self,
        operation_id: String,
        error: String,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.owner
                .fail_attempt(operation_id, error)
                .await
                .map_err(agent_error)
        })
    }

    fn fail<'a>(
        &'a self,
        operation_id: String,
        snapshot: SessionSnapshot,
        error: String,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            let prepared = self.prepare_snapshot(snapshot)?;
            self.owner
                .fail(operation_id, prepared.payload, error)
                .await
                .map_err(agent_error)?;
            self.remember(prepared.keys)
        })
    }
}

/// A fork's own durable state, opened in the parent's store on first use.
///
/// [`ExecutionPolicy::branch`] runs synchronously before the child starts, so
/// the child state is acquired lazily by the child's first policy call.
struct LazyExecution {
    store: SharedStore,
    record: SessionRecord,
    ready: tokio::sync::OnceCell<DurableExecution>,
}

impl LazyExecution {
    fn new(store: SharedStore, record: SessionRecord) -> Self {
        Self {
            store,
            record,
            ready: tokio::sync::OnceCell::new(),
        }
    }

    async fn get(&self) -> AgentResult<&DurableExecution> {
        self.ready
            .get_or_try_init(|| async {
                let state = DurableSession::open_shared(
                    self.store.clone(),
                    self.record.session_id.clone(),
                    None,
                )
                .await
                .map_err(agent_error)?;
                let record = state
                    .describe(self.record.clone())
                    .await
                    .map_err(agent_error)?;
                let (owner, _) = state.acquire_agent().await.map_err(agent_error)?;
                Ok(DurableExecution::ready(
                    owner,
                    record.session_id.clone(),
                    Some(Branches {
                        store: self.store.clone(),
                        record: Some(record),
                    }),
                ))
            })
            .await
    }
}

impl ExecutionPolicy for LazyExecution {
    fn durable_state_id(&self) -> Option<String> {
        Some(self.record.session_id.clone())
    }

    fn branch(
        &self,
        child: &nanocodex_agent::SessionInfo,
    ) -> AgentResult<Option<Arc<dyn ExecutionPolicy>>> {
        let branches = Branches {
            store: self.store.clone(),
            record: Some(self.record.clone()),
        };
        Ok(Some(Arc::new(Self::new(
            self.store.clone(),
            branches.child_record(child)?,
        ))))
    }

    fn recover_failure<'a>(
        &'a self,
        operation_id: String,
        error: NanocodexError,
    ) -> ExecutionFuture<'a, NanocodexError> {
        Box::pin(async move {
            match self.get().await {
                Ok(policy) => policy.recover_failure(operation_id, error).await,
                Err(_) => error,
            }
        })
    }

    fn shutdown<'a>(&'a self) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            match self.ready.get() {
                Some(policy) => policy.shutdown().await,
                None => Ok(()),
            }
        })
    }

    fn commit_checkpoint<'a>(
        &'a self,
        snapshot: SessionSnapshot,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move { self.get().await?.commit_checkpoint(snapshot).await })
    }

    fn admit<'a>(
        &'a self,
        operation_id: String,
        input_json: String,
    ) -> ExecutionFuture<'a, AgentResult<ExecutionAdmission>> {
        Box::pin(async move { self.get().await?.admit(operation_id, input_json).await })
    }

    fn admit_automatic<'a>(
        &'a self,
        candidate_operation_id: String,
        input_json: String,
    ) -> ExecutionFuture<'a, AgentResult<(String, ExecutionAdmission)>> {
        Box::pin(async move {
            self.get()
                .await?
                .admit_automatic(candidate_operation_id, input_json)
                .await
        })
    }

    fn release<'a>(&'a self, operation_id: String) -> ExecutionFuture<'a, ()> {
        Box::pin(async move {
            if let Some(policy) = self.ready.get() {
                policy.release(operation_id).await;
            }
        })
    }

    fn cancel<'a>(
        &'a self,
        operation_id: String,
        snapshot: Option<SessionSnapshot>,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move { self.get().await?.cancel(operation_id, snapshot).await })
    }

    fn begin_attempt<'a>(&'a self, operation_id: String) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move { self.get().await?.begin_attempt(operation_id).await })
    }

    fn accept_steer<'a>(
        &'a self,
        operation_id: String,
        accepted_after_model_call_index: u32,
        input_json: String,
    ) -> ExecutionFuture<'a, AgentResult<u32>> {
        Box::pin(async move {
            self.get()
                .await?
                .accept_steer(operation_id, accepted_after_model_call_index, input_json)
                .await
        })
    }

    fn supports_steer_receipts(&self) -> bool {
        true
    }

    fn accept_identified_steer<'a>(
        &'a self,
        operation_id: String,
        message_id: String,
        accepted_after_model_call_index: u32,
        input_json: String,
        capacity_available: bool,
    ) -> ExecutionFuture<'a, AgentResult<Option<u32>>> {
        Box::pin(async move {
            self.get()
                .await?
                .accept_identified_steer(
                    operation_id,
                    message_id,
                    accepted_after_model_call_index,
                    input_json,
                    capacity_available,
                )
                .await
        })
    }

    fn retained_steers<'a>(
        &'a self,
        operation_id: String,
    ) -> ExecutionFuture<'a, AgentResult<Vec<ExecutionSteer>>> {
        Box::pin(async move { self.get().await?.retained_steers(operation_id).await })
    }

    fn retained_identified_steers<'a>(
        &'a self,
        operation_id: String,
    ) -> ExecutionFuture<'a, AgentResult<Vec<IdentifiedExecutionSteer>>> {
        Box::pin(async move {
            self.get()
                .await?
                .retained_identified_steers(operation_id)
                .await
        })
    }

    fn withdraw_steer<'a>(
        &'a self,
        operation_id: String,
        steer_index: u32,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.get()
                .await?
                .withdraw_steer(operation_id, steer_index)
                .await
        })
    }

    fn bind_steer<'a>(
        &'a self,
        operation_id: String,
        steer_index: u32,
        model_call_index: u32,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.get()
                .await?
                .bind_steer(operation_id, steer_index, model_call_index)
                .await
        })
    }

    fn continuation<'a>(
        &'a self,
        operation_id: String,
    ) -> ExecutionFuture<'a, AgentResult<Option<ExecutionContinuation>>> {
        Box::pin(async move { self.get().await?.continuation(operation_id).await })
    }

    fn advance<'a>(
        &'a self,
        operation_id: String,
        continuation: ExecutionContinuation,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move { self.get().await?.advance(operation_id, continuation).await })
    }

    fn begin_step<'a>(
        &'a self,
        operation_id: String,
        step_id: String,
        kind: String,
        input_json: String,
    ) -> ExecutionFuture<'a, AgentResult<ExecutionStepAdmission>> {
        Box::pin(async move {
            self.get()
                .await?
                .begin_step(operation_id, step_id, kind, input_json)
                .await
        })
    }

    fn complete_step<'a>(
        &'a self,
        operation_id: String,
        step_id: String,
        output_json: String,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.get()
                .await?
                .complete_step(operation_id, step_id, output_json)
                .await
        })
    }

    fn complete<'a>(
        &'a self,
        operation_id: String,
        snapshot: SessionSnapshot,
        output: ExecutionOutput,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.get()
                .await?
                .complete(operation_id, snapshot, output)
                .await
        })
    }

    fn fail_attempt<'a>(
        &'a self,
        operation_id: String,
        error: String,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move { self.get().await?.fail_attempt(operation_id, error).await })
    }

    fn fail<'a>(
        &'a self,
        operation_id: String,
        snapshot: SessionSnapshot,
        error: String,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move { self.get().await?.fail(operation_id, snapshot, error).await })
    }
}

async fn map_admission(
    owner: &DurableOwner,
    admission: Admission<crate::context::Snapshot, ExecutionOutput>,
) -> AgentResult<ExecutionAdmission> {
    Ok(match admission {
        Admission::Accepted => ExecutionAdmission::Execute,
        Admission::Pending => ExecutionAdmission::Resume,
        Admission::Completed { checkpoint, output } => ExecutionAdmission::Completed {
            snapshot: crate::context::restore_snapshot(owner.into(), checkpoint)
                .await
                .map_err(agent_error)?,
            output,
        },
        Admission::Failed { checkpoint, error } => ExecutionAdmission::Failed {
            snapshot: crate::context::restore_snapshot(owner.into(), checkpoint)
                .await
                .map_err(agent_error)?,
            error,
        },
        Admission::Cancelled => ExecutionAdmission::Cancelled,
    })
}

fn raw(json: String) -> AgentResult<Box<RawValue>> {
    RawValue::from_string(json).map_err(NanocodexError::ExecutionPayload)
}

pub(crate) fn agent_error(error: Error) -> NanocodexError {
    if matches!(error, Error::SteerQueueFull) {
        return NanocodexError::SteerQueueFull;
    }
    if matches!(
        error,
        Error::SteerConflict { .. } | Error::SteerWithdrawn { .. }
    ) {
        return NanocodexError::InvalidRequest(error.to_string());
    }
    let disposition = match &error {
        Error::Store(crate::StoreError::NotCommitted(_))
        | Error::OperationBlocked { .. }
        | Error::OperationActive { .. } => ExecutionPolicyDisposition::Retry,
        Error::Store(
            crate::StoreError::Fenced
            | crate::StoreError::Conflict { .. }
            | crate::StoreError::Backend(_),
        )
        | Error::ModelOwnerFenced
        | Error::DriverStopped => ExecutionPolicyDisposition::Reopen,
        _ => ExecutionPolicyDisposition::Fatal,
    };
    NanocodexError::execution_policy_with_disposition("durability", disposition, error)
}

#[cfg(test)]
mod tests {
    use nanocodex_agent::ExecutionPolicyDisposition;

    use super::*;

    #[tokio::test]
    async fn corruption_is_fatal_even_when_an_operation_is_pending() {
        let state = DurableSession::open(crate::MemoryStore::new().unwrap(), "corrupt")
            .await
            .unwrap();
        let (owner, _) = state.acquire_agent().await.unwrap();
        owner
            .admit_typed::<_, u32, String>("turn".into(), &"input")
            .await
            .unwrap();
        let policy = DurableExecution::ready(owner, "test".into(), None);
        let failure = policy
            .recover_failure(
                "turn".into(),
                agent_error(Error::InvalidState("missing payload record".into())),
            )
            .await;
        assert_eq!(
            failure.execution_policy_disposition(),
            Some(ExecutionPolicyDisposition::Fatal)
        );
        policy.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failure_classification_follows_settlement_instead_of_error_text() {
        let state = DurableSession::open(crate::MemoryStore::new().unwrap(), "settlement")
            .await
            .unwrap();
        let (owner, _) = state.acquire_agent().await.unwrap();
        owner
            .admit_typed::<_, u32, String>("first".into(), &"input")
            .await
            .unwrap();
        owner.begin_attempt("first".into()).await.unwrap();
        let policy = DurableExecution::ready(owner, "test".into(), None);
        let failure = policy
            .recover_failure(
                "first".into(),
                NanocodexError::MalformedResponse {
                    detail: "failure before a safe checkpoint",
                },
            )
            .await;
        assert_eq!(
            failure.execution_policy_disposition(),
            Some(ExecutionPolicyDisposition::Retry)
        );

        policy
            .owner
            .fail(
                "first".into(),
                crate::EncodedPayload::encode(&1_u32).unwrap(),
                "transport failed and turn was cancelled".into(),
            )
            .await
            .unwrap();
        let failure = policy
            .recover_failure("first".into(), NanocodexError::TurnStopped)
            .await;
        assert!(matches!(
            failure,
            NanocodexError::ReplayedExecutionFailed(_)
        ));
        assert_eq!(failure.execution_policy_disposition(), None);

        let owner = &policy.owner;
        owner
            .admit_typed::<_, u32, String>("second".into(), &"input")
            .await
            .unwrap();
        owner.begin_attempt("second".into()).await.unwrap();
        owner
            .complete(
                "second".into(),
                crate::EncodedPayload::encode(&2_u32).unwrap(),
                &"answer",
            )
            .await
            .unwrap();
        let failure = policy
            .recover_failure("second".into(), NanocodexError::TurnStopped)
            .await;
        assert_eq!(
            failure.execution_policy_disposition(),
            Some(ExecutionPolicyDisposition::Retry),
            "lost result delivery must replay the completed receipt"
        );
        policy.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pending_failure_identifies_the_oldest_operation_that_needs_recovery() {
        let state = DurableSession::open(crate::MemoryStore::new().unwrap(), "blocked")
            .await
            .unwrap();
        let (owner, _) = state.acquire_agent().await.unwrap();
        for id in ["older", "newer"] {
            owner
                .admit_typed::<_, u32, String>(id.into(), &"input")
                .await
                .unwrap();
        }
        let policy = DurableExecution::ready(owner, "test".into(), None);
        let failure = policy
            .recover_failure("newer".into(), NanocodexError::TurnStopped)
            .await;
        assert_eq!(
            failure.execution_policy_disposition(),
            Some(ExecutionPolicyDisposition::Retry)
        );
        let NanocodexError::ExecutionPolicy { source, .. } = failure else {
            panic!("missing recovery policy")
        };
        assert!(
            matches!(source.downcast_ref::<Error>(), Some(Error::OperationBlocked { pending_id, .. }) if pending_id == "older")
        );
        policy.shutdown().await.unwrap();
    }

    #[test]
    fn durability_errors_preserve_their_required_recovery_action() {
        let cases = [
            (
                Error::Store(crate::StoreError::NotCommitted("retry".to_owned())),
                ExecutionPolicyDisposition::Retry,
            ),
            (
                Error::Store(crate::StoreError::Fenced),
                ExecutionPolicyDisposition::Reopen,
            ),
            (
                Error::InvalidState("broken".to_owned()),
                ExecutionPolicyDisposition::Fatal,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(
                agent_error(error).execution_policy_disposition(),
                Some(expected)
            );
        }
    }
}
