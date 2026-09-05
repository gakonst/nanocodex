use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use serde_json::Value;

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_mins(5);

/// Model-facing surfaces on which one MCP server's tools are available.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum McpToolExposure {
    /// Discover tools through `tool_search` and call them from Code Mode.
    #[default]
    DeferredAndCodeMode,
    /// Discover tools through `tool_search`, but omit them from Code Mode.
    DeferredOnly,
    /// Expose tools only inside Code Mode, without provider-native discovery.
    CodeModeOnly,
    /// Keep the server connected without exposing or dispatching its tools.
    Hidden,
}

impl McpToolExposure {
    pub(crate) const fn is_deferred(self) -> bool {
        matches!(self, Self::DeferredAndCodeMode | Self::DeferredOnly)
    }

    pub(crate) const fn is_available_in_code_mode(self) -> bool {
        matches!(self, Self::DeferredAndCodeMode | Self::CodeModeOnly)
    }

    pub(crate) const fn is_callable(self) -> bool {
        !matches!(self, Self::Hidden)
    }
}

/// One MCP server transport and its lifecycle limits.
#[derive(Clone)]
pub struct McpServer {
    pub(crate) transport: McpTransport,
    pub(crate) description: Option<String>,
    pub(crate) startup_timeout: Duration,
    pub(crate) tool_timeout: Duration,
    pub(crate) supports_parallel_tool_calls: bool,
    pub(crate) parallel_tools: BTreeSet<String>,
    pub(crate) tool_exposure: McpToolExposure,
    pub(crate) enabled_tools: Option<Vec<String>>,
    pub(crate) disabled_tools: Vec<String>,
    pub(crate) payment: Option<Arc<dyn McpPaymentProvider>>,
    pub(crate) unsupported_option: Option<&'static str>,
}

/// Creates and reconciles protocol-level MPP credentials for paid MCP calls.
#[async_trait]
pub trait McpPaymentProvider: Send + Sync {
    /// Prepares one payment-required payload, or returns `None` when none of
    /// its challenges are supported.
    async fn prepare(
        &self,
        payment_required: &Value,
    ) -> Result<Option<Box<dyn McpPendingPayment>>, String>;
}

/// One prepared MCP payment whose provider lifecycle follows request delivery.
#[async_trait]
pub trait McpPendingPayment: Send {
    /// Returns the credential to attach to request metadata.
    fn credential(&self) -> &Value;

    /// Commits provider state after the paid retry succeeds.
    async fn commit(self: Box<Self>) -> Result<(), String>;

    /// Rolls provider state back after the credential was not sent or was rejected.
    async fn rollback(self: Box<Self>) -> Result<(), String>;
}

#[derive(Clone)]
pub(crate) enum McpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
    },
    StreamableHttp {
        url: String,
        bearer: Option<SecretSource>,
        headers: BTreeMap<String, SecretSource>,
    },
}

#[derive(Clone)]
pub(crate) enum SecretSource {
    Value(String),
    Environment(String),
}

impl McpServer {
    /// Creates a local MCP server launched over stdio.
    #[must_use]
    pub fn stdio(command: impl Into<String>) -> Self {
        Self {
            transport: McpTransport::Stdio {
                command: command.into(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
            },
            description: None,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            supports_parallel_tool_calls: false,
            parallel_tools: BTreeSet::new(),
            tool_exposure: McpToolExposure::default(),
            enabled_tools: None,
            disabled_tools: Vec::new(),
            payment: None,
            unsupported_option: None,
        }
    }

    /// Creates a remote MCP server using the Streamable HTTP transport.
    #[must_use]
    pub fn http(url: impl Into<String>) -> Self {
        Self {
            transport: McpTransport::StreamableHttp {
                url: url.into(),
                bearer: None,
                headers: BTreeMap::new(),
            },
            description: None,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            supports_parallel_tool_calls: false,
            parallel_tools: BTreeSet::new(),
            tool_exposure: McpToolExposure::default(),
            enabled_tools: None,
            disabled_tools: Vec::new(),
            payment: None,
            unsupported_option: None,
        }
    }

    /// Sets the source description included in `tool_search` instructions.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the maximum duration for initialize and initial discovery.
    #[must_use]
    pub const fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Sets the maximum duration of one remote tool call.
    #[must_use]
    pub const fn tool_timeout(mut self, timeout: Duration) -> Self {
        self.tool_timeout = timeout;
        self
    }

    /// Declares that every tool exposed by this server is safe to call in parallel.
    ///
    /// This is disabled by default. Individual tools may still opt in through
    /// MCP's `annotations.readOnlyHint` metadata.
    #[must_use]
    pub const fn supports_parallel_tool_calls(mut self, supports: bool) -> Self {
        self.supports_parallel_tool_calls = supports;
        self
    }

    /// Declares specific remote tools safe to call concurrently.
    ///
    /// This supplements MCP's `annotations.readOnlyHint` without opting every
    /// tool on the server into parallel execution.
    #[must_use]
    pub fn parallel_tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.parallel_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    /// Selects whether this server's tools are deferred, nested in Code Mode, both, or hidden.
    #[must_use]
    pub const fn tool_exposure(mut self, exposure: McpToolExposure) -> Self {
        self.tool_exposure = exposure;
        self
    }

    /// Restricts discovery to the named remote tools.
    #[must_use]
    pub fn enabled_tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.enabled_tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }

    /// Excludes the named remote tools from discovery.
    #[must_use]
    pub fn disabled_tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.disabled_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    /// Enables protocol-level paid tool retries for this MCP server.
    #[must_use]
    pub fn payment_provider(mut self, provider: Arc<dyn McpPaymentProvider>) -> Self {
        self.payment = Some(provider);
        self
    }

    /// Adds one argument to a stdio server command.
    #[must_use]
    pub fn arg(mut self, argument: impl Into<String>) -> Self {
        match &mut self.transport {
            McpTransport::Stdio { args, .. } => args.push(argument.into()),
            McpTransport::StreamableHttp { .. } => self.unsupported_option = Some("arg"),
        }
        self
    }

    /// Adds arguments to a stdio server command.
    #[must_use]
    pub fn args(mut self, arguments: impl IntoIterator<Item = impl Into<String>>) -> Self {
        match &mut self.transport {
            McpTransport::Stdio { args, .. } => {
                args.extend(arguments.into_iter().map(Into::into));
            }
            McpTransport::StreamableHttp { .. } => self.unsupported_option = Some("args"),
        }
        self
    }

    /// Adds an explicit environment value to a stdio server process.
    #[must_use]
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        match &mut self.transport {
            McpTransport::Stdio { env, .. } => {
                env.insert(name.into(), value.into());
            }
            McpTransport::StreamableHttp { .. } => self.unsupported_option = Some("env"),
        }
        self
    }

    /// Sets the working directory for a stdio server process.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        match &mut self.transport {
            McpTransport::Stdio { cwd: current, .. } => *current = Some(cwd.into()),
            McpTransport::StreamableHttp { .. } => self.unsupported_option = Some("cwd"),
        }
        self
    }

    /// Sets a Streamable HTTP bearer token directly.
    #[must_use]
    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        match &mut self.transport {
            McpTransport::StreamableHttp { bearer, .. } => {
                *bearer = Some(SecretSource::Value(token.into()));
            }
            McpTransport::Stdio { .. } => self.unsupported_option = Some("bearer_token"),
        }
        self
    }

    /// Resolves a Streamable HTTP bearer token from an environment variable.
    #[must_use]
    pub fn bearer_token_env(mut self, variable: impl Into<String>) -> Self {
        match &mut self.transport {
            McpTransport::StreamableHttp { bearer, .. } => {
                *bearer = Some(SecretSource::Environment(variable.into()));
            }
            McpTransport::Stdio { .. } => self.unsupported_option = Some("bearer_token_env"),
        }
        self
    }

    /// Adds a fixed Streamable HTTP header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        match &mut self.transport {
            McpTransport::StreamableHttp { headers, .. } => {
                headers.insert(name.into(), SecretSource::Value(value.into()));
            }
            McpTransport::Stdio { .. } => self.unsupported_option = Some("header"),
        }
        self
    }

    /// Resolves a Streamable HTTP header value from an environment variable.
    #[must_use]
    pub fn header_env(mut self, name: impl Into<String>, variable: impl Into<String>) -> Self {
        match &mut self.transport {
            McpTransport::StreamableHttp { headers, .. } => {
                headers.insert(name.into(), SecretSource::Environment(variable.into()));
            }
            McpTransport::Stdio { .. } => self.unsupported_option = Some("header_env"),
        }
        self
    }

    pub(crate) fn includes_tool(&self, name: &str) -> bool {
        self.enabled_tools
            .as_ref()
            .is_none_or(|enabled| enabled.iter().any(|candidate| candidate == name))
            && !self
                .disabled_tools
                .iter()
                .any(|candidate| candidate == name)
    }
}

impl SecretSource {
    pub(crate) fn resolve(&self) -> Result<String, String> {
        match self {
            Self::Value(value) => Ok(value.clone()),
            Self::Environment(variable) => std::env::var(variable).map_err(|error| {
                format!("environment variable `{variable}` is unavailable: {error}")
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::McpServer;

    #[test]
    fn parallel_tool_calls_require_explicit_server_opt_in() {
        assert!(!McpServer::stdio("fixture").supports_parallel_tool_calls);
        assert!(
            McpServer::stdio("fixture")
                .supports_parallel_tool_calls(true)
                .supports_parallel_tool_calls
        );
        assert_eq!(
            McpServer::stdio("fixture")
                .parallel_tools(["lookup", "search"])
                .parallel_tools,
            BTreeSet::from(["lookup".to_owned(), "search".to_owned()])
        );
    }
}
