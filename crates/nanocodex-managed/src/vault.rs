//! Safe account Vault metadata and broker-owned HTTP operations.

use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

/// Public metadata for a saved SSH target; never contains private key material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultSshTarget {
    /// Opaque identity reference used to select the saved target.
    pub reference: String,
    /// Saved destination hostname.
    pub hostname: String,
    /// Saved SSH port.
    pub port: u16,
    /// Saved SSH username.
    pub username: String,
    /// Pinned server host-key SHA-256 fingerprint. Absent only when the target
    /// is bound to device host-key trust instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_key_sha256: Option<String>,
    /// Owner-saved device host-key trust binding: `device` (this reference's
    /// server Hand) or `hand:MACHINE_ID`; never derived from the hostname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_key_trust: Option<String>,
    /// Public key for installation on the server, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
}

#[derive(Deserialize)]
struct SshCredentials {
    // Deserialize only the public SSH projection, ignoring all other credentials.
    ssh: Vec<VaultSshTarget>,
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
    /// Lists saved SSH targets using only the account endpoint's public metadata.
    ///
    /// # Errors
    /// Rejects unsuccessful requests or malformed metadata without reflecting
    /// arbitrary response bodies. An absent public key is omitted from output.
    pub async fn vault_ssh_targets(&self) -> Result<Vec<VaultSshTarget>, ManagedError> {
        let response = self
            .request(Method::GET, "v1/credentials", None, None)
            .await?;
        let credentials: SshCredentials = decode(response).await?;
        Ok(credentials.ssh)
    }

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

/// Maximum encoded public request size accepted by the Vault broker.
pub const VAULT_REQUEST_MAX_BYTES: usize = 96 * 1024;

/// An HTTP operation containing only a Vault reference and public templates.
///
/// Credentials and signing keys are resolved only inside the broker. Do not put
/// raw secrets in these fields. Debug intentionally omits all request content.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultRequest {
    /// Exact opaque saved Vault item ID.
    pub vault_id: String,
    /// Exact authorized HTTPS destination; placeholders belong in headers/body only.
    pub url: String,
    /// HTTP method; defaults to GET.
    #[serde(default)]
    pub method: VaultRequestMethod,
    /// Public header templates; defaults to an empty object.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Public body template, never a resolved credential.
    /// `{{NANOCODEX_VAULT_TOTP}}` is resolved only by the broker at the item’s saved origin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Escaping applied to substituted body values by the broker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_encoding: Option<VaultBodyEncoding>,
    /// Optional broker-owned signature or JWT computation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing: Option<VaultSigning>,
}

impl std::fmt::Debug for VaultRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultRequest").finish_non_exhaustive()
    }
}

/// Supported outbound HTTP methods.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum VaultRequestMethod {
    /// Read a resource.
    #[default]
    Get,
    /// Read response metadata.
    Head,
    /// Query supported operations.
    Options,
    /// Submit a resource.
    Post,
    /// Replace a resource.
    Put,
    /// Update a resource.
    Patch,
    /// Delete a resource.
    Delete,
}

/// Body placeholder escaping mode.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VaultBodyEncoding {
    /// Substitute directly into the public template.
    Raw,
    /// Substitute in parsed JSON values with JSON escaping.
    Json,
    /// Substitute in parsed form values with form escaping.
    Form,
}

/// Broker-owned signing configuration; contains no key material.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultSigning {
    /// Cryptographic algorithm used with the selected Vault item.
    pub algorithm: VaultSigningAlgorithm,
    /// Public UTF-8 message; mutually exclusive with `jwt`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Public JWT claims; mutually exclusive with `message`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jwt: Option<VaultJwt>,
    /// Encoding of a message signature; defaults to base64url.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<VaultSignatureEncoding>,
    /// Interpretation of the saved key; the key never leaves the broker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_encoding: Option<VaultKeyEncoding>,
}

/// Public JWT input. The broker validates and binds the algorithm header.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultJwt {
    /// Public JWT headers.
    pub header: serde_json::Map<String, serde_json::Value>,
    /// Public JWT claims.
    pub payload: serde_json::Map<String, serde_json::Value>,
}

/// Algorithms supported by broker-owned signing.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum VaultSigningAlgorithm {
    /// HMAC using SHA-256.
    #[serde(rename = "HMAC-SHA256")]
    HmacSha256,
    /// HMAC using SHA-512.
    #[serde(rename = "HMAC-SHA512")]
    HmacSha512,
    /// RSA PKCS#1 v1.5 using SHA-256.
    RS256,
    /// P-256 ECDSA using SHA-256.
    ES256,
    /// Edwards-curve signature.
    EdDSA,
}

/// Message signature encoding.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VaultSignatureEncoding {
    /// Hexadecimal.
    Hex,
    /// Standard base64.
    Base64,
    /// URL-safe base64.
    Base64url,
}

/// Interpretation of key material inside the broker.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VaultKeyEncoding {
    /// UTF-8 HMAC key.
    Utf8,
    /// Base64 bytes or asymmetric DER.
    Base64,
    /// Hex bytes or asymmetric DER.
    Hex,
    /// Asymmetric PKCS#8 PEM.
    Pkcs8,
}

/// Closed, status-only receipt: no response body, headers, cookies or signatures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultRequestReceipt {
    /// Destination HTTP status, from 100 through 599.
    pub status: u16,
    /// Whether the destination status is in the 200 through 299 range.
    pub ok: bool,
}

impl ManagedClient {
    /// Performs one authorized broker-owned Vault HTTP operation, without retries.
    ///
    /// Inputs contain only public references/templates. A transport failure,
    /// malformed receipt or `vault_request_outcome_unknown` can mean the operation
    /// executed: reconcile the destination before considering another request.
    ///
    /// # Errors
    /// Rejects oversized or invalid requests locally. Returns only fixed error
    /// codes for remote failures; arbitrary response/error content is discarded.
    pub async fn vault_request(
        &self,
        request: &VaultRequest,
    ) -> Result<VaultRequestReceipt, ManagedError> {
        self.vault_request_at("v1/vault/request", request).await
    }

    pub(crate) async fn vault_request_at(
        &self,
        path: &str,
        request: &VaultRequest,
    ) -> Result<VaultRequestReceipt, ManagedError> {
        validate_id(&request.vault_id)?;
        let invalid = || ManagedError::Configuration("invalid_vault_request".into());
        if request.url.len() > 8 * 1024
            || !Url::parse(&request.url).is_ok_and(|url| {
                url.scheme() == "https"
                    && url.host().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none()
            })
            || request
                .body
                .as_ref()
                .is_some_and(|body| body.len() > 64 * 1024)
            || (request.body_encoding.is_some() && request.body.is_none())
            || request.headers.len() > 64
            || request
                .signing
                .as_ref()
                .is_some_and(|signing| signing.message.is_some() == signing.jwt.is_some())
        {
            return Err(invalid());
        }
        let mut header_bytes = 0;
        let mut names = std::collections::BTreeSet::new();
        for (name, value) in &request.headers {
            header_bytes += name.len() + value.len();
            if name.len() > 128
                || value.len() > 4 * 1024
                || header_bytes > 32 * 1024
                || reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err()
                || reqwest::header::HeaderValue::from_str(value).is_err()
                || !names.insert(name.to_ascii_lowercase())
            {
                return Err(invalid());
            }
        }
        let body = serde_json::to_vec(request).map_err(|_| invalid())?;
        if body.len() > VAULT_REQUEST_MAX_BYTES {
            return Err(ManagedError::Configuration(
                "vault_request_too_large".into(),
            ));
        }
        // This endpoint is not eligible for the client's agent access-token
        // refresh/retry path. Never retry a potentially consequential dispatch.
        let mut response = self
            .request(Method::POST, path, Some(&body), None)
            .await
            .map_err(|_| vault_outcome_unknown())?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| vault_outcome_unknown())?
        {
            if bytes.len() + chunk.len() > 4096 {
                return Err(vault_outcome_unknown());
            }
            bytes.extend_from_slice(&chunk);
        }
        if status == reqwest::StatusCode::OK {
            let receipt: VaultRequestReceipt =
                serde_json::from_slice(&bytes).map_err(|_| vault_outcome_unknown())?;
            if !(100..=599).contains(&receipt.status)
                || receipt.ok != (200..300).contains(&receipt.status)
            {
                return Err(vault_outcome_unknown());
            }
            return Ok(receipt);
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Failure {
            error: String,
        }
        if (status.is_client_error() || status.is_server_error())
            && let Ok(failure) = serde_json::from_slice::<Failure>(&bytes)
            && VAULT_ERROR_CODES.contains(&failure.error.as_str())
        {
            return Err(ManagedError::Http {
                status,
                code: failure.error,
                message: "Vault request failed; no automatic retry".into(),
            });
        }
        Err(vault_outcome_unknown())
    }
}

fn vault_outcome_unknown() -> ManagedError {
    ManagedError::InvalidResponse("vault_request_outcome_unknown; do not retry automatically")
}

// Arbitrary server strings, even strings resembling codes, are never reflected.
const VAULT_ERROR_CODES: &[&str] = &[
    "unauthorized",
    "forbidden",
    "forbidden_origin",
    "method_not_allowed",
    "invalid_vault_request",
    "invalid_vault_placeholder",
    "vault_secret_placeholder_required",
    "vault_entry_kind_mismatch",
    "vault_entry_unavailable",
    "vault_broker_unavailable",
    "invalid_vault_entry_response",
    "vault_destination_denied",
    "vault_header_denied",
    "vault_raw_credential_denied",
    "vault_request_too_large",
    "invalid_vault_signing",
    "invalid_vault_signing_placeholder",
    "vault_signing_failed",
    "vault_signing_key_invalid",
    "vault_signing_kind_mismatch",
    "agent_subject_required",
    "subject_unavailable",
    "subject_deleted",
    "method_denied",
    "vault_request_outcome_unknown",
];

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
