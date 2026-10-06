//! Private owner input transport. Values never pass through agent/tool endpoints.
#![allow(missing_docs)]
use crate::{ManagedClient, ManagedError};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrivateInputKind {
    Login,
    Takeover,
    Otp,
    Password,
    Form,
    Vault(String),
    /// Caller-created account credential form; never decoded from tool output.
    Credential(String),
    /// Caller-local provider sign-in panel, excluded from model decoding.
    Connector(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateInputRequest {
    pub request_id: String,
    pub agent_id: String,
    pub origin: String,
    pub expires_at: Option<u64>,
    pub kind: PrivateInputKind,
    pub allowed_origins: Vec<String>,
    pub name: String,
}
impl PrivateInputRequest {
    pub fn is_current(&self) -> bool {
        valid_id(&self.agent_id, 1, 128)
            && valid_id(&self.request_id, 1, 256)
            && self
                .expires_at
                .is_none_or(|v| v > now() && v <= 9_007_199_254_740_991)
            && match &self.kind {
                PrivateInputKind::Connector(kind) => {
                    self.origin.is_empty()
                        && self.expires_at.is_some()
                        && ((kind == "whatsapp" && uuid::Uuid::parse_str(&self.name).is_ok())
                            || (kind == "chatgpt"
                                && ["start", "status"].contains(&self.name.as_str())))
                }
                PrivateInputKind::Credential(kind) => {
                    self.origin.is_empty()
                        && (kind == "openai"
                            || (kind == "ssh"
                                && !self.name.is_empty()
                                && self.name.len() <= 64
                                && self.name.as_bytes()[0].is_ascii_alphanumeric()
                                && self
                                    .name
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                                && !["__proto__", "constructor", "prototype"]
                                    .contains(&self.name.as_str())))
                }
                PrivateInputKind::Vault(kind) => {
                    matches!(
                        kind.as_str(),
                        "login" | "api_key" | "card" | "address" | "phone"
                    ) && (self.origin.is_empty() || valid_origin(&self.origin))
                }
                _ => {
                    valid_origin(&self.origin)
                        && self.expires_at.is_some()
                        && uuid::Uuid::parse_str(&self.request_id)
                            .is_ok_and(|id| id.to_string() == self.request_id)
                }
            }
    }
    pub fn parse(value: &Value, agent: &str) -> Option<Self> {
        let mut found = Vec::new();
        let mut budget = 256;
        walk(value, agent, 0, &mut budget, &mut found)?;
        let first = found.pop()?;
        found.iter().all(|other| *other == first).then_some(first)
    }
    pub fn path(&self) -> String {
        match &self.kind {
            PrivateInputKind::Vault(kind) => format!("v1/credentials/vault/{kind}"),
            PrivateInputKind::Credential(kind) => {
                if kind == "ssh" {
                    format!("v1/credentials/ssh/{}", self.name)
                } else {
                    "v1/credentials/openai".into()
                }
            }
            _ => format!(
                "v1/agents/{}/{}",
                self.agent_id,
                match self.kind {
                    PrivateInputKind::Login | PrivateInputKind::Takeover =>
                        "browser-vault/takeover",
                    PrivateInputKind::Otp => "browser-vault/challenge",
                    _ => "secure-input",
                }
            ),
        }
    }
    pub fn control(&self, action: &str) -> Value {
        json!({if matches!(self.kind, PrivateInputKind::Login | PrivateInputKind::Takeover | PrivateInputKind::Otp) {"challenge_id"} else {"request_id"}: self.request_id, "action":action})
    }
}
/// Remove the host's Code Mode status envelope before decoding its JSON output.
pub fn private_input_output_text(text: &str) -> &str {
    if [
        "Script completed",
        "Script running",
        "Script terminated",
        "Script failed",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
        && let Some((_, output)) = text.split_once("\nOutput:\n")
    {
        return output;
    }
    text
}
fn walk(
    v: &Value,
    agent: &str,
    depth: usize,
    budget: &mut usize,
    found: &mut Vec<PrivateInputRequest>,
) -> Option<()> {
    if depth >= 12 || *budget == 0 {
        return None;
    }
    *budget -= 1;
    if let Some(text) = v.as_str() {
        if text.len() > 65536 {
            return None;
        }
        if let Ok(v) = serde_json::from_str::<Value>(private_input_output_text(text)) {
            walk(&v, agent, depth + 1, budget, found)?;
        }
        return Some(());
    }
    if let Some(items) = v.as_array() {
        for item in items {
            walk(item, agent, depth + 1, budget, found)?;
        }
        return Some(());
    }
    let Some(o) = v.as_object() else {
        return Some(());
    };
    if o.get("isError") == Some(&Value::Bool(true)) || o.get("success") == Some(&Value::Bool(false))
    {
        return None;
    }
    let t = s(v, "type");
    let kind = match t {
        "browser_login" => Some(PrivateInputKind::Login),
        "browser_vault_takeover" => Some(PrivateInputKind::Takeover),
        "browser_vault_challenge" => Some(PrivateInputKind::Otp),
        "secure_input" => match s(v, "kind") {
            "browser_password" => Some(PrivateInputKind::Password),
            "browser_form" => Some(PrivateInputKind::Form),
            _ => None,
        },
        "vault_intake"
            if matches!(s(v, "operation"), "" | "create")
                && matches!(
                    s(v, "kind"),
                    "login" | "api_key" | "card" | "address" | "phone"
                ) =>
        {
            Some(PrivateInputKind::Vault(s(v, "kind").into()))
        }
        _ => None,
    };
    if let Some(kind) = kind {
        if s(v, "status") != "input_required" {
            return Some(());
        }
        let vault = matches!(kind, PrivateInputKind::Vault(_));
        let id = if vault {
            // Stable receipt selector, without trusting arbitrary model IDs.
            {
                use sha2::{Digest, Sha256};
                format!(
                    "vault_{}",
                    Sha256::digest(serde_json::to_vec(v).ok()?)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                )
            }
        } else if matches!(
            kind,
            PrivateInputKind::Login | PrivateInputKind::Form | PrivateInputKind::Password
        ) {
            s(v, "request_id").into()
        } else {
            s(v, "challenge_id").into()
        };
        if !valid_id(&id, 1, 256)
            || (!vault && s(v, "agent_id") != agent)
            || !valid_id(agent, 1, 128)
        {
            return None;
        }
        let origin = s(v, "origin").to_owned();
        if !(vault && origin.is_empty()) && !valid_origin(&origin) {
            return None;
        }
        let expiry = v.get("expires_at").and_then(Value::as_u64);
        if !vault && expiry.is_none() {
            return None;
        }
        let allowed = if kind == PrivateInputKind::Login {
            let items = v.get("allowed_origins")?.as_array()?;
            if items.is_empty() || items.len() > 8 {
                return None;
            }
            let sites: Vec<String> = items
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<_>>()?;
            if !sites.contains(&origin)
                || sites.iter().any(|v| !valid_origin(v))
                || s(v, "challenge_id") != id
            {
                return None;
            }
            sites
        } else {
            vec![]
        };
        let name = s(v, "name");
        if name.len() > 120 || name.chars().any(char::is_control) {
            return None;
        }
        found.push(PrivateInputRequest {
            request_id: id,
            agent_id: agent.into(),
            origin,
            expires_at: expiry,
            kind,
            allowed_origins: allowed,
            name: name.into(),
        });
    } else {
        for key in ["content", "text", "structuredContent", "result", "output"] {
            if let Some(child) = o.get(key) {
                walk(child, agent, depth + 1, budget, found)?;
            }
        }
    }
    Some(())
}
pub fn valid_origin(v: &str) -> bool {
    v.len() <= 2048
        && reqwest::Url::parse(v).is_ok_and(|u| {
            u.scheme() == "https"
                && u.host().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.origin().ascii_serialization() == v
        })
}
fn valid_id(v: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&v.len())
        && v.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |v| v.as_millis() as u64)
}
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn invalid() -> ManagedError {
    ManagedError::InvalidResponse("private input unavailable; no automatic retry")
}

/// A private JSON body with no Debug/Clone/Serialize implementation. It is erased on drop.
pub struct PrivateInputBody(Zeroizing<Vec<u8>>);
impl PrivateInputBody {
    pub fn encode(value: &Value) -> Result<Self, ManagedError> {
        let bytes = Zeroizing::new(serde_json::to_vec(value).map_err(|_| invalid())?);
        if bytes.len() > 256 * 1024 {
            return Err(invalid());
        }
        Ok(Self(bytes))
    }
}
/// Safe picker metadata; no credential material is retained or exposed.
#[derive(Clone)]
pub struct PrivateVaultItem {
    pub id: String,
    pub name: String,
    pub kind: String,
}
impl ManagedClient {
    /// Read only the authenticated owner's safe Vault catalogue, without redirects or retries.
    pub async fn private_vault_items(&self) -> Result<Vec<PrivateVaultItem>, ManagedError> {
        let mut builder = self
            .http
            .get(self.url("v1/credentials")?)
            .timeout(Duration::from_secs(30))
            .header("cache-control", "no-store");
        if let Some(origin) = &self.request_origin {
            builder = builder.header("x-nanocodex-client-context", origin);
        }
        let mut response = builder.send().await.map_err(|_| invalid())?;
        if !response.status().is_success() {
            return Err(invalid());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err(invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let entries = value
            .get("vault")
            .and_then(Value::as_array)
            .ok_or_else(invalid)?;
        if entries.len() > 2000 {
            return Err(invalid());
        }
        let mut items: Vec<PrivateVaultItem> = Vec::new();
        for entry in entries {
            let (id, name, kind) = (s(entry, "id"), s(entry, "name"), s(entry, "kind"));
            if !matches!(kind, "login" | "api_key" | "card" | "address" | "phone") {
                continue;
            }
            if !valid_id(id, 22, 64)
                || name.len() > 512
                || name.chars().any(char::is_control)
                || items.iter().any(|item| item.id == id)
            {
                return Err(invalid());
            }
            items.push(PrivateVaultItem {
                id: id.into(),
                name: name.into(),
                kind: kind.into(),
            });
        }
        Ok(items)
    }
    /// Send exactly once with the immutable authenticated client's redirect-disabled transport.
    /// Errors are fixed; server bodies never enter diagnostics, history or tools.
    pub async fn private_input_post(
        &self,
        request: &PrivateInputRequest,
        body: PrivateInputBody,
    ) -> Result<Value, ManagedError> {
        if !request.is_current() || matches!(request.kind, PrivateInputKind::Connector(_)) {
            return Err(invalid());
        }
        let mut builder = self
            .http
            .request(
                if matches!(request.kind, PrivateInputKind::Credential(_)) {
                    reqwest::Method::PUT
                } else {
                    reqwest::Method::POST
                },
                self.url(&request.path())?,
            )
            .timeout(Duration::from_secs(60))
            .header("content-type", "application/json")
            .header("cache-control", "no-store")
            .body(body.0.to_vec());
        if matches!(request.kind, PrivateInputKind::Vault(_)) {
            // The TUI binds this receipt selector to its originating turn. Convert
            // it to a stable UUID so repeat submissions share the broker receipt.
            let operation_id = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("nanocodex:vault-intake:{}", request.request_id).as_bytes(),
            );
            builder = builder.header("x-nanocodex-operation-id", operation_id.to_string());
        }
        if let Some(origin) = &self.request_origin {
            builder = builder.header("x-nanocodex-client-context", origin);
        }
        let mut response = builder.send().await.map_err(|_| invalid())?;
        if !response.status().is_success() {
            return Err(invalid());
        }
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
            if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
                return Err(invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| invalid())
    }
    /// Private native display transport. Never serialize its response into agent events.
    pub async fn private_connector_input(
        &self,
        request: &PrivateInputRequest,
        start: bool,
    ) -> Result<Value, ManagedError> {
        if !request.is_current() {
            return Err(invalid());
        }
        let (method, path) = match &request.kind {
            PrivateInputKind::Connector(kind) if kind == "whatsapp" => (
                reqwest::Method::GET,
                format!(
                    "v1/connectors/whatsapp/pairing?operation_id={}",
                    request.name
                ),
            ),
            PrivateInputKind::Connector(kind) if kind == "chatgpt" => (
                if start && request.name == "start" {
                    reqwest::Method::POST
                } else {
                    reqwest::Method::GET
                },
                "v1/credentials/chatgpt/login".into(),
            ),
            _ => return Err(invalid()),
        };
        let mut response = self
            .request(method, &path, None, None)
            .await
            .map_err(|_| invalid())?;
        if !response.status().is_success() {
            return Err(invalid());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
            if bytes.len() + chunk.len() > 16384 {
                return Err(invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| invalid())
    }
    /// Account-owned fallback route; never accepts a model-supplied URL.
    pub fn private_input_browser_url(
        &self,
        request: &PrivateInputRequest,
    ) -> Result<String, ManagedError> {
        let mut url = reqwest::Url::parse(&self.vault_url()).map_err(|_| invalid())?;
        url.set_query(None);
        if request.kind == PrivateInputKind::Login {
            url.set_path("/browser-login");
            url.query_pairs_mut()
                .append_pair("agent", &request.agent_id)
                .append_pair("request", &request.request_id);
        } else {
            url.set_path(&format!("/agent/{}", request.agent_id));
        }
        Ok(url.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    const ID: &str = "11111111-1111-4111-8111-111111111111";
    fn hint() -> Value {
        json!({"type":"browser_login","status":"input_required","request_id":ID,"challenge_id":ID,"agent_id":"agent","origin":"https://example.com","allowed_origins":["https://example.com"],"expires_at":now()+60000})
    }
    #[test]
    fn receipt_bindings_and_identical_exec_echoes() {
        let h = hint();
        let r = PrivateInputRequest::parse(&h, "agent").unwrap();
        assert!(r.is_current());
        assert_eq!(
            PrivateInputRequest::parse(
                &json!({"content":[{"type":"text","text":h.to_string()}],"structuredContent":h}),
                "agent"
            ),
            Some(r)
        );
        assert!(PrivateInputRequest::parse(&h, "other").is_none());
        assert!(PrivateInputRequest::parse(&json!({"isError":true,"output":h}), "agent").is_none());
        let mut changed = h.clone();
        changed["request_id"] = json!("22222222-2222-4222-8222-222222222222");
        changed["challenge_id"] = changed["request_id"].clone();
        assert!(PrivateInputRequest::parse(&json!([h, changed]), "agent").is_none());
        for origin in [
            "http://example.com",
            "https://example.com/path",
            "https://example.com@evil.test",
            "https://example.com/",
        ] {
            let mut h = hint();
            h["origin"] = json!(origin);
            assert!(PrivateInputRequest::parse(&h, "agent").is_none());
        }
        let mut h = hint();
        h["expires_at"] = json!(1);
        assert!(
            !PrivateInputRequest::parse(&h, "agent")
                .unwrap()
                .is_current()
        );
        let mut nested = hint();
        for _ in 0..15 {
            nested = json!({"output":nested});
        }
        assert!(PrivateInputRequest::parse(&nested, "agent").is_none());
    }
    async fn server(app: Router) -> (ManagedClient, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let key =
            crate::ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)))
                .unwrap();
        (
            ManagedClient::new(format!("http://{address}"), key).unwrap(),
            task,
        )
    }
    #[tokio::test]
    async fn private_route_authentication_and_no_redirect_or_error_body() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::BAD_REQUEST,
            StatusCode::TEMPORARY_REDIRECT,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let count = Arc::new(AtomicUsize::new(0));
            let calls = count.clone();
            let app = Router::new()
                .route(
                    "/v1/agents/agent/browser-vault/takeover",
                    post(move |headers: HeaderMap, Json(body): Json<Value>| {
                        let calls = calls.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            assert!(
                                headers["authorization"]
                                    .to_str()
                                    .unwrap()
                                    .starts_with("Bearer ncx_live_")
                            );
                            assert_eq!(body["challenge_id"], ID);
                            assert_eq!(body["fields"][0]["value"], "fixture-private-value");
                            (
                                status,
                                [
                                    ("location", "/v1/agents/agent/turns"),
                                    ("x-nanocodex-access-rejected", "1"),
                                ],
                                "fixture-private-value",
                            )
                        }
                    }),
                )
                .route(
                    "/v1/agents/agent/turns",
                    post(|| async { Json(json!({"unexpected_redirect": true})) }),
                );
            let (client, task) = server(app).await;
            let r = PrivateInputRequest::parse(&hint(), "agent").unwrap();
            let error=client.private_input_post(&r,PrivateInputBody::encode(&json!({"challenge_id":ID,"action":"fill_fields","fields":[{"ref":ID,"value":"fixture-private-value"}]})).unwrap()).await.unwrap_err();
            assert!(!format!("{error:?} {error}").contains("fixture-private-value"));
            assert_eq!(count.load(Ordering::SeqCst), 1);
            task.abort();
        }
    }
    #[tokio::test]
    async fn vault_picker_projects_only_safe_metadata_and_never_follows_redirects() {
        use axum::routing::get;
        let app = Router::new().route("/v1/credentials", get(|headers:HeaderMap| async move {
            assert!(headers["authorization"].to_str().unwrap().starts_with("Bearer ncx_live_"));
            Json(json!({"vault":[
                {"id":"fixture_login_1234567890","kind":"login","name":"Login","password":"never-project-canary"},
                {"id":"fixture_apikey_1234567890","kind":"api_key","name":"API"},
                {"id":"fixture_card_1234567890","kind":"card","name":"Card"},
                {"id":"fixture_address_1234567890","kind":"address","name":"Address"},
                {"id":"fixture_phone_1234567890","kind":"phone","name":"Phone"}
            ]}))
        }));
        let (client, task) = server(app).await;
        let items = client.private_vault_items().await.unwrap();
        assert_eq!(items.len(), 5);
        assert_eq!(
            items.iter().map(|i| i.kind.as_str()).collect::<Vec<_>>(),
            vec!["login", "api_key", "card", "address", "phone"]
        );
        assert!(
            items
                .iter()
                .all(|i| !format!("{}{}{}", i.id, i.name, i.kind).contains("never-project-canary"))
        );
        task.abort();
        let app = Router::new()
            .route(
                "/v1/credentials",
                get(|| async {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [("location", "/secret")],
                        "never-project-canary",
                    )
                }),
            )
            .route("/secret", get(|| async { Json(json!({"vault": []})) }));
        let (client, task) = server(app).await;
        let result = client.private_vault_items().await;
        assert!(result.is_err());
        assert!(
            !result
                .err()
                .unwrap()
                .to_string()
                .contains("never-project-canary")
        );
        task.abort();
    }
    #[test]
    fn nested_code_mode_receipts_reject_conflicting_bindings() {
        let h = hint();
        let envelope = json!({"content":[{"type":"text","text":format!("Script completed\nWall time: 1s\nOutput:\n{}",json!({"content":[{"type":"text","text":h.to_string()}],"structuredContent":h}))}]});
        assert_eq!(
            PrivateInputRequest::parse(&envelope, "agent"),
            PrivateInputRequest::parse(&h, "agent")
        );
        let mut other = h;
        other["origin"] = json!("https://other.test");
        other["allowed_origins"] = json!(["https://other.test"]);
        assert!(PrivateInputRequest::parse(&json!([envelope, other]), "agent").is_none());
    }
    #[tokio::test]
    async fn fallback_routes_match_existing_account_pages() {
        let (client, task) = server(Router::new()).await;
        let mut r = PrivateInputRequest::parse(&hint(), "agent").unwrap();
        let login = reqwest::Url::parse(&client.private_input_browser_url(&r).unwrap()).unwrap();
        assert_eq!(login.path(), "/browser-login");
        assert!(login.query().unwrap().contains("request="));
        r.kind = PrivateInputKind::Takeover;
        let takeover = reqwest::Url::parse(&client.private_input_browser_url(&r).unwrap()).unwrap();
        assert_eq!(takeover.path(), "/agent/agent");
        assert!(takeover.query().is_none());
        task.abort();
    }
}
