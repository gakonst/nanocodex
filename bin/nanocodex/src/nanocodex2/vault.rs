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
    /// List safe Vault metadata, without secret values.
    List,
    /// Add an item with private, field-by-field terminal input.
    Add {
        #[arg(value_parser=["login","api_key","card","address","phone"])]
        kind: String,
    },
    /// Delete one exact saved item.
    Delete {
        #[arg(value_parser=["login","api_key","card","address","phone"])]
        kind: String,
        id: String,
    },
    /// Generate an SSH key in Vault, or privately import a PEM file.
    SshSave {
        reference: String,
        #[arg(long)]
        hostname: String,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long)]
        username: String,
        #[arg(long)]
        host_key_sha256: String,
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
    SshRemove {
        reference: String,
    },
    /// Save an existing private capture; reuse operation-id after uncertainty.
    Store {
        capture_id: String,
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        address_vault_id: Option<String>,
    },
    /// Read or refresh a provider card by Vault ID, or by capture ID with --capture.
    Card {
        #[arg(value_parser=["status","balance","refresh"])]
        operation: String,
        id: String,
        #[arg(long)]
        capture: bool,
        #[arg(long)]
        operation_id: Option<String>,
    },
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
        let print = |value: serde_json::Value| -> Result<(), ManagedError> {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).map_err(|_| private_error())?
            );
            Ok(())
        };
        let file = match self.command {
            Command::List => return print(client.vault_list().await?),
            Command::Add { kind } => {
                return print(client.vault_add(&kind, &private_form(&kind)?).await?);
            }
            Command::Delete { kind, id } => return print(client.vault_delete(&kind, &id).await?),
            Command::SshSave {
                reference,
                hostname,
                port,
                username,
                host_key_sha256,
                key_file,
            } => {
                use zeroize::Zeroize;
                let mut value = serde_json::json!({"hostname":hostname,"port":port,"username":username,"host_key_sha256":host_key_sha256});
                if let Some(path) = key_file {
                    let mut key = zeroize::Zeroizing::new(String::new());
                    File::open(path)
                        .map_err(|_| private_error())?
                        .take(65537)
                        .read_to_string(&mut key)
                        .map_err(|_| private_error())?;
                    if key.len() > 65536 {
                        return Err(private_error());
                    }
                    value["private_key"] = serde_json::Value::String(key.to_string());
                } else {
                    value["generate"] = serde_json::Value::Bool(true);
                }
                let body = zeroize::Zeroizing::new(
                    serde_json::to_vec(&value).map_err(|_| private_error())?,
                );
                if let Some(serde_json::Value::String(key)) = value.get_mut("private_key") {
                    key.zeroize();
                }
                return print(client.vault_ssh_put(&reference, &body).await?);
            }
            Command::SshRemove { reference } => {
                return print(client.vault_ssh_remove(&reference).await?);
            }
            Command::Store {
                capture_id,
                operation_id,
                name,
                address_vault_id,
            } => {
                return print(
                    client
                        .vault_provider_store(
                            &capture_id,
                            &operation_id,
                            name.as_deref(),
                            address_vault_id.as_deref(),
                        )
                        .await?,
                );
            }
            Command::Card {
                operation,
                id,
                capture,
                operation_id,
            } => {
                return print(
                    client
                        .vault_provider_card(&operation, &id, capture, operation_id.as_deref())
                        .await?,
                );
            }
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

fn private_error() -> ManagedError {
    ManagedError::Configuration("private Vault input could not be completed".into())
}

/// A local terminal form, never the agent's command input or conversation history.
fn private_form(kind: &str) -> Result<zeroize::Zeroizing<Vec<u8>>, ManagedError> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
    use std::io::{IsTerminal, Write};
    use zeroize::{Zeroize, Zeroizing};
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(ManagedError::Configuration(
            "Use an interactive terminal for private Vault input".into(),
        ));
    }
    let fields: &[&str] = match kind {
        "login" => &["name", "username", "password", "browser_origin (optional)"],
        "api_key" => &["name", "api_key"],
        "card" => &[
            "name",
            "card_number",
            "expiry_month",
            "expiry_year",
            "billing_zip",
        ],
        "address" => &[
            "name",
            "address_line_1",
            "address_line_2 (optional)",
            "city",
            "state",
            "zip",
            "country",
        ],
        "phone" => &["name", "phone_number"],
        _ => return Err(private_error()),
    };
    eprintln!(
        "Add {kind} to Vault. Input stays private and is not echoed. Enter advances; Esc cancels."
    );
    crossterm::terminal::enable_raw_mode().map_err(|_| private_error())?;
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
            eprintln!();
        }
    }
    let _restore = Restore;
    let mut json = Zeroizing::new(String::from("{"));
    let mut first = true;
    for field in fields {
        eprint!("\r\n{field}: ");
        let _ = io::stderr().flush();
        let mut text = Zeroizing::new(String::new());
        loop {
            match crossterm::event::read().map_err(|_| private_error())? {
                Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                    KeyCode::Enter => break,
                    KeyCode::Esc => return Err(private_error()),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Err(private_error());
                    }
                    KeyCode::Char(c) if !c.is_control() => {
                        if text.len() + c.len_utf8() > 8192 {
                            return Err(private_error());
                        }
                        text.push(c);
                    }
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    _ => {}
                },
                Event::Paste(mut paste) => {
                    if text.len() + paste.len() > 8192 || paste.chars().any(char::is_control) {
                        paste.zeroize();
                        return Err(private_error());
                    }
                    text.push_str(&paste);
                    paste.zeroize();
                }
                _ => {}
            }
        }
        if text.is_empty() && field.ends_with(" (optional)") {
            continue;
        }
        if !first {
            json.push(',');
        }
        first = false;
        let key = field.split_whitespace().next().ok_or_else(private_error)?;
        json.push_str(&serde_json::to_string(key).map_err(|_| private_error())?);
        json.push(':');
        let encoded = Zeroizing::new(serde_json::to_string(&*text).map_err(|_| private_error())?);
        json.push_str(&encoded);
    }
    json.push('}');
    Ok(Zeroizing::new(json.as_bytes().to_vec()))
}
