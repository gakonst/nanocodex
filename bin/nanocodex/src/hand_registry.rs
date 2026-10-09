//! Owner-facing account Hand registry: list what the account still remembers,
//! and evict entries the owner no longer wants routed. Removal is an account
//! operation, so it never requires the device itself to be reachable.

use std::time::Duration;

use eyre::{Result, eyre};
use reqwest::{
    Client, Method, StatusCode,
    header::{AUTHORIZATION, HeaderValue},
};
use serde_json::Value;

#[path = "hand_playback.rs"]
mod playback;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    /// List account Hands, including offline registrations.
    List,
    /// Remove an account Hand without requiring the device to be reachable.
    Forget {
        id: String,
        #[arg(long)]
        force: bool,
    },
    /// Remove only Hands observed definitively offline.
    Prune,
    /// Create or revoke a portable, view-only screen playback link.
    Stream(playback::Playback),
    /// List, revoke, rotate, or re-enroll Hand device keys.
    Devices {
        #[command(subcommand)]
        command: Option<nanocodex_bin_shared::device_identity::DevicesCommand>,
    },
}

impl Command {
    pub(crate) async fn run(self) -> Result<()> {
        match self {
            Self::List => list().await,
            Self::Forget { id, force } => forget(&id, force).await,
            Self::Prune => prune().await,
            Self::Stream(command) => command.run().await,
            Self::Devices { command } => command
                .unwrap_or(nanocodex_bin_shared::device_identity::DevicesCommand::List { json: false })
                .run()
                .await
                .map_err(|error| eyre!(error.to_string())),
        }
    }
}

struct Account {
    client: Client,
    origin: String,
}

fn account() -> Result<Account> {
    nanocodex::oai::transport::install_default_rustls_crypto_provider();
    let (origin, key) = nanocodex_cli_auth::optional_enrollment_credentials(None)?
        .ok_or_else(|| eyre!("Sign in first: nanocodex account login"))?;
    let mut authorization = HeaderValue::from_str(&format!("Bearer {}", key.as_str()))?;
    authorization.set_sensitive(true);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(AUTHORIZATION, authorization);
    let client = Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(REQUEST_TIMEOUT)
        .build()?;
    Ok(Account { client, origin })
}

impl Account {
    async fn send(&self, method: Method, path: &str) -> Result<(StatusCode, Value)> {
        let response = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .header("origin", &self.origin)
            .header("cache-control", "no-cache")
            .send()
            .await?;
        let status = response.status();
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }
}

/// Every Hand the account still remembers, including offline rows that keep
/// occupying the inventory until an owner evicts them.
pub(crate) async fn list() -> Result<()> {
    let (status, body) = account()?
        .send(Method::GET, "/v1/account/hands/inventory")
        .await?;
    if !status.is_success() {
        return Err(eyre!("Could not read the Hand inventory ({status})"));
    }
    let hands = body
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if hands.is_empty() {
        println!("No Hands are registered on this account.");
        return Ok(());
    }
    println!("{:<42}  {:<24}  {:<10}  HEALTH", "ID", "NAME", "KIND");
    for hand in &hands {
        let field = |key: &str| {
            hand.get(key)
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_string()
        };
        println!(
            "{:<42}  {:<24}  {:<10}  {}",
            field("id"),
            field("name"),
            field("kind"),
            field("health")
        );
    }
    if body.get("complete").and_then(Value::as_bool) == Some(false) {
        println!("\nThis listing is partial; some sources could not be read.");
    }
    Ok(())
}

/// Remove one Hand. A connected Hand is refused unless the owner forces it,
/// so a working machine is never cut loose by a mistyped identifier.
pub(crate) async fn forget(id: &str, force: bool) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id.as_bytes()[0].is_ascii_alphanumeric()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
    {
        return Err(eyre!("Invalid Hand identifier; use an ID from hand list"));
    }
    let path = if force {
        format!("/v1/account/hands/{id}?force=1")
    } else {
        format!("/v1/account/hands/{id}")
    };
    let (status, body) = account()?.send(Method::DELETE, &path).await?;
    if status == StatusCode::CONFLICT {
        if body.get("error").and_then(Value::as_str) == Some("hand_unknown") {
            return Err(eyre!(
                "{id} has unconfirmed connection status. Retry, or use --force to drop its account routing."
            ));
        }
        return Err(eyre!(
            "{id} is connected right now. Stop it, or re-run with --force to drop its account routing."
        ));
    }
    if status == StatusCode::NOT_FOUND {
        return Err(eyre!("This account has no Hand {id}."));
    }
    if !status.is_success() {
        return Err(eyre!("Could not forget {id} ({status})"));
    }
    if body.get("forgotten").and_then(Value::as_bool) == Some(true) {
        println!("Forgot Hand {id}.");
    } else {
        println!("Hand {id} was already absent from this account.");
    }
    Ok(())
}

/// Evict every Hand observed definitively offline. Hands whose state could not
/// be determined stay registered: absence of evidence is not an eviction.
pub(crate) async fn prune() -> Result<()> {
    let (status, body) = account()?
        .send(Method::POST, "/v1/account/hands/prune")
        .await?;
    if !status.is_success() {
        return Err(eyre!("Could not prune offline Hands ({status})"));
    }
    let forgotten = body
        .get("forgotten")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if forgotten.is_empty() {
        println!("No offline Hands to remove.");
    } else {
        for id in &forgotten {
            println!("Forgot {}", id.as_str().unwrap_or("-"));
        }
        println!("\nRemoved {} offline Hand(s).", forgotten.len());
    }
    if body.get("complete").and_then(Value::as_bool) == Some(false) {
        println!("Some sources could not be read; re-run to catch the rest.");
    }
    Ok(())
}
