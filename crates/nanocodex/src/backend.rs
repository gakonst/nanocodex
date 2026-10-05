//! Native provider selection with the CLI's workspace capabilities.

use crate::{AgentEvents, HarnessFamily, HarnessModel, Nanocodex, NanocodexError};
use nanocodex_agent::{BuilderBackend, Result};
#[cfg(feature = "openai")]
use nanocodex_oai_tools::Tools;
use std::{path::PathBuf, sync::Arc};

/// A provider choice with the same type for Codex and Claude.
///
/// The native facade installs local workspace and subagent tools automatically.
/// Commands run with the embedding process's permissions; the workspace is a
/// working directory, not an OS sandbox. No ambient credentials are discovered.
#[derive(Clone)]
pub struct Backend {
    family: HarnessFamily,
    key: String,
    endpoint: Option<String>,
    model: HarnessModel,
}

impl Backend {
    /// Selects Codex with an explicit OpenAI API key and its default model.
    #[cfg(feature = "openai")]
    pub fn codex(api_key: impl Into<String>) -> Result<Self> {
        Self::new(HarnessFamily::Codex, api_key.into())
    }

    /// Selects Claude with an explicit Anthropic API key and its default model.
    #[cfg(feature = "claude")]
    pub fn claude(api_key: impl Into<String>) -> Result<Self> {
        Self::new(HarnessFamily::Claude, api_key.into())
    }

    fn new(family: HarnessFamily, key: String) -> Result<Self> {
        if key.trim().is_empty() || reqwest::header::HeaderValue::from_str(&key).is_err() {
            return Err(invalid(
                "API key must be nonempty and valid in an HTTP header",
            ));
        }
        Ok(Self {
            family,
            key,
            endpoint: None,
            model: family.default_model(),
        })
    }

    /// Pins a catalog model belonging to this provider family.
    pub fn model(mut self, model: HarnessModel) -> Result<Self> {
        if model.family() != self.family {
            return Err(invalid("model family does not match backend"));
        }
        self.model = model;
        Ok(self)
    }

    /// Uses a caller-selected endpoint: an OpenAI API base (ending in `/v1`)
    /// or a complete Claude Messages URL. HTTP is supported for local fixtures.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Result<Self> {
        let endpoint = endpoint.into();
        let url =
            reqwest::Url::parse(&endpoint).map_err(|_| invalid("invalid backend endpoint"))?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid(
                "backend endpoint must be an HTTP(S) URL without credentials or fragment",
            ));
        }
        self.endpoint = Some(endpoint);
        Ok(self)
    }
}

enum NativeBuilder {
    #[cfg(feature = "openai")]
    Codex(crate::NanocodexBuilder),
    #[cfg(feature = "claude")]
    Claude(nanocodex_claude::ClaudeBuilder),
}

/// Common native builder returned by `Nanocodex::builder(Backend::codex(key)?)`.
///
/// Concrete `OpenAi` and `Claude` builders remain available for custom transport,
/// tool, authentication and execution policy.
pub struct BackendBuilder {
    native: Result<NativeBuilder>,
    workspace: Option<PathBuf>,
}

impl BuilderBackend for Backend {
    type Builder = BackendBuilder;
    fn into_builder(self) -> BackendBuilder {
        let native = (|| match self.family {
            #[cfg(feature = "openai")]
            HarnessFamily::Codex => {
                let HarnessModel::Codex(model) = self.model else {
                    return Err(invalid("model family does not match backend"));
                };
                let mut provider =
                    crate::OpenAi::builder(crate::oai::auth::OpenAiAuth::api_key(self.key))
                        .model(model);
                if let Some(endpoint) = self.endpoint {
                    provider = provider
                        .api_base_url(endpoint)
                        .transport(crate::oai::transport::ResponsesTransport::Https)
                        .websocket_warmup(false);
                }
                Ok(NativeBuilder::Codex(Nanocodex::builder(
                    provider.build().map_err(invalid)?,
                )))
            }
            #[cfg(feature = "claude")]
            HarnessFamily::Claude => {
                let http = reqwest::Client::builder().build().map_err(invalid)?;
                let client = match self.endpoint {
                    Some(endpoint) => nanocodex_claude::ClaudeClient::new(http, endpoint, self.key),
                    None => nanocodex_claude::ClaudeClient::official(http, self.key),
                };
                Ok(NativeBuilder::Claude(Nanocodex::builder(
                    nanocodex_claude::Claude::new(client, self.model.as_str()),
                )))
            }
            #[allow(unreachable_patterns)]
            _ => Err(invalid("backend feature is not enabled")),
        })();
        BackendBuilder {
            native,
            workspace: None,
        }
    }
}

impl BackendBuilder {
    /// Selects the working directory for native tools. Defaults to current cwd.
    #[must_use]
    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// Replaces the agent's system instructions.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        let instructions = instructions.into();
        self.native = self.native.map(|native| match native {
            #[cfg(feature = "openai")]
            NativeBuilder::Codex(builder) => {
                NativeBuilder::Codex(builder.instructions(instructions))
            }
            #[cfg(feature = "claude")]
            NativeBuilder::Claude(builder) => NativeBuilder::Claude(builder.system(instructions)),
        });
        self
    }

    /// Builds the selected native runtime, tools, and common lifecycle handle.
    ///
    /// Requires an active Tokio runtime and an existing workspace directory.
    pub fn build(self) -> Result<(Nanocodex, AgentEvents)> {
        tokio::runtime::Handle::try_current()
            .map_err(|_| invalid("BackendBuilder::build requires an active Tokio runtime"))?;
        let workspace = self
            .workspace
            .map(Ok)
            .unwrap_or_else(std::env::current_dir)
            .map_err(invalid)?
            .canonicalize()
            .map_err(invalid)?;
        if !workspace.is_dir() {
            return Err(invalid("workspace must be a directory"));
        }
        let (registry, control, mut updates) =
            nanocodex_subagents::channel(nanocodex_subagents::DEFAULT_MAX_SUBAGENTS);
        let tool_registry = Arc::clone(&registry);
        #[cfg(feature = "claude")]
        let shells = Arc::new(claude::Shells::default());
        #[cfg(feature = "claude")]
        let tool_shells = Arc::clone(&shells);
        let result = match self.native? {
            #[cfg(feature = "openai")]
            NativeBuilder::Codex(builder) => {
                let tools = Tools::builder().workspace(true).build().map_err(invalid)?;
                builder
                    .workspace(workspace)
                    .tools_factory(move |parent| {
                        let registry = Arc::clone(&tool_registry);
                        nanocodex_subagents::install_tools(tools.clone(), parent, registry)
                    })
                    .build()?
            }
            #[cfg(feature = "claude")]
            NativeBuilder::Claude(builder) => {
                use nanocodex_claude_tools::{ClaudeNotebook, ClaudeTasks, ClaudeWorkspaceFiles};
                let files = Arc::new(ClaudeWorkspaceFiles::new(&workspace).map_err(invalid)?);
                let notebook = Arc::new(ClaudeNotebook::new(&workspace).map_err(invalid)?);
                let shell_workspace = workspace.clone();
                builder
                    .workspace(workspace.to_string_lossy().into_owned())
                    .max_tokens(16_384)
                    .workspace_files(files)
                    .notebook(notebook)
                    .tasks(Arc::new(ClaudeTasks::new()))
                    .nested_web_search(true)
                    .client_tool_search()
                    .tools_factory(move |parent| {
                        let registry = Arc::clone(&tool_registry);
                        nanocodex_subagents::install_claude_tools(
                            claude::tools(shell_workspace.clone(), &tool_shells)?,
                            parent,
                            registry,
                        )
                    })
                    .build()?
            }
        };
        let (agent, events) = result;
        let session = agent.session_id().to_owned();
        let drain = tokio::spawn(async move { while updates.recv().await.is_some() {} });
        let agent = agent.with_shutdown_hook(move || async move {
            // Root admission has stopped before descendants and their tool
            // runtimes are joined. Keep registry ownership until cleanup ends.
            let _registry = registry;
            let children = control.close_all(&session).await.map_err(invalid);
            #[cfg(feature = "claude")]
            shells.cancel().await;
            drain.abort();
            let _ = drain.await;
            children
        })?;

        Ok((agent, events))
    }
}

#[cfg(feature = "durability")]
impl nanocodex_durability::DurableAgentExt for BackendBuilder {
    async fn durability(mut self, state: nanocodex_durability::DurableSession) -> Result<Self> {
        self.native = Ok(match self.native? {
            #[cfg(feature = "openai")]
            NativeBuilder::Codex(builder) => NativeBuilder::Codex(builder.durability(state).await?),
            #[cfg(feature = "claude")]
            NativeBuilder::Claude(builder) => {
                NativeBuilder::Claude(builder.durability(state).await?)
            }
        });
        Ok(self)
    }
}

fn invalid(error: impl std::fmt::Display) -> NanocodexError {
    NanocodexError::InvalidRequest(error.to_string())
}

#[cfg(feature = "claude")]
mod claude {
    use super::*;
    use nanocodex_claude_tools::{BashRequest, BashResult, ClaudeBash, SandboxBashExecutor};
    use nanocodex_oai_tools::{ToolContext, ToolInput, workspace_runtime::WorkspaceToolRuntime};
    use serde_json::{Value, json, value::to_raw_value};
    use tokio::time::{Duration, Instant};

    #[derive(Default)]
    pub(super) struct Shells(std::sync::Mutex<Vec<std::sync::Weak<WorkspaceToolRuntime>>>);
    impl Shells {
        pub(super) async fn cancel(&self) {
            let runtimes = std::mem::take(
                &mut *self
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            for runtime in runtimes {
                if let Some(runtime) = runtime.upgrade() {
                    runtime.control().cancel().await;
                }
            }
        }
    }

    pub(super) fn tools(
        workspace: PathBuf,
        shells: &Shells,
    ) -> Result<nanocodex_claude::ClaudeTools> {
        use nanocodex_claude::{ClaudeToolReply, ClaudeTools, ToolDefinition, ToolResultContent};
        let shell = Arc::new(shell(workspace, shells));
        let mut tools = ClaudeTools::new();
        for schema in ClaudeBash::<RetainedBash>::definitions() {
            let definition: ToolDefinition = serde_json::from_value(schema).map_err(invalid)?;
            let name = definition.name.clone();
            let shell = Arc::clone(&shell);
            tools = tools.tool_with_context(definition, move |input, _| {
                let shell = Arc::clone(&shell);
                let name = name.clone();
                async move {
                    shell
                        .execute(&name, input)
                        .await
                        .map(|text| ClaudeToolReply::success(ToolResultContent::Text(text)))
                }
            });
        }
        Ok(tools)
    }

    pub(super) fn shell(workspace: PathBuf, shells: &Shells) -> ClaudeBash<RetainedBash> {
        let runtime = Arc::new(WorkspaceToolRuntime::new(workspace));
        shells
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::downgrade(&runtime));
        ClaudeBash::new(RetainedBash {
            runtime,
            gate: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
    pub(super) struct RetainedBash {
        runtime: Arc<WorkspaceToolRuntime>,
        gate: Arc<tokio::sync::Mutex<()>>,
    }

    struct CancelShell {
        runtime: Option<Arc<WorkspaceToolRuntime>>,
        gate: Option<tokio::sync::OwnedMutexGuard<()>>,
    }
    impl Drop for CancelShell {
        fn drop(&mut self) {
            if let Some(runtime) = self.runtime.take() {
                let gate = self.gate.take();
                tokio::spawn(async move {
                    runtime.control().cancel().await;
                    drop(gate);
                });
            }
        }
    }

    impl SandboxBashExecutor for RetainedBash {
        async fn execute(&self, request: BashRequest) -> std::result::Result<BashResult, String> {
            let gate = Arc::clone(&self.gate).lock_owned().await;
            let mut cleanup = CancelShell {
                runtime: Some(Arc::clone(&self.runtime)),
                gate: Some(gate),
            };
            let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
            let context = ToolContext::new("claude", "native-bash", "bash", &[], 1024);
            let input = json!({"cmd":request.command,"yield_time_ms":250,"max_output_tokens":1024});
            let mut output = match tokio::time::timeout_at(
                deadline,
                self.runtime.execute_tool(
                    "exec_command",
                    ToolInput::Function(to_raw_value(&input).map_err(|e| e.to_string())?),
                    context,
                ),
            )
            .await
            {
                Ok(output) => output,
                Err(_) => {
                    self.runtime.control().cancel().await;
                    cleanup.runtime = None;
                    return Err("Bash timed out; retained process terminated".to_owned());
                }
            };
            let mut stdout = String::new();
            let mut truncated = false;
            loop {
                if !output.success {
                    return Err(output.structured_result().to_string());
                }
                let result = output.structured_result();
                let chunk = result
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let remaining = request.max_stdout_bytes.saturating_sub(stdout.len());
                let mut end = chunk.len().min(remaining);
                while !chunk.is_char_boundary(end) {
                    end -= 1;
                }
                stdout.push_str(&chunk[..end]);
                truncated |= end < chunk.len();
                truncated |= result
                    .get("original_token_count")
                    .and_then(Value::as_u64)
                    .is_some_and(|tokens| tokens.saturating_mul(4) > chunk.len() as u64 + 3);
                if let Some(code) = result.get("exit_code").and_then(Value::as_i64) {
                    cleanup.runtime = None;
                    return Ok(BashResult {
                        stdout,
                        stderr: String::new(),
                        exit_code: i32::try_from(code).map_err(|e| e.to_string())?,
                        truncated,
                    });
                }
                let session = result
                    .get("session_id")
                    .and_then(Value::as_i64)
                    .ok_or("Bash host returned neither exit status nor retained process")?;
                let input =
                    json!({"session_id":session,"yield_time_ms":250,"max_output_tokens":1024});
                output = match tokio::time::timeout_at(
                    deadline,
                    self.runtime.execute_tool(
                        "write_stdin",
                        ToolInput::Function(to_raw_value(&input).map_err(|e| e.to_string())?),
                        context,
                    ),
                )
                .await
                {
                    Ok(output) => output,
                    Err(_) => {
                        self.runtime.control().cancel().await;
                        cleanup.runtime = None;
                        return Err("Bash timed out; retained process terminated".to_owned());
                    }
                };
            }
        }
    }
}
