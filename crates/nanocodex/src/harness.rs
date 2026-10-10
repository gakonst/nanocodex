//! Host-owned composition of native harness recipes.

use std::{collections::HashMap, fmt, future::Future, sync::Arc};

use crate::{
    AgentEvents, ClaudeModel, HarnessFamily, HarnessModel, Model, Nanocodex, NanocodexError,
    Thinking,
};
use nanocodex_agent::{
    AgentHandle, ChildSnapshot, SpawnOptions,
    backend::{AgentFactory, BackendFuture},
};

type AgentResult = nanocodex_agent::Result<(Nanocodex, AgentEvents)>;
type Recipe = Arc<dyn Fn(HarnessRequest) -> BackendFuture<AgentResult> + Send + Sync>;

/// Validated construction input passed to a concrete provider recipe.
///
/// Recipes keep their native builders, credentials, tools and checkpoint policy.
/// The shared router erases only the finished lifecycle and construction future.
///
/// The router only invokes a recipe with a model from the family it was
/// registered for, so [`HarnessRequest::codex_model`] or
/// [`HarnessRequest::claude_model`] is the typed way to read the selection.
/// New fields may be added; the struct is not constructible outside this crate.
#[non_exhaustive]
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
    /// Native idle boundary to restore with the recipe's current host capabilities.
    /// Its unredacted transcript stays in memory and must not enter model arguments.
    pub snapshot: Option<ChildSnapshot>,
    /// Shared router to install on every per-agent weak handle.
    pub spawn_factory: Arc<dyn AgentFactory>,
}

impl HarnessRequest {
    /// The family whose recipe is being invoked.
    pub const fn family(&self) -> HarnessFamily {
        self.model.family()
    }

    /// The selected Responses model for a Codex recipe.
    ///
    /// Returns [`NanocodexError::InvalidRequest`] when the request belongs to
    /// another family, which only happens if a recipe is registered for the
    /// wrong [`HarnessFamily`].
    pub fn codex_model(&self) -> crate::agent::Result<Model> {
        self.model
            .as_codex()
            .ok_or_else(|| self.wrong_family(HarnessFamily::Codex))
    }

    /// The selected Messages model for a Claude recipe.
    ///
    /// Returns [`NanocodexError::InvalidRequest`] when the request belongs to
    /// another family, which only happens if a recipe is registered for the
    /// wrong [`HarnessFamily`].
    pub fn claude_model(&self) -> crate::agent::Result<ClaudeModel> {
        self.model
            .as_claude()
            .ok_or_else(|| self.wrong_family(HarnessFamily::Claude))
    }

    fn wrong_family(&self, expected: HarnessFamily) -> NanocodexError {
        NanocodexError::InvalidRequest(format!(
            "{expected} recipe received {} model {}",
            self.model.family(),
            self.model
        ))
    }
}

impl fmt::Debug for HarnessRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessRequest")
            .field("model", &self.model)
            .field("thinking", &self.thinking)
            .field("options", &self.options)
            .field("parent", &self.parent.as_ref().map(AgentHandle::session_id))
            .field("has_host_context", &self.host_context.is_some())
            .field("has_snapshot", &self.snapshot.is_some())
            .finish_non_exhaustive()
    }
}

/// A reusable registry of explicitly authorized native harness recipes.
///
/// Cloning is cheap and shares the same recipes.
#[derive(Clone)]
pub struct Harness {
    inner: Arc<Router>,
}

/// Configures concrete recipes before crossing the lifecycle-erasure boundary.
#[derive(Default)]
#[must_use = "a HarnessBuilder does nothing until .build() is called"]
pub struct HarnessBuilder {
    recipes: HashMap<HarnessFamily, Recipe>,
}

fn sorted_families<'a>(families: impl Iterator<Item = &'a HarnessFamily>) -> Vec<HarnessFamily> {
    let mut families: Vec<_> = families.copied().collect();
    families.sort_by_key(|family| HarnessFamily::ALL.iter().position(|known| known == family));
    families
}

impl fmt::Debug for HarnessBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessBuilder")
            .field("families", &sorted_families(self.recipes.keys()))
            .finish()
    }
}

impl fmt::Debug for Harness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Harness")
            .field("families", &self.families())
            .finish()
    }
}

impl HarnessBuilder {
    /// Registers one native construction recipe for a family.
    ///
    /// Registering the same family again replaces its earlier recipe.
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
    ///
    /// Registering the same family again replaces its earlier recipe.
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

    /// Registered families, in [`HarnessFamily::ALL`] order.
    pub fn families(&self) -> Vec<HarnessFamily> {
        sorted_families(self.inner.recipes.keys())
    }

    /// Whether a recipe is registered for this family.
    pub fn supports(&self, family: HarnessFamily) -> bool {
        self.inner.recipes.contains_key(&family)
    }

    /// Starts a new thread with one selected native model and its default effort.
    ///
    /// Accepts a [`HarnessModel`] or either family's model directly, for
    /// example `harness.start(Model::Sol)` or `harness.start(ClaudeModel::Sonnet55)`.
    pub async fn start(&self, model: impl Into<HarnessModel>) -> AgentResult {
        let model = model.into();
        self.start_with(
            SpawnOptions::new()
                .harness(model.family())
                .harness_model(model),
        )
        .await
    }

    /// Starts a new thread after validating its family, model and effort.
    ///
    /// The family is taken from the selected model when only a model is set;
    /// with neither selected, the Codex family default is used.
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
        snapshot: Option<ChildSnapshot>,
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
            snapshot,
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

    fn fork(&self, parent: AgentHandle) -> BackendFuture<AgentResult> {
        Box::pin(async move { parent.fork().await })
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
            let family = options
                .selected_harness()
                .unwrap_or(parent.harness_family());
            if family == parent.harness_family() {
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

    fn restore(
        &self,
        parent: AgentHandle,
        snapshot: ChildSnapshot,
        host_context: Option<Arc<str>>,
    ) -> BackendFuture<AgentResult> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            parent.ensure_available().await?;
            let model = snapshot.model();
            if model.family() == parent.harness_family() {
                return parent.restore_native_runtime(snapshot, host_context).await;
            }
            let thinking = match &snapshot {
                ChildSnapshot::Codex(snapshot) => snapshot.thinking,
                ChildSnapshot::Native { thinking, .. } => *thinking,
            };
            let options = SpawnOptions::new()
                .harness(model.family())
                .harness_model(model)
                .thinking(thinking);
            options.validate_harness()?;
            inner
                .construct(Some(parent), options, host_context, Some(snapshot))
                .await
        })
    }
}
