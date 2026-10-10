//! Host-owned composition of native harness recipes.

use std::{collections::HashMap, future::Future, sync::Arc};

use crate::{AgentEvents, HarnessFamily, HarnessModel, Nanocodex, NanocodexError, Thinking};
use nanocodex_agent::{
    AgentHandle, ForkRequest, SessionCheckpoint, SpawnOptions,
    backend::{AgentFactory, BackendFuture},
};

type AgentResult = nanocodex_agent::Result<(Nanocodex, AgentEvents)>;
type Recipe = Arc<dyn Fn(HarnessRequest) -> BackendFuture<AgentResult> + Send + Sync>;

/// Validated construction input passed to a concrete provider recipe.
///
/// Recipes keep their native builders, credentials, tools and checkpoint policy.
/// The shared router erases only the finished lifecycle and construction future.
pub struct HarnessRequest {
    /// Family-scoped model selected before any recipe executes.
    pub model: HarnessModel,
    /// Reasoning effort validated for the selected model.
    pub thinking: Thinking,
    /// Complete resolved child configuration.
    pub options: SpawnOptions,
    /// Weak invoking capability, absent for a newly started root thread.
    pub parent: Option<AgentHandle>,
    /// Private host context inherited at this boundary, never model arguments.
    pub host_context: Option<Arc<str>>,
    /// Session to reopen, keeping the checkpoint's session identity, lineage,
    /// model policy and conversation, with the recipe's current host
    /// capabilities. Present for [`Harness::resume`] (with no parent) and for
    /// a parent restoring an evicted child; recipes pass it to their native
    /// builder's `resume`. Its unredacted transcript stays in memory
    /// and must not enter model arguments.
    pub checkpoint: Option<SessionCheckpoint>,
    /// Durable catalog state to own, present for [`Harness::open`] (with no
    /// parent and no checkpoint). Recipes attach it with
    /// [`crate::DurableAgentExt::durability`], which restores the state's
    /// latest boundary, identity and lineage and records every later turn.
    #[cfg(all(feature = "durability", not(target_family = "wasm")))]
    pub durable_state: Option<crate::durability::DurableSession>,
    /// Shared router to install on every per-agent weak handle.
    pub spawn_factory: Arc<dyn AgentFactory>,
}

/// A reusable registry of explicitly authorized native harness recipes.
#[derive(Clone)]
pub struct Harness {
    inner: Arc<Router>,
}

/// Configures concrete recipes before crossing the lifecycle-erasure boundary.
#[derive(Default)]
pub struct HarnessBuilder {
    recipes: HashMap<HarnessFamily, Recipe>,
}

impl HarnessBuilder {
    /// Registers one native construction recipe for a family.
    #[cfg(not(target_family = "wasm"))]
    pub fn register<F, Fut>(mut self, family: HarnessFamily, recipe: F) -> Self
    where
        F: Fn(HarnessRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = AgentResult> + Send + 'static,
    {
        self.recipes
            .insert(family, Arc::new(move |request| Box::pin(recipe(request))));
        self
    }

    /// Registers an isolate-local native construction recipe for a family.
    #[cfg(target_family = "wasm")]
    pub fn register<F, Fut>(mut self, family: HarnessFamily, recipe: F) -> Self
    where
        F: Fn(HarnessRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = AgentResult> + 'static,
    {
        self.recipes
            .insert(family, Arc::new(move |request| Box::pin(recipe(request))));
        self
    }

    /// Freezes this host's authorized families into a reusable router.
    pub fn build(self) -> Harness {
        Harness {
            inner: Arc::new(Router {
                recipes: self.recipes,
            }),
        }
    }
}

impl Harness {
    /// Starts configuring this host's native harness recipes.
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder::default()
    }

    /// Starts a new thread with one selected native model and its default effort.
    pub async fn start(&self, model: HarnessModel) -> AgentResult {
        self.start_with(
            SpawnOptions::new()
                .harness(model.family())
                .harness_model(model),
        )
        .await
    }

    /// Starts a new thread after validating its family, model and effort.
    pub async fn start_with(&self, options: SpawnOptions) -> AgentResult {
        options.validate_harness()?;
        let family = options
            .selected_harness()
            .or_else(|| options.selected_harness_model().map(HarnessModel::family))
            .unwrap_or(HarnessFamily::Codex);
        let model = options
            .selected_harness_model()
            .unwrap_or_else(|| family.default_model());
        let options = options.resolve(model, model.default_thinking())?;
        self.inner
            .clone()
            .construct(None, options, None, Reopen::default())
            .await
    }

    /// Reopens a checkpointed session through the recipe registered for its
    /// family, so hosts resume Codex and Claude sessions the same way.
    ///
    /// The resumed session keeps the checkpoint's session identity, lineage,
    /// model, thinking level, conversation tree and committed history; it uses
    /// the recipe's current credentials, tools and instructions. A checkpoint
    /// taken before the first completed turn reopens the session with its
    /// settings and no history. Use [`Nanocodex::fork`] instead to continue a
    /// conversation under a new identity.
    ///
    /// ```
    /// # use nanocodex::{Harness, HarnessModel, Model, SessionCheckpoint};
    /// # async fn example(harness: Harness) -> nanocodex::agent::Result<()> {
    /// let (agent, _events) = harness.start(HarnessModel::Codex(Model::Sol)).await?;
    /// agent.prompt("Summarize the open issues.").await?.result().await?;
    /// let saved = agent.checkpoint().await?.to_json()?;
    /// agent.shutdown().await?;
    ///
    /// // Later, possibly in another process with the same recipes:
    /// let checkpoint = SessionCheckpoint::from_json(&saved)?;
    /// let (resumed, _events) = harness.resume(checkpoint).await?;
    /// assert_eq!(resumed.session_id(), agent.session_id());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::InvalidCheckpoint`] when the checkpoint fails
    /// [`SessionCheckpoint::validate`] or its native payload is malformed,
    /// [`NanocodexError::InvalidRequest`] when no recipe is registered for
    /// the checkpoint's family, or the recipe's construction error.
    pub async fn resume(&self, checkpoint: SessionCheckpoint) -> AgentResult {
        checkpoint.validate()?;
        let model = checkpoint.model();
        let options = SpawnOptions::new()
            .harness(model.family())
            .harness_model(model)
            .thinking(checkpoint.thinking());
        options.validate_harness()?;
        self.inner
            .clone()
            .construct(None, options, None, Reopen::checkpoint(checkpoint))
            .await
    }

    /// Reopens a session stored in a durable catalog by its ID, through the
    /// recipe registered for its recorded family, so hosts reopen Codex and
    /// Claude sessions the same way.
    ///
    /// The reopened session owns its durable state: it keeps the stored
    /// session identity, lineage, model, thinking level and committed history,
    /// and records every later turn in the same catalog entry. Use
    /// [`SessionStore::branch`](crate::durability::SessionStore::branch) and
    /// open the branch to continue from an earlier boundary under a new
    /// identity, or [`Self::resume`] for a portable checkpoint.
    ///
    /// ```
    /// # use nanocodex::{Harness, durability::SessionStore};
    /// # async fn example(harness: Harness, store: SessionStore) -> nanocodex::agent::Result<()> {
    /// // `store` is the host's catalog, such as `SessionStore::open(codex_home)`.
    /// let (agent, _events) = harness.open(&store, "0190f5d4-7f8e-7c4a-9b1e-2d3c4b5a6978").await?;
    /// agent.prompt("Where were we?").await?.result().await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`NanocodexError::InvalidRequest`] for an unknown session or
    /// when no recipe is registered for its family,
    /// [`NanocodexError::Backend`] for a catalog failure, an error when
    /// another live agent owns the state, or the recipe's construction error.
    #[cfg(all(feature = "durability", not(target_family = "wasm")))]
    pub async fn open(
        &self,
        store: &crate::durability::SessionStore,
        session_id: &str,
    ) -> AgentResult {
        let stored = store.load(session_id).await.map_err(catalog_error)?;
        let model = stored.summary.record.model;
        let thinking = stored
            .session_checkpoint()
            .map_err(catalog_error)?
            .map_or_else(
                || model.default_thinking(),
                |checkpoint| checkpoint.thinking(),
            );
        let options = SpawnOptions::new()
            .harness(model.family())
            .harness_model(model)
            .thinking(thinking);
        options.validate_harness()?;
        let durable_state = store.resume(session_id).await.map_err(catalog_error)?;
        self.inner
            .clone()
            .construct(
                None,
                options,
                None,
                Reopen {
                    checkpoint: None,
                    durable_state: Some(durable_state),
                },
            )
            .await
    }

    /// Installs the same router on a concrete builder's per-agent capabilities.
    pub fn spawn_factory(&self) -> Arc<dyn AgentFactory> {
        Arc::new(RoutedFactory {
            inner: Arc::clone(&self.inner),
        })
    }
}

/// What a recipe reopens instead of starting a new session.
#[derive(Default)]
struct Reopen {
    checkpoint: Option<SessionCheckpoint>,
    #[cfg(all(feature = "durability", not(target_family = "wasm")))]
    durable_state: Option<crate::durability::DurableSession>,
}
impl Reopen {
    fn checkpoint(checkpoint: SessionCheckpoint) -> Self {
        Self {
            checkpoint: Some(checkpoint),
            ..Self::default()
        }
    }
}

struct Router {
    recipes: HashMap<HarnessFamily, Recipe>,
}
impl Router {
    async fn construct(
        self: Arc<Self>,
        parent: Option<AgentHandle>,
        options: SpawnOptions,
        host_context: Option<Arc<str>>,
        reopen: Reopen,
    ) -> AgentResult {
        let model = options.selected_harness_model().ok_or_else(|| {
            NanocodexError::InvalidRequest("harness construction requires a resolved model".into())
        })?;
        let thinking = options.selected_thinking().ok_or_else(|| {
            NanocodexError::InvalidRequest("harness construction requires a resolved effort".into())
        })?;
        let recipe = self
            .recipes
            .get(&model.family())
            .ok_or_else(|| {
                NanocodexError::InvalidRequest(format!(
                    "{} harness is not configured for this host",
                    model.family()
                ))
            })?
            .clone();
        let spawn_factory: Arc<dyn AgentFactory> = Arc::new(RoutedFactory { inner: self });
        recipe(HarnessRequest {
            model,
            thinking,
            options,
            parent,
            host_context,
            checkpoint: reopen.checkpoint,
            #[cfg(all(feature = "durability", not(target_family = "wasm")))]
            durable_state: reopen.durable_state,
            spawn_factory,
        })
        .await
    }
}

struct RoutedFactory {
    inner: Arc<Router>,
}
impl AgentFactory for RoutedFactory {
    fn ensure_available(&self, parent: AgentHandle) -> BackendFuture<nanocodex_agent::Result<()>> {
        Box::pin(async move { parent.ensure_available().await })
    }

    fn settings(
        &self,
        parent: AgentHandle,
    ) -> BackendFuture<nanocodex_agent::Result<(HarnessModel, Thinking)>> {
        Box::pin(async move { parent.settings().await })
    }

    /// Forks stay native: a conversation is continued by the family that owns it.
    fn fork(&self, parent: AgentHandle, request: ForkRequest) -> BackendFuture<AgentResult> {
        Box::pin(async move { parent.fork(request).await })
    }

    fn spawn(
        &self,
        parent: AgentHandle,
        options: SpawnOptions,
        host_context: Option<Arc<str>>,
    ) -> BackendFuture<AgentResult> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            parent.ensure_available().await?;
            options.validate_harness()?;
            if resolve_family(&parent, &options) == parent.harness_family() {
                return parent
                    .spawn_native_with_host_context(options, host_context)
                    .await;
            }
            // A family switch takes destination defaults. The native parent's
            // model may be outside the shared catalog and is never inherited.
            let parent_model = parent.harness_model();
            let options = options.resolve(parent_model, parent_model.default_thinking())?;
            inner
                .construct(Some(parent), options, host_context, Reopen::default())
                .await
        })
    }

    /// Batches carry no family override, so they resolve exactly like a
    /// default single spawn: the parent's family, through its native batch
    /// boundary with atomic admission and rollback.
    fn spawn_many(
        &self,
        parent: AgentHandle,
        count: usize,
        observer: Arc<dyn Fn(&str) + Send + Sync>,
        host_context: Option<Arc<str>>,
    ) -> BackendFuture<nanocodex_agent::Result<Vec<(Nanocodex, AgentEvents)>>> {
        Box::pin(async move {
            parent.ensure_available().await?;
            debug_assert_eq!(
                resolve_family(&parent, &SpawnOptions::new()),
                parent.harness_family()
            );
            parent
                .spawn_many_native_with_host_context(count, move |id| observer(id), host_context)
                .await
        })
    }

    fn restore(
        &self,
        parent: AgentHandle,
        checkpoint: SessionCheckpoint,
        host_context: Option<Arc<str>>,
    ) -> BackendFuture<AgentResult> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            parent.ensure_available().await?;
            checkpoint.validate()?;
            if checkpoint.family() == parent.harness_family() {
                return parent
                    .restore_native_runtime(checkpoint, host_context)
                    .await;
            }
            let model = checkpoint.model();
            let options = SpawnOptions::new()
                .harness(model.family())
                .harness_model(model)
                .thinking(checkpoint.thinking());
            options.validate_harness()?;
            inner
                .construct(
                    Some(parent),
                    options,
                    host_context,
                    Reopen::checkpoint(checkpoint),
                )
                .await
        })
    }
}

#[cfg(all(feature = "durability", not(target_family = "wasm")))]
fn catalog_error(error: crate::durability::Error) -> NanocodexError {
    match error {
        crate::durability::Error::SessionNotFound { .. } => {
            NanocodexError::InvalidRequest(error.to_string())
        }
        error => NanocodexError::Backend {
            backend: "durability",
            source: Arc::new(error),
        },
    }
}

/// The family a child of `parent` runs in: an explicit harness or model
/// selection, otherwise the parent's own family.
fn resolve_family(parent: &AgentHandle, options: &SpawnOptions) -> HarnessFamily {
    options
        .selected_harness()
        .or_else(|| options.selected_harness_model().map(HarnessModel::family))
        .unwrap_or(parent.harness_family())
}
