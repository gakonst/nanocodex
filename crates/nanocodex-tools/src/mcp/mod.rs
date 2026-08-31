//! Background-handshaken MCP tools for Nanocodex Code Mode.

mod catalog;
mod client;
mod config;
mod oauth;
mod pagination;
mod stdio;

use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::{
    DynamicToolProvider, Tool, ToolContext, ToolInput, ToolOutput, ToolOutputContent, ToolResult,
};
use async_trait::async_trait;
use catalog::{ConnectedCatalog, ProviderState, ToolEntry};
use nanocodex_oai_api::tools::ToolDefinition;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{Instrument, info_span};

pub use config::{McpPaymentProvider, McpPendingPayment, McpServer, McpToolExposure};
pub use oauth::{McpOAuthCredentials, McpOAuthRefreshGuard, McpOAuthStore};

const MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES: usize = 4 * 1024;
const CODEX_ENCRYPTED_CONTENT_META_KEY: &str = "codex/encryptedContent";
const CODEX_IMAGE_DETAIL_META_KEY: &str = "codex/imageDetail";

fn same_origin_redirect_policy(
    replays_plaintext_proxy_credentials: bool,
) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        let Some(original) = attempt.previous().first() else {
            return attempt.error("MCP HTTP redirect has no original request URL");
        };
        if attempt.url().origin() != original.origin() {
            return attempt.error("MCP HTTP redirect to a different origin is not allowed");
        }
        if attempt.previous().len() > 10 {
            return attempt.error("MCP HTTP request exceeded the redirect limit");
        }
        let secure_destination = attempt.url().scheme() == "https"
            || match attempt.url().host() {
                Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)) => true,
                Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
                None => false,
            };
        if !secure_destination {
            return attempt.error("MCP HTTP redirects for non-loopback hostnames require HTTPS");
        }
        if replays_plaintext_proxy_credentials && original.scheme() != "https" {
            return attempt
                .error("MCP HTTP redirects cannot replay proxy credentials over plaintext HTTP");
        }
        attempt.follow()
    })
}

/// A configured family of MCP servers installed into [`crate::Tools`].
pub struct Mcp {
    servers: Arc<[NamedServer]>,
    state: Arc<ProviderState>,
    search: Arc<McpSearch>,
    oauth_store: Option<Arc<dyn McpOAuthStore>>,
    oauth_metadata: Arc<oauth::OAuthMetadataCache>,
    started: AtomicBool,
    startup_tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

pub(crate) struct PreparedMcpTool {
    entry: Arc<ToolEntry>,
    timeout: Duration,
}

struct NamedServer {
    name: String,
    model_namespace: String,
    config: McpServer,
}

#[derive(Clone, Copy)]
enum McpResourceOperation {
    List,
    ListTemplates,
    Read,
}

struct McpResourceTool {
    name: String,
    server_name: String,
    state: Arc<ProviderState>,
    timeout: Duration,
    operation: McpResourceOperation,
}

/// Builder for an MCP provider.
#[derive(Default)]
pub struct McpBuilder {
    servers: BTreeMap<String, McpServer>,
    oauth_store: Option<Arc<dyn McpOAuthStore>>,
    duplicate: Option<String>,
}

/// Cheap control handle for reconnecting and authorizing a running MCP provider.
#[derive(Clone)]
pub struct McpHandle {
    servers: Arc<[NamedServer]>,
    state: Arc<ProviderState>,
    oauth_store: Option<Arc<dyn McpOAuthStore>>,
    oauth_metadata: Arc<oauth::OAuthMetadataCache>,
}

/// An in-progress browser OAuth login.
pub struct McpLogin {
    authorization_url: String,
    completion: Option<tokio::task::JoinHandle<Result<usize, McpControlError>>>,
}

/// Failure while controlling an already configured MCP provider.
#[derive(Debug, thiserror::Error)]
pub enum McpControlError {
    /// No configured server has the requested name.
    #[error("unknown MCP server `{0}`")]
    UnknownServer(String),
    /// OAuth login was requested for a stdio server.
    #[error("MCP server `{0}` does not use Streamable HTTP")]
    NotHttp(String),
    /// OAuth login cannot replace an explicit bearer token.
    #[error("MCP server `{0}` has explicit bearer authentication")]
    ExplicitBearer(String),
    /// OAuth login requires caller-owned credential persistence.
    #[error("no MCP OAuth credential store is configured")]
    NoOAuthStore,
    /// OAuth discovery, authorization, callback, or persistence failed.
    #[error("MCP OAuth login failed: {0}")]
    OAuth(String),
    /// A server replacement connection failed.
    #[error("MCP server `{server}` failed to reload: {error}")]
    Reload {
        /// Configured server name.
        server: String,
        /// Complete connection or discovery error.
        error: String,
    },
    /// The spawned login task stopped before returning its result.
    #[error("MCP OAuth login task stopped: {0}")]
    LoginTask(String),
}

/// Invalid MCP provider configuration.
#[derive(Debug, thiserror::Error)]
pub enum McpBuildError {
    /// No servers were configured.
    #[error("at least one MCP server is required")]
    Empty,
    /// A configured server name was empty.
    #[error("MCP server name must not be empty")]
    EmptyName,
    /// The same server name was added more than once.
    #[error("MCP server `{0}` is configured more than once")]
    DuplicateServer(String),
    /// A required transport field was empty.
    #[error("MCP server `{server}` has an empty {field}")]
    EmptyField {
        /// Configured server name.
        server: String,
        /// Invalid field name.
        field: &'static str,
    },
    /// A lifecycle timeout was zero.
    #[error("MCP server `{server}` has a zero {field}")]
    ZeroTimeout {
        /// Configured server name.
        server: String,
        /// Invalid timeout field.
        field: &'static str,
    },
    /// An option was applied to the wrong transport kind.
    #[error("MCP server `{server}` does not support option `{option}` for its transport")]
    UnsupportedOption {
        /// Configured server name.
        server: String,
        /// Unsupported builder option.
        option: &'static str,
    },
}

impl Mcp {
    /// Starts an empty MCP provider builder.
    #[must_use]
    pub fn builder() -> McpBuilder {
        McpBuilder::default()
    }

    /// Returns a cheap cloneable handle for reload and OAuth operations.
    #[must_use]
    pub fn handle(&self) -> McpHandle {
        McpHandle {
            servers: Arc::clone(&self.servers),
            state: Arc::clone(&self.state),
            oauth_store: self.oauth_store.clone(),
            oauth_metadata: Arc::clone(&self.oauth_metadata),
        }
    }

    pub(crate) async fn prepared_snapshot(
        &self,
        max_timeout: Duration,
    ) -> Result<Vec<PreparedMcpTool>, String> {
        DynamicToolProvider::start(self);
        self.state.prepared_entries().await.map(|entries| {
            entries
                .into_iter()
                .map(|entry| PreparedMcpTool {
                    timeout: entry.timeout.min(max_timeout),
                    entry,
                })
                .collect()
        })
    }
}

impl PreparedMcpTool {
    pub(crate) fn provider(&self) -> &str {
        self.entry.attached_provider()
    }

    pub(crate) fn remote_name(&self) -> &str {
        &self.entry.remote_name
    }

    pub(crate) fn definition(&self) -> &ToolDefinition {
        &self.entry.definition
    }

    pub(crate) fn supports_parallel_tool_calls(&self) -> bool {
        self.entry.supports_parallel_tool_calls
    }

    pub(crate) const fn timeout(&self) -> Duration {
        self.timeout
    }

    pub(crate) async fn execute(&self, input: Value, context: ToolContext<'_>) -> ToolOutput {
        execute_mcp_entry(
            &self.entry,
            input,
            self.timeout,
            context.output_token_budget(),
        )
        .await
    }
}

impl McpBuilder {
    /// Installs caller-owned persistence for OAuth-capable Streamable HTTP servers.
    #[must_use]
    pub fn oauth_store(mut self, store: Arc<dyn McpOAuthStore>) -> Self {
        self.oauth_store = Some(store);
        self
    }

    /// Adds a named stdio or Streamable HTTP MCP server.
    #[must_use]
    pub fn server(mut self, name: impl Into<String>, server: McpServer) -> Self {
        let name = name.into();
        match self.servers.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(server);
            }
            Entry::Occupied(entry) => {
                self.duplicate.get_or_insert_with(|| entry.key().clone());
            }
        }
        self
    }

    /// Validates configuration without connecting; handshakes begin with the agent driver.
    ///
    /// # Errors
    ///
    /// Returns an error when no servers are configured, a name is empty or
    /// duplicated, a required transport field is empty, or a timeout is zero.
    pub fn build(self) -> Result<Mcp, McpBuildError> {
        if self.servers.is_empty() {
            return Err(McpBuildError::Empty);
        }
        if let Some(name) = self.duplicate {
            return Err(McpBuildError::DuplicateServer(name));
        }
        let mut discovery_timeout = Duration::ZERO;
        let mut named = Vec::with_capacity(self.servers.len());
        let model_namespaces =
            catalog::normalized_server_names(self.servers.keys().map(String::as_str));
        for (name, server) in self.servers {
            validate_server(&name, &server)?;
            discovery_timeout = discovery_timeout.max(server.startup_timeout.saturating_mul(2));
            let model_namespace = model_namespaces
                .get(&name)
                .cloned()
                .ok_or(McpBuildError::EmptyName)?;
            named.push(NamedServer {
                name,
                model_namespace,
                config: server,
            });
        }
        let servers: Arc<[NamedServer]> = named.into();
        let state = Arc::new(ProviderState::new(
            servers.iter().map(|server| server.name.clone()),
            discovery_timeout,
        ));
        let search = Arc::new(McpSearch {
            state: Arc::clone(&state),
            description: search_description(&servers),
        });
        Ok(Mcp {
            servers,
            state,
            search,
            oauth_store: self.oauth_store,
            oauth_metadata: Arc::new(oauth::OAuthMetadataCache::default()),
            started: AtomicBool::new(false),
            startup_tasks: std::sync::Mutex::new(Vec::new()),
        })
    }
}

impl Mcp {
    /// Revokes catalog authority and awaits cleanup of every owned MCP service.
    ///
    /// This is the explicit lifecycle boundary: no discovered tool or resource
    /// client remains callable once shutdown starts, and RMCP transport tasks
    /// are cancelled and joined before this method returns.
    pub async fn shutdown(&self) {
        let clients = self.state.revoke_all();
        let mut tasks = {
            let mut tasks = self
                .startup_tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *tasks)
        };
        for task in &tasks {
            task.abort();
        }
        while let Some(task) = tasks.pop() {
            let _ = task.await;
        }
        for client in clients {
            client.shutdown().await;
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        // Drop cannot await, but it must still fence background startup so a
        // discarded provider cannot later publish authority into its catalog.
        if let Ok(mut tasks) = self.startup_tasks.try_lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

impl McpHandle {
    /// Reconnects one configured server and atomically replaces its discovered tools.
    ///
    /// # Errors
    ///
    /// Returns an error when the server name is unknown or the replacement connection cannot
    /// initialize and list its tools.
    pub async fn reload(&self, server_name: &str) -> Result<usize, McpControlError> {
        let span = info_span!(
            target: "nanocodex_tools",
            parent: None,
            "mcp.server_reload",
            otel.kind = "client",
            otel.status_code = tracing::field::Empty,
            mcp.server = server_name,
            status = tracing::field::Empty,
            tool.count = tracing::field::Empty,
        );
        // Do not enter this span while connecting. RMCP's transport task inherits the current
        // tracing context and outlives the reload operation; children use explicit parents.
        let result = self.reload_inner(server_name, &span).await;
        span.record(
            "status",
            if result.is_ok() {
                "completed"
            } else {
                "failed"
            },
        );
        span.record(
            "otel.status_code",
            if result.is_ok() { "OK" } else { "ERROR" },
        );
        if let Ok(tool_count) = &result {
            span.record("tool.count", tool_count);
        }
        result
    }

    async fn reload_inner(
        &self,
        server_name: &str,
        parent: &tracing::Span,
    ) -> Result<usize, McpControlError> {
        let server = self
            .servers
            .iter()
            .find(|server| server.name == server_name)
            .ok_or_else(|| McpControlError::UnknownServer(server_name.to_owned()))?;
        let (generation, retired) = self.state.begin_server(server_name);
        for client in retired {
            client.shutdown().await;
        }
        let result = client::connect(
            server_name,
            &server.config,
            self.oauth_store.clone(),
            Arc::clone(&self.oauth_metadata),
            parent,
        )
        .await;
        match result {
            Ok(connected) => {
                let count = connected.tools.len();
                let entries = ToolEntry::new_many(
                    server_name,
                    &server.model_namespace,
                    connected.tools,
                    Arc::clone(&connected.client),
                    &server.config,
                );
                let retired = self.state.complete_server(
                    server_name,
                    generation,
                    Ok(ConnectedCatalog {
                        client: connected.client,
                        entries,
                    }),
                );
                for client in retired {
                    client.shutdown().await;
                }
                Ok(count)
            }
            Err(error) => {
                let retired =
                    self.state
                        .complete_server(server_name, generation, Err(error.clone()));
                for client in retired {
                    client.shutdown().await;
                }
                Err(McpControlError::Reload {
                    server: server_name.to_owned(),
                    error,
                })
            }
        }
    }

    /// Starts an OAuth browser login for one server.
    ///
    /// Await [`McpLogin::wait`] after opening [`McpLogin::authorization_url`]. A successful login
    /// persists the credentials and hot-reloads the server before completing.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown or unsupported server, a missing OAuth store, or when the
    /// authorization flow cannot be initialized.
    pub async fn login(&self, server_name: &str) -> Result<McpLogin, McpControlError> {
        let span = info_span!(
            target: "nanocodex_tools",
            parent: None,
            "mcp.oauth.login",
            otel.kind = "client",
            otel.status_code = tracing::field::Empty,
            mcp.server = server_name,
            status = tracing::field::Empty,
            tool.count = tracing::field::Empty,
        );
        self.login_inner(server_name, span).await
    }

    async fn login_inner(
        &self,
        server_name: &str,
        span: tracing::Span,
    ) -> Result<McpLogin, McpControlError> {
        let server = self
            .servers
            .iter()
            .find(|server| server.name == server_name)
            .ok_or_else(|| McpControlError::UnknownServer(server_name.to_owned()))?;
        let store = self
            .oauth_store
            .clone()
            .ok_or(McpControlError::NoOAuthStore)?;
        let (url, bearer, headers) = match &server.config.transport {
            config::McpTransport::StreamableHttp {
                url,
                bearer,
                headers,
            } => (url.clone(), bearer.is_some(), headers.clone()),
            config::McpTransport::Stdio { .. } => {
                return Err(McpControlError::NotHttp(server_name.to_owned()));
            }
        };
        if bearer {
            return Err(McpControlError::ExplicitBearer(server_name.to_owned()));
        }
        let flow = oauth::begin_login(server_name.to_owned(), url, headers, store)
            .instrument(span.clone())
            .await
            .map_err(|error| {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                McpControlError::OAuth(error)
            })?;
        let handle = self.clone();
        let name = server_name.to_owned();
        let completion_span = span.clone();
        let completion = tokio::spawn(
            async move {
                let result = async {
                    flow.completion
                        .await
                        .map_err(|error| McpControlError::LoginTask(error.to_string()))?
                        .map_err(McpControlError::OAuth)?;
                    handle.reload(&name).await
                }
                .await;
                completion_span.record(
                    "status",
                    if result.is_ok() {
                        "completed"
                    } else {
                        "failed"
                    },
                );
                completion_span.record(
                    "otel.status_code",
                    if result.is_ok() { "OK" } else { "ERROR" },
                );
                if let Ok(tool_count) = &result {
                    completion_span.record("tool.count", tool_count);
                }
                result
            }
            .instrument(span),
        );
        Ok(McpLogin {
            authorization_url: flow.authorization_url,
            completion: Some(completion),
        })
    }
}

impl McpLogin {
    /// Returns the URL the embedding application should open in a browser.
    #[must_use]
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// Waits for the browser callback and the automatic server reload.
    ///
    /// # Errors
    ///
    /// Returns an error when authorization, credential persistence, or the subsequent hot reload
    /// fails.
    pub async fn wait(mut self) -> Result<usize, McpControlError> {
        self.completion
            .take()
            .ok_or_else(|| {
                McpControlError::LoginTask("login completion was already consumed".to_owned())
            })?
            .await
            .map_err(|error| McpControlError::LoginTask(error.to_string()))?
    }
}

impl Drop for McpLogin {
    fn drop(&mut self) {
        // The browser callback future owns a local listener and credential
        // persistence path. A caller that abandons login must also abandon
        // that authority, rather than leaking an unobserved OAuth task.
        if let Some(completion) = &self.completion {
            completion.abort();
        }
    }
}

#[async_trait]
impl DynamicToolProvider for Mcp {
    fn start(&self) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        for server in &*self.servers {
            let name = server.name.clone();
            let model_namespace = server.model_namespace.clone();
            let config = server.config.clone();
            let state = Arc::clone(&self.state);
            let oauth_store = self.oauth_store.clone();
            let oauth_metadata = Arc::clone(&self.oauth_metadata);
            let span = info_span!(
                target: "nanocodex_tools",
                parent: None,
                "mcp.server_start",
                otel.kind = "client",
                otel.status_code = tracing::field::Empty,
                mcp.server = %name,
                status = tracing::field::Empty,
                tool.count = tracing::field::Empty,
            );
            let task = tokio::spawn(async move {
                let result = client::connect(&name, &config, oauth_store, oauth_metadata, &span)
                    .await
                    .map(|connected| {
                        let entries = ToolEntry::new_many(
                            &name,
                            &model_namespace,
                            connected.tools,
                            Arc::clone(&connected.client),
                            &config,
                        );
                        ConnectedCatalog {
                            client: connected.client,
                            entries,
                        }
                    });
                span.record(
                    "status",
                    if result.is_ok() {
                        "completed"
                    } else {
                        "failed"
                    },
                );
                span.record(
                    "otel.status_code",
                    if result.is_ok() { "OK" } else { "ERROR" },
                );
                if let Ok(catalog) = &result {
                    span.record("tool.count", catalog.entries.len());
                }
                for client in state.complete_server(&name, 0, result) {
                    client.shutdown().await;
                }
            });
            // Startup is provider-owned work. Retaining the task makes both
            // explicit shutdown and Drop cancellation deterministic.
            if let Ok(mut tasks) = self.startup_tasks.try_lock() {
                tasks.retain(|task| !task.is_finished());
                tasks.push(task);
            } else {
                task.abort();
            }
        }
    }

    fn direct_tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = vec![Arc::clone(&self.search) as Arc<dyn Tool>];
        for server in &*self.servers {
            for (suffix, operation) in [
                ("resources_list", McpResourceOperation::List),
                (
                    "resource_templates_list",
                    McpResourceOperation::ListTemplates,
                ),
                ("resource_read", McpResourceOperation::Read),
            ] {
                tools.push(Arc::new(McpResourceTool {
                    name: format!("{}{}", server.model_namespace, suffix),
                    server_name: server.name.clone(),
                    state: Arc::clone(&self.state),
                    timeout: server.config.tool_timeout,
                    operation,
                }));
            }
        }
        tools
    }

    fn direct_tools_for_exposure(&self, exposure: crate::ToolExposure) -> Vec<Arc<dyn Tool>> {
        // Preserve Codex's compact Code Mode-only prefix: MCP server tools
        // are deferred there, while resources become explicit model tools
        // when the embedding elects direct exposure.
        if exposure == crate::ToolExposure::CodeModeOnly {
            vec![Arc::clone(&self.search) as Arc<dyn Tool>]
        } else {
            self.direct_tools()
        }
    }

    fn available_definitions(&self) -> Vec<ToolDefinition> {
        self.state.available_definitions()
    }

    fn contains(&self, name: &str) -> bool {
        self.state
            .entry(name)
            .is_some_and(|entry| entry.tool_exposure.is_callable())
    }

    fn supports_parallel_tool_calls(&self, name: &str) -> bool {
        self.state.entry(name).is_some_and(|entry| {
            entry.tool_exposure.is_callable() && entry.supports_parallel_tool_calls
        })
    }

    async fn execute(
        &self,
        name: &str,
        input: Value,
        context: ToolContext<'_>,
    ) -> Option<ToolOutput> {
        let entry = self.state.ready_entry(name).await?;
        if !entry.tool_exposure.is_callable() {
            return None;
        }
        Some(execute_mcp_entry(&entry, input, entry.timeout, context.output_token_budget()).await)
    }
}

#[async_trait]
impl Tool for McpResourceTool {
    fn definition(&self) -> ToolDefinition {
        let (description, parameters) = match self.operation {
            McpResourceOperation::List => (
                format!(
                    "Lists resources exposed by MCP server {}.",
                    self.server_name
                ),
                json!({"type":"object", "properties":{}, "additionalProperties":false}),
            ),
            McpResourceOperation::ListTemplates => (
                format!(
                    "Lists resource templates exposed by MCP server {}.",
                    self.server_name
                ),
                json!({"type":"object", "properties":{}, "additionalProperties":false}),
            ),
            McpResourceOperation::Read => (
                format!("Reads one resource from MCP server {}.", self.server_name),
                json!({"type":"object", "properties":{"uri":{"type":"string"}}, "required":["uri"], "additionalProperties":false}),
            ),
        };
        ToolDefinition::function(self.name.clone(), description, parameters)
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let input: Value = input.decode_json()?;
        let client = self
            .state
            .ready_client(&self.server_name)
            .await
            .ok_or_else(|| {
                std::io::Error::other(format!("MCP server {} is not ready", self.server_name))
            })?;
        let call = async {
            if !client.is_current() {
                return Err(
                    "MCP resource catalog revision was revoked before the call was sent".to_owned(),
                );
            }
            client.client().refresh_oauth().await?;
            if !client.is_current() {
                return Err(
                    "MCP resource catalog revision was revoked before the call was sent".to_owned(),
                );
            }
            match self.operation {
                McpResourceOperation::List => client.client().list_resources().await,
                McpResourceOperation::ListTemplates => {
                    client.client().list_resource_templates().await
                }
                McpResourceOperation::Read => {
                    let uri = input
                        .get("uri")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "MCP resource_read requires a string uri".to_owned())?;
                    client.client().read_resource(uri.to_owned()).await
                }
            }
        };
        match tokio::time::timeout(self.timeout, call).await {
            Ok(Ok(result)) => Ok(ToolOutput::from_json(result, true).with_metadata(json!({
                "mcp_server": self.server_name,
                "mcp_resource_operation": self.name,
            }))),
            Ok(Err(error)) => Ok(ToolOutput::error(format!(
                "MCP resource operation failed: {error}"
            ))),
            Err(_) => Ok(ToolOutput::error(format!(
                "MCP resource operation exceeded {:.1} seconds",
                self.timeout.as_secs_f64()
            ))),
        }
    }
}

async fn execute_mcp_entry(
    entry: &ToolEntry,
    input: Value,
    timeout: Duration,
    context_budget: usize,
) -> ToolOutput {
    if !entry.is_current() {
        return ToolOutput::error(format!(
            "MCP tool {}/{} is no longer available; refresh the catalog before retrying",
            entry.server_name, entry.remote_name
        ));
    }
    let Value::Object(arguments) = input else {
        return ToolOutput::error(format!(
            "MCP tool {} requires an object argument",
            entry.canonical_name
        ));
    };
    let argument_bytes = serde_json::to_vec(&arguments).map_or(0, |encoded| encoded.len());
    let argument_keys = arguments
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");
    let argument_count = arguments.len();
    let params = CallToolRequestParams::new(entry.remote_name.clone()).with_arguments(arguments);
    let span = info_span!(
        target: "nanocodex_tools",
        "mcp.tool_call",
        otel.kind = "client",
        otel.status_code = tracing::field::Empty,
        mcp.server = %entry.server_name,
        mcp.tool = %entry.remote_name,
        mcp.arguments.bytes = argument_bytes,
        mcp.arguments.keys = argument_keys,
        mcp.arguments.count = argument_count,
        status = tracing::field::Empty,
    );
    let call = async {
        entry.client.refresh_oauth().await?;
        if !entry.is_current() {
            return Err("MCP catalog revision was revoked before the call was sent".to_owned());
        }
        entry.client.call_tool(params).await
    };
    let result = match tokio::time::timeout(timeout, call.instrument(span.clone())).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            return ToolOutput::error(format!(
                "MCP tool {}/{} failed: {error}",
                entry.server_name, entry.remote_name
            ));
        }
        Err(_) => {
            span.record("status", "timeout");
            span.record("otel.status_code", "ERROR");
            return ToolOutput::error(format!(
                "MCP tool {}/{} exceeded {:.1} seconds",
                entry.server_name,
                entry.remote_name,
                timeout.as_secs_f64()
            ));
        }
    };
    let success = !result.is_error.unwrap_or(false);
    span.record("status", if success { "completed" } else { "failed" });
    span.record("otel.status_code", if success { "OK" } else { "ERROR" });
    match tool_output_from_mcp_result(result, &entry.server_name, &entry.remote_name) {
        Ok(output) => output.bounded_for_model(entry.output_token_budget(context_budget)),
        Err(error) => {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            ToolOutput::error(format!("failed to encode MCP tool result: {error}"))
        }
    }
}

fn tool_output_from_mcp_result(
    result: CallToolResult,
    server_name: &str,
    remote_name: &str,
) -> Result<ToolOutput, serde_json::Error> {
    let success = !result.is_error.unwrap_or(false);
    let value = serde_json::to_value(&result)?;
    let result_metadata = result.meta.as_ref().map(|metadata| &metadata.0);
    let direct_content = direct_mcp_content(&result.content)?;
    let contains_encrypted_content = direct_content.as_ref().is_some_and(|content| {
        content
            .iter()
            .any(|item| matches!(item, ToolOutputContent::EncryptedContent { .. }))
    });
    let mut output = if contains_encrypted_content {
        let mut output = ToolOutput::content(direct_content.unwrap_or_default());
        output.success = success;
        output.with_structured_result(value)
    } else if let Some(structured_content) = result
        .structured_content
        .as_ref()
        .filter(|content| !content.is_null())
    {
        ToolOutput::from_json(structured_content.clone(), success).with_structured_result(value)
    } else if let Some(content) = direct_content {
        let mut output = ToolOutput::content(content);
        output.success = success;
        output.with_structured_result(value)
    } else {
        ToolOutput::from_json(serde_json::to_value(&result.content)?, success)
            .with_structured_result(value)
    };
    output = output.with_metadata(json!({
        "mcp_server": server_name,
        "mcp_tool": remote_name,
        "mcp_result_meta": result_metadata,
    }));
    Ok(output)
}

fn direct_mcp_content(
    content: &[ContentBlock],
) -> Result<Option<Vec<ToolOutputContent>>, serde_json::Error> {
    let mut has_typed_content = false;
    let mut output = Vec::with_capacity(content.len());
    for item in content {
        let item = match item {
            ContentBlock::Text(text)
                if content_meta_bool(&text.meta, CODEX_ENCRYPTED_CONTENT_META_KEY) =>
            {
                has_typed_content = true;
                ToolOutputContent::EncryptedContent {
                    encrypted_content: text.text.clone(),
                }
            }
            ContentBlock::Text(text) => ToolOutputContent::InputText {
                text: text.text.clone(),
            },
            ContentBlock::Image(image) => {
                has_typed_content = true;
                ToolOutputContent::InputImage {
                    image_url: media_data_url(&image.data, &image.mime_type),
                    detail: image_detail(&image.meta),
                }
            }
            ContentBlock::Audio(audio) => {
                has_typed_content = true;
                ToolOutputContent::InputAudio {
                    audio_url: media_data_url(&audio.data, &audio.mime_type),
                }
            }
            _ => ToolOutputContent::InputText {
                text: serde_json::to_string(item)?,
            },
        };
        output.push(item);
    }
    Ok(has_typed_content.then_some(output))
}

fn content_meta_bool(meta: &Option<rmcp::model::MetaObject>, key: &str) -> bool {
    meta.as_ref()
        .and_then(|meta| meta.0.get(key))
        .and_then(Value::as_bool)
        == Some(true)
}

fn media_data_url(data: &str, mime_type: &str) -> String {
    if data.starts_with("data:") {
        data.to_owned()
    } else {
        format!("data:{mime_type};base64,{data}")
    }
}

fn image_detail(meta: &Option<rmcp::model::MetaObject>) -> nanocodex_oai_api::ImageDetail {
    match meta
        .as_ref()
        .and_then(|meta| meta.0.get(CODEX_IMAGE_DETAIL_META_KEY))
        .and_then(Value::as_str)
    {
        Some("low") => nanocodex_oai_api::ImageDetail::Low,
        Some("high") => nanocodex_oai_api::ImageDetail::High,
        Some("original") => nanocodex_oai_api::ImageDetail::Original,
        Some("auto") | Some(_) | None => nanocodex_oai_api::ImageDetail::Auto,
    }
}

struct McpSearch {
    state: Arc<ProviderState>,
    description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for McpSearch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::tool_search(
            "client",
            self.description.clone(),
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for deferred tools."
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of tools to return. Defaults to 8."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        )
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let input = input.decode_json::<SearchInput>()?;
        let span = info_span!(
            target: "nanocodex_tools",
            "mcp.catalog_search",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            query.bytes = input.query.len(),
            result.count = tracing::field::Empty,
            pending_servers = tracing::field::Empty,
            status = tracing::field::Empty,
        );
        tracing::info!(
            target: "nanocodex_tools",
            parent: &span,
            query = %input.query,
            limit = input.limit,
            "MCP catalog search"
        );
        let result = self
            .state
            .search(&input.query, input.limit)
            .instrument(span.clone())
            .await;
        span.record(
            "status",
            if result.is_ok() {
                "completed"
            } else {
                "failed"
            },
        );
        span.record(
            "otel.status_code",
            if result.is_ok() { "OK" } else { "ERROR" },
        );
        if let Ok(result) = &result {
            span.record("result.count", result.tool_count());
            span.record("pending_servers", result.pending_server_count());
        }
        Ok(match result {
            Ok(result) => match result.loadable_tools() {
                Ok(tools) => ToolOutput::json(&result).with_structured_result(tools),
                Err(error) => {
                    ToolOutput::error(format!("failed to encode MCP tool definitions: {error}"))
                }
            },
            Err(error) => ToolOutput::error(error),
        })
    }
}

fn validate_server(name: &str, server: &McpServer) -> Result<(), McpBuildError> {
    if name.trim().is_empty() {
        return Err(McpBuildError::EmptyName);
    }
    if let Some(option) = server.unsupported_option {
        return Err(McpBuildError::UnsupportedOption {
            server: name.to_owned(),
            option,
        });
    }
    let (field, value) = match &server.transport {
        config::McpTransport::Stdio { command, .. } => ("command", command.as_str()),
        config::McpTransport::StreamableHttp { url, .. } => ("URL", url.as_str()),
    };
    if value.trim().is_empty() {
        return Err(McpBuildError::EmptyField {
            server: name.to_owned(),
            field,
        });
    }
    for (field, timeout) in [
        ("startup timeout", server.startup_timeout),
        ("tool timeout", server.tool_timeout),
    ] {
        if timeout.is_zero() {
            return Err(McpBuildError::ZeroTimeout {
                server: name.to_owned(),
                field,
            });
        }
    }
    Ok(())
}

fn search_description(servers: &[NamedServer]) -> String {
    let reserved_name_bytes = servers
        .iter()
        .fold(servers.len().saturating_sub(1), |reserved, server| {
            reserved.saturating_add(2).saturating_add(server.name.len())
        });
    let mut description_budget =
        MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES.saturating_sub(reserved_name_bytes);
    let mut sources = String::new();
    for server in servers {
        let separator_bytes = usize::from(!sources.is_empty());
        let required = separator_bytes
            .saturating_add(2)
            .saturating_add(server.name.len());
        if required > MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES.saturating_sub(sources.len()) {
            continue;
        }
        if !sources.is_empty() {
            sources.push('\n');
        }
        sources.push_str("- ");
        sources.push_str(&server.name);
        if let Some(description) = server.config.description.as_deref()
            && description_budget >= 2
        {
            sources.push_str(": ");
            description_budget -= 2;
            let description = take_bytes_at_char_boundary(description.trim(), description_budget);
            sources.push_str(description);
            description_budget -= description.len();
        }
    }
    format!(
        "# Tool discovery\n\nSearches over deferred tool metadata with BM25 and exposes matching tools for the next model call.\n\nYou have access to tools from the following sources:\n{sources}\nSome of the tools may not have been provided to you upfront. For MCP tool discovery, always use `tool_search`. Each search result reports whether it supports parallel tool calls. After discovery, invoke two or more independent supported tools together from one `exec` cell with `Promise.all` instead of using serial model rounds."
    )
}

fn take_bytes_at_char_boundary(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::runtime::ToolRuntime;
    use crate::{ToolOutputBody, Tools, contract::DEFAULT_TOOL_OUTPUT_TOKENS};
    use futures_util::future::join_all;
    use nanocodex_oai_api::MODEL;
    use rmcp::model::ContentBlock;
    use serde_json::value::to_raw_value;

    fn test_context(session_id: &'static str, call_id: &'static str) -> ToolContext<'static> {
        ToolContext::new(MODEL, session_id, call_id, &[], DEFAULT_TOOL_OUTPUT_TOKENS)
    }

    fn output_contains_text(output: &ToolOutputBody, expected: &str) -> bool {
        match output {
            ToolOutputBody::Text(text) => text.contains(expected),
            ToolOutputBody::Content(content) => content.iter().any(|content| {
                matches!(content, ToolOutputContent::InputText { text } if text.contains(expected))
            }),
        }
    }

    #[test]
    fn exposes_named_resource_operations_for_each_mcp_server() {
        let mcp = Mcp::builder()
            .server("fixtures", McpServer::stdio("node"))
            .build()
            .unwrap();
        let names = mcp
            .direct_tools()
            .into_iter()
            .map(|tool| tool.definition().name().to_owned())
            .collect::<Vec<_>>();
        assert!(names.contains(&"mcp__fixtures__resources_list".to_owned()));
        assert!(names.contains(&"mcp__fixtures__resource_templates_list".to_owned()));
        assert!(names.contains(&"mcp__fixtures__resource_read".to_owned()));
    }

    #[test]
    fn mcp_structured_content_precedes_unencrypted_content_for_model_input() {
        let mut result = CallToolResult::success(vec![
            ContentBlock::text("hello"),
            ContentBlock::image("aW1hZ2U=", "image/png"),
        ]);
        result.structured_content = Some(json!({"answer": 42}));
        let expected = serde_json::to_value(&result).unwrap();

        let output = tool_output_from_mcp_result(result, "fixture", "media").unwrap();
        assert!(output.success);
        assert_eq!(output.structured_result(), expected);
        assert!(matches!(output.output, ToolOutputBody::Text(text) if text == r#"{"answer":42}"#));
    }

    #[test]
    fn mcp_media_preserves_data_urls_image_detail_and_unsupported_blocks() {
        let result: CallToolResult = serde_json::from_value(json!({
            "content": [
                {"type": "text", "text": "hello"},
                {
                    "type": "image",
                    "data": "data:image/png;base64,aW1hZ2U=",
                    "mimeType": "image/png",
                    "_meta": {"codex/imageDetail": "original"}
                },
                {"type": "audio", "data": "YXVkaW8=", "mimeType": "audio/wav"},
                {
                    "type": "resource_link",
                    "uri": "file:///tmp/report.txt",
                    "name": "report",
                    "_meta": {"retained": true}
                }
            ]
        }))
        .unwrap();
        let expected = serde_json::to_value(&result).unwrap();

        let output = tool_output_from_mcp_result(result, "fixture", "media").unwrap();
        assert_eq!(output.structured_result(), expected);
        assert!(matches!(
            output.output,
            ToolOutputBody::Content(content)
                if matches!(&content[0], ToolOutputContent::InputText { text } if text == "hello")
                    && matches!(&content[1], ToolOutputContent::InputImage { image_url, detail: nanocodex_oai_api::ImageDetail::Original } if image_url == "data:image/png;base64,aW1hZ2U=")
                    && matches!(&content[2], ToolOutputContent::InputAudio { audio_url } if audio_url == "data:audio/wav;base64,YXVkaW8=")
                    && matches!(&content[3], ToolOutputContent::InputText { text } if text.contains("resource_link") && text.contains("retained"))
        ));
    }

    #[test]
    fn encrypted_mcp_text_stays_opaque_and_overrides_structured_content() {
        let result: CallToolResult = serde_json::from_value(json!({
            "content": [
                {"type": "text", "text": "Lookup completed"},
                {
                    "type": "text",
                    "text": "gAAAA-test",
                    "_meta": {"codex/encryptedContent": true}
                }
            ],
            "structuredContent": {"encrypted_output": "ignored"}
        }))
        .unwrap();
        let expected = serde_json::to_value(&result).unwrap();

        let output = tool_output_from_mcp_result(result, "fixture", "encrypted").unwrap();
        assert_eq!(output.structured_result(), expected);
        let ToolOutputBody::Content(content) = output.output else {
            panic!("encrypted MCP text must use typed function output content");
        };
        assert!(matches!(
            &content[0],
            ToolOutputContent::InputText { text } if text == "Lookup completed"
        ));
        assert!(matches!(
            &content[1],
            ToolOutputContent::EncryptedContent { encrypted_content }
                if encrypted_content == "gAAAA-test"
        ));
        assert!(!content.iter().any(|item| {
            matches!(item, ToolOutputContent::InputText { text } if text.contains("gAAAA-test"))
        }));
    }

    #[test]
    fn unsupported_mcp_content_falls_back_to_lossless_json_text_and_preserves_error() {
        let result: CallToolResult = serde_json::from_value(json!({
            "content": [{
                "type": "resource_link",
                "uri": "file:///tmp/report.txt",
                "name": "report"
            }],
            "isError": true,
            "_meta": {"trace": "retained"}
        }))
        .unwrap();
        let expected = serde_json::to_value(&result).unwrap();

        let output = tool_output_from_mcp_result(result, "fixture", "resource").unwrap();
        assert!(!output.success);
        assert_eq!(output.structured_result(), expected);
        assert!(matches!(
            output.output,
            ToolOutputBody::Text(text)
                if text == serde_json::to_string(&expected["content"]).unwrap()
        ));
        let metadata: Value = serde_json::from_str(output.metadata.unwrap().get()).unwrap();
        assert_eq!(
            metadata,
            json!({
                "mcp_server": "fixture",
                "mcp_tool": "resource",
                "mcp_result_meta": {"trace": "retained"}
            })
        );
    }

    #[derive(Default)]
    struct FixturePayment {
        lifecycle: Arc<FixturePaymentLifecycle>,
    }

    #[derive(Default)]
    struct FixturePaymentLifecycle {
        credentials: AtomicUsize,
        commits: AtomicUsize,
        rollbacks: AtomicUsize,
        abandons: AtomicUsize,
        fail_commit: AtomicBool,
    }

    #[async_trait]
    impl McpPaymentProvider for FixturePayment {
        async fn prepare(
            &self,
            payment_required: &Value,
        ) -> Result<Option<Box<dyn McpPendingPayment>>, String> {
            if payment_required.get("challenges").is_none() {
                return Ok(None);
            }
            self.lifecycle.credentials.fetch_add(1, Ordering::Relaxed);
            Ok(Some(Box::new(FixturePendingPayment {
                lifecycle: Arc::clone(&self.lifecycle),
                credential: json!("fixture-paid"),
                active: true,
            })))
        }
    }

    struct FixturePendingPayment {
        lifecycle: Arc<FixturePaymentLifecycle>,
        credential: Value,
        active: bool,
    }

    #[async_trait]
    impl McpPendingPayment for FixturePendingPayment {
        fn credential(&self) -> &Value {
            &self.credential
        }

        async fn commit(mut self: Box<Self>) -> Result<(), String> {
            self.lifecycle.commits.fetch_add(1, Ordering::Relaxed);
            if self.lifecycle.fail_commit.load(Ordering::Relaxed) {
                return Err("fixture commit failed".to_owned());
            }
            self.active = false;
            Ok(())
        }

        async fn rollback(mut self: Box<Self>) -> Result<(), String> {
            self.lifecycle.rollbacks.fetch_add(1, Ordering::Relaxed);
            self.active = false;
            Ok(())
        }
    }

    impl Drop for FixturePendingPayment {
        fn drop(&mut self) {
            if self.active {
                self.lifecycle.abandons.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[test]
    fn validates_empty_and_duplicate_servers() {
        assert!(matches!(Mcp::builder().build(), Err(McpBuildError::Empty)));
        assert!(matches!(
            Mcp::builder()
                .server("docs", McpServer::http("https://example.test/mcp"))
                .server("docs", McpServer::stdio("node"))
                .build(),
            Err(McpBuildError::DuplicateServer(name)) if name == "docs"
        ));
        assert!(matches!(
            Mcp::builder()
                .server(
                    "local",
                    McpServer::stdio("node").bearer_token("not-applicable")
                )
                .build(),
            Err(McpBuildError::UnsupportedOption {
                server,
                option: "bearer_token"
            }) if server == "local"
        ));
    }

    #[test]
    fn search_definition_uses_the_native_provider_contract() {
        let mcp = Mcp::builder()
            .server(
                "docs",
                McpServer::http("https://example.test/mcp")
                    .description("Search product documentation."),
            )
            .build()
            .unwrap();
        let definition = serde_json::to_value(mcp.search.definition()).unwrap();
        assert_eq!(
            definition,
            json!({
                "type": "tool_search",
                "execution": "client",
                "description": "# Tool discovery\n\nSearches over deferred tool metadata with BM25 and exposes matching tools for the next model call.\n\nYou have access to tools from the following sources:\n- docs: Search product documentation.\nSome of the tools may not have been provided to you upfront. For MCP tool discovery, always use `tool_search`. Each search result reports whether it supports parallel tool calls. After discovery, invoke two or more independent supported tools together from one `exec` cell with `Promise.all` instead of using serial model rounds.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query for deferred tools."
                        },
                        "limit": {
                            "type": "number",
                            "description": "Maximum number of tools to return. Defaults to 8."
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            })
        );
    }

    #[test]
    fn search_definition_bounds_aggregate_source_descriptions() {
        let mut builder = Mcp::builder();
        for index in 0..8 {
            builder = builder.server(
                format!("source-{index:02}"),
                McpServer::http("https://example.test/mcp").description("🦀".repeat(300)),
            );
        }
        let mcp = builder.build().unwrap();
        let description = mcp.search.definition().description().to_owned();
        let (_, source_section) = description
            .split_once("You have access to tools from the following sources:\n")
            .unwrap();
        let (sources, _) = source_section
            .split_once("\nSome of the tools may not have been provided")
            .unwrap();

        assert!(sources.len() <= MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES);
        assert!(sources.starts_with("- source-00: 🦀"));
        assert!(sources.contains("- source-07"));
    }

    #[tokio::test]
    async fn code_mode_only_matches_codex_mcp_exposure() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node").arg(fixture.to_string_lossy()),
            )
            .build()
            .unwrap();
        let handle = mcp.handle();
        assert_eq!(handle.reload("fixture").await.unwrap(), 1);

        let tools = Tools::builder()
            .without_defaults()
            .add(mcp)
            .build()
            .unwrap();
        let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
        let specs = runtime.model_specs("test-session");
        assert_eq!(
            specs.iter().map(ToolDefinition::name).collect::<Vec<_>>(),
            ["exec", "wait", "tool_search"],
            "Code Mode-only must retain the discovery primitive while deferring MCP tools"
        );

        let description = specs[0].description();
        assert!(description.contains("Some deferred nested tools may be omitted"));
        assert!(
            !description.contains("### `mcp__fixture__echo`"),
            "deferred MCP schemas must not inflate the stable request prefix"
        );

        assert_eq!(
            runtime
                .model_contract("test-session")
                .1
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            ["mcp__fixture__echo"],
            "discovered MCP tools must be callable through Code Mode from its first cell"
        );
    }

    #[tokio::test]
    async fn stdio_handshake_search_and_call_share_the_background_client() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node").arg(fixture.to_string_lossy()),
            )
            .build()
            .unwrap();
        mcp.start();
        let context = test_context("test-session", "search-call");
        let search = mcp
            .search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo message" })).unwrap()),
                context,
            )
            .await
            .unwrap();
        assert!(search.success);
        assert_eq!(
            search.structured_result(),
            json!([
                {
                    "type": "namespace",
                    "name": "mcp__fixture__",
                    "description": "Tools in the mcp__fixture__ namespace.",
                    "tools": [
                        {
                            "type": "function",
                            "name": "echo",
                            "description": "Echo deterministic MCP fixture message 0.",
                            "strict": false,
                            "defer_loading": true,
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "message": { "type": "string" },
                                    "delay_ms": {
                                        "type": "integer",
                                        "minimum": 0,
                                        "maximum": 1000
                                    }
                                },
                                "required": ["message"],
                                "additionalProperties": false
                            }
                        }
                    ]
                }
            ])
        );
        assert!(matches!(
            &search.output,
            ToolOutputBody::Text(output) if output.contains("mcp__fixture__echo")
        ));
        assert!(
            mcp.available_definitions()
                .iter()
                .any(|definition| definition.name() == "mcp__fixture__echo")
        );

        let execution = mcp
            .execute(
                "mcp__fixture__echo",
                json!({ "message": "hello" }),
                test_context("test-session", "tool-call"),
            )
            .await
            .unwrap();
        assert!(execution.success);
        assert!(output_contains_text(&execution.output, "fixture:hello"));
    }

    #[tokio::test]
    async fn paid_mcp_call_retries_with_metadata_and_commits_once() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let payment = Arc::new(FixturePayment::default());
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node")
                    .arg(fixture.to_string_lossy())
                    .env("NANOCODEX_MCP_FIXTURE_PAYMENT", "1")
                    .payment_provider(payment.clone()),
            )
            .build()
            .unwrap();
        mcp.start();
        let search = mcp
            .search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo" })).unwrap()),
                test_context("paid-session", "search-call"),
            )
            .await
            .unwrap();
        assert!(search.success);

        let execution = mcp
            .execute(
                "mcp__fixture__echo",
                json!({ "message": "after-payment" }),
                test_context("paid-session", "paid-call"),
            )
            .await
            .unwrap();
        assert!(execution.success);
        assert!(output_contains_text(
            &execution.output,
            "fixture:after-payment"
        ));
        assert_eq!(payment.lifecycle.credentials.load(Ordering::Relaxed), 1);
        assert_eq!(payment.lifecycle.commits.load(Ordering::Relaxed), 1);
        assert_eq!(payment.lifecycle.rollbacks.load(Ordering::Relaxed), 0);
        assert_eq!(payment.lifecycle.abandons.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn payment_required_mcp_result_retries_and_commits_once() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let payment = Arc::new(FixturePayment::default());
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node")
                    .arg(fixture.to_string_lossy())
                    .env("NANOCODEX_MCP_FIXTURE_PAYMENT", "1")
                    .env("NANOCODEX_MCP_FIXTURE_PAYMENT_RESULT", "1")
                    .payment_provider(payment.clone()),
            )
            .build()
            .unwrap();
        mcp.start();
        mcp.search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo" })).unwrap()),
                test_context("paid-result-session", "search-call"),
            )
            .await
            .unwrap();

        let execution = mcp
            .execute(
                "mcp__fixture__echo",
                json!({ "message": "after-payment-result" }),
                test_context("paid-result-session", "paid-call"),
            )
            .await
            .unwrap();

        assert!(execution.success);
        assert_eq!(payment.lifecycle.credentials.load(Ordering::Relaxed), 1);
        assert_eq!(payment.lifecycle.commits.load(Ordering::Relaxed), 1);
        assert_eq!(payment.lifecycle.rollbacks.load(Ordering::Relaxed), 0);
        assert_eq!(payment.lifecycle.abandons.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn failed_paid_mcp_commit_abandons_once() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let payment = Arc::new(FixturePayment::default());
        payment.lifecycle.fail_commit.store(true, Ordering::Relaxed);
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node")
                    .arg(fixture.to_string_lossy())
                    .env("NANOCODEX_MCP_FIXTURE_PAYMENT", "1")
                    .payment_provider(payment.clone()),
            )
            .build()
            .unwrap();
        mcp.start();
        mcp.search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo" })).unwrap()),
                test_context("failed-commit-session", "search-call"),
            )
            .await
            .unwrap();

        let execution = mcp
            .execute(
                "mcp__fixture__echo",
                json!({ "message": "failed-commit" }),
                test_context("failed-commit-session", "paid-call"),
            )
            .await
            .unwrap();
        assert!(!execution.success);
        assert_eq!(payment.lifecycle.credentials.load(Ordering::Relaxed), 1);
        assert_eq!(payment.lifecycle.commits.load(Ordering::Relaxed), 1);
        assert_eq!(payment.lifecycle.rollbacks.load(Ordering::Relaxed), 0);
        assert_eq!(payment.lifecycle.abandons.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn rejected_paid_mcp_call_rolls_back_once() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let payment = Arc::new(FixturePayment::default());
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node")
                    .arg(fixture.to_string_lossy())
                    .env("NANOCODEX_MCP_FIXTURE_PAYMENT", "1")
                    .env("NANOCODEX_MCP_FIXTURE_PAYMENT_REJECT", "1")
                    .payment_provider(payment.clone()),
            )
            .build()
            .unwrap();
        mcp.start();
        let search = mcp
            .search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo" })).unwrap()),
                test_context("rejected-paid-session", "search-call"),
            )
            .await
            .unwrap();
        assert!(search.success);

        let execution = mcp
            .execute(
                "mcp__fixture__echo",
                json!({ "message": "rejected-payment" }),
                test_context("rejected-paid-session", "paid-call"),
            )
            .await
            .unwrap();
        assert!(!execution.success);
        assert_eq!(payment.lifecycle.credentials.load(Ordering::Relaxed), 1);
        assert_eq!(payment.lifecycle.commits.load(Ordering::Relaxed), 0);
        assert_eq!(payment.lifecycle.rollbacks.load(Ordering::Relaxed), 1);
        assert_eq!(payment.lifecycle.abandons.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn cancelled_paid_mcp_call_abandons_once() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let payment = Arc::new(FixturePayment::default());
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node")
                    .arg(fixture.to_string_lossy())
                    .env("NANOCODEX_MCP_FIXTURE_PAYMENT", "1")
                    .payment_provider(payment.clone()),
            )
            .build()
            .unwrap();
        mcp.start();
        let search = mcp
            .search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo" })).unwrap()),
                test_context("cancelled-paid-session", "search-call"),
            )
            .await
            .unwrap();
        assert!(search.success);

        let execution = tokio::time::timeout(
            Duration::from_millis(50),
            mcp.execute(
                "mcp__fixture__echo",
                json!({ "message": "cancelled-payment", "delay_ms": 500 }),
                test_context("cancelled-paid-session", "paid-call"),
            ),
        )
        .await;
        assert!(execution.is_err());
        assert_eq!(payment.lifecycle.credentials.load(Ordering::Relaxed), 1);
        assert_eq!(payment.lifecycle.commits.load(Ordering::Relaxed), 0);
        assert_eq!(payment.lifecycle.rollbacks.load(Ordering::Relaxed), 0);
        assert_eq!(payment.lifecycle.abandons.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn mcp_tool_exposure_selects_deferred_and_code_mode_surfaces_per_server() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let server = || McpServer::stdio("node").arg(fixture.to_string_lossy());
        let mcp = Mcp::builder()
            .server(
                "deferred",
                server().tool_exposure(McpToolExposure::DeferredOnly),
            )
            .server(
                "nested",
                server().tool_exposure(McpToolExposure::CodeModeOnly),
            )
            .server("hidden", server().tool_exposure(McpToolExposure::Hidden))
            .build()
            .unwrap();
        mcp.start();

        let search = mcp.state.search("echo", None).await.unwrap();
        let loadable = search.loadable_tools().unwrap();
        assert_eq!(loadable.as_array().unwrap().len(), 1);
        assert_eq!(loadable[0]["name"], "mcp__deferred__");

        assert_eq!(
            mcp.available_definitions()
                .iter()
                .map(ToolDefinition::name)
                .collect::<Vec<_>>(),
            ["mcp__nested__echo"]
        );
        assert!(mcp.contains("mcp__deferred__echo"));
        assert!(mcp.contains("mcp__nested__echo"));
        assert!(!mcp.contains("mcp__hidden__echo"));

        let Err(attachment_error) = mcp.prepared_snapshot(Duration::from_secs(1)).await else {
            panic!("attachment unexpectedly flattened MCP exposure policy");
        };
        assert!(attachment_error.contains("attachment cannot preserve"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_mcp_terminates_stdio_descendants() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "nanocodex-mcp-descendant-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let marker = directory.join("survived");
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node")
                    .arg(fixture.to_string_lossy())
                    .env("NANOCODEX_MCP_DESCENDANT_MARKER", marker.to_string_lossy()),
            )
            .build()
            .unwrap();
        mcp.start();
        let search = mcp
            .search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo" })).unwrap()),
                test_context("drop-session", "drop-call"),
            )
            .await
            .unwrap();
        assert!(search.success);

        drop(mcp);
        tokio::time::sleep(Duration::from_secs(1)).await;
        let escaped = marker.exists();
        std::fs::remove_dir_all(directory).unwrap();

        assert!(!escaped, "dropping MCP left a stdio descendant running");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_revokes_catalog_and_awaits_stdio_service_cleanup() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "nanocodex-mcp-shutdown-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let marker = directory.join("survived");
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node")
                    .arg(fixture.to_string_lossy())
                    .env("NANOCODEX_MCP_DESCENDANT_MARKER", marker.to_string_lossy()),
            )
            .build()
            .unwrap();
        mcp.start();
        let search = mcp
            .search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo" })).unwrap()),
                test_context("shutdown-session", "shutdown-call"),
            )
            .await
            .unwrap();
        assert!(search.success);
        let prior = mcp.state.entry("mcp__fixture__echo").unwrap();

        mcp.shutdown().await;

        assert!(!prior.is_current());
        assert!(mcp.state.entry("mcp__fixture__echo").is_none());
        tokio::time::sleep(Duration::from_secs(1)).await;
        let escaped = marker.exists();
        std::fs::remove_dir_all(directory).unwrap();
        assert!(
            !escaped,
            "awaited MCP shutdown left a stdio descendant running"
        );
    }

    #[tokio::test]
    async fn mcp_per_tool_output_limit_tightens_the_invocation_context() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node")
                    .arg(fixture.to_string_lossy())
                    .tool_output_token_limit("echo", std::num::NonZeroUsize::new(4).unwrap()),
            )
            .build()
            .unwrap();
        mcp.start();
        let search = mcp
            .search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo" })).unwrap()),
                test_context("limit-session", "search"),
            )
            .await
            .unwrap();
        assert!(search.success);

        let output = mcp
            .execute(
                "mcp__fixture__echo",
                json!({ "message": "x".repeat(1_000) }),
                ToolContext::new(MODEL, "limit-session", "call", &[], 100),
            )
            .await
            .unwrap();
        assert!(matches!(&output.output, ToolOutputBody::Text(text) if text.len() <= 16));
    }

    #[tokio::test]
    async fn reload_replaces_a_live_server_without_restarting_or_deactivating_tools() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node").arg(fixture.to_string_lossy()),
            )
            .build()
            .unwrap();
        let handle = mcp.handle();
        mcp.start();
        let context = test_context("reload-session", "search-call");
        let search = mcp
            .search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo message" })).unwrap()),
                context,
            )
            .await
            .unwrap();
        assert!(search.success);

        assert_eq!(handle.reload("fixture").await.unwrap(), 1);
        assert!(
            mcp.available_definitions()
                .iter()
                .any(|definition| definition.name() == "mcp__fixture__echo")
        );
        let execution = mcp
            .execute(
                "mcp__fixture__echo",
                json!({ "message": "after-reload" }),
                test_context("reload-session", "tool-call"),
            )
            .await
            .unwrap();
        assert!(execution.success);
        assert!(output_contains_text(
            &execution.output,
            "fixture:after-reload"
        ));
    }

    #[tokio::test]
    async fn failed_reload_revokes_the_previous_catalog_revision() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node").arg(fixture.to_string_lossy()),
            )
            .build()
            .unwrap();
        mcp.start();
        let search = mcp
            .search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo" })).unwrap()),
                test_context("revision-session", "search"),
            )
            .await
            .unwrap();
        assert!(search.success);
        let name = "mcp__fixture__echo";
        let prior = mcp.state.entry(name).unwrap();
        let resource_client = mcp.state.ready_client("fixture").await.unwrap();

        let (revision, retired) = mcp.state.begin_server("fixture");
        let stale_call = prior
            .client
            .call_tool(
                CallToolRequestParams::new(prior.remote_name.clone())
                    .with_arguments(serde_json::Map::new()),
            )
            .await;
        assert!(
            matches!(stale_call, Err(error) if error == "MCP connection has been retired"),
            "a stale entry must fail at client admission before RMCP sees the call"
        );
        for client in retired {
            client.shutdown().await;
        }
        mcp.state.complete_server(
            "fixture",
            revision,
            Err("replacement transport rejected credentials".to_owned()),
        );

        assert!(!prior.is_current());
        assert!(
            !resource_client.is_current(),
            "resource calls must observe the same revision fence as tools"
        );
        assert!(mcp.state.entry(name).is_none());
        assert!(
            mcp.execute(
                name,
                json!({ "message": "must not call stale transport" }),
                test_context("revision-session", "call"),
            )
            .await
            .is_none()
        );
    }

    #[tokio::test]
    async fn concurrent_server_startup_and_remote_calls_are_bounded_and_reusable() {
        const SERVERS: usize = 8;
        const CALLS: usize = 256;

        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mcp-stdio-server.mjs");
        let mut builder = Mcp::builder();
        for index in 0..SERVERS {
            builder = builder.server(
                format!("fixture_{index}"),
                McpServer::stdio("node").arg(fixture.to_string_lossy()),
            );
        }
        let mcp = builder.build().unwrap();
        mcp.start();
        let context = test_context("stress-session", "stress-call");
        let search = mcp
            .search
            .execute(
                ToolInput::Function(
                    to_raw_value(&json!({ "query": "echo message", "limit": 32 })).unwrap(),
                ),
                context,
            )
            .await
            .unwrap();
        assert!(search.success);
        let names = mcp
            .available_definitions()
            .into_iter()
            .map(|definition| definition.name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), SERVERS);
        assert!(
            names
                .iter()
                .all(|name| mcp.supports_parallel_tool_calls(name)),
            "the read-only echo fixture must retain its parallel-safety hint"
        );

        let calls = (0..CALLS).map(|index| {
            mcp.execute(
                &names[index % names.len()],
                json!({ "message": index.to_string() }),
                context,
            )
        });
        let results = join_all(calls).await;
        assert!(
            results
                .into_iter()
                .all(|result| { result.is_some_and(|execution| execution.success) })
        );
    }

    #[tokio::test]
    #[ignore = "manual Streamable HTTP MCP handshake and discovery smoke"]
    async fn smoke_http_servers_from_environment() {
        let configured = std::env::var("NANOCODEX_MCP_SMOKE_SERVERS")
            .expect("set NANOCODEX_MCP_SMOKE_SERVERS to comma-separated NAME=URL entries");
        let bearers = std::env::var("NANOCODEX_MCP_SMOKE_BEARERS")
            .ok()
            .map(|configured| {
                configured
                    .split(',')
                    .map(|entry| {
                        let (name, variable) = entry
                            .split_once('=')
                            .expect("each smoke bearer must use NAME=ENV");
                        (name.to_owned(), variable.to_owned())
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let mut builder = Mcp::builder();
        for entry in configured.split(',') {
            let (name, url) = entry
                .split_once('=')
                .expect("each smoke server must use NAME=URL");
            let mut server = McpServer::http(url).startup_timeout(Duration::from_mins(2));
            if let Some(variable) = bearers.get(name) {
                server = server.bearer_token_env(variable);
            }
            builder = builder.server(name, server);
        }
        let mcp = builder.build().unwrap();
        mcp.start();
        let result = mcp
            .search
            .execute(
                ToolInput::Function(
                    to_raw_value(&json!({
                        "query": "documentation status health search list",
                        "limit": 32
                    }))
                    .unwrap(),
                ),
                test_context("http-smoke-session", "http-smoke-search"),
            )
            .await
            .unwrap();
        assert!(result.success);
        let ToolOutputBody::Text(output) = result.output else {
            panic!("expected JSON text search result");
        };
        let output: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(output["pending_servers"], 0);
        assert_eq!(output["failed_servers"], json!({}));
        let tools = output["tools"].as_array().expect("tools must be an array");
        assert!(!tools.is_empty());
        let names = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        eprintln!("HTTP MCP smoke discovered {} tools: {names:?}", tools.len());
    }
}
