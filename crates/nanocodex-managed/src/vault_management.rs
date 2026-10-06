//! Account Vault management; private request bodies and closed metadata receipts.
#![allow(missing_docs)]
use crate::{ManagedClient, ManagedError, VaultSshTarget};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroizing;

#[derive(Deserialize, Serialize)]
struct Overview {
    pub vault: Vec<Item>,
    pub ssh: Vec<VaultSshTarget>,
}
#[derive(Deserialize, Serialize)]
pub struct Item {
    pub issuer: Option<String>,
    pub account: Option<String>,
    pub origin: Option<String>,
    pub algorithm: Option<String>,
    pub digits: Option<u8>,
    pub period: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    pub id: String,
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last4: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address_line_1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address_line_2: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
}
/// Provider receipt containing only safe references and observed balance metadata.
#[derive(Debug, Deserialize, Serialize)]
pub struct ProviderVaultReceipt {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last4: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_updated_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}
impl ProviderVaultReceipt {
    fn validate(&self) -> Result<(), ManagedError> {
        if ![
            "captured",
            "pending",
            "awaiting_card",
            "awaiting_billing_address",
            "saved",
            "ready",
            "balance_pending",
            "outcome_unknown",
            "failed",
            "refresh_pending",
            "awaiting_issuer_approval",
        ]
        .contains(&self.status.as_str())
            || self.provider.as_deref().is_some_and(|p| p != "laso")
            || self
                .kind
                .as_deref()
                .is_some_and(|k| !["card", "api_key"].contains(&k))
            || self.currency.as_deref().is_some_and(|c| c != "USD")
            || self
                .freshness
                .as_deref()
                .is_some_and(|f| !["current", "stale", "unknown"].contains(&f))
            || self
                .last4
                .as_ref()
                .is_some_and(|v| v.len() != 4 || !v.bytes().all(|b| b.is_ascii_digit()))
            || self.balance.is_some_and(|n| !n.is_finite() || n < 0.0)
        {
            return Err(invalid());
        }
        if let Some(id) = &self.capture_id {
            vault_id(id)?;
        }
        if let Some(id) = &self.vault_id {
            vault_id(id)?;
        }
        Ok(())
    }
}
impl ManagedClient {
    pub async fn vault_list(&self) -> Result<Value, ManagedError> {
        let value = self
            .vault_management(Method::GET, "v1/credentials", None)
            .await?;
        let overview: Overview = serde_json::from_value(value).map_err(|_| invalid())?;
        for item in &overview.vault {
            vault_id(&item.id)?;
            kind_check(&item.kind)?;
        }
        serde_json::to_value(overview).map_err(|_| invalid())
    }
    pub async fn vault_add(
        &self,
        kind: &str,
        private_json: &Zeroizing<Vec<u8>>,
    ) -> Result<Value, ManagedError> {
        kind_check(kind)?;
        let value = self
            .vault_management(
                Method::POST,
                &format!("v1/credentials/vault/{kind}"),
                Some(private_json),
            )
            .await?;
        let item: Item = serde_json::from_value(value).map_err(|_| invalid())?;
        vault_id(&item.id)?;
        if item.kind != kind {
            return Err(invalid());
        }
        serde_json::to_value(item).map_err(|_| invalid())
    }
    pub async fn vault_delete(&self, kind: &str, id: &str) -> Result<Value, ManagedError> {
        kind_check(kind)?;
        vault_id(id)?;
        self.vault_management(
            Method::DELETE,
            &format!("v1/credentials/vault/{kind}/{id}"),
            None,
        )
        .await?;
        Ok(json!({"status":"removed"}))
    }
    pub async fn vault_ssh_put(
        &self,
        reference: &str,
        private_json: &Zeroizing<Vec<u8>>,
    ) -> Result<Value, ManagedError> {
        reference_check(reference)?;
        self.vault_management(
            Method::PUT,
            &format!("v1/credentials/ssh/{reference}"),
            Some(private_json),
        )
        .await?;
        Ok(json!({"status":"saved","reference":reference}))
    }
    pub async fn vault_ssh_remove(&self, reference: &str) -> Result<Value, ManagedError> {
        reference_check(reference)?;
        self.vault_management(
            Method::DELETE,
            &format!("v1/credentials/ssh/{reference}"),
            None,
        )
        .await?;
        Ok(json!({"status":"removed","reference":reference}))
    }
    pub async fn vault_provider_store(
        &self,
        capture_id: &str,
        operation_id: &str,
        name: Option<&str>,
        address_vault_id: Option<&str>,
    ) -> Result<Value, ManagedError> {
        vault_id(capture_id)?;
        operation_check(operation_id)?;
        if let Some(id) = address_vault_id {
            vault_id(id)?;
        }
        let mut body = json!({"capture_id":capture_id,"operation_id":operation_id});
        if let Some(name) = name {
            body["name"] = json!(name);
        }
        if let Some(id) = address_vault_id {
            body["address_vault_id"] = json!(id);
        }
        self.provider_receipt("v1/vault/store", &body).await
    }
    pub async fn vault_provider_card(
        &self,
        operation: &str,
        id: &str,
        capture: bool,
        operation_id: Option<&str>,
    ) -> Result<Value, ManagedError> {
        vault_id(id)?;
        if !["status", "balance", "refresh"].contains(&operation)
            || operation == "refresh" && operation_id.is_none()
        {
            return Err(invalid());
        }
        let mut body = json!({"operation":operation});
        body[if capture { "capture_id" } else { "vault_id" }] = json!(id);
        if let Some(id) = operation_id {
            operation_check(id)?;
            body["operation_id"] = json!(id);
        }
        self.provider_receipt("v1/vault/card", &body).await
    }
    async fn provider_receipt(&self, path: &str, body: &Value) -> Result<Value, ManagedError> {
        let bytes = serde_json::to_vec(body).map_err(|_| invalid())?;
        let value = self
            .vault_management(Method::POST, path, Some(&bytes))
            .await?;
        let receipt: ProviderVaultReceipt = serde_json::from_value(value).map_err(|_| invalid())?;
        receipt.validate()?;
        serde_json::to_value(receipt).map_err(|_| invalid())
    }
    async fn vault_management(
        &self,
        method: Method,
        path: &str,
        body: Option<&[u8]>,
    ) -> Result<Value, ManagedError> {
        if body.is_some_and(|b| b.len() > 96 * 1024) {
            return Err(invalid());
        }
        let mut response = self.request(method, path, body, None).await?;
        if !response.status().is_success() {
            return Err(ManagedError::Http {
                status: response.status(),
                code: "vault_request_failed".into(),
                message: "Vault request failed; reconcile uncertain writes before retrying".into(),
            });
        }
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
            if bytes.len() + chunk.len() > 256 * 1024 {
                return Err(invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| invalid())
    }
}
fn invalid() -> ManagedError {
    ManagedError::InvalidResponse("invalid Vault management request or receipt")
}
fn vault_id(s: &str) -> Result<(), ManagedError> {
    if !(22..=64).contains(&s.len())
        || !s
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    {
        return Err(invalid());
    }
    Ok(())
}
fn operation_check(s: &str) -> Result<(), ManagedError> {
    uuid::Uuid::parse_str(s).map(|_| ()).map_err(|_| invalid())
}
fn reference_check(s: &str) -> Result<(), ManagedError> {
    if s.is_empty()
        || s.len() > 64
        || !s.as_bytes()[0].is_ascii_alphanumeric()
        || !s
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        || ["__proto__", "constructor", "prototype"].contains(&s)
    {
        return Err(invalid());
    }
    Ok(())
}
fn kind_check(s: &str) -> Result<(), ManagedError> {
    if ["login", "api_key", "card", "address", "phone", "totp"].contains(&s) {
        Ok(())
    } else {
        Err(invalid())
    }
}
