use std::{path::PathBuf, process::Command};

use clap::{Args, Subcommand};
use eyre::{Result, WrapErr};
use nanocodex::oai::auth::{ChatGptLogin, logout_chatgpt, resolve_chatgpt_auth_status};

use crate::config::default_auth_file;

#[derive(Args)]
pub(crate) struct Auth {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Sign Codex and Nanocodex in with a `ChatGPT` subscription.
    Login(AuthLogin),
    /// Show the locally selected `ChatGPT` account without displaying tokens.
    Status(AuthFile),
    /// Remove the shared credentials, logging Codex and Nanocodex out.
    Logout(AuthFile),
}

#[derive(Args)]
struct AuthFile {
    /// Override the shared Codex `auth.json` credential file.
    #[arg(long, env = "NANOCODEX_AUTH_FILE")]
    auth_file: Option<PathBuf>,
}

#[derive(Args)]
struct AuthLogin {
    #[command(flatten)]
    auth: AuthFile,
    /// Print the authorization URL without opening a browser.
    #[arg(long)]
    no_open: bool,
}

impl Auth {
    pub(crate) async fn run(self) -> Result<()> {
        match self.command {
            AuthCommand::Login(args) => login(args.auth.path()?, !args.no_open).await,
            AuthCommand::Status(args) => status(&args.path()?).await,
            AuthCommand::Logout(args) => logout(&args.path()?),
        }
    }
}

impl AuthFile {
    fn path(self) -> Result<PathBuf> {
        self.auth_file.map_or_else(default_auth_file, Ok)
    }
}

async fn login(auth_file: PathBuf, open_automatically: bool) -> Result<()> {
    let login = ChatGptLogin::start(&auth_file)
        .await
        .wrap_err("failed to start ChatGPT login")?;
    let url = login.authorization_url().to_owned();
    eprintln!("Open this URL to sign in with ChatGPT:\n\n{url}\n");
    if open_automatically && let Err(error) = open_browser(&url) {
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

async fn status(auth_file: &PathBuf) -> Result<()> {
    let account = resolve_chatgpt_auth_status(auth_file)
        .await
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

pub(crate) fn open_browser(url: &str) -> std::io::Result<()> {
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
