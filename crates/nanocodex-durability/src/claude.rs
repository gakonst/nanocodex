//! Claude-native adapter for the same durable owner and execution state machine.

use std::sync::Arc;

use nanocodex_agent::Result as AgentResult;
use nanocodex_claude::{
    ClaudeBuilder,
    execution::{
        Admission as ClaudeAdmission, ClaudeExecutionPolicy, PolicyFuture, RequestPreparation, Step,
    },
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
        let owner = Arc::new(owner);
        #[cfg(not(target_family = "wasm"))]
        let code_journal = Arc::new(crate::code_mode::DurableCodeJournal::new(
            owner.clone(),
            state,
        ));
        self.execution_policy(
            Arc::new(ClaudeExecution {
                settings: None,
                warm: None,
                state_id,
                owner,
                #[cfg(not(target_family = "wasm"))]
                code_journal,
            }),
            checkpoint,
        )
    }
}

struct ClaudeExecution {
    state_id: String,
    owner: Arc<DurableOwner>,
    #[cfg(not(target_family = "wasm"))]
    code_journal: Arc<crate::code_mode::DurableCodeJournal>,
    settings: Option<crate::request_policy::RequestPolicySettings>,
    warm: Option<(
        nanocodex_claude::ClaudeClient,
        crate::cache_warm::CacheWarmPolicy,
    )>,
}

impl crate::request_policy::DurableClaudeRequestExt for ClaudeBuilder {
    async fn durability_with_request_policy(
        self,
        state: DurableSession,
        settings: crate::request_policy::RequestPolicySettings,
    ) -> AgentResult<Self> {
        let state_id = state.state_id().to_owned();
        let (owner, checkpoint) = state.acquire_agent().await.map_err(agent_error)?;
        let checkpoint = checkpoint
            .map(|value| value.decode::<Value>().map_err(agent_error))
            .transpose()?;
        let owner = Arc::new(owner);
        #[cfg(not(target_family = "wasm"))]
        let code_journal = Arc::new(crate::code_mode::DurableCodeJournal::new(
            owner.clone(),
            state,
        ));
        self.execution_policy(
            Arc::new(ClaudeExecution {
                state_id,
                owner,
                settings: Some(settings),
                warm: None,
                #[cfg(not(target_family = "wasm"))]
                code_journal,
            }),
            checkpoint,
        )
    }
    async fn durability_with_request_policy_and_cache_warm(
        self,
        state: DurableSession,
        settings: crate::request_policy::RequestPolicySettings,
        client: nanocodex_claude::ClaudeClient,
        policy: crate::cache_warm::CacheWarmPolicy,
    ) -> AgentResult<Self> {
        policy.validate().map_err(agent_error)?;
        let state_id = state.state_id().to_owned();
        let (owner, checkpoint) = state.acquire_agent().await.map_err(agent_error)?;
        let checkpoint = checkpoint
            .map(|value| value.decode::<Value>().map_err(agent_error))
            .transpose()?;
        let owner = Arc::new(owner);
        #[cfg(not(target_family = "wasm"))]
        let code_journal = Arc::new(crate::code_mode::DurableCodeJournal::new(
            owner.clone(),
            state,
        ));
        self.execution_policy(
            Arc::new(ClaudeExecution {
                state_id,
                owner,
                settings: Some(settings),
                warm: Some((client, policy)),
                #[cfg(not(target_family = "wasm"))]
                code_journal,
            }),
            checkpoint,
        )
    }
}

impl ClaudeExecutionPolicy for ClaudeExecution {
    fn prepare_request(
        &self,
        operation: String,
        request_id: String,
        continuation: bool,
        state: Value,
        request: Value,
    ) -> PolicyFuture<'_, Option<RequestPreparation>> {
        Box::pin(async move {
            let Some(settings) = &self.settings else {
                return Ok(None);
            };
            if state["requests"].as_array().is_some_and(|requests| {
                requests
                    .iter()
                    .any(|entry| entry["request"]["request_id"] == request_id)
            }) {
                return settings
                    .prepare_claude(request_id, continuation, state, request)
                    .map(Some)
                    .map_err(agent_error);
            }
            let step = format!("prepare/{request_id}");
            // Configuration and virtual selection are frozen in the receipt. A
            // recovered turn consumes that receipt even if host settings changed.
            let input =
                serde_json::json!({"request":request, "state":state, "continuation":continuation});
            let prepared = match self
                .owner
                .begin_step(
                    operation.clone(),
                    step.clone(),
                    "request_policy".into(),
                    &input,
                    crate::ReplaySafety::Safe,
                )
                .await
                .map_err(agent_error)?
            {
                BeginStep::OutcomeUnknown => Err(agent_error(crate::Error::InvalidState(
                    "request preparation outcome is unknown".into(),
                ))),
                BeginStep::Replay(value) => {
                    let prepared: RequestPreparation = value.decode().map_err(agent_error)?;
                    // Current host authorization and limits still constrain a frozen receipt.
                    settings
                        .prepare_claude(request_id.clone(), continuation, prepared.state, request)
                        .map_err(agent_error)
                }
                BeginStep::Execute => {
                    let prepared = settings
                        .prepare_claude(request_id.clone(), continuation, state, request)
                        .map_err(agent_error)?;
                    self.owner
                        .complete_step(operation.clone(), step, &prepared)
                        .await
                        .map_err(agent_error)?;
                    Ok(prepared)
                }
            }?;
            if !continuation && let Some((client, policy)) = &self.warm {
                return crate::cache_warm::warm(
                    &self.owner,
                    &operation,
                    &request_id,
                    prepared,
                    client,
                    policy,
                )
                .await
                .map(Some)
                .map_err(agent_error);
            }
            Ok(Some(prepared))
        })
    }
    #[cfg(not(target_family = "wasm"))]
    fn code_mode_journal(
        &self,
    ) -> Option<Arc<dyn nanocodex_oai_tools::code_mode::CodeModeJournal>> {
        Some(self.code_journal.clone())
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

    fn advance_retaining(
        &self,
        id: String,
        state: Value,
        retained_steps: Vec<String>,
    ) -> PolicyFuture<'_, ()> {
        Box::pin(async move {
            self.owner
                .advance_retaining(id, payload(&state)?, retained_steps)
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
        self.begin_step_with_replay(id, step_id, kind, input, crate::ReplaySafety::Unsafe)
    }

    fn begin_step_with_replay(
        &self,
        id: String,
        step_id: String,
        kind: String,
        input: Value,
        replay_safety: crate::ReplaySafety,
    ) -> PolicyFuture<'_, Step> {
        Box::pin(async move {
            match self
                .owner
                .begin_step(
                    id.clone(),
                    step_id.clone(),
                    kind.clone(),
                    &input,
                    replay_safety,
                )
                .await
                .map_err(agent_error)?
            {
                BeginStep::OutcomeUnknown => Ok(Step::OutcomeUnknown),
                BeginStep::Execute => {
                    #[cfg(not(target_family = "wasm"))]
                    if kind == "tool" && input.get("name").and_then(Value::as_str) == Some("exec") {
                        self.code_journal
                            .bind(&id, &step_id, &input.to_string())
                            .map_err(agent_error)?;
                    }
                    Ok(Step::Execute)
                }
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
