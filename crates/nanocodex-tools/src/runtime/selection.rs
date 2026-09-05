use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};

use crate::{Tool, ToolDefinition};
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
use crate::{ToolContext, ToolOutput};
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
use async_trait::async_trait;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
use serde_json::Value;
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
use std::ffi::OsString;

#[cfg(all(not(target_family = "wasm"), feature = "native"))]
pub(crate) const CODEX_THREAD_ID_ENV_VAR: &str = "CODEX_THREAD_ID";

/// Nanocodex's model-visible tool exposure policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ToolExposure {
    /// Expose normal tools only through Code Mode's `exec` entrypoint.
    #[default]
    CodeModeOnly,
    /// Expose `exec` and `wait` before ordinary direct tools, while retaining
    /// the same handlers for calls composed through Code Mode.
    DirectAndCodeMode,
    /// Expose a tool directly without making it callable from Code Mode.
    DirectOnly,
    /// Keep a tool registered for dispatch without exposing it to the model.
    Hidden,
}

#[cfg(feature = "native")]
impl ToolExposure {
    #[cfg(not(target_family = "wasm"))]
    pub(super) const fn is_direct(self) -> bool {
        matches!(self, Self::DirectAndCodeMode | Self::DirectOnly)
    }

    pub(super) const fn is_available_in_code_mode(self) -> bool {
        matches!(self, Self::CodeModeOnly | Self::DirectAndCodeMode)
    }
}

#[derive(Clone)]
pub(crate) struct RegisteredTool {
    pub(crate) handler: Arc<dyn Tool>,
    #[cfg(feature = "native")]
    pub(crate) exposure: Option<ToolExposure>,
}

/// A lazily populated family of Code Mode tools.
///
/// Providers start with the agent driver, advertise only their small direct
/// tool surface initially, and may make additional tools callable at runtime.
#[cfg(all(not(target_family = "wasm"), feature = "native"))]
#[async_trait]
pub trait DynamicToolProvider: Send + Sync {
    /// Starts background discovery or connection work. Implementations must be idempotent.
    fn start(&self);

    /// Returns the provider's always-visible tools, such as `tool_search`.
    fn direct_tools(&self) -> Vec<Arc<dyn Tool>>;

    /// Returns the provider's direct tools for one model exposure policy.
    ///
    /// Providers normally expose the same tools under either policy. In
    /// particular, discovery entrypoints such as `tool_search` remain visible
    /// while the tools they discover stay deferred.
    fn direct_tools_for_exposure(&self, _exposure: ToolExposure) -> Vec<Arc<dyn Tool>> {
        self.direct_tools()
    }

    /// Returns deferred tools currently callable from new Code Mode cells.
    fn available_definitions(&self) -> Vec<ToolDefinition>;

    /// Returns compact, stable guidance for provider tools that should be
    /// discoverable before the model starts its first Code Mode cell.
    ///
    /// The complete definitions remain runtime-only and available through
    /// `ALL_TOOLS`; summaries keep large dynamic schemas out of the model
    /// request prefix.
    fn code_mode_tool_summaries(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Returns whether this provider currently exposes `name`.
    fn contains(&self, name: &str) -> bool {
        self.available_definitions()
            .iter()
            .any(|definition| definition.name() == name)
    }

    /// Returns whether a callable deferred tool is safe to execute in parallel.
    ///
    /// Providers are conservative by default. Implementations must return
    /// `true` only for a currently callable tool with explicit safety
    /// metadata.
    fn supports_parallel_tool_calls(&self, _name: &str) -> bool {
        false
    }

    /// Executes a callable deferred tool, or returns `None` when this provider
    /// does not currently expose `name`.
    ///
    /// The owning runtime converts handler panics into a failed `aborted`
    /// output; they never unwind through the runtime owner.
    async fn execute(
        &self,
        name: &str,
        input: Value,
        context: ToolContext<'_>,
    ) -> Option<ToolOutput>;
}

mod source_sealed {
    pub trait Sealed {}

    impl<T: crate::Tool + 'static> Sealed for T {}

    #[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
    impl Sealed for crate::WorkspaceTools {}

    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    impl Sealed for crate::mcp::Mcp {}
}

/// One capability source accepted by [`ToolsBuilder::add`].
///
/// The sealed implementations cover fixed [`Tool`] values, workspace tools,
/// and native MCP families when those features are enabled.
pub trait ToolSource: source_sealed::Sealed + Sized {
    #[doc(hidden)]
    fn install(self, builder: ToolsBuilder) -> ToolsBuilder;
}

impl<T: Tool + 'static> ToolSource for T {
    fn install(self, builder: ToolsBuilder) -> ToolsBuilder {
        builder.tool(self)
    }
}

#[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
impl ToolSource for crate::WorkspaceTools {
    fn install(self, mut builder: ToolsBuilder) -> ToolsBuilder {
        if builder.tools.workspace_tools.is_some() {
            builder.duplicate_workspace = true;
        }
        builder.tools.workspace = true;
        builder.tools.workspace_tools = Some(self);
        builder
    }
}

#[cfg(all(not(target_family = "wasm"), feature = "native"))]
impl ToolSource for crate::mcp::Mcp {
    fn install(self, mut builder: ToolsBuilder) -> ToolsBuilder {
        let provider = Arc::new(self);
        builder
            .tools
            .providers
            .push(Arc::clone(&provider) as Arc<dyn DynamicToolProvider>);
        builder.tools.attachment_mcps.push(provider);
        builder.refresh_provider_direct();
        builder
    }
}

/// Declarative selection of the built-in tools installed for an agent.
#[derive(Clone)]
pub struct Tools {
    exposure: ToolExposure,
    workspace: bool,
    web_search: bool,
    image_generation: bool,
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub(super) working_directory: Option<Arc<str>>,
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub(super) default_shell: Option<Arc<str>>,
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    process_environment: Arc<Vec<(OsString, OsString)>>,
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    remote_http_client: Option<reqwest::Client>,
    pub(crate) registered: Vec<RegisteredTool>,
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub(super) provider_direct: Vec<Arc<dyn Tool>>,
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub(super) providers: Vec<Arc<dyn DynamicToolProvider>>,
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub(crate) attachment_mcps: Vec<Arc<crate::mcp::Mcp>>,
    #[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
    pub(crate) workspace_tools: Option<crate::WorkspaceTools>,
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub(super) deferred_tools_guidance_enabled: bool,
    #[cfg(feature = "native")]
    pub(crate) embedded_host: Option<Arc<dyn crate::embedded::CodeModeHost>>,
    #[cfg(feature = "native")]
    pub(crate) embedded_session_id: Option<Arc<str>>,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            exposure: ToolExposure::default(),
            workspace: true,
            web_search: true,
            image_generation: true,
            #[cfg(all(not(target_family = "wasm"), feature = "native"))]
            working_directory: None,
            #[cfg(all(not(target_family = "wasm"), feature = "native"))]
            default_shell: None,
            #[cfg(all(not(target_family = "wasm"), feature = "native"))]
            process_environment: Arc::new(Vec::new()),
            #[cfg(all(not(target_family = "wasm"), feature = "native"))]
            remote_http_client: None,
            registered: Vec::new(),
            #[cfg(all(not(target_family = "wasm"), feature = "native"))]
            provider_direct: Vec::new(),
            #[cfg(all(not(target_family = "wasm"), feature = "native"))]
            providers: Vec::new(),
            #[cfg(all(not(target_family = "wasm"), feature = "native"))]
            attachment_mcps: Vec::new(),
            #[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
            workspace_tools: None,
            #[cfg(all(not(target_family = "wasm"), feature = "native"))]
            deferred_tools_guidance_enabled: false,
            #[cfg(feature = "native")]
            embedded_host: None,
            #[cfg(feature = "native")]
            embedded_session_id: None,
        }
    }
}

#[cfg(all(not(target_family = "wasm"), feature = "native"))]
impl fmt::Debug for Tools {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let remote_http_client_configured = self.remote_http_client.is_some();
        formatter
            .debug_struct("Tools")
            .field("exposure", &self.exposure)
            .field("workspace", &self.workspace)
            .field("web_search", &self.web_search)
            .field("image_generation", &self.image_generation)
            .field("working_directory", &self.working_directory)
            .field("default_shell", &self.default_shell)
            .field("process_environment_count", &self.process_environment.len())
            .field(
                "remote_http_client_configured",
                &remote_http_client_configured,
            )
            .field(
                "registered",
                &self
                    .registered
                    .iter()
                    .map(|tool| tool.handler.definition().name().to_owned())
                    .collect::<Vec<_>>(),
            )
            .field(
                "provider_direct",
                &self
                    .provider_direct
                    .iter()
                    .map(|tool| tool.definition().name().to_owned())
                    .collect::<Vec<_>>(),
            )
            .field("provider_count", &self.providers.len())
            .field("attachment_mcp_count", &self.attachment_mcps.len())
            .field(
                "workspace_tools_configured",
                &self.workspace_tools.is_some(),
            )
            .finish()
    }
}

#[cfg(any(target_family = "wasm", not(feature = "native")))]
impl fmt::Debug for Tools {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("Tools");
        debug
            .field("exposure", &self.exposure)
            .field("workspace", &self.workspace)
            .field("web_search", &self.web_search)
            .field("image_generation", &self.image_generation)
            .field(
                "registered",
                &self
                    .registered
                    .iter()
                    .map(|tool| tool.handler.definition().name().to_owned())
                    .collect::<Vec<_>>(),
            );
        #[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
        debug.field(
            "workspace_tools_configured",
            &self.workspace_tools.is_some(),
        );
        debug.finish()
    }
}

impl Tools {
    /// Starts a builder with all standard tools enabled.
    #[must_use]
    pub fn builder() -> ToolsBuilder {
        ToolsBuilder::default()
    }

    /// Resumes configuring this tool selection while preserving its built-ins,
    /// registered tools, and dynamic providers.
    #[must_use]
    pub const fn into_builder(self) -> ToolsBuilder {
        ToolsBuilder {
            tools: self,
            duplicate_workspace: false,
        }
    }

    /// Returns the model-visible tool exposure policy.
    #[must_use]
    pub const fn exposure(&self) -> ToolExposure {
        self.exposure
    }

    /// Returns whether the standard workspace tools are enabled.
    #[must_use]
    pub const fn workspace_enabled(&self) -> bool {
        self.workspace
    }

    /// Returns whether the standard web-search tool is enabled.
    #[must_use]
    pub const fn web_search_enabled(&self) -> bool {
        self.web_search
    }

    /// Returns whether the standard image-generation tool is enabled.
    #[must_use]
    pub const fn image_generation_enabled(&self) -> bool {
        self.image_generation
    }

    /// Returns this tool selection bound to one agent session.
    ///
    /// An embedded execution host receives the session ID directly. Native
    /// workspace commands additionally receive it through `CODEX_THREAD_ID`.
    /// This binding does not mutate other clones of the tool selection.
    #[must_use]
    #[cfg(feature = "native")]
    pub fn for_session(mut self, session_id: &str) -> Self {
        self.embedded_session_id = Some(Arc::from(session_id));
        #[cfg(not(target_family = "wasm"))]
        self.insert_process_environment(CODEX_THREAD_ID_ENV_VAR.into(), session_id.into());
        self
    }

    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub(super) fn process_environment(&self) -> Arc<Vec<(OsString, OsString)>> {
        Arc::clone(&self.process_environment)
    }

    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    fn insert_process_environment(&mut self, name: OsString, value: OsString) {
        let environment = Arc::make_mut(&mut self.process_environment);
        environment.retain(|(candidate, _)| candidate != &name);
        environment.push((name, value));
    }

    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub(super) fn remote_http_client(&self) -> Option<reqwest::Client> {
        self.remote_http_client.clone()
    }

    /// Starts all dynamic providers without waiting for their handshakes.
    #[cfg(feature = "native")]
    #[cfg(not(target_family = "wasm"))]
    pub fn start_providers(&self) {
        for provider in &self.providers {
            provider.start();
        }
    }

    /// Starts all dynamic providers without waiting for their handshakes.
    #[cfg(all(feature = "native", target_family = "wasm"))]
    pub const fn start_providers(&self) {}

    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub(crate) fn has_unattachable_provider(&self) -> bool {
        self.providers.len() != self.attachment_mcps.len()
    }

    #[cfg(not(feature = "native"))]
    pub(crate) const fn has_unattachable_provider(&self) -> bool {
        false
    }
}

/// Builder for the built-in tool selection.
#[derive(Default)]
pub struct ToolsBuilder {
    tools: Tools,
    duplicate_workspace: bool,
}

/// Invalid declarative tool selection.
#[derive(Debug, thiserror::Error)]
pub enum ToolsBuildError {
    /// A custom definition has an empty registry name.
    #[error("tool name must not be empty")]
    EmptyName,

    /// The model-visible working-directory override is empty.
    #[error("working directory override must not be empty")]
    EmptyWorkingDirectory,

    /// The model-visible shell override is empty.
    #[error("default shell override must not be empty")]
    EmptyDefaultShell,

    /// Two custom tools use the same definition name.
    #[error("tool name `{0}` is registered more than once")]
    DuplicateName(Box<str>),

    /// A singleton tool source was added more than once.
    #[error("tool source `{0}` is configured more than once")]
    DuplicateSource(&'static str),

    /// A custom tool collides with an enabled built-in tool.
    #[error("tool name `{0}` conflicts with an enabled built-in tool")]
    BuiltInName(Box<str>),

    /// A custom tool collides with a host-owned routing tool.
    #[error("tool name `{0}` is reserved by the Code Mode host")]
    ReservedName(Box<str>),

    /// A model-visible name does not match the Responses tool-name grammar.
    #[error(
        "invalid public tool name `{0}`; expected 1-128 ASCII bytes beginning with an alphanumeric and containing only alphanumerics, `.`, `_`, `:`, or `-`"
    )]
    InvalidPublicName(Box<str>),

    /// Two Code Mode tools map to the same JavaScript identifier.
    #[error("tool names `{first}` and `{second}` both normalize to Code Mode name `{normalized}`")]
    NormalizedNameCollision {
        /// First public tool name using the normalized identifier.
        first: Box<str>,
        /// Later public tool name using the normalized identifier.
        second: Box<str>,
        /// Conflicting JavaScript identifier.
        normalized: Box<str>,
    },
}

impl ToolsBuilder {
    /// Adds one capability source to this recipe.
    ///
    /// Fixed tools, workspace tools, and MCP use this same composition path;
    /// execution placement is selected only after the recipe is built.
    #[must_use]
    #[allow(
        clippy::should_implement_trait,
        reason = "builder composition is intentionally named add and is not arithmetic"
    )]
    pub fn add<S: ToolSource>(self, source: S) -> Self {
        source.install(self)
    }

    /// Selects whether registered tools are also exposed directly to the model.
    ///
    /// The default is [`ToolExposure::CodeModeOnly`]. This changes only the
    /// model-visible declaration set; all registered handlers remain callable
    /// from Code Mode.
    #[must_use]
    pub const fn exposure(mut self, exposure: ToolExposure) -> Self {
        self.tools.exposure = exposure;
        self
    }

    /// Starts from an empty built-in tool set.
    #[must_use]
    pub const fn without_defaults(mut self) -> Self {
        self.tools.workspace = false;
        self.tools.web_search = false;
        self.tools.image_generation = false;
        self
    }

    /// Enables or disables the standard command, patch, plan, and file tools.
    #[must_use]
    #[cfg(not(target_family = "wasm"))]
    #[allow(clippy::missing_const_for_fn)]
    pub fn workspace(mut self, enabled: bool) -> Self {
        self.tools.workspace = enabled;
        #[cfg(all(not(target_family = "wasm"), feature = "workspace-runtime"))]
        if !enabled {
            self.tools.workspace_tools = None;
        }
        self
    }

    /// Enables or disables the standard command, patch, plan, and file tools.
    #[must_use]
    #[cfg(target_family = "wasm")]
    pub const fn workspace(mut self, enabled: bool) -> Self {
        self.tools.workspace = enabled;
        self
    }

    /// Enables or disables the built-in direct web-search tool.
    #[must_use]
    pub const fn web_search(mut self, enabled: bool) -> Self {
        self.tools.web_search = enabled;
        self
    }

    /// Enables or disables the built-in image-generation tool.
    #[must_use]
    pub const fn image_generation(mut self, enabled: bool) -> Self {
        self.tools.image_generation = enabled;
        self
    }

    /// Overrides the default working directory described to the model.
    #[must_use]
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub fn working_directory(mut self, directory: impl Into<Arc<str>>) -> Self {
        self.tools.working_directory = Some(directory.into());
        self
    }

    /// Overrides the default shell described to the model.
    #[must_use]
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub fn default_shell(mut self, shell: impl Into<Arc<str>>) -> Self {
        self.tools.default_shell = Some(shell.into());
        self
    }

    /// Adds explicit environment overrides to workspace-tool child processes.
    ///
    /// Overrides are scoped to commands spawned by this tool selection and do
    /// not mutate the embedding process. A later value for the same name wins.
    #[must_use]
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub fn process_environment<I, K, V>(mut self, variables: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        for (name, value) in variables {
            self.tools
                .insert_process_environment(name.into(), value.into());
        }
        self
    }

    /// Overrides the HTTP client used by in-process remote tools.
    #[must_use]
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub fn remote_http_client(mut self, client: reqwest::Client) -> Self {
        self.tools.remote_http_client = Some(client);
        self
    }

    /// Adds a function or freeform tool to the runtime.
    #[must_use]
    pub fn tool<T: Tool + 'static>(mut self, tool: T) -> Self {
        self.tools.registered.push(RegisteredTool {
            handler: Arc::new(tool),
            #[cfg(feature = "native")]
            exposure: None,
        });
        self
    }

    /// Adds a function or freeform tool with an explicit model-facing exposure.
    #[must_use]
    pub fn tool_with_exposure<T: Tool + 'static>(
        mut self,
        tool: T,
        exposure: ToolExposure,
    ) -> Self {
        #[cfg(not(feature = "native"))]
        let _ = exposure;
        self.tools.registered.push(RegisteredTool {
            handler: Arc::new(tool),
            #[cfg(feature = "native")]
            exposure: Some(exposure),
        });
        self
    }

    /// Adds a dynamic family of Code Mode tools.
    #[must_use]
    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    pub fn provider<P: DynamicToolProvider + 'static>(mut self, provider: P) -> Self {
        let provider: Arc<dyn DynamicToolProvider> = Arc::new(provider);
        self.tools.providers.push(provider);
        self.refresh_provider_direct();
        self
    }

    /// Validates tool names and finishes the runtime configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, duplicate, or enabled built-in tool names.
    #[allow(unused_mut, reason = "native provider refresh mutates this builder")]
    pub fn build(mut self) -> Result<Tools, ToolsBuildError> {
        if self.duplicate_workspace {
            return Err(ToolsBuildError::DuplicateSource("workspace"));
        }
        #[cfg(all(not(target_family = "wasm"), feature = "native"))]
        self.refresh_provider_direct();
        #[cfg(all(not(target_family = "wasm"), feature = "native"))]
        if self
            .tools
            .working_directory
            .as_deref()
            .is_some_and(|directory| directory.trim().is_empty())
        {
            return Err(ToolsBuildError::EmptyWorkingDirectory);
        }
        #[cfg(all(not(target_family = "wasm"), feature = "native"))]
        if self
            .tools
            .default_shell
            .as_deref()
            .is_some_and(|shell| shell.trim().is_empty())
        {
            return Err(ToolsBuildError::EmptyDefaultShell);
        }
        let mut names = HashSet::with_capacity(self.tools.registered.len());
        #[cfg(feature = "native")]
        let mut code_mode_names = HashMap::new();
        #[cfg(feature = "native")]
        if self.tools.exposure.is_available_in_code_mode() {
            for name in enabled_built_in_names(&self.tools) {
                insert_code_mode_name(&mut code_mode_names, name)?;
            }
        }
        for tool in &self.tools.registered {
            let definition = tool.handler.definition();
            let name = definition.name();
            #[cfg(feature = "native")]
            let exposure = tool.exposure.unwrap_or_else(|| self.tools.exposure());
            #[cfg(not(feature = "native"))]
            let exposure = self.tools.exposure();
            if exposure != ToolExposure::Hidden {
                validate_registered_tool_name(
                    name,
                    matches!(definition, ToolDefinition::ToolSearch { .. }),
                )?;
            } else if name.is_empty() {
                return Err(ToolsBuildError::EmptyName);
            }
            if host_owned_name(name)
                || (name == "tool_search"
                    && !matches!(definition, ToolDefinition::ToolSearch { .. }))
            {
                return Err(ToolsBuildError::ReservedName(name.into()));
            }
            if built_in_name(&self.tools, name) {
                return Err(ToolsBuildError::BuiltInName(name.into()));
            }
            if !names.insert(name.to_owned()) {
                return Err(ToolsBuildError::DuplicateName(name.into()));
            }
            #[cfg(feature = "native")]
            if exposure.is_available_in_code_mode()
                && !matches!(definition, ToolDefinition::ToolSearch { .. })
            {
                insert_code_mode_name(&mut code_mode_names, name)?;
            }
        }
        #[cfg(all(not(target_family = "wasm"), feature = "native"))]
        for tool in &self.tools.provider_direct {
            let definition = tool.definition();
            let name = definition.name();
            validate_registered_tool_name(
                name,
                matches!(definition, ToolDefinition::ToolSearch { .. }),
            )?;
            if host_owned_name(name) {
                return Err(ToolsBuildError::ReservedName(name.into()));
            }
            if built_in_name(&self.tools, name) {
                return Err(ToolsBuildError::BuiltInName(name.into()));
            }
            if !names.insert(name.to_owned()) {
                return Err(ToolsBuildError::DuplicateName(name.into()));
            }
            if self.tools.exposure.is_available_in_code_mode()
                && !matches!(definition, ToolDefinition::ToolSearch { .. })
            {
                insert_code_mode_name(&mut code_mode_names, name)?;
            }
        }
        #[cfg(all(not(target_family = "wasm"), feature = "native"))]
        for provider in &self.tools.providers {
            for definition in provider.available_definitions() {
                let name = definition.name();
                validate_registered_tool_name(
                    name,
                    matches!(definition, ToolDefinition::ToolSearch { .. }),
                )?;
                if host_owned_name(name)
                    || (name == "tool_search"
                        && !matches!(definition, ToolDefinition::ToolSearch { .. }))
                {
                    return Err(ToolsBuildError::ReservedName(name.into()));
                }
                if built_in_name(&self.tools, name) {
                    return Err(ToolsBuildError::BuiltInName(name.into()));
                }
                if !names.insert(name.to_owned()) {
                    return Err(ToolsBuildError::DuplicateName(name.into()));
                }
                if !matches!(definition, ToolDefinition::ToolSearch { .. }) {
                    insert_code_mode_name(&mut code_mode_names, name)?;
                }
            }
        }
        Ok(self.tools)
    }

    #[cfg(all(not(target_family = "wasm"), feature = "native"))]
    fn refresh_provider_direct(&mut self) {
        self.tools.deferred_tools_guidance_enabled = self.tools.providers.iter().any(|provider| {
            provider
                .direct_tools()
                .iter()
                .any(|tool| matches!(tool.definition(), ToolDefinition::ToolSearch { .. }))
        });
        self.tools.provider_direct = self
            .tools
            .providers
            .iter()
            .flat_map(|provider| provider.direct_tools_for_exposure(self.tools.exposure))
            .collect();
    }
}

#[cfg(not(target_family = "wasm"))]
#[derive(Debug, thiserror::Error)]
pub(crate) enum PublicToolCatalogError {
    #[error("invalid public tool name `{0}`")]
    InvalidName(Box<str>),
    #[error(
        "public tool names `{first}` and `{second}` both normalize to Code Mode name `{normalized}`"
    )]
    NormalizedNameCollision {
        first: Box<str>,
        second: Box<str>,
        normalized: Box<str>,
    },
}

#[cfg(not(target_family = "wasm"))]
pub(crate) fn validate_public_tool_catalog_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> Result<(), PublicToolCatalogError> {
    let mut normalized_names = HashMap::new();
    for name in names {
        if !valid_public_tool_name(name) {
            return Err(PublicToolCatalogError::InvalidName(name.into()));
        }
        let normalized = normalize_public_tool_name(name);
        if let Some(first) = normalized_names.insert(normalized.clone(), name.to_owned()) {
            return Err(PublicToolCatalogError::NormalizedNameCollision {
                first: first.into(),
                second: name.into(),
                normalized: normalized.into(),
            });
        }
    }
    Ok(())
}

fn validate_registered_tool_name(
    name: &str,
    native_tool_search: bool,
) -> Result<(), ToolsBuildError> {
    if name.is_empty() {
        return Err(ToolsBuildError::EmptyName);
    }
    if !valid_public_tool_name_grammar(name) && !(native_tool_search && name == "tool_search") {
        return Err(ToolsBuildError::InvalidPublicName(name.into()));
    }
    Ok(())
}

#[cfg(not(target_family = "wasm"))]
fn valid_public_tool_name(name: &str) -> bool {
    valid_public_tool_name_grammar(name) && !matches!(name, "exec" | "tool_search" | "wait")
}

fn valid_public_tool_name_grammar(name: &str) -> bool {
    name.len() <= 128
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

#[cfg(feature = "native")]
fn insert_code_mode_name(
    names: &mut HashMap<String, String>,
    name: &str,
) -> Result<(), ToolsBuildError> {
    let normalized = normalize_public_tool_name(name);
    if let Some(first) = names.insert(normalized.clone(), name.to_owned()) {
        return Err(ToolsBuildError::NormalizedNameCollision {
            first: first.into(),
            second: name.into(),
            normalized: normalized.into(),
        });
    }
    Ok(())
}

pub(crate) fn normalize_public_tool_name(name: &str) -> String {
    name.chars()
        .enumerate()
        .map(|(index, character)| {
            let valid = if index == 0 {
                character == '_' || character == '$' || character.is_ascii_alphabetic()
            } else {
                character == '_' || character == '$' || character.is_ascii_alphanumeric()
            };
            if valid { character } else { '_' }
        })
        .collect()
}

fn host_owned_name(name: &str) -> bool {
    matches!(name, "exec" | "wait")
}

fn built_in_name(tools: &Tools, name: &str) -> bool {
    (tools.workspace
        && matches!(
            name,
            "exec_command" | "write_stdin" | "update_plan" | "apply_patch" | "view_image"
        ))
        || (tools.web_search && name == "web__run")
        || (tools.image_generation && name == "image_gen__imagegen")
}

#[cfg(feature = "native")]
fn enabled_built_in_names(tools: &Tools) -> impl Iterator<Item = &'static str> {
    [
        (tools.workspace, "exec_command"),
        (tools.workspace, "write_stdin"),
        (tools.workspace, "update_plan"),
        (tools.workspace, "apply_patch"),
        (tools.workspace, "view_image"),
        (tools.web_search, "web__run"),
        (tools.image_generation, "image_gen__imagegen"),
    ]
    .into_iter()
    .filter_map(|(enabled, name)| enabled.then_some(name))
}
