use super::*;

impl<S> ModelRun<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<nanocodex_oai_api::ResponseError>,
    S::Future: AgentSend,
{
    pub(crate) async fn compact(
        &mut self,
        requested_workspace: Option<Arc<str>>,
        thinking: Thinking,
        fast_mode: bool,
        logical_turn: u64,
        cancel: &mut tokio::sync::oneshot::Receiver<()>,
        execution_steps: Option<ExecutionSteps>,
    ) -> Result<ModelCompactOutcome> {
        self.execution_steps = execution_steps;
        self.thinking = thinking;
        self.fast_mode = fast_mode;
        self.started_at = Instant::now();
        self.stats = RunStats::default();
        let mut session = match self.session.take() {
            Some(session) => session,
            None => self.empty_session(requested_workspace.as_deref())?,
        };
        session.factory = session.factory.for_logical_turn(logical_turn);
        if let Err(error) = session.validate_workspace(requested_workspace.as_deref()) {
            let checkpoint =
                Self::checkpoint_from_session(&session, false, self.global_instructions.clone());
            self.session = Some(session);
            return Ok(ModelCompactOutcome::Failed { error, checkpoint });
        }
        session
            .conversation
            .prepare_request_policy(self.continuation_policy());

        let active_context_tokens = session.conversation.active_context_tokens();
        let previous_response_id = session
            .conversation
            .previous_response_id()
            .map(str::to_owned);
        let auto_compact_token_limit =
            compaction::auto_compact_token_limit(self.model, self.config.context_window_tokens);
        let compacted = {
            let compaction = self.perform_compaction(
                self.stats.model_calls,
                session.conversation.prompt_history(),
                session.conversation.delta_start(),
                previous_response_id.as_deref(),
                active_context_tokens,
                auto_compact_token_limit,
                &session.factory,
            );
            tokio::pin!(compaction);
            tokio::select! {
                biased;
                _ = &mut *cancel => None,
                outcome = &mut compaction => Some(outcome),
            }
        };
        let Some(compacted) = compacted else {
            session.conversation.reset_for_full_request();
            let checkpoint =
                Self::checkpoint_from_session(&session, false, self.global_instructions.clone());
            self.session = Some(session);
            return Ok(ModelCompactOutcome::Cancelled(checkpoint));
        };
        let (item, _usage, server_reasoning_included) = match compacted {
            Ok(compacted) => compacted,
            Err(error) => {
                session.conversation.reset_for_full_request();
                let checkpoint = Self::checkpoint_from_session(
                    &session,
                    false,
                    self.global_instructions.clone(),
                );
                self.session = Some(session);
                return Ok(ModelCompactOutcome::Failed { error, checkpoint });
            }
        };
        session
            .conversation
            .observe_server_reasoning(server_reasoning_included);
        session
            .conversation
            .install_pre_turn_compaction(item, session.factory.profile().prefix());
        session.conversation.commit_tail();
        session.context.require_full_reinjection();
        session.preserve_inherited_delta = false;
        self.force_compaction = false;
        let checkpoint =
            Self::checkpoint_from_session(&session, false, self.global_instructions.clone());
        self.session = Some(session);
        Ok(ModelCompactOutcome::Completed(checkpoint))
    }

    pub(crate) fn emit_cancelled_before_start(
        &mut self,
        task: &Prompt,
        workspace: Option<&str>,
        thinking: Thinking,
        fast_mode: bool,
    ) -> Result<()> {
        self.thinking = thinking;
        self.fast_mode = fast_mode;
        self.started_at = Instant::now();
        self.stats = RunStats::default();
        self.events.emit(
            AgentEventKind::RunStarted,
            RunStarted {
                mode: "openai_model",
                model: self.model.as_str(),
                reasoning_mode: self.config.reasoning_mode.as_str(),
                effort: self.thinking.as_str(),
                transport: self.config.responses_transport.as_str(),
                orchestration: ModelConfig::orchestration(),
                websocket_url: display_endpoint(self.responses_endpoint()),
                workspace,
                instruction_bytes: task.text_bytes(),
            },
        )?;
        let error = NanocodexError::TurnCancelled;
        let message = error.to_string();
        self.events
            .emit(AgentEventKind::RunError, RunError { message: &message })?;
        let usage = self.stats.turn_usage(self.model, self.fast_mode);
        record_turn_usage(&tracing::Span::current(), &usage);
        self.events.emit(
            AgentEventKind::RunFailed,
            terminal_payload(
                "cancelled",
                self.started_at.elapsed(),
                &self.config,
                self.model,
                self.thinking,
                &self.stats,
                &usage,
            ),
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_failed_before_start(
        &mut self,
        task: &Prompt,
        workspace: Option<&str>,
        thinking: Thinking,
        fast_mode: bool,
        error: &NanocodexError,
    ) -> Result<()> {
        self.thinking = thinking;
        self.fast_mode = fast_mode;
        self.started_at = Instant::now();
        self.stats = RunStats::default();
        self.events.emit(
            AgentEventKind::RunStarted,
            RunStarted {
                mode: "openai_model",
                model: self.model.as_str(),
                reasoning_mode: self.config.reasoning_mode.as_str(),
                effort: self.thinking.as_str(),
                transport: self.config.responses_transport.as_str(),
                orchestration: ModelConfig::orchestration(),
                websocket_url: display_endpoint(self.responses_endpoint()),
                workspace,
                instruction_bytes: task.text_bytes(),
            },
        )?;
        let message = error.to_string();
        self.events
            .emit(AgentEventKind::RunError, RunError { message: &message })?;
        self.emit_terminal("failed")
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute(
        &mut self,
        task: Prompt,
        workspace: Option<Arc<str>>,
        thinking: Thinking,
        fast_mode: bool,
        logical_turn: u64,
        steers: tokio::sync::mpsc::Receiver<Prompt>,
        mut cancel: tokio::sync::oneshot::Receiver<()>,
        fork_snapshots: watch::Sender<Option<ModelCheckpoint>>,
        execution_steps: Option<ExecutionSteps>,
    ) -> Result<ModelTurnOutcome> {
        self.execution_steps = execution_steps;
        self.thinking = thinking;
        self.fast_mode = fast_mode;
        self.started_at = Instant::now();
        self.stats = RunStats::default();
        if let Some(tools) = &self.active_tools {
            tools.begin_turn();
        }
        let transport_before = self.transport_stats.snapshot();
        self.events.emit(
            AgentEventKind::RunStarted,
            RunStarted {
                mode: "openai_model",
                model: self.model.as_str(),
                reasoning_mode: self.config.reasoning_mode.as_str(),
                effort: self.thinking.as_str(),
                transport: self.config.responses_transport.as_str(),
                orchestration: ModelConfig::orchestration(),
                websocket_url: display_endpoint(self.responses_endpoint()),
                workspace: workspace.as_deref(),
                instruction_bytes: task.text_bytes(),
            },
        )?;

        let outcome = self
            .execute_task(
                task,
                workspace,
                logical_turn,
                steers,
                &mut cancel,
                &fork_snapshots,
            )
            .await;
        match outcome {
            Ok(ModelTaskOutcome::Completed(message)) => {
                self.stats
                    .apply_transport(self.transport_stats.since(transport_before));
                let usage = self.stats.turn_usage(self.model, self.fast_mode);
                record_turn_usage(&tracing::Span::current(), &usage);
                let checkpoint = self.commit_checkpoint()?;
                Ok(ModelTurnOutcome::Completed(CompletedModelTurn {
                    final_message: message,
                    usage,
                    checkpoint,
                }))
            }
            Ok(ModelTaskOutcome::Cancelled) => {
                if let Some(tools) = &self.active_tools {
                    tools.cancel_turn().await;
                }
                let checkpoint = self.commit_cancelled_checkpoint().await?;
                let error = NanocodexError::TurnCancelled;
                let message = error.to_string();
                self.events
                    .emit(AgentEventKind::RunError, RunError { message: &message })?;
                self.stats
                    .apply_transport(self.transport_stats.since(transport_before));
                let usage = self.stats.turn_usage(self.model, self.fast_mode);
                record_turn_usage(&tracing::Span::current(), &usage);
                Ok(ModelTurnOutcome::Cancelled(checkpoint))
            }
            Err(error) => {
                if error
                    .responses_error()
                    .is_some_and(ResponsesError::is_context_window_exceeded)
                {
                    // The provider is authoritative about the usable context
                    // window. Compact before the next model attempt even when
                    // local usage remains below the proactive threshold.
                    self.force_compaction = true;
                }
                let checkpoint = if self.active_tool_calls.is_empty() {
                    self.finish_active_tool_batch_wall();
                    // Retain client-authored state at its safe boundary, but
                    // drop the transport checkpoint: the provider may have
                    // observed the failed request without returning a usable
                    // continuation.
                    if let Some(session) = &mut self.session {
                        if error.responses_error().is_some_and(|source| {
                            matches!(source, ResponsesError::InvalidImageRequest { .. })
                        }) {
                            session.conversation.replace_rejected_images();
                        }
                        session.conversation.commit_interrupted();
                        session.preserve_inherited_delta = false;
                    }
                    self.session.as_ref().map(|session| {
                        Self::checkpoint_from_session(
                            session,
                            false,
                            self.global_instructions.clone(),
                        )
                    })
                } else {
                    // A tool-dispatch failure may leave sibling calls in
                    // flight. Stop their retained runtime work, preserve every
                    // completed slot, and synthesize outputs for only the
                    // unfinished calls before committing the failure boundary.
                    if let Some(tools) = &self.active_tools {
                        tools.cancel_turn().await;
                    }
                    Some(self.commit_interrupted_checkpoint()?)
                };
                let message = error.to_string();
                self.events
                    .emit(AgentEventKind::RunError, RunError { message: &message })?;
                self.stats
                    .apply_transport(self.transport_stats.since(transport_before));
                let usage = self.stats.turn_usage(self.model, self.fast_mode);
                record_turn_usage(&tracing::Span::current(), &usage);
                match checkpoint {
                    Some(checkpoint) => Ok(ModelTurnOutcome::Failed { error, checkpoint }),
                    None => Err(error),
                }
            }
        }
    }

    pub(crate) fn emit_terminal(&self, status: &'static str) -> Result<()> {
        let usage = self.stats.turn_usage(self.model, self.fast_mode);
        let kind = if status == "completed" {
            AgentEventKind::RunCompleted
        } else {
            AgentEventKind::RunFailed
        };
        self.events.emit(
            kind,
            terminal_payload(
                status,
                self.started_at.elapsed(),
                &self.config,
                self.model,
                self.thinking,
                &self.stats,
                &usage,
            ),
        )?;
        Ok(())
    }

    pub(super) async fn prepare_follow_on_turn(
        &mut self,
        session: &mut ModelSessionState,
        task: &Prompt,
        cancel: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> Result<bool> {
        let compacted = {
            let compaction = self.maybe_compact(
                self.stats.model_calls,
                &mut session.conversation,
                &session.factory,
                CompactionContext {
                    snapshot: session.context.snapshot(),
                    phase: CompactionPhase::PreTurn,
                },
            );
            tokio::pin!(compaction);
            tokio::select! {
                biased;
                _ = &mut *cancel => None,
                outcome = &mut compaction => Some(outcome?),
            }
        };
        let Some(compacted) = compacted else {
            let user_content = prepare_user_input(&task.instruction).await;
            session
                .conversation
                .append(prompt_messages(task, user_content));
            return Ok(false);
        };
        if compacted || session.preserve_inherited_delta {
            session.preserve_inherited_delta = false;
        } else {
            session.conversation.clear_delta();
        }
        if compacted {
            session.context.require_full_reinjection();
        }
        let current_context = session.context.capture(
            session.tools.working_directory(),
            session.tools.default_shell_name(),
            self.context_source.execution_environment(),
        );
        let canonical_context = current_context.full_item();
        if let Some(update) = session.context.update(current_context) {
            if update.full {
                session
                    .conversation
                    .append_canonical_context(developer_context(), update.item);
            } else {
                session.conversation.append([update.item]);
                session
                    .conversation
                    .set_canonical_context(canonical_context);
            }
        } else {
            session
                .conversation
                .set_canonical_context(canonical_context);
        }
        let user_content = prepare_user_input(&task.instruction).await;
        session
            .conversation
            .append(prompt_messages(task, user_content));
        Ok(true)
    }

    pub(super) async fn execute_task(
        &mut self,
        task: Prompt,
        requested_workspace: Option<Arc<str>>,
        logical_turn: u64,
        steers: tokio::sync::mpsc::Receiver<Prompt>,
        cancel: &mut tokio::sync::oneshot::Receiver<()>,
        fork_snapshots: &watch::Sender<Option<ModelCheckpoint>>,
    ) -> Result<ModelTaskOutcome> {
        let mut session = if let Some(mut session) = self.session.take() {
            session.factory = session.factory.for_logical_turn(logical_turn);
            if let Err(error) = session.validate_workspace(requested_workspace.as_deref()) {
                self.session = Some(session);
                return Err(error);
            }
            session
                .conversation
                .prepare_request_policy(self.continuation_policy());
            match self
                .prepare_follow_on_turn(&mut session, &task, cancel)
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    self.session = Some(session);
                    return Ok(ModelTaskOutcome::Cancelled);
                }
                Err(error) => {
                    self.session = Some(session);
                    return Err(error);
                }
            }
            session
        } else {
            // The owning driver resolves its workspace before accepting
            // prompts. Reuse that stable path instead of introducing a second
            // filesystem failure between acceptance and session creation.
            let workspace = requested_workspace.map_or_else(
                || self.context_source.resolve_workspace(None),
                |workspace| Ok(workspace.to_string()),
            )?;
            let selected_agents_md = self
                .context_source
                .project_instructions(&workspace)
                .map(Arc::<str>::from);
            let tools = tool_runtime(&workspace, &self.config, &self.tools);
            let tool_control = tools.control();
            tool_control.begin_turn();
            self.active_tools = Some(tool_control);
            let factory = self.attempt_factory(&tools)?.for_logical_turn(logical_turn);
            let user_content = prepare_user_input(&task.instruction).await;
            let mut context = ContextState::new(selected_agents_md, ContextBaseline::Missing);
            let context_snapshot = context.capture(
                tools.working_directory(),
                tools.default_shell_name(),
                self.context_source.execution_environment(),
            );
            let mut history = task_input(&task, user_content, &context_snapshot);
            if !self.pending_developer_messages.is_empty() {
                history.splice(2..2, self.pending_developer_messages.drain(..));
            }
            context.establish(context_snapshot);
            let conversation = ConversationState::new(history)?;
            let mut session = ModelSessionState {
                workspace,
                tools,
                factory,
                conversation,
                context,
                preserve_inherited_delta: false,
            };
            session
                .conversation
                .prepare_request_policy(self.continuation_policy());
            Self::publish_fork_snapshot(
                &mut session,
                fork_snapshots,
                self.global_instructions.as_ref(),
            );
            let warmup = {
                let warmup = self.perform_warmup(&session.factory);
                tokio::pin!(warmup);
                tokio::select! {
                    biased;
                    _ = &mut *cancel => None,
                    outcome = &mut warmup => Some(outcome),
                }
            };
            let Some(warmup) = warmup else {
                self.session = Some(session);
                return Ok(ModelTaskOutcome::Cancelled);
            };
            match warmup {
                Ok(outcome) => {
                    session
                        .conversation
                        .observe_server_reasoning(outcome.server_reasoning_included);
                    if let Some(response_id) = outcome.response_id {
                        session.conversation.set_previous_response_id(response_id);
                    } else {
                        session.conversation.reset_for_full_request();
                        self.stats.last_response_id = None;
                    }
                }
                Err(error) if error.responses_error().is_some() => {
                    session.conversation.reset_for_full_request();
                    self.stats.last_response_id = None;
                }
                Err(error) => {
                    self.session = Some(session);
                    return Err(error);
                }
            }
            session
        };

        let outcome = {
            let task = self.drive_session(&mut session, steers, fork_snapshots);
            tokio::pin!(task);
            tokio::select! {
                biased;
                _ = &mut *cancel => None,
                outcome = &mut task => Some(outcome),
            }
        };
        self.session = Some(session);
        match outcome {
            Some(outcome) => outcome.map(ModelTaskOutcome::Completed),
            None => Ok(ModelTaskOutcome::Cancelled),
        }
    }

    pub(super) const fn continuation_policy(&self) -> ContinuationPolicy {
        ContinuationPolicy {
            model: self.model,
            thinking: self.thinking,
            fast_mode: self.fast_mode,
        }
    }

    pub(super) fn commit_checkpoint(&mut self) -> Result<ModelCheckpoint> {
        let session = self
            .session
            .as_mut()
            .ok_or(NanocodexError::InvalidAttemptState {
                detail: "completed turn did not have a model session",
            })?;
        if self.stats.last_response_id.is_some() {
            session.conversation.commit()?;
        } else {
            // A journal-replayed model result is authoritative conversation
            // history, but its provider response ID belongs to the replaced
            // transport. Its output was already installed with a forced full
            // replay baseline, so commit that typed tail without restoring the
            // invalid response chain.
            session.conversation.commit_tail();
        }
        Ok(Self::checkpoint_from_session(
            session,
            false,
            self.global_instructions.clone(),
        ))
    }

    pub(super) fn commit_interrupted_checkpoint(&mut self) -> Result<ModelCheckpoint> {
        let mut aborted_outputs = Vec::with_capacity(self.active_tool_calls.len());
        for call in std::mem::take(&mut self.active_tool_calls) {
            let completed = call
                .completion
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(completed) = completed {
                aborted_outputs.extend(self.finish_completed_tool_call(completed, &call.progress)?);
                continue;
            }
            let duration_ns = elapsed_ns(call.started_at);
            let elapsed_seconds = call.started_at.elapsed().as_secs_f64();
            let output = ToolOutputBody::Text(if call.shell_abort_format {
                format!("Wall time: {elapsed_seconds:.1} seconds\naborted by user")
            } else {
                format!("aborted by user after {:.1}s", elapsed_seconds.max(0.1))
            });
            let structured_result = output.structured_result();
            self.finish_active_tool_progress(&call.progress);
            self.finish_cancelled_tool_work(&call);
            record_tool_span_terminal(&call.span, "cancelled", "ERROR", duration_ns, &output);
            self.events.emit(
                AgentEventKind::ToolResult,
                ToolResultEvent {
                    call_id: &call.call_id,
                    tool: &call.name,
                    status: "cancelled",
                    duration_ns,
                    started_after_ns: None,
                    result: &output,
                    structured_result: &structured_result,
                    metadata: None,
                },
            )?;
            aborted_outputs.push(match call.kind {
                CodeCallKind::Custom => custom_tool_output(call.call_id, output),
                CodeCallKind::Function => function_tool_output(call.call_id, output),
                CodeCallKind::ToolSearch => tool_search_output(call.call_id.clone(), Vec::new()),
            });
        }
        self.finish_active_tool_batch_wall();
        let session = self
            .session
            .as_mut()
            .ok_or(NanocodexError::InvalidAttemptState {
                detail: "interrupted turn did not have a model session",
            })?;
        session.conversation.append(aborted_outputs);
        session.conversation.append([turn_aborted()]);
        session.conversation.commit_interrupted();
        Ok(Self::checkpoint_from_session(
            session,
            false,
            self.global_instructions.clone(),
        ))
    }

    async fn commit_cancelled_checkpoint(&mut self) -> Result<ModelCheckpoint> {
        if let Some(steps) = self.execution_steps.clone() {
            for call in &self.active_tool_calls {
                if call.execution_retry != crate::agent::execution::ExecutionRetry::Never {
                    continue;
                }
                let mut unknown = Self::unknown_recovered_tool_call(call);
                let work_duration_ns = Self::completed_tool_work_duration(call);
                unknown.duration_ns = elapsed_ns(call.started_at);
                unknown.work_duration_ns = work_duration_ns;
                let reconciled = steps
                    .reconcile_cancelled(&call.execution_step_id, &unknown)
                    .await?;
                let crate::agent::ReconciledExecutionStep::Completed(completed) = reconciled else {
                    continue;
                };
                let mut retained = call
                    .completion
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if retained.is_none() {
                    if completed
                        .structured_result
                        .get("status")
                        .is_some_and(|status| status == "unknown")
                    {
                        record_tool_span_terminal(
                            &call.span,
                            "failed",
                            "ERROR",
                            completed.duration_ns,
                            &completed.output,
                        );
                    }
                    Self::emit_completed_tool_result(&self.events, &completed)?;
                }
                retained.replace(completed);
            }
        }
        self.commit_interrupted_checkpoint()
    }

    pub(super) fn checkpoint_from_session(
        session: &ModelSessionState,
        preserve_inherited_delta: bool,
        global_instructions: Option<Arc<str>>,
    ) -> ModelCheckpoint {
        ModelCheckpoint {
            workspace: session.workspace.clone(),
            conversation: session.conversation.clone(),
            request_prefix: session.factory.profile().shared_prefix(),
            prompt_cache_key: Arc::from(session.factory.profile().prompt_cache_key()),
            preserve_inherited_delta,
            global_instructions,
            context_baseline: session.context.baseline(),
        }
    }

    pub(super) fn publish_fork_snapshot(
        session: &mut ModelSessionState,
        snapshots: &watch::Sender<Option<ModelCheckpoint>>,
        global_instructions: Option<&Arc<str>>,
    ) {
        session.conversation.commit_tail();
        snapshots.send_replace(Some(ModelCheckpoint {
            workspace: session.workspace.clone(),
            conversation: session.conversation.clone(),
            request_prefix: session.factory.profile().shared_prefix(),
            prompt_cache_key: Arc::from(session.factory.profile().prompt_cache_key()),
            preserve_inherited_delta: true,
            global_instructions: global_instructions.cloned(),
            context_baseline: session.context.baseline(),
        }));
    }

    pub(super) async fn drive_session(
        &mut self,
        session: &mut ModelSessionState,
        mut steers: tokio::sync::mpsc::Receiver<Prompt>,
        fork_snapshots: &watch::Sender<Option<ModelCheckpoint>>,
    ) -> Result<String> {
        // Match Codex's ordering: always sample the turn's initial prompt once
        // before injecting input that arrived while that first request ran.
        let mut can_drain_steers = false;
        loop {
            if can_drain_steers {
                self.drain_steers(&mut session.conversation, &mut steers)
                    .await?;
            }
            Self::publish_fork_snapshot(session, fork_snapshots, self.global_instructions.as_ref());
            let call_index = self.stats.model_calls + 1;
            let model_call = self
                .perform_model_call(call_index, &mut session.conversation, &session.factory)
                .await?;
            let ModelCallOutcome {
                response,
                transport_continuation_valid,
            } = model_call;
            session
                .conversation
                .update_token_info(response.usage.as_ref());
            if transport_continuation_valid {
                session
                    .conversation
                    .set_previous_response_id(response.id.clone());
                if session.conversation.previous_response_id().is_none() {
                    return Err(NanocodexError::MalformedResponse {
                        detail: "completed turn did not have a response ID",
                    });
                }
            } else {
                session.conversation.reset_for_full_request();
            }
            let end_turn = response.end_turn;
            let final_message = response.final_message;
            let code_calls = response.code_calls;
            session.conversation.append(response.output_items);
            can_drain_steers = true;

            if code_calls.is_empty() {
                if end_turn == Some(false) {
                    session.conversation.clear_delta();
                    let compacted = self
                        .maybe_compact(
                            call_index,
                            &mut session.conversation,
                            &session.factory,
                            CompactionContext {
                                snapshot: session.context.snapshot(),
                                phase: CompactionPhase::MidTurn,
                            },
                        )
                        .await?;
                    // After a mid-turn compaction, resume the model-requested
                    // continuation before injecting newer steering input.
                    can_drain_steers = !compacted;
                    continue;
                }
                if !steers.is_empty() {
                    // The completed response is retained by previous_response_id;
                    // the next delta contains only newly drained steer messages.
                    session.conversation.clear_delta();
                    self.maybe_compact(
                        call_index,
                        &mut session.conversation,
                        &session.factory,
                        CompactionContext {
                            snapshot: session.context.snapshot(),
                            phase: CompactionPhase::MidTurn,
                        },
                    )
                    .await?;
                    continue;
                }
                if let Some(message) = final_message {
                    return Ok(if message.trim().is_empty() {
                        "The model completed without emitting assistant text.".to_owned()
                    } else {
                        message
                    });
                }
                return Err(NanocodexError::MalformedResponse {
                    detail: "model completed without a final message or exec call",
                });
            }

            session.conversation.clear_delta();
            let history = code_calls
                .iter()
                .any(|call| call.name == "exec")
                .then(|| Arc::new(session.conversation.flattened_history()));
            self.execute_model_tools(
                &session.tools,
                &mut session.conversation,
                call_index,
                code_calls,
                history,
            )
            .await?;
            let compacted = self
                .maybe_compact(
                    call_index,
                    &mut session.conversation,
                    &session.factory,
                    CompactionContext {
                        snapshot: session.context.snapshot(),
                        phase: CompactionPhase::MidTurn,
                    },
                )
                .await?;
            // Codex resumes a model/tool continuation immediately after
            // compaction, then drains steering at the following boundary.
            can_drain_steers = !compacted;
        }
    }

    pub(super) async fn drain_steers(
        &mut self,
        conversation: &mut ConversationState,
        steers: &mut tokio::sync::mpsc::Receiver<Prompt>,
    ) -> Result<()> {
        while let Ok(steer) = steers.try_recv() {
            if trace_content_enabled()
                && let Ok(content) = serde_json::to_string(&steer)
            {
                info!(
                    target: "nanocodex",
                    content_kind = "steer",
                    content = content.as_str(),
                    "turn content"
                );
            }
            let instruction_bytes = steer.text_bytes();
            let user_content = prepare_user_input(&steer.instruction).await;
            conversation.append(prompt_messages(&steer, user_content));
            self.stats.steers += 1;
            self.events.emit(
                AgentEventKind::RunSteered,
                RunSteered {
                    steer_index: self.stats.steers,
                    instruction_bytes,
                },
            )?;
        }
        Ok(())
    }
}
