//! Application-owned account authentication shared by both terminal clients.
//! Provider OAuth and Nanocodex Connect grants remain separate credentials.

mod http;
mod store;

use std::{
    env,
    io::{BufRead, Write},
    path::PathBuf,
    time::Instant,
};

use clap::{Args, Subcommand};
use nanocodex_managed::{ManagedApiKey, ManagedClient, ManagedError};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

pub const DEFAULT_MANAGED_ORIGIN: &str = "https://nanocodex.gakonst.workers.dev";
const API_KEY_ENVS: [&str; 2] = ["NANOCODEX_API_KEY", "NC_API_KEY"];
type Result<T> = std::result::Result<T, Error>;

/// Errors contain only local messages, never server bodies or credentials.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    message: String,
    status: Option<u16>,
    retry_code: bool,
}

impl Error {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: None,
            retry_code: false,
        }
    }

    fn response(status: u16, body: &Value, headers: &reqwest::header::HeaderMap) -> Self {
        let code = body["error"].as_str().unwrap_or_default();
        if status == 429 || code == "rate_limited" {
            let seconds = headers
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .or_else(|| body["retry_after"].as_u64())
                .unwrap_or(60)
                .clamp(1, 3600);
            return Self {
                message: format!("Please wait {seconds} seconds before trying again"),
                status: Some(status),
                retry_code: false,
            };
        }
        let message = match code {
            "invalid_phone" => {
                "Enter a phone number with its country code, such as +1 415 555 0123"
            }
            "invalid_otp" => "Enter the six-digit code from your text message",
            "invalid_or_expired_otp" => {
                "That code is incorrect or expired; try again or run login to request another"
            }
            "sms_otp_unavailable" => "Phone sign-in is temporarily unavailable; try again shortly",
            "sms_delivery_failed" => "Could not send your code; check your number and try again",
            "sms_verification_failed" => "Could not check your code; try again",
            "sms_identity_unavailable" => "Could not finish signing you in; try again",
            "wallet_unavailable" => {
                "Your account is still being prepared; try the code again shortly"
            }
            "unauthorized" | "reauthentication_required" => {
                "Your account credential or sign-in expired; run login again"
            }
            "forbidden" => {
                "This account cannot authorize the CLI; contact your account administrator"
            }
            _ => "Account request failed; try again",
        };
        Self {
            message: message.to_owned(),
            status: Some(status),
            retry_code: matches!(
                code,
                "invalid_otp"
                    | "invalid_or_expired_otp"
                    | "wallet_unavailable"
                    | "sms_verification_failed"
            ),
        }
    }
}

#[derive(Args)]
pub struct Account {
    #[command(subcommand)]
    command: AccountCommand,
}

#[derive(Subcommand)]
pub enum AccountCommand {
    /// Sign in with an SMS code, or import an account API key from stdin.
    Login(Login),
    /// Verify the selected account credential without displaying secrets.
    Status(Options),
    /// Remove the saved account credential on this machine.
    Logout(Options),
}

#[derive(Args)]
pub struct Options {
    /// Managed account origin (HTTPS, or loopback HTTP for local development).
    #[arg(long, env = "NANOCODEX_MANAGED_URL")]
    managed_url: Option<String>,
    /// Shared private credential file; defaults to $CODEX_HOME/nanocodex-account.json.
    #[arg(long, env = "NANOCODEX_ACCOUNT_FILE")]
    account_file: Option<PathBuf>,
}

#[derive(Args)]
pub struct Login {
    #[command(flatten)]
    options: Options,
    /// Phone number including country code. Prompted when omitted.
    #[arg(long, conflicts_with = "with_api_key")]
    phone: Option<String>,
    /// Read an existing account-issued ncx_live key from stdin instead of SMS.
    #[arg(long)]
    with_api_key: bool,
    /// Name for the newly issued key in your account's API Keys menu.
    #[arg(long, default_value = "Nanocodex CLI", conflicts_with = "with_api_key")]
    label: String,
}

impl Account {
    pub async fn run(self) -> Result<()> {
        self.command.run().await
    }
}

impl AccountCommand {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::Login(login) => login.run().await,
            Self::Status(options) => status(options).await,
            Self::Logout(options) => logout(options),
        }
    }
}

impl Options {
    fn resolve(self) -> Result<(String, PathBuf)> {
        let origin = self
            .managed_url
            .as_deref()
            .unwrap_or(DEFAULT_MANAGED_ORIGIN);
        let path = self.account_file.map_or_else(store::default_path, Ok)?;
        if path.as_os_str().is_empty() {
            return Err(Error::message(
                "Account credential file path must not be empty",
            ));
        }
        Ok((canonical_origin(origin)?, path))
    }
}

impl Login {
    pub async fn run(self) -> Result<()> {
        let (origin, path) = self.options.resolve()?;
        let label = self.label.trim();
        if label.is_empty() || label.chars().count() > 120 || label.chars().any(char::is_control) {
            return Err(Error::message(
                "Key label must contain 1–120 characters without control characters",
            ));
        }
        let _lock = store::lock(&path)?;
        let mut store = store::load(&path)?;
        let mut session = http::Session::new(origin.clone())?;
        let cancel = CancellationToken::new();
        let signal_cancel = cancel.clone();
        let signal = tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            signal_cancel.cancel();
            eprintln!("Cancelling sign-in; finishing any pending account request...");
        });
        let mut input = Input::new();
        let result = async {
            let key = if self.with_api_key {
                let key = zeroize::Zeroizing::new(
                    input
                        .line("Account API key (stdin): ", &cancel, None)
                        .await?,
                );
                validate_key(&key)?;
                session.identity(&key).await?;
                key
            } else {
                let phone = match self.phone {
                    Some(phone) => phone,
                    None => {
                        input
                            .line("Phone number (include country code): ", &cancel, None)
                            .await?
                    }
                };
                let phone = normalize_phone(&phone)?;
                check_cancelled(&cancel)?;
                let challenge = session.start(phone).await?;
                check_cancelled(&cancel)?;
                eprintln!("Code sent. Enter the six-digit SMS code (Ctrl-C to cancel).");
                let mut verified = false;
                for _ in 0..3 {
                    let code = input
                        .line("SMS code: ", &cancel, Some(challenge.expires))
                        .await?;
                    let code: String = code
                        .chars()
                        .filter(|character| !character.is_whitespace())
                        .collect();
                    if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
                        eprintln!("Enter the six-digit code from your text message.");
                        continue;
                    }
                    // Finish each bounded request before observing cancellation:
                    // a verification or mint may already have committed remotely.
                    match session.verify(&challenge, &code).await {
                        Ok(()) => {
                            verified = true;
                            break;
                        }
                        Err(error) if error.retry_code => eprintln!("{error}"),
                        Err(error) => return Err(error),
                    }
                }
                check_cancelled(&cancel)?;
                if !verified {
                    return Err(Error::message(
                        "Sign-in did not complete; run login to request another code",
                    ));
                }
                session.mint(label).await?;
                zeroize::Zeroizing::new(
                    session
                        .minted_key
                        .as_ref()
                        .expect("mint retains the key")
                        .clone(),
                )
            };
            check_cancelled(&cancel)?;
            store.accounts.insert(
                origin.clone(),
                store::Credential {
                    api_key: key.to_string(),
                },
            );
            store::save(&path, &store)?;
            Ok(())
        }
        .await;
        if let Err(error) = session.finish(result.is_ok()).await {
            eprintln!("Warning: {error}");
        }
        signal.abort();
        result?;
        println!(
            "Signed in to {origin}. Account credential saved to {}.",
            path.display()
        );
        if API_KEY_ENVS.iter().any(|name| env::var_os(name).is_some()) {
            eprintln!(
                "NANOCODEX_API_KEY or NC_API_KEY is set and takes precedence over this saved login."
            );
        }
        Ok(())
    }
}

async fn status(options: Options) -> Result<()> {
    let (origin, path) = options.resolve()?;
    let Some((key, source)) = resolve_key(&origin, &path)? else {
        println!("{}", json!({"authenticated": false, "origin": origin}));
        return Ok(());
    };
    let account = http::Session::new(origin.clone())?.identity(&key).await?;
    println!(
        "{}",
        json!({"authenticated": true, "origin": origin, "source": source, "key_id": &key[9..21], "account": account})
    );
    Ok(())
}

fn logout(options: Options) -> Result<()> {
    let (origin, path) = options.resolve()?;
    let _lock = store::lock(&path)?;
    let mut store = store::load(&path)?;
    if store.accounts.remove(&origin).is_some() {
        store::save(&path, &store)?;
        println!("Removed the saved account login for {origin}.");
    } else {
        println!("No saved account login for {origin}.");
    }
    println!("To revoke the API key on the server, remove it in the account API Keys menu.");
    if API_KEY_ENVS.iter().any(|name| env::var_os(name).is_some()) {
        eprintln!(
            "NANOCODEX_API_KEY or NC_API_KEY is still set; unset it to stop using environment credentials."
        );
    }
    Ok(())
}

/// Resolve the CLI-selected cluster, falling back to an attached agent URL.
pub fn managed_url_from_environment(fallback: Option<&str>) -> Result<String> {
    let configured = optional_env("NANOCODEX_MANAGED_URL")?;
    canonical_origin(
        configured
            .as_deref()
            .or(fallback)
            .unwrap_or(DEFAULT_MANAGED_ORIGIN),
    )
}

/// Application credential selection stays outside the managed transport.
pub fn client_from_environment(
    fallback: Option<&str>,
) -> std::result::Result<ManagedClient, ManagedError> {
    let resolve = || -> Result<(String, ManagedApiKey)> {
        let origin = managed_url_from_environment(fallback)?;
        // An explicit environment key never requires a local credential file.
        let key = if let Some((key, _)) = env_key()? {
            key
        } else {
            resolve_key(&origin, &store::default_path()?)?.map(|(key, _)| key)
                .ok_or_else(|| Error::message("No account login for this origin; run nanocodex2 login (or nanocodex account login), or set NANOCODEX_API_KEY / NC_API_KEY to an account-issued ncx_live key"))?
        };
        let key = ManagedApiKey::parse(key.to_string())
            .map_err(|_| Error::message("Invalid account API key"))?;
        Ok((origin, key))
    };
    let (origin, key) =
        resolve().map_err(|error| ManagedError::Configuration(error.to_string()))?;
    ManagedClient::new(origin, key)
}

fn resolve_key(
    origin: &str,
    path: &std::path::Path,
) -> Result<Option<(zeroize::Zeroizing<String>, &'static str)>> {
    if let Some(key) = env_key()? {
        return Ok(Some(key));
    }
    Ok(store::load(path)?
        .accounts
        .get(origin)
        .map(|credential| (zeroize::Zeroizing::new(credential.api_key.clone()), "saved")))
}

fn env_key() -> Result<Option<(zeroize::Zeroizing<String>, &'static str)>> {
    for name in API_KEY_ENVS {
        if let Some(key) = optional_env(name)? {
            validate_key(&key)?;
            return Ok(Some((zeroize::Zeroizing::new(key), name)));
        }
    }
    Ok(None)
}

fn optional_env(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
        Ok(_) => Err(Error::message(format!("{name} must not be empty"))),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            Err(Error::message(format!("{name} must be valid Unicode")))
        }
    }
}

fn canonical_origin(value: &str) -> Result<String> {
    let url =
        Url::parse(value).map_err(|_| Error::message("Managed URL must be an HTTP(S) origin"))?;
    let loopback = match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::message(
            "Managed URL must be an HTTPS origin without credentials, path, query, or fragment (loopback HTTP is allowed)",
        ));
    }
    Ok(url.origin().ascii_serialization())
}

fn validate_key(value: &str) -> Result<()> {
    ManagedApiKey::parse(value.to_owned())
        .map(|_| ())
        .map_err(|_| Error::message("Expected an account-issued ncx_live API key"))
}

fn normalize_phone(value: &str) -> Result<String> {
    let phone: String = value
        .chars()
        .filter(|character| {
            !character.is_whitespace() && !matches!(character, '(' | ')' | '.' | '-')
        })
        .collect();
    let digits = phone.strip_prefix('+').unwrap_or_default();
    if !(8..=15).contains(&digits.len())
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::message(
            "Enter a phone number with its country code, such as +1 415 555 0123",
        ));
    }
    Ok(phone)
}

fn check_cancelled(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(Error::message("Sign-in cancelled"))
    } else {
        Ok(())
    }
}

struct Input(tokio::sync::mpsc::Receiver<Result<String>>);

impl Input {
    fn new() -> Self {
        let (send, receive) = tokio::sync::mpsc::channel(1);
        // A detached OS thread can block on stdin without making Tokio runtime
        // shutdown hang after Ctrl-C. Bound both the line and the queue.
        std::thread::spawn(move || {
            let mut input = std::io::stdin().lock();
            loop {
                use std::io::Read;
                let mut line = String::new();
                let result = match (&mut input).take(4097).read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) if line.len() <= 4096 => Ok(line.trim().to_owned()),
                    _ => Err(Error::message(
                        "Cannot read sign-in input, or input exceeds 4096 bytes",
                    )),
                };
                if send.blocking_send(result).is_err() {
                    break;
                }
            }
        });
        Self(receive)
    }

    async fn line(
        &mut self,
        prompt: &str,
        cancel: &CancellationToken,
        expires: Option<Instant>,
    ) -> Result<String> {
        eprint!("{prompt}");
        std::io::stderr()
            .flush()
            .map_err(|_| Error::message("Cannot write the sign-in prompt"))?;
        let deadline =
            expires.unwrap_or_else(|| Instant::now() + std::time::Duration::from_secs(300));
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::message("Sign-in cancelled")),
            _ = tokio::time::sleep_until(deadline.into()) => Err(Error::message("Sign-in timed out; run login again")),
            line = self.0.recv() => line.unwrap_or_else(|| Err(Error::message("Sign-in input closed; run login again"))),
        }
    }
}

#[cfg(test)]
mod tests;
