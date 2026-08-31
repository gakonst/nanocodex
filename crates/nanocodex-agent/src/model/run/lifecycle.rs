use super::*;

#[derive(Deserialize, Serialize)]
pub(super) struct WarmupExecution {
    pub(super) response_id: String,
    pub(super) attempt: u32,
    pub(super) connection_generation: u32,
    pub(super) usage: Option<Usage>,
    pub(super) server_reasoning_included: bool,
}

#[derive(Serialize)]
struct RecordedWarmupCall<'a> {
    model: &'a str,
    model_id_prefix: Option<&'a str>,
    reasoning_mode: &'a str,
    effort: &'a str,
    fast_mode: bool,
    store_responses: bool,
    transport: &'a str,
    websocket_url: &'a str,
    api_base_url: &'a str,
    prompt_cache_key: &'a str,
    request_prefix: &'a [ResponseItem],
}

pub(super) struct WarmupOutcome {
    pub(super) response_id: Option<String>,
    pub(super) server_reasoning_included: bool,
}

#[derive(Serialize)]
struct RecordedCompactionCall<'a> {
    after_model_call_index: u32,
    model: &'a str,
    model_id_prefix: Option<&'a str>,
    reasoning_mode: &'a str,
    effort: &'a str,
    fast_mode: bool,
    store_responses: bool,
    transport: &'a str,
    websocket_url: &'a str,
    api_base_url: &'a str,
    prompt_cache_key: &'a str,
    request_prefix: &'a [ResponseItem],
    prompt_history: &'a [ResponseItem],
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
        let auto_compact_token_limit =
            compaction::auto_compact_token_limit(self.model, self.config.context_window_tokens);
        let active_context_tokens = conversation.active_context_tokens();
        if !self.force_compaction && active_context_tokens < auto_compact_token_limit {
            return Ok(false);
        }
        let previous_response_id = conversation.previous_response_id();
        let (item, _usage, server_reasoning_included) = self
            .perform_compaction(
                after_model_call_index,
                conversation.prompt_history(),
                conversation.delta_start(),
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
        let shared_prompt_cache = self.prompt_cache.shared().cloned();
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
                    initialized.map(|()| execution)
                }
                Err(error) => Err(error),
            }
        } else {
            self.execute_warmup(factory, &span).await.map(Some)
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
                    self.stats.warmup_usage.add(usage);
                }
                (
                    Some(execution.response_id),
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
    ) -> Result<WarmupExecution> {
        let mut recorded_request_prefix = factory.profile().prefix().to_vec();
        for item in &mut recorded_request_prefix {
            item.strip_id();
        }
        let recorded = RecordedWarmupCall {
            model: self.model.as_str(),
            model_id_prefix: self.config.model_id_prefix.as_deref(),
            reasoning_mode: self.config.reasoning_mode.as_str(),
            effort: self.thinking.as_str(),
            fast_mode: self.fast_mode,
            store_responses: self.config.store_responses,
            transport: self.config.responses_transport.as_str(),
            websocket_url: &self.config.websocket_url,
            api_base_url: &self.config.api_base_url,
            prompt_cache_key: factory.profile().prompt_cache_key(),
            request_prefix: &recorded_request_prefix,
        };
        if let Some(steps) = &self.execution_steps {
            // A reopened durable step cannot prove that the provider did not
            // observe the request. Live in-attempt retries remain client-owned.
            match steps
                .begin::<_, WarmupExecution>(
                    "warmup",
                    "warmup",
                    &recorded,
                    crate::agent::execution::ExecutionRetry::Never,
                )
                .await?
            {
                crate::agent::ExecutionStep::Replay(output) => return Ok(output),
                crate::agent::ExecutionStep::Execute => {}
                crate::agent::ExecutionStep::Unknown => {
                    return Err(NanocodexError::ProviderOutcomeUnknown {
                        operation: "warmup",
                    });
                }
            }
        }
        let success = self
            .client
            .execute(factory.warmup(self.model, self.thinking, self.fast_mode))
            .instrument(span.clone())
            .await
            .map_err(|error| NanocodexError::Response(error.into()))?;
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
        };
        if let Some(steps) = &self.execution_steps {
            steps.complete("warmup", &output).await?;
        }
        Ok(output)
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
        let trigger = compaction::trigger();
        let mut history = history;
        compaction::trim_tool_outputs_to_fit_context_window(
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
            incremental_start,
            previous_response_id,
            trigger,
            self.model,
            self.thinking,
            self.fast_mode,
        );
        let (input_item_count, input_bytes, input_content) = trace_model_input(&request);
        let span = compaction_span(after_model_call_index, input_item_count, input_bytes);
        if let Some(input_content) = &input_content {
            record_span_content(&span, "model.input", input_content);
        }
        let execution_steps = self.execution_steps.clone();
        let step_id = format!("compaction-{after_model_call_index}");
        let mut recorded_prompt_history = history.iter().cloned().collect::<Vec<_>>();
        for item in &mut recorded_prompt_history {
            item.strip_id();
        }
        let mut recorded_request_prefix = factory.profile().prefix().to_vec();
        for item in &mut recorded_request_prefix {
            item.strip_id();
        }
        let step_input = RecordedCompactionCall {
            after_model_call_index,
            model: self.model.as_str(),
            model_id_prefix: self.config.model_id_prefix.as_deref(),
            reasoning_mode: self.config.reasoning_mode.as_str(),
            effort: self.thinking.as_str(),
            fast_mode: self.fast_mode,
            store_responses: self.config.store_responses,
            transport: self.config.responses_transport.as_str(),
            websocket_url: &self.config.websocket_url,
            api_base_url: &self.config.api_base_url,
            prompt_cache_key: factory.profile().prompt_cache_key(),
            request_prefix: &recorded_request_prefix,
            prompt_history: &recorded_prompt_history,
        };
        let recovered = if let Some(steps) = &execution_steps {
            // A reopened durable step cannot prove that the provider did not
            // observe the request. Live in-attempt retries remain client-owned.
            match steps
                .begin::<_, RecordedCompactionResult>(
                    &step_id,
                    "compaction",
                    &step_input,
                    crate::agent::execution::ExecutionRetry::Never,
                )
                .await?
            {
                crate::agent::ExecutionStep::Execute => None,
                crate::agent::ExecutionStep::Replay(output) => Some(output),
                crate::agent::ExecutionStep::Unknown => {
                    let error = NanocodexError::ProviderOutcomeUnknown {
                        operation: "compaction",
                    };
                    span.record("status", "failed");
                    span.record("otel.status_code", "ERROR");
                    span.record("duration_ns", elapsed_ns(started_at));
                    return self.compaction_failed(after_model_call_index, started_at, error);
                }
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
                    span.record("status", "failed");
                    span.record("otel.status_code", "ERROR");
                    span.record("duration_ns", elapsed_ns(started_at));
                    return self.compaction_failed(
                        after_model_call_index,
                        started_at,
                        NanocodexError::Response(error.into()),
                    );
                }
            };
            let attempt = success.attempt();
            let connection_generation = success.connection_generation();
            let server_reasoning_included = success.server_reasoning_included();
            let ResponsesOutput::Compaction(response) = success.into_output() else {
                let error = NanocodexError::InvalidAttemptState {
                    detail: "compaction returned a non-compaction response",
                };
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                span.record("duration_ns", elapsed_ns(started_at));
                return self.compaction_failed(after_model_call_index, started_at, error);
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
            if let Some(steps) = &execution_steps {
                steps.complete(&step_id, &output).await?;
            }
            output
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
        self.stats.model_duration_ns += duration_ns;
        self.stats.compaction_duration_ns += duration_ns;
        if let Some(usage) = &usage {
            record_usage(&span, usage, self.model, self.fast_mode);
            self.stats.usage.add(usage);
        }
        self.stats.last_response_id = Some(response_id.clone());
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
        Ok((item, usage, server_reasoning_included))
    }

    pub(super) fn compaction_failed<T>(
        &mut self,
        after_model_call_index: u32,
        started_at: Instant,
        error: crate::NanocodexError,
    ) -> Result<T> {
        let duration_ns = elapsed_ns(started_at);
        self.stats.model_duration_ns += duration_ns;
        self.stats.compaction_duration_ns += duration_ns;
        let message = error.to_string();
        self.events.emit(
            AgentEventKind::ModelCompactionFailed,
            CompactionFailed {
                after_model_call_index,
                duration_ns,
                error: &message,
            },
        )?;
        Err(error)
    }
}
