use super::*;

pub(in crate::agent) struct BranchSpawner<S> {
    pub(in crate::agent) config: Arc<ModelConfig>,
    pub(in crate::agent) tools: ToolsConfiguration,
    pub(in crate::agent) lineage_id: Arc<str>,
    pub(in crate::agent) provider_session_id: Arc<str>,
    pub(in crate::agent) prompt_cache_key: Option<Arc<str>>,
    pub(in crate::agent) shared_prompt_cache: Option<SharedPromptCache>,
    pub(in crate::agent) context_config: ContextSourceConfig,
    pub(in crate::agent) context_source: ContextSource,
    pub(in crate::agent) depth: u32,
    pub(in crate::agent) execution: ExecutionConfig,
    pub(in crate::agent) restored_snapshot: Option<SessionSnapshot>,
    pub(in crate::agent) host_context: Option<Arc<str>>,
    pub(in crate::agent) service_factory: ServiceFactory<S>,
}

#[derive(Clone)]
pub(in crate::agent) struct AgentOrigin {
    pub(in crate::agent) kind: &'static str,
    pub(in crate::agent) depth: u32,
    pub(in crate::agent) parent_session_id: Option<Arc<str>>,
}

impl<S> BranchSpawner<S> {
    fn for_new_thread(&self, operation: &'static str) -> Result<Self> {
        Ok(self.with_execution(self.execution.for_new_thread(operation)?))
    }

    fn with_execution(&self, execution: ExecutionConfig) -> Self {
        Self {
            config: Arc::clone(&self.config),
            tools: self.tools.clone(),
            lineage_id: Arc::clone(&self.lineage_id),
            provider_session_id: Arc::clone(&self.provider_session_id),
            prompt_cache_key: self.prompt_cache_key.as_ref().map(Arc::clone),
            shared_prompt_cache: self.shared_prompt_cache.clone(),
            context_config: self.context_config.clone(),
            context_source: self.context_source.clone(),
            depth: self.depth,
            execution,
            restored_snapshot: None,
            host_context: self.host_context.as_ref().map(Arc::clone),
            service_factory: Arc::clone(&self.service_factory),
        }
    }
}

impl<S> BranchSpawner<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<ResponseError> + AgentSend + 'static,
    S::Future: AgentSend,
{
    pub(super) fn spawn_fork(
        &self,
        checkpoint: &CommittedSession,
        parent_session_id: &str,
        model: Model,
        thinking: Thinking,
        fast_mode: bool,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        let session_id = SessionId::new();
        let workspace = Some(Arc::<str>::from(checkpoint.model().workspace()));
        let mut spawner = self.for_new_thread("fork")?;
        spawner.context_source = spawner.context_config.build();
        spawner.host_context = host_context;
        let mut config = (*spawner.config).clone();
        config.model = model;
        config.thinking = thinking;
        config.fast_mode = fast_mode;
        spawner.config = Arc::new(config);
        spawner.depth = self.depth.saturating_add(1);
        let service = (spawner.service_factory)(Arc::clone(&spawner.config));
        spawn_agent_driver(
            spawner,
            session_id,
            workspace,
            service,
            Some(InitialResume::Exact(Box::new(checkpoint.model().clone()))),
            AgentOrigin {
                kind: "fork",
                depth: self.depth.saturating_add(1),
                parent_session_id: Some(Arc::from(parent_session_id)),
            },
        )
    }

    pub(super) fn spawn_clean(
        &self,
        workspace: Option<Arc<str>>,
        parent_session_id: &str,
        model: Model,
        thinking: Thinking,
        fast_mode: bool,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        let session_id = SessionId::new();
        let session_id_text = session_id.to_string();
        let depth = self.depth.saturating_add(1);
        let mut config = (*self.config).clone();
        config.model = model;
        config.thinking = thinking;
        config.fast_mode = fast_mode;
        let prompt_cache_key = self
            .prompt_cache_key
            .as_ref()
            .map_or_else(|| Arc::clone(&self.lineage_id), Arc::clone);
        let spawner = Self {
            config: Arc::new(config),
            tools: self.tools.clone(),
            lineage_id: Arc::from(session_id_text.as_str()),
            provider_session_id: Arc::clone(&self.provider_session_id),
            prompt_cache_key: Some(prompt_cache_key),
            shared_prompt_cache: self.shared_prompt_cache.clone(),
            context_config: self.context_config.clone(),
            context_source: self.context_config.build(),
            depth,
            execution: self.execution.for_new_thread("spawn")?,
            restored_snapshot: None,
            host_context,
            service_factory: Arc::clone(&self.service_factory),
        };
        let service = (spawner.service_factory)(Arc::clone(&spawner.config));
        spawn_agent_driver(
            spawner,
            session_id,
            workspace,
            service,
            None,
            AgentOrigin {
                kind: "spawn",
                depth,
                parent_session_id: Some(Arc::from(parent_session_id)),
            },
        )
    }

    pub(super) fn restore_child(
        &self,
        snapshot: ChildRuntimeSnapshot,
        workspace: Option<Arc<str>>,
        parent_session_id: &str,
        host_context: Option<Arc<str>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        snapshot.validate()?;
        let session_id = snapshot.session_id.parse::<SessionId>().map_err(|error| {
            NanocodexError::InvalidSessionSnapshot(format!("invalid child session ID: {error}"))
        })?;
        // Restore under the retained child's identity and checkpoint, never
        // under a fresh-spawn recipe or the parent's execution owner.
        let mut spawner = self.with_execution(
            self.execution
                .for_restored_thread(snapshot.conversation.as_ref())?,
        );
        spawner.restored_snapshot = snapshot.conversation.clone();
        spawner.depth = self.depth.saturating_add(1);
        spawner.context_source = spawner.context_config.build();
        spawner.host_context = host_context;
        spawner.lineage_id = Arc::from(snapshot.session_id);
        let mut config = (*spawner.config).clone();
        config.model = snapshot.model;
        config.thinking = snapshot.thinking;
        config.fast_mode = snapshot.fast_mode;
        validate_model_thinking(config.model, config.thinking)?;
        validate_model_reasoning_mode(config.model, config.reasoning_mode)?;
        config.context_window_tokens = config
            .context_window_tokens
            .min(config.model.max_context_window_tokens());
        spawner.config = Arc::new(config);
        let workspace = snapshot
            .conversation
            .as_ref()
            .map(|conversation| Arc::<str>::from(conversation.workspace()))
            .or(workspace);
        if let Some(workspace) = &workspace {
            let resolved = spawner.context_source.resolve_workspace(Some(workspace))?;
            if resolved != workspace.as_ref() {
                return Err(NanocodexError::InvalidSessionSnapshot(
                    "child workspace no longer resolves to the stored location".into(),
                ));
            }
        }
        let initial = snapshot
            .conversation
            .map(|conversation| -> Result<InitialResume> {
                let resume = conversation.into_resume()?;
                spawner.lineage_id = Arc::clone(&resume.lineage_id);
                spawner.prompt_cache_key = Some(Arc::clone(&resume.prompt_cache_key));
                Ok(resume.checkpoint.map_or_else(
                    || {
                        InitialResume::History(Box::new(HistoryCheckpoint {
                            workspace: resume.workspace,
                            provider_session_id: resume.lineage_id,
                            canonical_context: resume.canonical_context,
                            history: resume.history,
                            client_authored: resume.client_authored,
                            prompt_cache_key: resume.prompt_cache_key,
                            context_baseline: resume.context_baseline,
                        }))
                    },
                    |checkpoint| InitialResume::Exact(Box::new(checkpoint)),
                ))
            })
            .transpose()?;
        let service = (spawner.service_factory)(Arc::clone(&spawner.config));
        spawn_agent_driver(
            spawner,
            session_id,
            workspace,
            service,
            initial,
            AgentOrigin {
                kind: "restore",
                depth: self.depth.saturating_add(1),
                parent_session_id: Some(Arc::from(parent_session_id)),
            },
        )
    }

    pub(super) fn spawn_clean_many(
        &self,
        workspace: Option<Arc<str>>,
        parent_session_id: &str,
        defaults: TurnDefaults,
        count: usize,
        observer: Option<&SpawnObserver>,
        host_context: Option<Arc<str>>,
    ) -> Result<Vec<(Nanocodex, AgentEvents)>> {
        let mut children = Vec::with_capacity(count);
        for _ in 0..count {
            let child = self.spawn_clean(
                workspace.clone(),
                parent_session_id,
                defaults.model,
                defaults.thinking,
                defaults.fast_mode,
                host_context
                    .as_ref()
                    .or(self.host_context.as_ref())
                    .map(Arc::clone),
            )?;
            if let Some(observer) = observer {
                observer(child.0.session_id());
            }
            children.push(child);
        }
        Ok(children)
    }
}
