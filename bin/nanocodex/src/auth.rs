use std::{path::PathBuf, process::Command};

use clap::{Args, Subcommand};
use eyre::{Result, WrapErr, eyre};
use nanocodex::oai::auth::{ChatGptLogin, chatgpt_auth_status, logout_chatgpt};
use nanocodex_anthropic::{
    AnthropicLogin, AnthropicOAuthConfig, anthropic_auth_status, default_anthropic_auth_file,
    logout_anthropic,
};

use crate::config::default_auth_file;

#[derive(Args)]
pub(crate) struct Auth {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Sign Nanocodex in with a subscription account.
    Login(AuthArgs),
    /// Show the locally selected account without displaying tokens.
    Status(AuthArgs),
    /// Remove the selected locally stored credentials.
    Logout(AuthArgs),
}

#[derive(Args)]
struct AuthArgs {
    #[command(flatten)]
    file: AuthFile,

    /// Select the Nanocodex-owned Anthropic subscription login.
    #[arg(long)]
    anthropic: bool,
}

#[derive(Args)]
struct AuthFile {
    /// Override the shared Codex `auth.json` credential file.
    #[arg(long, env = "NANOCODEX_AUTH_FILE", conflicts_with = "anthropic")]
    auth_file: Option<PathBuf>,
}

impl Auth {
    pub(crate) async fn run(self) -> Result<()> {
        match self.command {
            AuthCommand::Login(args) if args.anthropic => anthropic_login().await,
            AuthCommand::Login(args) => login(args.file.path()?).await,
            AuthCommand::Status(args) if args.anthropic => anthropic_status().await,
            AuthCommand::Status(args) => status(&args.file.path()?),
            AuthCommand::Logout(args) if args.anthropic => anthropic_logout(),
            AuthCommand::Logout(args) => logout(&args.file.path()?),
        }
    }
}

impl AuthFile {
    fn path(self) -> Result<PathBuf> {
        self.auth_file.map_or_else(default_auth_file, Ok)
    }
}

async fn login(auth_file: PathBuf) -> Result<()> {
    let login = ChatGptLogin::start(&auth_file)
        .await
        .wrap_err("failed to start ChatGPT login")?;
    let url = login.authorization_url().to_owned();
    eprintln!("Open this URL to sign in with ChatGPT:\n\n{url}\n");
    if let Err(error) = open_browser(&url) {
        eprintln!("Could not open a browser automatically ({error}). Open the URL above manually.");
    }
    let account = login
        .complete()
        .await
        .wrap_err("ChatGPT login did not complete")?;
    eprintln!(
        "Codex and Nanocodex are logged in{} (account {}). Credentials saved to {}.",
        account
            .email
            .as_deref()
            .map_or(String::new(), |email| format!(" as {email}")),
        account.account_id,
        auth_file.display()
    );
    Ok(())
}

fn status(auth_file: &PathBuf) -> Result<()> {
    let account = chatgpt_auth_status(auth_file)
        .wrap_err_with(|| format!("could not load {}", auth_file.display()))?;
    println!("Logged in with ChatGPT");
    if let Some(email) = account.email {
        println!("Email: {email}");
    }
    if let Some(plan) = account.plan {
        println!("Plan: {plan}");
    }
    println!("Account: {}", account.account_id);
    println!("FedRAMP: {}", account.fedramp);
    println!("Credentials: {}", auth_file.display());
    Ok(())
}

fn logout(auth_file: &PathBuf) -> Result<()> {
    if logout_chatgpt(auth_file)? {
        eprintln!(
            "Removed shared ChatGPT credentials from {}. Codex and Nanocodex are logged out.",
            auth_file.display()
        );
    } else {
        eprintln!(
            "No ChatGPT credentials were stored at {}.",
            auth_file.display()
        );
    }
    Ok(())
}

async fn anthropic_login() -> Result<()> {
    let auth_file = default_anthropic_auth_file()
        .ok_or_else(|| eyre!("could not determine where to store Anthropic credentials"))?;
    let login = AnthropicLogin::start_with_config(AnthropicOAuthConfig::from_env(), &auth_file)
        .await
        .wrap_err("failed to start Anthropic login")?;
    let url = login.authorization_url().to_owned();
    eprintln!("Open this URL to sign in with Anthropic:\n\n{url}\n");
    if let Err(error) = open_browser(&url) {
        eprintln!("Could not open a browser automatically ({error}). Open the URL above manually.");
    }
    let account = login
        .complete()
        .await
        .wrap_err("Anthropic login did not complete")?;
    let profile = account
        .email
        .or(account.account_id)
        .unwrap_or_else(|| "your Anthropic account".to_owned());
    eprintln!(
        "Nanocodex is logged in as {profile}. Credentials saved to {}.",
        auth_file.display()
    );
    Ok(())
}

async fn anthropic_status() -> Result<()> {
    let account = anthropic_auth_status()
        .await
        .wrap_err("could not resolve Anthropic credentials")?;
    println!("Logged in with Anthropic");
    println!("Mode: {:?}", account.mode);
    println!("Source: {}", account.source);
    if let Some(profile) = account.profile {
        println!("Profile: {profile}");
    }
    if let Some(seconds) = account.expires_in_seconds {
        if seconds > 0 {
            println!("Expires in: {}m {}s", seconds / 60, seconds % 60);
        } else {
            println!("Expires in: expired (refreshes on next use)");
        }
    }
    Ok(())
}

fn anthropic_logout() -> Result<()> {
    let auth_file = default_anthropic_auth_file()
        .ok_or_else(|| eyre!("could not determine the Anthropic credential path"))?;
    if logout_anthropic(&auth_file)? {
        eprintln!(
            "Removed Nanocodex Anthropic credentials from {}.",
            auth_file.display()
        );
    } else {
        eprintln!(
            "No Nanocodex Anthropic credentials were stored at {}.",
            auth_file.display()
        );
    }
    Ok(())
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "automatic browser launch is unsupported on this platform",
    ));

    command.arg(url);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "browser launcher exited with {status}"
        )))
    }
}
