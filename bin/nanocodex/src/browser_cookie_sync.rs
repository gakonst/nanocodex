use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::{Args, Subcommand, ValueEnum};
use eyre::{Result, WrapErr, bail, ensure};
use fs2::FileExt as _;
use futures_util::StreamExt;
use nanocodex_browser::{
    BraveSession, BrowserCookieAuthorization, BrowserCookieSameSite, BrowserOriginCookieCapture,
    BrowserProfileCookie, BrowserProfileKind,
};
use rand::RngCore;
use reqwest::{Client, Response, StatusCode, Url, redirect::Policy};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    config::default_codex_home,
    login::{
        APP_ID, APP_ORIGIN, ScopedManagedCredential, canonical_browser_cookie_origin,
        load_browser_cookie_sync_credential,
    },
};

const COOKIE_JARS_PATH: &str = "/v1/browser-cookie-jars";
const MAX_COOKIE_COUNT: usize = 300;
const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const PROFILE_STATE_BYTES: u64 = 16 * 1024;
const PROFILE_STATE_LOCK: &str = "browser-cookie-profiles.lock";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Args)]
pub(crate) struct Cookies {
    #[command(subcommand)]
    command: CookieCommand,
}

#[derive(Subcommand)]
enum CookieCommand {
    /// List cookie names from the local profile, Vault, or both without printing values.
    List(List),
    /// Copy cookies for one exact origin from a local Chromium profile into Vault.
    Sync(Sync),
}

#[derive(Args)]
struct List {
    /// Exact HTTPS origin to inspect, such as https://console.twilio.com.
    #[arg(value_parser = canonical_browser_cookie_origin)]
    origin: String,

    /// Read the live local profile, its encrypted Vault snapshot, or compare both.
    #[arg(long = "from", value_enum, default_value_t = CookieLocation::Local)]
    location: CookieLocation,

    /// How the temporary cookie broker may request local credential access (local/both only).
    #[cfg_attr(target_os = "macos", arg(long, value_enum, default_value_t = CookieAuth::Interactive))]
    #[cfg_attr(not(target_os = "macos"), arg(long, value_enum, default_value_t = CookieAuth::Background))]
    cookie_auth: CookieAuth,
}

#[derive(Args)]
struct Sync {
    /// Exact HTTPS origin to synchronize, such as https://console.twilio.com.
    #[arg(value_parser = canonical_browser_cookie_origin)]
    origin: String,

    /// How the temporary cookie broker may request local credential access.
    #[cfg_attr(target_os = "macos", arg(long, value_enum, default_value_t = CookieAuth::Interactive))]
    #[cfg_attr(not(target_os = "macos"), arg(long, value_enum, default_value_t = CookieAuth::Background))]
    cookie_auth: CookieAuth,
}

#[derive(Clone, Copy, Debug)]
enum CookieSource {
    Brave,
    Chrome,
    Chromium,
    Edge,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CookieLocation {
    Local,
    Vault,
    Both,
}

impl CookieLocation {
    const fn includes_local(self) -> bool {
        matches!(self, Self::Local | Self::Both)
    }

    const fn includes_vault(self) -> bool {
        matches!(self, Self::Vault | Self::Both)
    }
}

impl CookieSource {
    const ALL: [Self; 4] = [Self::Brave, Self::Chrome, Self::Chromium, Self::Edge];

    const fn profile_kind(self) -> BrowserProfileKind {
        match self {
            Self::Brave => BrowserProfileKind::Brave,
            Self::Chrome => BrowserProfileKind::Chrome,
            Self::Chromium => BrowserProfileKind::Chromium,
            Self::Edge => BrowserProfileKind::Edge,
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::Brave => "brave",
            Self::Chrome => "chrome",
            Self::Chromium => "chromium",
            Self::Edge => "edge",
        }
    }

    fn store_id(self) -> String {
        format!("local-{}", self.key())
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CookieAuth {
    Background,
    Interactive,
}

impl From<CookieAuth> for BrowserCookieAuthorization {
    fn from(value: CookieAuth) -> Self {
        match value {
            CookieAuth::Background => Self::Background,
            CookieAuth::Interactive => Self::Interactive,
        }
    }
}

impl Cookies {
    pub(crate) async fn run(self) -> Result<()> {
        match self.command {
            CookieCommand::List(list) => list.run().await,
            CookieCommand::Sync(sync) => sync.run().await,
        }
    }
}

impl List {
    async fn run(self) -> Result<()> {
        let codex_home = default_codex_home()?;
        let credential = if self.location.includes_vault() {
            Some(
                load_browser_cookie_sync_credential(&codex_home, &self.origin)
                    .await?
                    .ok_or_else(|| {
                        eyre::eyre!(
                            "Nanocodex Connect is not logged in; run `nanocodex login --browser-cookie-origin={}` first",
                            self.origin
                        )
                    })?,
            )
        } else {
            None
        };
        let sessions = detected_cookie_sessions(self.cookie_auth)?;
        let mut reports = Vec::new();
        for (source, session) in sessions {
            let store_id = source.store_id();
            let profile_key = local_profile_key(source, &session)?;
            let profile_id = if self.location.includes_vault() {
                existing_local_profile_id(&codex_home, &profile_key)?
            } else {
                None
            };
            let local = if self.location.includes_local() {
                match capture_local_cookies(&self.origin, source, session).await {
                    Ok(capture) => Some(cookie_name_list(&project_cookies(capture, &store_id)?)),
                    Err(_) => {
                        eprintln!(
                            "Skipped {} because its cookie store could not be read.",
                            source.profile_kind().name()
                        );
                        None
                    }
                }
            } else {
                None
            };
            let vault = match (credential.as_ref(), profile_id.as_deref()) {
                (Some(credential), Some(profile_id)) => Some(
                    fetch_vault_cookie_names(credential, &self.origin, profile_id, &store_id)
                        .await?,
                ),
                (Some(_), None) => Some(empty_vault_cookie_names()),
                (None, _) => None,
            };
            if local.is_some() || vault.is_some() {
                reports.push((source, local, vault));
            }
        }
        ensure!(
            !reports.is_empty(),
            "none of the detected browser cookie stores could be read"
        );
        for (index, (source, local, vault)) in reports.iter().enumerate() {
            if index > 0 {
                println!();
            }
            print_cookie_names(
                source.profile_kind().name(),
                &self.origin,
                local.as_ref(),
                vault.as_ref(),
            )?;
        }
        Ok(())
    }
}

impl Sync {
    async fn run(self) -> Result<()> {
        let codex_home = default_codex_home()?;
        let credential = load_browser_cookie_sync_credential(&codex_home, &self.origin)
            .await?
            .ok_or_else(|| {
                eyre::eyre!(
                    "Nanocodex Connect is not logged in; run `nanocodex login --browser-cookie-origin={}` first",
                    self.origin
                )
            })?;
        let mut prepared = Vec::new();
        for (source, session) in detected_cookie_sessions(self.cookie_auth)? {
            let profile_key = local_profile_key(source, &session)?;
            let store_id = source.store_id();
            let capture = match capture_local_cookies(&self.origin, source, session).await {
                Ok(capture) => capture,
                Err(_) => {
                    eprintln!(
                        "Skipped {} because its cookie store could not be read.",
                        source.profile_kind().name()
                    );
                    continue;
                }
            };
            let cookies = project_cookies(capture, &store_id)?;
            prepared.push((source, profile_key, store_id, cookies));
        }
        ensure!(
            !prepared.is_empty(),
            "none of the detected browser cookie stores could be read"
        );
        for (source, profile_key, store_id, cookies) in prepared {
            let profile_id = local_profile_id(&codex_home, &profile_key)?;
            sync_vault_cookie_jar(
                &credential,
                &self.origin,
                source,
                &profile_id,
                &store_id,
                &cookies,
            )
            .await?;
        }
        Ok(())
    }
}

async fn sync_vault_cookie_jar(
    credential: &ScopedManagedCredential,
    origin: &str,
    source: CookieSource,
    profile_id: &str,
    store_id: &str,
    cookies: &[BrowserCookieWire],
) -> Result<()> {
    let client = http_client()?;
    let mut list_url = credential.origin().join(COOKIE_JARS_PATH)?;
    list_url
        .query_pairs_mut()
        .append_pair("origin", origin)
        .append_pair("profile_id", profile_id)
        .append_pair("store_id", store_id);
    let listed = client
        .get(list_url.clone())
        .bearer_auth(credential.bearer_token())
        .header("origin", APP_ORIGIN)
        .header("x-nanocodex-app-id", APP_ID)
        .send()
        .await
        .wrap_err("failed to list browser cookie jars")?;
    require_response_url(&listed, &list_url)?;
    ensure_success(&listed, "list browser cookie jars")?;
    let listed: BrowserCookieJarList = response_json(listed).await?;
    ensure!(
        listed.browser_cookie_jars.len() <= 1,
        "Vault returned multiple cookie jars for the same origin and local profile"
    );
    let (jar_id, revision) = match listed.browser_cookie_jars.into_iter().next() {
        Some(metadata) => {
            metadata.validate(origin, profile_id, store_id)?;
            (metadata.id, metadata.revision)
        }
        None => (random_opaque_id(), 0),
    };
    let request = BrowserCookieJarUpsert {
        schema_version: 1,
        origin,
        profile_id,
        store_id,
        revision,
        cookies,
    };
    let encoded = serde_json::to_vec(&request).wrap_err("failed to encode cookie jar")?;
    ensure!(
        encoded.len() <= MAX_REQUEST_BYTES,
        "cookie jar is {} bytes, above the {}-byte Vault limit",
        encoded.len(),
        MAX_REQUEST_BYTES
    );
    let item_url = credential
        .origin()
        .join(&format!("{COOKIE_JARS_PATH}/{jar_id}"))?;
    let saved = client
        .put(item_url.clone())
        .bearer_auth(credential.bearer_token())
        .header("origin", APP_ORIGIN)
        .header("x-nanocodex-app-id", APP_ID)
        .header("content-type", "application/json")
        .body(encoded)
        .send()
        .await
        .wrap_err("failed to upload browser cookie jar")?;
    require_response_url(&saved, &item_url)?;
    if saved.status() == StatusCode::CONFLICT {
        let code = response_json::<VaultConflict>(saved)
            .await
            .ok()
            .map(|conflict| conflict.error);
        match code.as_deref() {
            Some("browser_cookie_jar_limit_reached") => {
                bail!("Vault already contains the maximum number of browser cookie jars")
            }
            Some("browser_cookie_jar_binding_conflict") => {
                bail!("Vault cookie jar identity is already bound to another profile")
            }
            _ => bail!("Vault cookie jar changed concurrently; rerun the sync command"),
        }
    }
    ensure_success(&saved, "upload browser cookie jar")?;
    let saved: BrowserCookieJarMetadata = response_json(saved).await?;
    saved.validate(origin, profile_id, store_id)?;
    ensure!(
        saved.id == jar_id,
        "Vault returned another cookie jar identity"
    );
    ensure!(
        saved.cookie_count == cookies.len(),
        "Vault returned a different cookie count"
    );
    println!(
        "Synced {} {} cookies for {} to Vault (revision {}).",
        cookies.len(),
        source.profile_kind().name(),
        origin,
        saved.revision
    );
    Ok(())
}

#[derive(Serialize)]
struct BrowserCookieJarUpsert<'a> {
    schema_version: u8,
    origin: &'a str,
    profile_id: &'a str,
    store_id: &'a str,
    revision: u64,
    cookies: &'a [BrowserCookieWire],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserCookieWire {
    name: String,
    value: String,
    domain: String,
    path: String,
    host_only: bool,
    secure: bool,
    http_only: bool,
    same_site: String,
    session: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expiration_date: Option<f64>,
    store_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    partition_key: Option<BrowserCookiePartitionWire>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserCookiePartitionWire {
    top_level_site: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    has_cross_site_ancestor: Option<bool>,
}

fn local_cookie_session(source: CookieSource, authorization: CookieAuth) -> Result<BraveSession> {
    Ok(BraveSession::standard_cookie_for(source.profile_kind())?
        .cookie_authorization(authorization.into()))
}

fn detected_cookie_sessions(
    authorization: CookieAuth,
) -> Result<Vec<(CookieSource, BraveSession)>> {
    let sessions = CookieSource::ALL
        .into_iter()
        .filter_map(|source| {
            local_cookie_session(source, authorization)
                .ok()
                .map(|session| (source, session))
        })
        .collect::<Vec<_>>();
    ensure!(
        !sessions.is_empty(),
        "no installed Brave, Chrome, Chromium, or Edge profile with cookies was found"
    );
    Ok(sessions)
}

fn local_profile_key(source: CookieSource, session: &BraveSession) -> Result<String> {
    let directory = session
        .selected_profile_directory()
        .to_str()
        .ok_or_else(|| eyre::eyre!("browser profile directory is not valid UTF-8"))?;
    Ok(format!("{}:{directory}", source.key()))
}

async fn capture_local_cookies(
    origin: &str,
    source: CookieSource,
    session: BraveSession,
) -> Result<BrowserOriginCookieCapture> {
    session
        .capture_origin_cookies(Url::parse(origin)?)
        .await
        .wrap_err_with(|| {
            format!(
                "failed to read {} cookies for {origin}",
                source.profile_kind().name(),
            )
        })
}

fn project_cookies(
    capture: BrowserOriginCookieCapture,
    store_id: &str,
) -> Result<Vec<BrowserCookieWire>> {
    ensure!(
        capture.cookies.len() <= MAX_COOKIE_COUNT,
        "{} cookies were selected, above the {MAX_COOKIE_COUNT}-cookie Vault limit",
        capture.cookies.len()
    );
    let mut identities = HashSet::with_capacity(capture.cookies.len());
    capture
        .cookies
        .into_iter()
        .map(|cookie| {
            validate_profile_cookie(&cookie)?;
            if let Some(partition) = &cookie.partition_key {
                ensure!(
                    canonical_browser_cookie_origin(&partition.top_level_site)
                        .is_ok_and(|origin| origin == partition.top_level_site),
                    "source cookie has an unsupported partition origin"
                );
            }
            let partition_key = cookie
                .partition_key
                .map(|partition| BrowserCookiePartitionWire {
                    top_level_site: partition.top_level_site,
                    has_cross_site_ancestor: Some(partition.has_cross_site_ancestor),
                });
            let identity = (
                cookie.name.clone(),
                cookie.domain.clone(),
                cookie.host_only,
                cookie.path.clone(),
                store_id.to_owned(),
                partition_key
                    .as_ref()
                    .map(|value| value.top_level_site.clone()),
                partition_key
                    .as_ref()
                    .and_then(|value| value.has_cross_site_ancestor),
            );
            ensure!(
                identities.insert(identity),
                "source profile contains a duplicate cookie identity"
            );
            Ok(BrowserCookieWire {
                name: cookie.name,
                value: cookie.value,
                domain: cookie.domain,
                path: cookie.path,
                host_only: cookie.host_only,
                secure: cookie.secure,
                http_only: cookie.http_only,
                same_site: match cookie.same_site {
                    Some(BrowserCookieSameSite::Strict) => "strict",
                    Some(BrowserCookieSameSite::Lax) => "lax",
                    Some(BrowserCookieSameSite::None) => "no_restriction",
                    None => "unspecified",
                }
                .to_owned(),
                session: cookie.session,
                expiration_date: cookie.expires_epoch_seconds,
                store_id: store_id.to_owned(),
                partition_key,
            })
        })
        .collect()
}

struct CookieNameList {
    cookie_count: usize,
    names: BTreeSet<String>,
}

struct VaultCookieNames {
    jar_present: bool,
    revision: Option<u64>,
    list: CookieNameList,
}

fn empty_vault_cookie_names() -> VaultCookieNames {
    VaultCookieNames {
        jar_present: false,
        revision: None,
        list: CookieNameList {
            cookie_count: 0,
            names: BTreeSet::new(),
        },
    }
}

#[derive(Serialize)]
struct BrowserCookieJarBinding<'a> {
    origin: &'a str,
    profile_id: &'a str,
    store_id: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserCookieJarNames {
    id: String,
    origin: String,
    profile_id: String,
    store_id: String,
    revision: u64,
    updated_at: u64,
    cookie_count: usize,
    cookie_names: Vec<String>,
}

async fn fetch_vault_cookie_names(
    credential: &ScopedManagedCredential,
    origin: &str,
    profile_id: &str,
    store_id: &str,
) -> Result<VaultCookieNames> {
    let client = http_client()?;
    let mut list_url = credential.origin().join(COOKIE_JARS_PATH)?;
    list_url
        .query_pairs_mut()
        .append_pair("origin", origin)
        .append_pair("profile_id", profile_id)
        .append_pair("store_id", store_id);
    let listed = client
        .get(list_url.clone())
        .bearer_auth(credential.bearer_token())
        .header("origin", APP_ORIGIN)
        .header("x-nanocodex-app-id", APP_ID)
        .send()
        .await
        .wrap_err("failed to list browser cookie jars")?;
    require_response_url(&listed, &list_url)?;
    ensure_success(&listed, "list browser cookie jars")?;
    let listed: BrowserCookieJarList = response_json(listed).await?;
    ensure!(
        listed.browser_cookie_jars.len() <= 1,
        "Vault returned multiple cookie jars for the same origin and local profile"
    );
    let Some(metadata) = listed.browser_cookie_jars.into_iter().next() else {
        return Ok(VaultCookieNames {
            jar_present: false,
            revision: None,
            list: CookieNameList {
                cookie_count: 0,
                names: BTreeSet::new(),
            },
        });
    };
    metadata.validate(origin, profile_id, store_id)?;

    let item_url = credential
        .origin()
        .join(&format!("{COOKIE_JARS_PATH}/{}/names", metadata.id))?;
    let names = client
        .post(item_url.clone())
        .bearer_auth(credential.bearer_token())
        .header("origin", APP_ORIGIN)
        .header("x-nanocodex-app-id", APP_ID)
        .header("content-type", "application/json")
        .json(&BrowserCookieJarBinding {
            origin,
            profile_id,
            store_id,
        })
        .send()
        .await
        .wrap_err("failed to list Vault cookie names")?;
    require_response_url(&names, &item_url)?;
    ensure_success(&names, "list Vault cookie names")?;
    require_no_store(&names)?;
    let names: BrowserCookieJarNames = response_json(names).await?;
    ensure!(
        names.id == metadata.id,
        "Vault returned another cookie jar identity"
    );
    ensure!(
        names.origin == origin,
        "Vault returned another cookie origin"
    );
    ensure!(
        names.profile_id == profile_id,
        "Vault returned another browser profile"
    );
    ensure!(
        names.store_id == store_id,
        "Vault returned another cookie store"
    );
    ensure!(
        names.revision > 0 && names.cookie_count <= MAX_COOKIE_COUNT,
        "Vault returned invalid cookie name metadata"
    );
    let _ = (metadata.revision, metadata.cookie_count, names.updated_at);
    let mut unique = BTreeSet::new();
    let mut previous: Option<String> = None;
    for name in names.cookie_names {
        validate_cookie_name(&name, "Vault")?;
        ensure!(
            previous.as_ref().is_none_or(|value| value < &name),
            "Vault returned unsorted or duplicate cookie names"
        );
        previous = Some(name.clone());
        ensure!(unique.insert(name), "Vault returned duplicate cookie names");
    }
    ensure!(
        unique.len() <= names.cookie_count,
        "Vault returned more cookie names than cookie identities"
    );
    Ok(VaultCookieNames {
        jar_present: true,
        revision: Some(names.revision),
        list: CookieNameList {
            cookie_count: names.cookie_count,
            names: unique,
        },
    })
}

fn print_cookie_names(
    browser_name: &str,
    origin: &str,
    local: Option<&CookieNameList>,
    vault: Option<&VaultCookieNames>,
) -> Result<()> {
    println!("Cookie names for {origin} ({browser_name}); values are never printed.");
    println!("NAME\tLOCAL\tVAULT");
    let names = local
        .into_iter()
        .flat_map(|value| value.names.iter())
        .chain(vault.into_iter().flat_map(|value| value.list.names.iter()))
        .collect::<BTreeSet<_>>();
    for name in names {
        let local_status = local.map_or("-", |value| {
            if value.names.contains(name) {
                "yes"
            } else {
                "no"
            }
        });
        let vault_status = vault.map_or("-", |value| {
            if value.list.names.contains(name) {
                "yes"
            } else {
                "no"
            }
        });
        println!(
            "{}\t{local_status}\t{vault_status}",
            terminal_safe_cookie_name(name)
        );
    }
    let local_summary = local
        .map(|value| {
            format!(
                "{} identities / {} unique names",
                value.cookie_count,
                value.names.len()
            )
        })
        .unwrap_or_else(|| "not requested".to_owned());
    let vault_summary = vault.map_or_else(
        || "not requested".to_owned(),
        |value| match (value.jar_present, value.revision) {
            (true, Some(revision)) => format!(
                "{} identities / {} unique names / revision {revision}",
                value.list.cookie_count,
                value.list.names.len()
            ),
            _ => "no jar for this exact origin/profile/store".to_owned(),
        },
    );
    println!("Summary: local: {local_summary}; Vault: {vault_summary}.");
    Ok(())
}

fn cookie_name_list(cookies: &[BrowserCookieWire]) -> CookieNameList {
    CookieNameList {
        cookie_count: cookies.len(),
        names: cookies.iter().map(|cookie| cookie.name.clone()).collect(),
    }
}

fn terminal_safe_cookie_name(name: &str) -> String {
    let mut output = String::from("\"");
    for character in name.chars() {
        if character.is_ascii_graphic() && !matches!(character, '\\' | '"') {
            output.push(character);
        } else {
            for unit in character.encode_utf16(&mut [0; 2]) {
                output.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    output.push('"');
    output
}

fn validate_profile_cookie(cookie: &BrowserProfileCookie) -> Result<()> {
    validate_cookie_name(&cookie.name, "source")?;
    ensure!(
        cookie.value.len() <= 16 * 1024 && !has_control(&cookie.value),
        "source cookie has an invalid value"
    );
    ensure!(
        cookie.domain.len() <= 253 && valid_cookie_domain(&cookie.domain),
        "source cookie has an invalid domain"
    );
    ensure!(
        cookie.path.len() <= 2_048 && cookie.path.starts_with('/') && !has_control(&cookie.path),
        "source cookie has an invalid path"
    );
    ensure!(
        cookie.session == cookie.expires_epoch_seconds.is_none(),
        "source cookie has inconsistent expiration metadata"
    );
    if let Some(expiration) = cookie.expires_epoch_seconds {
        ensure!(
            expiration.is_finite() && expiration > 0.0,
            "source cookie has an invalid expiration"
        );
    }
    Ok(())
}

fn validate_cookie_name(name: &str, source: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 4_096,
        "{source} cookie has an invalid name"
    );
    ensure!(
        !name.chars().any(|character| {
            character.is_ascii_control() || "()<>@,;:\\\"/[]?={}".contains(character)
        }),
        "{source} cookie has an invalid name"
    );
    Ok(())
}

fn has_control(value: &str) -> bool {
    value.chars().any(|character| character.is_ascii_control())
}

fn valid_cookie_domain(value: &str) -> bool {
    let bare = value.strip_prefix('.').unwrap_or(value);
    !bare.is_empty()
        && value == value.to_ascii_lowercase()
        && bare.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserCookieJarList {
    browser_cookie_jars: Vec<BrowserCookieJarMetadata>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultConflict {
    error: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserCookieJarMetadata {
    id: String,
    origin: String,
    profile_id: String,
    store_id: String,
    revision: u64,
    cookie_count: usize,
    updated_at: u64,
}

impl BrowserCookieJarMetadata {
    fn validate(&self, origin: &str, profile_id: &str, store_id: &str) -> Result<()> {
        ensure!(
            valid_opaque_id(&self.id),
            "Vault returned an invalid cookie jar identity"
        );
        ensure!(
            self.origin == origin,
            "Vault returned another cookie origin"
        );
        ensure!(
            self.profile_id == profile_id,
            "Vault returned another browser profile"
        );
        ensure!(
            self.store_id == store_id,
            "Vault returned another cookie store"
        );
        ensure!(
            self.revision > 0,
            "Vault returned an invalid cookie jar revision"
        );
        ensure!(
            self.cookie_count <= MAX_COOKIE_COUNT,
            "Vault returned an invalid cookie count"
        );
        let _ = self.updated_at;
        Ok(())
    }
}

fn valid_opaque_id(value: &str) -> bool {
    (22..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn random_opaque_id() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BrowserProfileState {
    version: u8,
    profiles: BTreeMap<String, String>,
}

fn local_profile_id(codex_home: &Path, profile_key: &str) -> Result<String> {
    fs::create_dir_all(codex_home)?;
    let _lock = lock_browser_profile_state(codex_home, false)?;
    let mut state = read_browser_profile_state(codex_home)?.unwrap_or(BrowserProfileState {
        version: 1,
        profiles: BTreeMap::new(),
    });
    if let Some(profile_id) = state.profiles.get(profile_key) {
        ensure!(
            valid_profile_id(profile_id),
            "stored browser profile identity is invalid"
        );
        return Ok(profile_id.clone());
    }
    let profile_id = format!("local-{}", uuid::Uuid::new_v4());
    state
        .profiles
        .insert(profile_key.to_owned(), profile_id.clone());
    let path = codex_home.join("browser-cookie-profiles.json");
    let encoded = serde_json::to_vec(&state)?;
    let mut temporary = tempfile::NamedTempFile::new_in(codex_home)?;
    set_private_file(temporary.as_file(), temporary.path())?;
    temporary.write_all(&encoded)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| error.error)?;
    if let Ok(directory) = fs::File::open(codex_home) {
        let _ = directory.sync_all();
    }
    Ok(profile_id)
}

fn existing_local_profile_id(codex_home: &Path, profile_key: &str) -> Result<Option<String>> {
    fs::create_dir_all(codex_home)?;
    let _lock = lock_browser_profile_state(codex_home, true)?;
    let Some(state) = read_browser_profile_state(codex_home)? else {
        return Ok(None);
    };
    let Some(profile_id) = state.profiles.get(profile_key) else {
        return Ok(None);
    };
    ensure!(
        valid_profile_id(profile_id),
        "stored browser profile identity is invalid"
    );
    Ok(Some(profile_id.clone()))
}

fn lock_browser_profile_state(codex_home: &Path, shared: bool) -> Result<fs::File> {
    let path = codex_home.join(PROFILE_STATE_LOCK);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        ensure!(
            !metadata.file_type().is_symlink(),
            "browser cookie profile lock must not be a symbolic link"
        );
    }
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => {
            set_private_file(&file, &path)?;
            file
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            OpenOptions::new().read(true).write(true).open(&path)?
        }
        Err(error) => return Err(error.into()),
    };
    ensure_private_file(&file, &path)?;
    if shared {
        fs2::FileExt::lock_shared(&file)?;
    } else {
        file.lock_exclusive()?;
    }
    Ok(file)
}

fn read_browser_profile_state(codex_home: &Path) -> Result<Option<BrowserProfileState>> {
    let path = codex_home.join("browser-cookie-profiles.json");
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        ensure!(
            !metadata.file_type().is_symlink(),
            "browser cookie profile record must not be a symbolic link"
        );
    }
    match fs::File::open(&path) {
        Ok(mut file) => {
            ensure_private_file(&file, &path)?;
            ensure!(
                file.metadata()?.len() <= PROFILE_STATE_BYTES,
                "browser cookie profile record is too large"
            );
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            let value: BrowserProfileState =
                serde_json::from_slice(&bytes).wrap_err("invalid browser cookie profile record")?;
            ensure!(
                value.version == 1,
                "unsupported browser cookie profile record version"
            );
            Ok(Some(value))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).wrap_err_with(|| format!("failed to open {}", path.display())),
    }
}

fn valid_profile_id(value: &str) -> bool {
    value
        .strip_prefix("local-")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .is_some()
}

#[cfg(unix)]
fn ensure_private_file(file: &fs::File, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = file.metadata()?.permissions().mode() & 0o777;
    ensure!(
        mode == 0o600,
        "browser cookie profile file {} must have Unix permissions 0600",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_file(_file: &fs::File, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(file: &fs::File, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .wrap_err_with(|| format!("failed to protect {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_file(_file: &fs::File, _path: &Path) -> Result<()> {
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

fn require_response_url(response: &Response, expected: &Url) -> Result<()> {
    ensure!(
        response.url() == expected,
        "Connect response changed URL or origin"
    );
    Ok(())
}

fn ensure_success(response: &Response, operation: &str) -> Result<()> {
    ensure!(
        response.status().is_success(),
        "failed to {operation} ({})",
        response.status()
    );
    Ok(())
}

fn require_no_store(response: &Response) -> Result<()> {
    let cache_control = response
        .headers()
        .get(reqwest::header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    ensure!(
        cache_control
            .split(',')
            .any(|directive| directive.trim().eq_ignore_ascii_case("no-store")),
        "Vault cookie materialization response is missing Cache-Control: no-store"
    );
    Ok(())
}

async fn response_json<T: DeserializeOwned>(response: Response) -> Result<T> {
    ensure!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("application/json")),
        "Connect returned a non-JSON response"
    );
    if let Some(length) = response.content_length() {
        ensure!(
            length <= MAX_RESPONSE_BYTES as u64,
            "Connect response is too large"
        );
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.wrap_err("failed to read Connect response")?;
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= MAX_RESPONSE_BYTES,
            "Connect response is too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).wrap_err("Connect returned invalid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_cookie(name: &str, path: &str, value: &str) -> BrowserCookieWire {
        BrowserCookieWire {
            name: name.to_owned(),
            value: value.to_owned(),
            domain: ".example.com".to_owned(),
            path: path.to_owned(),
            host_only: false,
            secure: true,
            http_only: true,
            same_site: "lax".to_owned(),
            session: true,
            expiration_date: None,
            store_id: "local-brave".to_owned(),
            partition_key: None,
        }
    }

    #[test]
    fn cookie_projection_preserves_vault_fields_without_debugging_values() {
        let capture = BrowserOriginCookieCapture {
            origin: Url::parse("https://console.example.com").unwrap(),
            cookies: vec![BrowserProfileCookie {
                name: "session".to_owned(),
                value: "top-secret".to_owned(),
                domain: ".example.com".to_owned(),
                path: "/".to_owned(),
                host_only: false,
                secure: true,
                http_only: true,
                same_site: Some(BrowserCookieSameSite::Lax),
                session: false,
                expires_epoch_seconds: Some(1_900_000_000.0),
                partition_key: None,
            }],
        };
        let cookies = project_cookies(capture, "local-brave").unwrap();
        let value = serde_json::to_value(&cookies[0]).unwrap();
        assert_eq!(value["sameSite"], "lax");
        assert_eq!(value["expirationDate"], 1_900_000_000.0);
        assert_eq!(value["storeId"], "local-brave");
        assert!(
            format!(
                "{:?}",
                BrowserOriginCookieCapture {
                    origin: Url::parse("https://example.com").unwrap(),
                    cookies: vec![],
                }
            )
            .contains("cookie_count")
        );
    }

    #[test]
    fn profile_identity_is_stable_opaque_and_private() {
        let directory = tempfile::tempdir().unwrap();
        let first = local_profile_id(directory.path(), "brave:Default").unwrap();
        let second = local_profile_id(directory.path(), "brave:Default").unwrap();
        let other_profile = local_profile_id(directory.path(), "brave:Profile 1").unwrap();
        let chrome = local_profile_id(directory.path(), "chrome:Default").unwrap();
        assert_eq!(first, second);
        assert_ne!(first, other_profile);
        assert_ne!(first, chrome);
        assert!(valid_profile_id(&first));
        assert!(!first.contains(&directory.path().display().to_string()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(directory.path().join("browser-cookie-profiles.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn cookie_name_projection_deduplicates_names_but_retains_identity_count() {
        let cookies = vec![
            wire_cookie("session", "/", "first-secret"),
            wire_cookie("session", "/admin", "second-secret"),
            wire_cookie("csrf", "/", "third-secret"),
        ];
        let names = cookie_name_list(&cookies);
        assert_eq!(names.cookie_count, 3);
        assert_eq!(
            names.names.into_iter().collect::<Vec<_>>(),
            vec!["csrf", "session"]
        );
    }

    #[test]
    fn cookie_names_are_terminal_safe_and_never_include_values() {
        let escaped = terminal_safe_cookie_name("safe\u{202e}\u{2066}\u{200b}name");
        assert_eq!(escaped, "\"safe\\u202e\\u2066\\u200bname\"");
        assert!(!escaped.contains('\u{202e}'));
        assert!(!escaped.contains("first-secret"));
    }

    #[test]
    fn looking_up_a_missing_profile_identity_does_not_create_state() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            existing_local_profile_id(directory.path(), "brave").unwrap(),
            None
        );
        assert!(
            !directory
                .path()
                .join("browser-cookie-profiles.json")
                .exists()
        );
    }
}
