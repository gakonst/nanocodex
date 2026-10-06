//! Claude-native adapter for the same durable owner and execution state machine.

use std::sync::Arc;

use nanocodex_agent::Result as AgentResult;
use nanocodex_claude::{
    ClaudeBuilder,
    execution::{Admission as ClaudeAdmission, ClaudeExecutionPolicy, PolicyFuture, Step},
};
use serde_json::Value;

use crate::{
    Admission, BeginStep, DurableAgentExt, DurableSession, EncodedPayload, agent::agent_error,
    session::DurableOwner,
};

impl DurableAgentExt for ClaudeBuilder {
    async fn durability(self, state: DurableSession) -> AgentResult<Self> {
        let state_id = state.state_id().to_owned();
        let (owner, checkpoint) = state.acquire_agent().await.map_err(agent_error)?;
        let checkpoint = checkpoint
            .map(|value| value.decode::<Value>().map_err(agent_error))
            .transpose()?;
        self.execution_policy(Arc::new(ClaudeExecution { state_id, owner }), checkpoint)
    }
}

struct ClaudeExecution {
    state_id: String,
    owner: DurableOwner,
}

impl ClaudeExecutionPolicy for ClaudeExecution {
    fn accept_steer(
        &self,
        id: String,
        accepted_after_model_call_index: u32,
        message_id: Option<String>,
        input: Value,
        capacity: bool,
    ) -> PolicyFuture<'_, Option<u32>> {
        Box::pin(async move {
            // Preserve the public Prompt serialization order used by browser receipt hashes.
            let input: nanocodex_agent::Prompt =
                serde_json::from_value(input).map_err(|error| {
                    nanocodex_agent::NanocodexError::InvalidRequest(error.to_string())
                })?;
            self.owner
                .accept_steer(
                    id,
                    accepted_after_model_call_index,
                    &input,
                    message_id,
                    capacity,
                )
                .await
                .map_err(agent_error)
        })
    }
    fn retained_steers(
        &self,
        id: String,
    ) -> PolicyFuture<'_, Vec<nanocodex_claude::execution::RetainedSteer>> {
        Box::pin(async move {
            self.owner
                .retained_steers(id)
                .await
                .map_err(agent_error)?
                .into_iter()
                .map(|steer| {
                    Ok(nanocodex_claude::execution::RetainedSteer {
                        index: steer.index,
                        message_id: steer.state.message_id,
                        input: steer.state.input.decode().map_err(agent_error)?,
                        model_call_index: steer.state.model_call_index,
                    })
                })
                .collect()
        })
    }
    fn bind_steer(&self, id: String, index: u32, model_call_index: u32) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.owner
                .bind_steer(id, index, model_call_index)
                .await
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

    fn state_id(&self) -> &str {
        &self.state_id
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
