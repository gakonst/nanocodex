//! Safe account Vault login metadata and website approval.

use reqwest::Method;
use serde::Deserialize;
use url::Url;

use crate::{ManagedClient, ManagedError};

/// Safe metadata for a saved Vault login; never contains login credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultLogin {
    /// Opaque identifier of the saved login.
    pub id: String,
    /// User-assigned display name.
    pub name: String,
    /// Exact HTTPS origin approved for browser use, if any.
    pub browser_origin: Option<String>,
}

#[derive(Deserialize)]
struct VaultEntry {
    id: String,
    kind: String,
    name: String,
    browser_origin: Option<String>,
}

#[derive(Deserialize)]
struct Credentials {
    vault: Vec<VaultEntry>,
}

impl ManagedClient {
    /// Returns the configured account website's Vault page for secure browser handoff.
    pub fn vault_url(&self) -> String {
        let mut url = self.base_url.clone();
        url.set_path("/vault");
        url.into()
    }

    /// Looks up one saved login by its exact opaque ID, returning safe metadata.
    ///
    /// # Errors
    /// Rejects invalid IDs, missing or non-login entries, malformed metadata, and
    /// unsuccessful account requests.
    pub async fn vault_login(&self, id: &str) -> Result<VaultLogin, ManagedError> {
        validate_id(id)?;
        let response = self
            .request(Method::GET, "v1/credentials", None, None)
            .await?;
        let credentials: Credentials = decode(response).await?;
        let mut entries = credentials.vault.into_iter().filter(|entry| entry.id == id);
        let entry = entries
            .next()
            .ok_or(ManagedError::InvalidResponse("Vault login not found"))?;
        if entries.next().is_some() {
            return Err(ManagedError::InvalidResponse("duplicate Vault login ID"));
        }
        project(entry, id, None)
    }

    /// Approves one exact HTTPS origin for an existing Vault login.
    ///
    /// This mutation is sent once and is never retried by this method.
    ///
    /// # Errors
    /// Rejects invalid IDs or noncanonical HTTPS origins, unsuccessful requests,
    /// and receipts whose login ID or approved origin differs from the request.
    pub async fn approve_vault_login_origin(
        &self,
        id: &str,
        origin: &str,
    ) -> Result<VaultLogin, ManagedError> {
        validate_id(id)?;
        if !valid_origin(origin) {
            return Err(ManagedError::Configuration(
                "Vault browser origin must be an exact HTTPS origin".to_owned(),
            ));
        }
        let body = serde_json::to_vec(&serde_json::json!({"browser_origin": origin}))
            .map_err(|_| ManagedError::Configuration("invalid Vault approval".to_owned()))?;
        let response = self
            .request(
                Method::PUT,
                &format!("v1/credentials/vault/login/{id}/origin"),
                Some(&body),
                None,
            )
            .await?;
        project(decode(response).await?, id, Some(origin))
    }
}

// Do not forward arbitrary service error bodies: they are not safe metadata.
async fn decode<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ManagedError> {
    if !response.status().is_success() {
        return Err(ManagedError::Http {
            status: response.status(),
            code: "vault_request_failed".to_owned(),
            message: "Vault account request failed".to_owned(),
        });
    }
    let bytes = response.bytes().await.map_err(ManagedError::Transport)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ManagedError::InvalidResponse("invalid Vault metadata"))
}

fn validate_id(id: &str) -> Result<(), ManagedError> {
    if !(22..=64).contains(&id.len())
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(ManagedError::Configuration(
            "Vault ID must be 22-64 letters, digits, underscores or hyphens".to_owned(),
        ));
    }
    Ok(())
}

fn valid_origin(origin: &str) -> bool {
    origin.len() <= 2048
        && Url::parse(origin).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.origin().ascii_serialization() == origin
        })
}

fn project(entry: VaultEntry, id: &str, origin: Option<&str>) -> Result<VaultLogin, ManagedError> {
    if entry.id != id
        || entry.kind != "login"
        || entry.name.trim().is_empty()
        || entry.name.chars().count() > 120
        || entry.name.chars().any(char::is_control)
        || entry
            .browser_origin
            .as_deref()
            .is_some_and(|value| !valid_origin(value))
        || origin.is_some_and(|value| entry.browser_origin.as_deref() != Some(value))
    {
        return Err(ManagedError::InvalidResponse("invalid Vault login receipt"));
    }
    Ok(VaultLogin {
        id: entry.id,
        name: entry.name,
        browser_origin: entry.browser_origin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::StatusCode,
        routing::{get, put},
    };
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    const ID: &str = "abcdefghijklmnopqrstuv";
    const ORIGIN: &str = "https://example.com";

    async fn client(app: Router) -> (ManagedClient, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let key =
            crate::ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)))
                .unwrap();
        (
            ManagedClient::new(format!("http://{address}"), key).unwrap(),
            server,
        )
    }

    #[tokio::test]
    async fn wire_lookup_and_approval_project_only_safe_metadata() {
        let app = Router::new()
            .route("/v1/credentials", get(|| async { Json(json!({"vault": [
                {"id": ID, "kind": "login", "name": "Example", "username": "private", "password": "secret"}
            ]})) }))
            .route(&format!("/v1/credentials/vault/login/{ID}/origin"), put(|headers: axum::http::HeaderMap, Json(body): Json<serde_json::Value>| async move {
                assert!(headers.contains_key("authorization"));
                assert_eq!(body, json!({"browser_origin": ORIGIN}));
                Json(json!({"id": ID, "kind": "login", "name": "Example", "browser_origin": ORIGIN, "password": "secret"}))
            }));
        let (client, server) = client(app).await;
        assert_eq!(
            client.vault_login(ID).await.unwrap(),
            VaultLogin {
                id: ID.to_owned(),
                name: "Example".to_owned(),
                browser_origin: None
            }
        );
        let receipt = client.approve_vault_login_origin(ID, ORIGIN).await.unwrap();
        assert_eq!(receipt.browser_origin.as_deref(), Some(ORIGIN));
        assert!(!format!("{receipt:?}").contains("secret"));
        server.abort();
    }

    #[tokio::test]
    async fn mutation_failure_is_not_retried_or_forwarded() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let app = Router::new().route(
            &format!("/v1/credentials/vault/login/{ID}/origin"),
            put(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error":"secret", "message":"password"})),
                    )
                }
            }),
        );
        let (client, server) = client(app).await;
        let error = client
            .approve_vault_login_origin(ID, ORIGIN)
            .await
            .unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!error.to_string().contains("secret"));
        assert!(!error.to_string().contains("password"));
        server.abort();
    }

    #[tokio::test]
    async fn rejects_mismatched_wire_receipts() {
        for patch in [
            json!({"id": "differentabcdefghijklmnop"}),
            json!({"kind":"card"}),
            json!({"browser_origin":"https://other.com"}),
            json!({"browser_origin":null}),
        ] {
            let mut entry =
                json!({"id":ID,"kind":"login","name":"Example","browser_origin":ORIGIN});
            entry
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            let app = Router::new().route(
                &format!("/v1/credentials/vault/login/{ID}/origin"),
                put(move || {
                    let entry = entry.clone();
                    async move { Json(entry) }
                }),
            );
            let (client, server) = client(app).await;
            assert!(client.approve_vault_login_origin(ID, ORIGIN).await.is_err());
            server.abort();
        }
    }

    #[test]
    fn validates_exact_ids_and_origins() {
        for id in [
            "",
            "short",
            "../../abcdefghijklmnopqrstuv",
            "abcdefghijklmnopqrstu.",
        ] {
            assert!(validate_id(id).is_err());
        }
        assert!(validate_id(ID).is_ok());
        for origin in [
            "http://example.com",
            "https://example.com/",
            "https://EXAMPLE.com",
            "https://example.com:443",
            "https://example.com/path",
            "https://example.com?x",
            "https://example.com#x",
            "https://user:pass@example.com",
        ] {
            assert!(!valid_origin(origin), "{origin}");
        }
        assert!(valid_origin(ORIGIN));
        assert!(valid_origin("https://example.com:8443"));
    }
}
