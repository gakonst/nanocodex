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
    /// Portable boundary to restore with the recipe's current host capabilities,
    /// keeping the checkpoint's session identity. Its unredacted transcript stays
    /// in memory and must not enter model arguments.
    pub checkpoint: Option<SessionCheckpoint>,
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
            .construct(None, options, None, None)
            .await
    }

    /// Installs the same router on a concrete builder's per-agent capabilities.
    pub fn spawn_factory(&self) -> Arc<dyn AgentFactory> {
        Arc::new(RoutedFactory {
            inner: Arc::clone(&self.inner),
        })
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
        checkpoint: Option<SessionCheckpoint>,
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
            checkpoint,
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
                .construct(Some(parent), options, host_context, None)
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
                .construct(Some(parent), options, host_context, Some(checkpoint))
                .await
        })
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
