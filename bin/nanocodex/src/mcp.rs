use std::{collections::BTreeMap, str::FromStr, time::Duration};

use clap::Args;
use eyre::{Result, bail};
use nanocodex::{Mcp, McpServer};

#[derive(Args, Default)]
pub(crate) struct McpArgs {
    /// Add a named Streamable HTTP MCP server (`NAME=URL`). Repeatable.
    #[arg(long = "mcp", global = true, value_name = "NAME=URL")]
    http: Vec<NamedValue>,

    /// Add a named stdio MCP server executable (`NAME=COMMAND`). Repeatable.
    #[arg(long = "mcp-stdio", global = true, value_name = "NAME=COMMAND")]
    stdio: Vec<NamedValue>,

    /// Append one argument to a named stdio MCP server (`NAME=ARG`). Repeatable.
    #[arg(long = "mcp-arg", global = true, value_name = "NAME=ARG")]
    arguments: Vec<NamedValue>,

    /// Resolve a named HTTP server's bearer token from an environment variable (`NAME=ENV`).
    #[arg(long = "mcp-bearer-env", global = true, value_name = "NAME=ENV")]
    bearer_env: Vec<NamedValue>,

    /// Resolve an HTTP header from an environment variable (`NAME:HEADER=ENV`). Repeatable.
    #[arg(long = "mcp-header-env", global = true, value_name = "NAME:HEADER=ENV")]
    header_env: Vec<NamedHeaderValue>,

    /// Seconds allowed for each MCP initialize and tools/list operation.
    #[arg(long, global = true, default_value_t = 30)]
    mcp_startup_timeout: u64,

    /// Seconds allowed for one remote MCP tool call.
    #[arg(long, global = true, default_value_t = 300)]
    mcp_tool_timeout: u64,
}

enum Transport {
    Http(String),
    Stdio(String),
}

struct ServerConfig {
    transport: Transport,
    arguments: Vec<String>,
    bearer_env: Option<String>,
    header_env: Vec<(String, String)>,
}

#[derive(Clone)]
struct NamedValue {
    name: String,
    value: String,
}

#[derive(Clone)]
struct NamedHeaderValue {
    name: String,
    header: String,
    value: String,
}

impl McpArgs {
    pub(crate) fn build(self) -> Result<Option<Mcp>> {
        if self.http.is_empty() && self.stdio.is_empty() {
            if self.arguments.is_empty() && self.bearer_env.is_empty() && self.header_env.is_empty()
            {
                return Ok(None);
            }
            bail!("MCP options require at least one --mcp or --mcp-stdio server");
        }
        if self.mcp_startup_timeout == 0 || self.mcp_tool_timeout == 0 {
            bail!("MCP timeouts must be greater than zero");
        }

        let mut servers = BTreeMap::new();
        for endpoint in self.http {
            insert_server(&mut servers, endpoint, Transport::Http)?;
        }
        for command in self.stdio {
            insert_server(&mut servers, command, Transport::Stdio)?;
        }
        for argument in self.arguments {
            let server = server_mut(&mut servers, &argument.name, "--mcp-arg")?;
            if !matches!(server.transport, Transport::Stdio(_)) {
                bail!("--mcp-arg requires a stdio MCP server");
            }
            server.arguments.push(argument.value);
        }
        for bearer in self.bearer_env {
            let server = server_mut(&mut servers, &bearer.name, "--mcp-bearer-env")?;
            if !matches!(server.transport, Transport::Http(_)) {
                bail!("--mcp-bearer-env requires an HTTP MCP server");
            }
            if server.bearer_env.replace(bearer.value).is_some() {
                bail!(
                    "MCP server `{}` has more than one bearer environment",
                    bearer.name
                );
            }
        }
        for header in self.header_env {
            let server = server_mut(&mut servers, &header.name, "--mcp-header-env")?;
            if !matches!(server.transport, Transport::Http(_)) {
                bail!("--mcp-header-env requires an HTTP MCP server");
            }
            server.header_env.push((header.header, header.value));
        }

        let startup_timeout = Duration::from_secs(self.mcp_startup_timeout);
        let tool_timeout = Duration::from_secs(self.mcp_tool_timeout);
        let mut builder = Mcp::builder();
        for (name, server) in servers {
            let mut configured = match server.transport {
                Transport::Http(url) => McpServer::http(url),
                Transport::Stdio(command) => McpServer::stdio(command).args(server.arguments),
            }
            .startup_timeout(startup_timeout)
            .tool_timeout(tool_timeout);
            if let Some(variable) = server.bearer_env {
                configured = configured.bearer_token_env(variable);
            }
            for (header, variable) in server.header_env {
                configured = configured.header_env(header, variable);
            }
            builder = builder.server(name, configured);
        }
        Ok(Some(builder.build()?))
    }
}

fn insert_server(
    servers: &mut BTreeMap<String, ServerConfig>,
    named: NamedValue,
    transport: impl FnOnce(String) -> Transport,
) -> Result<()> {
    let name = named.name;
    if servers
        .insert(
            name.clone(),
            ServerConfig {
                transport: transport(named.value),
                arguments: Vec::new(),
                bearer_env: None,
                header_env: Vec::new(),
            },
        )
        .is_some()
    {
        bail!("MCP server `{name}` is configured more than once");
    }
    Ok(())
}

fn server_mut<'a>(
    servers: &'a mut BTreeMap<String, ServerConfig>,
    name: &str,
    option: &str,
) -> Result<&'a mut ServerConfig> {
    servers
        .get_mut(name)
        .ok_or_else(|| eyre::eyre!("{option} references unknown MCP server `{name}`"))
}

impl FromStr for NamedValue {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, value) = value
            .split_once('=')
            .ok_or_else(|| "expected NAME=VALUE".to_owned())?;
        if name.is_empty() || value.is_empty() {
            return Err("name and value must not be empty".to_owned());
        }
        Ok(Self {
            name: name.to_owned(),
            value: value.to_owned(),
        })
    }
}

impl FromStr for NamedHeaderValue {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (server_and_header, value) = value
            .split_once('=')
            .ok_or_else(|| "expected NAME:HEADER=ENV".to_owned())?;
        let (name, header) = server_and_header
            .split_once(':')
            .ok_or_else(|| "expected NAME:HEADER=ENV".to_owned())?;
        if name.is_empty() || header.is_empty() || value.is_empty() {
            return Err("server, header, and environment variable must not be empty".to_owned());
        }
        Ok(Self {
            name: name.to_owned(),
            header: header.to_owned(),
            value: value.to_owned(),
        })
    }
}
