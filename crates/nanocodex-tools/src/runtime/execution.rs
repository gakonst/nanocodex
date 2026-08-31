use super::*;

/// Stateful execution runtime for one agent driver.
///
/// A runtime retains Code Mode cells and shell sessions across calls. It is
/// normally owned privately by the higher-level agent driver.
pub struct ToolRuntime {
    pub(super) registry: Arc<ToolRegistry>,
    exposure: Option<ToolExposure>,
    deferred_tools_guidance_enabled: bool,
    code_mode: code_mode::CodeModeRuntime,
    sessions: Arc<ShellSessions>,
    current_turn: Arc<AtomicU64>,
    default_shell_name: Arc<str>,
    working_directory: Arc<str>,
}

#[doc(hidden)]
#[derive(Clone)]
pub struct ToolRuntimeControl {
    code_mode: code_mode::CodeModeControl,
    sessions: Arc<ShellSessions>,
    current_turn: Arc<AtomicU64>,
}

impl ToolRuntime {
    /// Creates a runtime with the standard workspace tools enabled.
    ///
    /// Pass `None` for web search or image generation to omit that built-in
    /// HTTP handler.
    pub fn new(
        workspace: impl Into<PathBuf>,
        web_search: Option<WebSearchConfig>,
        image_generation: Option<ImageGenerationConfig>,
    ) -> Self {
        Self::new_inner(
            workspace,
            web_search,
            image_generation,
            true,
            false,
            Arc::new(Vec::new()),
            None,
        )
    }

    /// Builds the runtime from one complete declarative tool selection.
    #[must_use]
    pub fn new_with_tools(
        workspace: impl Into<PathBuf>,
        web_search: Option<WebSearchConfig>,
        image_generation: Option<ImageGenerationConfig>,
        tools: &Tools,
    ) -> Self {
        let workspace = tools
            .workspace_tools
            .as_ref()
            .map(|workspace| workspace.root.clone())
            .unwrap_or_else(|| workspace.into());
        Self::new_inner(
            workspace,
            web_search,
            image_generation,
            tools.workspace_enabled(),
            tools.plan_enabled(),
            tools.process_environment(),
            tools.remote_http_client(),
        )
        .with_tools(tools)
    }

    fn new_inner(
        workspace: impl Into<PathBuf>,
        web_search: Option<WebSearchConfig>,
        image_generation: Option<ImageGenerationConfig>,
        workspace_enabled: bool,
        plan_enabled: bool,
        process_environment: Arc<Vec<(OsString, OsString)>>,
        remote_http_client: Option<reqwest::Client>,
    ) -> Self {
        nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
        let workspace = workspace.into();
        let current_turn = Arc::new(AtomicU64::new(0));
        let sessions = Arc::new(ShellSessions::with_environment(process_environment));
        let default_shell_name = Arc::from(sessions.default_shell_name());
        let working_directory = Arc::from(workspace.to_string_lossy().into_owned());
        let code_mode_workspace = workspace.clone();
        let mut handlers: Vec<Arc<dyn Tool>> = Vec::new();
        if workspace_enabled {
            handlers.extend([
                Arc::new(apply_patch::ApplyPatchHandler::new(workspace.clone())) as Arc<dyn Tool>,
                Arc::new(shell::ExecCommandHandler::new(
                    workspace.clone(),
                    Arc::clone(&sessions),
                )),
                Arc::new(view_image::ViewImageHandler::new(workspace)),
                Arc::new(shell::WriteStdinHandler::new(Arc::clone(&sessions))),
            ]);
        }
        if plan_enabled {
            handlers.push(Arc::new(plan::UpdatePlanTool::new()));
        }
        let remote_http_client = remote_http_client.unwrap_or_default();
        if let Some(web_search) = web_search {
            handlers.push(Arc::new(web_search::WebSearchHandler::with_client(
                web_search,
                remote_http_client.clone(),
            )));
        }
        if let Some(image_generation) = image_generation {
            handlers.push(Arc::new(
                image_generation::ImageGenerationHandler::with_client(
                    image_generation,
                    remote_http_client,
                ),
            ));
        }
        Self {
            registry: Arc::new(ToolRegistry::from_ordered(handlers)),
            exposure: None,
            deferred_tools_guidance_enabled: false,
            code_mode: code_mode::CodeModeRuntime::new_with_turn(
                code_mode_workspace,
                Arc::clone(&current_turn),
            ),
            sessions,
            current_turn,
            default_shell_name,
            working_directory,
        }
    }

    fn with_tools(mut self, tools: &Tools) -> Self {
        tools.start_providers();
        let registry = Arc::make_mut(&mut self.registry);
        if self.exposure.is_none() {
            let exposure = tools.exposure();
            self.exposure = Some(exposure);
            registry.set_all_exposures(exposure);
        }
        self.deferred_tools_guidance_enabled |= tools.deferred_tools_guidance_enabled;
        registry.extend(tools.registered.iter().map(|tool| {
            (
                Arc::clone(&tool.handler),
                tool.exposure.unwrap_or_else(|| tools.exposure()),
            )
        }));
        registry.extend(
            tools
                .provider_direct
                .iter()
                .cloned()
                .map(|tool| (tool, tools.exposure())),
        );
        registry.providers.extend(tools.providers.iter().cloned());
        if let Some(working_directory) = &tools.working_directory {
            self.working_directory = Arc::clone(working_directory);
        }
        if let Some(default_shell) = &tools.default_shell {
            self.default_shell_name = Arc::clone(default_shell);
        }
        self
    }

    /// Returns the shell name described to the model.
    #[must_use]
    pub fn default_shell_name(&self) -> &str {
        &self.default_shell_name
    }

    /// Returns the working directory described to the model.
    #[must_use]
    pub fn working_directory(&self) -> &str {
        &self.working_directory
    }

    #[cfg(test)]
    pub(crate) async fn has_shell_session(&self, session_id: i64) -> bool {
        self.sessions.contains(session_id).await
    }

    #[cfg(test)]
    pub(crate) async fn hold_code_mode_admission(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.code_mode.hold_admission().await
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_code_mode_admission_attempt(&self) {
        self.code_mode.wait_for_admission_attempt().await;
    }

    #[doc(hidden)]
    #[must_use]
    pub fn control(&self) -> ToolRuntimeControl {
        ToolRuntimeControl {
            code_mode: self.code_mode.control(),
            sessions: Arc::clone(&self.sessions),
            current_turn: Arc::clone(&self.current_turn),
        }
    }

    /// Returns the direct model-visible Code Mode tool definitions.
    ///
    /// Native definitions are session-independent. The session ID keeps this
    /// method aligned with hosted runtimes whose available tools may vary by
    /// session.
    #[must_use]
    pub fn model_specs(&self, _session_id: &str) -> Vec<ToolDefinition> {
        let mut nested = self.registry.registered_code_mode_definitions();
        let provider_summaries = self.registry.code_mode_tool_summaries();
        let mut direct = self
            .registry
            .direct_definitions()
            .filter(|definition| !matches!(definition, ToolDefinition::ToolSearch { .. }))
            .cloned()
            .collect::<Vec<_>>();
        let code_mode_names = nested
            .iter()
            .map(|definition| definition.name().to_owned())
            .collect::<HashSet<_>>();
        direct = group_direct_code_mode_definitions(direct, &code_mode_names);
        crate::code_mode_order::sort_direct_definitions(&mut direct);
        crate::code_mode_order::sort_definitions(&mut nested);
        let mut native = vec![
            code_mode::exec_spec(
                &nested,
                &provider_summaries,
                self.deferred_tools_guidance_enabled,
                self.exposure.unwrap_or_default() == ToolExposure::CodeModeOnly,
            ),
            code_mode::wait_spec(),
        ];
        native.append(&mut direct);
        native.extend(
            self.registry
                .definitions()
                .iter()
                .filter(|definition| matches!(definition, ToolDefinition::ToolSearch { .. }))
                .cloned(),
        );
        native
    }

    pub(crate) fn model_contract(
        &self,
        session_id: &str,
    ) -> (Vec<ToolDefinition>, Vec<(String, String)>) {
        (
            self.model_specs(session_id),
            self.registry.code_mode_tool_names(),
        )
    }

    /// Returns whether a model-visible tool explicitly permits parallel calls.
    ///
    /// Unknown tools and tools without an explicit opt-in return `false`.
    #[must_use]
    pub fn supports_parallel_tool_calls(&self, name: &str) -> bool {
        self.registry.supports_parallel_tool_calls(name)
    }

    /// Returns whether a registered or dynamically activated tool is callable.
    ///
    /// Deferred provider tools become visible here only after activation.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.registry.contains(name)
    }

    /// Starts or resumes a Code Mode cell and observes its first terminal boundary.
    pub async fn execute_code(&self, source: &str, context: ToolContext<'_>) -> CodeModeExecution {
        self.code_mode
            .execute(
                source,
                Arc::clone(&self.registry),
                OwnedToolContext::from_context(context),
            )
            .await
    }

    #[doc(hidden)]
    pub async fn execute_code_with_updates(
        &self,
        source: &str,
        context: ToolContext<'_>,
        observer: &mut dyn CodeModeObserver,
    ) -> CodeModeExecution {
        self.code_mode
            .execute_with_updates(
                source,
                Arc::clone(&self.registry),
                OwnedToolContext::from_context(context),
                observer,
            )
            .await
    }

    /// Executes Code Mode without copying an already-owned history snapshot.
    #[doc(hidden)]
    pub async fn execute_code_owned(
        &self,
        source: &str,
        context: OwnedToolContext,
    ) -> CodeModeExecution {
        self.code_mode
            .execute(source, Arc::clone(&self.registry), context)
            .await
    }

    #[doc(hidden)]
    pub async fn execute_code_owned_with_updates(
        &self,
        source: &str,
        context: OwnedToolContext,
        observer: &mut dyn CodeModeObserver,
    ) -> CodeModeExecution {
        self.code_mode
            .execute_with_updates(source, Arc::clone(&self.registry), context, observer)
            .await
    }

    /// Waits for a previously yielded Code Mode cell.
    pub async fn wait_for_code(&self, input: &str, context: ToolContext<'_>) -> CodeModeExecution {
        self.code_mode.wait(input, context).await
    }

    #[doc(hidden)]
    pub async fn wait_for_code_with_updates(
        &self,
        input: &str,
        _context: ToolContext<'_>,
        observer: &mut dyn CodeModeObserver,
    ) -> CodeModeExecution {
        self.code_mode.wait_with_updates(input, observer).await
    }

    /// Executes one registered or dynamically activated tool through this
    /// runtime's retained state.
    ///
    /// Shell sessions created by `exec_command` remain available to later
    /// `write_stdin` calls on the same runtime, including after cancellation
    /// of the turn that started or last observed them.
    ///
    /// Handler panics become failed `aborted` outputs and never unwind through
    /// the runtime owner.
    pub async fn execute_tool(
        &self,
        name: &str,
        input: ToolInput,
        context: ToolContext<'_>,
    ) -> ToolOutput {
        self.registry.execute_direct(name, input, context).await
    }
}

fn group_direct_code_mode_definitions(
    definitions: Vec<ToolDefinition>,
    code_mode_names: &HashSet<String>,
) -> Vec<ToolDefinition> {
    let mut grouped = Vec::<ToolDefinition>::new();
    for definition in definitions {
        let canonical_name = definition.name().to_owned();
        let mut definition = if code_mode_names.contains(&canonical_name) {
            crate::code_mode_description::augment_definition_for_code_mode(definition)
        } else {
            definition
        };
        let Some((namespace, name)) = canonical_name.rsplit_once("__") else {
            grouped.push(definition);
            continue;
        };
        if namespace.is_empty() || name.is_empty() {
            grouped.push(definition);
            continue;
        }
        let ToolDefinition::Function {
            name: direct_name, ..
        } = &mut definition
        else {
            grouped.push(definition);
            continue;
        };
        *direct_name = name.into();
        if let Some(ToolDefinition::Namespace { tools, .. }) = grouped.iter_mut().find(
            |group| matches!(group, ToolDefinition::Namespace { name, .. } if &**name == namespace),
        ) {
            tools.push(definition);
        } else {
            grouped.push(ToolDefinition::namespace(
                namespace,
                format!("Tools in the {namespace} namespace."),
                [definition],
            ));
        }
    }
    grouped
}

impl ToolRuntimeControl {
    #[doc(hidden)]
    pub fn begin_turn(&self) {
        let _ = self
            .current_turn
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |turn| {
                Some(turn.saturating_add(1))
            });
    }

    #[doc(hidden)]
    pub async fn cancel_turn(&self) {
        let turn_id = self.current_turn.load(Ordering::Acquire);
        // Shell sessions are owned by this runtime once spawned, not by the
        // turn that launched or observed them. Full runtime cancellation below
        // remains their explicit cleanup boundary.
        self.code_mode.terminate_turn(turn_id).await;
    }

    #[doc(hidden)]
    pub async fn cancel(&self) {
        // Code Mode cells can still be inside a nested exec_command. Quiesce
        // and join every producer before draining the session-owned shells so
        // no late registration can escape the shutdown boundary.
        let _code_mode_quiescence = self.code_mode.terminate_all().await;
        self.sessions.terminate_all().await;
    }
}

pub(super) fn record_tool_content(span: &tracing::Span, kind: &'static str, content: &str) {
    span.in_scope(|| {
        info!(
            target: "nanocodex_tools",
            content_kind = kind,
            content,
            "tool content"
        );
    });
}

pub(super) fn panicked_tool_output(
    span: &tracing::Span,
    payload: Box<dyn Any + Send>,
) -> ToolOutput {
    let message = panic_payload(payload);
    record_tool_content(span, "tool.panic", &message);
    ToolOutput::error("aborted")
}

fn panic_payload(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast::<&'static str>().map_or_else(
            |_| "non-string panic payload".to_owned(),
            |message| (*message).to_owned(),
        ),
    }
}

pub(super) fn tool_execution_span(
    name: &str,
    context: ToolContext<'_>,
    arguments_bytes: usize,
    arguments_kind: &'static str,
    arguments_count: usize,
    argument_keys: &str,
) -> tracing::Span {
    info_span!(
        target: "nanocodex_tools",
        "tool.execute",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        tool.name = name,
        session.id = context.session_id(),
        tool.call_id = context.call_id(),
        tool.arguments.bytes = arguments_bytes,
        tool.arguments.kind = arguments_kind,
        tool.arguments.count = arguments_count,
        tool.arguments.keys = argument_keys,
        process.exit.code = tracing::field::Empty,
        process.running = tracing::field::Empty,
        process.wall_time_ms = tracing::field::Empty,
        shell.session.id = tracing::field::Empty,
        tool.output.bytes = tracing::field::Empty,
        tool.output.original_tokens = tracing::field::Empty,
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    )
}

pub(super) fn finish_tool_execution_span(
    span: &tracing::Span,
    started_at: std::time::Instant,
    execution: &ToolOutput,
    output_content: Option<&str>,
) {
    if let Some(output_content) = output_content {
        record_tool_content(span, "tool.output", output_content);
        span.record("tool.output.bytes", output_content.len());
    }
    span.record(
        "status",
        if execution.success {
            "completed"
        } else {
            "failed"
        },
    );
    span.record(
        "otel.status_code",
        if execution.success { "OK" } else { "ERROR" },
    );
    span.record(
        "duration_ns",
        u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
    );
    if let Some(process) = execution.process_trace() {
        if let Some(exit_code) = process.exit_code {
            span.record("process.exit.code", exit_code);
        }
        span.record("process.running", process.session_id.is_some());
        span.record("process.wall_time_ms", process.wall_time_seconds * 1_000.0);
        if let Some(session_id) = process.session_id {
            span.record("shell.session.id", session_id);
        }
        span.record("tool.output.bytes", process.output_bytes);
        if let Some(original_token_count) = process.original_token_count {
            span.record("tool.output.original_tokens", original_token_count);
        }
    }
}
