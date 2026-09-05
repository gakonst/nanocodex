use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    thread,
    time::Duration,
};

use async_trait::async_trait;
use clap::{ArgAction, Args};
use eyre::{Result, WrapErr, bail, eyre};
use nanocodex::tools::mcp::{
    Mcp, McpHandle, McpOAuthCredentials, McpOAuthRefreshGuard, McpOAuthStore, McpServer,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::login::{APP_ID, APP_ORIGIN, ScopedManagedCredential};

const DEFAULT_MCP_SERVERS: [(&str, &str, &str); 5] = [
    (
        "openaiDeveloperDocs",
        "https://developers.openai.com/mcp",
        "Search OpenAI developer documentation.",
    ),
    (
        "tempo",
        "https://mcp.tempo.xyz",
        "Tempo network and protocol tools.",
    ),
    (
        "cloudflare",
        "https://docs.mcp.cloudflare.com/mcp",
        "Search Cloudflare developer documentation.",
    ),
    (
        "viem",
        "https://viem.sh/api/mcp",
        "Search Viem developer documentation.",
    ),
    (
        "vocs",
        "https://vocs.dev/api/mcp",
        "Search Vocs developer documentation.",
    ),
];
pub(crate) const MERCATOR_MCP_URL: &str = "https://mercator.tempo.xyz/mcp";
const MERCATOR_MCP_DESCRIPTION: &str = "Discovers and composes paid Tempo services and MPP flows.";

fn default_parallel_tools(name: &str) -> &'static [&'static str] {
    match name {
        "openaiDeveloperDocs" => &["fetch_openai_doc", "search_openai_docs"],
        "tempo" => &["code", "search"],
        "cloudflare" => &["search_cloudflare_documentation"],
        "viem" | "vocs" => &["list_pages", "read_page", "search_docs", "search_source"],
        _ => &[],
    }
}

#[derive(Args)]
pub(crate) struct McpArgs {
    #[arg(skip)]
    disabled: bool,

    /// Load the standard docs MCPs, plus paid Mercator in Tempo provider mode.
    #[arg(
        long,
        env = "NANOCODEX_MCP_DEFAULTS",
        default_value_t = true,
        action = ArgAction::Set
    )]
    mcp_defaults: bool,

    /// Load enabled MCP servers from `$CODEX_HOME/config.toml`.
    #[arg(
        long,
        env = "NANOCODEX_MCP_CODEX_CONFIG",
        default_value_t = true,
        action = ArgAction::Set
    )]
    mcp_codex_config: bool,

    /// Add a named Streamable HTTP MCP server (`NAME=URL`). Repeatable.
    #[arg(long = "mcp", value_name = "NAME=URL")]
    http: Vec<NamedValue>,

    /// Add a named stdio MCP server executable (`NAME=COMMAND`). Repeatable.
    #[arg(long = "mcp-stdio", value_name = "NAME=COMMAND")]
    stdio: Vec<NamedValue>,

    /// Append one argument to a named stdio MCP server (`NAME=ARG`). Repeatable.
    #[arg(long = "mcp-arg", value_name = "NAME=ARG")]
    arguments: Vec<NamedValue>,

    /// Resolve a named HTTP server's bearer token from an environment variable (`NAME=ENV`).
    #[arg(long = "mcp-bearer-env", value_name = "NAME=ENV")]
    bearer_env: Vec<NamedValue>,

    /// Resolve an HTTP header from an environment variable (`NAME:HEADER=ENV`). Repeatable.
    #[arg(long = "mcp-header-env", value_name = "NAME:HEADER=ENV")]
    header_env: Vec<NamedHeaderValue>,

    /// Seconds allowed for each MCP initialize and tools/list operation.
    #[arg(long, default_value_t = 30)]
    mcp_startup_timeout: u64,

    /// Seconds allowed for one remote MCP tool call.
    #[arg(long, default_value_t = 300)]
    mcp_tool_timeout: u64,
}

enum Transport {
    Http(String),
    Stdio(String),
}

struct ServerConfig {
    transport: Transport,
    description: Option<String>,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    cwd: Option<PathBuf>,
    bearer_env: Option<String>,
    bearer: Option<String>,
    headers: BTreeMap<String, String>,
    header_env: Vec<(String, String)>,
    startup_timeout: Option<Duration>,
    tool_timeout: Option<Duration>,
    enabled_tools: Option<Vec<String>>,
    disabled_tools: Vec<String>,
    parallel_tools: Vec<String>,
}

#[derive(Default, Deserialize)]
struct CodexConfig {
    #[serde(default)]
    mcp_servers: BTreeMap<String, CodexMcpServer>,
}

pub(crate) struct ConfiguredMcp {
    pub(crate) provider: Mcp,
    pub(crate) handle: McpHandle,
}

#[derive(Clone)]
struct CodexOAuthStore {
    codex_home: PathBuf,
}

#[derive(Clone, Deserialize, Serialize)]
struct CodexOAuthEntry {
    server_name: String,
    server_url: String,
    client_id: String,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    scopes: Vec<String>,
}

type CodexOAuthFile = BTreeMap<String, CodexOAuthEntry>;

#[derive(Deserialize)]
struct CodexMcpServer {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    cwd: Option<PathBuf>,
    url: Option<String>,
    bearer_token_env_var: Option<String>,
    #[serde(default)]
    http_headers: BTreeMap<String, String>,
    #[serde(default)]
    env_http_headers: BTreeMap<String, String>,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    startup_timeout_sec: Option<f64>,
    startup_timeout_ms: Option<u64>,
    tool_timeout_sec: Option<f64>,
    enabled_tools: Option<Vec<String>>,
    #[serde(default)]
    disabled_tools: Vec<String>,
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
    pub(crate) const fn loads_managed(&self) -> bool {
        !self.disabled
    }

    pub(crate) fn disable(&mut self) {
        self.disabled = true;
        self.mcp_defaults = false;
        self.mcp_codex_config = false;
        self.http.clear();
        self.stdio.clear();
        self.arguments.clear();
        self.bearer_env.clear();
        self.header_env.clear();
    }

    pub(crate) fn build(
        self,
        codex_home: &Path,
        tempo: Option<&crate::mpp::MppAdapter>,
        managed: Option<&ScopedManagedCredential>,
    ) -> Result<Option<ConfiguredMcp>> {
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
        let mut codex_server_names = BTreeSet::new();
        let oauth_store = self
            .mcp_codex_config
            .then(|| Arc::new(CodexOAuthStore::new(codex_home.to_path_buf())));
        if self.mcp_codex_config {
            for (name, server) in load_codex_mcp_servers(codex_home)? {
                codex_server_names.insert(name.clone());
                if servers.contains_key(&name) {
                    continue;
                }
                if let Some(server) = server {
                    servers.insert(name, server);
                }
            }
        }
        if self.mcp_defaults {
            for (name, url, description) in DEFAULT_MCP_SERVERS {
                if codex_server_names.contains(name) {
                    continue;
                }
                servers
                    .entry(name.to_owned())
                    .or_insert_with(|| ServerConfig {
                        transport: Transport::Http(url.to_owned()),
                        description: Some(description.to_owned()),
                        arguments: Vec::new(),
                        environment: BTreeMap::new(),
                        cwd: None,
                        bearer_env: None,
                        bearer: None,
                        headers: BTreeMap::new(),
                        header_env: Vec::new(),
                        startup_timeout: None,
                        tool_timeout: None,
                        enabled_tools: None,
                        disabled_tools: Vec::new(),
                        parallel_tools: default_parallel_tools(name)
                            .iter()
                            .map(|tool| (*tool).to_owned())
                            .collect(),
                    });
            }
            if tempo.is_some() && !codex_server_names.contains("mercator") {
                servers
                    .entry("mercator".to_owned())
                    .or_insert_with(|| ServerConfig {
                        transport: Transport::Http(MERCATOR_MCP_URL.to_owned()),
                        description: Some(MERCATOR_MCP_DESCRIPTION.to_owned()),
                        arguments: Vec::new(),
                        environment: BTreeMap::new(),
                        cwd: None,
                        bearer_env: None,
                        bearer: None,
                        headers: BTreeMap::new(),
                        header_env: Vec::new(),
                        startup_timeout: None,
                        tool_timeout: None,
                        enabled_tools: None,
                        disabled_tools: Vec::new(),
                        parallel_tools: Vec::new(),
                    });
            }
        }
        if !self.disabled
            && let Some(managed) = managed
        {
            insert_managed_mcp_servers(&mut servers, managed)?;
        }
        if servers.is_empty() {
            if self.arguments.is_empty() && self.bearer_env.is_empty() && self.header_env.is_empty()
            {
                return Ok(None);
            }
            bail!("MCP options require at least one --mcp or --mcp-stdio server");
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
        Ok(Some(build_mcp(
            servers,
            startup_timeout,
            tool_timeout,
            oauth_store,
            tempo,
        )?))
    }
}

fn insert_managed_mcp_servers(
    servers: &mut BTreeMap<String, ServerConfig>,
    credential: &ScopedManagedCredential,
) -> Result<()> {
    for connection in credential.mcp_connections() {
        let name = unique_managed_server_name(connection.name(), connection.id(), servers);
        servers.insert(
            name,
            managed_mcp_server_config(
                credential.origin(),
                credential.grant_id(),
                credential.bearer_token(),
                connection,
            )?,
        );
    }
    Ok(())
}

fn managed_mcp_server_config(
    origin: &reqwest::Url,
    grant_id: &str,
    bearer: &str,
    connection: &crate::login::ManagedMcpConnection,
) -> Result<ServerConfig> {
    let url = origin
        .join(&format!("/v1/grants/{grant_id}/mcp/{}", connection.id()))
        .wrap_err("invalid managed MCP proxy URL")?;
    Ok(ServerConfig {
        transport: Transport::Http(url.into()),
        description: Some(format!(
            "Managed {} connection through Nanocodex Connect.",
            connection.name()
        )),
        arguments: Vec::new(),
        environment: BTreeMap::new(),
        cwd: None,
        bearer_env: None,
        bearer: Some(bearer.to_owned()),
        headers: BTreeMap::from([
            ("origin".to_owned(), APP_ORIGIN.to_owned()),
            ("x-nanocodex-app-id".to_owned(), APP_ID.to_owned()),
        ]),
        header_env: Vec::new(),
        startup_timeout: None,
        tool_timeout: None,
        enabled_tools: None,
        disabled_tools: Vec::new(),
        parallel_tools: Vec::new(),
    })
}

fn unique_managed_server_name(
    display_name: &str,
    connection_id: &str,
    servers: &BTreeMap<String, ServerConfig>,
) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in display_name.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            separator = false;
            if slug.len() < 40 {
                slug.push(character.to_ascii_lowercase());
            }
        } else {
            separator = true;
        }
    }
    if slug.is_empty() {
        slug.push_str("connection");
    }
    let id_prefix = &connection_id[..8];
    let base = format!("managed-{slug}-{id_prefix}");
    if !servers.contains_key(&base) {
        return base;
    }
    for suffix in 2_u32.. {
        let candidate = format!("{base}-{suffix}");
        if !servers.contains_key(&candidate) {
            return candidate;
        }
    }
    unreachable!("an unbounded numeric suffix always has a free MCP server name")
}

fn build_mcp(
    servers: BTreeMap<String, ServerConfig>,
    startup_timeout: Duration,
    tool_timeout: Duration,
    oauth_store: Option<Arc<CodexOAuthStore>>,
    tempo: Option<&crate::mpp::MppAdapter>,
) -> Result<ConfiguredMcp> {
    let mut builder = Mcp::builder();
    if let Some(store) = oauth_store {
        builder = builder.oauth_store(store);
    }
    for (name, server) in servers {
        let description = server.description;
        let payment = match &server.transport {
            Transport::Http(url) if name == "mercator" => tempo
                .map(|tempo| tempo.mcp_payment_provider(url))
                .transpose()?,
            _ => None,
        };
        let mut configured = match server.transport {
            Transport::Http(url) => McpServer::http(url),
            Transport::Stdio(command) => {
                let mut configured = McpServer::stdio(command).args(server.arguments);
                for (name, value) in server.environment {
                    configured = configured.env(name, value);
                }
                if let Some(cwd) = server.cwd {
                    configured = configured.cwd(cwd);
                }
                configured
            }
        }
        .startup_timeout(server.startup_timeout.unwrap_or(startup_timeout))
        .tool_timeout(server.tool_timeout.unwrap_or(tool_timeout));
        if let Some(description) = description {
            configured = configured.description(description);
        }
        if let Some(payment) = payment {
            configured = configured.payment_provider(payment);
        }
        if let Some(variable) = server.bearer_env {
            configured = configured.bearer_token_env(variable);
        }
        if let Some(token) = server.bearer {
            configured = configured.bearer_token(token);
        }
        for (header, value) in server.headers {
            configured = configured.header(header, value);
        }
        for (header, variable) in server.header_env {
            configured = configured.header_env(header, variable);
        }
        if let Some(enabled_tools) = server.enabled_tools {
            configured = configured.enabled_tools(enabled_tools);
        }
        configured = configured
            .disabled_tools(server.disabled_tools)
            .parallel_tools(server.parallel_tools);
        builder = builder.server(name, configured);
    }
    let provider = builder.build()?;
    let handle = provider.handle();
    Ok(ConfiguredMcp { provider, handle })
}

fn load_codex_mcp_servers(codex_home: &Path) -> Result<BTreeMap<String, Option<ServerConfig>>> {
    let path = codex_home.join("config.toml");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(error).wrap_err_with(|| format!("failed to read {}", path.display()));
        }
    };
    let config: CodexConfig = toml::from_str(&contents)
        .wrap_err_with(|| format!("failed to parse {}", path.display()))?;
    config
        .mcp_servers
        .into_iter()
        .map(|(name, server)| {
            let server = server.into_server_config(&name)?;
            Ok((name, server))
        })
        .collect()
}

impl CodexOAuthStore {
    const fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    fn credentials_path(&self) -> PathBuf {
        self.codex_home.join(".credentials.json")
    }

    fn load_blocking(
        &self,
        server_name: &str,
        server_url: &str,
    ) -> Result<Option<McpOAuthCredentials>, String> {
        let _lock = CodexOAuthFileLock::acquire(&self.codex_home)?;
        let entries = read_oauth_file(&self.credentials_path())?;
        let entry = entries
            .values()
            .filter(|entry| entry.server_name == server_name && entry.server_url == server_url)
            .max_by_key(|entry| entry.expires_at)
            .cloned();
        let Some(entry) = entry else {
            return Ok(None);
        };
        if entry.client_id.trim().is_empty() || entry.access_token.trim().is_empty() {
            return Err(format!(
                "stored OAuth credentials for `{server_name}` are incomplete"
            ));
        }
        let mut credentials =
            McpOAuthCredentials::new(entry.client_id, entry.access_token).scopes(entry.scopes);
        if let Some(refresh_token) = entry.refresh_token.filter(|token| !token.trim().is_empty()) {
            credentials = credentials.refresh_token(refresh_token);
        }
        if let Some(issuer) = entry.issuer.filter(|issuer| !issuer.trim().is_empty()) {
            credentials = credentials.issuer(issuer);
        }
        if let Some(expires_at) = entry.expires_at {
            credentials = credentials.expires_at_millis(expires_at);
        }
        Ok(Some(credentials))
    }

    fn save_blocking(
        &self,
        server_name: &str,
        server_url: &str,
        credentials: &McpOAuthCredentials,
    ) -> Result<(), String> {
        let _lock = CodexOAuthFileLock::acquire(&self.codex_home)?;
        let path = self.credentials_path();
        let mut entries = read_oauth_file(&path)?;
        entries
            .retain(|_, entry| entry.server_name != server_name || entry.server_url != server_url);
        entries.insert(
            codex_oauth_key(server_name, server_url)?,
            CodexOAuthEntry {
                server_name: server_name.to_owned(),
                server_url: server_url.to_owned(),
                client_id: credentials.client_id().to_owned(),
                access_token: credentials.access_token().to_owned(),
                expires_at: credentials.expires_at(),
                refresh_token: credentials.refresh_token_value().map(ToOwned::to_owned),
                issuer: credentials.authorization_issuer().map(ToOwned::to_owned),
                scopes: credentials.granted_scopes().to_vec(),
            },
        );
        write_oauth_file(&path, &entries)
    }
}

#[async_trait]
impl McpOAuthStore for CodexOAuthStore {
    async fn load(
        &self,
        server_name: &str,
        server_url: &str,
    ) -> Result<Option<McpOAuthCredentials>, String> {
        let store = self.clone();
        let server_name = server_name.to_owned();
        let server_url = server_url.to_owned();
        tokio::task::spawn_blocking(move || store.load_blocking(&server_name, &server_url))
            .await
            .map_err(|error| format!("MCP OAuth credential reader stopped: {error}"))?
    }

    async fn save(
        &self,
        server_name: &str,
        server_url: &str,
        credentials: &McpOAuthCredentials,
    ) -> Result<(), String> {
        let store = self.clone();
        let server_name = server_name.to_owned();
        let server_url = server_url.to_owned();
        let credentials = credentials.clone();
        tokio::task::spawn_blocking(move || {
            store.save_blocking(&server_name, &server_url, &credentials)
        })
        .await
        .map_err(|error| format!("MCP OAuth credential writer stopped: {error}"))?
    }

    async fn acquire_refresh_lock(
        &self,
        server_name: &str,
        server_url: &str,
    ) -> Result<Box<dyn McpOAuthRefreshGuard>, String> {
        let codex_home = self.codex_home.clone();
        let server_name = server_name.to_owned();
        let server_url = server_url.to_owned();
        tokio::task::spawn_blocking(move || {
            let store_key = codex_oauth_key(&server_name, &server_url)?;
            let mut hasher = Sha256::new();
            hasher.update(store_key.as_bytes());
            let path = codex_home
                .join("mcp-oauth-locks")
                .join(format!("{}.lock", hex::encode(hasher.finalize())));
            acquire_oauth_file_lock(&path, "refresh transaction")
                .map(|file| Box::new(file) as Box<dyn McpOAuthRefreshGuard>)
        })
        .await
        .map_err(|error| format!("MCP OAuth refresh-lock task stopped: {error}"))?
    }
}

struct CodexOAuthFileLock {
    _file: File,
}

impl CodexOAuthFileLock {
    fn acquire(codex_home: &Path) -> Result<Self, String> {
        let path = codex_home.join("mcp-oauth-locks/file-store.lock");
        acquire_oauth_file_lock(&path, "credential store").map(|file| Self { _file: file })
    }
}

fn acquire_oauth_file_lock(path: &Path, purpose: &str) -> Result<File, String> {
    let directory = path
        .parent()
        .ok_or_else(|| format!("MCP OAuth lock path has no parent: {}", path.display()))?;
    fs::create_dir_all(directory).map_err(|error| {
        format!(
            "failed to create MCP OAuth lock directory {}: {error}",
            directory.display()
        )
    })?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| format!("failed to open MCP OAuth lock {}: {error}", path.display()))?;
    let started = std::time::Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock)
                if started.elapsed() >= Duration::from_mins(1) =>
            {
                return Err(format!(
                    "timed out waiting for MCP OAuth {purpose} lock {}",
                    path.display()
                ));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(format!(
                    "failed to lock MCP OAuth {purpose} {}: {error}",
                    path.display()
                ));
            }
        }
    }
}

fn read_oauth_file(path: &Path) -> Result<CodexOAuthFile, String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(error) => {
            return Err(format!(
                "failed to read MCP OAuth credentials {}: {error}",
                path.display()
            ));
        }
    };
    serde_json::from_str(&contents).map_err(|error| {
        format!(
            "failed to parse MCP OAuth credentials {}: {error}",
            path.display()
        )
    })
}

fn write_oauth_file(path: &Path, entries: &CodexOAuthFile) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| {
        format!(
            "MCP OAuth credential path has no parent: {}",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create MCP OAuth credential directory {}: {error}",
            parent.display()
        )
    })?;
    let encoded = serde_json::to_vec(entries)
        .map_err(|error| format!("failed to encode MCP OAuth credentials: {error}"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        format!(
            "failed to create temporary MCP OAuth credential file in {}: {error}",
            parent.display()
        )
    })?;
    temporary
        .write_all(&encoded)
        .map_err(|error| format!("failed to write MCP OAuth credentials: {error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("failed to sync MCP OAuth credentials: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("failed to protect MCP OAuth credentials: {error}"))?;
    }
    temporary.persist(path).map_err(|error| {
        format!(
            "failed to replace MCP OAuth credentials {}: {}",
            path.display(),
            error.error
        )
    })?;
    Ok(())
}

fn codex_oauth_key(server_name: &str, server_url: &str) -> Result<String, String> {
    #[derive(Serialize)]
    struct KeyPayload<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        url: &'a str,
        headers: BTreeMap<String, String>,
    }

    let encoded = serde_json::to_vec(&KeyPayload {
        kind: "http",
        url: server_url,
        headers: BTreeMap::new(),
    })
    .map_err(|error| format!("failed to encode MCP OAuth credential key: {error}"))?;
    let prefix = hex::encode(Sha256::digest(encoded));
    Ok(format!("{server_name}|{}", &prefix[..16]))
}

impl CodexMcpServer {
    fn into_server_config(self, name: &str) -> Result<Option<ServerConfig>> {
        if !self.enabled {
            return Ok(None);
        }
        let transport = match (self.command, self.url) {
            (Some(command), None) => Transport::Stdio(command),
            (None, Some(url)) => Transport::Http(url),
            (Some(_), Some(_)) => {
                bail!("Codex MCP server `{name}` configures both `command` and `url`");
            }
            (None, None) => {
                bail!("Codex MCP server `{name}` configures neither `command` nor `url`");
            }
        };
        let startup_timeout = match (self.startup_timeout_sec, self.startup_timeout_ms) {
            (Some(seconds), _) => {
                Some(duration_from_seconds(name, "startup_timeout_sec", seconds)?)
            }
            (None, Some(milliseconds)) => Some(Duration::from_millis(milliseconds)),
            (None, None) => None,
        };
        let tool_timeout = self
            .tool_timeout_sec
            .map(|seconds| duration_from_seconds(name, "tool_timeout_sec", seconds))
            .transpose()?;
        Ok(Some(ServerConfig {
            transport,
            description: Some("Configured in Codex config.toml.".to_owned()),
            arguments: self.args,
            environment: self.env,
            cwd: self.cwd,
            bearer_env: self.bearer_token_env_var,
            bearer: None,
            headers: self.http_headers,
            header_env: self.env_http_headers.into_iter().collect(),
            startup_timeout,
            tool_timeout,
            enabled_tools: self.enabled_tools,
            disabled_tools: self.disabled_tools,
            parallel_tools: Vec::new(),
        }))
    }
}

fn duration_from_seconds(server: &str, field: &str, seconds: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(seconds)
        .map_err(|error| eyre!("Codex MCP server `{server}` has invalid `{field}`: {error}"))
}

const fn enabled_by_default() -> bool {
    true
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
                description: None,
                arguments: Vec::new(),
                environment: BTreeMap::new(),
                cwd: None,
                bearer_env: None,
                bearer: None,
                headers: BTreeMap::new(),
                header_env: Vec::new(),
                startup_timeout: None,
                tool_timeout: None,
                enabled_tools: None,
                disabled_tools: Vec::new(),
                parallel_tools: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use nanocodex::{oai::MODEL, tools::ToolContext};
    use nanocodex_observability::{LogFormat, LogOutput, ObservabilityBuilder};
    use nanocodex_tools::{
        ToolExposure, ToolInput, Tools,
        contract::DEFAULT_TOOL_OUTPUT_TOKENS,
        runtime::{DynamicToolProvider, ToolRuntime},
    };
    use serde_json::{Value, json, value::to_raw_value};

    fn args() -> McpArgs {
        McpArgs {
            disabled: false,
            mcp_defaults: true,
            mcp_codex_config: true,
            http: Vec::new(),
            stdio: Vec::new(),
            arguments: Vec::new(),
            bearer_env: Vec::new(),
            header_env: Vec::new(),
            mcp_startup_timeout: 30,
            mcp_tool_timeout: 300,
        }
    }

    #[test]
    fn default_mcp_servers_build() {
        assert!(
            args()
                .build(Path::new("/missing"), None, None)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn managed_mcp_config_uses_exact_proxy_url_and_scoped_headers() {
        let origin = reqwest::Url::parse("https://connect.example/").unwrap();
        let grant_id = format!("0x{}", "33".repeat(32));
        let connection = crate::login::ManagedMcpConnection {
            id: "a".repeat(43),
            name: "Linear Workspace".to_owned(),
        };
        let config =
            managed_mcp_server_config(&origin, &grant_id, "grant-token", &connection).unwrap();
        assert!(matches!(
            config.transport,
            Transport::Http(ref url)
                if url == &format!(
                    "https://connect.example/v1/grants/{grant_id}/mcp/{}",
                    connection.id
                )
        ));
        assert_eq!(config.bearer.as_deref(), Some("grant-token"));
        assert_eq!(
            config.headers,
            BTreeMap::from([
                ("origin".to_owned(), APP_ORIGIN.to_owned()),
                ("x-nanocodex-app-id".to_owned(), APP_ID.to_owned()),
            ])
        );
        assert!(config.bearer_env.is_none());
    }

    #[test]
    fn managed_mcp_names_preserve_existing_servers_without_collisions() {
        let connection_id = "a".repeat(43);
        let mut servers = BTreeMap::new();
        let first = unique_managed_server_name("Linear Workspace", &connection_id, &servers);
        assert_eq!(first, "managed-linear-workspace-aaaaaaaa");
        servers.insert(
            first.clone(),
            managed_mcp_server_config(
                &reqwest::Url::parse("https://connect.example/").unwrap(),
                &format!("0x{}", "33".repeat(32)),
                "grant-token",
                &crate::login::ManagedMcpConnection {
                    id: connection_id.clone(),
                    name: "Linear Workspace".to_owned(),
                },
            )
            .unwrap(),
        );
        assert_eq!(
            unique_managed_server_name("Linear Workspace", &connection_id, &servers),
            "managed-linear-workspace-aaaaaaaa-2"
        );
        assert!(servers.contains_key(&first));
    }

    #[test]
    fn defaults_can_be_disabled() {
        assert!(
            McpArgs {
                mcp_defaults: false,
                ..args()
            }
            .build(Path::new("/missing"), None, None)
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn explicit_server_can_override_a_default_name() {
        let mut args = args();
        args.http.push(NamedValue {
            name: "tempo".to_owned(),
            value: "https://example.test/mcp".to_owned(),
        });

        assert!(
            args.build(Path::new("/missing"), None, None)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn duplicate_explicit_servers_are_rejected() {
        let mut args = args();
        for value in ["https://one.test/mcp", "https://two.test/mcp"] {
            args.http.push(NamedValue {
                name: "tempo".to_owned(),
                value: value.to_owned(),
            });
        }

        assert!(args.build(Path::new("/missing"), None, None).is_err());
    }

    #[tokio::test]
    async fn codex_servers_merge_between_explicit_entries_and_defaults() {
        let codex_home = tempfile::tempdir().unwrap();
        fs::write(
            codex_home.path().join("config.toml"),
            r#"
[mcp_servers.centaur-paradigm]
url = "https://centaur.example.test/mcp"
startup_timeout_sec = 12.5
tool_timeout_sec = 45
enabled_tools = ["search"]
disabled_tools = ["write"]

[mcp_servers.local]
command = "node"
args = ["server.mjs"]
cwd = "/workspace"

[mcp_servers.local.env]
MODE = "read-only"

[mcp_servers.tempo]
url = "https://disabled.example.test/mcp"
enabled = false
"#,
        )
        .unwrap();
        let mut args = args();
        args.mcp_codex_config = true;
        let mcp = args.build(codex_home.path(), None, None).unwrap().unwrap();
        let tools = Tools::builder()
            .exposure(ToolExposure::DirectAndCodeMode)
            .provider(mcp.provider)
            .build()
            .unwrap();
        let encoded = serde_json::to_string(
            &ToolRuntime::new_with_tools(".", None, None, &tools).model_specs("test-session"),
        )
        .unwrap();

        assert!(encoded.contains("centaur-paradigm"));
        assert!(encoded.contains("local"));
        assert!(encoded.contains("openaiDeveloperDocs"));
        assert!(encoded.contains("cloudflare"));
        assert!(encoded.contains("viem"));
        assert!(encoded.contains("vocs"));
        assert!(!encoded.contains("\n- tempo:"));
    }

    #[test]
    fn codex_transport_options_are_preserved() {
        let codex_home = tempfile::tempdir().unwrap();
        fs::write(
            codex_home.path().join("config.toml"),
            r#"
[mcp_servers.remote]
url = "https://remote.example.test/mcp"
bearer_token_env_var = "REMOTE_TOKEN"
http_headers = { "X-Fixed" = "fixed" }
env_http_headers = { "X-Secret" = "SECRET_HEADER" }
startup_timeout_ms = 2500
tool_timeout_sec = 9.5
"#,
        )
        .unwrap();

        let mut loaded = load_codex_mcp_servers(codex_home.path()).unwrap();
        let server = loaded.remove("remote").unwrap().unwrap();
        assert!(
            matches!(server.transport, Transport::Http(ref url) if url == "https://remote.example.test/mcp")
        );
        assert_eq!(server.bearer_env.as_deref(), Some("REMOTE_TOKEN"));
        assert_eq!(
            server.headers.get("X-Fixed").map(String::as_str),
            Some("fixed")
        );
        assert_eq!(
            server.header_env,
            vec![("X-Secret".to_owned(), "SECRET_HEADER".to_owned())]
        );
        assert_eq!(server.startup_timeout, Some(Duration::from_millis(2500)));
        assert_eq!(server.tool_timeout, Some(Duration::from_millis(9500)));
    }

    #[tokio::test]
    async fn codex_oauth_store_reads_legacy_keys_and_replaces_duplicates() {
        let codex_home = tempfile::tempdir().unwrap();
        let path = codex_home.path().join(".credentials.json");
        fs::write(
            &path,
            r#"{
                "remote|legacy": {
                    "server_name": "remote",
                    "server_url": "https://remote.example/mcp",
                    "client_id": "old-client",
                    "access_token": "old-access",
                    "expires_at": 100,
                    "refresh_token": "old-refresh",
                    "scopes": ["mcp:tools"]
                },
                "remote|newer": {
                    "server_name": "remote",
                    "server_url": "https://remote.example/mcp",
                    "client_id": "new-client",
                    "access_token": "new-access",
                    "expires_at": 200,
                    "refresh_token": "new-refresh",
                    "issuer": "https://remote.example",
                    "scopes": ["mcp:tools"]
                },
                "other|entry": {
                    "server_name": "other",
                    "server_url": "https://other.example/mcp",
                    "client_id": "other-client",
                    "access_token": "other-access",
                    "scopes": []
                }
            }"#,
        )
        .unwrap();
        let store = CodexOAuthStore::new(codex_home.path().to_path_buf());
        let loaded = store
            .load("remote", "https://remote.example/mcp")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.client_id(), "new-client");
        assert_eq!(loaded.access_token(), "new-access");
        assert_eq!(
            loaded.authorization_issuer(),
            Some("https://remote.example")
        );

        let replacement = McpOAuthCredentials::new("current-client", "current-access")
            .refresh_token("current-refresh")
            .issuer("https://remote.example")
            .expires_at_millis(300)
            .scopes(["mcp:tools"]);
        store
            .save("remote", "https://remote.example/mcp", &replacement)
            .await
            .unwrap();
        let entries: CodexOAuthFile =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(entries.len(), 2);
        let key = codex_oauth_key("remote", "https://remote.example/mcp").unwrap();
        assert_eq!(entries[&key].access_token, "current-access");
        assert_eq!(
            entries[&key].issuer.as_deref(),
            Some("https://remote.example")
        );
        assert!(entries.contains_key("other|entry"));
    }

    #[test]
    fn codex_oauth_key_matches_current_insertion_order() {
        assert_eq!(
            codex_oauth_key(
                "centaur-paradigm",
                "https://centaur-mcp.tailea35.ts.net/mcp"
            )
            .unwrap(),
            "centaur-paradigm|144c94893cdf13f9"
        );
    }

    #[tokio::test]
    #[ignore = "manual live Codex-config OAuth MCP reload smoke"]
    async fn smoke_codex_oauth_mcp_reload() {
        let codex_home = std::env::var_os("NANOCODEX_MCP_SMOKE_CODEX_HOME")
            .map(PathBuf::from)
            .expect("set NANOCODEX_MCP_SMOKE_CODEX_HOME");
        let mut servers = load_codex_mcp_servers(&codex_home).unwrap();
        servers.retain(|name, _| name.starts_with("centaur-"));
        let servers = servers
            .into_iter()
            .filter_map(|(name, server)| server.map(|server| (name, server)))
            .collect();
        let configured = build_mcp(
            servers,
            Duration::from_secs(30),
            Duration::from_mins(5),
            Some(Arc::new(CodexOAuthStore::new(codex_home))),
            None,
        )
        .unwrap();

        for name in ["centaur-paradigm", "centaur-tempo"] {
            let tools = configured.handle.reload(name).await.unwrap();
            assert!(tools > 0, "{name} should expose tools");
            eprintln!("{name}: {tools} tools");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "manual live MCP lifecycle benchmark"]
    #[allow(
        clippy::too_many_lines,
        reason = "the benchmark keeps one lifecycle and one configured provider"
    )]
    async fn benchmark_codex_oauth_mcp_lifecycle() {
        let trace_guard = std::env::var_os("NANOCODEX_MCP_BENCH_TRACE").map(|path| {
            let mut builder =
                ObservabilityBuilder::new("nanocodex-mcp-bench", env!("CARGO_PKG_VERSION"))
                    .filter("warn,nanocodex_tools=info")
                    .format(LogFormat::Json)
                    .output(LogOutput::File(PathBuf::from(path)));
            if let Ok(endpoint) = std::env::var("NANOCODEX_MCP_BENCH_OTEL") {
                builder = builder
                    .otel_filter("warn,nanocodex_tools=info")
                    .otlp_endpoint(endpoint);
            }
            builder.install().unwrap()
        });
        let iterations = benchmark_iterations("NANOCODEX_MCP_BENCH_RELOADS", 10);
        let search_iterations = benchmark_iterations("NANOCODEX_MCP_BENCH_SEARCHES", 10_000);
        let codex_home = std::env::var_os("NANOCODEX_MCP_SMOKE_CODEX_HOME")
            .map(PathBuf::from)
            .expect("set NANOCODEX_MCP_SMOKE_CODEX_HOME");
        let mut servers = load_codex_mcp_servers(&codex_home).unwrap();
        servers.retain(|name, _| name.starts_with("centaur-"));
        let servers = servers
            .into_iter()
            .filter_map(|(name, server)| server.map(|server| (name, server)))
            .collect();
        let configured = build_mcp(
            servers,
            Duration::from_secs(30),
            Duration::from_mins(5),
            Some(Arc::new(CodexOAuthStore::new(codex_home))),
            None,
        )
        .unwrap();

        let search = configured
            .provider
            .direct_tools()
            .into_iter()
            .next()
            .unwrap();
        let context = ToolContext::new(
            MODEL,
            "mcp-lifecycle-benchmark",
            "catalog-search",
            &[],
            DEFAULT_TOOL_OUTPUT_TOKENS,
        );
        let input = to_raw_value(&json!({
            "query": "search tools domain ownership documentation"
        }))
        .unwrap();
        let start_return = std::time::Instant::now();
        configured.provider.start();
        let start_return = start_return.elapsed();
        let prewarm = std::time::Instant::now();
        let initial_search = search
            .execute(ToolInput::Function(input.clone()), context)
            .await
            .unwrap();
        let prewarm = prewarm.elapsed();
        assert!(initial_search.success);
        eprintln!(
            "{}",
            json!({
                "benchmark": "mcp_background_startup",
                "start_return_us": start_return.as_secs_f64() * 1_000_000.0,
                "catalog_ready_ms": duration_millis(prewarm),
                "activated_tools": configured.provider.available_definitions().len(),
            })
        );

        let mut catalog_tools = 0;
        for name in ["centaur-paradigm", "centaur-tempo"] {
            let mut samples = Vec::with_capacity(iterations);
            let mut tool_count = 0;
            for _ in 0..iterations {
                let started = std::time::Instant::now();
                tool_count = configured.handle.reload(name).await.unwrap();
                samples.push(started.elapsed());
            }
            assert!(tool_count > 0, "{name} should expose tools");
            catalog_tools += tool_count;
            eprintln!(
                "{}",
                json!({
                    "benchmark": "mcp_reload",
                    "server": name,
                    "tools": tool_count,
                    "iterations": iterations,
                    "cold_ms": duration_millis(samples[0]),
                    "warm": duration_summary(&samples[1..]),
                })
            );
        }

        let started = std::time::Instant::now();
        for _ in 0..search_iterations {
            let result = search
                .execute(ToolInput::Function(input.clone()), context)
                .await
                .unwrap();
            assert!(result.success);
        }
        let elapsed = started.elapsed();
        let search_count = f64::from(u32::try_from(search_iterations).unwrap());
        eprintln!(
            "{}",
            json!({
                "benchmark": "mcp_catalog_search",
                "catalog_tools": catalog_tools,
                "activated_tools": configured.provider.available_definitions().len(),
                "iterations": search_iterations,
                "total_ms": duration_millis(elapsed),
                "mean_us": elapsed.as_secs_f64() * 1_000_000.0 / search_count,
                "throughput_per_second": search_count / elapsed.as_secs_f64(),
            })
        );
        if let Some(mut trace_guard) = trace_guard {
            trace_guard.shutdown().unwrap();
        }
    }

    fn benchmark_iterations(variable: &str, default: usize) -> usize {
        std::env::var(variable)
            .ok()
            .map_or(default, |value| value.parse::<usize>().unwrap())
            .max(1)
    }

    fn duration_millis(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1_000.0
    }

    fn duration_summary(samples: &[Duration]) -> Value {
        if samples.is_empty() {
            return json!({});
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let percentile = |numerator: usize| {
            let index = (sorted.len() - 1) * numerator / 100;
            duration_millis(sorted[index])
        };
        json!({
            "samples": sorted.len(),
            "min_ms": duration_millis(sorted[0]),
            "p50_ms": percentile(50),
            "p95_ms": percentile(95),
            "max_ms": duration_millis(*sorted.last().unwrap()),
            "mean_ms": sorted.iter().map(Duration::as_secs_f64).sum::<f64>()
                * 1_000.0 / f64::from(u32::try_from(sorted.len()).unwrap()),
        })
    }

    #[tokio::test]
    async fn defaults_keep_mcp_tool_schemas_deferred_in_code_mode_only() {
        let baseline_tools = Tools::builder().build().unwrap();
        let baseline = serde_json::to_vec(
            &ToolRuntime::new_with_tools(".", None, None, &baseline_tools)
                .model_specs("test-session"),
        )
        .unwrap();

        let mcp = args()
            .build(Path::new("/missing"), None, None)
            .unwrap()
            .unwrap();
        let default_tools = Tools::builder().provider(mcp.provider).build().unwrap();
        let with_defaults = serde_json::to_vec(
            &ToolRuntime::new_with_tools(".", None, None, &default_tools)
                .model_specs("test-session"),
        )
        .unwrap();
        let encoded = String::from_utf8(with_defaults.clone()).unwrap();

        assert!(encoded.contains("\"type\":\"tool_search\""));
        assert!(encoded.contains("Some deferred nested tools may be omitted"));
        assert!(!encoded.contains("mcp__openaiDeveloperDocs__"));
        assert!(!encoded.contains("mcp__tempo__"));
        assert!(!encoded.contains("mcp__cloudflare__"));
        assert!(with_defaults.len() > baseline.len());

        eprintln!(
            "initial serialized tool context: baseline={} bytes, default MCP={} bytes, delta={} bytes",
            baseline.len(),
            with_defaults.len(),
            with_defaults.len() - baseline.len()
        );
    }
}
