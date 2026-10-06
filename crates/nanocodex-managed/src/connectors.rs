//! Direct account connector management with closed, bounded public receipts.
#![allow(missing_docs)]
use crate::{ManagedClient, ManagedError};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use zeroize::Zeroizing;
#[derive(Debug, Deserialize, Serialize)]
pub struct ConnectorConnection {
    pub id: String,
    pub label: String,
    #[serde(rename = "accountId")]
    pub account_id: Option<String>,
    pub capabilities: Option<Vec<String>>,
    pub scopes: Option<Vec<String>>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct ConnectorStatus {
    pub connected: bool,
    pub connections: Option<Vec<ConnectorConnection>>,
    pub account: Option<String>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct ConnectorList {
    pub connectors: BTreeMap<String, ConnectorStatus>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct ConnectorCapability {
    pub id: String,
    pub name: String,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct ConnectorProvider {
    pub id: String,
    pub name: String,
    pub description: String,
    pub capabilities: Vec<ConnectorCapability>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct ConnectorCatalog {
    pub providers: Vec<ConnectorProvider>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct McpConnection {
    pub id: String,
    pub name: String,
    pub status: String,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct McpConnections {
    pub mcp_connections: Vec<McpConnection>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct ConnectorReceipt {
    pub authorization_url: Option<String>,
    pub connected: Option<bool>,
    pub mcp_connection: Option<McpConnection>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct WhatsAppStartReceipt {
    pub status: Option<String>,
    pub phase: Option<String>,
    pub operation_id: Option<String>,
}
fn invalid() -> ManagedError {
    ManagedError::InvalidResponse("invalid connector request or receipt")
}
fn identifier(s: &str, min: usize, max: usize) -> Result<(), ManagedError> {
    if !(min..=max).contains(&s.len())
        || !s
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    {
        return Err(invalid());
    }
    Ok(())
}
fn provider(s: &str) -> Result<(), ManagedError> {
    if ![
        "cloudflare",
        "github",
        "google",
        "gmail",
        "gdrive",
        "slack",
        "x",
        "spotify",
        "soundcloud",
        "link",
        "whatsapp",
    ]
    .contains(&s)
    {
        return Err(invalid());
    }
    Ok(())
}
fn return_path(s: &str) -> Result<(), ManagedError> {
    if !s.starts_with('/')
        || s.starts_with("//")
        || s.contains('\\')
        || s.len() > 2048
        || s.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    Ok(())
}
impl ManagedClient {
    pub async fn connector_catalog(&self) -> Result<ConnectorCatalog, ManagedError> {
        self.connector_request(Method::GET, "v1/connectors/catalog", None)
            .await
    }
    pub async fn connector_list(&self) -> Result<ConnectorList, ManagedError> {
        self.connector_request(Method::GET, "v1/connectors", None)
            .await
    }
    pub async fn connector_start(
        &self,
        name: &str,
        return_to: &str,
    ) -> Result<ConnectorReceipt, ManagedError> {
        provider(name)?;
        return_path(return_to)?;
        if ["cloudflare", "whatsapp"].contains(&name) {
            return Err(invalid());
        }
        self.connector_request(
            Method::POST,
            &format!("v1/connectors/{name}"),
            Some(json!({"return_to":return_to})),
        )
        .await
    }
    pub async fn connector_disconnect(&self, name: &str, id: &str) -> Result<(), ManagedError> {
        provider(name)?;
        identifier(id, 43, 43)?;
        self.connector_delete(&format!("v1/connectors/{name}/connections/{id}"))
            .await
    }
    pub async fn connector_cloudflare(
        &self,
        vault_id: &str,
        account_id: Option<&str>,
    ) -> Result<ConnectorReceipt, ManagedError> {
        identifier(vault_id, 22, 64)?;
        if account_id.is_some_and(|s| {
            s.len() != 32
                || !s
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        }) {
            return Err(invalid());
        }
        let mut body = json!({"vault_id":vault_id});
        if let Some(id) = account_id {
            body["account_id"] = json!(id);
        }
        self.connector_request(Method::POST, "v1/connectors/cloudflare", Some(body))
            .await
    }
    pub async fn mcp_connections(&self) -> Result<McpConnections, ManagedError> {
        self.connector_request(Method::GET, "v1/connectors/mcp-connections", None)
            .await
    }
    pub async fn mcp_connection_create(
        &self,
        target: &str,
    ) -> Result<ConnectorReceipt, ManagedError> {
        if target.is_empty() || target.len() > 2048 || target.chars().any(char::is_control) {
            return Err(invalid());
        }
        self.connector_request(
            Method::POST,
            "v1/connectors/mcp-connections",
            Some(json!({"target":target})),
        )
        .await
    }
    pub async fn mcp_connection_start(
        &self,
        id: &str,
        return_to: &str,
    ) -> Result<ConnectorReceipt, ManagedError> {
        identifier(id, 43, 43)?;
        return_path(return_to)?;
        self.connector_request(
            Method::POST,
            &format!("v1/connectors/mcp-connections/{id}/start"),
            Some(json!({"return_to":return_to})),
        )
        .await
    }
    pub async fn mcp_connection_disconnect(&self, id: &str) -> Result<(), ManagedError> {
        identifier(id, 43, 43)?;
        self.connector_delete(&format!("v1/connectors/mcp-connections/{id}"))
            .await
    }
    /// Start linking once. Pairing codes must be read only through the private native flow.
    pub async fn whatsapp_start(
        &self,
        phone: &str,
        operation_id: &str,
    ) -> Result<WhatsAppStartReceipt, ManagedError> {
        uuid::Uuid::parse_str(operation_id).map_err(|_| invalid())?;
        if !phone.starts_with('+')
            || !(8..=16).contains(&phone.len())
            || !phone[1..].bytes().all(|c| c.is_ascii_digit())
        {
            return Err(invalid());
        }
        self.connector_request(
            Method::POST,
            "v1/connectors/whatsapp/start",
            Some(json!({"phone":phone,"operation_id":operation_id})),
        )
        .await
    }
    async fn connector_delete(&self, path: &str) -> Result<(), ManagedError> {
        self.connector_request::<serde_json::Value>(Method::DELETE, path, None)
            .await
            .map(|_| ())
    }
    async fn connector_request<T: serde::de::DeserializeOwned + Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, ManagedError> {
        let body = body
            .map(|v| serde_json::to_vec(&v))
            .transpose()
            .map_err(|_| invalid())?;
        let mut response = self
            .request(method, path, body.as_deref(), None)
            .await
            .map_err(|_| {
                ManagedError::InvalidResponse(
                    "connector transport failed; reconcile writes before retrying",
                )
            })?;
        if !response.status().is_success() {
            return Err(ManagedError::Http {
                status: response.status(),
                code: "connector_request_failed".into(),
                message: "Connector request failed; reconcile uncertain writes before retrying"
                    .into(),
            });
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
            if bytes.len() + chunk.len() > 256 * 1024 {
                return Err(invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            bytes.extend_from_slice(b"null");
        }
        let receipt: T = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let projected = serde_json::to_value(&receipt).map_err(|_| invalid())?;
        bounded_metadata(&projected, 0)?;
        Ok(receipt)
    }
}

fn bounded_metadata(value: &serde_json::Value, depth: usize) -> Result<(), ManagedError> {
    if depth > 8 {
        return Err(invalid());
    }
    match value {
        serde_json::Value::String(s) if s.len() > 8192 || s.chars().any(char::is_control) => {
            return Err(invalid());
        }
        serde_json::Value::Array(a) => {
            if a.len() > 128 {
                return Err(invalid());
            }
            for v in a {
                bounded_metadata(v, depth + 1)?;
            }
        }
        serde_json::Value::Object(o) => {
            if o.len() > 64 {
                return Err(invalid());
            }
            for (k, v) in o {
                if k.len() > 128 {
                    return Err(invalid());
                }
                if k == "authorization_url" && !v.is_null() {
                    let url =
                        url::Url::parse(v.as_str().ok_or_else(invalid)?).map_err(|_| invalid())?;
                    if url.scheme() != "https"
                        || !url.username().is_empty()
                        || url.password().is_some()
                    {
                        return Err(invalid());
                    }
                }
                if k == "id" {
                    identifier(v.as_str().ok_or_else(invalid)?, 1, 64)?;
                }
                bounded_metadata(v, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
