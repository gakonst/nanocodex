//! Account navigation and standalone service clients.
#![allow(missing_docs)]
use crate::{ManagedClient, ManagedError, VaultRequest, VaultRequestReceipt};
use reqwest::Method;
use serde::{Deserialize, Serialize};

/// Safe account metadata. Pending login codes are deliberately omitted;
/// trusted clients use `private_connector_input` for the private login panel.
#[derive(Serialize, Deserialize)]
pub struct CredentialsOverview {
    pub ready: bool,
    pub active: Option<String>,
    pub openai: ModelCredentialStatus,
    pub chatgpt: ModelCredentialStatus,
    pub claude: ModelCredentialStatus,
    pub vault: Vec<crate::VaultItemMetadata>,
    pub ssh: Vec<crate::VaultSshTarget>,
}
#[derive(Serialize, Deserialize)]
pub struct ModelCredentialStatus {
    pub connected: bool,
    pub state: Option<String>,
    pub account_id: Option<String>,
    pub organization_id: Option<String>,
    pub expires_at: Option<u64>,
    pub accounts: Option<Vec<ModelCredentialAccount>>,
}
#[derive(Serialize, Deserialize)]
pub struct ModelCredentialAccount {
    pub account_id: String,
    pub connected: bool,
    pub active: bool,
    pub limited_until: Option<u64>,
}
#[derive(Serialize, Deserialize)]
pub struct AccountLinks {
    pub connections: String,
    pub vault: String,
    pub wallet: String,
    pub access: String,
}
#[derive(Default)]
pub struct AccountLinkOptions<'a> {
    pub connect: Option<&'a str>,
    pub add: Option<&'a str>,
}
#[derive(Serialize, Deserialize)]
pub struct AccountService {
    pub id: String,
    pub path: String,
    pub operations: Option<Vec<String>>,
    pub enrollment_path: Option<String>,
    pub secret_export: Option<bool>,
    pub human_approval_required: Option<bool>,
    pub sms_2fa_compatibility: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct AccountServiceCatalog {
    pub services: Vec<AccountService>,
}
#[derive(Serialize, Deserialize)]
pub struct ServiceVaultList {
    pub vault: Vec<crate::vault_management::Item>,
}
#[derive(Serialize, Deserialize)]
pub struct ServiceVaultEntry {
    pub entry: crate::vault_management::Item,
}
#[derive(Default)]
pub struct ServiceLinkOptions<'a> {
    pub service: Option<&'a str>,
    pub action: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub operation_id: Option<&'a str>,
    pub app_origin: Option<&'a str>,
    pub state: Option<&'a str>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceLink {
    pub url: String,
    pub service: Option<String>,
    pub action: Option<String>,
    pub kind: Option<String>,
    pub operation_id: Option<String>,
    pub app_origin: Option<String>,
    pub state: Option<String>,
}
fn validate_human_url(value: &str) -> Result<(), ManagedError> {
    let url = url::Url::parse(value).map_err(|_| invalid())?;
    if !(url.scheme() == "https"
        || url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid());
    }
    Ok(())
}
fn invalid() -> ManagedError {
    ManagedError::InvalidResponse("invalid account service request or receipt")
}
impl ManagedClient {
    /// Read model connection state and safe Vault/SSH metadata without login codes.
    pub async fn credentials_overview(&self) -> Result<CredentialsOverview, ManagedError> {
        self.connector_request(Method::GET, "v1/credentials", None)
            .await
    }
    /// Disconnect all ChatGPT subscriptions once; inspect overview after uncertainty.
    pub async fn chatgpt_disconnect(&self) -> Result<(), ManagedError> {
        self.connector_request(Method::DELETE, "v1/credentials/chatgpt", None)
            .await
    }
    /// Remove the saved OpenAI API key once. Saving uses `private_input_post`
    /// with `PrivateInputKind::Credential("openai".into())` and `PrivateInputBody`.
    pub async fn openai_disconnect(&self) -> Result<(), ManagedError> {
        self.connector_request(Method::DELETE, "v1/credentials/openai", None)
            .await
    }

    pub async fn account_links(
        &self,
        options: AccountLinkOptions<'_>,
    ) -> Result<AccountLinks, ManagedError> {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if let Some(connect) = options.connect {
            if ![
                "cloudflare",
                "github",
                "google",
                "slack",
                "x",
                "spotify",
                "soundcloud",
                "link",
                "whatsapp",
                "claude",
                "chatgpt",
                "openai",
                "mcp",
            ]
            .contains(&connect)
            {
                return Err(invalid());
            }
            query.append_pair("connect", connect);
        }
        if let Some(add) = options.add {
            if !["login", "api_key", "card", "address", "phone", "totp"].contains(&add) {
                return Err(invalid());
            }
            query.append_pair("add", add);
        }
        let query = query.finish();
        let path = if query.is_empty() {
            "v1/account/links".into()
        } else {
            format!("v1/account/links?{query}")
        };
        let links: AccountLinks = self.connector_request(Method::GET, &path, None).await?;
        for value in [
            &links.connections,
            &links.vault,
            &links.wallet,
            &links.access,
        ] {
            validate_human_url(value)?;
        }
        Ok(links)
    }
    pub async fn services_link(
        &self,
        o: ServiceLinkOptions<'_>,
    ) -> Result<ServiceLink, ManagedError> {
        if o.service.is_some_and(|v| !["vault", "phone"].contains(&v))
            || o.action.is_some_and(|v| !["enroll", "select"].contains(&v))
            || o.kind.is_some_and(|v| {
                !["login", "api_key", "card", "address", "phone", "totp"].contains(&v)
            })
            || o.operation_id
                .is_some_and(|v| uuid::Uuid::parse_str(v).is_err())
            || o.app_origin.is_none() && o.state.is_some()
            || o.app_origin
                .is_some_and(|v| !crate::private_input::valid_origin(v))
            || o.state.is_some_and(|v| {
                !(16..=128).contains(&v.len())
                    || !v
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
        {
            return Err(invalid());
        }
        let mut q = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in [
            ("service", o.service),
            ("action", o.action),
            ("kind", o.kind),
            ("operation_id", o.operation_id),
            ("app_origin", o.app_origin),
            ("state", o.state),
        ] {
            if let Some(v) = v {
                q.append_pair(k, v);
            }
        }
        let q = q.finish();
        let path = if q.is_empty() {
            "v1/services/links".into()
        } else {
            format!("v1/services/links?{q}")
        };
        let link: ServiceLink = self.connector_request(Method::GET, &path, None).await?;
        validate_human_url(&link.url)?;
        Ok(link)
    }
    pub async fn services_catalog(&self) -> Result<AccountServiceCatalog, ManagedError> {
        self.connector_request(Method::GET, "v1/services", None)
            .await
    }
    pub async fn services_vault_list(&self) -> Result<ServiceVaultList, ManagedError> {
        self.connector_request(Method::GET, "v1/services/vault", None)
            .await
    }
    pub async fn services_vault_get(&self, id: &str) -> Result<ServiceVaultEntry, ManagedError> {
        if !(22..=64).contains(&id.len())
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(invalid());
        }
        self.connector_request(Method::GET, &format!("v1/services/vault/{id}"), None)
            .await
    }
    pub async fn services_vault_request(
        &self,
        request: &VaultRequest,
    ) -> Result<VaultRequestReceipt, ManagedError> {
        self.vault_request_at("v1/services/vault/request", request)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        routing::{delete, get, post},
    };
    use serde_json::json;
    #[tokio::test]
    async fn account_services_http_projection_and_query() {
        let app = Router::new()
            .route("/v1/services/vault/request", post(|Json(body): Json<serde_json::Value>| async move {
                assert_eq!(body["vault_id"], "abcdefghijklmnopqrstuv");
                Json(if body["url"] == "https://example.com/invalid" { json!({"status":0,"ok":true}) } else { json!({"status":204,"ok":true}) })
            }))
            .route("/v1/credentials", get(|headers: axum::http::HeaderMap| async move {
                assert!(headers.contains_key("authorization"));
                Json(json!({"ready":true,"active":"chatgpt","openai":{"connected":false},
                    "chatgpt":{"connected":true,"account_id":"synthetic-account","login":{"user_code":"discard-me"},"access_token":"discard-me"},
                    "claude":{"connected":false,"state":"signed_out"},"vault":[],"ssh":[]}))
            }))
            .route("/v1/credentials/chatgpt", delete(|| async { axum::http::StatusCode::NO_CONTENT }))
            .route("/v1/credentials/openai", delete(|| async { (axum::http::StatusCode::FORBIDDEN, "discard-me") }))

            .route("/v1/account/links", get(|uri: axum::http::Uri| async move {
                assert_eq!(uri.query(), Some("connect=cloudflare&add=totp"));
                Json(json!({"connections":"https://example.com/connect", "vault":"https://example.com/connect/vault", "wallet":"https://example.com/connect/wallet", "access":"https://example.com/connect/access", "secret":"discard-me"}))
            }))
            .route("/v1/services/links", get(|uri: axum::http::Uri| async move {
                assert_eq!(uri.query(), Some("service=vault&action=enroll&kind=totp&app_origin=https%3A%2F%2Fclient.example&state=abcdefghijklmnop0123"));
                Json(json!({"url":"https://example.com/connect/vault?add=totp","service":"vault","action":"enroll","kind":"totp","appOrigin":"https://client.example","state":"abcdefghijklmnop0123","extra":"discard-me"}))
            }))
            .route("/v1/services/vault", get(|| async {
                Json(json!({"vault":[{"id":"abcdefghijklmnopqrstuv", "kind":"totp", "name":"Authenticator", "issuer":"Example", "digits":6, "created_at":1700000000000u64, "seed":"discard-me"}]}))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let key =
            crate::ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)))
                .unwrap();
        let client = ManagedClient::new(format!("http://{address}"), key).unwrap();
        let mut request: VaultRequest = serde_json::from_value(
            json!({"vault_id":"abcdefghijklmnopqrstuv", "url":"https://example.com/valid"}),
        )
        .unwrap();
        assert_eq!(
            client
                .services_vault_request(&request)
                .await
                .unwrap()
                .status,
            204
        );
        request.url = "https://example.com/invalid".into();
        assert!(client.services_vault_request(&request).await.is_err());
        let overview = client.credentials_overview().await.unwrap();
        assert!(overview.chatgpt.connected);
        assert!(
            !serde_json::to_string(&overview)
                .unwrap()
                .contains("discard-me")
        );
        client.chatgpt_disconnect().await.unwrap();
        let error = client.openai_disconnect().await.unwrap_err();
        assert!(matches!(
            error,
            ManagedError::Http {
                status: reqwest::StatusCode::FORBIDDEN,
                ..
            }
        ));
        assert!(!error.to_string().contains("discard-me"));
        let links = client
            .account_links(AccountLinkOptions {
                connect: Some("cloudflare"),
                add: Some("totp"),
            })
            .await
            .unwrap();
        assert!(
            !serde_json::to_string(&links)
                .unwrap()
                .contains("discard-me")
        );
        let link = client
            .services_link(ServiceLinkOptions {
                service: Some("vault"),
                action: Some("enroll"),
                kind: Some("totp"),
                app_origin: Some("https://client.example"),
                state: Some("abcdefghijklmnop0123"),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(link.app_origin.as_deref(), Some("https://client.example"));
        assert!(!serde_json::to_string(&link).unwrap().contains("discard-me"));
        assert!(
            client
                .services_link(ServiceLinkOptions {
                    state: Some("abcdefghijklmnop0123"),
                    ..Default::default()
                })
                .await
                .is_err()
        );
        let vault = client.services_vault_list().await.unwrap();
        assert_eq!(vault.vault[0].digits, Some(6));
        assert_eq!(vault.vault[0].created_at, Some(1700000000000));
        assert!(
            !serde_json::to_string(&vault)
                .unwrap()
                .contains("discard-me")
        );
        task.abort();
    }
}
