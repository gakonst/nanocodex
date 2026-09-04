use super::*;

#[derive(Serialize)]
struct RecordedModelCall<'a> {
    call_index: u32,
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
struct RecordedModelResult {
    response: TurnResult,
    attempt: u32,
    connection_generation: u32,
    server_reasoning_included: bool,
    duration_ns: u64,
}

pub(super) struct ModelCallOutcome {
    pub(super) response: TurnResult,
    pub(super) transport_continuation_valid: bool,
}

impl<S> ModelRun<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<nanocodex_oai_api::ResponseError>,
    S::Future: AgentSend,
{
    pub(super) async fn perform_model_call(
        &mut self,
        call_index: u32,
        conversation: &mut ConversationState,
        factory: &ResponsesAttemptFactory,
    ) -> Result<ModelCallOutcome> {
        let (prompt_history, prompt_repaired) = conversation.prompt_history_with_repair();
        let previous_response_id = if prompt_repaired {
            None
        } else {
            conversation.previous_response_id().map(str::to_owned)
        };
        let started_at = Instant::now();
        self.stats.model_calls += 1;
        self.events.emit(
            AgentEventKind::ModelCallStarted,
            ModelCallStarted {
                call_index,
                model: self.model.as_str(),
                reasoning_mode: self.config.reasoning_mode.as_str(),
                effort: self.thinking.as_str(),
                previous_response_id: previous_response_id.as_deref(),
            },
        )?;
        let request = factory.generation(
            call_index,
            prompt_history.clone(),
            conversation.shared_history(),
            conversation.delta_start(),
            previous_response_id.as_deref(),
            self.model,
            self.thinking,
            self.fast_mode,
        );
        let (input_item_count, input_bytes, input_content) = trace_model_input(&request);
        let span = model_call_span(
            call_index,
            self.model.as_str(),
            self.config.reasoning_mode.as_str(),
            self.thinking.as_str(),
            previous_response_id.is_some(),
            input_item_count,
            input_bytes,
        );
        if let Some(input_content) = &input_content {
            record_span_content(&span, "model.input", input_content);
        }
        let execution_steps = self.execution_steps.clone();
        let step_id = format!("model-{call_index}");
        let mut recorded_prompt_history = prompt_history.iter().cloned().collect::<Vec<_>>();
        for item in &mut recorded_prompt_history {
            item.strip_id();
        }
        let mut recorded_request_prefix = factory.profile().prefix().to_vec();
        for item in &mut recorded_request_prefix {
            item.strip_id();
        }
        let step_input = RecordedModelCall {
            call_index,
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
            match steps
                .begin::<_, RecordedModelResult>(&step_id, "model_call", &step_input)
                .await?
            {
                crate::agent::ExecutionStep::Execute => None,
                crate::agent::ExecutionStep::Replay(output) => Some(output),
            }
        } else {
            None
        };
        let (recorded_result, transport_continuation_valid) = if let Some(output) = recovered {
            (output, false)
        } else {
            let success = match self.client.execute(request).instrument(span.clone()).await {
                Ok(success) => success,
                Err(error) => {
                    span.record("status", "failed");
                    span.record("otel.status_code", "ERROR");
                    span.record("duration_ns", elapsed_ns(started_at));
                    return self.model_call_failed(
                        call_index,
                        started_at,
                        NanocodexError::Response(error.into()),
                    );
                }
            };
            let attempt = success.attempt();
            let connection_generation = success.connection_generation();
            let server_reasoning_included = success.server_reasoning_included();
            let ResponsesOutput::Generation(response) = success.into_output() else {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                return Err(NanocodexError::InvalidAttemptState {
                    detail: "generation returned a non-generation response",
                });
            };
            let output = RecordedModelResult {
                response,
                attempt,
                connection_generation,
                server_reasoning_included,
                duration_ns: elapsed_ns(started_at),
            };
            validate_provider_response_id(&output.response.id)?;
            if let Some(steps) = &execution_steps {
                steps.complete(&step_id, &output).await?;
            }
            (output, true)
        };
        let RecordedModelResult {
            response,
            attempt,
            connection_generation,
            server_reasoning_included,
            duration_ns,
        } = recorded_result;
        validate_provider_response_id(&response.id)?;
        conversation.observe_server_reasoning(server_reasoning_included);
        if prompt_repaired {
            conversation.adopt_prompt_history(prompt_history);
        }
        record_model_response(&span, &response);
        span.record("status", "completed");
        span.record("otel.status_code", "OK");
        span.record("duration_ns", duration_ns);
        if let Some(usage) = &response.usage {
            record_usage(&span, usage, self.model, self.fast_mode);
        }
        self.stats.model_duration_ns += duration_ns;
        if let Some(usage) = &response.usage {
            self.stats.usage.add(usage, self.model, self.fast_mode);
        }
        self.stats.last_response_id = transport_continuation_valid.then(|| response.id.clone());
        self.events.emit(
            AgentEventKind::ModelCallCompleted,
            ModelCallCompleted {
                call_index,
                model: self.model.as_str(),
                response_id: &response.id,
                attempt,
                connection_generation,
                status: &response.status,
                duration_ns,
                time_to_first_event_ns: response.time_to_first_event_ns,
                time_to_first_output_ns: response.time_to_first_output_ns,
                tool_calls: response.code_calls.len(),
                usage: response.usage.as_ref(),
            },
        )?;
        Ok(ModelCallOutcome {
            response,
            transport_continuation_valid,
        })
    }

    pub(super) fn model_call_failed<T>(
        &mut self,
        call_index: u32,
        started_at: Instant,
        error: crate::NanocodexError,
    ) -> Result<T> {
        let duration_ns = elapsed_ns(started_at);
        self.stats.model_duration_ns += duration_ns;
        let message = error.to_string();
        self.events.emit(
            AgentEventKind::ModelCallFailed,
            ModelCallFailed {
                call_index,
                model: self.model.as_str(),
                duration_ns,
                error: &message,
            },
        )?;
        Err(error)
    }
}

pub(super) fn validate_provider_response_id(response_id: &str) -> Result<()> {
    if response_id.trim().is_empty() {
        return Err(NanocodexError::MalformedResponse {
            detail: "completed turn did not have a response ID",
        });
    }
    Ok(())
}

pub(super) fn unsupported_tool_message(tools: &ToolRuntime, call: &CodeCall) -> Option<String> {
    let qualified_name = qualified_tool_name(call);
    if (call.namespace.is_none() && matches!(call.name.as_str(), "exec" | "wait"))
        || tools.contains(&qualified_name)
    {
        return None;
    }
    Some(match &call.kind {
        CodeCallKind::Custom => format!("unsupported custom tool call: {qualified_name}"),
        CodeCallKind::Function => format!("unsupported call: {qualified_name}"),
        CodeCallKind::ToolSearch => return None,
    })
}

pub(super) fn qualified_tool_name(call: &CodeCall) -> String {
    let Some(namespace) = call.namespace.as_deref() else {
        return call.name.clone();
    };
    if namespace.ends_with('_') || call.name.starts_with('_') {
        format!("{namespace}{}", call.name)
    } else {
        format!("{namespace}__{}", call.name)
    }
}

pub(super) fn trace_model_input(request: &ResponsesAttempt) -> (usize, usize, Option<String>) {
    let item_count = request.input_item_count();
    if !trace_content_enabled() {
        return (item_count, 0, None);
    }
    let items = request.input_items().collect::<Vec<_>>();
    let content = serde_json::to_string(&items).ok();
    let bytes = content.as_ref().map_or(0, String::len);
    (item_count, bytes, content)
}

pub(super) fn trace_content_enabled() -> bool {
    tracing::enabled!(target: "nanocodex", tracing::Level::INFO)
}

pub(super) fn serialize_trace_content<T: Serialize + ?Sized>(value: &T) -> Option<String> {
    trace_content_enabled()
        .then(|| serde_json::to_string(value).ok())
        .flatten()
}

pub(super) fn record_tool_span_terminal(
    span: &tracing::Span,
    status: &'static str,
    otel_status: &'static str,
    duration_ns: u64,
    output: &ToolOutputBody,
) {
    if let Some(content) = serialize_trace_content(output) {
        record_span_content(span, "tool.output", &content);
    }
    span.record("status", status);
    span.record("otel.status_code", otel_status);
    span.record("duration_ns", duration_ns);
}

pub(super) fn panic_payload(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast::<&'static str>().map_or_else(
            |_| "non-string panic payload".to_owned(),
            |message| (*message).to_owned(),
        ),
    }
}

pub(super) fn model_tool_span(call: &CodeCall, call_index: u32) -> tracing::Span {
    let qualified_name = qualified_tool_name(call);
    info_span!(
        target: "nanocodex",
        "tool.call",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        tool.name = %qualified_name,
        tool.call_id = %call.call_id,
        tool.arguments.bytes = call.input.len(),
        model.call_index = call_index,
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    )
}

pub(super) fn owned_code_context(
    call: &CodeCall,
    history: Option<Arc<Vec<ResponseItem>>>,
    session_id: &str,
    model: Model,
) -> Result<Option<OwnedToolContext>> {
    if call.name != "exec" {
        return Ok(None);
    }
    let history = history.ok_or(NanocodexError::MalformedResponse {
        detail: "exec call did not have an owned history snapshot",
    })?;
    Ok(Some(OwnedToolContext::new(
        model.as_str(),
        session_id,
        &call.call_id,
        history,
        DEFAULT_TOOL_OUTPUT_TOKENS,
    )))
}

pub(super) fn record_span_content(span: &tracing::Span, kind: &'static str, content: &str) {
    span.in_scope(|| {
        info!(
            target: "nanocodex",
            content_kind = kind,
            content,
            "trace content"
        );
    });
}

pub(super) fn record_indexed_span_content(
    span: &tracing::Span,
    kind: &'static str,
    index: usize,
    content: &str,
) {
    span.in_scope(|| {
        info!(
            target: "nanocodex",
            content_kind = kind,
            output.index = index,
            content,
            "trace content"
        );
    });
}

pub(super) fn model_call_span(
    call_index: u32,
    model: &str,
    reasoning_mode: &str,
    reasoning_effort: &str,
    previous_response: bool,
    input_item_count: usize,
    input_bytes: usize,
) -> tracing::Span {
    info_span!(
        target: "nanocodex",
        "model.call",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        model,
        reasoning.mode = reasoning_mode,
        reasoning.effort = reasoning_effort,
        model.call_index = call_index,
        previous_response,
        model.input.item_count = input_item_count,
        model.input.bytes = input_bytes,
        model.response.id = tracing::field::Empty,
        model.response.status = tracing::field::Empty,
        model.response.end_turn = tracing::field::Empty,
        model.output.item_count = tracing::field::Empty,
        model.output.bytes = tracing::field::Empty,
        model.tool_call_count = tracing::field::Empty,
        assistant.output.bytes = tracing::field::Empty,
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
        input_tokens = tracing::field::Empty,
        cached_input_tokens = tracing::field::Empty,
        cache_write_input_tokens = tracing::field::Empty,
        output_tokens = tracing::field::Empty,
        reasoning_output_tokens = tracing::field::Empty,
        total_tokens = tracing::field::Empty,
        cost.usd = tracing::field::Empty,
        cost.service_tier = tracing::field::Empty,
        reasoning.summary_count = tracing::field::Empty,
        time_to_first_event_ns = tracing::field::Empty,
        time_to_first_output_ns = tracing::field::Empty,
        stream.display_delta.count = tracing::field::Empty,
        stream.display_delta.bytes = tracing::field::Empty,
        stream.inter_delta_gap.max_ns = tracing::field::Empty,
        stream.inter_delta_stall_100ms.count = tracing::field::Empty,
    )
}

pub(super) fn warmup_span(config: &ModelConfig, model: Model) -> tracing::Span {
    info_span!(
        target: "nanocodex",
        "model.warmup",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        model = model.as_str(),
        system_prompt.bytes = config.system_prompt().len(),
        warmup.source = tracing::field::Empty,
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
        input_tokens = tracing::field::Empty,
        cached_input_tokens = tracing::field::Empty,
        cache_write_input_tokens = tracing::field::Empty,
        output_tokens = tracing::field::Empty,
        reasoning_output_tokens = tracing::field::Empty,
        total_tokens = tracing::field::Empty,
        cost.usd = tracing::field::Empty,
        cost.service_tier = tracing::field::Empty,
    )
}

pub(super) fn compaction_span(
    after_model_call_index: u32,
    input_item_count: usize,
    input_bytes: usize,
) -> tracing::Span {
    info_span!(
        target: "nanocodex",
        "model.compaction",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        after_model_call_index,
        model.input.item_count = input_item_count,
        model.input.bytes = input_bytes,
        model.response.id = tracing::field::Empty,
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
        input_tokens = tracing::field::Empty,
        cached_input_tokens = tracing::field::Empty,
        cache_write_input_tokens = tracing::field::Empty,
        output_tokens = tracing::field::Empty,
        reasoning_output_tokens = tracing::field::Empty,
        total_tokens = tracing::field::Empty,
        cost.usd = tracing::field::Empty,
        cost.service_tier = tracing::field::Empty,
    )
}

pub(super) fn record_usage(span: &tracing::Span, usage: &Usage, model: Model, fast_mode: bool) {
    let cached_input_tokens = usage
        .input_tokens_details
        .as_ref()
        .map_or(0, |details| details.cached_tokens);
    let cache_write_input_tokens = usage
        .input_tokens_details
        .as_ref()
        .map_or(0, |details| details.cache_write_tokens);
    let reasoning_output_tokens = usage
        .output_tokens_details
        .as_ref()
        .map_or(0, |details| details.reasoning_tokens);
    span.record("input_tokens", usage.input_tokens);
    span.record("cached_input_tokens", cached_input_tokens);
    span.record("cache_write_input_tokens", cache_write_input_tokens);
    span.record("output_tokens", usage.output_tokens);
    span.record("reasoning_output_tokens", reasoning_output_tokens);
    span.record("total_tokens", usage.total_tokens);
    let estimate = estimate_for_model(usage, model, ServiceTier::for_model(model, fast_mode));
    let amount = estimate.amount().decimal();
    span.record("cost.usd", amount.as_str());
    span.record("cost.service_tier", estimate.service_tier().as_str());
}

pub(super) fn record_turn_usage(span: &tracing::Span, usage: &TurnUsage) {
    span.record("usage.input_tokens", usage.input_tokens());
    span.record("usage.cached_input_tokens", usage.cached_input_tokens());
    span.record(
        "usage.cache_write_input_tokens",
        usage.cache_write_input_tokens(),
    );
    span.record("usage.output_tokens", usage.output_tokens());
    span.record(
        "usage.reasoning_output_tokens",
        usage.reasoning_output_tokens(),
    );
    span.record("usage.total_tokens", usage.total_tokens());
    span.record("cost.status", usage.cost_status().as_str());
    if let Some(cost) = usage.estimated_cost() {
        let amount = cost.amount().decimal();
        span.record("cost.usd", amount.as_str());
        span.record("cost.service_tier", cost.service_tier().as_str());
    }
}

pub(super) fn record_model_response(span: &tracing::Span, response: &TurnResult) {
    span.record("model.response.id", response.id.as_str());
    span.record("model.response.status", response.status.as_str());
    if let Some(end_turn) = response.end_turn {
        span.record("model.response.end_turn", end_turn);
    }
    span.record("model.output.item_count", response.output_items.len());
    span.record("model.tool_call_count", response.code_calls.len());
    let trace_content = trace_content_enabled();
    let mut output_bytes = usize::from(trace_content).saturating_mul(2);
    let mut serialized_items = 0_usize;
    let mut summary_count = 0_usize;
    for (index, item) in response.output_items.iter().enumerate() {
        let kind = if let ResponseItem::Reasoning { summary, .. } = item {
            summary_count = summary_count.saturating_add(summary.len());
            "reasoning"
        } else {
            "model.output_item"
        };
        if trace_content && let Ok(content) = serde_json::to_string(item) {
            output_bytes = output_bytes
                .saturating_add(usize::from(serialized_items != 0))
                .saturating_add(content.len());
            serialized_items = serialized_items.saturating_add(1);
            record_indexed_span_content(span, kind, index, &content);
        }
    }
    span.record("model.output.bytes", output_bytes);
    if let Some(message) = &response.final_message {
        span.record("assistant.output.bytes", message.len());
    }
    span.record("reasoning.summary_count", summary_count);
    span.record("time_to_first_event_ns", response.time_to_first_event_ns);
    if let Some(time_to_first_output_ns) = response.time_to_first_output_ns {
        span.record("time_to_first_output_ns", time_to_first_output_ns);
    }
    span.record(
        "stream.display_delta.count",
        response.pipeline_stats.display_delta_count,
    );
    span.record(
        "stream.display_delta.bytes",
        response.pipeline_stats.display_delta_bytes,
    );
    span.record(
        "stream.inter_delta_gap.max_ns",
        response.pipeline_stats.inter_delta_gap_max_ns,
    );
    span.record(
        "stream.inter_delta_stall_100ms.count",
        response.pipeline_stats.inter_delta_stall_100ms_count,
    );
}

pub(super) fn request_profile(
    session_id: &str,
    prompt_cache_key: &str,
    tool_specs: Vec<ToolDefinition>,
    code_mode_tool_names: Vec<(String, String)>,
    system_prompt: &str,
) -> Result<RequestProfile> {
    let prefix = responses_lite_request_prefix(prompt_cache_key, tool_specs, system_prompt)
        .map_err(NanocodexError::SerializePromptPrefix)?;
    Ok(with_code_mode_tool_names(
        RequestProfile::new(session_id, prompt_cache_key, Arc::from(prefix)),
        code_mode_tool_names,
    ))
}

pub(super) fn attempt_factory(
    events: &EventSink,
    transport_stats: &Arc<TransportStats>,
    provider_session_id: &str,
    prompt_cache_key: &str,
    tools: &ToolRuntime,
    system_prompt: &str,
) -> Result<ResponsesAttemptFactory> {
    let (tool_specs, code_mode_tool_names) = model_tool_contract(tools, events.request_id());
    Ok(ResponsesAttemptFactory::new(
        request_profile(
            provider_session_id,
            prompt_cache_key,
            tool_specs,
            code_mode_tool_names,
            system_prompt,
        )?
        .with_thread_id(events.request_id()),
        events.clone(),
        Arc::clone(transport_stats),
    ))
}

pub(super) fn tool_runtime(workspace: &str, config: &ModelConfig, tools: &Tools) -> ToolRuntime {
    ToolRuntime::new_with_tools(
        workspace,
        tools.web_search_enabled().then(|| WebSearchConfig {
            endpoint: config.search_endpoint(),
            auth: config.auth.clone(),
        }),
        tools
            .image_generation_enabled()
            .then(|| ImageGenerationConfig {
                api_base_url: config.api_base_url.clone(),
                auth: config.auth.clone(),
                save_root: Path::new(workspace).to_path_buf(),
            }),
        tools,
    )
}

pub(super) const fn status(success: bool) -> &'static str {
    if success { "completed" } else { "failed" }
}

pub(super) const fn otel_status(success: bool) -> &'static str {
    if success { "OK" } else { "ERROR" }
}

#[cfg(test)]
mod response_id_tests {
    use super::validate_provider_response_id;

    #[test]
    fn provider_response_ids_must_contain_non_whitespace_text() {
        assert!(validate_provider_response_id("response-1").is_ok());
        assert!(validate_provider_response_id("").is_err());
        assert!(validate_provider_response_id(" \n\t").is_err());
    }
}
