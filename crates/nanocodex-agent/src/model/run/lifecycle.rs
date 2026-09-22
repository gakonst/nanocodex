use super::*;

#[derive(Deserialize, Serialize)]
pub(super) struct WarmupExecution {
    pub(super) response_id: String,
    pub(super) attempt: u32,
    pub(super) connection_generation: u32,
    pub(super) usage: Option<Usage>,
    pub(super) server_reasoning_included: bool,
    // A persisted response ID cannot continue on a replacement transport.
    #[serde(skip)]
    transport_continuation_valid: bool,
}

pub(super) struct WarmupOutcome {
    pub(super) response_id: Option<String>,
    pub(super) server_reasoning_included: bool,
}

// Owns the terminal event even when an outer cancellation select drops the future.
// This cannot publish after the hosting process or isolate has been lost.
struct CompactionLifecycle<'a> {
    events: &'a EventSink,
    stats: &'a mut RunStats,
    span: tracing::Span,
    after_model_call_index: u32,
    started_at: Instant,
    finished: bool,
}

impl CompactionLifecycle<'_> {
    fn fail(&mut self, error: &str) -> Result<()> {
        self.finished = true;
        let duration_ns = elapsed_ns(self.started_at);
        self.span.record("status", "failed");
        self.span.record("otel.status_code", "ERROR");
        self.span.record("duration_ns", duration_ns);
        self.stats.model_duration_ns += duration_ns;
        self.stats.compaction_duration_ns += duration_ns;
        self.events.emit(
            AgentEventKind::ModelCompactionFailed,
            CompactionFailed {
                after_model_call_index: self.after_model_call_index,
                duration_ns,
                error,
            },
        )?;
        Ok(())
    }
}

impl Drop for CompactionLifecycle<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // Drop cannot return an event publication error to the cancelled caller.
            let _ = self.fail("compaction cancelled");
        }
    }
}

// Untagged success preserves receipts written before failures were recorded.
#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum RecordedCompactionOutcome {
    Success(RecordedCompactionResult),
    Failure {
        compaction_error: String,
        #[serde(default)]
        requires_session_stop: bool,
        #[serde(default)]
        recovery: crate::error::CompactionRecovery,
    },
}

#[derive(Deserialize, Serialize)]
struct RecordedCompactionResult {
    response_id: String,
    status: String,
    item: ResponseItem,
    usage: Option<Usage>,
    attempt: u32,
    connection_generation: u32,
    server_reasoning_included: bool,
    duration_ns: u64,
    time_to_first_event_ns: u64,
    time_to_first_output_ns: Option<u64>,
}

pub(super) enum ModelTaskOutcome {
    Completed(String),
    Cancelled,
}

#[derive(Clone, Copy)]
pub(super) enum CompactionPhase {
    PreTurn,
    MidTurn,
}

pub(super) struct CompactionContext<'a> {
    pub(super) snapshot: Option<&'a ContextSnapshot>,
    pub(super) phase: CompactionPhase,
}

impl<S> ModelRun<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<nanocodex_oai_api::ResponseError>,
    S::Future: AgentSend,
{
    pub(super) async fn maybe_compact(
        &mut self,
        after_model_call_index: u32,
        conversation: &mut ConversationState,
        factory: &ResponsesAttemptFactory,
        context: CompactionContext<'_>,
    ) -> Result<bool> {
        let CompactionContext { snapshot, phase } = context;
        let Some(auto_compact_token_limit) = compaction::auto_compact_token_limit(
            self.model.as_str(),
            self.config.context_window_tokens,
        ) else {
            return Ok(false);
        };
        let active_context_tokens = conversation.active_context_tokens();
        if !self.force_compaction && active_context_tokens < auto_compact_token_limit {
            return Ok(false);
        }
        let (history, prompt_repaired) = conversation.prompt_history_with_repair();
        let previous_response_id = conversation
            .previous_response_id()
            .filter(|_| !prompt_repaired);
        let (item, _usage, server_reasoning_included) = self
            .perform_compaction(
                after_model_call_index,
                history,
                if prompt_repaired {
                    0
                } else {
                    conversation.delta_start()
                },
                previous_response_id,
                active_context_tokens,
                auto_compact_token_limit,
                factory,
            )
            .await?;
        conversation.observe_server_reasoning(server_reasoning_included);
        match phase {
            CompactionPhase::PreTurn => {
                conversation.install_pre_turn_compaction(item, factory.profile().prefix());
            }
            CompactionPhase::MidTurn => {
                let snapshot = snapshot.ok_or(NanocodexError::InvalidAttemptState {
                    detail: "mid-turn compaction is missing its context snapshot",
                })?;
                let canonical_context = snapshot.full_item();
                conversation.install_mid_turn_compaction(
                    item,
                    developer_context(),
                    canonical_context,
                    factory.profile().prefix(),
                );
            }
        }
        self.force_compaction = false;
        Ok(true)
    }

    pub(super) async fn perform_warmup(
        &mut self,
        factory: &ResponsesAttemptFactory,
    ) -> Result<WarmupOutcome> {
        if matches!(self.config.responses_transport, ResponsesTransport::Https)
            || !self.config.websocket_warmup
        {
            return Ok(WarmupOutcome {
                response_id: None,
                server_reasoning_included: false,
            });
        }
        let started_at = Instant::now();
        self.events.emit(
            AgentEventKind::ModelWarmupStarted,
            WarmupStarted {
                model: self.model.as_str(),
                prompt_cache_key: factory.profile().prompt_cache_key(),
            },
        )?;
        let span = warmup_span(&self.config, self.model);
        if let Some(content) = serialize_trace_content(factory.profile().prefix()) {
            record_span_content(&span, "model.input", &content);
        }
        // A durable warmup must settle its own admitted effect even if another
        // owner warmed the same cache while it was interrupted.
        let shared_prompt_cache = self
            .execution_steps
            .is_none()
            .then(|| self.prompt_cache.shared().cloned())
            .flatten();
        let outcome = if let Some(cache) = shared_prompt_cache {
            match cache.entry(self.model, factory.profile()).await {
                Ok(entry) => {
                    let mut execution = None;
                    let initialized = entry
                        .get_or_try_init(|| async {
                            let completed = self.execute_warmup(factory, &span).await?;
                            execution = Some(completed);
                            Ok(())
                        })
                        .await;
                    initialized.map(|()| execution.flatten())
                }
                Err(error) => Err(error),
            }
        } else {
            self.execute_warmup(factory, &span).await
        };
        let execution = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                span.record("duration_ns", elapsed_ns(started_at));
                return self.warmup_failed(started_at, error);
            }
        };
        let duration_ns = elapsed_ns(started_at);
        let (response_id, source, attempt, connection_generation, usage, server_reasoning_included) =
            if let Some(execution) = execution {
                if let Some(usage) = &execution.usage {
                    self.stats
                        .warmup_usage
                        .add(usage, self.model, self.fast_mode);
                }
                (
                    execution
                        .transport_continuation_valid
                        .then_some(execution.response_id),
                    "response",
                    Some(execution.attempt),
                    Some(execution.connection_generation),
                    execution.usage,
                    execution.server_reasoning_included,
                )
            } else {
                (None, "shared_prefix", None, None, None, false)
            };
        span.record("warmup.source", source);
        if let Some(usage) = &usage {
            record_usage(&span, usage, self.model, self.fast_mode);
        }
        span.record("status", "completed");
        span.record("otel.status_code", "OK");
        span.record("duration_ns", duration_ns);
        self.stats.warmup_duration_ns += duration_ns;
        self.stats.last_response_id.clone_from(&response_id);
        self.events.emit(
            AgentEventKind::ModelWarmupCompleted,
            WarmupCompleted {
                response_id: response_id.as_deref(),
                source,
                attempt,
                connection_generation,
                duration_ns,
                usage: usage.as_ref(),
            },
        )?;
        Ok(WarmupOutcome {
            response_id,
            server_reasoning_included,
        })
    }

    pub(super) async fn execute_warmup(
        &mut self,
        factory: &ResponsesAttemptFactory,
        span: &tracing::Span,
    ) -> Result<Option<WarmupExecution>> {
        if let Some(steps) = &self.execution_steps
            && let crate::agent::ExecutionStep::Replay(output) = steps
                .begin::<_, Option<WarmupExecution>>("warmup", "warmup", &())
                .await?
        {
            return Ok(output);
        }
        let success = match self
            .client
            .execute(factory.warmup(self.model, self.thinking, self.fast_mode))
            .instrument(span.clone())
            .await
        {
            Ok(success) => success,
            Err(error) => {
                let error = NanocodexError::Response(error.into());
                if !error
                    .responses_error()
                    .is_some_and(|source| source.is_misalignment_policy_violation())
                    && let Some(steps) = &self.execution_steps
                {
                    steps.complete("warmup", &None::<WarmupExecution>).await?;
                }
                return Err(error);
            }
        };
        let attempt = success.attempt();
        let connection_generation = success.connection_generation();
        let server_reasoning_included = success.server_reasoning_included();
        let ResponsesOutput::Warmup(response) = success.into_output() else {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            return Err(NanocodexError::InvalidAttemptState {
                detail: "warmup returned a non-warmup response",
            });
        };
        let output = WarmupExecution {
            response_id: response.id,
            attempt,
            connection_generation,
            usage: response.usage,
            server_reasoning_included,
            transport_continuation_valid: true,
        };
        if let Some(steps) = &self.execution_steps {
            steps.complete("warmup", &Some(&output)).await?;
        }
        Ok(Some(output))
    }

    pub(super) fn warmup_failed<T>(
        &mut self,
        started_at: Instant,
        error: NanocodexError,
    ) -> Result<T> {
        let duration_ns = elapsed_ns(started_at);
        self.stats.warmup_duration_ns += duration_ns;
        let message = error.to_string();
        self.events.emit(
            AgentEventKind::ModelWarmupFailed,
            WarmupFailed {
                duration_ns,
                error: &message,
            },
        )?;
        Err(error)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn perform_compaction(
        &mut self,
        after_model_call_index: u32,
        history: nanocodex_oai_api::responses::ResponseHistory,
        incremental_start: usize,
        previous_response_id: Option<&str>,
        active_context_tokens: u64,
        auto_compact_token_limit: u64,
        factory: &ResponsesAttemptFactory,
    ) -> Result<(ResponseItem, Option<Usage>, bool)> {
        let step_id = format!("compaction-{after_model_call_index}");
        let model = self.model;
        let thinking = self.thinking;
        let fast_mode = self.fast_mode;
        let trigger = compaction::trigger();
        let mut history = history;
        let rewritten = compaction::trim_tool_outputs_to_fit_context_window(
            &mut history,
            factory.profile().prefix(),
            self.config.context_window_tokens,
        );
        let started_at = Instant::now();
        self.stats.compactions += 1;
        self.events.emit(
            AgentEventKind::ModelCompactionStarted,
            CompactionStarted {
                after_model_call_index,
                active_context_tokens,
                auto_compact_token_limit,
                previous_response_id,
            },
        )?;
        let request = factory.compaction(
            after_model_call_index,
            history.clone(),
            history.clone(),
            if rewritten == 0 { incremental_start } else { 0 },
            previous_response_id.filter(|_| rewritten == 0),
            trigger,
            model,
            thinking,
            fast_mode,
        );
        let (input_item_count, input_bytes, input_content) = trace_model_input(&request);
        let span = compaction_span(after_model_call_index, input_item_count, input_bytes);
        if let Some(input_content) = &input_content {
            record_span_content(&span, "model.input", input_content);
        }
        let mut lifecycle = CompactionLifecycle {
            events: &self.events,
            stats: &mut self.stats,
            span: span.clone(),
            after_model_call_index,
            started_at,
            finished: false,
        };
        let result = async {
            let execution_steps = self.execution_steps.clone();
            let recovered = if let Some(steps) = &execution_steps {
                match steps
                    .begin::<_, RecordedCompactionOutcome>(&step_id, "compaction", &())
                    .await?
                {
                    crate::agent::ExecutionStep::Execute => None,
                    crate::agent::ExecutionStep::Replay(output) => Some(output),
                }
            } else {
                None
            };
            let recorded_result = if let Some(output) = recovered {
                output
            } else {
                let success = match self.client.execute(request).instrument(span.clone()).await {
                    Ok(success) => success,
                    Err(error) => {
                        let error = NanocodexError::Response(error.into());
                        // Only completed provider failures consume the compaction budget.
                        // Policy, ownership, and storage failures retain their recovery semantics.
                        if error.responses_error().is_none() {
                            return Err(error);
                        }
                        let requires_session_stop = error
                            .responses_error()
                            .is_some_and(|source| source.is_misalignment_policy_violation());
                        let recovery = if error.requires_image_repair() {
                            crate::error::CompactionRecovery::ReplaceRejectedImages
                        } else {
                            crate::error::CompactionRecovery::None
                        };
                        let compaction_error = error.to_string();
                        if let Some(steps) = &execution_steps {
                            steps
                                .complete(
                                    &step_id,
                                    &RecordedCompactionOutcome::Failure {
                                        compaction_error: compaction_error.clone(),
                                        requires_session_stop,
                                        recovery,
                                    },
                                )
                                .await?;
                        }
                        return Err(NanocodexError::CompactionFailed {
                            detail: compaction_error,
                            requires_session_stop,
                            recovery,
                        });
                    }
                };
                let attempt = success.attempt();
                let connection_generation = success.connection_generation();
                let server_reasoning_included = success.server_reasoning_included();
                let ResponsesOutput::Compaction(response) = success.into_output() else {
                    let error = NanocodexError::InvalidAttemptState {
                        detail: "compaction returned a non-compaction response",
                    };
                    return Err(error);
                };
                let output = RecordedCompactionResult {
                    response_id: response.id,
                    status: response.status,
                    item: response.item,
                    usage: response.usage,
                    attempt,
                    connection_generation,
                    server_reasoning_included,
                    duration_ns: elapsed_ns(started_at),
                    time_to_first_event_ns: response.time_to_first_event_ns,
                    time_to_first_output_ns: response.time_to_first_output_ns,
                };
                validate_provider_response_id(&output.response_id)?;
                let output = RecordedCompactionOutcome::Success(output);
                if let Some(steps) = &execution_steps {
                    steps.complete(&step_id, &output).await?;
                }
                output
            };
            let recorded_result = match recorded_result {
                RecordedCompactionOutcome::Success(output) => output,
                RecordedCompactionOutcome::Failure {
                    compaction_error,
                    requires_session_stop,
                    recovery,
                } => {
                    return Err(NanocodexError::CompactionFailed {
                        detail: compaction_error,
                        requires_session_stop,
                        recovery,
                    });
                }
            };
            let RecordedCompactionResult {
                response_id,
                status,
                item,
                usage,
                attempt,
                connection_generation,
                server_reasoning_included,
                duration_ns,
                time_to_first_event_ns,
                time_to_first_output_ns,
            } = recorded_result;
            validate_provider_response_id(&response_id)?;
            span.record("model.response.id", response_id.as_str());
            if let Some(content) = serialize_trace_content(&item) {
                record_span_content(&span, "model.output_item", &content);
            }
            span.record("status", "completed");
            span.record("otel.status_code", "OK");
            span.record("duration_ns", duration_ns);
            self.events.emit(
                AgentEventKind::ModelCompactionCompleted,
                CompactionCompleted {
                    after_model_call_index,
                    response_id: &response_id,
                    attempt,
                    connection_generation,
                    status: &status,
                    duration_ns,
                    time_to_first_event_ns,
                    time_to_first_output_ns,
                    usage: usage.as_ref(),
                },
            )?;
            lifecycle.finished = true;
            lifecycle.stats.model_duration_ns += duration_ns;
            lifecycle.stats.compaction_duration_ns += duration_ns;
            if let Some(usage) = &usage {
                record_usage(&span, usage, model, self.fast_mode);
                lifecycle.stats.usage.add(usage, model, self.fast_mode);
            }
            lifecycle.stats.last_response_id = Some(response_id);
            Ok((item, usage, server_reasoning_included))
        }
        .await;
        if let Err(error) = &result {
            lifecycle.fail(&error.to_string())?;
        }
        result
    }
}

#[cfg(test)]
mod compaction_receipt_tests {
    use super::*;

    #[test]
    fn failure_receipt_preserves_session_stop_and_defaults_old_receipts() {
        let old: RecordedCompactionOutcome = serde_json::from_value(serde_json::json!({
            "compaction_error": "exhausted"
        }))
        .unwrap();
        assert!(matches!(
            old,
            RecordedCompactionOutcome::Failure {
                requires_session_stop: false,
                ..
            }
        ));
        let receipt = RecordedCompactionOutcome::Failure {
            compaction_error: "stop this conversation".into(),
            requires_session_stop: true,
            recovery: crate::error::CompactionRecovery::None,
        };
        let replay: RecordedCompactionOutcome =
            serde_json::from_value(serde_json::to_value(receipt).unwrap()).unwrap();
        assert!(matches!(
            replay,
            RecordedCompactionOutcome::Failure {
                requires_session_stop: true,
                ..
            }
        ));
    }

    #[test]
    fn image_failure_receipt_preserves_repair_and_policy_stop_wins() {
        for stop in [false, true] {
            let receipt = RecordedCompactionOutcome::Failure {
                compaction_error: "rejected image".into(),
                requires_session_stop: stop,
                recovery: crate::error::CompactionRecovery::ReplaceRejectedImages,
            };
            let replay: RecordedCompactionOutcome =
                serde_json::from_value(serde_json::to_value(receipt).unwrap()).unwrap();
            let RecordedCompactionOutcome::Failure {
                compaction_error,
                requires_session_stop,
                recovery,
            } = replay
            else {
                panic!("expected failure receipt");
            };
            let error = NanocodexError::CompactionFailed {
                detail: compaction_error,
                requires_session_stop,
                recovery,
            };
            assert_eq!(error.requires_image_repair(), !stop);
            assert!(error.responses_error().is_none());
        }
    }

    #[test]
    fn legacy_success_receipt_keeps_its_wire_shape() {
        let legacy = serde_json::json!({
            "response_id": "resp-legacy", "status": "completed",
            "item": {"type": "compaction", "encrypted_content": "retained"},
            "usage": null, "attempt": 1, "connection_generation": 0,
            "server_reasoning_included": false, "duration_ns": 1,
            "time_to_first_event_ns": 1, "time_to_first_output_ns": null
        });
        let output: RecordedCompactionOutcome = serde_json::from_value(legacy.clone()).unwrap();
        assert!(matches!(output, RecordedCompactionOutcome::Success(_)));
        assert_eq!(serde_json::to_value(output).unwrap(), legacy);
    }
}
