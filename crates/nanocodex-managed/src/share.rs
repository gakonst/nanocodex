//! Owner-scoped thread share links.

use reqwest::Method;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    ManagedClient, ManagedError,
    client::{agent_path, response_error, validate_id},
};

/// A guest's permitted access to one managed thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SharePermission {
    /// Anyone with the link can read the visible conversation.
    Read,
    /// Anyone with the link can read and add guest comments.
    Write,
}

/// Active share-link metadata. Listing never returns a guest secret.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct ShareLink {
    /// Opaque revocable link ID.
    pub id: String,
    /// View or comment permission.
    pub permission: SharePermission,
    /// Creation time as Unix milliseconds.
    pub created_at: u64,
}

/// One-time creation receipt. The URL is a bearer secret and is redacted from Debug.
pub struct CreatedShareLink {
    /// Metadata for revocation.
    pub link: ShareLink,
    /// Bearer URL; only available on creation, never from list.
    pub url: String,
}

impl std::fmt::Debug for CreatedShareLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedShareLink")
            .field("link", &self.link)
            .field("url", &"[REDACTED]")
            .finish()
    }
}

#[derive(Deserialize)]
struct LinkList {
    data: Vec<ShareLink>,
}

#[derive(Deserialize)]
struct CreationReceipt {
    #[serde(flatten)]
    link: ShareLink,
    url: String,
}

fn share_path(agent_id: &str) -> Result<String, ManagedError> {
    validate_id("agent", agent_id)?;
    Ok(format!("{}/share-links", agent_path(agent_id)))
}

fn valid_link_id(id: &str) -> bool {
    id.len() == 36 && uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id)
}

fn validate_link(link: &ShareLink) -> Result<(), ManagedError> {
    // The service uses UUIDs for revocable links.
    if !valid_link_id(&link.id) {
        return Err(ManagedError::InvalidResponse("invalid share link metadata"));
    }
    Ok(())
}

impl ManagedClient {
    /// Lists active share-link metadata, never their bearer URLs.
    pub async fn list_share_links(&self, agent_id: &str) -> Result<Vec<ShareLink>, ManagedError> {
        let path = share_path(agent_id)?;
        let response = self.request(Method::GET, &path, None, None).await?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let list: LinkList = response.json().await.map_err(ManagedError::Transport)?;
        for link in &list.data {
            validate_link(link)?;
        }
        Ok(list.data)
    }

    /// Creates a view or comment link exactly once. On uncertain transport failure,
    /// list metadata rather than retrying: a link may already have been created.
    pub async fn create_share_link(
        &self,
        agent_id: &str,
        permission: SharePermission,
    ) -> Result<CreatedShareLink, ManagedError> {
        let path = share_path(agent_id)?;
        let body = serde_json::to_vec(&serde_json::json!({"permission": permission}))
            .map_err(|_| ManagedError::InvalidResponse("invalid share request"))?;
        let response = self.request(Method::POST, &path, Some(&body), None).await?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let receipt: CreationReceipt = response.json().await.map_err(ManagedError::Transport)?;
        validate_link(&receipt.link)?;
        if receipt.link.permission != permission
            || !valid_share_url(&receipt.url, &self.base_url, agent_id)
        {
            return Err(ManagedError::InvalidResponse(
                "invalid share link creation receipt",
            ));
        }
        Ok(CreatedShareLink {
            link: receipt.link,
            url: receipt.url,
        })
    }

    /// Revokes one link exactly once. A transport error does not prove failure;
    /// consult list rather than repeating an uncertain mutation.
    pub async fn revoke_share_link(
        &self,
        agent_id: &str,
        link_id: &str,
    ) -> Result<(), ManagedError> {
        let path = share_path(agent_id)?;
        if !valid_link_id(link_id) {
            return Err(ManagedError::Configuration(
                "share link ID must be a UUID".into(),
            ));
        }
        let response = self
            .request(Method::DELETE, &format!("{path}/{link_id}"), None, None)
            .await?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        Ok(())
    }
}

fn valid_share_url(value: &str, origin: &Url, agent_id: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        let Some(token) = url
            .fragment()
            .and_then(|fragment| fragment.strip_prefix("token=nsl_"))
        else {
            return false;
        };
        url.origin() == origin.origin()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == format!("/share/{agent_id}")
            && url.query().is_none()
            && token.len() == 43
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManagedApiKey;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, Method as HttpMethod, StatusCode},
        routing::get,
    };
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    type RecordedCalls = Arc<Mutex<Vec<(HttpMethod, String)>>>;

    #[tokio::test]
    async fn owner_share_journey_and_failure_are_not_retried_or_exposed() {
        let calls = Arc::new(Mutex::new(Vec::<(HttpMethod, String)>::new()));
        let app = Router::new().route("/v1/agents/{agent}/share-links", get({
            async fn list(State(calls): State<RecordedCalls>, headers: HeaderMap) -> Json<serde_json::Value> {
                assert!(headers.get("authorization").is_some());
                calls.lock().unwrap().push((HttpMethod::GET, String::new()));
                Json(serde_json::json!({"data": [{"id": "00000000-0000-4000-8000-000000000001", "permission": "read", "created_at": 123}]}))
            }
            list
        }).post({
            async fn create(State(calls): State<RecordedCalls>, headers: HeaderMap, Json(body): Json<serde_json::Value>) -> (StatusCode, Json<serde_json::Value>) {
                calls.lock().unwrap().push((HttpMethod::POST, body.to_string()));
                if body["permission"] == "read" {
                    return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "forbidden"})));
                }
                (StatusCode::CREATED, Json(serde_json::json!({"id": "00000000-0000-4000-8000-000000000002", "permission": "write", "created_at": 124, "url": format!("http://{}/share/agent-1#token=nsl_{}", headers.get("host").unwrap().to_str().unwrap(), "x".repeat(43))})))
            }
            create
        })).route("/v1/agents/{agent}/share-links/{id}", axum::routing::delete({
            async fn revoke(State(calls): State<RecordedCalls>) -> StatusCode {
                calls.lock().unwrap().push((HttpMethod::DELETE, String::new()));
                StatusCode::NO_CONTENT
            }
            revoke
        })).with_state(calls.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let key = ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)))
            .unwrap();
        let client = crate::ManagedClient::new(&origin, key).unwrap();
        let list = client.list_share_links("agent-1").await.unwrap();
        assert_eq!(list[0].id, "00000000-0000-4000-8000-000000000001");
        let created = client
            .create_share_link("agent-1", SharePermission::Write)
            .await
            .unwrap();
        assert_eq!(
            created.url,
            format!("{origin}/share/agent-1#token=nsl_{}", "x".repeat(43))
        );
        client
            .revoke_share_link("agent-1", "00000000-0000-4000-8000-000000000002")
            .await
            .unwrap();
        let error = client
            .create_share_link("agent-1", SharePermission::Read)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ManagedError::Http {
                status: StatusCode::FORBIDDEN,
                ..
            }
        ));
        assert!(!format!("{error:?}").contains("nsl_"));
        assert!(!format!("{created:?}").contains("nsl_"));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                (HttpMethod::GET, String::new()),
                (HttpMethod::POST, r#"{"permission":"write"}"#.into()),
                (HttpMethod::DELETE, String::new()),
                (HttpMethod::POST, r#"{"permission":"read"}"#.into()),
            ]
        );
        server.abort();
    }
}
