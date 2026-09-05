use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    io::{Read, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::{Address, U256};
use alloy_signer_local::PrivateKeySigner;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::{ArgAction, Args};
use eyre::{Result, WrapErr, bail, ensure, eyre};
use futures_util::StreamExt;
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use rand::RngCore;
use reqwest::{Client, Response, StatusCode, Url, redirect::Policy};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempo_alloy::{
    accounts::{TempoAccountsKeyAuthorization, TempoAccountsStore},
    primitives::transaction::{
        CallScope, SelectorRule, SignatureType, SignedKeyAuthorization, TokenLimit,
    },
};

use crate::{
    auth::open_browser,
    config::{default_auth_file, default_codex_home},
};

pub(crate) const APP_ID: &str = "nanocodex-cli";
pub(crate) const APP_ORIGIN: &str = "https://cli.nanocodex.xyz";
const DEFAULT_DEVICE_BASE: &str = "https://nanocodex-connect-api.gakonst.workers.dev/v1/device";
const PRODUCTION_API_ORIGIN: &str = "https://nanocodex-connect-api.gakonst.workers.dev";
const PRODUCTION_VERIFY_ORIGIN: &str = "https://nanocodex.gakonst.workers.dev";
const RPC_ID: &str = "nanocodex-cli-login";
const CHAIN_ID: u64 = 4217;
const CHAIN_ID_HEX: &str = "0x1079";
const RESPONSE_LIMIT: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const OVERALL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_SLOW_DOWN_STREAK: u8 = 3;
const ACCESS_KEY_LIFETIME: u64 = 30 * 86_400;
const HOSTED_EXPIRY_CLOCK_SKEW: u64 = 5 * 60;
const CHATGPT_IMPORT_EXPIRY_MARGIN: Duration = Duration::from_secs(5 * 60);
const CHATGPT_AUTH_FILE_LIMIT: u64 = 64 * 1024;
const CHATGPT_TOKEN_LIMIT: usize = 32 * 1024;
const CHATGPT_ACCOUNT_ID_LIMIT: usize = 256;
const CHATGPT_IMPORT_DOMAIN: &[u8] = b"nanocodex/chatgpt-credential-import/v1\0";
const CHATGPT_IMPORT_RESOURCE_PREFIX: &str =
    "urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:";
pub(crate) const LOCAL_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX: &str =
    "urn:nanocodex:browser-cookies:local-sync:";
const URI_COMPONENT_ORIGIN_ENCODE_SET: &AsciiSet =
    &CONTROLS.add(b'%').add(b'/').add(b':').add(b'[').add(b']');
const MPP_LIMIT: u64 = 10_000_000;
const MPP_PERIOD: u64 = 86_400;

const MACH: &str = "0x20c000000000000000000000f37de3740ADec032";
const USDC_E: &str = "0x20C000000000000000000000b9537d11c60E8b50";
const TIP20_CHANNEL_ESCROW: &str = "0x33b901018174DDabE4841042ab76ba85D4e24f25";
const MERCATOR_SETTLEMENT: &str = "0xa295C42FBCC026a62304A7701f25B4c91799B0dA";

const REQUIRED_DATA_CAPABILITIES: &[&str] = &[
    "agent.output.final",
    "agent.output.actions",
    "history:read",
    "memory:read",
    "memory:write",
];
const CONNECTOR_NAMES: &[&str] = &["chatgpt", "github", "gmail", "gdrive", "x"];

#[derive(Args, Clone)]
pub(crate) struct Login {
    /// Approve the bounded MACH/USDC.e MPP spending policy.
    #[arg(long, action = ArgAction::SetTrue)]
    mpp: bool,
    /// Authorize this installation to sync cookies for one exact HTTP(S) origin.
    #[arg(
        long = "browser-cookie-origin",
        value_name = "ORIGIN",
        value_parser = canonical_browser_cookie_origin
    )]
    browser_cookie_origin: Option<String>,
    /// Use a trusted local Nanocodex Connect endpoint for development.
    #[arg(
        long,
        env = "NANOCODEX_CONNECT_DEVICE_BASE_URL",
        hide_env_values = true
    )]
    device_base_url: Option<String>,
    /// Print the verification URL without opening a browser.
    #[arg(long)]
    no_open: bool,
}

#[derive(Args, Clone)]
pub(crate) struct Connect {
    /// Hosted services or remote MCP hosts to connect and grant to this installation.
    #[arg(required = true, num_args = 1.., value_name = "SERVICE")]
    services: Vec<ConnectTarget>,
    /// Override the Codex `auth.json` imported by an explicit ChatGPT connection.
    #[arg(long, env = "NANOCODEX_AUTH_FILE")]
    auth_file: Option<PathBuf>,
    /// Use a trusted local Nanocodex Connect endpoint for development.
    #[arg(
        long,
        env = "NANOCODEX_CONNECT_DEVICE_BASE_URL",
        hide_env_values = true
    )]
    device_base_url: Option<String>,
    /// Print the verification URL without opening a browser.
    #[arg(long)]
    no_open: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Connector {
    Chatgpt,
    Github,
    Gmail,
    Gdrive,
    X,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ConnectTarget {
    Connector(Connector),
    RemoteMcp(String),
}

impl FromStr for ConnectTarget {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let connector = match value {
            "chatgpt" => Some(Connector::Chatgpt),
            "github" => Some(Connector::Github),
            "gmail" => Some(Connector::Gmail),
            "gdrive" => Some(Connector::Gdrive),
            "x" => Some(Connector::X),
            _ => None,
        };
        connector.map_or_else(
            || canonical_remote_mcp_target(value).map(Self::RemoteMcp),
            |connector| Ok(Self::Connector(connector)),
        )
    }
}

fn canonical_remote_mcp_target(value: &str) -> std::result::Result<String, String> {
    let host = value.to_ascii_lowercase();
    if value.is_empty()
        || !value.is_ascii()
        || value != value.trim()
        || host.len() > 253
        || !host.starts_with("mcp.")
        || host.ends_with('.')
        || host.parse::<std::net::IpAddr>().is_ok()
    {
        return Err("expected a supported service or public MCP host beginning `mcp.`".to_owned());
    }
    let labels: Vec<_> = host.split('.').collect();
    let valid_label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && label
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && label
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
    };
    let tld = labels.last().copied().unwrap_or_default();
    let reserved_tld = matches!(
        tld,
        "arpa"
            | "example"
            | "home"
            | "internal"
            | "invalid"
            | "lan"
            | "local"
            | "localhost"
            | "onion"
            | "test"
    );
    if labels.len() < 3
        || labels.iter().any(|label| !valid_label(label))
        || tld.len() < 2
        || !tld.bytes().all(|byte| byte.is_ascii_alphabetic())
        || reserved_tld
    {
        return Err("expected a supported service or public MCP host beginning `mcp.`".to_owned());
    }
    Ok(host)
}

impl Connector {
    const fn id(self) -> &'static str {
        match self {
            Self::Chatgpt => "chatgpt",
            Self::Github => "github",
            Self::Gmail => "gmail",
            Self::Gdrive => "gdrive",
            Self::X => "x",
        }
    }
}

#[derive(Args, Clone, Copy)]
pub(crate) struct Status {}

#[derive(Args, Clone, Copy)]
pub(crate) struct Logout {}

impl Login {
    pub(crate) async fn run(self) -> Result<()> {
        let paths = LoginPaths::default()?;
        let mut request =
            RequestedCapabilities::login(self.mpp, self.browser_cookie_origin.as_deref());
        let device_base = validated_device_base(self.device_base_url.as_deref())?;
        let expected_origin = normalized_origin(api_origin(&device_base)?)?;
        let prior = StoredLogin::load(&paths.login)?;
        let mut active = active_login(&paths.login, Some(&expected_origin)).await?;
        if let Some(stored) = &mut active {
            finish_pending_retirement(&paths, stored).await?;
            if stored.satisfies_login(self.mpp, self.browser_cookie_origin.as_deref()) {
                print_summary(stored);
                return Ok(());
            }
            request.preserve_from(stored)?;
        }
        let retirement = active
            .as_ref()
            .map(|login| PendingRetirement::from_login(login, true))
            .or_else(|| {
                prior
                    .as_ref()
                    .map(|login| PendingRetirement::from_login(login, false))
            });
        LoginFlow::new(device_base, paths, !self.no_open)?
            .complete(request, retirement)
            .await
    }
}

impl Connect {
    pub(crate) async fn run(self) -> Result<()> {
        let chatgpt_credential_import = self.load_chatgpt_credential_import()?;
        let paths = LoginPaths::default()?;
        let device_base = validated_device_base(self.device_base_url.as_deref())?;
        let expected_origin = normalized_origin(api_origin(&device_base)?)?;
        let prior = StoredLogin::load(&paths.login)?;
        let mut active = active_login(&paths.login, Some(&expected_origin)).await?;
        if let Some(stored) = &mut active {
            finish_pending_retirement(&paths, stored).await?;
        }

        let mut request = RequestedCapabilities::connect(&self.services);
        if let Some(stored) = &active {
            request.preserve_from(stored)?;
        }
        let retirement = active
            .as_ref()
            .map(|login| PendingRetirement::from_login(login, true))
            .or_else(|| {
                prior
                    .as_ref()
                    .map(|login| PendingRetirement::from_login(login, false))
            });
        LoginFlow::new(device_base, paths, !self.no_open)?
            .with_chatgpt_credential_import(chatgpt_credential_import)
            .complete(request, retirement)
            .await
    }

    fn load_chatgpt_credential_import(&self) -> Result<Option<ChatgptCredentialImport>> {
        let imports_chatgpt = self
            .services
            .iter()
            .any(|service| matches!(service, ConnectTarget::Connector(Connector::Chatgpt)));
        ensure!(
            imports_chatgpt || self.auth_file.is_none(),
            "--auth-file is only valid with an explicit `nanocodex connect chatgpt` request"
        );
        let chatgpt_credential_import = if imports_chatgpt {
            let path = self.auth_file.clone().map_or_else(default_auth_file, Ok)?;
            Some(load_chatgpt_credential_import(&path)?)
        } else {
            None
        };
        Ok(chatgpt_credential_import)
    }
}

fn requested_connectors(targets: &[&'static str]) -> Vec<&'static str> {
    CONNECTOR_NAMES
        .iter()
        .copied()
        .filter(|connector| targets.contains(connector))
        .collect()
}

impl Status {
    pub(crate) async fn run(self) -> Result<()> {
        let paths = LoginPaths::default()?;
        match active_login(&paths.login, None).await? {
            Some(mut login) => {
                if let Err(error) = finish_pending_retirement(&paths, &mut login).await {
                    eprintln!("Prior login cleanup is still pending: {error:#}");
                }
                print_summary(&login);
            }
            None => println!("Not logged in to Nanocodex"),
        }
        Ok(())
    }
}

impl Logout {
    pub(crate) async fn run(self) -> Result<()> {
        let paths = LoginPaths::default()?;
        let Some(login) = StoredLogin::load(&paths.login)? else {
            println!("Not logged in to Nanocodex");
            return Ok(());
        };
        let mut login = login;
        finish_pending_retirement(&paths, &mut login).await?;
        retire_installation_key(&paths.accounts, &login)?;
        if login.expires_at > unix_timestamp()? {
            revoke_grant(&login).await?;
        }
        remove_login(&paths.login)?;
        println!("Logged out of Nanocodex");
        Ok(())
    }
}

#[derive(Clone)]
struct LoginPaths {
    login: PathBuf,
    accounts: PathBuf,
}

impl LoginPaths {
    fn default() -> Result<Self> {
        Ok(Self {
            login: default_codex_home()?.join("connect.json"),
            accounts: TempoAccountsStore::default_path()
                .wrap_err("failed to resolve the Tempo Accounts store")?
                .path()
                .to_owned(),
        })
    }
}

#[derive(Clone, Debug)]
struct RequestedCapabilities {
    connectors: Vec<&'static str>,
    mcp_targets: Vec<String>,
    mcp_connections: Vec<ManagedMcpConnection>,
    mpp: bool,
    browser_cookie_origin: Option<String>,
    focus_connector: Option<&'static str>,
    focus_mcp: bool,
}

impl RequestedCapabilities {
    fn login(mpp: bool, browser_cookie_origin: Option<&str>) -> Self {
        Self {
            connectors: Vec::new(),
            mcp_targets: Vec::new(),
            mcp_connections: Vec::new(),
            mpp,
            browser_cookie_origin: browser_cookie_origin.map(str::to_owned),
            focus_connector: None,
            focus_mcp: false,
        }
    }

    fn connect(services: &[ConnectTarget]) -> Self {
        let mut connector_targets = Vec::new();
        let mut mcp_targets = Vec::new();
        for service in services {
            match service {
                ConnectTarget::Connector(service) => {
                    let id = service.id();
                    if !connector_targets.contains(&id) {
                        connector_targets.push(id);
                    }
                }
                ConnectTarget::RemoteMcp(target) if !mcp_targets.contains(target) => {
                    mcp_targets.push(target.clone());
                }
                ConnectTarget::RemoteMcp(_) => {}
            }
        }
        let sole_target = connector_targets.len() + mcp_targets.len() == 1;
        Self {
            connectors: requested_connectors(&connector_targets),
            mcp_targets,
            mcp_connections: Vec::new(),
            mpp: false,
            browser_cookie_origin: None,
            focus_connector: sole_target
                .then(|| connector_targets.first().copied())
                .flatten(),
            focus_mcp: sole_target && connector_targets.is_empty(),
        }
    }

    fn preserve_from(&mut self, login: &StoredLogin) -> Result<()> {
        let capabilities: HashSet<&str> = login.capabilities.iter().map(String::as_str).collect();
        self.connectors = requested_connectors(
            &CONNECTOR_NAMES
                .iter()
                .copied()
                .filter(|connector| {
                    self.connectors.contains(connector) || capabilities.contains(*connector)
                })
                .collect::<Vec<_>>(),
        );
        for connection in &login.mcp_connections {
            if !self
                .mcp_connections
                .iter()
                .any(|existing| existing.id == connection.id)
            {
                self.mcp_connections.push(connection.clone());
            }
        }
        self.mpp |= capabilities.contains("mpp.mach");
        if self.browser_cookie_origin.is_none() {
            let origins = login
                .capabilities
                .iter()
                .filter(|capability| {
                    capability.starts_with(LOCAL_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX)
                })
                .map(|capability| {
                    parse_local_browser_cookie_sync_resource(capability).ok_or_else(|| {
                        eyre!("stored grant has an invalid browser cookie sync authority")
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                origins.len() <= 1,
                "stored grant has multiple browser cookie sync authorities"
            );
            self.browser_cookie_origin = origins.into_iter().next();
        }
        Ok(())
    }

    fn resources(&self, chatgpt_import_resource: Option<&str>) -> Vec<String> {
        let mut resources = vec![
            "urn:nanocodex:agent:run".to_owned(),
            "urn:nanocodex:history:read".to_owned(),
            "urn:nanocodex:memory:read".to_owned(),
            "urn:nanocodex:memory:write".to_owned(),
            format!("urn:nanocodex:app:{APP_ID}"),
            "urn:nanocodex:origin:https%3A%2F%2Fcli.nanocodex.xyz".to_owned(),
            "urn:nanocodex:agent:visibility:reply,actions,history".to_owned(),
        ];
        if !self.connectors.is_empty() {
            resources.push(format!(
                "urn:nanocodex:connectors:{}",
                self.connectors.join(",")
            ));
        }
        if let Some(connector) = self.focus_connector {
            resources.push(format!("urn:nanocodex:connector-focus:{connector}"));
        }
        if let Some(resource) = chatgpt_import_resource {
            resources.push(resource.to_owned());
        }
        resources.extend(
            self.mcp_connections
                .iter()
                .map(|connection| format!("urn:nanocodex:mcp:{}", connection.id)),
        );
        if self.focus_mcp
            && let Some(connection) = self.mcp_connections.first()
        {
            resources.push(format!("urn:nanocodex:mcp-focus:{}", connection.id));
        }
        if self.mpp {
            resources.extend([
                "urn:nanocodex:capability:mercator:boost".to_owned(),
                "urn:nanocodex:mpp:machusd:spend".to_owned(),
            ]);
        } else {
            resources.push("urn:nanocodex:authorization:hosted".to_owned());
        }
        if let Some(origin) = &self.browser_cookie_origin {
            resources.push(local_browser_cookie_sync_resource(origin));
        }
        resources
    }
}

pub(crate) fn local_browser_cookie_sync_resource(origin: &str) -> String {
    format!(
        "{LOCAL_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX}{}",
        utf8_percent_encode(origin, URI_COMPONENT_ORIGIN_ENCODE_SET)
    )
}

fn parse_local_browser_cookie_sync_resource(resource: &str) -> Option<String> {
    let encoded = resource.strip_prefix(LOCAL_BROWSER_COOKIE_SYNC_RESOURCE_PREFIX)?;
    let decoded = percent_decode_str(encoded).decode_utf8().ok()?;
    let canonical = canonical_browser_cookie_origin(&decoded).ok()?;
    (local_browser_cookie_sync_resource(&canonical) == resource).then_some(canonical)
}

pub(crate) fn canonical_browser_cookie_origin(value: &str) -> std::result::Result<String, String> {
    let url = Url::parse(value).map_err(|_| "expected an absolute HTTP(S) origin".to_owned())?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err("expected only a scheme, host, and optional port".to_owned());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "expected an HTTP(S) origin with a host".to_owned())?;
    let secure = url.scheme() == "https";
    let loopback_http = url.scheme() == "http"
        && (host.eq_ignore_ascii_case("localhost")
            || host == "127.0.0.1"
            || matches!(host, "::1" | "[::1]"));
    if !secure && !loopback_http {
        return Err(
            "browser cookie sync requires HTTPS, except for loopback development".to_owned(),
        );
    }
    Ok(url.origin().ascii_serialization())
}

#[derive(Serialize)]
struct ChatgptCredentialImport {
    access_token: String,
    refresh_token: String,
    account_id: String,
    expires_at: u64,
    fedramp: bool,
}

impl fmt::Debug for ChatgptCredentialImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatgptCredentialImport")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("account_id", &self.account_id)
            .field("expires_at", &self.expires_at)
            .field("fedramp", &self.fedramp)
            .finish()
    }
}

impl ChatgptCredentialImport {
    fn resource(&self) -> String {
        let mut commitment = Sha256::new();
        commitment.update(CHATGPT_IMPORT_DOMAIN);
        update_length_prefixed(&mut commitment, self.access_token.as_bytes());
        update_length_prefixed(&mut commitment, self.refresh_token.as_bytes());
        update_length_prefixed(&mut commitment, self.account_id.as_bytes());
        commitment.update(self.expires_at.to_be_bytes());
        commitment.update([u8::from(self.fedramp)]);
        format!(
            "{CHATGPT_IMPORT_RESOURCE_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(commitment.finalize())
        )
    }
}

fn update_length_prefixed(commitment: &mut Sha256, value: &[u8]) {
    // All imported values are bounded far below u32::MAX before this point.
    commitment.update((value.len() as u32).to_be_bytes());
    commitment.update(value);
}

#[derive(Deserialize)]
struct CodexAuthImportFile {
    auth_mode: Option<String>,
    tokens: Option<CodexAuthImportTokens>,
}

#[derive(Deserialize)]
struct CodexAuthImportTokens {
    id_token: String,
    access_token: String,
    refresh_token: String,
    account_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChatgptJwtClaims {
    account_id: Option<String>,
    fedramp: Option<bool>,
    exp: Option<u64>,
}

fn load_chatgpt_credential_import(path: &Path) -> Result<ChatgptCredentialImport> {
    let mut file = open_codex_auth_file(path)?;
    let metadata = file
        .metadata()
        .wrap_err_with(|| format!("failed to inspect Codex auth file {}", path.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "Codex auth file {} is not a regular file",
        path.display()
    );
    validate_codex_auth_file_metadata(&metadata, path)?;
    ensure!(
        metadata.len() <= CHATGPT_AUTH_FILE_LIMIT,
        "Codex auth file {} exceeds 64 KiB",
        path.display()
    );

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(CHATGPT_AUTH_FILE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .wrap_err_with(|| format!("failed to read Codex auth file {}", path.display()))?;
    ensure!(
        bytes.len() as u64 <= CHATGPT_AUTH_FILE_LIMIT,
        "Codex auth file {} exceeds 64 KiB",
        path.display()
    );
    let auth: CodexAuthImportFile = serde_json::from_slice(&bytes)
        .map_err(|_| eyre!("Codex auth file {} is not valid JSON", path.display()))?;
    ensure!(
        auth.auth_mode.as_deref() == Some("chatgpt"),
        "Codex auth file is not a ChatGPT login"
    );
    let tokens = auth
        .tokens
        .ok_or_else(|| eyre!("Codex auth file contains no ChatGPT tokens"))?;
    validate_secret_token("ID", &tokens.id_token)?;
    validate_secret_token("access", &tokens.access_token)?;
    validate_secret_token("refresh", &tokens.refresh_token)?;

    let id_claims = decode_chatgpt_jwt_claims("ID", &tokens.id_token)?;
    let access_claims = decode_chatgpt_jwt_claims("access", &tokens.access_token)?;
    let account_id = tokens
        .account_id
        .ok_or_else(|| eyre!("Codex ChatGPT tokens contain no account ID"))?;
    ensure_bounded_account_id(&account_id)?;
    ensure!(
        id_claims.account_id.as_deref() == Some(account_id.as_str())
            && access_claims.account_id.as_deref() == Some(account_id.as_str()),
        "Codex ChatGPT account claims are missing or inconsistent"
    );

    let id_fedramp = id_claims.fedramp.unwrap_or(false);
    let access_fedramp = access_claims.fedramp.unwrap_or(false);
    ensure!(
        id_fedramp == access_fedramp,
        "Codex ChatGPT FedRAMP claims are inconsistent"
    );

    let expires_at = access_claims
        .exp
        .ok_or_else(|| eyre!("Codex ChatGPT access token has no valid expiry"))?
        .checked_mul(1_000)
        .ok_or_else(|| eyre!("Codex ChatGPT access token expiry is invalid"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .wrap_err("system clock is before the Unix epoch")?;
    let minimum_expiry = u64::try_from(
        now.checked_add(CHATGPT_IMPORT_EXPIRY_MARGIN)
            .ok_or_else(|| eyre!("system time overflowed the ChatGPT import margin"))?
            .as_millis(),
    )
    .wrap_err("system time overflowed milliseconds")?;
    ensure!(
        expires_at > minimum_expiry,
        "Codex ChatGPT access token expires too soon; refresh it before connecting"
    );

    Ok(ChatgptCredentialImport {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        account_id,
        expires_at,
        fedramp: access_fedramp,
    })
}

fn validate_secret_token(kind: &str, token: &str) -> Result<()> {
    ensure!(
        !token.is_empty()
            && token.trim() == token
            && token.len() <= CHATGPT_TOKEN_LIMIT
            && !token.chars().any(char::is_control),
        "Codex ChatGPT {kind} token is missing or invalid"
    );
    Ok(())
}

fn ensure_bounded_account_id(account_id: &str) -> Result<()> {
    ensure!(
        !account_id.is_empty()
            && account_id.trim() == account_id
            && account_id.len() <= CHATGPT_ACCOUNT_ID_LIMIT
            && !account_id.chars().any(char::is_control),
        "Codex ChatGPT account ID is missing or invalid"
    );
    Ok(())
}

fn decode_chatgpt_jwt_claims(kind: &str, token: &str) -> Result<ChatgptJwtClaims> {
    let mut segments = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        bail!("Codex ChatGPT {kind} token is not a three-segment JWT");
    };
    ensure!(
        !header.is_empty() && !payload.is_empty() && !signature.is_empty(),
        "Codex ChatGPT {kind} token is not a three-segment JWT"
    );
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| eyre!("Codex ChatGPT {kind} token has an invalid JWT payload"))?;
    let payload: serde_json::Map<String, Value> = serde_json::from_slice(&payload)
        .map_err(|_| eyre!("Codex ChatGPT {kind} token has an invalid JWT payload"))?;
    let exp = match payload.get("exp") {
        Some(value) => Some(
            value
                .as_u64()
                .ok_or_else(|| eyre!("Codex ChatGPT {kind} token has an invalid expiry"))?,
        ),
        None => None,
    };
    let Some(auth) = payload.get("https://api.openai.com/auth") else {
        return Ok(ChatgptJwtClaims {
            account_id: None,
            fedramp: None,
            exp,
        });
    };
    let auth = auth
        .as_object()
        .ok_or_else(|| eyre!("Codex ChatGPT {kind} token has invalid account claims"))?;
    let account_id = match auth.get("chatgpt_account_id") {
        Some(Value::String(value)) => {
            ensure_bounded_account_id(value)?;
            Some(value.clone())
        }
        Some(_) => bail!("Codex ChatGPT {kind} token has an invalid account claim"),
        None => None,
    };
    let fedramp = match auth.get("chatgpt_account_is_fedramp") {
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => bail!("Codex ChatGPT {kind} token has an invalid FedRAMP claim"),
        None => None,
    };
    Ok(ChatgptJwtClaims {
        account_id,
        fedramp,
        exp,
    })
}

fn open_codex_auth_file(path: &Path) -> Result<fs::File> {
    #[cfg(unix)]
    {
        use nix::{
            fcntl::{OFlag, open},
            sys::stat::Mode,
        };

        let descriptor = open(
            path,
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)
        .wrap_err_with(|| format!("failed to securely open Codex auth file {}", path.display()))?;
        Ok(fs::File::from(descriptor))
    }
    #[cfg(not(unix))]
    {
        fs::File::open(path)
            .wrap_err_with(|| format!("failed to open Codex auth file {}", path.display()))
    }
}

#[cfg(unix)]
fn validate_codex_auth_file_metadata(metadata: &fs::Metadata, path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    ensure!(
        metadata.uid() == nix::unistd::geteuid().as_raw(),
        "Codex auth file {} is not owned by the current user",
        path.display()
    );
    let mode = metadata.permissions().mode() & 0o777;
    ensure!(
        mode & 0o077 == 0,
        "Codex auth file {} must not be accessible by group or other users",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn validate_codex_auth_file_metadata(_metadata: &fs::Metadata, _path: &Path) -> Result<()> {
    Ok(())
}

struct LoginFlow {
    http: Client,
    device_base: Url,
    api_origin: Url,
    paths: LoginPaths,
    overall_timeout: Duration,
    poll_second: Duration,
    open_browser: bool,
    chatgpt_credential_import: Option<ChatgptCredentialImport>,
    #[cfg(test)]
    fail_login_persistence: bool,
}

impl LoginFlow {
    fn new(device_base: Url, paths: LoginPaths, open_browser: bool) -> Result<Self> {
        Ok(Self {
            http: http_client()?,
            api_origin: api_origin(&device_base)?,
            device_base,
            paths,
            overall_timeout: OVERALL_TIMEOUT,
            poll_second: Duration::from_secs(1),
            open_browser,
            chatgpt_credential_import: None,
            #[cfg(test)]
            fail_login_persistence: false,
        })
    }

    fn with_chatgpt_credential_import(
        mut self,
        credential_import: Option<ChatgptCredentialImport>,
    ) -> Self {
        self.chatgpt_credential_import = credential_import;
        self
    }

    async fn complete(
        self,
        mut request: RequestedCapabilities,
        retirement: Option<PendingRetirement>,
    ) -> Result<()> {
        ensure!(
            self.chatgpt_credential_import.is_none()
                || request.connectors.contains(&Connector::Chatgpt.id()),
            "ChatGPT credential import requires an explicit ChatGPT connection request"
        );
        let created_mcp_connections = self.create_mcp_intents(&request.mcp_targets).await?;
        for connection in created_mcp_connections {
            if !request
                .mcp_connections
                .iter()
                .any(|existing| existing.id == connection.id)
            {
                request.mcp_connections.push(connection);
            }
        }
        let signer = PrivateKeySigner::random();
        let requested_expiry = unix_timestamp()?
            .checked_add(ACCESS_KEY_LIFETIME)
            .ok_or_else(|| eyre!("system time overflowed the access-key expiry"))?;
        let verifier = pkce_verifier();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let import_resource = self
            .chatgpt_credential_import
            .as_ref()
            .map(ChatgptCredentialImport::resource);
        let rpc = wallet_connect_request(
            &self.api_origin,
            &request,
            &signer,
            requested_expiry,
            import_resource.as_deref(),
        )?;
        let registration = self.register(&challenge, rpc).await?;
        print_verification_prompt(&registration);
        if self.open_browser
            && let Some(url) = registration
                .verification_uri_complete
                .as_deref()
                .or(Some(registration.verification_uri.as_str()))
            && let Err(error) = open_browser(url)
        {
            eprintln!("Could not open a browser automatically ({error}). Use the URL above.");
        }
        let result = self.poll(&registration, &verifier).await?;
        let approved = validate_wallet_result(result, &signer, requested_expiry, &request)?;
        let connection = self.create_connection(&approved, &request).await?;
        let mut stored =
            StoredLogin::from_connection(self.api_origin.as_str(), connection.clone())?;
        stored.pending_retirement = retirement;
        if let Err(error) = validate_connection(&connection, &approved, &request) {
            let _ = revoke_grant(&stored).await;
            return Err(error);
        }
        let previous_login = match StoredLogin::load(&self.paths.login) {
            Ok(previous) => previous,
            Err(error) => {
                let _ = revoke_grant(&stored).await;
                return Err(error).wrap_err("failed to preserve the current login for rotation");
            }
        };
        let persistence = (|| -> Result<()> {
            if let ApprovedWalletResult::AccessKey(approved) = &approved {
                TempoAccountsStore::at(&self.paths.accounts)
                    .upsert_secp256k1_access_key(
                        approved.account,
                        &signer,
                        &approved.signed_authorization,
                    )
                    .wrap_err("failed to persist the local Tempo installation key")?;
            }
            stored.store(&self.paths.login)?;
            #[cfg(test)]
            ensure!(
                !self.fail_login_persistence,
                "injected replacement login persistence failure"
            );
            Ok(())
        })();
        if let Err(error) = persistence {
            let rollback = match &previous_login {
                Some(previous) => previous.store(&self.paths.login),
                None => remove_login(&self.paths.login),
            };
            if let Err(rollback) = rollback {
                return Err(error).wrap_err_with(|| {
                    format!(
                        "replacement persistence failed and the prior login could not be restored: {rollback:#}"
                    )
                });
            }
            if self.paths.accounts.exists()
                && let ApprovedWalletResult::AccessKey(approved) = &approved
            {
                let _ = TempoAccountsStore::at(&self.paths.accounts).retire_access_key(
                    approved.account,
                    CHAIN_ID,
                    approved.signed_authorization.key_id,
                );
            }
            let _ = revoke_grant(&stored).await;
            return Err(error);
        }
        finish_pending_retirement(&self.paths, &mut stored)
            .await
            .wrap_err("replacement login is active, but prior login cleanup is still pending")?;
        print_summary(&stored);
        Ok(())
    }

    async fn create_mcp_intents(&self, targets: &[String]) -> Result<Vec<ManagedMcpConnection>> {
        let url = self
            .api_origin
            .join("/v1/mcp-intents")
            .wrap_err("invalid MCP intent URL")?;
        let mut connections = Vec::with_capacity(targets.len());
        for target in targets {
            let response = self
                .http
                .post(url.clone())
                .header("origin", APP_ORIGIN)
                .header("x-nanocodex-app-id", APP_ID)
                .json(&json!({ "target": target }))
                .send()
                .await
                .wrap_err("MCP intent request failed")?;
            require_response_url(&response, &url)?;
            ensure!(
                matches!(response.status(), StatusCode::OK | StatusCode::CREATED),
                "MCP intent request failed with HTTP {}",
                response.status()
            );
            let connection: ManagedMcpConnection = response_json(response).await?;
            connection.validate()?;
            ensure!(
                !connections
                    .iter()
                    .any(|existing: &ManagedMcpConnection| existing.id == connection.id),
                "Connect returned a duplicate MCP connection ID"
            );
            connections.push(connection);
        }
        Ok(connections)
    }

    async fn register(&self, challenge: &str, rpc: Value) -> Result<DeviceRegistration> {
        let url = self
            .device_base
            .join("register")
            .wrap_err("invalid device registration URL")?;
        let response = self
            .http
            .post(url.clone())
            .json(&json!({
                "code_challenge": challenge,
                "code_challenge_method": "S256",
                "message": { "type": "rpc-requests", "payload": [rpc] },
                "meta": {
                    "name": "Nanocodex CLI",
                    "description": "First-party Nanocodex command-line agent",
                    "website_url": APP_ORIGIN,
                },
                "consumer_url": APP_ORIGIN,
            }))
            .send()
            .await
            .wrap_err("device registration request failed")?;
        require_response_url(&response, &url)?;
        if response.status() != StatusCode::OK {
            let status = response.status();
            let detail = response_json::<Value>(response)
                .await
                .ok()
                .and_then(|body| {
                    body.pointer("/error/message")
                        .or_else(|| body.get("error_description"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            bail!(
                "device registration failed with HTTP {status}{}",
                detail
                    .as_deref()
                    .map(|message| format!(": {message}"))
                    .unwrap_or_default()
            );
        }
        let registration: DeviceRegistration = response_json(response).await?;
        registration.validate(&self.device_base)?;
        Ok(registration)
    }

    async fn poll(&self, registration: &DeviceRegistration, verifier: &str) -> Result<Value> {
        let url = self
            .device_base
            .join("token")
            .wrap_err("invalid device token URL")?;
        let started = tokio::time::Instant::now();
        let deadline = started
            + self
                .overall_timeout
                .min(Duration::from_secs(registration.expires_in));
        let mut delay = self
            .poll_second
            .saturating_mul(registration.interval as u32);
        let mut slow_down_streak = 0_u8;
        loop {
            ensure!(
                tokio::time::Instant::now() < deadline,
                "Nanocodex login timed out; restart login and use {}",
                registration.verification_uri
            );
            let response = self
                .http
                .post(url.clone())
                .json(&json!({
                    "code_verifier": verifier,
                    "device_code": registration.device_code,
                    "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                }))
                .send()
                .await
                .wrap_err("device token request failed")?;
            require_response_url(&response, &url)?;
            match response.status() {
                StatusCode::OK => {
                    let envelope: RpcResponseEnvelope = response_json(response).await?;
                    return envelope.into_result();
                }
                StatusCode::BAD_REQUEST => {
                    let error: DeviceError = response_json(response).await?;
                    match error.error.as_str() {
                        "authorization_pending" => {
                            slow_down_streak = 0;
                        }
                        "slow_down" => {
                            slow_down_streak = slow_down_streak.saturating_add(1);
                            ensure!(
                                slow_down_streak < MAX_SLOW_DOWN_STREAK,
                                "device authorization repeatedly asked the CLI to slow down"
                            );
                            delay = delay.saturating_add(self.poll_second.saturating_mul(5));
                        }
                        "access_denied" => bail!("Nanocodex login was denied"),
                        "expired_token" => bail!("Nanocodex login code expired"),
                        "invalid_grant" => bail!("device authorization rejected the PKCE proof"),
                        other => bail!("device authorization failed with {other}"),
                    }
                }
                status => bail!("device token request failed with HTTP {status}"),
            }
            tokio::time::sleep_until((tokio::time::Instant::now() + delay).min(deadline)).await;
        }
    }

    async fn create_connection(
        &self,
        approved: &ApprovedWalletResult,
        request: &RequestedCapabilities,
    ) -> Result<ConnectionResponse> {
        let url = self
            .api_origin
            .join("/v1/connections")
            .wrap_err("invalid Connect grant URL")?;
        let deadline = tokio::time::Instant::now() + self.overall_timeout;
        loop {
            let mut body = match approved {
                ApprovedWalletResult::AccessKey(approved) => json!({
                    "authorization_mode": "access_key",
                    "app_id": APP_ID,
                    "account_address": approved.account,
                    "approval_id": approved.approval_id,
                    "key_authorization": approved.flat_authorization,
                    "signed_key_authorization": approved.serialized_authorization,
                    "permission": "agent.run",
                    "requested_connectors": request.connectors,
                    "requested_mcp_connections": request
                        .mcp_connections
                        .iter()
                        .map(|connection| connection.id.as_str())
                        .collect::<Vec<_>>(),
                }),
                ApprovedWalletResult::Hosted(approved) => json!({
                    "authorization_mode": "hosted",
                    "app_id": APP_ID,
                    "account_address": approved.account,
                    "approval_id": approved.approval_id,
                    "permission": "agent.run",
                    "requested_connectors": request.connectors,
                    "requested_mcp_connections": request
                        .mcp_connections
                        .iter()
                        .map(|connection| connection.id.as_str())
                        .collect::<Vec<_>>(),
                }),
            };
            if let Some(credential_import) = &self.chatgpt_credential_import {
                body.as_object_mut()
                    .ok_or_else(|| eyre!("invalid Connect grant request"))?
                    .insert(
                        "chatgpt_credential_import".to_owned(),
                        serde_json::to_value(credential_import)
                            .wrap_err("failed to encode ChatGPT credential import")?,
                    );
            }
            let response = self
                .http
                .post(url.clone())
                .header("origin", APP_ORIGIN)
                .header("x-nanocodex-app-id", APP_ID)
                .json(&body)
                .send()
                .await
                .wrap_err("Connect grant request failed")?;
            require_response_url(&response, &url)?;
            if response.status() == StatusCode::CREATED {
                return response_json(response).await;
            }
            let status = response.status();
            let body = response_json::<Value>(response)
                .await
                .unwrap_or(Value::Null);
            let connector_pending = status == StatusCode::FORBIDDEN
                && body.pointer("/error/code").and_then(Value::as_str)
                    == Some("connector_not_connected");
            if connector_pending && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(self.poll_second).await;
                continue;
            }
            let detail = if self.chatgpt_credential_import.is_some() {
                String::new()
            } else {
                body.pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(|message| format!(": {message}"))
                    .unwrap_or_default()
            };
            bail!("Connect grant request failed with HTTP {status}{detail}");
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceRegistration {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: u64,
}

impl DeviceRegistration {
    fn validate(&self, device_base: &Url) -> Result<()> {
        ensure!(is_opaque(&self.device_code, 16, 256), "invalid device code");
        ensure!(
            self.user_code.len() >= 4
                && self.user_code.len() <= 32
                && self
                    .user_code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "invalid user code"
        );
        ensure!(
            (1..=3600).contains(&self.expires_in),
            "invalid device-code expiry"
        );
        ensure!(
            (1..=60).contains(&self.interval),
            "invalid polling interval"
        );
        let verification =
            Url::parse(&self.verification_uri).wrap_err("invalid device verification URL")?;
        ensure!(
            verification.username().is_empty() && verification.password().is_none(),
            "verification URL cannot contain credentials"
        );
        require_verification_origin(&verification, device_base)?;
        if let Some(complete) = &self.verification_uri_complete {
            let complete = Url::parse(complete).wrap_err("invalid complete verification URL")?;
            ensure!(
                complete.username().is_empty() && complete.password().is_none(),
                "complete verification URL cannot contain credentials"
            );
            ensure!(
                same_origin(&verification, &complete),
                "verification URLs changed origin"
            );
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceError {
    error: String,
    #[allow(dead_code)]
    error_description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcResponseEnvelope {
    #[serde(rename = "type")]
    kind: String,
    payload: Vec<JsonRpcResponse>,
}

impl RpcResponseEnvelope {
    fn into_result(mut self) -> Result<Value> {
        ensure!(
            self.kind == "rpc-responses",
            "invalid device response envelope"
        );
        ensure!(
            self.payload.len() == 1,
            "device response must contain one RPC result"
        );
        let response = self.payload.pop().expect("length checked");
        ensure!(response.jsonrpc == "2.0", "invalid JSON-RPC version");
        ensure!(
            response.id == RPC_ID,
            "device response JSON-RPC ID did not match"
        );
        if let Some(error) = response.error {
            bail!("wallet_connect failed ({}): {}", error.code, error.message);
        }
        response
            .result
            .ok_or_else(|| eyre!("wallet_connect response contained no result"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: String,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[allow(dead_code)]
    data: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WalletConnectResult {
    accounts: Vec<WalletAccount>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WalletAccount {
    address: Address,
    capabilities: WalletCapabilities,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WalletCapabilities {
    AccessKey(AccessKeyWalletCapabilities),
    Hosted(HostedWalletCapabilities),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AccessKeyWalletCapabilities {
    auth: AccessKeyWalletAuth,
    key_authorization: Value,
    personal_sign: PersonalSign,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessKeyWalletAuth {
    approval_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostedWalletCapabilities {
    auth: HostedWalletAuth,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostedWalletAuth {
    approval_id: String,
    #[serde(rename = "mode")]
    _mode: HostedAuthorizationMode,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum HostedAuthorizationMode {
    Hosted,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersonalSign {
    key_authorization: String,
}

struct ApprovedAccessKeyResult {
    account: Address,
    approval_id: String,
    flat_authorization: Value,
    serialized_authorization: String,
    signed_authorization: SignedKeyAuthorization,
}

struct ApprovedHostedResult {
    account: Address,
    approval_id: String,
}

enum ApprovedWalletResult {
    AccessKey(ApprovedAccessKeyResult),
    Hosted(ApprovedHostedResult),
}

impl ApprovedWalletResult {
    const fn account(&self) -> Address {
        match self {
            Self::AccessKey(approved) => approved.account,
            Self::Hosted(approved) => approved.account,
        }
    }
}

fn validate_wallet_result(
    value: Value,
    signer: &PrivateKeySigner,
    requested_expiry: u64,
    requested: &RequestedCapabilities,
) -> Result<ApprovedWalletResult> {
    let mut result: WalletConnectResult =
        serde_json::from_value(value).wrap_err("invalid wallet_connect result")?;
    ensure!(
        result.accounts.len() == 1,
        "wallet_connect must return one account"
    );
    let account = result.accounts.pop().expect("length checked");
    match account.capabilities {
        WalletCapabilities::AccessKey(capabilities) => {
            ensure!(
                is_opaque(&capabilities.auth.approval_id, 16, 256),
                "invalid Connect approval identifier"
            );
            let flat: TempoAccountsKeyAuthorization =
                serde_json::from_value(capabilities.key_authorization.clone())
                    .wrap_err("invalid flat Tempo key authorization")?;
            let serialized = capabilities.personal_sign.key_authorization;
            ensure!(
                serialized.starts_with("0x") && serialized.len() <= 16 * 1024,
                "invalid serialized Tempo key authorization"
            );
            let bytes = hex::decode(&serialized[2..]).wrap_err("invalid authorization hex")?;
            let signed: SignedKeyAuthorization = alloy_rlp::decode_exact(bytes)
                .wrap_err("invalid signed Tempo key authorization RLP")?;
            validate_signed_authorization(
                &signed,
                flat.as_signed(),
                account.address,
                signer,
                requested_expiry,
                requested.mpp,
            )?;
            Ok(ApprovedWalletResult::AccessKey(ApprovedAccessKeyResult {
                account: account.address,
                approval_id: capabilities.auth.approval_id,
                flat_authorization: capabilities.key_authorization,
                serialized_authorization: serialized,
                signed_authorization: signed,
            }))
        }
        WalletCapabilities::Hosted(capabilities) => {
            ensure!(
                !requested.mpp
                    && requested
                        .resources(None)
                        .iter()
                        .any(|resource| resource == "urn:nanocodex:authorization:hosted"),
                "hosted authorization was not requested"
            );
            ensure!(
                is_base64url(&capabilities.auth.approval_id, 43),
                "invalid hosted Connect approval identifier"
            );
            Ok(ApprovedWalletResult::Hosted(ApprovedHostedResult {
                account: account.address,
                approval_id: capabilities.auth.approval_id,
            }))
        }
    }
}

fn validate_signed_authorization(
    signed: &SignedKeyAuthorization,
    flat: &SignedKeyAuthorization,
    account: Address,
    signer: &PrivateKeySigner,
    requested_expiry: u64,
    mpp: bool,
) -> Result<()> {
    let expected_limits = expected_limits(mpp)?;
    let expected_calls = expected_calls(mpp)?;
    ensure!(
        signed.chain_id == CHAIN_ID,
        "authorization has the wrong chain"
    );
    ensure!(
        signed.key_type == SignatureType::Secp256k1,
        "authorization has the wrong key type"
    );
    ensure!(
        signed.key_id == signer.address(),
        "authorization has the wrong installation key"
    );
    ensure!(
        signed.expiry == NonZeroU64::new(requested_expiry),
        "authorization expiry changed"
    );
    ensure!(
        signed.limits == Some(expected_limits),
        "authorization spending limits changed"
    );
    ensure!(
        signed.allowed_calls == Some(expected_calls),
        "authorization call scopes changed"
    );
    ensure!(
        !signed.is_admin,
        "administrative access keys are not accepted"
    );
    ensure!(
        signed.account.is_none_or(|bound| bound == account),
        "authorization targets another account"
    );
    ensure!(
        signed
            .recover_signer()
            .wrap_err("invalid root authorization signature")?
            == account,
        "authorization signer is not the connected account"
    );

    ensure!(
        flat.chain_id == signed.chain_id,
        "flat authorization chain mismatch"
    );
    ensure!(
        flat.key_type == signed.key_type,
        "flat authorization key type mismatch"
    );
    ensure!(
        flat.key_id == signed.key_id,
        "flat authorization key mismatch"
    );
    ensure!(
        flat.expiry == signed.expiry,
        "flat authorization expiry mismatch"
    );
    ensure!(
        flat.limits == signed.limits,
        "flat authorization limits mismatch"
    );
    ensure!(
        flat.signature == signed.signature,
        "flat authorization signature mismatch"
    );
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManagedMcpConnection {
    pub(crate) id: String,
    pub(crate) name: String,
}

impl ManagedMcpConnection {
    fn validate(&self) -> Result<()> {
        ensure!(
            is_base64url(&self.id, 43),
            "invalid MCP connection identifier"
        );
        ensure!(
            !self.name.is_empty()
                && self.name.len() <= 128
                && self.name.trim() == self.name
                && !self.name.chars().any(char::is_control),
            "invalid MCP connection name"
        );
        Ok(())
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionResponse {
    authorization_mode: AuthorizationMode,
    grant_token: String,
    account_id: String,
    account_address: Address,
    agent_id: String,
    grant: ConnectionGrant,
    #[serde(default)]
    mcp_connections: Vec<ManagedMcpConnection>,
    #[serde(default)]
    access_key: Option<ConnectionAccessKey>,
    #[serde(default = "empty_json_object")]
    mpp: Value,
}

fn empty_json_object() -> Value {
    Value::Object(Default::default())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum AuthorizationMode {
    AccessKey,
    Hosted,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionGrant {
    id: String,
    permission: String,
    status: String,
    expires_at: u64,
    capabilities: Vec<String>,
    #[serde(default)]
    connector_connections: HashMap<String, Vec<String>>,
    #[serde(default)]
    mcp_connections: Vec<ManagedMcpConnection>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionAccessKey {
    address: Address,
    chain_id: String,
    key_id: Address,
    key_type: String,
    limits: Vec<ConnectionLimit>,
    scopes: Vec<ConnectionScope>,
    witness: String,
    expiry: u64,
    authorization: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ConnectionLimit {
    token: Address,
    limit: String,
    period: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ConnectionScope {
    address: Address,
    selector: String,
    #[serde(default)]
    recipients: Vec<Address>,
}

fn validate_connection(
    response: &ConnectionResponse,
    approved: &ApprovedWalletResult,
    requested: &RequestedCapabilities,
) -> Result<()> {
    ensure!(
        is_base64url(&response.grant_token, 43),
        "invalid grant token"
    );
    ensure!(
        response.account_address == approved.account(),
        "grant account changed"
    );
    ensure!(
        is_uuid(&response.account_id),
        "invalid Nanocodex account ID"
    );
    ensure!(
        is_opaque(&response.agent_id, 3, 128),
        "invalid hosted agent identifier"
    );
    ensure!(
        response.grant.id.starts_with("0x") && response.grant.id.len() == 66,
        "invalid grant identifier"
    );
    ensure!(
        response.grant.permission == "agent.run",
        "grant permission changed"
    );
    ensure!(
        response.grant.status == "active",
        "Connect returned an inactive grant"
    );
    ensure!(
        response.grant.expires_at > unix_timestamp()?,
        "Connect returned an expired grant"
    );
    ensure!(response.mpp.is_object(), "invalid MPP grant projection");
    validate_exact_mcp_connections(&response.mcp_connections, requested)?;
    if !response.grant.mcp_connections.is_empty() {
        validate_exact_mcp_connections(&response.grant.mcp_connections, requested)?;
        ensure!(
            response.grant.mcp_connections == response.mcp_connections,
            "grant MCP connections changed"
        );
    }

    let capabilities: HashSet<&str> = response
        .grant
        .capabilities
        .iter()
        .map(String::as_str)
        .collect();
    ensure!(
        capabilities.contains("nanocodex.agent"),
        "hosted agent capability is missing"
    );
    for capability in REQUIRED_DATA_CAPABILITIES {
        ensure!(
            capabilities.contains(capability),
            "approved capability {capability} is missing"
        );
    }
    ensure!(
        !capabilities.contains("agent.trace.read"),
        "raw trace access was not requested"
    );
    for connector in &requested.connectors {
        ensure!(
            capabilities.contains(connector),
            "connector {connector} is missing"
        );
    }
    for connector in CONNECTOR_NAMES {
        ensure!(
            requested.connectors.contains(connector) || !capabilities.contains(connector),
            "unrequested connector {connector} was granted"
        );
    }
    for (connector, connection_ids) in &response.grant.connector_connections {
        ensure!(
            requested.connectors.contains(&connector.as_str()),
            "unrequested connector {connector} connection authority was granted"
        );
        let unique: HashSet<&str> = connection_ids.iter().map(String::as_str).collect();
        ensure!(
            !connection_ids.is_empty()
                && unique.len() == connection_ids.len()
                && connection_ids.iter().all(|id| is_base64url(id, 43)),
            "invalid connector {connector} connection authority"
        );
    }
    ensure!(
        requested.mpp == capabilities.contains("mpp.mach"),
        "MPP capability did not match the request"
    );
    if let Some(origin) = &requested.browser_cookie_origin {
        let capability = local_browser_cookie_sync_resource(origin);
        ensure!(
            capabilities.contains(capability.as_str()),
            "browser cookie sync authority for {origin} is missing"
        );
    }
    match (
        approved,
        response.authorization_mode,
        response.access_key.as_ref(),
    ) {
        (
            ApprovedWalletResult::AccessKey(approved),
            AuthorizationMode::AccessKey,
            Some(access_key),
        ) => validate_connection_access_key(access_key, approved, requested, &response.grant)?,
        (ApprovedWalletResult::Hosted(_), AuthorizationMode::Hosted, None) => {
            validate_hosted_connection(response, requested, &capabilities)?;
        }
        _ => bail!("Connect authorization mode changed"),
    }
    Ok(())
}

fn validate_exact_mcp_connections(
    connections: &[ManagedMcpConnection],
    requested: &RequestedCapabilities,
) -> Result<()> {
    for connection in connections {
        connection.validate()?;
    }
    let actual: HashSet<&str> = connections
        .iter()
        .map(|connection| connection.id.as_str())
        .collect();
    let expected: HashSet<&str> = requested
        .mcp_connections
        .iter()
        .map(|connection| connection.id.as_str())
        .collect();
    ensure!(
        connections.len() == actual.len()
            && requested.mcp_connections.len() == expected.len()
            && actual == expected,
        "granted MCP connection IDs changed"
    );
    Ok(())
}

fn validate_connection_access_key(
    access_key: &ConnectionAccessKey,
    approved: &ApprovedAccessKeyResult,
    requested: &RequestedCapabilities,
    grant: &ConnectionGrant,
) -> Result<()> {
    ensure!(
        access_key.address == approved.signed_authorization.key_id,
        "grant key changed"
    );
    ensure!(
        access_key.key_id == approved.signed_authorization.key_id,
        "grant key ID changed"
    );
    ensure!(
        access_key.chain_id == CHAIN_ID.to_string(),
        "grant key chain changed"
    );
    ensure!(access_key.key_type == "secp256k1", "grant key type changed");
    ensure!(
        access_key.expiry == grant.expires_at,
        "grant and key expiry differ"
    );
    ensure!(
        access_key
            .authorization
            .eq_ignore_ascii_case(&approved.serialized_authorization),
        "grant authorization changed"
    );
    ensure!(
        access_key.witness.starts_with("0x") && access_key.witness.len() == 66,
        "invalid authorization witness"
    );
    ensure!(
        access_key.limits == connection_limits(requested.mpp)?,
        "grant limits changed"
    );
    ensure!(
        access_key.scopes == connection_scopes(requested.mpp)?,
        "grant scopes changed"
    );
    Ok(())
}

fn validate_hosted_connection(
    response: &ConnectionResponse,
    requested: &RequestedCapabilities,
    capabilities: &HashSet<&str>,
) -> Result<()> {
    ensure!(!requested.mpp, "hosted authorization cannot grant MPP");
    ensure!(
        !capabilities.contains("mpp.mach"),
        "hosted authorization cannot grant MPP"
    );
    let now = unix_timestamp()?;
    let latest_expiry = now
        .checked_add(ACCESS_KEY_LIFETIME + HOSTED_EXPIRY_CLOCK_SKEW)
        .ok_or_else(|| eyre!("system time overflowed the hosted grant expiry"))?;
    ensure!(
        response.grant.expires_at <= latest_expiry,
        "hosted grant expiry exceeds the requested lifetime"
    );

    let mut expected = HashSet::from(["nanocodex.agent"]);
    let expected_mcp_capabilities: Vec<_> = requested
        .mcp_connections
        .iter()
        .map(|connection| format!("mcp:{}", connection.id))
        .collect();
    expected.extend(REQUIRED_DATA_CAPABILITIES.iter().copied());
    expected.insert("agent.history.read");
    expected.extend(requested.connectors.iter().copied());
    expected.extend(expected_mcp_capabilities.iter().map(String::as_str));
    let expected_cookie_capability = requested
        .browser_cookie_origin
        .as_deref()
        .map(local_browser_cookie_sync_resource);
    expected.extend(expected_cookie_capability.as_deref());
    ensure!(
        response.grant.capabilities.len() == expected.len() && capabilities == &expected,
        "hosted grant capabilities changed"
    );
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredLogin {
    version: u8,
    origin: String,
    grant_token: String,
    grant_id: String,
    account_id: String,
    account_address: Address,
    agent_id: String,
    expires_at: u64,
    capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mcp_connections: Vec<ManagedMcpConnection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_key_id: Option<Address>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_retirement: Option<PendingRetirement>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRetirement {
    origin: String,
    grant_token: String,
    grant_id: String,
    expires_at: u64,
    capabilities: Vec<String>,
    account_address: Address,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_key_id: Option<Address>,
    revoke_grant: bool,
}

impl PendingRetirement {
    fn from_login(login: &StoredLogin, revoke_grant: bool) -> Self {
        Self {
            origin: login.origin.clone(),
            grant_token: login.grant_token.clone(),
            grant_id: login.grant_id.clone(),
            expires_at: login.expires_at,
            capabilities: login.capabilities.clone(),
            account_address: login.account_address,
            access_key_id: login.access_key_id,
            revoke_grant,
        }
    }

    fn validate(&self) -> Result<()> {
        let origin = Url::parse(&self.origin).wrap_err("invalid prior Connect origin")?;
        ensure!(
            normalized_origin(origin)? == self.origin,
            "prior Connect origin is not canonical"
        );
        ensure!(
            is_base64url(&self.grant_token, 43),
            "invalid prior grant token"
        );
        ensure!(
            self.grant_id.starts_with("0x") && self.grant_id.len() == 66,
            "invalid prior grant ID"
        );
        ensure!(
            !self.capabilities.is_empty(),
            "prior grant has no capabilities"
        );
        Ok(())
    }
}

impl fmt::Debug for StoredLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredLogin")
            .field("version", &self.version)
            .field("origin", &self.origin)
            .field("grant_token", &"[REDACTED]")
            .field("grant_id", &self.grant_id)
            .field("account_id", &self.account_id)
            .field("account_address", &self.account_address)
            .field("agent_id", &self.agent_id)
            .field("expires_at", &self.expires_at)
            .field("capabilities", &self.capabilities)
            .field("mcp_connections", &self.mcp_connections)
            .field("access_key_id", &self.access_key_id)
            .field(
                "pending_retirement",
                &self.pending_retirement.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl StoredLogin {
    fn from_connection(origin: &str, response: ConnectionResponse) -> Result<Self> {
        let mut mcp_ids = HashSet::with_capacity(response.mcp_connections.len());
        for connection in &response.mcp_connections {
            connection.validate()?;
            ensure!(
                mcp_ids.insert(connection.id.as_str()),
                "Connect returned a duplicate MCP connection ID"
            );
        }
        if !response.grant.mcp_connections.is_empty() {
            let mut grant_mcp_ids = HashSet::with_capacity(response.grant.mcp_connections.len());
            for connection in &response.grant.mcp_connections {
                connection.validate()?;
                ensure!(
                    grant_mcp_ids.insert(connection.id.as_str()),
                    "Connect grant returned a duplicate MCP connection ID"
                );
            }
            ensure!(
                response.grant.mcp_connections == response.mcp_connections,
                "Connect response and grant MCP connections differ"
            );
        }
        let access_key_id = match (response.authorization_mode, response.access_key.as_ref()) {
            (AuthorizationMode::AccessKey, Some(access_key)) => Some(access_key.key_id),
            (AuthorizationMode::Hosted, None) => None,
            _ => bail!("invalid Connect authorization response"),
        };
        Ok(Self {
            version: 1,
            origin: normalized_origin(Url::parse(origin)?)?,
            grant_token: response.grant_token,
            grant_id: response.grant.id,
            account_id: response.account_id,
            account_address: response.account_address,
            agent_id: response.agent_id,
            expires_at: response.grant.expires_at,
            capabilities: response.grant.capabilities,
            mcp_connections: response.mcp_connections,
            access_key_id,
            pending_retirement: None,
        })
    }

    fn load(path: &Path) -> Result<Option<Self>> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).wrap_err_with(|| format!("failed to open {}", path.display()));
            }
        };
        ensure_owner_only(&file, path)?;
        let bytes =
            fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
        ensure!(bytes.len() <= RESPONSE_LIMIT, "login record is too large");
        let stored: Self =
            serde_json::from_slice(&bytes).wrap_err("invalid Nanocodex login record")?;
        stored.validate()?;
        Ok(Some(stored))
    }

    fn load_if_valid(path: &Path, expected_origin: Option<&str>) -> Result<Option<Self>> {
        let Some(stored) = Self::load(path)? else {
            return Ok(None);
        };
        if stored.expires_at <= unix_timestamp()? {
            return Ok(None);
        }
        if expected_origin.is_some_and(|expected| stored.origin != expected) {
            return Ok(None);
        }
        Ok(Some(stored))
    }

    fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "unsupported login record version");
        let origin = Url::parse(&self.origin).wrap_err("invalid stored Connect origin")?;
        ensure!(
            normalized_origin(origin)? == self.origin,
            "stored Connect origin is not canonical"
        );
        ensure!(
            is_base64url(&self.grant_token, 43),
            "invalid stored grant token"
        );
        ensure!(
            self.grant_id.starts_with("0x") && self.grant_id.len() == 66,
            "invalid stored grant ID"
        );
        ensure!(
            is_uuid(&self.account_id),
            "invalid stored Nanocodex account ID"
        );
        ensure!(is_opaque(&self.agent_id, 3, 128), "invalid stored agent ID");
        ensure!(
            !self.capabilities.is_empty(),
            "stored grant has no capabilities"
        );
        let mut mcp_ids = HashSet::with_capacity(self.mcp_connections.len());
        for connection in &self.mcp_connections {
            connection.validate()?;
            ensure!(
                mcp_ids.insert(connection.id.as_str()),
                "stored grant has a duplicate MCP connection ID"
            );
        }
        if let Some(retirement) = &self.pending_retirement {
            retirement.validate()?;
            ensure!(
                retirement.grant_id != self.grant_id
                    || retirement.access_key_id != self.access_key_id,
                "login cannot retire its own authority"
            );
        }
        Ok(())
    }

    fn satisfies_login(&self, mpp: bool, browser_cookie_origin: Option<&str>) -> bool {
        let capabilities: HashSet<&str> = self.capabilities.iter().map(String::as_str).collect();
        REQUIRED_DATA_CAPABILITIES
            .iter()
            .all(|item| capabilities.contains(item))
            && (!mpp || capabilities.contains("mpp.mach"))
            && browser_cookie_origin.is_none_or(|origin| {
                let resource = local_browser_cookie_sync_resource(origin);
                capabilities.contains(resource.as_str())
            })
    }

    const fn authorization_mode(&self) -> AuthorizationMode {
        if self.access_key_id.is_some() {
            AuthorizationMode::AccessKey
        } else {
            AuthorizationMode::Hosted
        }
    }

    fn store(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path
            .parent()
            .ok_or_else(|| eyre!("login path has no parent"))?;
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        let encoded = serde_json::to_vec(self).wrap_err("failed to encode Nanocodex login")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).wrap_err_with(|| {
            format!("failed to create a temporary file in {}", parent.display())
        })?;
        set_owner_only(temporary.as_file(), temporary.path())?;
        temporary
            .write_all(&encoded)
            .wrap_err("failed to write Nanocodex login")?;
        temporary
            .as_file()
            .sync_all()
            .wrap_err("failed to flush Nanocodex login")?;
        temporary
            .persist(path)
            .map_err(|error| error.error)
            .wrap_err_with(|| format!("failed to replace {}", path.display()))?;
        sync_parent(parent)?;
        Ok(())
    }
}

/// The only credential shape exposed to the hosted history and memory tools.
/// The bearer remains private to this crate interface and is redacted from
/// diagnostics; callers can only use it to address the fixed Connect API.
#[derive(Clone)]
pub(crate) struct ScopedManagedCredential {
    origin: Url,
    grant_token: String,
    grant_id: String,
    account_id: String,
    account_address: Address,
    mcp_connections: Vec<ManagedMcpConnection>,
}

impl fmt::Debug for ScopedManagedCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedManagedCredential")
            .field("origin", &self.origin)
            .field("grant_token", &"[REDACTED]")
            .field("grant_id", &self.grant_id)
            .field("account_id", &self.account_id)
            .field("account_address", &self.account_address)
            .field("mcp_connections", &self.mcp_connections)
            .finish()
    }
}

impl ScopedManagedCredential {
    pub(crate) const fn origin(&self) -> &Url {
        &self.origin
    }

    pub(crate) fn bearer_token(&self) -> &str {
        &self.grant_token
    }

    #[allow(dead_code)]
    pub(crate) fn grant_id(&self) -> &str {
        &self.grant_id
    }

    #[allow(dead_code)]
    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }

    #[allow(dead_code)]
    pub(crate) const fn account_address(&self) -> Address {
        self.account_address
    }

    pub(crate) fn mcp_connections(&self) -> &[ManagedMcpConnection] {
        &self.mcp_connections
    }
}

/// Load connect.json and validate the grant against Connect before handing it
/// to hosted history/memory. No stale local record is accepted.
pub(crate) async fn load_managed_credential(
    codex_home: &Path,
) -> Result<Option<ScopedManagedCredential>> {
    let path = codex_home.join("connect.json");
    let Some(login) = active_login(&path, None).await? else {
        return Ok(None);
    };
    let capabilities: HashSet<&str> = login.capabilities.iter().map(String::as_str).collect();
    for capability in ["history:read", "memory:read", "memory:write"] {
        ensure!(
            capabilities.contains(capability),
            "Nanocodex Connect grant is missing required {capability} capability"
        );
    }
    Ok(Some(ScopedManagedCredential {
        origin: Url::parse(&login.origin).wrap_err("stored Connect origin is invalid")?,
        grant_token: login.grant_token,
        grant_id: login.grant_id,
        account_id: login.account_id,
        account_address: login.account_address,
        mcp_connections: login.mcp_connections,
    }))
}

/// Loads a live CLI grant authorized to sync cookies for one exact origin.
pub(crate) async fn load_browser_cookie_sync_credential(
    codex_home: &Path,
    origin: &str,
) -> Result<Option<ScopedManagedCredential>> {
    let path = codex_home.join("connect.json");
    let Some(login) = active_login(&path, None).await? else {
        return Ok(None);
    };
    let capability = local_browser_cookie_sync_resource(origin);
    ensure!(
        login.capabilities.iter().any(|item| item == &capability),
        "Nanocodex Connect grant is not authorized to sync cookies for {origin}; rerun `nanocodex login --browser-cookie-origin={origin}`"
    );
    Ok(Some(ScopedManagedCredential {
        origin: Url::parse(&login.origin).wrap_err("stored Connect origin is invalid")?,
        grant_token: login.grant_token,
        grant_id: login.grant_id,
        account_id: login.account_id,
        account_address: login.account_address,
        mcp_connections: login.mcp_connections,
    }))
}

/// Loads a live scoped grant only when its local record contains managed MCP
/// connections. A normal login without remote MCP authority does not add a
/// network dependency to native agent startup.
pub(crate) async fn load_managed_mcp_credential(
    codex_home: &Path,
) -> Result<Option<ScopedManagedCredential>> {
    let path = codex_home.join("connect.json");
    let Some(stored) = StoredLogin::load_if_valid(&path, None)? else {
        return Ok(None);
    };
    if stored.mcp_connections.is_empty() {
        return Ok(None);
    }
    let Some(login) = active_login(&path, None).await? else {
        return Ok(None);
    };
    ensure!(
        !login.mcp_connections.is_empty(),
        "live grant removed its managed MCP authority"
    );
    Ok(Some(ScopedManagedCredential {
        origin: Url::parse(&login.origin).wrap_err("stored Connect origin is invalid")?,
        grant_token: login.grant_token,
        grant_id: login.grant_id,
        account_id: login.account_id,
        account_address: login.account_address,
        mcp_connections: login.mcp_connections,
    }))
}

async fn revoke_grant(login: &StoredLogin) -> Result<()> {
    revoke_authority(
        &login.origin,
        &login.grant_token,
        &login.grant_id,
        login.expires_at,
        &login.capabilities,
    )
    .await
}

async fn revoke_authority(
    origin: &str,
    grant_token: &str,
    grant_id: &str,
    expires_at: u64,
    capabilities: &[String],
) -> Result<()> {
    let origin = Url::parse(origin).wrap_err("stored Connect origin is invalid")?;
    let url = origin
        .join(&format!("/v1/grants/{grant_id}/revoke"))
        .wrap_err("invalid grant revocation URL")?;
    let response = http_client()?
        .post(url.clone())
        .header("authorization", format!("Bearer {grant_token}"))
        .header("origin", APP_ORIGIN)
        .header("x-nanocodex-app-id", APP_ID)
        .send()
        .await
        .wrap_err("failed to revoke the Nanocodex grant")?;
    require_response_url(&response, &url)?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND
    ) {
        return Ok(());
    }
    ensure!(
        response.status() == StatusCode::OK,
        "grant revocation failed with HTTP {}",
        response.status()
    );
    let revoked: RevokedGrant = response_json(response).await?;
    ensure!(revoked.id == grant_id, "revoked grant ID changed");
    ensure!(
        revoked.permission == "agent.run",
        "revoked grant permission changed"
    );
    ensure!(
        revoked.status == "revoked",
        "Connect did not revoke the grant"
    );
    ensure!(
        revoked.expires_at == expires_at,
        "revoked grant expiry changed"
    );
    ensure!(
        revoked.capabilities == capabilities,
        "revoked grant capabilities changed"
    );
    Ok(())
}

async fn finish_pending_retirement(paths: &LoginPaths, login: &mut StoredLogin) -> Result<()> {
    let Some(retirement) = login.pending_retirement.as_ref() else {
        return Ok(());
    };
    if paths.accounts.exists()
        && let Some(access_key_id) = retirement.access_key_id
    {
        TempoAccountsStore::at(&paths.accounts)
            .retire_access_key(retirement.account_address, CHAIN_ID, access_key_id)
            .wrap_err("failed to retire the prior local Tempo installation key")?;
    }
    if retirement.revoke_grant && retirement.expires_at > unix_timestamp()? {
        revoke_authority(
            &retirement.origin,
            &retirement.grant_token,
            &retirement.grant_id,
            retirement.expires_at,
            &retirement.capabilities,
        )
        .await
        .wrap_err("failed to revoke the prior Nanocodex grant")?;
    }
    login.pending_retirement = None;
    login
        .store(&paths.login)
        .wrap_err("failed to record completion of prior login cleanup")
}

fn retire_installation_key(accounts_path: &Path, login: &StoredLogin) -> Result<()> {
    if accounts_path.exists()
        && let Some(access_key_id) = login.access_key_id
    {
        TempoAccountsStore::at(accounts_path)
            .retire_access_key(login.account_address, CHAIN_ID, access_key_id)
            .wrap_err("failed to remove the local Tempo installation key")?;
    }
    Ok(())
}

async fn active_login(path: &Path, expected_origin: Option<&str>) -> Result<Option<StoredLogin>> {
    let Some(stored) = StoredLogin::load_if_valid(path, expected_origin)? else {
        return Ok(None);
    };
    let origin = Url::parse(&stored.origin).wrap_err("stored Connect origin is invalid")?;
    let url = origin
        .join(&format!("/v1/grants/{}", stored.grant_id))
        .wrap_err("invalid grant status URL")?;
    let response = http_client()?
        .get(url.clone())
        .header("authorization", format!("Bearer {}", stored.grant_token))
        .header("origin", APP_ORIGIN)
        .header("x-nanocodex-app-id", APP_ID)
        .send()
        .await
        .wrap_err("failed to validate the current Nanocodex login")?;
    require_response_url(&response, &url)?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND
    ) {
        return Ok(None);
    }
    ensure!(
        response.status() == StatusCode::OK,
        "grant status failed with HTTP {}",
        response.status()
    );
    let live: ConnectionResponse = response_json(response).await?;
    if live.grant.status != "active" || live.grant.expires_at <= unix_timestamp()? {
        return Ok(None);
    }
    let mut refreshed = StoredLogin::from_connection(&stored.origin, live)?;
    ensure!(
        refreshed.grant_token == stored.grant_token,
        "live grant token changed"
    );
    ensure!(
        refreshed.grant_id == stored.grant_id,
        "live grant ID changed"
    );
    ensure!(
        refreshed.account_id == stored.account_id,
        "live account ID changed"
    );
    ensure!(
        refreshed.account_address == stored.account_address,
        "live Tempo account changed"
    );
    ensure!(
        refreshed.agent_id == stored.agent_id,
        "live hosted agent changed"
    );
    ensure!(
        refreshed.authorization_mode() == stored.authorization_mode(),
        "live authorization mode changed"
    );
    ensure!(
        refreshed.access_key_id == stored.access_key_id,
        "live installation key changed"
    );
    let stored_mcp_ids: HashSet<&str> = stored
        .mcp_connections
        .iter()
        .map(|connection| connection.id.as_str())
        .collect();
    let refreshed_mcp_ids: HashSet<&str> = refreshed
        .mcp_connections
        .iter()
        .map(|connection| connection.id.as_str())
        .collect();
    ensure!(
        stored.mcp_connections.len() == refreshed.mcp_connections.len()
            && stored_mcp_ids == refreshed_mcp_ids,
        "live MCP connection authority changed"
    );
    refreshed.pending_retirement = stored.pending_retirement;
    refreshed.store(path)?;
    Ok(Some(refreshed))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokedGrant {
    id: String,
    permission: String,
    status: String,
    expires_at: u64,
    capabilities: Vec<String>,
    #[serde(default, rename = "connector_connections")]
    _connector_connections: HashMap<String, Vec<String>>,
    #[serde(default, rename = "mcp_connections")]
    _mcp_connections: Vec<ManagedMcpConnection>,
}

fn wallet_connect_request(
    api_origin: &Url,
    requested: &RequestedCapabilities,
    signer: &PrivateKeySigner,
    expiry: u64,
    chatgpt_import_resource: Option<&str>,
) -> Result<Value> {
    let challenge = api_origin.join("/v1/connect/auth/challenge")?;
    let verify = api_origin.join("/v1/connect/auth")?;
    let logout = api_origin.join("/v1/connect/auth/logout")?;
    let public_key = format!("0x04{}", hex::encode(signer.public_key()));
    let limits = if requested.mpp {
        json!([
            { "token": MACH, "limit": "0x989680", "period": MPP_PERIOD },
            { "token": USDC_E, "limit": "0x989680", "period": MPP_PERIOD },
        ])
    } else {
        json!([
            { "token": MACH, "limit": "0x0", "period": 0 },
            { "token": USDC_E, "limit": "0x0", "period": 0 },
        ])
    };
    let scopes = if requested.mpp {
        json!([
            { "address": USDC_E, "selector": "0xa9059cbb", "recipients": [MERCATOR_SETTLEMENT] },
            { "address": USDC_E, "selector": "0x95777d59", "recipients": [MERCATOR_SETTLEMENT] },
            { "address": MACH, "selector": "0xa9059cbb", "recipients": [MERCATOR_SETTLEMENT] },
            { "address": MACH, "selector": "0x95777d59", "recipients": [MERCATOR_SETTLEMENT] },
            { "address": TIP20_CHANNEL_ESCROW, "selector": "0xedc53b00" },
            { "address": TIP20_CHANNEL_ESCROW, "selector": "0xdc48471e" },
        ])
    } else {
        json!([])
    };
    let access_key = json!({
        "address": signer.address(),
        "publicKey": public_key,
        "keyType": "secp256k1",
        "chainId": CHAIN_ID_HEX,
        "expiry": expiry,
        "limits": limits,
        "scopes": scopes,
    });
    Ok(json!({
        "jsonrpc": "2.0",
        "id": RPC_ID,
        "method": "wallet_connect",
        "params": [{
            "chainId": CHAIN_ID_HEX,
            "capabilities": {
                "auth": {
                    "challenge": challenge,
                    "verify": verify,
                    "logout": logout,
                    "resources": requested.resources(chatgpt_import_resource),
                    "returnToken": false,
                },
                "authorizeAccessKey": access_key,
            },
        }],
    }))
}

fn expected_limits(mpp: bool) -> Result<Vec<TokenLimit>> {
    if !mpp {
        return Ok(vec![
            TokenLimit {
                token: address(MACH)?,
                limit: U256::ZERO,
                period: 0,
            },
            TokenLimit {
                token: address(USDC_E)?,
                limit: U256::ZERO,
                period: 0,
            },
        ]);
    }
    Ok(vec![
        TokenLimit {
            token: address(MACH)?,
            limit: U256::from(MPP_LIMIT),
            period: MPP_PERIOD,
        },
        TokenLimit {
            token: address(USDC_E)?,
            limit: U256::from(MPP_LIMIT),
            period: MPP_PERIOD,
        },
    ])
}

fn expected_calls(mpp: bool) -> Result<Vec<CallScope>> {
    if !mpp {
        return Ok(Vec::new());
    }
    let scope = |target: &str, selector: [u8; 4], recipients: &[&str]| -> Result<CallScope> {
        Ok(CallScope {
            target: address(target)?,
            selector_rules: vec![SelectorRule {
                selector,
                recipients: recipients
                    .iter()
                    .map(|value| address(value))
                    .collect::<Result<_>>()?,
            }],
        })
    };
    Ok(vec![
        scope(USDC_E, [0xa9, 0x05, 0x9c, 0xbb], &[MERCATOR_SETTLEMENT])?,
        scope(USDC_E, [0x95, 0x77, 0x7d, 0x59], &[MERCATOR_SETTLEMENT])?,
        scope(MACH, [0xa9, 0x05, 0x9c, 0xbb], &[MERCATOR_SETTLEMENT])?,
        scope(MACH, [0x95, 0x77, 0x7d, 0x59], &[MERCATOR_SETTLEMENT])?,
        scope(TIP20_CHANNEL_ESCROW, [0xed, 0xc5, 0x3b, 0x00], &[])?,
        scope(TIP20_CHANNEL_ESCROW, [0xdc, 0x48, 0x47, 0x1e], &[])?,
    ])
}

fn connection_limits(mpp: bool) -> Result<Vec<ConnectionLimit>> {
    if !mpp {
        return Ok(vec![
            ConnectionLimit {
                token: address(MACH)?,
                limit: "0".to_owned(),
                period: 0,
            },
            ConnectionLimit {
                token: address(USDC_E)?,
                limit: "0".to_owned(),
                period: 0,
            },
        ]);
    }
    Ok(vec![
        ConnectionLimit {
            token: address(MACH)?,
            limit: MPP_LIMIT.to_string(),
            period: MPP_PERIOD,
        },
        ConnectionLimit {
            token: address(USDC_E)?,
            limit: MPP_LIMIT.to_string(),
            period: MPP_PERIOD,
        },
    ])
}

fn connection_scopes(mpp: bool) -> Result<Vec<ConnectionScope>> {
    if !mpp {
        return Ok(Vec::new());
    }
    let scope = |target: &str, selector: &str, recipients: &[&str]| -> Result<ConnectionScope> {
        Ok(ConnectionScope {
            address: address(target)?,
            selector: selector.to_owned(),
            recipients: recipients
                .iter()
                .map(|value| address(value))
                .collect::<Result<_>>()?,
        })
    };
    Ok(vec![
        scope(USDC_E, "0xa9059cbb", &[MERCATOR_SETTLEMENT])?,
        scope(USDC_E, "0x95777d59", &[MERCATOR_SETTLEMENT])?,
        scope(MACH, "0xa9059cbb", &[MERCATOR_SETTLEMENT])?,
        scope(MACH, "0x95777d59", &[MERCATOR_SETTLEMENT])?,
        scope(TIP20_CHANNEL_ESCROW, "0xedc53b00", &[])?,
        scope(TIP20_CHANNEL_ESCROW, "0xdc48471e", &[])?,
    ])
}

fn print_verification_prompt(registration: &DeviceRegistration) {
    println!("Authorize this Nanocodex installation:\n");
    println!("Code  {}", registration.user_code);
    println!(
        "URL   {}\n",
        registration
            .verification_uri_complete
            .as_deref()
            .unwrap_or(&registration.verification_uri)
    );
}

fn print_summary(login: &StoredLogin) {
    let capabilities: HashSet<&str> = login.capabilities.iter().map(String::as_str).collect();
    println!("Logged in to Nanocodex\n");
    println!("Account       {}", login.account_id);
    println!(
        "Tempo account {}",
        abbreviated_address(login.account_address)
    );
    println!("Hosted agent  {}", login.agent_id);
    for connector in CONNECTOR_NAMES {
        println!(
            "{:<13}{}",
            connector_label(connector),
            if capabilities.contains(connector) {
                "connected"
            } else {
                "not requested"
            }
        );
    }
    println!(
        "History       {}",
        enabled(capabilities.contains("history:read"))
    );
    println!(
        "Memory        {}",
        enabled(capabilities.contains("memory:read") && capabilities.contains("memory:write"))
    );
    println!(
        "MPP           {}",
        enabled(capabilities.contains("mpp.mach"))
    );
    println!("Grant expires {}", format_utc(login.expires_at));
}

fn connector_label(name: &str) -> &str {
    match name {
        "chatgpt" => "ChatGPT",
        "github" => "GitHub",
        "gmail" => "Gmail",
        "gdrive" => "Google Drive",
        "x" => "X",
        _ => name,
    }
}

const fn enabled(value: bool) -> &'static str {
    if value { "enabled" } else { "disabled" }
}

fn abbreviated_address(address: Address) -> String {
    let full = format!("{address:#x}");
    format!("{}…{}", &full[..10], &full[full.len() - 4..])
}

fn validated_device_base(override_url: Option<&str>) -> Result<Url> {
    let raw = override_url.unwrap_or(DEFAULT_DEVICE_BASE);
    let mut url = Url::parse(raw).wrap_err("invalid device-code base URL")?;
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "device-code base URL cannot contain a query or fragment"
    );
    let production = raw == DEFAULT_DEVICE_BASE;
    let canonical_local =
        is_local_nanocodex_origin(&url) && matches!(url.path(), "/v1/device" | "/v1/device/");
    if production {
        ensure!(
            url.scheme() == "https",
            "production device-code URL must use HTTPS"
        );
    } else {
        ensure!(
            canonical_local,
            "device-code override must use a trusted local Nanocodex /v1/device origin"
        );
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn api_origin(device_base: &Url) -> Result<Url> {
    let mut origin = device_base.clone();
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    Ok(origin)
}

fn normalized_origin(mut url: Url) -> Result<String> {
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Connect origin cannot contain credentials"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "Connect origin cannot contain query state"
    );
    if url.as_str().starts_with(PRODUCTION_API_ORIGIN) {
        ensure!(
            url.origin().ascii_serialization() == PRODUCTION_API_ORIGIN,
            "invalid production Connect origin"
        );
    } else if is_local_nanocodex_origin(&url) {
    } else {
        if url.host_str().is_some_and(is_loopback_host) && matches!(url.scheme(), "http" | "https")
        {
            url.set_path("/");
            return Ok(url.origin().ascii_serialization());
        }
        bail!("stored Connect origin is not trusted");
    }
    url.set_path("/");
    Ok(url.origin().ascii_serialization())
}

fn is_local_nanocodex_origin(url: &Url) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let trusted_label = |suffix: &str| {
        host.strip_suffix(suffix)
            .is_some_and(|label| !label.is_empty() && !label.contains('.'))
    };
    (url.scheme() == "https"
        && url.port().is_none()
        && (host == "nanocodex.local"
            || trusted_label(".nanocodex.local")
            || host == "nanocodex.localhost"
            || trusted_label(".nanocodex.localhost")))
        || (url.scheme() == "http"
            && (host == "nanocodex.localhost" || trusted_label(".nanocodex.localhost")))
}

fn require_verification_origin(url: &Url, device_base: &Url) -> Result<()> {
    if device_base.as_str().starts_with(DEFAULT_DEVICE_BASE) {
        let origin = url.origin().ascii_serialization();
        ensure!(
            url.scheme() == "https"
                && matches!(
                    origin.as_str(),
                    PRODUCTION_API_ORIGIN | PRODUCTION_VERIFY_ORIGIN
                ),
            "untrusted production verification origin"
        );
    } else {
        ensure!(
            same_origin(url, device_base),
            "local verification URL changed origin"
        );
    }
    Ok(())
}

fn http_client() -> Result<Client> {
    Client::builder()
        .redirect(Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("nanocodex-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .wrap_err("failed to build Connect HTTP client")
}

async fn response_json<T: DeserializeOwned>(response: Response) -> Result<T> {
    if let Some(length) = response.content_length() {
        ensure!(
            length <= RESPONSE_LIMIT as u64,
            "Connect response is too large"
        );
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.wrap_err("failed to read Connect response")?;
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= RESPONSE_LIMIT,
            "Connect response is too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).wrap_err("Connect returned invalid JSON")
}

fn require_response_url(response: &Response, expected: &Url) -> Result<()> {
    ensure!(
        response.url() == expected,
        "Connect response changed URL or origin"
    );
    Ok(())
}

fn pkce_verifier() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unix_timestamp() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .wrap_err("system clock is before Unix epoch")?
        .as_secs())
}

fn address(value: &str) -> Result<Address> {
    Address::from_str(value).wrap_err_with(|| format!("invalid policy address {value}"))
}

fn is_loopback_host(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.origin() == right.origin()
}

fn is_base64url(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn is_opaque(value: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn format_utc(timestamp: u64) -> String {
    let days = (timestamp / 86_400) as i64;
    let seconds = timestamp % 86_400;
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = seconds / 3_600;
    let minute = seconds % 3_600 / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

fn remove_login(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                sync_parent(parent)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).wrap_err_with(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(unix)]
fn ensure_owner_only(file: &fs::File, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = file.metadata()?.permissions().mode() & 0o777;
    ensure!(
        mode == 0o600,
        "login file {} must have Unix permissions 0600 (found {mode:04o})",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only(_file: &fs::File, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(file: &fs::File, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .wrap_err_with(|| format!("failed to protect {}", path.display()))
}

#[cfg(not(unix))]
fn set_owner_only(_file: &fs::File, _path: &Path) -> Result<()> {
    Ok(())
}

fn sync_parent(parent: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .wrap_err_with(|| format!("failed to flush {}", parent.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use alloy_primitives::B256;
    use alloy_signer::SignerSync;
    use clap::Parser;
    use serde_json::json;
    use tempo_alloy::primitives::transaction::{
        KeyAuthorization, PrimitiveSignature, SignatureType,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::Mutex,
    };

    use super::*;

    const ACCESS_SENTINEL: &str = "access-token-sentinel-never-log";
    const REFRESH_SENTINEL: &str = "opaque-refresh-token-sentinel-never-log_A1b2C3d4E5f6G7h8I9j0";
    const CHATGPT_ACCOUNT: &str = "account-test-123";

    fn test_jwt(payload: Value) -> String {
        test_jwt_with_signature(payload, &URL_SAFE_NO_PAD.encode(b"signature"))
    }

    fn test_jwt_with_signature(payload: Value, signature: &str) -> String {
        format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap()),
            signature
        )
    }

    fn chatgpt_claims(account_id: &str, fedramp: bool) -> Value {
        json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_account_is_fedramp": fedramp,
            }
        })
    }

    fn valid_codex_auth(exp: u64) -> Value {
        let mut access_claims = chatgpt_claims(CHATGPT_ACCOUNT, true);
        access_claims
            .as_object_mut()
            .unwrap()
            .insert("exp".to_owned(), json!(exp));
        json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": test_jwt(chatgpt_claims(CHATGPT_ACCOUNT, true)),
                "access_token": test_jwt_with_signature(access_claims, ACCESS_SENTINEL),
                "refresh_token": REFRESH_SENTINEL,
                "account_id": CHATGPT_ACCOUNT,
            },
            "last_refresh": "2026-08-27T00:00:00Z",
        })
    }

    fn write_private_auth(path: &Path, auth: &Value) {
        fs::write(path, serde_json::to_vec(auth).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn device_override_accepts_only_canonical_local_nanocodex_origins() {
        assert!(validated_device_base(None).is_ok());
        assert!(validated_device_base(Some("https://nanocodex.local/v1/device")).is_ok());
        assert!(validated_device_base(Some("https://nanocodex.local/v1/device/")).is_ok());
        assert!(validated_device_base(Some("https://nanocodex.localhost/v1/device")).is_ok());
        assert!(
            validated_device_base(Some("https://passkey-fix-a1b2c3.nanocodex.local/v1/device"))
                .is_ok()
        );
        assert!(
            validated_device_base(Some(
                "http://passkey-fix-a1b2c3.nanocodex.localhost:20735/v1/device"
            ))
            .is_ok()
        );
        assert!(validated_device_base(Some("http://127.0.0.1:8787/v1/device")).is_err());
        assert!(validated_device_base(Some("https://localhost:8787/v1/device")).is_err());
        assert!(
            validated_device_base(Some("https://nested.evil.nanocodex.local/v1/device")).is_err()
        );
        assert!(
            validated_device_base(Some("https://user@passkey-fix.nanocodex.local/v1/device"))
                .is_err()
        );
        assert!(
            validated_device_base(Some("https://passkey-fix.nanocodex.local:8443/v1/device"))
                .is_err()
        );
        assert!(validated_device_base(Some("https://evil.example/v1/device")).is_err());
        assert!(validated_device_base(Some("file:///tmp/device")).is_err());
        assert!(validated_device_base(Some("http://localhost:8787/v1/device?secret=x")).is_err());
    }

    #[test]
    fn connect_target_accepts_connectors_and_public_mcp_hosts_only() {
        #[derive(clap::Parser)]
        struct ConnectCli {
            #[command(flatten)]
            connect: Connect,
        }

        let parsed = ConnectCli::try_parse_from(["connect", "mcp.linear.app"]).unwrap();
        assert_eq!(
            parsed.connect.services,
            vec![ConnectTarget::RemoteMcp("mcp.linear.app".to_owned())]
        );
        assert_eq!(
            ConnectTarget::from_str("github").unwrap(),
            ConnectTarget::Connector(Connector::Github)
        );
        assert_eq!(
            ConnectTarget::from_str("MCP.Linear.App").unwrap(),
            ConnectTarget::RemoteMcp("mcp.linear.app".to_owned())
        );
        for rejected in [
            "linear.app",
            "https://mcp.linear.app/mcp",
            "mcp.local",
            "mcp.internal",
            "mcp.localhost",
            "mcp.127.0.0.1",
            "mcp.10.0.0.1",
            "mcp.linear.app:443",
            "mcp.linear.app/path",
            "mcp.-linear.app",
        ] {
            assert!(
                ConnectTarget::from_str(rejected).is_err(),
                "unexpected accepted target: {rejected}"
            );
            assert!(ConnectCli::try_parse_from(["connect", rejected]).is_err());
        }
    }

    #[test]
    fn auth_file_override_is_rejected_before_read_without_explicit_chatgpt() {
        #[derive(clap::Parser)]
        struct ConnectCli {
            #[command(flatten)]
            connect: Connect,
        }

        let missing = std::env::temp_dir().join("nanocodex-missing-auth-sentinel.json");
        let github = ConnectCli::try_parse_from([
            "connect",
            "github",
            "--auth-file",
            missing.to_str().unwrap(),
        ])
        .unwrap();
        let error = github.connect.load_chatgpt_credential_import().unwrap_err();
        assert!(error.to_string().contains("only valid"));
        assert!(!error.to_string().contains(missing.to_str().unwrap()));

        let github = ConnectCli::try_parse_from(["connect", "github"]).unwrap();
        assert!(
            github
                .connect
                .load_chatgpt_credential_import()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn codex_chatgpt_auth_import_is_strict_bounded_and_secret_safe() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let exp = unix_timestamp().unwrap() + 10 * 60;
        let auth = valid_codex_auth(exp);
        write_private_auth(&path, &auth);

        let imported = load_chatgpt_credential_import(&path).unwrap();
        assert_eq!(imported.account_id, CHATGPT_ACCOUNT);
        assert_eq!(imported.refresh_token, REFRESH_SENTINEL);
        assert_eq!(imported.expires_at, exp * 1_000);
        assert!(imported.fedramp);
        let debug = format!("{imported:?}");
        assert!(!debug.contains(&imported.access_token));
        assert!(!debug.contains(&imported.refresh_token));
        assert!(debug.contains("[REDACTED]"));
        let projected = serde_json::to_value(&imported).unwrap();
        assert_eq!(projected.as_object().unwrap().len(), 5);
        assert_eq!(projected["refresh_token"], REFRESH_SENTINEL);
        assert_eq!(projected["account_id"], CHATGPT_ACCOUNT);
        assert_eq!(projected["expires_at"], exp * 1_000);
        assert_eq!(projected["fedramp"], true);

        let mut wrong_mode = auth.clone();
        wrong_mode["auth_mode"] = json!("apikey");
        write_private_auth(&path, &wrong_mode);
        assert!(
            load_chatgpt_credential_import(&path)
                .unwrap_err()
                .to_string()
                .contains("not a ChatGPT login")
        );

        let mut inconsistent_account = auth.clone();
        inconsistent_account["tokens"]["account_id"] = json!("different-account");
        write_private_auth(&path, &inconsistent_account);
        let error = load_chatgpt_credential_import(&path).unwrap_err();
        let rendered = format!("{error:?}");
        assert!(rendered.contains("inconsistent"));
        assert!(!rendered.contains(ACCESS_SENTINEL));
        assert!(!rendered.contains(REFRESH_SENTINEL));

        let mut inconsistent_fedramp = auth.clone();
        inconsistent_fedramp["tokens"]["id_token"] =
            json!(test_jwt(chatgpt_claims(CHATGPT_ACCOUNT, false)));
        write_private_auth(&path, &inconsistent_fedramp);
        assert!(
            load_chatgpt_credential_import(&path)
                .unwrap_err()
                .to_string()
                .contains("FedRAMP")
        );

        for invalid in [
            String::new(),
            " leading-space".to_owned(),
            "control\ncharacter".to_owned(),
            "x".repeat(CHATGPT_TOKEN_LIMIT + 1),
        ] {
            let mut invalid_refresh = auth.clone();
            invalid_refresh["tokens"]["refresh_token"] = json!(invalid);
            write_private_auth(&path, &invalid_refresh);
            let error = load_chatgpt_credential_import(&path).unwrap_err();
            let rendered = format!("{error:?}");
            assert!(rendered.contains("refresh token is missing or invalid"));
            assert!(!rendered.contains(ACCESS_SENTINEL));
            assert!(!rendered.contains(REFRESH_SENTINEL));
        }

        for (kind, field) in [("ID", "id_token"), ("access", "access_token")] {
            let mut invalid_jwt = auth.clone();
            invalid_jwt["tokens"][field] = json!("opaque-not-a-jwt");
            write_private_auth(&path, &invalid_jwt);
            let error = load_chatgpt_credential_import(&path).unwrap_err();
            let rendered = format!("{error:?}");
            assert!(rendered.contains(&format!("{kind} token is not a three-segment JWT")));
            assert!(!rendered.contains(ACCESS_SENTINEL));
            assert!(!rendered.contains(REFRESH_SENTINEL));
        }

        let near_expiry = valid_codex_auth(unix_timestamp().unwrap() + 5 * 60);
        write_private_auth(&path, &near_expiry);
        assert!(
            load_chatgpt_credential_import(&path)
                .unwrap_err()
                .to_string()
                .contains("expires too soon")
        );
    }

    #[cfg(unix)]
    #[test]
    fn codex_auth_import_rejects_symlinks_public_modes_and_oversized_files() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = tempfile::tempdir().unwrap();
        let auth_path = directory.path().join("auth.json");
        let link_path = directory.path().join("auth-link.json");
        write_private_auth(
            &auth_path,
            &valid_codex_auth(unix_timestamp().unwrap() + 10 * 60),
        );
        symlink(&auth_path, &link_path).unwrap();
        assert!(load_chatgpt_credential_import(&link_path).is_err());

        fs::set_permissions(&auth_path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(
            load_chatgpt_credential_import(&auth_path)
                .unwrap_err()
                .to_string()
                .contains("group or other")
        );

        fs::write(&auth_path, vec![b'x'; CHATGPT_AUTH_FILE_LIMIT as usize + 1]).unwrap();
        fs::set_permissions(&auth_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            load_chatgpt_credential_import(&auth_path)
                .unwrap_err()
                .to_string()
                .contains("exceeds 64 KiB")
        );
    }

    #[test]
    fn chatgpt_import_commitment_has_exact_domain_and_binary_layout() {
        let imported = ChatgptCredentialImport {
            access_token: "access".to_owned(),
            refresh_token: "refresh".to_owned(),
            account_id: "acct_123".to_owned(),
            expires_at: 1_700_000_000_123,
            fedramp: false,
        };
        assert_eq!(
            imported.resource(),
            "urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:vo_PpDlpaEWBzcjBCi0CpMQsPYiutjEMtb6HsNBjhng"
        );
    }

    #[test]
    fn remote_mcp_resources_are_exact_and_focus_is_ui_only() {
        let connection = ManagedMcpConnection {
            id: "a".repeat(43),
            name: "Linear".to_owned(),
        };
        let mut focused = RequestedCapabilities::connect(&[ConnectTarget::RemoteMcp(
            "mcp.linear.app".to_owned(),
        )]);
        focused.mcp_connections.push(connection.clone());
        let resources = focused.resources(None);
        assert!(resources.contains(&format!("urn:nanocodex:mcp:{}", connection.id)));
        assert!(resources.contains(&format!("urn:nanocodex:mcp-focus:{}", connection.id)));
        assert!(
            !resources
                .iter()
                .any(|resource| resource.contains("linear.app"))
        );

        let mut multiple = RequestedCapabilities::connect(&[
            ConnectTarget::RemoteMcp("mcp.linear.app".to_owned()),
            ConnectTarget::Connector(Connector::Github),
        ]);
        multiple.mcp_connections.push(connection);
        assert!(
            !multiple
                .resources(None)
                .iter()
                .any(|resource| resource.starts_with("urn:nanocodex:mcp-focus:"))
        );
    }

    #[test]
    fn wallet_request_has_exact_default_and_optional_resources() {
        let signer = PrivateKeySigner::random();
        let origin = Url::parse("https://nanocodex-connect-api.gakonst.workers.dev/").unwrap();
        let defaults = RequestedCapabilities {
            connectors: Vec::new(),
            mcp_targets: Vec::new(),
            mcp_connections: Vec::new(),
            mpp: false,
            browser_cookie_origin: None,
            focus_connector: None,
            focus_mcp: false,
        };
        let request =
            wallet_connect_request(&origin, &defaults, &signer, 4_000_000_000, None).unwrap();
        let capabilities = &request["params"][0]["capabilities"];
        assert!(
            capabilities["authorizeAccessKey"]["publicKey"]
                .as_str()
                .is_some_and(|value| value.len() == 132 && value.starts_with("0x04")),
            "unexpected public key: {}",
            capabilities["authorizeAccessKey"]["publicKey"]
        );
        assert_eq!(request["method"], "wallet_connect");
        assert_eq!(request["params"][0]["chainId"], CHAIN_ID_HEX);
        assert_eq!(
            capabilities["authorizeAccessKey"]["limits"],
            json!([
                { "token": MACH, "limit": "0x0", "period": 0 },
                { "token": USDC_E, "limit": "0x0", "period": 0 },
            ])
        );
        assert_eq!(capabilities["authorizeAccessKey"]["scopes"], json!([]));
        let resources = capabilities["auth"]["resources"].as_array().unwrap();
        assert!(resources.contains(&json!("urn:nanocodex:history:read")));
        assert!(resources.contains(&json!("urn:nanocodex:memory:read")));
        assert!(resources.contains(&json!("urn:nanocodex:memory:write")));
        assert!(resources.contains(&json!(
            "urn:nanocodex:agent:visibility:reply,actions,history"
        )));
        assert!(
            !resources
                .iter()
                .any(|resource| resource.as_str().unwrap().contains("trace"))
        );
        assert!(
            !resources
                .iter()
                .any(|resource| resource.as_str().unwrap().contains("connectors"))
        );
        assert!(!resources.contains(&json!("urn:nanocodex:mpp:machusd:spend")));
        assert!(resources.contains(&json!("urn:nanocodex:authorization:hosted")));

        let optional = RequestedCapabilities {
            connectors: vec!["chatgpt", "github"],
            mcp_targets: Vec::new(),
            mcp_connections: Vec::new(),
            mpp: true,
            browser_cookie_origin: None,
            focus_connector: Some("github"),
            focus_mcp: false,
        };
        let request =
            wallet_connect_request(&origin, &optional, &signer, 4_000_000_000, None).unwrap();
        let capabilities = &request["params"][0]["capabilities"];
        let resources = capabilities["auth"]["resources"].as_array().unwrap();
        assert!(resources.contains(&json!("urn:nanocodex:connectors:chatgpt,github")));
        assert!(resources.contains(&json!("urn:nanocodex:connector-focus:github")));
        assert!(resources.contains(&json!("urn:nanocodex:mpp:machusd:spend")));
        assert!(!resources.contains(&json!("urn:nanocodex:authorization:hosted")));
        assert_eq!(
            capabilities["authorizeAccessKey"]["limits"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            capabilities["authorizeAccessKey"]["scopes"]
                .as_array()
                .unwrap()
                .len(),
            6
        );

        let multiple = RequestedCapabilities {
            connectors: vec!["github", "gmail"],
            mcp_targets: Vec::new(),
            mcp_connections: Vec::new(),
            mpp: false,
            browser_cookie_origin: None,
            focus_connector: None,
            focus_mcp: false,
        };
        let request =
            wallet_connect_request(&origin, &multiple, &signer, 4_000_000_000, None).unwrap();
        let resources = request["params"][0]["capabilities"]["auth"]["resources"]
            .as_array()
            .unwrap();
        assert!(resources.contains(&json!("urn:nanocodex:connectors:github,gmail")));
        assert!(resources.contains(&json!("urn:nanocodex:authorization:hosted")));
        assert!(!resources.iter().any(|resource| {
            resource
                .as_str()
                .is_some_and(|value| value.starts_with("urn:nanocodex:connector-focus:"))
        }));
    }

    #[test]
    fn hosted_wallet_result_is_strict_and_never_accepted_for_mpp() {
        let signer = PrivateKeySigner::random();
        let request =
            RequestedCapabilities::connect(&[ConnectTarget::Connector(Connector::Github)]);
        let account = Address::repeat_byte(0x11);
        let approval_id = "a".repeat(43);
        let result = json!({
            "accounts": [{
                "address": account,
                "capabilities": {
                    "auth": {"approval_id": approval_id, "mode": "hosted"}
                }
            }]
        });
        let approved = validate_wallet_result(result.clone(), &signer, 4_000_000_000, &request)
            .expect("valid hosted result");
        assert!(matches!(approved, ApprovedWalletResult::Hosted(_)));

        let mpp = RequestedCapabilities::login(true, None);
        assert!(
            validate_wallet_result(result.clone(), &signer, 4_000_000_000, &mpp)
                .err()
                .unwrap()
                .to_string()
                .contains("not requested")
        );
        let mut invalid = result;
        invalid["accounts"][0]["capabilities"]["auth"]["approval_id"] = json!("too-short");
        assert!(
            validate_wallet_result(invalid, &signer, 4_000_000_000, &request)
                .err()
                .unwrap()
                .to_string()
                .contains("approval")
        );
    }

    #[test]
    fn hosted_connection_rejects_extra_authority_and_long_expiry() {
        let request =
            RequestedCapabilities::connect(&[ConnectTarget::Connector(Connector::Github)]);
        let account = Address::repeat_byte(0x11);
        let approved = ApprovedWalletResult::Hosted(ApprovedHostedResult {
            account,
            approval_id: "a".repeat(43),
        });
        let expires_at = unix_timestamp().unwrap() + ACCESS_KEY_LIFETIME;
        let mut hosted = hosted_connection_wire(
            account,
            expires_at,
            vec![
                "nanocodex.agent",
                "agent.output.final",
                "agent.output.actions",
                "agent.history.read",
                "history:read",
                "memory:read",
                "memory:write",
                "github",
            ],
        );
        hosted.as_object_mut().unwrap().remove("mpp");
        let response: ConnectionResponse = serde_json::from_value(hosted).unwrap();
        validate_connection(&response, &approved, &request).unwrap();

        let mut exact_connector = hosted_connection_wire(
            account,
            expires_at,
            vec![
                "nanocodex.agent",
                "agent.output.final",
                "agent.output.actions",
                "agent.history.read",
                "history:read",
                "memory:read",
                "memory:write",
                "github",
            ],
        );
        exact_connector["grant"]["connector_connections"] = json!({"github": ["a".repeat(43)]});
        let response: ConnectionResponse = serde_json::from_value(exact_connector.clone()).unwrap();
        validate_connection(&response, &approved, &request).unwrap();

        exact_connector["grant"]["connector_connections"] = json!({"gmail": ["b".repeat(43)]});
        let response: ConnectionResponse = serde_json::from_value(exact_connector).unwrap();
        assert!(validate_connection(&response, &approved, &request).is_err());

        let mut extra = hosted_connection_wire(
            account,
            expires_at,
            vec![
                "nanocodex.agent",
                "agent.output.final",
                "agent.output.actions",
                "agent.history.read",
                "history:read",
                "memory:read",
                "memory:write",
                "github",
                "gmail",
            ],
        );
        let extra: ConnectionResponse = serde_json::from_value(extra.take()).unwrap();
        assert!(validate_connection(&extra, &approved, &request).is_err());

        let long: ConnectionResponse = serde_json::from_value(hosted_connection_wire(
            account,
            expires_at + HOSTED_EXPIRY_CLOCK_SKEW + 60,
            vec![
                "nanocodex.agent",
                "agent.output.final",
                "agent.output.actions",
                "agent.history.read",
                "history:read",
                "memory:read",
                "memory:write",
                "github",
            ],
        ))
        .unwrap();
        assert!(validate_connection(&long, &approved, &request).is_err());
    }

    #[test]
    fn connection_response_requires_the_exact_mcp_id_set() {
        let connection = ManagedMcpConnection {
            id: "a".repeat(43),
            name: "Linear".to_owned(),
        };
        let mut request = RequestedCapabilities::connect(&[ConnectTarget::RemoteMcp(
            "mcp.linear.app".to_owned(),
        )]);
        request.mcp_connections.push(connection.clone());
        let account = Address::repeat_byte(0x11);
        let approved = ApprovedWalletResult::Hosted(ApprovedHostedResult {
            account,
            approval_id: "b".repeat(43),
        });
        let mut wire = hosted_connection_wire(
            account,
            unix_timestamp().unwrap() + ACCESS_KEY_LIFETIME,
            vec![
                "nanocodex.agent",
                "agent.output.final",
                "agent.output.actions",
                "agent.history.read",
                "history:read",
                "memory:read",
                "memory:write",
                &format!("mcp:{}", connection.id),
            ],
        );
        wire["mcp_connections"] = json!([connection]);
        wire["grant"]["mcp_connections"] = wire["mcp_connections"].clone();
        let exact: ConnectionResponse = serde_json::from_value(wire.clone()).unwrap();
        validate_connection(&exact, &approved, &request).unwrap();

        for connections in [
            json!([]),
            json!([{"id":"c".repeat(43),"name":"Cloudflare"}]),
            json!([
                {"id":"a".repeat(43),"name":"Linear"},
                {"id":"a".repeat(43),"name":"Linear duplicate"}
            ]),
        ] {
            let mut changed = wire.clone();
            changed["mcp_connections"] = connections;
            let changed: ConnectionResponse = serde_json::from_value(changed).unwrap();
            assert!(validate_connection(&changed, &approved, &request).is_err());
        }

        let mut changed_grant = wire.clone();
        changed_grant["grant"]["mcp_connections"] =
            json!([{"id":"c".repeat(43),"name":"Cloudflare"}]);
        let changed_grant: ConnectionResponse = serde_json::from_value(changed_grant).unwrap();
        assert!(validate_connection(&changed_grant, &approved, &request).is_err());

        let mut secret = wire;
        secret["mcp_connections"][0]["endpoint"] = json!("https://mcp.linear.app/mcp");
        assert!(serde_json::from_value::<ConnectionResponse>(secret).is_err());
    }

    #[test]
    fn rpc_parser_rejects_mismatched_or_ambiguous_results() {
        let wrong_id: RpcResponseEnvelope = serde_json::from_value(json!({
            "type": "rpc-responses",
            "payload": [{"jsonrpc":"2.0", "id":"other", "result":{}}]
        }))
        .unwrap();
        assert!(
            wrong_id
                .into_result()
                .unwrap_err()
                .to_string()
                .contains("ID")
        );

        let multiple: RpcResponseEnvelope = serde_json::from_value(json!({
            "type": "rpc-responses",
            "payload": [
                {"jsonrpc":"2.0", "id":RPC_ID, "result":{}},
                {"jsonrpc":"2.0", "id":RPC_ID, "result":{}}
            ]
        }))
        .unwrap();
        assert!(
            multiple
                .into_result()
                .unwrap_err()
                .to_string()
                .contains("one RPC")
        );
    }

    #[test]
    fn login_storage_is_atomic_owner_only_and_redacted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("connect.json");
        let login = test_stored_login("http://127.0.0.1:8787");
        login.store(&path).unwrap();
        let loaded = StoredLogin::load(&path).unwrap().unwrap();
        assert_eq!(loaded.account_id, login.account_id);
        assert!(!format!("{loaded:?}").contains(&login.grant_token));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                StoredLogin::load(&path)
                    .unwrap_err()
                    .to_string()
                    .contains("0600")
            );
        }
    }

    #[test]
    fn login_persists_only_safe_mcp_identity_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("connect.json");
        let mut login = test_stored_login("http://127.0.0.1:8787");
        login.mcp_connections = vec![ManagedMcpConnection {
            id: "a".repeat(43),
            name: "Linear".to_owned(),
        }];
        login.store(&path).unwrap();

        let encoded = String::from_utf8(fs::read(&path).unwrap()).unwrap();
        assert!(encoded.contains(&"a".repeat(43)));
        assert!(encoded.contains("Linear"));
        for forbidden in [
            "mcp.linear.app",
            "endpoint",
            "access_token",
            "refresh_token",
            "provider_token",
        ] {
            assert!(!encoded.contains(forbidden));
        }

        let loaded = StoredLogin::load(&path).unwrap().unwrap();
        assert_eq!(loaded.mcp_connections, login.mcp_connections);
    }

    #[test]
    fn login_loads_a_prior_record_without_mcp_connections() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("connect.json");
        let login = test_stored_login("http://127.0.0.1:8787");
        login.store(&path).unwrap();

        let mut encoded: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        encoded.as_object_mut().unwrap().remove("mcp_connections");
        fs::write(&path, serde_json::to_vec(&encoded).unwrap()).unwrap();

        let loaded = StoredLogin::load(&path).unwrap().unwrap();
        assert!(loaded.mcp_connections.is_empty());
        assert_eq!(loaded.grant_id, login.grant_id);
    }

    #[test]
    fn login_reuses_existing_optional_authority() {
        let mut login = test_stored_login("http://127.0.0.1:8787");
        login.capabilities.extend(
            REQUIRED_DATA_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned()),
        );
        login.capabilities.push("chatgpt".to_owned());
        login.capabilities.push("github".to_owned());
        login.capabilities.push("mpp.mach".to_owned());

        assert!(login.satisfies_login(false, None));
        assert!(login.satisfies_login(true, None));
        login
            .capabilities
            .retain(|capability| capability != "mpp.mach");
        assert!(login.satisfies_login(false, None));
        assert!(!login.satisfies_login(true, None));
        login
            .capabilities
            .retain(|capability| capability != "chatgpt");
        assert!(login.satisfies_login(false, None));
        login
            .capabilities
            .retain(|capability| capability != "memory:write");
        assert!(!login.satisfies_login(false, None));
    }

    #[test]
    fn browser_cookie_authority_is_canonical_and_exact() {
        let origin = canonical_browser_cookie_origin("https://Console.Example.com:443").unwrap();
        assert_eq!(origin, "https://console.example.com");
        assert_eq!(
            local_browser_cookie_sync_resource(&origin),
            "urn:nanocodex:browser-cookies:local-sync:https%3A%2F%2Fconsole.example.com"
        );
        assert!(canonical_browser_cookie_origin("http://example.com").is_err());
        assert!(canonical_browser_cookie_origin("http://127.0.0.2:8787").is_err());
        assert_eq!(
            canonical_browser_cookie_origin("http://[::1]:8787").unwrap(),
            "http://[::1]:8787"
        );
        assert!(canonical_browser_cookie_origin("https://example.com/path").is_err());

        let mut login = test_stored_login("http://127.0.0.1:8787");
        login.capabilities.extend(
            REQUIRED_DATA_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned()),
        );
        login
            .capabilities
            .push(local_browser_cookie_sync_resource(&origin));
        assert!(login.satisfies_login(false, Some(&origin)));
        assert!(!login.satisfies_login(false, Some("https://other.example")));
    }

    #[test]
    fn grant_rotation_preserves_existing_optional_authority() {
        let origin = "https://console.twilio.com";
        let mut stored = test_stored_login("http://127.0.0.1:8787");
        stored.capabilities.extend([
            "github".to_owned(),
            "mpp.mach".to_owned(),
            local_browser_cookie_sync_resource(origin),
        ]);
        stored.mcp_connections.push(ManagedMcpConnection {
            id: "m".repeat(43),
            name: "Existing MCP".to_owned(),
        });

        let mut connect =
            RequestedCapabilities::connect(&[ConnectTarget::Connector(Connector::Gmail)]);
        connect.preserve_from(&stored).unwrap();
        assert_eq!(connect.connectors, vec!["github", "gmail"]);
        assert_eq!(connect.mcp_connections, stored.mcp_connections);
        assert!(connect.mpp);
        assert_eq!(connect.browser_cookie_origin.as_deref(), Some(origin));

        let replacement = "https://other.example";
        let mut login = RequestedCapabilities::login(false, Some(replacement));
        login.preserve_from(&stored).unwrap();
        assert_eq!(login.browser_cookie_origin.as_deref(), Some(replacement));
        assert_eq!(login.connectors, vec!["github"]);
    }

    #[test]
    fn login_for_an_explicit_new_origin_preserves_the_prior_as_inactive() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("connect.json");
        test_stored_login("http://127.0.0.1:8787")
            .store(&path)
            .unwrap();

        assert!(
            StoredLogin::load_if_valid(&path, Some("http://127.0.0.1:8788"))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            StoredLogin::load(&path).unwrap().unwrap().origin,
            "http://127.0.0.1:8787"
        );
    }

    #[test]
    fn connection_requests_only_the_explicit_connectors() {
        let login = RequestedCapabilities::login(false, None);
        assert!(login.connectors.is_empty());
        assert_eq!(login.focus_connector, None);

        let focused = RequestedCapabilities::connect(&[ConnectTarget::Connector(Connector::Gmail)]);
        assert_eq!(focused.connectors, vec!["gmail"]);
        assert_eq!(focused.focus_connector, Some("gmail"));
        assert!(!focused.mpp);

        let multiple = RequestedCapabilities::connect(&[
            ConnectTarget::Connector(Connector::Gmail),
            ConnectTarget::Connector(Connector::Gdrive),
        ]);
        assert_eq!(multiple.connectors, vec!["gmail", "gdrive"]);
        assert_eq!(multiple.focus_connector, None);
        assert!(!multiple.mpp);

        let fresh = RequestedCapabilities::connect(&[ConnectTarget::Connector(Connector::Gdrive)]);
        assert_eq!(fresh.connectors, vec!["gdrive"]);
        assert_eq!(fresh.focus_connector, Some("gdrive"));
        assert!(!fresh.mpp);
    }

    #[test]
    fn scoped_managed_credential_redacts_bearer_and_keeps_account_binding() {
        let credential = ScopedManagedCredential {
            origin: Url::parse("https://connect.example/").unwrap(),
            grant_token: "g".repeat(43),
            grant_id: format!("0x{}", "33".repeat(32)),
            account_id: "123e4567-e89b-42d3-a456-426614174000".to_owned(),
            account_address: Address::repeat_byte(0x11),
            mcp_connections: Vec::new(),
        };
        let debug = format!("{credential:?}");
        assert!(!debug.contains(credential.bearer_token()));
        assert_eq!(credential.origin().as_str(), "https://connect.example/");
        assert_eq!(credential.grant_id(), format!("0x{}", "33".repeat(32)));
        assert_eq!(
            credential.account_id(),
            "123e4567-e89b-42d3-a456-426614174000"
        );
        assert_eq!(credential.account_address(), Address::repeat_byte(0x11));
    }

    #[tokio::test]
    async fn managed_credential_rejects_a_live_grant_without_memory_capabilities() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let login = test_stored_login(&format!("http://{address}"));
        login.store(&directory.path().join("connect.json")).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (path, _, _) = read_request(&mut stream).await;
            assert!(path.starts_with("/v1/grants/"));
            write_json(&mut stream, "200 OK", &test_connection_wire(&login)).await;
        });
        let error = match load_managed_credential(directory.path()).await {
            Ok(_) => panic!("a grant without memory capabilities must be rejected"),
            Err(error) => error,
        };
        server.await.unwrap();
        assert!(error.to_string().contains("memory:read"));
    }

    #[test]
    fn stable_utc_expiry_format_handles_epoch_and_leap_day() {
        assert_eq!(format_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_utc(1_709_164_800), "2024-02-29 00:00 UTC");
    }

    #[tokio::test]
    async fn mock_device_flow_registers_polls_and_creates_grant() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = Url::parse(&format!("http://{address}/v1/device/")).unwrap();
        let root = PrivateKeySigner::random();
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let server = tokio::spawn(mock_login_server(
            listener,
            root,
            Arc::clone(&captured),
            None,
            None,
        ));
        let paths = LoginPaths {
            login: directory.path().join("connect.json"),
            accounts: directory.path().join("tempo-store.json"),
        };
        let mut flow = LoginFlow::new(base, paths.clone(), false).unwrap();
        flow.poll_second = Duration::from_millis(1);
        flow.overall_timeout = Duration::from_secs(2);
        flow.complete(RequestedCapabilities::login(false, None), None)
            .await
            .unwrap();
        server.await.unwrap();

        let requests = captured.lock().await;
        assert_eq!(requests.len(), 5);
        assert_eq!(requests[0]["path"], "/v1/device/register");
        let resources = requests[0]["body"]["message"]["payload"][0]["params"][0]
            ["capabilities"]["auth"]["resources"]
            .as_array()
            .unwrap();
        assert!(!resources.iter().any(|resource| {
            resource.as_str().is_some_and(|value| {
                value.starts_with("urn:nanocodex:connectors:")
                    || value.starts_with("urn:nanocodex:connector-focus:")
                    || value.starts_with(CHATGPT_IMPORT_RESOURCE_PREFIX)
            })
        }));
        assert_eq!(requests[1]["path"], "/v1/device/token");
        assert_eq!(requests[2]["path"], "/v1/device/token");
        assert_eq!(requests[3]["path"], "/v1/device/token");
        assert_eq!(requests[4]["path"], "/v1/connections");
        assert_eq!(requests[4]["origin"], APP_ORIGIN);
        assert_eq!(requests[4]["app_id"], APP_ID);
        assert!(
            requests[4]["body"]
                .get("chatgpt_credential_import")
                .is_none()
        );
        drop(requests);

        let stored = StoredLogin::load(&paths.login).unwrap().unwrap();
        assert_eq!(stored.account_id, "123e4567-e89b-42d3-a456-426614174000");
        assert!(stored.capabilities.contains(&"memory:write".to_owned()));
        let accounts = TempoAccountsStore::open(&paths.accounts).unwrap();
        let keys = accounts.access_keys().unwrap();
        assert_eq!(keys.len(), 1);
        assert!(keys[0].is_locally_signable());
        assert_eq!(keys[0].allowed_calls(), Some([].as_slice()));
    }

    #[tokio::test]
    async fn explicit_chatgpt_flow_commits_and_posts_ephemeral_credentials_only() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = Url::parse(&format!("http://{address}/v1/device/")).unwrap();
        let root = PrivateKeySigner::random();
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let server = tokio::spawn(mock_login_server(
            listener,
            root,
            Arc::clone(&captured),
            None,
            None,
        ));
        let paths = LoginPaths {
            login: directory.path().join("connect.json"),
            accounts: directory.path().join("tempo-store.json"),
        };
        let imported = ChatgptCredentialImport {
            access_token: ACCESS_SENTINEL.to_owned(),
            refresh_token: REFRESH_SENTINEL.to_owned(),
            account_id: CHATGPT_ACCOUNT.to_owned(),
            expires_at: 1_800_000_000_000,
            fedramp: true,
        };
        let expected_resource = imported.resource();
        let mut flow = LoginFlow::new(base, paths.clone(), false)
            .unwrap()
            .with_chatgpt_credential_import(Some(imported));
        flow.poll_second = Duration::from_millis(1);
        flow.overall_timeout = Duration::from_secs(2);
        flow.complete(
            RequestedCapabilities::connect(&[ConnectTarget::Connector(Connector::Chatgpt)]),
            None,
        )
        .await
        .unwrap();
        server.await.unwrap();

        let requests = captured.lock().await;
        let resources = requests[0]["body"]["message"]["payload"][0]["params"][0]
            ["capabilities"]["auth"]["resources"]
            .as_array()
            .unwrap();
        assert_eq!(
            resources
                .iter()
                .filter_map(Value::as_str)
                .filter(|resource| resource.starts_with(CHATGPT_IMPORT_RESOURCE_PREFIX))
                .collect::<Vec<_>>(),
            vec![expected_resource.as_str()]
        );
        let body = &requests[4]["body"];
        assert_eq!(body["requested_connectors"], json!(["chatgpt"]));
        assert_eq!(
            body["chatgpt_credential_import"].as_object().unwrap().len(),
            5
        );
        assert_eq!(
            body["chatgpt_credential_import"],
            json!({
                "access_token": ACCESS_SENTINEL,
                "refresh_token": REFRESH_SENTINEL,
                "account_id": CHATGPT_ACCOUNT,
                "expires_at": 1_800_000_000_000_u64,
                "fedramp": true,
            })
        );
        drop(requests);

        let stored = fs::read_to_string(&paths.login).unwrap();
        assert!(!stored.contains(ACCESS_SENTINEL));
        assert!(!stored.contains(REFRESH_SENTINEL));
        assert!(!stored.contains("chatgpt_credential_import"));
    }

    #[tokio::test]
    async fn hosted_flow_sends_no_key_material_and_persists_no_tempo_key() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let server = tokio::spawn(mock_hosted_login_server(listener, Arc::clone(&captured)));
        let paths = LoginPaths {
            login: directory.path().join("connect.json"),
            accounts: directory.path().join("tempo-store.json"),
        };
        let mut flow = LoginFlow::new(
            Url::parse(&format!("http://{address}/v1/device/")).unwrap(),
            paths.clone(),
            false,
        )
        .unwrap();
        flow.poll_second = Duration::from_millis(1);
        flow.overall_timeout = Duration::from_secs(2);
        flow.complete(
            RequestedCapabilities::connect(&[ConnectTarget::Connector(Connector::Github)]),
            None,
        )
        .await
        .unwrap();
        server.await.unwrap();

        let requests = captured.lock().await;
        let body = &requests[2]["body"];
        assert_eq!(body["authorization_mode"], "hosted");
        assert_eq!(body["requested_connectors"], json!(["github"]));
        assert!(body.get("key_authorization").is_none());
        assert!(body.get("signed_key_authorization").is_none());
        drop(requests);

        let stored = StoredLogin::load(&paths.login).unwrap().unwrap();
        assert_eq!(stored.authorization_mode(), AuthorizationMode::Hosted);
        assert_eq!(stored.access_key_id, None);
        assert!(!paths.accounts.exists());
        let encoded: Value = serde_json::from_slice(&fs::read(&paths.login).unwrap()).unwrap();
        assert!(encoded.get("access_key_id").is_none());
    }

    #[tokio::test]
    async fn remote_mcp_preflight_precedes_device_flow_and_persists_safe_grant() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let server = tokio::spawn(mock_remote_mcp_login_server(
            listener,
            Arc::clone(&captured),
        ));
        let paths = LoginPaths {
            login: directory.path().join("connect.json"),
            accounts: directory.path().join("tempo-store.json"),
        };
        let mut flow = LoginFlow::new(
            Url::parse(&format!("http://{address}/v1/device/")).unwrap(),
            paths.clone(),
            false,
        )
        .unwrap();
        flow.poll_second = Duration::from_millis(1);
        flow.overall_timeout = Duration::from_secs(2);
        flow.complete(
            RequestedCapabilities::connect(&[ConnectTarget::RemoteMcp(
                "mcp.linear.app".to_owned(),
            )]),
            None,
        )
        .await
        .unwrap();
        server.await.unwrap();

        let requests = captured.lock().await;
        assert_eq!(requests[0]["path"], "/v1/mcp-intents");
        assert_eq!(requests[0]["body"], json!({"target":"mcp.linear.app"}));
        assert_eq!(requests[0]["origin"], APP_ORIGIN);
        assert_eq!(requests[0]["app_id"], APP_ID);
        assert_eq!(requests[1]["path"], "/v1/device/register");
        assert_eq!(requests[2]["path"], "/v1/device/token");
        assert_eq!(requests[3]["path"], "/v1/connections");
        assert_eq!(
            requests[3]["body"]["requested_mcp_connections"],
            json!(["a".repeat(43)])
        );
        assert!(requests[3]["body"].to_string().find("linear.app").is_none());
        drop(requests);

        let stored = StoredLogin::load(&paths.login).unwrap().unwrap();
        assert_eq!(
            stored.mcp_connections,
            vec![ManagedMcpConnection {
                id: "a".repeat(43),
                name: "Linear".to_owned(),
            }]
        );
        let encoded = String::from_utf8(fs::read(paths.login).unwrap()).unwrap();
        assert!(!encoded.contains("linear.app"));
        assert!(!encoded.contains("access_token"));
    }

    #[tokio::test]
    async fn hosted_retirement_never_opens_or_retires_a_local_key() {
        let directory = tempfile::tempdir().unwrap();
        let paths = LoginPaths {
            login: directory.path().join("connect.json"),
            accounts: directory.path().join("not-a-tempo-store.json"),
        };
        fs::write(&paths.accounts, b"not a Tempo store").unwrap();
        let mut login = test_stored_login("http://127.0.0.1:8787");
        login.access_key_id = None;
        login.pending_retirement = Some(PendingRetirement {
            origin: login.origin.clone(),
            grant_token: "p".repeat(43),
            grant_id: format!("0x{}", "44".repeat(32)),
            expires_at: 0,
            capabilities: login.capabilities.clone(),
            account_address: login.account_address,
            access_key_id: None,
            revoke_grant: false,
        });
        login.store(&paths.login).unwrap();

        retire_installation_key(&paths.accounts, &login).unwrap();
        finish_pending_retirement(&paths, &mut login).await.unwrap();
        assert!(login.pending_retirement.is_none());
        assert_eq!(fs::read(&paths.accounts).unwrap(), b"not a Tempo store");
    }

    #[tokio::test]
    async fn denied_replacement_preserves_prior_login_and_installation_key() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let paths = LoginPaths {
            login: directory.path().join("connect.json"),
            accounts: directory.path().join("tempo-store.json"),
        };
        let root = PrivateKeySigner::random();
        let prior = persist_prior_login(&paths, &format!("http://{address}"), &root);
        let login_before = fs::read(&paths.login).unwrap();
        let accounts_before = fs::read(&paths.accounts).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut stream).await;
            write_json(&mut stream, "200 OK", &test_registration(address)).await;
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut stream).await;
            write_json(
                &mut stream,
                "400 Bad Request",
                &json!({"error":"access_denied"}),
            )
            .await;
        });
        let mut flow = LoginFlow::new(
            Url::parse(&format!("http://{address}/v1/device/")).unwrap(),
            paths.clone(),
            false,
        )
        .unwrap();
        flow.poll_second = Duration::from_millis(1);
        let error = flow
            .complete(
                RequestedCapabilities {
                    connectors: vec!["github"],
                    mcp_targets: Vec::new(),
                    mcp_connections: Vec::new(),
                    mpp: false,
                    browser_cookie_origin: None,
                    focus_connector: Some("github"),
                    focus_mcp: false,
                },
                Some(PendingRetirement::from_login(&prior, true)),
            )
            .await
            .unwrap_err();
        server.await.unwrap();

        assert!(error.to_string().contains("denied"));
        assert_eq!(fs::read(&paths.login).unwrap(), login_before);
        assert_eq!(fs::read(&paths.accounts).unwrap(), accounts_before);
        assert_prior_key_is_signable(&paths, &prior);
    }

    #[tokio::test]
    async fn replacement_persistence_failure_preserves_prior_login_and_key() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let paths = LoginPaths {
            login: directory.path().join("connect.json"),
            accounts: directory.path().join("tempo-store.json"),
        };
        let root = PrivateKeySigner::random();
        let prior = persist_prior_login(&paths, &format!("http://{address}"), &root);
        let login_before = fs::read(&paths.login).unwrap();
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let server = tokio::spawn(mock_login_server(
            listener,
            root,
            Arc::clone(&captured),
            None,
            None,
        ));
        let mut flow = LoginFlow::new(
            Url::parse(&format!("http://{address}/v1/device/")).unwrap(),
            paths.clone(),
            false,
        )
        .unwrap();
        flow.poll_second = Duration::from_millis(1);
        flow.fail_login_persistence = true;
        let error = flow
            .complete(
                RequestedCapabilities {
                    connectors: Vec::new(),
                    mcp_targets: Vec::new(),
                    mcp_connections: Vec::new(),
                    mpp: false,
                    browser_cookie_origin: None,
                    focus_connector: None,
                    focus_mcp: false,
                },
                Some(PendingRetirement::from_login(&prior, true)),
            )
            .await
            .unwrap_err();
        server.await.unwrap();

        assert!(error.to_string().contains("injected"));
        assert_eq!(fs::read(&paths.login).unwrap(), login_before);
        assert_prior_key_is_signable(&paths, &prior);
        let keys = TempoAccountsStore::open(&paths.accounts)
            .unwrap()
            .access_keys()
            .unwrap();
        assert_eq!(
            keys.iter().filter(|key| key.is_locally_signable()).count(),
            1
        );
    }

    #[tokio::test]
    async fn successful_replacement_commits_before_cleaning_prior_authority() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let paths = LoginPaths {
            login: directory.path().join("connect.json"),
            accounts: directory.path().join("tempo-store.json"),
        };
        let root = PrivateKeySigner::random();
        let prior = persist_prior_login(&paths, &format!("http://{address}"), &root);
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let server = tokio::spawn(mock_login_server(
            listener,
            root,
            Arc::clone(&captured),
            Some(prior.clone()),
            Some(paths.login.clone()),
        ));
        let mut flow = LoginFlow::new(
            Url::parse(&format!("http://{address}/v1/device/")).unwrap(),
            paths.clone(),
            false,
        )
        .unwrap();
        flow.poll_second = Duration::from_millis(1);
        flow.complete(
            RequestedCapabilities {
                connectors: Vec::new(),
                mcp_targets: Vec::new(),
                mcp_connections: Vec::new(),
                mpp: false,
                browser_cookie_origin: None,
                focus_connector: None,
                focus_mcp: false,
            },
            Some(PendingRetirement::from_login(&prior, true)),
        )
        .await
        .unwrap();
        server.await.unwrap();

        let replacement = StoredLogin::load(&paths.login).unwrap().unwrap();
        assert_ne!(replacement.grant_id, prior.grant_id);
        assert_ne!(replacement.access_key_id, prior.access_key_id);
        assert!(replacement.pending_retirement.is_none());
        let keys = TempoAccountsStore::open(&paths.accounts)
            .unwrap()
            .access_keys()
            .unwrap();
        assert!(keys.iter().any(|key| {
            Some(key.address()) == replacement.access_key_id && key.is_locally_signable()
        }));
        assert!(keys.iter().any(|key| {
            Some(key.address()) == prior.access_key_id && !key.is_locally_signable()
        }));
        let requests = captured.lock().await;
        assert_eq!(
            requests.last().unwrap()["path"],
            format!("/v1/grants/{}/revoke", prior.grant_id)
        );
    }

    #[tokio::test]
    async fn existing_login_is_reused_only_after_authenticated_live_validation() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let login = test_stored_login(&origin);
        let path = directory.path().join("connect.json");
        login.store(&path).unwrap();
        let expected = login.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (path, headers, body) = read_request(&mut stream).await;
            assert_eq!(path, format!("/v1/grants/{}", expected.grant_id));
            assert_eq!(body, Value::Null);
            assert_eq!(
                headers
                    .iter()
                    .find(|(name, _)| name == "authorization")
                    .map(|(_, value)| value.as_str()),
                Some(format!("Bearer {}", expected.grant_token).as_str())
            );
            write_json(&mut stream, "200 OK", &test_connection_wire(&expected)).await;
        });
        let active = active_login(&path, Some(&origin)).await.unwrap().unwrap();
        server.await.unwrap();
        assert_eq!(active.grant_id, login.grant_id);
        assert_eq!(active.account_id, login.account_id);
    }

    #[tokio::test]
    async fn live_refresh_rejects_an_authorization_mode_change() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let login = test_stored_login(&origin);
        let path = directory.path().join("connect.json");
        login.store(&path).unwrap();
        let expected = login.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (request_path, _, _) = read_request(&mut stream).await;
            assert_eq!(request_path, format!("/v1/grants/{}", expected.grant_id));
            write_json(
                &mut stream,
                "200 OK",
                &test_hosted_connection_wire(&expected),
            )
            .await;
        });
        let error = active_login(&path, Some(&origin)).await.unwrap_err();
        server.await.unwrap();
        assert!(error.to_string().contains("authorization mode changed"));
        assert_eq!(
            StoredLogin::load(&path)
                .unwrap()
                .unwrap()
                .authorization_mode(),
            AuthorizationMode::AccessKey
        );
    }

    #[tokio::test]
    async fn poll_aborts_on_the_third_consecutive_slow_down() {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let _ = read_request(&mut stream).await;
                write_json(
                    &mut stream,
                    "400 Bad Request",
                    &json!({
                        "error": "slow_down",
                    }),
                )
                .await;
            }
        });
        let mut flow = LoginFlow::new(
            Url::parse(&format!("http://{address}/v1/device/")).unwrap(),
            LoginPaths {
                login: directory.path().join("connect.json"),
                accounts: directory.path().join("tempo-store.json"),
            },
            false,
        )
        .unwrap();
        flow.poll_second = Duration::from_millis(1);
        flow.overall_timeout = Duration::from_secs(2);
        let registration = DeviceRegistration {
            device_code: "device_code_123456789".to_owned(),
            user_code: "ABCD-EFGH".to_owned(),
            verification_uri: format!("http://{address}/verify"),
            verification_uri_complete: None,
            expires_in: 60,
            interval: 1,
        };
        let error = flow.poll(&registration, "verifier").await.unwrap_err();
        server.await.unwrap();
        assert!(error.to_string().contains("repeatedly asked"));
    }

    async fn mock_login_server(
        listener: TcpListener,
        root: PrivateKeySigner,
        captured: Arc<Mutex<Vec<Value>>>,
        revoked_prior: Option<StoredLogin>,
        replacement_path: Option<PathBuf>,
    ) {
        let mut wallet_result = None;
        let mut connection = None;
        let mut requested_connectors = Vec::<String>::new();
        let request_count = if revoked_prior.is_some() { 6 } else { 5 };
        for index in 0..request_count {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            let path = request.0.clone();
            let body = request.2;
            captured.lock().await.push(json!({
                "path": path,
                "origin": request.1.iter().find(|(name, _)| name == "origin").map(|(_, value)| value),
                "app_id": request.1.iter().find(|(name, _)| name == "x-nanocodex-app-id").map(|(_, value)| value),
                "body": body,
            }));
            let response = match index {
                0 => {
                    assert_eq!(path, "/v1/device/register");
                    assert_eq!(body["code_challenge_method"], "S256");
                    assert_eq!(body["message"]["payload"].as_array().unwrap().len(), 1);
                    requested_connectors = body["message"]["payload"][0]["params"][0]
                        ["capabilities"]["auth"]["resources"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter_map(|resource| {
                            resource.strip_prefix("urn:nanocodex:connectors:")
                        })
                        .flat_map(|connectors| connectors.split(','))
                        .map(str::to_owned)
                        .collect();
                    let access = &body["message"]["payload"][0]["params"][0]["capabilities"]["authorizeAccessKey"];
                    let key = Address::from_str(access["address"].as_str().unwrap()).unwrap();
                    let expiry = access["expiry"].as_u64().unwrap();
                    let authorization = KeyAuthorization {
                        chain_id: CHAIN_ID,
                        key_type: SignatureType::Secp256k1,
                        key_id: key,
                        expiry: NonZeroU64::new(expiry),
                        limits: Some(expected_limits(false).unwrap()),
                        allowed_calls: Some(Vec::new()),
                        witness: Some(B256::repeat_byte(0x22)),
                        is_admin: false,
                        account: None,
                    };
                    let signature = root
                        .sign_hash_sync(&authorization.signature_hash())
                        .unwrap();
                    let signed =
                        authorization.into_signed(PrimitiveSignature::Secp256k1(signature));
                    let serialized = format!("0x{}", hex::encode(alloy_rlp::encode(&signed)));
                    let PrimitiveSignature::Secp256k1(signature) = &signed.signature else {
                        unreachable!()
                    };
                    let flat = json!({
                        "address": key,
                        "chainId": CHAIN_ID_HEX,
                        "expiry": expiry,
                        "keyId": key,
                        "keyType": "secp256k1",
                        "limits": [
                            { "token": MACH, "limit": "0", "period": 0 },
                            { "token": USDC_E, "limit": "0", "period": 0 },
                        ],
                        "signature": {
                            "type": "secp256k1",
                            "r": format!("{:#x}", signature.r()),
                            "s": format!("{:#x}", signature.s()),
                            "yParity": if signature.v() { "0x1" } else { "0x0" },
                        },
                    });
                    wallet_result = Some(json!({"accounts":[{
                        "address": root.address(),
                        "capabilities": {
                            "auth": {"approval_id":"approval_identifier_123456789"},
                            "keyAuthorization": flat,
                            "personalSign": {"keyAuthorization": serialized},
                        }
                    }]}));
                    let mut grant_capabilities = vec![
                        "nanocodex.agent".to_owned(),
                        "agent.output.final".to_owned(),
                        "agent.output.actions".to_owned(),
                        "history:read".to_owned(),
                        "memory:read".to_owned(),
                        "memory:write".to_owned(),
                    ];
                    grant_capabilities.extend(requested_connectors.iter().cloned());
                    connection = Some(json!({
                        "authorization_mode": "access_key",
                        "grant_token": "g".repeat(43),
                        "account_id": "123e4567-e89b-42d3-a456-426614174000",
                        "account_address": root.address(),
                        "agent_id": "agent_test",
                        "grant": {
                            "id": format!("0x{}", "33".repeat(32)),
                            "permission": "agent.run",
                            "status": "active",
                            "expires_at": expiry,
                            "capabilities": grant_capabilities,
                        },
                        "access_key": {
                            "address": key,
                            "chain_id": CHAIN_ID.to_string(),
                            "key_id": key,
                            "key_type": "secp256k1",
                            "limits": [
                                { "token": MACH, "limit": "0", "period": 0 },
                                { "token": USDC_E, "limit": "0", "period": 0 },
                            ],
                            "scopes": [],
                            "witness": format!("0x{}", "22".repeat(32)),
                            "expiry": expiry,
                            "authorization": serialized,
                        },
                        "mpp": {},
                    }));
                    json!({
                        "device_code": "device_code_123456789",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": format!("http://{}/v1/device/verify", listener.local_addr().unwrap()),
                        "verification_uri_complete": format!("http://{}/v1/device/verify?user_code=ABCDEFGH", listener.local_addr().unwrap()),
                        "expires_in": 60,
                        "interval": 1,
                    })
                }
                1 => json!({"error":"authorization_pending"}),
                2 => json!({"error":"slow_down"}),
                3 => json!({"type":"rpc-responses", "payload":[{
                    "jsonrpc":"2.0", "id":RPC_ID, "result":wallet_result.as_ref().unwrap()
                }]}),
                4 => {
                    assert_eq!(path, "/v1/connections");
                    assert_eq!(body["authorization_mode"], "access_key");
                    assert_eq!(body["permission"], "agent.run");
                    assert_eq!(body["requested_connectors"], json!(requested_connectors));
                    connection.take().unwrap()
                }
                5 => {
                    let prior = revoked_prior.as_ref().unwrap();
                    let replacement = StoredLogin::load(replacement_path.as_ref().unwrap())
                        .unwrap()
                        .unwrap();
                    assert_ne!(replacement.grant_id, prior.grant_id);
                    assert_eq!(
                        replacement
                            .pending_retirement
                            .as_ref()
                            .map(|retirement| retirement.grant_id.as_str()),
                        Some(prior.grant_id.as_str())
                    );
                    assert_eq!(path, format!("/v1/grants/{}/revoke", prior.grant_id));
                    assert_eq!(
                        request
                            .1
                            .iter()
                            .find(|(name, _)| name == "authorization")
                            .map(|(_, value)| value.as_str()),
                        Some(format!("Bearer {}", prior.grant_token).as_str())
                    );
                    json!({
                        "id": prior.grant_id,
                        "permission": "agent.run",
                        "status": "revoked",
                        "expires_at": prior.expires_at,
                        "capabilities": prior.capabilities,
                        "connector_connections": {},
                        "mcp_connections": [],
                    })
                }
                _ => unreachable!(),
            };
            let status = if matches!(index, 1 | 2) {
                "400 Bad Request"
            } else if index == 4 {
                "201 Created"
            } else {
                "200 OK"
            };
            write_json(&mut stream, status, &response).await;
        }
    }

    async fn mock_hosted_login_server(listener: TcpListener, captured: Arc<Mutex<Vec<Value>>>) {
        let account = Address::repeat_byte(0x11);
        for index in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (path, headers, body) = read_request(&mut stream).await;
            captured.lock().await.push(json!({
                "path": path,
                "headers": headers,
                "body": body,
            }));
            let response = match index {
                0 => {
                    assert_eq!(path, "/v1/device/register");
                    let resources = body["message"]["payload"][0]["params"][0]["capabilities"]
                        ["auth"]["resources"]
                        .as_array()
                        .unwrap();
                    assert!(resources.contains(&json!("urn:nanocodex:authorization:hosted")));
                    test_registration(listener.local_addr().unwrap())
                }
                1 => {
                    assert_eq!(path, "/v1/device/token");
                    json!({
                        "type": "rpc-responses",
                        "payload": [{
                            "jsonrpc": "2.0",
                            "id": RPC_ID,
                            "result": {
                                "accounts": [{
                                    "address": account,
                                    "capabilities": {
                                        "auth": {
                                            "approval_id": "a".repeat(43),
                                            "mode": "hosted",
                                        }
                                    }
                                }]
                            }
                        }]
                    })
                }
                2 => {
                    assert_eq!(path, "/v1/connections");
                    assert_eq!(body["account_address"], json!(account));
                    assert_eq!(body["approval_id"], "a".repeat(43));
                    hosted_connection_wire(
                        account,
                        unix_timestamp().unwrap() + ACCESS_KEY_LIFETIME,
                        vec![
                            "nanocodex.agent",
                            "agent.output.final",
                            "agent.output.actions",
                            "agent.history.read",
                            "history:read",
                            "memory:read",
                            "memory:write",
                            "github",
                        ],
                    )
                }
                _ => unreachable!(),
            };
            write_json(
                &mut stream,
                if index == 2 { "201 Created" } else { "200 OK" },
                &response,
            )
            .await;
        }
    }

    async fn mock_remote_mcp_login_server(listener: TcpListener, captured: Arc<Mutex<Vec<Value>>>) {
        let account = Address::repeat_byte(0x11);
        let connection = ManagedMcpConnection {
            id: "a".repeat(43),
            name: "Linear".to_owned(),
        };
        for index in 0..4 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (path, headers, body) = read_request(&mut stream).await;
            captured.lock().await.push(json!({
                "path": path,
                "origin": headers.iter().find(|(name, _)| name == "origin").map(|(_, value)| value),
                "app_id": headers.iter().find(|(name, _)| name == "x-nanocodex-app-id").map(|(_, value)| value),
                "body": body,
            }));
            let response = match index {
                0 => {
                    assert_eq!(path, "/v1/mcp-intents");
                    json!(connection)
                }
                1 => {
                    assert_eq!(path, "/v1/device/register");
                    let resources = body["message"]["payload"][0]["params"][0]["capabilities"]
                        ["auth"]["resources"]
                        .as_array()
                        .unwrap();
                    assert!(
                        resources.contains(&json!(format!("urn:nanocodex:mcp:{}", connection.id)))
                    );
                    assert!(
                        resources
                            .contains(&json!(format!("urn:nanocodex:mcp-focus:{}", connection.id)))
                    );
                    test_registration(listener.local_addr().unwrap())
                }
                2 => {
                    assert_eq!(path, "/v1/device/token");
                    json!({
                        "type": "rpc-responses",
                        "payload": [{
                            "jsonrpc": "2.0",
                            "id": RPC_ID,
                            "result": {
                                "accounts": [{
                                    "address": account,
                                    "capabilities": {
                                        "auth": {
                                            "approval_id": "b".repeat(43),
                                            "mode": "hosted",
                                        }
                                    }
                                }]
                            }
                        }]
                    })
                }
                3 => {
                    assert_eq!(path, "/v1/connections");
                    let mut response = hosted_connection_wire(
                        account,
                        unix_timestamp().unwrap() + ACCESS_KEY_LIFETIME,
                        vec![
                            "nanocodex.agent",
                            "agent.output.final",
                            "agent.output.actions",
                            "agent.history.read",
                            "history:read",
                            "memory:read",
                            "memory:write",
                            &format!("mcp:{}", connection.id),
                        ],
                    );
                    response["mcp_connections"] = json!([connection]);
                    response
                }
                _ => unreachable!(),
            };
            write_json(
                &mut stream,
                if matches!(index, 0 | 3) {
                    "201 Created"
                } else {
                    "200 OK"
                },
                &response,
            )
            .await;
        }
    }

    async fn read_request(stream: &mut TcpStream) -> (String, Vec<(String, String)>, Value) {
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 2048];
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers_text = std::str::from_utf8(&bytes[..header_end]).unwrap();
        let mut lines = headers_text.split("\r\n");
        let request_line = lines.next().unwrap();
        let path = request_line.split_whitespace().nth(1).unwrap().to_owned();
        let headers: Vec<_> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        let length = headers
            .iter()
            .find(|(name, _)| name == "content-length")
            .map(|(_, value)| value.parse::<usize>().unwrap())
            .unwrap_or(0);
        while bytes.len() - header_end < length {
            let mut chunk = [0_u8; 2048];
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&chunk[..read]);
        }
        let body = if length == 0 {
            Value::Null
        } else {
            serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
        };
        (path, headers, body)
    }

    async fn write_json(stream: &mut TcpStream, status: &str, value: &Value) {
        let body = serde_json::to_vec(value).unwrap();
        let headers = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
        stream.shutdown().await.unwrap();
    }

    fn test_stored_login(origin: &str) -> StoredLogin {
        StoredLogin {
            version: 1,
            origin: origin.to_owned(),
            grant_token: "g".repeat(43),
            grant_id: format!("0x{}", "33".repeat(32)),
            account_id: "123e4567-e89b-42d3-a456-426614174000".to_owned(),
            account_address: Address::repeat_byte(0x11),
            agent_id: "agent_test".to_owned(),
            expires_at: 4_000_000_000,
            capabilities: vec!["nanocodex.agent".to_owned(), "history:read".to_owned()],
            mcp_connections: Vec::new(),
            access_key_id: Some(Address::repeat_byte(0x22)),
            pending_retirement: None,
        }
    }

    fn test_registration(address: std::net::SocketAddr) -> Value {
        json!({
            "device_code": "device_code_123456789",
            "user_code": "ABCD-EFGH",
            "verification_uri": format!("http://{address}/v1/device/verify"),
            "verification_uri_complete": format!(
                "http://{address}/v1/device/verify?user_code=ABCDEFGH"
            ),
            "expires_in": 60,
            "interval": 1,
        })
    }

    fn persist_prior_login(
        paths: &LoginPaths,
        origin: &str,
        root: &PrivateKeySigner,
    ) -> StoredLogin {
        let signer = PrivateKeySigner::random();
        let authorization = KeyAuthorization {
            chain_id: CHAIN_ID,
            key_type: SignatureType::Secp256k1,
            key_id: signer.address(),
            expiry: NonZeroU64::new(4_000_000_000),
            limits: Some(expected_limits(false).unwrap()),
            allowed_calls: Some(Vec::new()),
            witness: Some(B256::repeat_byte(0x44)),
            is_admin: false,
            account: None,
        };
        let signature = root
            .sign_hash_sync(&authorization.signature_hash())
            .unwrap();
        let signed = authorization.into_signed(PrimitiveSignature::Secp256k1(signature));
        TempoAccountsStore::at(&paths.accounts)
            .upsert_secp256k1_access_key(root.address(), &signer, &signed)
            .unwrap();
        let mut login = test_stored_login(origin);
        login.grant_token = "p".repeat(43);
        login.grant_id = format!("0x{}", "44".repeat(32));
        login.account_address = root.address();
        login.access_key_id = Some(signer.address());
        login.capabilities = vec![
            "nanocodex.agent".to_owned(),
            "agent.output.final".to_owned(),
            "agent.output.actions".to_owned(),
            "history:read".to_owned(),
            "memory:read".to_owned(),
            "memory:write".to_owned(),
        ];
        login.store(&paths.login).unwrap();
        login
    }

    fn assert_prior_key_is_signable(paths: &LoginPaths, prior: &StoredLogin) {
        let keys = TempoAccountsStore::open(&paths.accounts)
            .unwrap()
            .access_keys()
            .unwrap();
        assert!(keys.iter().any(|key| {
            Some(key.address()) == prior.access_key_id && key.is_locally_signable()
        }));
    }

    fn test_connection_wire(login: &StoredLogin) -> Value {
        json!({
            "authorization_mode": "access_key",
            "grant_token": login.grant_token,
            "account_id": login.account_id,
            "account_address": login.account_address,
            "agent_id": login.agent_id,
            "grant": {
                "id": login.grant_id,
                "permission": "agent.run",
                "status": "active",
                "expires_at": login.expires_at,
                "capabilities": login.capabilities,
            },
            "access_key": {
                "address": login.access_key_id,
                "chain_id": CHAIN_ID.to_string(),
                "key_id": login.access_key_id,
                "key_type": "secp256k1",
                "limits": [],
                "scopes": [],
                "witness": format!("0x{}", "22".repeat(32)),
                "expiry": login.expires_at,
                "authorization": "0x1234",
            },
            "mpp": {},
        })
    }

    fn test_hosted_connection_wire(login: &StoredLogin) -> Value {
        json!({
            "authorization_mode": "hosted",
            "grant_token": login.grant_token,
            "account_id": login.account_id,
            "account_address": login.account_address,
            "agent_id": login.agent_id,
            "grant": {
                "id": login.grant_id,
                "permission": "agent.run",
                "status": "active",
                "expires_at": login.expires_at,
                "capabilities": login.capabilities,
            },
            "mpp": {},
        })
    }

    fn hosted_connection_wire(account: Address, expires_at: u64, capabilities: Vec<&str>) -> Value {
        json!({
            "authorization_mode": "hosted",
            "grant_token": "g".repeat(43),
            "account_id": "123e4567-e89b-42d3-a456-426614174000",
            "account_address": account,
            "agent_id": "agent_hosted",
            "grant": {
                "id": format!("0x{}", "33".repeat(32)),
                "permission": "agent.run",
                "status": "active",
                "expires_at": expires_at,
                "capabilities": capabilities,
            },
            "mpp": {},
        })
    }
}
