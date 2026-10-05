//! Owned summary work. The immutable input is part of the existing execution
//! continuation; its completed output is an ordinary journal receipt.
use super::*;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct PendingCompaction {
    pub(super) cutoff: Vec<ResponseItem>,
    pub(super) after_model_call_index: u32,
    pub(super) active_context_tokens: u64,
    pub(super) auto_compact_token_limit: u64,
}

pub(super) struct CompactionWork {
    future: crate::agent::backend::BackendFuture<Result<RecordedCompactionResult>>,
    pub(super) result: Option<Result<RecordedCompactionResult>>,
}

pub(super) async fn progress(
    work: &mut Option<CompactionWork>,
) -> Result<RecordedCompactionResult> {
    match work {
        Some(work) if work.result.is_none() => work.future.as_mut().await,
        _ => std::future::pending().await,
    }
}

impl<S> ModelRun<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<nanocodex_oai_api::ResponseError>,
    S::Future: AgentSend + 'static,
{
    pub(super) async fn start_background(
        &mut self,
        factory: &ResponsesAttemptFactory,
    ) -> Result<()> {
        if self.background_work.is_some() {
            return Ok(());
        }
        let Some(pending) = &self.background_compaction else {
            return Ok(());
        };
        let step = format!("background-compaction-{}", pending.after_model_call_index);
        let steps = self.execution_steps.clone();
        let recovered = if let Some(steps) = &steps {
            match steps
                .begin::<_, RecordedCompactionOutcome>(&step, "compaction", pending)
                .await?
            {
                crate::agent::ExecutionStep::Replay(result) => Some(result),
                crate::agent::ExecutionStep::Execute => None,
                crate::agent::ExecutionStep::OutcomeUnknown => unreachable!("safe model effect"),
            }
        } else {
            None
        };
        let recovered = match recovered {
            Some(RecordedCompactionOutcome::Success(output)) => Some(Ok(output)),
            Some(RecordedCompactionOutcome::Failure {
                compaction_error,
                requires_session_stop,
                recovery,
            }) => Some(Err(NanocodexError::CompactionFailed {
                detail: compaction_error,
                requires_session_stop,
                recovery,
            })),
            None => None,
        };
        if let Some(result) = recovered {
            self.background_work = Some(CompactionWork {
                future: Box::pin(std::future::pending()),
                result: Some(result),
            });
            return Ok(());
        }
        if let Some(hook) = &self.before_compaction {
            use crate::execution::{BeforeCompactionRequest, CompactionReceipt};
            let preservation = format!("before-{step}");
            let operation = steps.as_ref().map_or_else(
                || factory.profile().turn_id(),
                |steps| steps.operation_id().to_owned(),
            );
            let request = BeforeCompactionRequest::from_history(
                format!("{}:{operation}:{preservation}", self.events.request_id()),
                self.events.request_id().to_owned(),
                self.provider_session_id.to_string(),
                &nanocodex_oai_api::responses::ResponseHistory::new(pending.cutoff.clone()),
            );
            let replay = if let Some(steps) = &steps {
                match steps
                    .begin::<_, CompactionReceipt>(&preservation, "before_compaction", &request)
                    .await?
                {
                    crate::agent::ExecutionStep::Replay(receipt) => {
                        receipt.validate()?;
                        true
                    }
                    crate::agent::ExecutionStep::Execute => false,
                    crate::agent::ExecutionStep::OutcomeUnknown => {
                        unreachable!("safe preservation effect")
                    }
                }
            } else {
                false
            };
            if !replay {
                let receipt = hook.preserve(request).await?;
                receipt.validate()?;
                if let Some(steps) = &steps {
                    steps.complete(&preservation, &receipt).await?;
                }
            }
        }
        let state = ManagedSessionState::new(pending.cutoff.clone());
        let mut history = state.compaction_request();
        history.fit_context_window(
            factory.profile().prefix(),
            self.config.context_window_tokens,
        );
        let request = factory
            .compaction(
                pending.after_model_call_index,
                &history,
                compaction::trigger(),
                self.model,
                self.thinking,
                self.fast_mode,
            )
            .with_independent_connection();
        self.events.emit(
            AgentEventKind::ModelCompactionStarted,
            CompactionStarted {
                after_model_call_index: pending.after_model_call_index,
                active_context_tokens: pending.active_context_tokens,
                auto_compact_token_limit: pending.auto_compact_token_limit,
                previous_response_id: None,
            },
        )?;
        self.stats.compactions += 1;
        let dispatched = self
            .client
            .dispatch(request)
            .await
            .map_err(|error| NanocodexError::Response(error.into()))?;
        let started = Instant::now();
        self.background_work = Some(CompactionWork {
            result: None,
            future: Box::pin(async move {
                let response = match dispatched
                    .await
                    .map_err(|error| NanocodexError::Response(error.into()))
                {
                    Ok(response) => response,
                    Err(error) => {
                        let requires_session_stop = error
                            .responses_error()
                            .is_some_and(|source| source.is_misalignment_policy_violation());
                        let recovery = if error.requires_image_repair() {
                            crate::error::CompactionRecovery::ReplaceRejectedImages
                        } else {
                            crate::error::CompactionRecovery::None
                        };
                        if let Some(steps) = &steps {
                            steps
                                .complete(
                                    &step,
                                    &RecordedCompactionOutcome::Failure {
                                        compaction_error: error.to_string(),
                                        requires_session_stop,
                                        recovery,
                                    },
                                )
                                .await?;
                        }
                        return Err(NanocodexError::CompactionFailed {
                            detail: error.to_string(),
                            requires_session_stop,
                            recovery,
                        });
                    }
                };
                let attempt = response.attempt();
                let connection_generation = response.connection_generation();
                let server_reasoning_included = response.server_reasoning_included();
                let ResponsesOutput::Compaction(response) = response.into_output() else {
                    return Err(NanocodexError::InvalidAttemptState {
                        detail: "background compaction returned a non-compaction response",
                    });
                };
                validate_provider_response_id(&response.id)?;
                let output = RecordedCompactionOutcome::Success(RecordedCompactionResult {
                    response_id: response.id,
                    status: response.status,
                    item: response.item,
                    usage: response.usage,
                    attempt,
                    connection_generation,
                    server_reasoning_included,
                    duration_ns: elapsed_ns(started),
                    time_to_first_event_ns: response.time_to_first_event_ns,
                    time_to_first_output_ns: response.time_to_first_output_ns,
                });
                if let Some(steps) = &steps {
                    steps.complete(&step, &output).await?;
                }
                let RecordedCompactionOutcome::Success(output) = output else {
                    unreachable!()
                };
                Ok(output)
            }),
        });
        Ok(())
    }

    pub(super) async fn poll_background(&mut self) {
        if let Some(work) = &mut self.background_work
            && work.result.is_none()
            && let Some(result) = work.future.as_mut().now_or_never()
        {
            work.result = Some(result);
        }
    }

    pub(super) async fn wait_background(&mut self) {
        if let Some(work) = &mut self.background_work
            && work.result.is_none()
        {
            work.result = Some(work.future.as_mut().await);
        }
    }

    pub(super) async fn install_background(
        &mut self,
        conversation: &mut ConversationState,
        factory: &ResponsesAttemptFactory,
    ) -> Result<Option<bool>> {
        if !self
            .background_work
            .as_ref()
            .is_some_and(|work| work.result.is_some())
        {
            return Ok(None);
        }
        // Completion can have preceded a foreground await. Recheck the durable
        // owner before swapping the live context, even for a replayed receipt.
        if let Some(steps) = &self.execution_steps {
            steps.continuation::<serde_json::Value>().await?;
        }
        let mut work = self.background_work.take().expect("completed work");
        let pending = self.background_compaction.take().expect("owned cutoff");
        let output = work.result.take().expect("completed result")?;
        let history = conversation.flattened_history();
        // Prefix equality also rejects rewrites, competing summaries and repaired images.
        // A later append is compatible; every item after the cutoff remains lossless.
        if history.len() < pending.cutoff.len()
            || serde_json::to_value(&history[..pending.cutoff.len()])
                .map_err(|e| NanocodexError::InvalidExecutionPolicy(e.to_string()))?
                != serde_json::to_value(&pending.cutoff)
                    .map_err(|e| NanocodexError::InvalidExecutionPolicy(e.to_string()))?
        {
            return Ok(Some(false));
        }
        let tail = history
            .into_iter()
            .skip(pending.cutoff.len())
            .collect::<Vec<_>>();
        conversation.observe_server_reasoning(output.server_reasoning_included);
        conversation.managed.install_compaction_with_tail(
            output.item,
            &pending.cutoff,
            tail,
            factory.profile().prefix(),
        );
        // Installing a summary discards the foreground continuation chain.
        // Terminal commit must preserve this full-replay baseline as well.
        self.stats.last_response_id = None;
        self.stats.model_duration_ns += output.duration_ns;
        self.stats.compaction_duration_ns += output.duration_ns;
        if let Some(usage) = &output.usage {
            self.stats.usage.add(usage, self.model, self.fast_mode);
        }
        self.events.emit(
            AgentEventKind::ModelCompactionCompleted,
            CompactionCompleted {
                after_model_call_index: pending.after_model_call_index,
                response_id: &output.response_id,
                attempt: output.attempt,
                connection_generation: output.connection_generation,
                status: &output.status,
                duration_ns: output.duration_ns,
                time_to_first_event_ns: output.time_to_first_event_ns,
                time_to_first_output_ns: output.time_to_first_output_ns,
                usage: output.usage.as_ref(),
            },
        )?;
        Ok(Some(true))
    }
}
