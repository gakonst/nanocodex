//! Claude-native adapter for the same durable owner and execution state machine.

use std::sync::Arc;

use nanocodex_agent::Result as AgentResult;
use nanocodex_claude::{
    ClaudeBuilder,
    execution::{
        Admission as ClaudeAdmission, ClaudeExecutionPolicy, ClaudeSteer, PolicyFuture, Step,
    },
};
use serde_json::{Value, value::RawValue};

use crate::{
    Admission, BeginStep, DurableAgentExt, DurableSession, EncodedPayload, SessionRecord,
    agent::{Branches, agent_error},
    session::DurableOwner,
    shared_store::SharedStore,
};

impl DurableAgentExt for ClaudeBuilder {
    async fn durability(self, state: DurableSession) -> AgentResult<Self> {
        let state_id = state.state_id().to_owned();
        let journal = state.child_journal();
        let branches = Branches {
            store: state.shared_store(),
            record: state.record().await.map_err(agent_error)?,
        };
        // The catalog record owns provenance: a branch's copied checkpoint
        // still carries its source's lineage.
        let builder = match &branches.record {
            Some(record) => self.lineage(record.lineage.clone()),
            None => self,
        };
        let (owner, checkpoint) = state.acquire_agent().await.map_err(agent_error)?;
        let checkpoint = checkpoint
            .map(|value| value.decode::<Value>().map_err(agent_error))
            .transpose()?;
        builder.child_journal(journal).execution_policy(
            Arc::new(ClaudeExecution {
                state_id,
                owner,
                branches,
            }),
            checkpoint,
        )
    }
}

struct ClaudeExecution {
    state_id: String,
    owner: DurableOwner,
    branches: Branches,
}

/// A fork's own durable state, opened in the parent's store on first use, so
/// [`ClaudeExecutionPolicy::branch`] can stay synchronous.
struct LazyClaudeExecution {
    store: SharedStore,
    record: SessionRecord,
    ready: tokio::sync::OnceCell<ClaudeExecution>,
    /// The opened state already held a checkpoint, as for a restored child.
    reopened: std::sync::atomic::AtomicBool,
    /// This policy wrote the state's first checkpoint.
    initialized: std::sync::atomic::AtomicBool,
}

impl LazyClaudeExecution {
    fn new(store: SharedStore, record: SessionRecord) -> Self {
        Self {
            store,
            record,
            ready: tokio::sync::OnceCell::new(),
            reopened: std::sync::atomic::AtomicBool::new(false),
            initialized: std::sync::atomic::AtomicBool::new(false),
        }
    }

    async fn get(&self) -> AgentResult<&ClaudeExecution> {
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
                let (owner, checkpoint) = state.acquire_agent().await.map_err(agent_error)?;
                self.reopened.store(
                    checkpoint.is_some(),
                    std::sync::atomic::Ordering::SeqCst,
                );
                Ok(ClaudeExecution {
                    state_id: record.session_id.clone(),
                    owner,
                    branches: Branches {
                        store: self.store.clone(),
                        record: Some(record),
                    },
                })
            })
            .await
    }
}

fn branch_child(
    branches: &Branches,
    child: &nanocodex_agent::SessionInfo,
) -> AgentResult<Option<Arc<dyn ClaudeExecutionPolicy>>> {
    if branches.record.is_none() {
        // Sessions opened without catalog metadata keep ephemeral forks.
        return Ok(None);
    }
    Ok(Some(Arc::new(LazyClaudeExecution::new(
        branches.store.clone(),
        branches.child_record(child)?,
    ))))
}

impl ClaudeExecutionPolicy for LazyClaudeExecution {
    fn state_id(&self) -> &str {
        &self.record.session_id
    }

    fn admit(
        &self,
        id: String,
        input: Value,
        automatic: bool,
    ) -> PolicyFuture<'_, (String, ClaudeAdmission)> {
        Box::pin(async move { self.get().await?.admit(id, input, automatic).await })
    }

    fn begin_attempt(&self, id: String) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.get().await?.begin_attempt(id).await })
    }

    fn supports_steering(&self) -> bool {
        true
    }

    fn accept_steer(
        &self,
        id: String,
        message_id: Option<String>,
        after: u32,
        input_json: String,
        capacity: bool,
    ) -> PolicyFuture<'_, Option<u32>> {
        Box::pin(async move {
            self.get()
                .await?
                .accept_steer(id, message_id, after, input_json, capacity)
                .await
        })
    }

    fn retained_steers(&self, id: String) -> PolicyFuture<'_, Vec<ClaudeSteer>> {
        Box::pin(async move { self.get().await?.retained_steers(id).await })
    }

    fn withdraw_steer(&self, id: String, index: u32) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.get().await?.withdraw_steer(id, index).await })
    }

    fn bind_steer(&self, id: String, index: u32, boundary: u32) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.get().await?.bind_steer(id, index, boundary).await })
    }

    fn continuation(&self, id: String) -> PolicyFuture<'_, Option<Value>> {
        Box::pin(async move { self.get().await?.continuation(id).await })
    }

    fn advance(&self, id: String, state: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.get().await?.advance(id, state).await })
    }

    fn begin_step(
        &self,
        id: String,
        step_id: String,
        kind: String,
        input: Value,
    ) -> PolicyFuture<'_, Step> {
        Box::pin(async move { self.get().await?.begin_step(id, step_id, kind, input).await })
    }

    fn complete_step(&self, id: String, step_id: String, output: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.get().await?.complete_step(id, step_id, output).await })
    }

    fn complete(&self, id: String, checkpoint: Value, output: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.get().await?.complete(id, checkpoint, output).await })
    }

    fn fail(&self, id: String, checkpoint: Value, error: String) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.get().await?.fail(id, checkpoint, error).await })
    }

    fn cancel(&self, id: String, checkpoint: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.get().await?.cancel(id, checkpoint).await })
    }

    fn release(&self, id: String) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            match self.ready.get() {
                Some(policy) => policy.release(id).await,
                None => Ok(()),
            }
        })
    }

    fn shutdown(&self) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            match self.ready.get() {
                Some(policy) => policy.shutdown().await,
                None => Ok(()),
            }
        })
    }

    fn checkpoint(&self, state: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.get().await?.checkpoint(state).await })
    }

    fn initial_checkpoint(&self, state: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            // Opening the state records the child in the catalog; a restored
            // child keeps the history it already holds.
            let policy = self.get().await?;
            if self.reopened.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(());
            }
            policy.checkpoint(state).await?;
            self.initialized
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
    }

    fn discard_initial(&self) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            let Some(policy) = self.ready.get() else {
                return Ok(());
            };
            if !self.initialized.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(());
            }
            policy.owner.discard_unused().await.map_err(agent_error)
        })
    }

    fn branch(
        &self,
        child: &nanocodex_agent::SessionInfo,
    ) -> AgentResult<Option<Arc<dyn ClaudeExecutionPolicy>>> {
        branch_child(
            &Branches {
                store: self.store.clone(),
                record: Some(self.record.clone()),
            },
            child,
        )
    }
}

impl ClaudeExecutionPolicy for ClaudeExecution {
    fn state_id(&self) -> &str {
        &self.state_id
    }

    fn branch(
        &self,
        child: &nanocodex_agent::SessionInfo,
    ) -> AgentResult<Option<Arc<dyn ClaudeExecutionPolicy>>> {
        branch_child(&self.branches, child)
    }

    fn admit(
        &self,
        id: String,
        input: Value,
        automatic: bool,
    ) -> PolicyFuture<'_, (String, ClaudeAdmission)> {
        Box::pin(async move {
            let (id, admission) = if automatic {
                self.owner
                    .admit_automatic_typed::<_, Value, Value>(id, &input)
                    .await
                    .map_err(agent_error)?
                    .into_parts()
            } else {
                let admission = self
                    .owner
                    .admit_typed::<_, Value, Value>(id.clone(), &input)
                    .await
                    .map_err(agent_error)?;
                (id, admission)
            };
            Ok((id, map_admission(admission)))
        })
    }

    fn begin_attempt(&self, id: String) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.owner.begin_attempt(id).await.map_err(agent_error) })
    }

    fn supports_steering(&self) -> bool {
        true
    }

    fn accept_steer(
        &self,
        id: String,
        message_id: Option<String>,
        after: u32,
        input_json: String,
        capacity: bool,
    ) -> PolicyFuture<'_, Option<u32>> {
        Box::pin(async move {
            let input: Box<serde_json::value::RawValue> = serde_json::from_str(&input_json)
                .map_err(|error| {
                    nanocodex_agent::NanocodexError::InvalidRequest(error.to_string())
                })?;
            self.owner
                .accept_steer(id, after, &input, message_id, capacity)
                .await
                .map_err(agent_error)
        })
    }
    fn retained_steers(&self, id: String) -> PolicyFuture<'_, Vec<ClaudeSteer>> {
        Box::pin(async move {
            self.owner
                .retained_steers(id)
                .await
                .and_then(|steers| {
                    steers
                        .into_iter()
                        .map(|steer| {
                            Ok(ClaudeSteer {
                                message_id: steer.state.message_id,
                                index: steer.index,
                                accepted_after_model_call_index: steer
                                    .state
                                    .accepted_after_model_call_index,
                                model_call_index: steer.state.model_call_index,
                                input_json: steer.state.input.json()?.to_owned(),
                            })
                        })
                        .collect()
                })
                .map_err(agent_error)
        })
    }
    fn withdraw_steer(&self, id: String, index: u32) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.owner
                .withdraw_steer(id, index)
                .await
                .map_err(agent_error)
        })
    }
    fn bind_steer(&self, id: String, index: u32, boundary: u32) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.owner
                .bind_steer(id, index, boundary)
                .await
                .map_err(agent_error)
        })
    }
    fn continuation(&self, id: String) -> PolicyFuture<'_, Option<Value>> {
        Box::pin(async move {
            self.owner
                .continuation(id)
                .await
                .map_err(agent_error)?
                .map(|value| value.decode().map_err(agent_error))
                .transpose()
        })
    }

    fn advance_encoded(&self, id: String, state: Box<RawValue>) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            let state = EncodedPayload::encode(&*state).map_err(agent_error)?;
            self.owner.advance(id, state).await.map_err(agent_error)
        })
    }

    fn begin_step_encoded(
        &self,
        id: String,
        step_id: String,
        kind: String,
        input: Box<RawValue>,
    ) -> PolicyFuture<'_, Step> {
        Box::pin(async move {
            match self
                .owner
                .begin_step(id, step_id, kind, &*input)
                .await
                .map_err(agent_error)?
            {
                BeginStep::Execute => Ok(Step::Execute),
                BeginStep::Replay(value) => Ok(Step::Replay(value.decode().map_err(agent_error)?)),
            }
        })
    }

    fn advance(&self, id: String, state: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.owner
                .advance(id, payload(&state)?)
                .await
                .map_err(agent_error)
        })
    }

    fn begin_step(
        &self,
        id: String,
        step_id: String,
        kind: String,
        input: Value,
    ) -> PolicyFuture<'_, Step> {
        Box::pin(async move {
            match self
                .owner
                .begin_step(id, step_id, kind, &input)
                .await
                .map_err(agent_error)?
            {
                BeginStep::Execute => Ok(Step::Execute),
                BeginStep::Replay(value) => Ok(Step::Replay(value.decode().map_err(agent_error)?)),
            }
        })
    }

    fn complete_step(&self, id: String, step_id: String, output: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.owner
                .complete_step(id, step_id, &output)
                .await
                .map_err(agent_error)
        })
    }

    fn complete(&self, id: String, checkpoint: Value, output: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.owner
                .complete(id, payload(&checkpoint)?, &output)
                .await
                .map_err(agent_error)
        })
    }

    fn fail(&self, id: String, checkpoint: Value, error: String) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.owner
                .fail(id, payload(&checkpoint)?, error)
                .await
                .map_err(agent_error)
        })
    }

    fn cancel(&self, id: String, checkpoint: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.owner
                .cancel(id, Some(payload(&checkpoint)?))
                .await
                .map_err(agent_error)
        })
    }

    fn release(&self, id: String) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.owner.release_claim(id).await.map_err(agent_error) })
    }

    fn shutdown(&self) -> PolicyFuture<'_, ()> {
        Box::pin(async move { self.owner.shutdown().await.map_err(agent_error) })
    }

    fn checkpoint(&self, state: Value) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.owner
                .commit_checkpoint(payload(&state)?)
                .await
                .map_err(agent_error)
        })
    }
}

fn payload(value: &Value) -> AgentResult<EncodedPayload> {
    EncodedPayload::encode(value).map_err(agent_error)
}

fn map_admission(admission: Admission<Value, Value>) -> ClaudeAdmission {
    match admission {
        Admission::Accepted => ClaudeAdmission::Execute,
        Admission::Pending => ClaudeAdmission::Resume,
        Admission::Completed { checkpoint, output } => {
            ClaudeAdmission::Completed { checkpoint, output }
        }
        Admission::Failed { checkpoint, error } => ClaudeAdmission::Failed { checkpoint, error },
        Admission::Cancelled => ClaudeAdmission::Cancelled,
    }
}
