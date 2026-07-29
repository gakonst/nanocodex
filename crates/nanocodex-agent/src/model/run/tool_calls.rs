use super::*;

#[derive(Clone)]
pub(super) struct ActiveToolCall {
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) kind: CodeCallKind,
    pub(super) started_at: Instant,
    pub(super) shell_abort_format: bool,
    pub(super) completion: Arc<Mutex<Option<CompletedToolCall>>>,
    pub(super) progress: Arc<Mutex<ActiveToolProgress>>,
    pub(super) execution_started_at: Arc<Mutex<Option<Instant>>>,
    pub(super) span: tracing::Span,
}

#[derive(Default)]
pub(super) struct ActiveToolProgress {
    pub(super) nested_tool_calls: u32,
}

pub(super) struct CompletedToolCall {
    pub(super) call_id: String,
    pub(super) tool: String,
    pub(super) success: bool,
    pub(super) duration_ns: u64,
    pub(super) work_duration_ns: u64,
    pub(super) output: ToolOutputBody,
    pub(super) metadata: Option<Box<RawValue>>,
    pub(super) response_items: Vec<ResponseItem>,
}

pub(super) struct NestedToolEventObserver<'a> {
    events: &'a EventSink,
    tool_call_indices: &'a HashMap<Box<str>, u32>,
    pub(super) progress: &'a Mutex<ActiveToolProgress>,
    fallback_call_index: u32,
    parent_call_id: &'a str,
    error: Option<NanocodexError>,
}

impl CodeModeObserver for NestedToolEventObserver<'_> {
    fn update(&mut self, update: CodeModeUpdate<'_>) {
        if self.error.is_some() {
            return;
        }
        let result = match update {
            CodeModeUpdate::NestedCallStarted {
                call_id,
                name,
                input,
            } => {
                let (call_id, call_index) = self.event_context(call_id);
                let result = self.events.emit(
                    AgentEventKind::ToolCall,
                    ToolCallEvent {
                        call_id: &call_id,
                        tool: name,
                        arguments: input,
                        model_call_index: call_index,
                    },
                );
                if result.is_ok() {
                    self.progress
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .nested_tool_calls += 1;
                }
                result
            }
            CodeModeUpdate::NestedCallCompleted(call) => {
                let (call_id, _) = self.event_context(&call.call_id);
                self.events.emit(
                    AgentEventKind::ToolResult,
                    ToolResultEvent {
                        call_id: &call_id,
                        tool: &call.name,
                        status: status(call.success),
                        duration_ns: call.duration_ns,
                        started_after_ns: Some(call.started_after_ns),
                        result: &call.output,
                        metadata: call.metadata.as_deref(),
                    },
                )
            }
        };
        if let Err(error) = result {
            self.error = Some(error.into());
        }
    }
}

impl NestedToolEventObserver<'_> {
    fn event_context(&self, nested_call_id: &str) -> (String, u32) {
        let embedded_parent = nested_call_id
            .rsplit_once("/code-")
            .map(|(parent, _)| parent);
        let original_parent = embedded_parent.unwrap_or(self.parent_call_id);
        let call_id = embedded_parent.map_or_else(
            || format!("{}/{nested_call_id}", self.parent_call_id),
            |_| nested_call_id.to_owned(),
        );
        let call_index = self
            .tool_call_indices
            .get(original_parent)
            .copied()
            .unwrap_or(self.fallback_call_index);
        (call_id, call_index)
    }
}

async fn execute_code_call(
    tools: &ToolRuntime,
    call: &CodeCall,
    owned_context: Option<OwnedToolContext>,
    session_id: &str,
    observer: &mut dyn CodeModeObserver,
    tool_span: &tracing::Span,
) -> CodeModeExecution {
    if let Some(context) = owned_context {
        tools
            .execute_code_owned_with_updates(&call.input, context, observer)
            .instrument(tool_span.clone())
            .await
    } else {
        let context = ToolContext::new(
            MODEL,
            session_id,
            &call.call_id,
            &[],
            DEFAULT_TOOL_OUTPUT_TOKENS,
        );
        tools
            .wait_for_code_with_updates(&call.input, context, observer)
            .instrument(tool_span.clone())
            .await
    }
}

impl<S> ModelRun<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<nanocodex_oai_api::ResponseError>,
    S::Future: AgentSend,
{
    pub(super) async fn execute_model_tools(
        &mut self,
        tools: &ToolRuntime,
        conversation: &mut ConversationState,
        call_index: u32,
        calls: Vec<CodeCall>,
        history: Option<Arc<Vec<ResponseItem>>>,
    ) -> Result<()> {
        self.active_tool_batch_started_at = Some(Instant::now());
        let mut prepared = Vec::with_capacity(calls.len());
        for call in calls {
            let active = self.prepare_model_tool_call(call_index, &call)?;
            let supports_parallel = tools.supports_parallel_tool_calls(&qualified_tool_name(&call));
            prepared.push((call, supports_parallel, active));
        }

        let gate = Arc::new(RwLock::new(()));
        let events = self.events.clone();
        let tool_call_indices = self.tool_call_indices.clone();
        let session_id = events.request_id().to_owned();
        let mut executions = prepared
            .into_iter()
            .map(|(call, supports_parallel, active)| {
                let gate = Arc::clone(&gate);
                let history = history.clone();
                let events = events.clone();
                let tool_call_indices = tool_call_indices.clone();
                let session_id = session_id.clone();
                async move {
                    let started_at = active.started_at;
                    let dispatch = async {
                        if supports_parallel {
                            let _guard = gate.read().await;
                            active
                                .execution_started_at
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .replace(Instant::now());
                            Self::execute_model_tool_call(
                                tools,
                                &events,
                                &tool_call_indices,
                                call_index,
                                call,
                                history,
                                &session_id,
                                started_at,
                                &active.progress,
                                &active.span,
                            )
                            .await
                        } else {
                            let _guard = gate.write().await;
                            active
                                .execution_started_at
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .replace(Instant::now());
                            Self::execute_model_tool_call(
                                tools,
                                &events,
                                &tool_call_indices,
                                call_index,
                                call,
                                history,
                                &session_id,
                                started_at,
                                &active.progress,
                                &active.span,
                            )
                            .await
                        }
                    };
                    let result = match AssertUnwindSafe(dispatch).catch_unwind().await {
                        Ok(result) => result,
                        Err(payload) => Ok(Self::panicked_tool_call(&active, payload)),
                    };
                    match result {
                        Ok(mut completed) => {
                            completed.work_duration_ns =
                                Self::completed_tool_work_duration(&active);
                            Self::emit_completed_tool_result(&events, &completed)?;
                            active
                                .completion
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .replace(completed);
                            Ok(active)
                        }
                        Err(error) => Err(error),
                    }
                }
            })
            .collect::<FuturesOrdered<_>>();
        while let Some(active) = executions.next().await {
            let active = active?;
            let Some(completed) = active
                .completion
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            else {
                return Err(NanocodexError::InvalidAttemptState {
                    detail: "completed tool call did not retain its output",
                });
            };
            let Some(active_index) = self
                .active_tool_calls
                .iter()
                .position(|candidate| Arc::ptr_eq(&candidate.completion, &active.completion))
            else {
                return Err(NanocodexError::InvalidAttemptState {
                    detail: "completed tool call was not active",
                });
            };
            self.active_tool_calls.remove(active_index);
            conversation.append(self.finish_completed_tool_call(completed, &active.progress)?);
        }
        self.finish_active_tool_batch_wall();
        Ok(())
    }

    pub(super) fn prepare_model_tool_call(
        &mut self,
        call_index: u32,
        call: &CodeCall,
    ) -> Result<ActiveToolCall> {
        self.tool_call_indices
            .insert(call.call_id.clone().into_boxed_str(), call_index);
        let qualified_name = qualified_tool_name(call);
        let arguments = if call.name == "exec" {
            ToolCallArguments::Text(&call.input)
        } else {
            serde_json::from_str::<&RawValue>(&call.input)
                .map_or(ToolCallArguments::Text(&call.input), ToolCallArguments::Raw)
        };
        self.events.emit(
            AgentEventKind::ToolCall,
            ToolCallEvent {
                call_id: &call.call_id,
                tool: &qualified_name,
                arguments,
                model_call_index: call_index,
            },
        )?;
        self.stats.tool_calls += 1;
        let started_at = Instant::now();
        let span = model_tool_span(call, call_index);
        record_span_content(&span, "tool.arguments", &call.input);
        let active = ActiveToolCall {
            call_id: call.call_id.clone(),
            name: qualified_name,
            kind: call.kind,
            started_at,
            shell_abort_format: call.namespace.is_none()
                && matches!(call.name.as_str(), "shell_command" | "unified_exec"),
            completion: Arc::new(Mutex::new(None)),
            progress: Arc::new(Mutex::new(ActiveToolProgress::default())),
            execution_started_at: Arc::new(Mutex::new(None)),
            span,
        };
        self.active_tool_calls.push(active.clone());
        Ok(active)
    }

    pub(super) fn finish_completed_tool_call(
        &mut self,
        completed: CompletedToolCall,
        progress: &Mutex<ActiveToolProgress>,
    ) -> Result<Vec<ResponseItem>> {
        self.stats.tool_work_duration_ns += completed.work_duration_ns;
        self.finish_active_tool_progress(progress);
        Ok(completed.response_items)
    }

    pub(super) fn finish_active_tool_progress(&mut self, progress: &Mutex<ActiveToolProgress>) {
        let progress = std::mem::take(
            &mut *progress
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        self.stats.tool_calls += progress.nested_tool_calls;
    }

    pub(super) fn completed_tool_work_duration(active: &ActiveToolCall) -> u64 {
        active
            .execution_started_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map_or(0, |started_at| elapsed_ns(*started_at))
    }

    pub(super) fn finish_cancelled_tool_work(&mut self, active: &ActiveToolCall) {
        let started_at = active
            .execution_started_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(started_at) = started_at {
            self.stats.tool_work_duration_ns += elapsed_ns(started_at);
        }
    }

    pub(super) fn finish_active_tool_batch_wall(&mut self) {
        if let Some(started_at) = self.active_tool_batch_started_at.take() {
            self.stats.tool_wall_duration_ns += elapsed_ns(started_at);
        }
    }

    pub(super) fn emit_completed_tool_result(
        events: &EventSink,
        completed: &CompletedToolCall,
    ) -> Result<()> {
        events.emit(
            AgentEventKind::ToolResult,
            ToolResultEvent {
                call_id: &completed.call_id,
                tool: &completed.tool,
                status: status(completed.success),
                duration_ns: completed.duration_ns,
                started_after_ns: None,
                result: &completed.output,
                metadata: completed.metadata.as_deref(),
            },
        )?;
        Ok(())
    }

    pub(super) fn panicked_tool_call(
        active: &ActiveToolCall,
        payload: Box<dyn Any + Send>,
    ) -> CompletedToolCall {
        let message = panic_payload(payload);
        record_span_content(&active.span, "tool.panic", &message);
        let output = ToolOutputBody::Text("aborted".to_owned());
        let duration_ns = elapsed_ns(active.started_at);
        record_tool_span_terminal(&active.span, "failed", "ERROR", duration_ns, &output);
        let response_item = match active.kind {
            CodeCallKind::Custom => custom_tool_output(active.call_id.clone(), output.clone()),
            CodeCallKind::Function => function_tool_output(active.call_id.clone(), output.clone()),
            CodeCallKind::ToolSearch => tool_search_output(active.call_id.clone(), Vec::new()),
        };
        CompletedToolCall {
            call_id: active.call_id.clone(),
            tool: active.name.clone(),
            success: false,
            duration_ns,
            work_duration_ns: Self::completed_tool_work_duration(active),
            output,
            metadata: None,
            response_items: vec![response_item],
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_model_tool_call(
        tools: &ToolRuntime,
        events: &EventSink,
        tool_call_indices: &HashMap<Box<str>, u32>,
        call_index: u32,
        call: CodeCall,
        history: Option<Arc<Vec<ResponseItem>>>,
        session_id: &str,
        started_at: Instant,
        progress: &Mutex<ActiveToolProgress>,
        tool_span: &tracing::Span,
    ) -> Result<CompletedToolCall> {
        let qualified_name = qualified_tool_name(&call);
        if let Some(message) = unsupported_tool_message(tools, &call) {
            let output = ToolOutputBody::Text(message);
            record_tool_span_terminal(tool_span, "failed", "ERROR", 0, &output);
            let response_item = match call.kind {
                CodeCallKind::Custom => custom_tool_output(call.call_id.clone(), output.clone()),
                CodeCallKind::Function => {
                    function_tool_output(call.call_id.clone(), output.clone())
                }
                CodeCallKind::ToolSearch => tool_search_output(call.call_id.clone(), Vec::new()),
            };
            return Ok(CompletedToolCall {
                call_id: call.call_id,
                tool: qualified_name,
                success: false,
                duration_ns: 0,
                work_duration_ns: 0,
                output,
                metadata: None,
                response_items: vec![response_item],
            });
        }
        if matches!(call.kind, CodeCallKind::Function) && call.namespace.is_some() {
            let context = ToolContext::new(
                MODEL,
                session_id,
                &call.call_id,
                &[],
                DEFAULT_TOOL_OUTPUT_TOKENS,
            );
            let execution = match RawValue::from_string(call.input.clone()) {
                Ok(input) => {
                    tools
                        .execute_tool(&qualified_name, ToolInput::Function(input), context)
                        .instrument(tool_span.clone())
                        .await
                }
                Err(error) => ToolOutput::error(format!(
                    "failed to encode {qualified_name} arguments: {error}"
                )),
            };
            if let Some(content) = serialize_trace_content(&execution.output) {
                record_span_content(tool_span, "tool.output", &content);
            }
            let duration_ns = elapsed_ns(started_at);
            tool_span.record("status", status(execution.success));
            tool_span.record("otel.status_code", otel_status(execution.success));
            tool_span.record("duration_ns", duration_ns);
            return Ok(CompletedToolCall {
                call_id: call.call_id.clone(),
                tool: qualified_name,
                success: execution.success,
                duration_ns,
                work_duration_ns: 0,
                response_items: vec![function_tool_output(call.call_id, execution.output.clone())],
                output: execution.output,
                metadata: execution.metadata,
            });
        }
        if matches!(call.kind, CodeCallKind::ToolSearch) {
            let search_history = history.as_deref().map_or(&[][..], Vec::as_slice);
            let context = ToolContext::new(
                MODEL,
                session_id,
                &call.call_id,
                search_history,
                DEFAULT_TOOL_OUTPUT_TOKENS,
            );
            let execution = match RawValue::from_string(call.input.clone()) {
                Ok(input) => {
                    tools
                        .execute_tool("tool_search", ToolInput::Function(input), context)
                        .instrument(tool_span.clone())
                        .await
                }
                Err(error) => {
                    ToolOutput::error(format!("failed to encode tool_search arguments: {error}"))
                }
            };
            if let Some(content) = serialize_trace_content(&execution.output) {
                record_span_content(tool_span, "tool.output", &content);
            }
            let duration_ns = elapsed_ns(started_at);
            tool_span.record("status", status(execution.success));
            tool_span.record("otel.status_code", otel_status(execution.success));
            tool_span.record("duration_ns", duration_ns);
            let tools = if execution.success {
                match execution.code_mode_value() {
                    Value::Array(tools) => tools,
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };
            return Ok(CompletedToolCall {
                call_id: call.call_id.clone(),
                tool: qualified_name,
                success: execution.success,
                duration_ns,
                work_duration_ns: 0,
                response_items: vec![tool_search_output(call.call_id, tools)],
                output: execution.output,
                metadata: execution.metadata,
            });
        }
        let owned_context = owned_code_context(&call, history, session_id)?;
        let mut observer = NestedToolEventObserver {
            events,
            tool_call_indices,
            progress,
            fallback_call_index: call_index,
            parent_call_id: &call.call_id,
            error: None,
        };
        let mut execution = execute_code_call(
            tools,
            &call,
            owned_context,
            session_id,
            &mut observer,
            tool_span,
        )
        .await;
        let update_error = observer.error.take();
        drop(observer);
        if let Some(error) = update_error {
            return Err(error);
        }
        prepare_output_images(&mut execution.output).await;
        if let Some(content) = serialize_trace_content(&execution.output) {
            record_span_content(tool_span, "tool.output", &content);
        }
        let duration_ns = elapsed_ns(started_at);
        tool_span.record("status", status(execution.success));
        tool_span.record("otel.status_code", otel_status(execution.success));
        tool_span.record("duration_ns", duration_ns);
        let output = match call.kind {
            CodeCallKind::Custom => {
                custom_tool_output(call.call_id.clone(), execution.output.clone())
            }
            CodeCallKind::Function => {
                function_tool_output(call.call_id.clone(), execution.output.clone())
            }
            CodeCallKind::ToolSearch => {
                unreachable!("native tool_search returned through Code Mode")
            }
        };
        let mut outputs = Vec::with_capacity(execution.notifications.len() + 1);
        outputs.push(output);
        outputs.extend(
            execution.notifications.into_iter().map(|notification| {
                custom_tool_notification(notification.call_id, notification.text)
            }),
        );
        Ok(CompletedToolCall {
            call_id: call.call_id,
            tool: qualified_name,
            success: execution.success,
            duration_ns,
            work_duration_ns: 0,
            output: execution.output,
            metadata: None,
            response_items: outputs,
        })
    }
}
