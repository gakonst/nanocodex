//! Local account connector commands; no model turn or automatic write retries.
use clap::{Args, Subcommand};
use nanocodex_managed::{ManagedClient, ManagedError};
#[derive(Args)]
pub(crate) struct Connectors {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Catalog,
    List,
    Start {
        provider: String,
        #[arg(long, default_value = "/profile")]
        return_to: String,
    },
    Disconnect {
        provider: String,
        connection_id: String,
    },
    Cloudflare {
        vault_id: String,
        #[arg(long)]
        account_id: Option<String>,
    },
    McpList,
    McpCreate {
        target: String,
    },
    McpStart {
        connection_id: String,
        #[arg(long, default_value = "/profile")]
        return_to: String,
    },
    McpDisconnect {
        connection_id: String,
    },
    /// Start once with a stable UUID; complete private pairing in the native app.
    WhatsappStart {
        phone: String,
        #[arg(long)]
        operation_id: String,
    },
}
impl Connectors {
    pub(super) async fn run(self, client: &ManagedClient) -> Result<(), ManagedError> {
        let text = self.execute(client).await?;
        use std::io::Write;
        writeln!(std::io::stdout().lock(), "{text}")
            .map_err(|_| ManagedError::InvalidResponse("connector output failed"))
    }
    pub(crate) async fn execute(self, client: &ManagedClient) -> Result<String, ManagedError> {
        match self.command {
            Command::Catalog => encode(&client.connector_catalog().await?),
            Command::List => encode(&client.connector_list().await?),
            Command::Start {
                provider,
                return_to,
            } => encode(&client.connector_start(&provider, &return_to).await?),
            Command::Disconnect {
                provider,
                connection_id,
            } => {
                client
                    .connector_disconnect(&provider, &connection_id)
                    .await?;
                encode(&serde_json::json!({"status":"removed"}))
            }
            Command::Cloudflare {
                vault_id,
                account_id,
            } => encode(
                &client
                    .connector_cloudflare(&vault_id, account_id.as_deref())
                    .await?,
            ),
            Command::McpList => encode(&client.mcp_connections().await?),
            Command::McpCreate { target } => encode(&client.mcp_connection_create(&target).await?),
            Command::McpStart {
                connection_id,
                return_to,
            } => encode(
                &client
                    .mcp_connection_start(&connection_id, &return_to)
                    .await?,
            ),
            Command::McpDisconnect { connection_id } => {
                client.mcp_connection_disconnect(&connection_id).await?;
                encode(&serde_json::json!({"status":"removed"}))
            }
            Command::WhatsappStart {
                phone,
                operation_id,
            } => encode(&client.whatsapp_start(&phone, &operation_id).await?),
        }
    }
}

fn encode<T: serde::Serialize>(v: &T) -> Result<String, ManagedError> {
    serde_json::to_string(v).map_err(|_| ManagedError::InvalidResponse("connector output failed"))
}
#[derive(clap::Parser)]
struct LocalArgs {
    #[command(flatten)]
    connectors: Connectors,
}
pub(crate) fn parse_local(text: &str) -> Result<Connectors, ManagedError> {
    use clap::Parser;
    LocalArgs::try_parse_from(std::iter::once("connectors").chain(text.split_whitespace().skip(1))).map(|v|v.connectors).map_err(|_|ManagedError::InvalidResponse("Use /connectors list|catalog|start PROVIDER|disconnect PROVIDER ID|cloudflare VAULT_ID|mcp-list|mcp-create TARGET|mcp-start ID|mcp-disconnect ID"))
}
