//! The one Codex construction recipe, shared by Codex and Claude roots.
//!
//! Roots and children of either family use the same Responses client policy,
//! tool catalog, instructions and workspace bindings, so a Codex child behaves
//! the same whichever family started the task tree.
use super::*;

/// Responses client for every Codex session in one task tree.
///
/// A Codex root connects eagerly so credential errors surface at startup; a
/// Claude root connects on the first Codex child.
#[derive(Clone)]
pub(super) struct CodexConnection {
    client: Arc<tokio::sync::OnceCell<OpenAi>>,
    connect: Arc<dyn Fn() -> Result<OpenAi> + Send + Sync>,
}

impl CodexConnection {
    pub(super) fn ready(client: OpenAi) -> Self {
        Self {
            client: Arc::new(tokio::sync::OnceCell::new_with(Some(client))),
            connect: Arc::new(|| Err(eyre!("the Codex client is already connected"))),
        }
    }

    pub(super) fn lazy(connect: impl Fn() -> Result<OpenAi> + Send + Sync + 'static) -> Self {
        Self {
            client: Arc::new(tokio::sync::OnceCell::new()),
            connect: Arc::new(connect),
        }
    }

    pub(super) async fn client(&self) -> std::result::Result<OpenAi, String> {
        self.client
            .get_or_try_init(|| async { (self.connect)().map_err(|error| format!("{error:#}")) })
            .await
            .cloned()
    }
}

/// Shared inputs of the Codex recipe; both roots fill the same fields.
#[derive(Clone)]
pub(super) struct CodexRecipe {
    pub(super) connection: CodexConnection,
    /// Complete Codex tool catalog (workspace, web, image and host tools).
    pub(super) tools: Tools,
    pub(super) instructions: Option<String>,
    pub(super) additional_instructions: Option<String>,
    pub(super) reasoning_mode: ReasoningMode,
    pub(super) fast_mode: bool,
    pub(super) codex_home: PathBuf,
    pub(super) workspace: PathBuf,
    pub(super) workspaces: Arc<claude::WorkspaceRegistry>,
    pub(super) registry: Option<Arc<nanocodex_subagents::Registry>>,
}

impl CodexRecipe {
    /// Configures one Codex session with this recipe's shared host policy.
    pub(super) fn builder(
        &self,
        client: OpenAi,
        session_id: SessionId,
        workspace: PathBuf,
    ) -> NanocodexBuilder {
        let tools = self.tools.clone();
        let registry = self.registry.clone();
        let workspaces = Arc::clone(&self.workspaces);
        let tool_workspace = workspace.clone();
        let mut builder = Nanocodex::builder(client)
            .session_id(session_id)
            .reasoning_mode(self.reasoning_mode)
            .fast_mode(self.fast_mode)
            .workspace(workspace)
            .codex_home(self.codex_home.clone())
            .tools_factory(move |agent| {
                workspaces
                    .seed(agent.session_id(), tool_workspace.clone())
                    .map_err(nanocodex::tools::runtime::ToolsBuildError::HostInitialization)?;
                if let Some(registry) = &registry {
                    nanocodex_subagents::install_tools(tools.clone(), agent, Arc::clone(registry))
                } else {
                    Ok(tools.clone())
                }
            });
        // User instructions come from both homes, like Claude sessions.
        if let Some(home) = crate::homes::resolve() {
            builder = builder.claude_home(home.claude_home());
        }
        if let Some(instructions) = self.instructions.clone() {
            builder = builder.instructions(instructions);
        }
        if let Some(instructions) = self.additional_instructions.clone() {
            builder = builder.additional_instructions(instructions);
        }
        builder
    }
}

/// Registers the Codex family with the shared host recipe.
pub(super) fn register_codex_recipe(
    harness: nanocodex::HarnessBuilder,
    recipe: CodexRecipe,
) -> nanocodex::HarnessBuilder {
    harness.register(HarnessFamily::Codex, move |request| {
        let recipe = recipe.clone();
        async move {
            let invalid = nanocodex::NanocodexError::InvalidRequest;
            let HarnessModel::Codex(model) = request.model else {
                return Err(invalid("Codex recipe received a Claude model".into()));
            };
            recipe
                .workspaces
                .authorize_cross_family(request.parent.as_ref())
                .map_err(invalid)?;
            // Reopened sessions keep their checkpointed or durable identity.
            let session_id = match (&request.checkpoint, &request.durable_state) {
                (Some(checkpoint), _) => checkpoint
                    .session_id()
                    .parse::<SessionId>()
                    .map_err(|error| invalid(error.to_string()))?,
                (None, Some(state)) => state
                    .state_id()
                    .parse::<SessionId>()
                    .map_err(|error| invalid(error.to_string()))?,
                (None, None) => SessionId::new(),
            };
            let session_key = session_id.to_string();
            if let Some(parent) = &request.parent {
                recipe
                    .workspaces
                    .initialize(parent.session_id(), &session_key)
            } else {
                recipe
                    .workspaces
                    .seed(&session_key, recipe.workspace.clone())
            }
            .map_err(invalid)?;
            let workspace = recipe.workspaces.current(&session_key).map_err(invalid)?;
            let client = recipe.connection.client().await.map_err(invalid)?;
            let mut builder = recipe
                .builder(client, session_id, workspace)
                .model(model)
                // The session's fast preference where this model offers it.
                .fast_mode(recipe.fast_mode && nanocodex::HarnessModel::Codex(model).supports_fast_mode())
                .thinking(request.thinking)
                .host_context(request.host_context)
                .spawn_factory(request.spawn_factory);
            if let Some(checkpoint) = request.checkpoint {
                builder = builder.resume(checkpoint)?;
            }
            if let Some(state) = request.durable_state {
                builder = nanocodex::DurableAgentExt::durability(builder, state).await?;
            }
            builder.build()
        }
    })
}
