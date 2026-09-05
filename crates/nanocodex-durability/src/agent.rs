use std::sync::{Arc, Mutex};

use nanocodex_agent::{
    ExecutionPolicyDisposition, NanocodexBuilder, NanocodexError, Result as AgentResult,
    execution::{
        ExecutionAdmission, ExecutionFuture, ExecutionOutput, ExecutionPolicy, ExecutionSteer,
        ExecutionStepAdmission,
    },
    session::SessionSnapshot,
};
use serde_json::value::RawValue;
use tokio::sync::OnceCell;

use crate::{Admission, BeginStep, DurableSession, Error, OperationStatus, session::DurableOwner};

/// Fluent builder extension that attaches portable durability to an agent.
pub trait DurableAgentExt: Sized {
    /// Restores the state's latest checkpoint and installs its execution
    /// policy at the agent's neutral lifecycle seam.
    fn durability(self, state: DurableSession) -> impl Future<Output = AgentResult<Self>>;
}

impl<F> DurableAgentExt for NanocodexBuilder<F> {
    async fn durability(self, state: DurableSession) -> AgentResult<Self> {
        let state_id = state.state_id().to_owned();
        let mut builder = self;
        let (owner, checkpoint) = state.acquire_agent().await.map_err(agent_error)?;
        if let Some(checkpoint) = checkpoint {
            let restored = checkpoint
                .decode::<SessionSnapshot>()
                .map_err(agent_error)?;
            if let Some(configured) = builder.resume_snapshot()
                && serde_json::to_string(configured)
                    .map_err(|error| NanocodexError::InvalidSessionSnapshot(error.to_string()))?
                    != checkpoint.json()
            {
                return Err(NanocodexError::InvalidSessionSnapshot(
                    "configured resume snapshot does not match the durability state".to_owned(),
                ));
            }
            builder = builder.resume(restored);
        } else {
            builder = builder.default_prompt_cache_key(state_id);
        }
        let owner = Arc::new(Mutex::new(Some(owner)));
        let child_states = state.clone();
        Ok(builder
            .execution_policy_factory(move || {
                let owner = owner
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
                let policy: Arc<dyn ExecutionPolicy> =
                    Arc::new(DurableExecution::ready(owner));
                Ok(policy)
            })
            .spawned_execution_policy_factory(move |session_id| {
                let policy: Arc<dyn ExecutionPolicy> = Arc::new(DurableExecution::lazy(
                    child_states.clone(),
                    session_id.to_owned(),
                ));
                Ok(policy)
            }))
    }
}

struct DurableExecution {
    owner: DurableExecutionOwner,
}

enum DurableExecutionOwner {
    Ready(DurableOwner),
    Lazy {
        states: DurableSession,
        state_id: String,
        owner: OnceCell<DurableOwner>,
    },
}

impl DurableExecution {
    const fn ready(owner: DurableOwner) -> Self {
        Self {
            owner: DurableExecutionOwner::Ready(owner),
        }
    }

    fn lazy(states: DurableSession, state_id: String) -> Self {
        Self {
            owner: DurableExecutionOwner::Lazy {
                states,
                state_id,
                owner: OnceCell::new(),
            },
        }
    }

    async fn owner(&self) -> AgentResult<&DurableOwner> {
        match &self.owner {
            DurableExecutionOwner::Ready(owner) => Ok(owner),
            DurableExecutionOwner::Lazy {
                states,
                state_id,
                owner,
            } => {
                owner
                    .get_or_try_init(|| async {
                        let state = states
                            .open_agent_state(state_id.clone())
                            .await
                            .map_err(agent_error)?;
                        let (owner, checkpoint) =
                            state.acquire_agent().await.map_err(agent_error)?;
                        if checkpoint.is_some() {
                            return Err(NanocodexError::InvalidExecutionPolicy(
                                "a fresh spawned agent found an existing durability checkpoint"
                                    .to_owned(),
                            ));
                        }
                        Ok(owner)
                    })
                    .await
            }
        }
    }

    fn initialized_owner(&self) -> Option<&DurableOwner> {
        match &self.owner {
            DurableExecutionOwner::Ready(owner) => Some(owner),
            DurableExecutionOwner::Lazy { owner, .. } => owner.get(),
        }
    }
}

impl ExecutionPolicy for DurableExecution {
    fn recover_failure<'a>(
        &'a self,
        operation_id: String,
        error: NanocodexError,
    ) -> ExecutionFuture<'a, NanocodexError> {
        Box::pin(async move {
            if error.execution_policy_disposition() == Some(ExecutionPolicyDisposition::Reopen) {
                return error;
            }
            let owner = match self.owner().await {
                Ok(owner) => owner,
                Err(error) => return error,
            };
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
        Box::pin(async move {
            let Some(owner) = self.initialized_owner() else {
                return Ok(());
            };
            owner.shutdown().await.map_err(agent_error)
        })
    }

    fn commit_checkpoint<'a>(
        &'a self,
        snapshot: SessionSnapshot,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.owner()
                .await?
                .commit_checkpoint(&snapshot)
                .await
                .map_err(agent_error)
        })
    }

    fn admit<'a>(
        &'a self,
        operation_id: String,
        input_json: String,
    ) -> ExecutionFuture<'a, AgentResult<ExecutionAdmission>> {
        Box::pin(async move {
            let input = raw(input_json)?;
            self.owner()
                .await?
                .admit_typed::<_, SessionSnapshot, ExecutionOutput>(operation_id, &input)
                .await
                .map(map_admission)
                .map_err(agent_error)
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
                .owner()
                .await?
                .admit_automatic_typed::<_, SessionSnapshot, ExecutionOutput>(
                    candidate_operation_id,
                    &input,
                )
                .await
                .map_err(agent_error)?;
            let (operation_id, admission) = admission.into_parts();
            Ok((operation_id, map_admission(admission)))
        })
    }

    fn release<'a>(&'a self, operation_id: String) -> ExecutionFuture<'a, ()> {
        Box::pin(async move {
            if let Some(owner) = self.initialized_owner() {
                let _ = owner.release_claim(operation_id).await;
            }
        })
    }

    fn cancel<'a>(
        &'a self,
        operation_id: String,
        snapshot: Option<SessionSnapshot>,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.owner()
                .await?
                .cancel(operation_id, snapshot.as_ref())
                .await
                .map_err(agent_error)
        })
    }

    fn begin_attempt<'a>(&'a self, operation_id: String) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.owner()
                .await?
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
            self.owner()
                .await?
                .accept_steer(operation_id, accepted_after_model_call_index, &input)
                .await
                .map_err(agent_error)
        })
    }

    fn retained_steers<'a>(
        &'a self,
        operation_id: String,
    ) -> ExecutionFuture<'a, AgentResult<Vec<ExecutionSteer>>> {
        Box::pin(async move {
            self.owner()
                .await?
                .retained_steers(operation_id)
                .await
                .map(|steers| {
                    steers
                        .into_iter()
                        .map(|steer| ExecutionSteer {
                            index: steer.index,
                            accepted_after_model_call_index: steer
                                .state
                                .accepted_after_model_call_index,
                            model_call_index: steer.state.model_call_index,
                            input_json: steer.state.input.json().to_owned(),
                        })
                        .collect()
                })
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
            self.owner()
                .await?
                .bind_steer(operation_id, steer_index, model_call_index)
                .await
                .map_err(agent_error)
        })
    }

    fn retained_step_input<'a>(
        &'a self,
        operation_id: String,
        step_id: String,
        kind: String,
    ) -> ExecutionFuture<'a, AgentResult<Option<String>>> {
        Box::pin(async move {
            self.owner()
                .await?
                .retained_step_input(operation_id, step_id, kind)
                .await
                .map(|input| input.map(|input| input.json().to_owned()))
                .map_err(agent_error)
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
                .owner()
                .await?
                .begin_step(operation_id, step_id, kind, &input)
                .await
            {
                Ok(BeginStep::Execute) => Ok(ExecutionStepAdmission::Execute),
                Ok(BeginStep::Replay(output)) => {
                    Ok(ExecutionStepAdmission::Replay(output.json().to_owned()))
                }
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
            self.owner()
                .await?
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
            self.owner()
                .await?
                .complete(operation_id, &snapshot, &output)
                .await
                .map_err(agent_error)
        })
    }

    fn fail_attempt<'a>(
        &'a self,
        operation_id: String,
        error: String,
    ) -> ExecutionFuture<'a, AgentResult<()>> {
        Box::pin(async move {
            self.owner()
                .await?
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
            self.owner()
                .await?
                .fail(operation_id, &snapshot, error)
                .await
                .map_err(agent_error)
        })
    }
}

fn map_admission(admission: Admission<SessionSnapshot, ExecutionOutput>) -> ExecutionAdmission {
    match admission {
        Admission::Accepted | Admission::Pending => ExecutionAdmission::Execute,
        Admission::Completed { checkpoint, output } => ExecutionAdmission::Completed {
            snapshot: checkpoint,
            output,
        },
        Admission::Failed { checkpoint, error } => ExecutionAdmission::Failed {
            snapshot: checkpoint,
            error,
        },
        Admission::Cancelled => ExecutionAdmission::Cancelled,
    }
}

fn raw(json: String) -> AgentResult<Box<RawValue>> {
    RawValue::from_string(json).map_err(NanocodexError::ExecutionPayload)
}

fn agent_error(error: Error) -> NanocodexError {
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
        let policy = DurableExecution::ready(owner);
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
            .owner()
            .await
            .unwrap()
            .fail(
                "first".into(),
                &1_u32,
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

        let owner = policy.owner().await.unwrap();
        owner
            .admit_typed::<_, u32, String>("second".into(), &"input")
            .await
            .unwrap();
        owner.begin_attempt("second".into()).await.unwrap();
        owner
            .complete("second".into(), &2_u32, &"answer")
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
        let policy = DurableExecution::ready(owner);
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
