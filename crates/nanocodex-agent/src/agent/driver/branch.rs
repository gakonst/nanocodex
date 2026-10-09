use super::*;

pub(in crate::agent) struct BranchSpawner<S> {
    pub(in crate::agent) instant_tool_steering: bool,
    pub(in crate::agent) config: Arc<ModelConfig>,
    pub(in crate::agent) tools: ToolsConfiguration,
    pub(in crate::agent) spawn_factory: Option<Arc<dyn backend::AgentFactory>>,
    pub(in crate::agent) child_journal: Option<backend::ChildJournal>,
    pub(in crate::agent) lineage_id: Arc<str>,
    pub(in crate::agent) provider_session_id: Arc<str>,
    pub(in crate::agent) prompt_cache_key: Option<Arc<str>>,
    pub(in crate::agent) shared_prompt_cache: Option<SharedPromptCache>,
    pub(in crate::agent) before_compaction: Option<Arc<dyn execution::BeforeCompaction>>,
    pub(in crate::agent) context_config: ContextSourceConfig,
    pub(in crate::agent) context_source: ContextSource,
    /// This session's own provenance; children derive theirs from it.
    pub(in crate::agent) lineage: Lineage,
    pub(in crate::agent) execution: ExecutionConfig,
    pub(in crate::agent) restored_snapshot: Option<SessionSnapshot>,
    pub(in crate::agent) host_context: Option<Arc<str>>,
    pub(in crate::agent) service_factory: ServiceFactory<S>,
}

/// How a driver started and the provenance its handle reports.
#[derive(Clone)]
pub(in crate::agent) struct AgentOrigin {
    pub(in crate::agent) start: crate::session::SessionStart,
    pub(in crate::agent) lineage: Lineage,
}

impl AgentOrigin {
    pub(in crate::agent) const fn kind(&self) -> &'static str {
        self.start.kind()
    }

    pub(in crate::agent) const fn depth(&self) -> u32 {
        self.lineage.depth
    }

    pub(in crate::agent) fn parent_session_id(&self) -> Option<&str> {
        self.lineage.parent_session_id.as_deref()
    }

    /// Parent recorded in rollout metadata. Roots never have one; a resumed
    /// session keeps its persisted parent, so a reopened branch is still
    /// mirrored as a branch of its source.
    pub(in crate::agent) fn recorded_parent(&self) -> Option<&str> {
        match self.start {
            crate::session::SessionStart::New(crate::Origin::Root) => None,
            _ => self.parent_session_id(),
        }
    }
}

impl<S> BranchSpawner<S> {
    fn for_new_thread(
        &self,
        operation: &'static str,
        branch_policy: Option<Arc<dyn execution::ExecutionPolicy>>,
    ) -> Result<Self> {
        Ok(self.with_execution(self.execution.for_new_thread(operation, branch_policy)?))
    }

    fn with_execution(&self, execution: ExecutionConfig) -> Self {
        Self {
            instant_tool_steering: self.instant_tool_steering,
            config: Arc::clone(&self.config),
            tools: self.tools.clone(),
            spawn_factory: self.spawn_factory.clone(),
            // A durable task-tree journal belongs only to its configured root.
            child_journal: None,
            lineage_id: Arc::clone(&self.lineage_id),
            provider_session_id: Arc::clone(&self.provider_session_id),
            prompt_cache_key: self.prompt_cache_key.as_ref().map(Arc::clone),
            shared_prompt_cache: self.shared_prompt_cache.clone(),
            // Preservation belongs to the host that explicitly configured this root.
            before_compaction: None,
            context_config: self.context_config.clone(),
            context_source: self.context_source.clone(),
            lineage: self.lineage.clone(),
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
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_fork(
        &self,
        point: ForkFrom,
        latest: Option<&Arc<CommittedSession>>,
        parent_session_id: &str,
        model: Model,
        thinking: Thinking,
        service_tier: ServiceTier,
        host_context: Option<Arc<str>>,
        origin: crate::Origin,
        session_id: SessionId,
        branch_policy: Option<Arc<dyn execution::ExecutionPolicy>>,
    ) -> Result<(Nanocodex, AgentEvents)> {
        let mut spawner = self.for_new_thread("fork", branch_policy)?;
        spawner.context_source = spawner.context_config.build();
        let (workspace, initial) = match point {
            ForkFrom::Latest => {
                let checkpoint = latest.ok_or(NanocodexError::ForkBeforeCompletedTurn)?;
                (
                    Arc::<str>::from(checkpoint.model().workspace()),
                    InitialResume::Exact(Box::new(checkpoint.model().clone())),
                )
            }
            ForkFrom::Live(checkpoint) => (
                Arc::<str>::from(checkpoint.model().workspace()),
                InitialResume::Exact(Box::new(checkpoint.model().clone())),
            ),
            ForkFrom::Snapshot(snapshot) => {
                spawner.restored_snapshot = Some((*snapshot).clone());
                let resume = snapshot.into_resume()?;
                let resolved = spawner
                    .context_source
                    .resolve_workspace(Some(&resume.workspace))?;
                if resolved != resume.workspace {
                    return Err(NanocodexError::InvalidCheckpoint(
                        "fork workspace no longer resolves to the stored location".into(),
                    ));
                }
                spawner.prompt_cache_key = Some(Arc::clone(&resume.prompt_cache_key));
                (
                    Arc::<str>::from(resume.workspace.as_str()),
                    InitialResume::from_resume(resume),
                )
            }
        };
        spawner.host_context = host_context;
        let mut config = (*spawner.config).clone();
        config.model = model;
        config.thinking = thinking;
        config.service_tier = service_tier;
        spawner.config = Arc::new(config);
        spawner.lineage = Lineage::child_of(&self.lineage, parent_session_id, origin);
        let service = (spawner.service_factory)(Arc::clone(&spawner.config));
        spawn_agent_driver(
            spawner,
            session_id,
            Some(workspace),
            service,
            Some(initial),
            crate::session::SessionStart::New(origin),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_clean(
        &self,
        workspace: Option<Arc<str>>,
        parent_session_id: &str,
        model: Model,
        thinking: Thinking,
        service_tier: ServiceTier,
        stateless_http: bool,
        host_context: Option<Arc<str>>,
        parent: &execution::Execution,
    ) -> Result<(Nanocodex, AgentEvents)> {
        let session_id = SessionId::new();
        let session_id_text = session_id.to_string();
        let lineage = Lineage::child_of(&self.lineage, parent_session_id, crate::Origin::Subagent);
        // A subagent of a durable session is durable exactly like a fork.
        let branch_policy = parent.branch_policy(&crate::SessionInfo::new(
            session_id_text.as_str(),
            crate::HarnessFamily::Codex,
            lineage.clone(),
        ))?;
        let mut config = (*self.config).clone();
        config.model = model;
        config.thinking = thinking;
        config.service_tier = service_tier;
        if stateless_http {
            config.responses_transport = ResponsesTransport::Https;
            config.responses_history = ResponsesHistory::FullReplay;
            config.store_responses = false;
            config.websocket_warmup = false;
        }
        let prompt_cache_key = self
            .prompt_cache_key
            .as_ref()
            .map_or_else(|| Arc::clone(&self.lineage_id), Arc::clone);
        let spawner = Self {
            config: Arc::new(config),
            instant_tool_steering: self.instant_tool_steering,
            tools: self.tools.clone(),
            spawn_factory: self.spawn_factory.clone(),
            // A durable task-tree journal belongs only to its configured root.
            child_journal: None,
            lineage_id: Arc::from(session_id_text.as_str()),
            provider_session_id: Arc::clone(&self.provider_session_id),
            prompt_cache_key: Some(prompt_cache_key),
            shared_prompt_cache: self.shared_prompt_cache.clone(),
            // Preservation belongs to the host that explicitly configured this root.
            before_compaction: None,
            context_config: self.context_config.clone(),
            context_source: self.context_config.build(),
            lineage,
            execution: self.execution.for_new_thread("spawn", branch_policy)?,
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
            crate::session::SessionStart::New(crate::Origin::Subagent),
        )
    }

    pub(super) fn restore_child(
        &self,
        snapshot: ChildState,
        workspace: Option<Arc<str>>,
        parent_session_id: &str,
        host_context: Option<Arc<str>>,
        parent: &execution::Execution,
    ) -> Result<(Nanocodex, AgentEvents)> {
        snapshot.validate()?;
        let session_id = snapshot.session_id.parse::<SessionId>().map_err(|error| {
            NanocodexError::InvalidCheckpoint(format!("invalid child session ID: {error}"))
        })?;
        // The restoring runtime is the parent; the child keeps how it was created.
        let origin = match snapshot.lineage.origin {
            crate::Origin::Root => crate::Origin::Subagent,
            origin => origin,
        };
        let lineage = Lineage::child_of(&self.lineage, parent_session_id, origin);
        // Rehydrate an evicted child under its own branch policy, which reopens
        // the durable state and rollout it recorded under the same session ID.
        let branch_policy = parent.branch_policy(&crate::SessionInfo::new(
            snapshot.session_id.as_str(),
            crate::HarnessFamily::Codex,
            lineage.clone(),
        ))?;
        let mut spawner = self.for_new_thread("restore", branch_policy)?;
        spawner.restored_snapshot = snapshot.conversation.clone();
        spawner.lineage = lineage;
        spawner.context_source = spawner.context_config.build();
        spawner.host_context = host_context;
        spawner.lineage_id = Arc::clone(&snapshot.conversation_id);
        let mut config = (*spawner.config).clone();
        config.model = snapshot.model;
        config.thinking = snapshot.thinking;
        config.service_tier = snapshot.service_tier;
        if snapshot.stateless_http {
            config.responses_transport = ResponsesTransport::Https;
            config.responses_history = ResponsesHistory::FullReplay;
            config.store_responses = false;
            config.websocket_warmup = false;
        }
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
                return Err(NanocodexError::InvalidCheckpoint(
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
                Ok(InitialResume::from_resume(resume))
            })
            .transpose()?;
        let service = (spawner.service_factory)(Arc::clone(&spawner.config));
        spawn_agent_driver(
            spawner,
            session_id,
            workspace,
            service,
            initial,
            crate::session::SessionStart::Restore,
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
        parent: &execution::Execution,
    ) -> Result<Vec<(Nanocodex, AgentEvents)>> {
        let mut children = Vec::with_capacity(count);
        for _ in 0..count {
            let child = self.spawn_clean(
                workspace.clone(),
                parent_session_id,
                defaults.model,
                defaults.thinking,
                defaults.service_tier,
                false,
                host_context
                    .as_ref()
                    .or(self.host_context.as_ref())
                    .map(Arc::clone),
                parent,
            )?;
            children.push(child);
        }
        // All or nothing: a failure above drops every created child, whose
        // driver then stops before any turn recorded durable state or a
        // rollout. Observers see only a batch that started completely.
        if let Some(observer) = observer {
            for (child, _) in &children {
                observer(child.session_id());
            }
        }
        Ok(children)
    }
}
