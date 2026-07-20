//! Background-handshaken MCP tools for Nanocodex Code Mode.

mod catalog;
mod client;
mod config;

use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use catalog::{ProviderState, ToolEntry};
use nanocodex_core::ToolDefinition;
use nanocodex_tools::{DynamicToolProvider, Tool, ToolContext, ToolExecution, ToolInput};
use rmcp::model::CallToolRequestParams;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{Instrument, info_span};

pub use config::McpServer;

const TOOL_SEARCH_NAME: &str = "tool_search";

/// A configured family of MCP servers installed into [`nanocodex_tools::Tools`].
pub struct Mcp {
    servers: Arc<[NamedServer]>,
    state: Arc<ProviderState>,
    search: Arc<McpSearch>,
    started: AtomicBool,
}

struct NamedServer {
    name: String,
    config: McpServer,
}

/// Builder for an MCP provider.
#[derive(Default)]
pub struct McpBuilder {
    servers: BTreeMap<String, McpServer>,
    duplicate: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum McpBuildError {
    #[error("at least one MCP server is required")]
    Empty,
    #[error("MCP server name must not be empty")]
    EmptyName,
    #[error("MCP server `{0}` is configured more than once")]
    DuplicateServer(String),
    #[error("MCP server `{server}` has an empty {field}")]
    EmptyField { server: String, field: &'static str },
    #[error("MCP server `{server}` has a zero {field}")]
    ZeroTimeout { server: String, field: &'static str },
    #[error("MCP server `{server}` does not support option `{option}` for its transport")]
    UnsupportedOption {
        server: String,
        option: &'static str,
    },
}

impl Mcp {
    #[must_use]
    pub fn builder() -> McpBuilder {
        McpBuilder::default()
    }
}

impl McpBuilder {
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
        for (name, server) in self.servers {
            validate_server(&name, &server)?;
            discovery_timeout = discovery_timeout.max(server.startup_timeout.saturating_mul(2));
            named.push(NamedServer {
                name,
                config: server,
            });
        }
        let servers: Arc<[NamedServer]> = named.into();
        let state = Arc::new(ProviderState::new(servers.len(), discovery_timeout));
        let search = Arc::new(McpSearch {
            state: Arc::clone(&state),
            description: search_description(&servers),
        });
        Ok(Mcp {
            servers,
            state,
            search,
            started: AtomicBool::new(false),
        })
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
            let config = server.config.clone();
            let state = Arc::clone(&self.state);
            let span = info_span!(
                target: "nanocodex_mcp",
                parent: None,
                "mcp.server_start",
                otel.kind = "client",
                otel.status_code = tracing::field::Empty,
                mcp.server = %name,
                status = tracing::field::Empty,
                tool.count = tracing::field::Empty,
            );
            drop(tokio::spawn(
                async move {
                    let result = client::connect(&config).await.map(|connected| {
                        connected
                            .tools
                            .into_iter()
                            .map(|tool| {
                                ToolEntry::new(
                                    &name,
                                    &tool,
                                    Arc::clone(&connected.client),
                                    config.tool_timeout,
                                )
                            })
                            .collect::<Vec<_>>()
                    });
                    let current = tracing::Span::current();
                    current.record(
                        "status",
                        if result.is_ok() {
                            "completed"
                        } else {
                            "failed"
                        },
                    );
                    current.record(
                        "otel.status_code",
                        if result.is_ok() { "OK" } else { "ERROR" },
                    );
                    if let Ok(tools) = &result {
                        current.record("tool.count", tools.len());
                    }
                    state.complete_server(&name, result);
                }
                .instrument(span),
            ));
        }
    }

    fn direct_tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::clone(&self.search) as Arc<dyn Tool>]
    }

    fn available_definitions(&self) -> Vec<ToolDefinition> {
        self.state.available_definitions()
    }

    async fn execute(
        &self,
        name: &str,
        input: Value,
        _context: ToolContext<'_>,
    ) -> Option<ToolExecution> {
        let entry = self.state.active_entry(name)?;
        let Value::Object(arguments) = input else {
            return Some(ToolExecution::error(format!(
                "MCP tool {name} requires an object argument"
            )));
        };
        let argument_bytes = serde_json::to_vec(&arguments).map_or(0, |encoded| encoded.len());
        let argument_keys = arguments
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(",");
        let argument_count = arguments.len();
        let params =
            CallToolRequestParams::new(entry.remote_name.clone()).with_arguments(arguments);
        let span = info_span!(
            target: "nanocodex_mcp",
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
        let result = match tokio::time::timeout(
            entry.timeout,
            entry.client.call_tool(params).instrument(span.clone()),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                return Some(ToolExecution::error(format!(
                    "MCP tool {}/{} failed: {error}",
                    entry.server_name, entry.remote_name
                )));
            }
            Err(_) => {
                span.record("status", "timeout");
                span.record("otel.status_code", "ERROR");
                return Some(ToolExecution::error(format!(
                    "MCP tool {}/{} exceeded {:.1} seconds",
                    entry.server_name,
                    entry.remote_name,
                    entry.timeout.as_secs_f64()
                )));
            }
        };
        let success = !result.is_error.unwrap_or(false);
        span.record("status", if success { "completed" } else { "failed" });
        span.record("otel.status_code", if success { "OK" } else { "ERROR" });
        let value = match serde_json::to_value(result) {
            Ok(value) => value,
            Err(error) => {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                return Some(ToolExecution::error(format!(
                    "failed to encode MCP tool result: {error}"
                )));
            }
        };
        Some(
            ToolExecution::from_json(value, success).with_metadata(json!({
                "mcp_server": entry.server_name,
                "mcp_tool": entry.remote_name,
            })),
        )
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
    fn name(&self) -> &'static str {
        TOOL_SEARCH_NAME
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            TOOL_SEARCH_NAME,
            self.description.clone(),
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query for deferred MCP tools."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 32,
                        "description": "Maximum number of tools to return. Defaults to 8."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolExecution {
        let input = match input.decode_json::<SearchInput>() {
            Ok(input) => input,
            Err(error) => return ToolExecution::error(error.to_string()),
        };
        match self.state.search(&input.query, input.limit).await {
            Ok(result) => ToolExecution::json(&result),
            Err(error) => ToolExecution::error(error),
        }
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
    let sources = servers
        .iter()
        .map(|server| match server.config.description.as_deref() {
            Some(description) => format!("- {}: {}", server.name, description.trim()),
            None => format!("- {}", server.name),
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "# MCP tool discovery\n\nSearches deferred MCP tool metadata with BM25 and activates matching tools for Code Mode. MCP handshakes and tools/list run in the background when the agent starts. Search before using an MCP tool; returned names can be called as `tools[name](arguments)` in the same or a later exec cell.\n\nConfigured sources:\n{sources}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::join_all;
    use nanocodex_core::MODEL;
    use nanocodex_tools::{DEFAULT_TOOL_OUTPUT_TOKENS, ToolOutputBody};
    use serde_json::value::to_raw_value;

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
    fn search_definition_describes_background_discovery() {
        let mcp = Mcp::builder()
            .server(
                "docs",
                McpServer::http("https://example.test/mcp")
                    .description("Search product documentation."),
            )
            .build()
            .unwrap();
        assert!(
            mcp.search
                .definition()
                .description()
                .contains("tools/list run in the background")
        );
    }

    #[tokio::test]
    async fn stdio_handshake_search_and_call_share_the_background_client() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/stdio-server.mjs");
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node").arg(fixture.to_string_lossy()),
            )
            .build()
            .unwrap();
        mcp.start();
        let context = ToolContext {
            model: MODEL,
            session_id: "test-session",
            call_id: "search-call",
            history: &[],
            output_token_budget: DEFAULT_TOOL_OUTPUT_TOKENS,
        };
        let search = mcp
            .search
            .execute(
                ToolInput::Function(to_raw_value(&json!({ "query": "echo message" })).unwrap()),
                context,
            )
            .await;
        assert!(search.success);
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
                ToolContext {
                    call_id: "tool-call",
                    ..context
                },
            )
            .await
            .unwrap();
        assert!(execution.success);
        assert!(matches!(
            execution.output,
            ToolOutputBody::Text(output) if output.contains("fixture:hello")
        ));
    }

    #[tokio::test]
    async fn concurrent_server_startup_and_remote_calls_are_bounded_and_reusable() {
        const SERVERS: usize = 8;
        const CALLS: usize = 256;

        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/stdio-server.mjs");
        let mut builder = Mcp::builder();
        for index in 0..SERVERS {
            builder = builder.server(
                format!("fixture_{index}"),
                McpServer::stdio("node").arg(fixture.to_string_lossy()),
            );
        }
        let mcp = builder.build().unwrap();
        mcp.start();
        let context = ToolContext {
            model: MODEL,
            session_id: "stress-session",
            call_id: "stress-call",
            history: &[],
            output_token_budget: DEFAULT_TOOL_OUTPUT_TOKENS,
        };
        let search = mcp
            .search
            .execute(
                ToolInput::Function(
                    to_raw_value(&json!({ "query": "echo message", "limit": 32 })).unwrap(),
                ),
                context,
            )
            .await;
        assert!(search.success);
        let names = mcp
            .available_definitions()
            .into_iter()
            .map(|definition| definition.name().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), SERVERS);

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
    #[ignore = "manual repeated-search stress benchmark"]
    async fn stress_repeated_tool_search() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/stdio-server.mjs");
        let mcp = Mcp::builder()
            .server(
                "fixture",
                McpServer::stdio("node").arg(fixture.to_string_lossy()),
            )
            .build()
            .unwrap();
        mcp.start();
        let context = ToolContext {
            model: MODEL,
            session_id: "stress-session",
            call_id: "stress-search",
            history: &[],
            output_token_budget: DEFAULT_TOOL_OUTPUT_TOKENS,
        };
        let started = std::time::Instant::now();
        for _ in 0..10_000 {
            let result = mcp
                .search
                .execute(
                    ToolInput::Function(to_raw_value(&json!({ "query": "echo message" })).unwrap()),
                    context,
                )
                .await;
            assert!(result.success);
        }
        eprintln!("10k repeated searches: {:?}", started.elapsed());
    }
}
