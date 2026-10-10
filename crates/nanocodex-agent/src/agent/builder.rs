use super::spawn::{validate_model_reasoning_mode, validate_model_thinking};
use super::*;

#[cfg(not(target_family = "wasm"))]
use crate::rollout::RolloutConfig;

/// Builder for one owned agent lifecycle.
#[derive(Clone)]
pub struct NanocodexBuilder<F = StandardServiceFactory> {
    pub(super) config: ModelConfig,
    pub(super) tools: ToolsConfiguration,
    pub(super) workspace: Option<PathBuf>,
    pub(super) session_id: Option<SessionId>,
    pub(super) prompt_cache: PromptCacheConfig,
    pub(super) codex: CodexCompatibility,
    pub(super) resume: Option<SessionSnapshot>,
    pub(super) lineage: Option<Lineage>,
    // Whether this builder chose a tier, which then wins over a resumed
    // snapshot's recorded tier (as an explicit thinking level does).
    pub(super) service_tier_explicit: bool,
    pub(super) factory: F,
}

impl<F> BuilderBackend for OpenAi<F>
where
    F: ResponsesServiceFactory,
{
    type Builder = NanocodexBuilder<F>;

    fn into_builder(self) -> Self::Builder {
        let (config, factory) = into_openai_parts(self);
        NanocodexBuilder {
            config,
            tools: ToolsConfiguration::Shared(Tools::default()),
            workspace: None,
            session_id: None,
            prompt_cache: PromptCacheConfig::default(),
            codex: CodexCompatibility::default(),
            resume: None,
            lineage: None,
            service_tier_explicit: false,
            factory,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct PromptCacheConfig {
    pub(super) key: Option<String>,
    pub(super) shared: Option<SharedPromptCache>,
}

#[derive(Clone, Default)]
pub(super) struct CodexCompatibility {
    pub(super) instant_tool_steering: bool,
    pub(super) context: ContextSourceConfig,
    pub(super) execution: ExecutionConfig,
    pub(super) before_compaction: Option<Arc<dyn execution::BeforeCompaction>>,
    pub(super) spawn_factory: Option<Arc<dyn backend::AgentFactory>>,
    pub(super) host_context: Option<Arc<str>>,
    pub(super) child_journal: Option<backend::ChildJournal>,
}

impl<F> NanocodexBuilder<F> {
    /// Opt into steering-triggered Code Mode observer yields (default false).
    /// Cells and nested tools continue; this does not interrupt a model stream.
    #[must_use]
    pub const fn instant_tool_steering(mut self, enabled: bool) -> Self {
        self.codex.instant_tool_steering = enabled;
        self
    }

    /// Awaits durable host preservation before automatic or manual compaction.
    ///
    /// The host must deduplicate by boundary ID, return only after durable success,
    /// and bound its work. Dropping its future cancels an interrupted compaction.
    /// Failure stops compaction without trimming history. Disabled by default.
    #[must_use]
    pub fn before_compaction(mut self, hook: impl execution::BeforeCompaction + 'static) -> Self {
        self.codex.before_compaction = Some(Arc::new(hook));
        self
    }

    /// Resumes a checkpointed session in a fresh driver, transport and tool
    /// runtime built from this recipe.
    ///
    /// The resumed session *is* the checkpointed session: it keeps the
    /// checkpoint's session identity, lineage, conversation tree, committed
    /// history, model, thinking level, processing tier and transport policy.
    /// This recipe supplies the credentials, instructions, tools and handlers
    /// for later turns. Settings called after `resume` override the
    /// checkpoint's. A checkpoint taken before the first completed turn
    /// reopens the session with its settings and no history. Use
    /// [`Nanocodex::fork`] to continue a conversation under a new identity.
    ///
    /// ```no_run
    /// # use nanocodex_agent::{Nanocodex, SessionCheckpoint};
    /// # async fn example(
    /// #     openai: nanocodex_agent::OpenAi,
    /// #     saved: &str,
    /// # ) -> nanocodex_agent::Result<()> {
    /// let checkpoint = SessionCheckpoint::from_json(saved)?;
    /// let session_id = checkpoint.session_id().to_owned();
    /// let (agent, _events) = Nanocodex::builder(openai).resume(checkpoint)?.build()?;
    /// assert_eq!(agent.session_id().to_string(), session_id);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::CheckpointFamilyMismatch`] for a non-Codex
    /// checkpoint and [`NanocodexError::InvalidCheckpoint`] for an
    /// invalid one.
    pub fn resume(mut self, checkpoint: SessionCheckpoint) -> Result<Self> {
        let snapshot = ChildState::from_checkpoint(checkpoint)?;
        self = self
            .model(snapshot.model)
            .thinking(snapshot.thinking)
            .service_tier(snapshot.service_tier);
        self.session_id = Some(snapshot.session_id.parse().map_err(|error| {
            NanocodexError::InvalidCheckpoint(format!("invalid child session: {error}"))
        })?);
        self.resume = snapshot.conversation;
        self.lineage = Some(snapshot.lineage);
        if snapshot.stateless_http {
            self.config.responses_transport = ResponsesTransport::Https;
            self.config.responses_history = ResponsesHistory::FullReplay;
            self.config.store_responses = false;
            self.config.websocket_warmup = false;
        }
        Ok(self)
    }

    /// Retains embedding-private context in this agent's tool runtime.
    #[must_use]
    pub fn host_context(mut self, context: Option<Arc<str>>) -> Self {
        self.codex.host_context = context;
        self
    }

    /// Makes this root's subagent task tree durable beside its own state.
    /// Durability adapters call this; it is never inherited by children.
    #[must_use]
    pub fn child_journal(mut self, journal: backend::ChildJournal) -> Self {
        self.codex.child_journal = Some(journal);
        self
    }

    /// Configures embedding-owned native child construction across harness families.
    #[must_use]
    pub fn spawn_factory(mut self, factory: Arc<dyn backend::AgentFactory>) -> Self {
        self.codex.spawn_factory = Some(factory);
        self
    }

    /// Overrides the `OpenAi` recipe's model for this agent.
    ///
    /// Without this call the agent inherits the client default. The selected
    /// model is fixed for the lifetime of the agent thread.
    #[must_use]
    pub const fn model(mut self, model: Model) -> Self {
        self.config.model = model;
        if !self.config.thinking_explicit {
            self.config.thinking = model.default_thinking();
        }
        if self.config.context_window_tokens > model.max_context_window_tokens() {
            self.config.context_window_tokens = model.max_context_window_tokens();
        }
        self
    }

    /// Replaces the stable system/developer instructions.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<Arc<str>>) -> Self {
        self.config.system_prompt = Some(instructions.into());
        self
    }

    /// Adds host instructions after the selected model's built-in instructions
    /// or the explicit replacement supplied with [`Self::instructions`].
    #[must_use]
    pub fn additional_instructions(mut self, instructions: impl Into<Arc<str>>) -> Self {
        self.config.additional_instructions = Some(instructions.into());
        self
    }

    /// Overrides the `OpenAi` recipe's model thinking level for this agent.
    ///
    /// Without this call the agent inherits the client default. A later
    /// [`Nanocodex::set_thinking`] call affects subsequently accepted turns.
    #[must_use]
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.config.thinking = thinking;
        self.config.thinking_explicit = true;
        self
    }

    /// Overrides the `OpenAi` recipe's priority-processing policy for this
    /// agent.
    ///
    /// Without this call the agent inherits the client default. A later
    /// [`Nanocodex::set_service_tier`] call affects subsequently accepted turns.
    #[must_use]
    pub const fn fast_mode(self, enabled: bool) -> Self {
        self.service_tier(ServiceTier::from_fast_mode(enabled))
    }

    /// Selects the processing tier for subsequently accepted turns.
    ///
    /// Building fails when the selected model does not offer the tier; see
    /// [`crate::ModelCapabilities::service_tiers`].
    #[must_use]
    pub const fn service_tier(mut self, service_tier: ServiceTier) -> Self {
        self.config.service_tier = service_tier;
        self.service_tier_explicit = true;
        self
    }

    /// Sets the selected model's context window used for accounting and compaction.
    ///
    /// Values above the selected model's advertised maximum are clamped. The
    /// default remains 272,000 tokens to stay below long-context pricing.
    #[must_use]
    pub const fn context_window_tokens(mut self, tokens: u64) -> Self {
        let maximum = self.config.model.max_context_window_tokens();
        self.config.context_window_tokens = if tokens > maximum { maximum } else { tokens };
        self
    }

    /// Overrides the `OpenAi` recipe's Responses reasoning execution mode for
    /// this agent.
    ///
    /// Without this call the agent inherits the client default.
    #[must_use]
    pub const fn reasoning_mode(mut self, reasoning_mode: ReasoningMode) -> Self {
        self.config.reasoning_mode = reasoning_mode;
        self
    }

    /// Replaces the standard built-in tool selection.
    #[must_use]
    pub fn tools(mut self, tools: Tools) -> Self {
        self.tools = ToolsConfiguration::Shared(tools);
        self
    }

    /// Builds a fresh tool collection for every agent driver.
    ///
    /// The factory receives a weak capability targeting the driver whose tool
    /// runtime is being built. Use this for agent-relative tools such as Code
    /// Mode child-agent tools; stateless tools may continue using
    /// [`Self::tools`].
    #[must_use]
    pub fn tools_factory<T>(mut self, factory: T) -> Self
    where
        T: Fn(AgentHandle) -> std::result::Result<Tools, ToolsBuildError> + Send + Sync + 'static,
    {
        self.tools = ToolsConfiguration::PerAgent(Arc::new(factory));
        self
    }

    /// Fixes the workspace used by every prompt in this agent session.
    #[must_use]
    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// Describes the remote environment where model-visible tools execute.
    ///
    /// This replaces host date/time discovery and host `AGENTS.md` discovery
    /// together, so one agent never mixes context from two machines. The
    /// snapshot remains fixed for this agent lifecycle.
    #[must_use]
    pub fn execution_environment(mut self, environment: ExecutionEnvironment) -> Self {
        self.codex.context.set_execution_environment(environment);
        self
    }

    /// Sets the root agent's `UUIDv7` session identity.
    ///
    /// The root identity also seeds its checkpoint lineage. Spawned siblings
    /// and forks receive fresh session IDs; forks retain the root's opaque
    /// lineage so [`Nanocodex::fork`] can reject unrelated boundaries.
    ///
    /// Replacing the identity of a [`resume`](Self::resume)d session starts
    /// a new root that continues the checkpoint's conversation tree, which is
    /// how a host seeds a separately stored copy of a conversation.
    #[must_use]
    pub fn session_id(mut self, session_id: SessionId) -> Self {
        if self.session_id.is_some_and(|current| current != session_id) {
            self.lineage = None;
        }
        self.session_id = Some(session_id);
        self
    }

    /// Sets a stable cache identity for the immutable request prefix.
    ///
    /// Independent root agents may share this key without sharing their
    /// session, conversation, response chain, tools, or workspace. When
    /// omitted, each independently built root uses its own session lineage.
    /// Clean children and forks inherit their root's cache identity.
    #[must_use]
    pub fn prompt_cache_key(mut self, prompt_cache_key: impl Into<String>) -> Self {
        self.prompt_cache.key = Some(prompt_cache_key.into());
        self
    }

    /// Installs a cache identity only when the caller did not configure one.
    ///
    /// This is an internal composition seam for durable session owners that
    /// need a stable pre-checkpoint lineage across process replacement.
    #[doc(hidden)]
    #[must_use]
    pub fn default_prompt_cache_key(mut self, prompt_cache_key: impl Into<String>) -> Self {
        if self.prompt_cache.key.is_none() {
            self.prompt_cache.key = Some(prompt_cache_key.into());
        }
        self
    }

    /// Shares completed immutable-prefix warmups among builders cloned from
    /// this recipe.
    ///
    /// The first agent primes the provider cache. Other agents skip the
    /// redundant warmup and send their first complete generation with the same
    /// prefix cache key. Every clean agent still owns an independent session,
    /// conversation, response chain, service stack, tool runtime, event stream,
    /// and workspace. Entries are fingerprinted from the exact prefix and key.
    #[must_use]
    pub fn shared_prompt_cache(mut self) -> Self {
        self.prompt_cache.shared = Some(SharedPromptCache::default());
        self
    }

    /// Loads global user instructions from `AGENTS.override.md` or `AGENTS.md`
    /// in the supplied Codex state directory.
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    #[must_use]
    pub fn codex_home(mut self, codex_home: impl Into<PathBuf>) -> Self {
        self.codex.context.set_codex_home(codex_home.into());
        self
    }

    /// Also loads the global `CLAUDE.md` from the supplied Claude Code
    /// configuration directory, after the Codex home's instructions and only
    /// when it is a distinct document. Unset by default.
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    #[must_use]
    pub fn claude_home(mut self, claude_home: impl Into<PathBuf>) -> Self {
        self.codex.context.set_claude_home(claude_home.into());
        self
    }

    /// Records committed history in Codex's resumable JSONL rollout layout.
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    #[must_use]
    pub fn rollout(mut self, rollout: RolloutConfig) -> Self {
        if self.codex.context.codex_home().is_none() {
            self.codex
                .context
                .set_codex_home(rollout.codex_home().to_path_buf());
        }
        self.codex.execution.set_rollout(rollout);
        self
    }

    /// Resumes from a Codex-native session snapshot, such as one loaded from a
    /// rollout or a durable store.
    ///
    /// The session continues with the thinking level and processing tier the
    /// snapshot recorded, unless this builder chose them explicitly. Older
    /// snapshots that record neither keep the builder's settings.
    #[doc(hidden)]
    #[must_use]
    pub fn resume_native_snapshot(mut self, snapshot: SessionSnapshot) -> Self {
        if let Some(thinking) = snapshot.thinking()
            && !self.config.thinking_explicit
        {
            self.config.thinking = thinking;
        }
        if let Some(service_tier) = snapshot.service_tier()
            && !self.service_tier_explicit
        {
            self.config.service_tier = service_tier;
        }
        self.resume = Some(snapshot);
        self
    }

    /// Starts a reopened stored session that has no checkpoint yet with the
    /// model, reasoning effort and processing tier it was created with,
    /// unless this builder chose the effort or tier explicitly.
    #[doc(hidden)]
    #[must_use]
    pub fn initial_settings(
        mut self,
        model: Model,
        thinking: Thinking,
        service_tier: ServiceTier,
    ) -> Self {
        let explicit_thinking = self.config.thinking_explicit;
        self = self.model(model);
        if !explicit_thinking {
            self.config.thinking = thinking;
        }
        if !self.service_tier_explicit {
            self.config.service_tier = service_tier;
        }
        self
    }

    /// Records the provenance of a reopened stored session, such as a durable
    /// fork, instead of reporting a fresh root. Telemetry and
    /// [`Nanocodex::session`] report it; rollout metadata is unaffected.
    #[doc(hidden)]
    #[must_use]
    pub fn lineage(mut self, lineage: Lineage) -> Self {
        self.lineage = Some(lineage);
        self
    }

    /// Returns the explicitly configured native resume boundary, if any.
    #[doc(hidden)]
    #[must_use]
    pub const fn resume_snapshot(&self) -> Option<&SessionSnapshot> {
        self.resume.as_ref()
    }

    /// Attaches a higher-layer execution policy at the agent's model, tool,
    /// and committed-session boundaries.
    ///
    /// Persistence formats, storage, admission, and recovery remain owned by
    /// the implementing crate. Most callers use a higher-level extension such
    /// as `nanocodex-durability` instead of invoking this seam directly.
    #[must_use]
    pub fn execution_policy(mut self, policy: Arc<dyn execution::ExecutionPolicy>) -> Self {
        self.codex.execution.set_policy(policy);
        self
    }

    /// Builds a fresh higher-layer execution policy for every root agent.
    ///
    /// This internal composition seam is for stateful policy recipes whose
    /// builder remains cloneable while each built driver requires independent
    /// lifecycle ownership.
    #[doc(hidden)]
    #[must_use]
    pub fn execution_policy_factory<P>(mut self, factory: P) -> Self
    where
        P: Fn() -> Result<Arc<dyn execution::ExecutionPolicy>> + Send + Sync + 'static,
    {
        self.codex.execution.set_policy_factory(Arc::new(factory));
        self
    }
}

#[cfg(not(target_family = "wasm"))]
impl<F> NanocodexBuilder<F>
where
    F: ResponsesServiceFactory + Send + Sync + 'static,
    F::Service: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Error: Into<ResponseError> + Send + 'static,
    <F::Service as Service<ResponsesAttempt>>::Future: Send,
{
    /// Builds an agent from the configured [`OpenAi`] client recipe.
    ///
    /// Each root, spawned sibling, and fork receives a fresh concrete Tower
    /// service, tool runtime, event stream, and mutable conversation state.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid agent policy or, on native targets, when
    /// no Tokio runtime is active.
    pub fn build(self) -> Result<(Nanocodex, AgentEvents)> {
        build(self)
    }
}

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
impl<F> NanocodexBuilder<F>
where
    F: ResponsesServiceFactory + 'static,
    F::Service: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + 'static,
    <F::Service as Service<ResponsesAttempt>>::Error: Into<ResponseError> + 'static,
{
    /// Builds an agent from the configured [`OpenAi`] client recipe.
    ///
    /// Each root, spawned sibling, and fork receives a fresh concrete Tower
    /// service, tool runtime, event stream, and mutable conversation state.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid agent policy.
    pub fn build(self) -> Result<(Nanocodex, AgentEvents)> {
        build(self)
    }
}

fn build<F>(builder: NanocodexBuilder<F>) -> Result<(Nanocodex, AgentEvents)>
where
    F: ResponsesServiceFactory + AgentFactory + 'static,
    F::Service:
        Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    <F::Service as Service<ResponsesAttempt>>::Error: Into<ResponseError> + AgentSend + 'static,
    <F::Service as Service<ResponsesAttempt>>::Future: AgentSend,
{
    if builder.resume.is_none() {
        validate_model_thinking(builder.config.model, builder.config.thinking)?;
        validate_model_reasoning_mode(builder.config.model, builder.config.reasoning_mode)?;
        // An explicitly selected tier must be one the model offers; the
        // client default remains a preference clamped per model.
        if builder.service_tier_explicit {
            crate::HarnessModel::Codex(builder.config.model)
                .capabilities(crate::ModelTransport::Native)
                .check_service_tier(builder.config.service_tier)?;
        }
    }
    validate(&builder.config, builder.prompt_cache.key.as_deref())?;
    validate_execution_environment(builder.codex.context.execution_environment())?;
    let config = Arc::new(builder.config);
    let factory = builder.factory;
    let service_factory: ServiceFactory<F::Service> = Arc::new(move |config| factory.make(config));
    build_agent(
        config,
        builder.tools,
        builder.workspace,
        builder.session_id,
        builder.prompt_cache,
        builder.codex,
        builder.resume,
        builder.lineage,
        service_factory,
    )
}

fn validate_execution_environment(environment: Option<&ExecutionEnvironment>) -> Result<()> {
    let Some(environment) = environment else {
        return Ok(());
    };
    if environment.current_date.trim().is_empty() {
        return Err(NanocodexError::InvalidRequest(
            "execution-environment current date must not be empty".to_owned(),
        ));
    }
    if !is_iso_date(environment.current_date.trim()) {
        return Err(NanocodexError::InvalidRequest(
            "execution-environment current date must use YYYY-MM-DD".to_owned(),
        ));
    }
    if environment.timezone.trim().is_empty() {
        return Err(NanocodexError::InvalidRequest(
            "execution-environment timezone must not be empty".to_owned(),
        ));
    }
    if environment
        .project_instructions
        .as_deref()
        .is_some_and(|instructions| instructions.trim().is_empty())
    {
        return Err(NanocodexError::InvalidRequest(
            "execution-environment project instructions must not be empty".to_owned(),
        ));
    }
    Ok(())
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let Some(year) = decimal(&bytes[..4]) else {
        return false;
    };
    let Some(month) = decimal(&bytes[5..7]) else {
        return false;
    };
    let Some(day) = decimal(&bytes[8..]) else {
        return false;
    };
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

fn decimal(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + u32::from(*byte - b'0'))
    })
}

#[cfg(test)]
mod tests {
    use std::{
        future::{Pending, pending},
        sync::Mutex,
        task::{Context, Poll},
    };

    use nanocodex_oai_api::{
        auth::OpenAiAuth,
        responses::{ContentItem, MessageRole, ResponseItem},
    };

    use super::*;

    #[test]
    fn execution_environment_requires_complete_model_visible_context() {
        assert!(
            validate_execution_environment(Some(&ExecutionEnvironment::new(
                "2026-07-29",
                "Etc/UTC",
            )))
            .is_ok()
        );
        assert!(
            validate_execution_environment(Some(&ExecutionEnvironment::new("July 29", "Etc/UTC",)))
                .is_err()
        );
        assert!(
            validate_execution_environment(Some(&ExecutionEnvironment::new(
                "2026-02-29",
                "Etc/UTC",
            )))
            .is_err()
        );
        assert!(
            validate_execution_environment(Some(
                &ExecutionEnvironment::new("2026-07-29", "Etc/UTC").project_instructions(" "),
            ))
            .is_err()
        );
    }

    #[derive(Clone)]
    struct ObservingFactory {
        model: Arc<Mutex<Option<Model>>>,
    }

    impl ResponsesServiceFactory for ObservingFactory {
        type Service = PendingService;

        fn make(&self, config: Arc<ModelConfig>) -> Self::Service {
            *self.model.lock().expect("model observation lock") = Some(config.model);
            PendingService
        }
    }

    struct PendingService;

    impl Service<ResponsesAttempt> for PendingService {
        type Response = ResponsesServiceResponse;
        type Error = ResponseError;
        type Future = Pending<std::result::Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _context: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: ResponsesAttempt) -> Self::Future {
            pending()
        }
    }

    #[tokio::test]
    async fn resumed_model_reaches_the_service_factory() {
        let workspace = std::env::current_dir().expect("current workspace");
        let canonical_context = ResponseItem::message(
            MessageRole::User,
            [ContentItem::input_text("resume with the retained model")],
        );
        let obsolete: SessionSnapshot = serde_json::from_value(serde_json::json!({
            "version": 1,
            "model": "gpt-5.6-luna",
            "lineage_id": "019c0d31-c308-7d91-bff4-5dca82d15ac6",
            "prompt_cache_key": "obsolete-model",
            "workspace": workspace,
            "canonical_context": canonical_context,
            "history": [canonical_context],
        }))
        .expect("snapshot envelope should decode before model validation");
        let result = Nanocodex::builder(OpenAi::builder("test-key").build().unwrap())
            .resume_native_snapshot(obsolete)
            .build();
        let error = match result {
            Ok(_) => panic!("an obsolete snapshot model must not continue as another model"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("snapshot model is unsupported"));

        let snapshot = serde_json::from_value(serde_json::json!({
            "version": 1,
            "model": "gpt-6-luna",
            "lineage_id": "019c0d31-c308-7d91-bff4-5dca82d15ac6",
            "prompt_cache_key": "retained-model",
            "workspace": workspace,
            "canonical_context": canonical_context,
            "history": [canonical_context],
        }))
        .expect("valid session snapshot");
        let observed_model = Arc::new(Mutex::new(None));
        let mut config = ModelConfig {
            auth: OpenAiAuth::api_key("test-key"),
            ..ModelConfig::default()
        };
        config.model = Model::Sol;
        let builder = NanocodexBuilder {
            config,
            tools: ToolsConfiguration::Shared(
                Tools::builder()
                    .without_defaults()
                    .build()
                    .expect("empty tools"),
            ),
            workspace: None,
            session_id: None,
            prompt_cache: PromptCacheConfig::default(),
            codex: CodexCompatibility::default(),
            resume: Some(snapshot),
            lineage: None,
            service_tier_explicit: false,
            factory: ObservingFactory {
                model: Arc::clone(&observed_model),
            },
        };

        let (agent, events) = builder.build().expect("resumed agent");

        assert_eq!(
            *observed_model.lock().expect("model observation lock"),
            Some(Model::Luna)
        );
        agent.shutdown().await.expect("agent shutdown");
        drop(events);
    }
}
