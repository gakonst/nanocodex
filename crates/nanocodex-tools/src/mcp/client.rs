use std::{collections::HashMap, sync::Arc};

use http::{
    HeaderName, HeaderValue,
    header::{PROXY_AUTHORIZATION, USER_AGENT},
};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult, Tool},
    service::{RoleClient, RunningService, ServiceError},
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tracing::{Instrument, Span, info_span};

use super::config::{McpPaymentProvider, McpServer, McpTransport, SecretSource};
use super::oauth::{McpOAuthStore, OAuthMetadataCache, OAuthRuntime, transport_from_credentials};
use super::pagination::collect_paginated;
use super::stdio::McpStdioTransport;

const MCP_USER_AGENT: &str = concat!("nanocodex-mcp-client/", env!("CARGO_PKG_VERSION"));
const MCP_STDERR_CHUNK_BYTES: usize = 8 * 1024;

pub(crate) type Client = Arc<ClientInner>;

pub(crate) struct ClientInner {
    service: Arc<RunningService<RoleClient, ()>>,
    oauth: Option<Arc<OAuthRuntime>>,
    payment: Option<Arc<dyn McpPaymentProvider>>,
}

const MCP_CREDENTIAL_META_KEY: &str = "org.paymentauth/credential";
const MCP_PAYMENT_REQUIRED_CODE: i32 = -32042;
const MCP_PAYMENT_REQUIRED_META_KEY: &str = "org.paymentauth/payment-required";

impl ClientInner {
    pub(crate) async fn call_tool(
        &self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, String> {
        let parent = Span::current();
        let result = self.call_tool_with_payment(params).await;
        if let Some(oauth) = &self.oauth
            && let Err(error) = oauth.persist_if_changed(&parent).await
        {
            tracing::warn!(%error, "failed to persist refreshed MCP OAuth credentials");
        }
        result
    }

    async fn call_tool_with_payment(
        &self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, String> {
        let first = self.service.call_tool(params.clone()).await;
        let (payment_required, payment) = match (&first, &self.payment) {
            (Err(ServiceError::McpError(error)), Some(payment))
                if error.code.0 == MCP_PAYMENT_REQUIRED_CODE =>
            {
                let Some(data) = error.data.as_ref() else {
                    return first.map_err(|error| error.to_string());
                };
                (data, payment)
            }
            (Ok(result), Some(payment)) => {
                let Some(data) = payment_required_result(result) else {
                    return first.map_err(|error| error.to_string());
                };
                (data, payment)
            }
            _ => return first.map_err(|error| error.to_string()),
        };
        let Some(pending) = payment.prepare(payment_required).await? else {
            return first.map_err(|error| error.to_string());
        };

        let mut paid = params;
        paid.meta
            .get_or_insert_with(rmcp::model::RequestMetaObject::new)
            .0
            .0
            .insert(
                MCP_CREDENTIAL_META_KEY.to_owned(),
                pending.credential().clone(),
            );
        match self.service.call_tool(paid).await {
            Ok(result) if payment_required_result(&result).is_some() => {
                pending.rollback().await?;
                Ok(result)
            }
            Ok(result) => {
                pending.commit().await?;
                Ok(result)
            }
            Err(error @ ServiceError::McpError(_)) => {
                pending.rollback().await?;
                Err(error.to_string())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    async fn list_all_tools(&self, parent: &Span) -> Result<Vec<Tool>, String> {
        let service = Arc::clone(&self.service);
        let tools = collect_paginated("tools/list", move |params| {
            let service = Arc::clone(&service);
            async move {
                let result = service
                    .list_tools(params)
                    .await
                    .map_err(|error| error_chain(&error))?;
                Ok((result.tools, result.next_cursor))
            }
        })
        .await;
        if let Some(oauth) = &self.oauth
            && let Err(error) = oauth.persist_if_changed(parent).await
        {
            tracing::warn!(%error, "failed to persist refreshed MCP OAuth credentials");
        }
        tools
    }

    pub(crate) async fn refresh_oauth(&self) -> Result<(), String> {
        if let Some(oauth) = &self.oauth {
            oauth.refresh_if_needed().await?;
        }
        Ok(())
    }
}

fn payment_required_result(result: &CallToolResult) -> Option<&Value> {
    result.meta.as_ref()?.0.get(MCP_PAYMENT_REQUIRED_META_KEY)
}

pub(crate) struct ConnectedServer {
    pub client: Client,
    pub tools: Vec<Tool>,
}

struct HttpConnect<'a> {
    server_name: &'a str,
    server: &'a McpServer,
    url: &'a str,
    bearer: Option<&'a SecretSource>,
    headers: &'a std::collections::BTreeMap<String, SecretSource>,
    oauth_store: Option<Arc<dyn McpOAuthStore>>,
    oauth_metadata: Arc<OAuthMetadataCache>,
    parent: &'a Span,
}

struct StoredOAuthConnect<'a> {
    server_name: &'a str,
    server: &'a McpServer,
    url: &'a str,
    http_client: reqwest::Client,
    config: StreamableHttpClientTransportConfig,
    store: Arc<dyn McpOAuthStore>,
    metadata: Arc<OAuthMetadataCache>,
    parent: &'a Span,
}

pub(crate) async fn connect(
    server_name: &str,
    server: &McpServer,
    oauth_store: Option<Arc<dyn McpOAuthStore>>,
    oauth_metadata: Arc<OAuthMetadataCache>,
    parent: &Span,
) -> Result<ConnectedServer, String> {
    let (transport_name, auth_mode) = match &server.transport {
        McpTransport::Stdio { .. } => ("stdio", "none"),
        McpTransport::StreamableHttp { bearer, .. } if bearer.is_some() => {
            ("streamable_http", "bearer")
        }
        McpTransport::StreamableHttp { .. } if oauth_store.is_some() => {
            ("streamable_http", "oauth_store")
        }
        McpTransport::StreamableHttp { .. } => ("streamable_http", "none"),
    };
    let span = info_span!(
        target: "nanocodex_tools",
        parent: parent,
        "mcp.transport_connect",
        otel.kind = "client",
        otel.status_code = tracing::field::Empty,
        mcp.server = server_name,
        mcp.transport = transport_name,
        mcp.auth = auth_mode,
        status = tracing::field::Empty,
    );
    // Keep this operation span out of the current tracing context. RMCP's initialize call
    // creates a long-lived transport task which inherits the current context; entering this
    // span would therefore keep the complete startup/reload trace open until client shutdown.
    let result = match &server.transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            if command.trim().is_empty() {
                return Err("stdio command must not be empty".to_owned());
            }
            let mut command = tokio::process::Command::new(command);
            command.args(args).envs(env);
            if let Some(cwd) = cwd {
                command.current_dir(cwd);
            }
            let (transport, stderr) = McpStdioTransport::spawn(command)
                .map_err(|error| format!("failed to launch stdio transport: {error}"))?;
            if let Some(stderr) = stderr {
                drain_server_stderr(server_name.to_owned(), stderr);
            }
            let client = connect_transport(server, transport, &span).await?;
            finish_startup(server, client, None, &span).await
        }
        McpTransport::StreamableHttp {
            url,
            bearer,
            headers,
        } => {
            connect_http(HttpConnect {
                server_name,
                server,
                url,
                bearer: bearer.as_ref(),
                headers,
                oauth_store,
                oauth_metadata,
                parent: &span,
            })
            .await
        }
    };
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
    result
}

async fn connect_http(input: HttpConnect<'_>) -> Result<ConnectedServer, String> {
    let HttpConnect {
        server_name,
        server,
        url,
        bearer,
        headers,
        oauth_store,
        oauth_metadata,
        parent,
    } = input;
    if url.trim().is_empty() {
        return Err("Streamable HTTP URL must not be empty".to_owned());
    }
    let (resolved_headers, default_headers) = resolve_http_headers(headers)?;
    let replays_plaintext_proxy_credentials = default_headers.contains_key(PROXY_AUTHORIZATION);
    nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
    let http_client = reqwest::Client::builder()
        // Match RMCP's default: its streamed handshake responses are not always fully consumed
        // before the next request, so retaining them as idle connections can stall real peers.
        .pool_max_idle_per_host(0)
        .redirect(super::same_origin_redirect_policy(
            replays_plaintext_proxy_credentials,
        ))
        .default_headers(default_headers)
        .build()
        .map_err(|error| format!("failed to build MCP HTTP client: {error}"))?;
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_owned())
        .custom_headers(resolved_headers)
        .reinit_on_expired_session(true);
    if let Some(bearer) = bearer {
        let token = bearer.resolve()?;
        if token.trim().is_empty() {
            return Err("resolved bearer token must not be empty".to_owned());
        }
        config = config.auth_header(token);
        let transport = StreamableHttpClientTransport::with_client(http_client, config);
        let client = connect_transport(server, transport, parent).await?;
        return finish_startup(server, client, None, parent).await;
    }
    if let Some(store) = oauth_store {
        return connect_stored_oauth(StoredOAuthConnect {
            server_name,
            server,
            url,
            http_client,
            config,
            store,
            metadata: oauth_metadata,
            parent,
        })
        .await;
    }
    let transport = StreamableHttpClientTransport::with_client(http_client, config);
    let client = connect_transport(server, transport, parent).await?;
    finish_startup(server, client, None, parent).await
}

fn resolve_http_headers(
    headers: &std::collections::BTreeMap<String, SecretSource>,
) -> Result<(HashMap<HeaderName, HeaderValue>, reqwest::header::HeaderMap), String> {
    let mut resolved_headers = HashMap::with_capacity(headers.len().saturating_add(1));
    let mut default_headers =
        reqwest::header::HeaderMap::with_capacity(headers.len().saturating_add(1));
    let user_agent = HeaderValue::from_static(MCP_USER_AGENT);
    resolved_headers.insert(USER_AGENT, user_agent.clone());
    default_headers.insert(USER_AGENT, user_agent);
    for (name, source) in headers {
        let name = name
            .parse::<HeaderName>()
            .map_err(|error| format!("invalid HTTP header name `{name}`: {error}"))?;
        let value = source.resolve()?;
        let mut value = HeaderValue::from_str(&value)
            .map_err(|error| format!("invalid value for HTTP header `{name}`: {error}"))?;
        value.set_sensitive(true);
        resolved_headers.insert(name.clone(), value.clone());
        default_headers.insert(name, value);
    }
    Ok((resolved_headers, default_headers))
}

async fn connect_stored_oauth(input: StoredOAuthConnect<'_>) -> Result<ConnectedServer, String> {
    let StoredOAuthConnect {
        server_name,
        server,
        url,
        http_client,
        config,
        store,
        metadata,
        parent,
    } = input;
    let load_span = info_span!(
        target: "nanocodex_tools",
        parent: parent,
        "mcp.oauth.credentials_load",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        status = tracing::field::Empty,
        credential.found = tracing::field::Empty,
    );
    let credentials = store
        .load(server_name, url)
        .instrument(load_span.clone())
        .await;
    load_span.record(
        "status",
        if credentials.is_ok() {
            "completed"
        } else {
            "failed"
        },
    );
    load_span.record(
        "otel.status_code",
        if credentials.is_ok() { "OK" } else { "ERROR" },
    );
    if let Ok(credentials) = &credentials {
        load_span.record("credential.found", credentials.is_some());
    }
    let Some(credentials) = credentials? else {
        let transport = StreamableHttpClientTransport::with_client(http_client, config);
        let client = connect_transport(server, transport, parent).await?;
        return finish_startup(server, client, None, parent).await;
    };

    let restore_span = info_span!(
        target: "nanocodex_tools",
        parent: parent,
        "mcp.oauth.restore",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        status = tracing::field::Empty,
        metadata.cache_hit = tracing::field::Empty,
    );
    let oauth =
        transport_from_credentials(server_name, url, http_client, store, credentials, &metadata)
            .instrument(restore_span.clone())
            .await;
    restore_span.record("status", if oauth.is_ok() { "completed" } else { "failed" });
    restore_span.record(
        "otel.status_code",
        if oauth.is_ok() { "OK" } else { "ERROR" },
    );
    if let Ok(oauth) = &oauth {
        restore_span.record("metadata.cache_hit", oauth.metadata_cache_hit);
    }
    let oauth = oauth?;
    let runtime = oauth.runtime;
    runtime.refresh_if_needed().await?;
    let transport = StreamableHttpClientTransport::with_client(oauth.client, config);
    let client = connect_transport(server, transport, parent).await;
    if let Err(error) = runtime.persist_if_changed(parent).await {
        tracing::warn!(%error, "failed to persist refreshed MCP OAuth credentials");
    }
    let client = client?;
    finish_startup(server, client, Some(runtime), parent).await
}

async fn connect_transport<T, E, A>(
    server: &McpServer,
    transport: T,
    parent: &Span,
) -> Result<RunningService<RoleClient, ()>, String>
where
    T: rmcp::transport::IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let span = info_span!(
        target: "nanocodex_tools",
        parent: parent,
        "mcp.initialize",
        otel.kind = "client",
        otel.status_code = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    // Do not instrument `serve` with this span. RMCP's returned service retains the transport
    // task, which would retain this span and every parent until the client is dropped. The span's
    // own lifetime still measures the awaited initialize handshake without leaking into the
    // long-lived transport.
    let result = match tokio::time::timeout(server.startup_timeout, ().serve(transport)).await {
        Ok(Ok(client)) => Ok(client),
        Ok(Err(error)) => Err(format!("MCP initialize failed: {}", error_chain(&error))),
        Err(_) => Err(startup_timeout(server, "initialize")),
    };
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
    result
}

fn drain_server_stderr(server_name: String, stderr: tokio::process::ChildStderr) {
    drop(tokio::spawn(async move {
        let mut stderr = stderr;
        let mut chunk = [0_u8; MCP_STDERR_CHUNK_BYTES];
        let mut chunk_index = 0_u64;
        loop {
            match stderr.read(&mut chunk).await {
                Ok(0) => break,
                Ok(size) => {
                    let content = &chunk[..size];
                    if let Ok(message) = std::str::from_utf8(content) {
                        tracing::info!(
                            target: "nanocodex_tools",
                            server = %server_name,
                            chunk.index = chunk_index,
                            chunk.bytes = size,
                            message,
                            "MCP server stderr"
                        );
                    } else {
                        tracing::info!(
                            target: "nanocodex_tools",
                            server = %server_name,
                            chunk.index = chunk_index,
                            chunk.bytes = size,
                            message.bytes = ?content,
                            "MCP server stderr"
                        );
                    }
                    chunk_index = chunk_index.saturating_add(1);
                }
                Err(error) => {
                    tracing::warn!(
                        target: "nanocodex_tools",
                        server = %server_name,
                        %error,
                        "failed to read MCP server stderr"
                    );
                    break;
                }
            }
        }
    }));
}

async fn finish_startup(
    server: &McpServer,
    client: RunningService<RoleClient, ()>,
    oauth: Option<Arc<OAuthRuntime>>,
    parent: &Span,
) -> Result<ConnectedServer, String> {
    let client = Arc::new(ClientInner {
        service: Arc::new(client),
        oauth,
        payment: server.payment.clone(),
    });
    let span = info_span!(
        target: "nanocodex_tools",
        parent: parent,
        "mcp.tools_list",
        otel.kind = "client",
        otel.status_code = tracing::field::Empty,
        status = tracing::field::Empty,
        tool.count = tracing::field::Empty,
    );
    client.refresh_oauth().await?;
    let tools =
        match tokio::time::timeout(server.startup_timeout, client.list_all_tools(&span)).await {
            Ok(Ok(tools)) => Ok(tools
                .into_iter()
                .filter(|tool| server.includes_tool(tool.name.as_ref()))
                .collect::<Vec<_>>()),
            Ok(Err(error)) => Err(format!("MCP tools/list failed: {error}")),
            Err(_) => Err(startup_timeout(server, "tools/list")),
        };
    span.record("status", if tools.is_ok() { "completed" } else { "failed" });
    span.record(
        "otel.status_code",
        if tools.is_ok() { "OK" } else { "ERROR" },
    );
    if let Ok(tools) = &tools {
        span.record("tool.count", tools.len());
    }
    let tools = tools?;
    Ok(ConnectedServer { client, tools })
}

fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = error.source();
    }
    message
}

fn startup_timeout(server: &McpServer, operation: &str) -> String {
    format!(
        "MCP {operation} exceeded {:.1} seconds",
        server.startup_timeout.as_secs_f64()
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use super::*;
    use reqwest::header::USER_AGENT;
    use tokio::process::Command;
    use tracing::{Event, Subscriber};
    use tracing_subscriber::{Layer, layer::Context, prelude::*, registry::LookupSpan};

    #[derive(Clone)]
    struct StderrEventCount(Arc<AtomicUsize>);

    impl<S> Layer<S> for StderrEventCount
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            if event.metadata().target() == "nanocodex_tools" {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[test]
    fn streamable_http_headers_have_an_overridable_default_user_agent() {
        let (resolved, defaults) =
            resolve_http_headers(&std::collections::BTreeMap::new()).expect("default headers");
        let expected = concat!("nanocodex-mcp-client/", env!("CARGO_PKG_VERSION"));
        assert_eq!(
            resolved
                .get(&USER_AGENT)
                .and_then(|value| value.to_str().ok()),
            Some(expected)
        );
        assert_eq!(
            defaults
                .get(USER_AGENT)
                .and_then(|value| value.to_str().ok()),
            Some(expected)
        );

        let custom = std::collections::BTreeMap::from([(
            "user-agent".to_owned(),
            SecretSource::Value("custom-agent/9.9".to_owned()),
        )]);
        let (resolved, defaults) = resolve_http_headers(&custom).expect("custom headers");
        assert_eq!(
            resolved
                .get(&USER_AGENT)
                .and_then(|value| value.to_str().ok()),
            Some("custom-agent/9.9")
        );
        assert_eq!(
            defaults
                .get(USER_AGENT)
                .and_then(|value| value.to_str().ok()),
            Some("custom-agent/9.9")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stdio_stderr_is_emitted_before_a_long_line_reaches_eof() {
        let events = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(StderrEventCount(Arc::clone(&events)));
        let dispatch = tracing::Dispatch::new(subscriber);
        let _guard = tracing::dispatcher::set_default(&dispatch);
        let mut child = Command::new("node")
            .args([
                "-e",
                "process.stderr.write('x'.repeat(65536)); setTimeout(() => {}, 10000);",
            ])
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn stderr fixture");
        let stderr = child.stderr.take().expect("fixture stderr");

        drain_server_stderr("fixture".to_owned(), stderr);
        let observed = tokio::time::timeout(Duration::from_secs(2), async {
            while events.load(Ordering::Relaxed) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        child.kill().await.expect("kill stderr fixture");
        child.wait().await.expect("reap stderr fixture");

        assert!(
            observed.is_ok(),
            "stderr reader retained an unterminated line until EOF"
        );
    }
}
