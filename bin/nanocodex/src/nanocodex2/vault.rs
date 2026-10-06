//! Public SSH metadata and public-template CLI for broker-owned Vault requests.
use std::{
    fs::File,
    io::{self, Read},
    path::PathBuf,
};

use clap::{Args, Subcommand};
use nanocodex_managed::{ManagedClient, ManagedError, VAULT_REQUEST_MAX_BYTES, VaultRequest};

#[derive(Args)]
pub(super) struct Vault {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List saved SSH targets as a JSON array containing only public metadata.
    SshTargets,
    /// Send a public request template once; print only destination status and ok.
    ///
    /// JSON must contain vault_id and url, with optional method, headers, body,
    /// body_encoding and signing. Use Vault placeholders, never raw credentials.
    /// Input defaults to stdin. An unknown outcome must not be retried automatically.
    Request {
        /// Read public request JSON from this file instead of stdin.
        #[arg(long, conflicts_with = "stdin")]
        file: Option<PathBuf>,
        /// Read public request JSON from stdin (the default).
        #[arg(long)]
        stdin: bool,
    },
}

impl Vault {
    pub(super) async fn run(self, client: &ManagedClient) -> Result<(), ManagedError> {
        let file = match self.command {
            Command::SshTargets => {
                let targets = client.vault_ssh_targets().await?;
                let output = serde_json::to_string(&targets)
                    .map_err(|_| ManagedError::InvalidResponse("invalid Vault SSH metadata"))?;
                println!("{output}");
                return Ok(());
            }
            Command::Request { file, .. } => file,
        };
        let invalid = || ManagedError::Configuration("invalid_vault_request_input".into());
        let reader: Box<dyn Read> = match file {
            Some(path) => Box::new(File::open(path).map_err(|_| invalid())?),
            None => Box::new(io::stdin()),
        };
        let mut bytes = Vec::new();
        reader
            .take(VAULT_REQUEST_MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() > VAULT_REQUEST_MAX_BYTES {
            return Err(ManagedError::Configuration(
                "vault_request_too_large".into(),
            ));
        }
        // Do not include serde errors: they can quote attacker-controlled input.
        let request: VaultRequest = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let receipt = client.vault_request(&request).await?;
        println!(
            "{}",
            serde_json::json!({"status": receipt.status, "ok": receipt.ok})
        );
        Ok(())
    }
}
