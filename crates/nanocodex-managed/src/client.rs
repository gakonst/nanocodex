use std::{fmt, sync::Arc};

use reqwest::{
    Method, Response,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue},
};
use serde::{Deserialize, de::DeserializeOwned};
use url::{Host, Url};
use zeroize::Zeroize;

use nanocodex_oai_api::{Model, ReasoningMode, Thinking};

use crate::{
    AgentList, AgentReceipt, AgentSettings, AgentSettingsPatch, AgentSettingsResponse, AgentState,
    EventCursor, EventHistoryPage, FindSessionsRequest, FindSessionsResponse, ManagedApiKey,
    ManagedError, ManagedEventStream, MemoryKey, MemoryListResponse, MemoryRecord, PromptInput,
    ReadSessionBody, ReadSessionRequest, ReadSessionResponse, TurnAction, TurnSteer,
    TurnSubmission, TurnView,
};

const MAX_HISTORY_PAGE: u16 = 256;
const SUBMIT_ATTEMPTS: usize = 3;

/// Builder for a cloneable native managed HTTP client.
///
/// The builder owns the validated account credential and does not consult the
/// process environment.
pub struct ManagedClientBuilder {
    origin: Url,
    api_key: ManagedApiKey,
}

impl fmt::Debug for ManagedClientBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedClientBuilder")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl ManagedClientBuilder {
    /// Starts a builder from an HTTP(S) origin and validated account API key.
    ///
    /// # Errors
    ///
    /// Returns ManagedError::Configuration when the origin is not a URL.
    pub fn new(origin: impl AsRef<str>, api_key: ManagedApiKey) -> Result<Self, ManagedError> {
        let origin = Url::parse(origin.as_ref())
            .map_err(|_| ManagedError::Configuration("managed origin must be a URL".to_owned()))?;
        Ok(Self { origin, api_key })
    }

    /// Validates transport policy and builds the reusable managed client.
    ///
    /// Plain HTTP is accepted only for literal loopback hosts. The
    /// client never follows redirects.
    ///
    /// # Errors
    ///
    /// Returns a configuration or HTTP-client construction failure.
    pub fn build(self) -> Result<ManagedClient, ManagedError> {
        ManagedClient::from_builder(self)
    }
}

/// Cloneable authenticated client for the account-managed control plane.
///
/// Clones share one redirect-disabled HTTP pool and immutable authorization
/// policy. Turn submission is the only ordinary request that retries.
#[derive(Clone)]
pub struct ManagedClient {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: Url,
    pub(crate) bearer: Arc<str>,
}

impl fmt::Debug for ManagedClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl ManagedClient {
    /// Starts configuring a native managed client.
    ///
    /// # Errors
    ///
    /// Returns ManagedError::Configuration when the origin is not a URL.
    pub fn builder(
        origin: impl AsRef<str>,
        api_key: ManagedApiKey,
    ) -> Result<ManagedClientBuilder, ManagedError> {
        ManagedClientBuilder::new(origin, api_key)
    }

    /// Builds a native managed client with default transport policy.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid origin or if the underlying HTTP client
    /// cannot be constructed.
    pub fn new(origin: impl AsRef<str>, api_key: ManagedApiKey) -> Result<Self, ManagedError> {
        Self::builder(origin, api_key)?.build()
    }

    fn from_builder(mut builder: ManagedClientBuilder) -> Result<Self, ManagedError> {
        install_default_rustls_crypto_provider();
        validate_origin(&builder.origin)?;
        builder.origin.set_path("/");

        let api_bearer: Arc<str> = Arc::from(builder.api_key.expose());

        let mut bearer = b"Bearer ".to_vec();
        bearer.extend_from_slice(builder.api_key.expose().as_bytes());
        let mut authorization = HeaderValue::from_bytes(&bearer).map_err(|_| {
            ManagedError::Configuration("managed API key cannot form authorization".to_owned())
        })?;
        bearer.zeroize();
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(ManagedError::Transport)?;
        drop(builder.api_key);

        Ok(Self {
            http,
            base_url: builder.origin,
            bearer: api_bearer,
        })
    }

    /// Creates a new account-owned managed agent.
    ///
    /// # Errors
    ///
    /// Returns a transport, HTTP, size, or response-schema failure.
    pub async fn create(&self) -> Result<AgentReceipt, ManagedError> {
        self.json(Method::POST, "v1/agents", None, None).await
    }

    pub(crate) async fn create_with_settings(
        &self,
        settings: AgentSettings,
    ) -> Result<AgentReceipt, ManagedError> {
        let body = serde_json::to_vec(&serde_json::json!({ "settings": settings }))
            .map_err(|_| ManagedError::InvalidResponse("failed to encode agent settings"))?;
        self.json(Method::POST, "v1/agents", Some(&body), None)
            .await
    }

    /// Lists account-owned managed agents.
    ///
    /// # Errors
    ///
    /// Returns a transport, HTTP, size, or response-schema failure.
    pub async fn list(&self) -> Result<AgentList, ManagedError> {
        self.json(Method::GET, "v1/agents", None, None).await
    }

    /// Reads the current durable state of one managed agent.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation, transport, HTTP, size, or
    /// response-schema failure.
    pub async fn state(&self, agent_id: &str) -> Result<AgentState, ManagedError> {
        validate_id("agent", agent_id)?;
        let state: AgentState = self
            .json(Method::GET, &agent_path(agent_id), None, None)
            .await?;
        crate::sse::validate_numeric_cursor(&state.latest_event_cursor).map_err(|_| {
            ManagedError::InvalidResponse("agent state latest event cursor is invalid")
        })?;
        Ok(state)
    }

    /// Replaces the complete managed settings policy.
    ///
    /// Model and reasoning mode are accepted by the service only before the
    /// first turn. Prefer [`Self::set_thinking`] and [`Self::set_fast_mode`]
    /// for settings that remain dynamic later in the lifecycle.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation, transport, HTTP, size, or
    /// response-schema failure. The service also rejects immutable settings
    /// after the first turn has been accepted.
    pub async fn set_settings(
        &self,
        agent_id: &str,
        settings: AgentSettings,
    ) -> Result<AgentSettings, ManagedError> {
        self.patch_settings(agent_id, AgentSettingsPatch::from(settings))
            .await
    }

    /// Selects the hosted model before this agent's first accepted turn.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation, transport, HTTP, size, or
    /// response-schema failure, including the service's immutable-setting
    /// rejection after first-turn admission.
    pub async fn set_model(
        &self,
        agent_id: &str,
        model: Model,
    ) -> Result<AgentSettings, ManagedError> {
        self.patch_settings(
            agent_id,
            AgentSettingsPatch {
                model: Some(model),
                ..AgentSettingsPatch::default()
            },
        )
        .await
    }

    /// Selects the reasoning execution mode before the first accepted turn.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation, transport, HTTP, size, or
    /// response-schema failure, including the service's immutable-setting
    /// rejection after first-turn admission.
    pub async fn set_reasoning_mode(
        &self,
        agent_id: &str,
        reasoning_mode: ReasoningMode,
    ) -> Result<AgentSettings, ManagedError> {
        self.patch_settings(
            agent_id,
            AgentSettingsPatch {
                reasoning_mode: Some(reasoning_mode),
                ..AgentSettingsPatch::default()
            },
        )
        .await
    }

    /// Changes the reasoning effort for subsequently accepted turns.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation, transport, HTTP, size, or
    /// response-schema failure.
    pub async fn set_thinking(
        &self,
        agent_id: &str,
        thinking: Thinking,
    ) -> Result<AgentSettings, ManagedError> {
        self.patch_settings(
            agent_id,
            AgentSettingsPatch {
                thinking: Some(thinking),
                ..AgentSettingsPatch::default()
            },
        )
        .await
    }

    /// Enables or disables priority processing for subsequently accepted turns.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation, transport, HTTP, size, or
    /// response-schema failure.
    pub async fn set_fast_mode(
        &self,
        agent_id: &str,
        enabled: bool,
    ) -> Result<AgentSettings, ManagedError> {
        self.patch_settings(
            agent_id,
            AgentSettingsPatch {
                fast_mode: Some(enabled),
                ..AgentSettingsPatch::default()
            },
        )
        .await
    }

    async fn patch_settings(
        &self,
        agent_id: &str,
        patch: AgentSettingsPatch,
    ) -> Result<AgentSettings, ManagedError> {
        validate_id("agent", agent_id)?;
        let body = serde_json::to_vec(&patch)
            .map_err(|_| ManagedError::InvalidResponse("failed to encode agent settings"))?;
        let response: AgentSettingsResponse = self
            .json(
                Method::PATCH,
                &format!("{}/settings", agent_path(agent_id)),
                Some(&body),
                None,
            )
            .await?;
        Ok(response.settings)
    }

    /// Deletes one account-owned managed agent.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation, transport, or HTTP failure.
    pub async fn delete(&self, agent_id: &str) -> Result<(), ManagedError> {
        validate_id("agent", agent_id)?;
        let response = self
            .request(Method::DELETE, &agent_path(agent_id), None, None)
            .await?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        Ok(())
    }

    /// Searches retained managed sessions.
    ///
    /// # Errors
    ///
    /// Returns a request-validation, transport, HTTP, size, or response-schema
    /// failure.
    pub async fn find(
        &self,
        request: &FindSessionsRequest,
    ) -> Result<FindSessionsResponse, ManagedError> {
        request.validate()?;
        let body = serde_json::to_vec(request)
            .map_err(|_| ManagedError::InvalidResponse("failed to encode session search"))?;
        self.json(
            Method::POST,
            "/v1/history/sessions/search",
            Some(&body),
            None,
        )
        .await
    }

    /// Reads selected turns from one retained managed session.
    ///
    /// # Errors
    ///
    /// Returns a request-validation, transport, HTTP, size, or response-schema
    /// failure.
    pub async fn read(
        &self,
        request: &ReadSessionRequest,
    ) -> Result<ReadSessionResponse, ManagedError> {
        request.validate()?;
        let body = serde_json::to_vec(&ReadSessionBody {
            turn_ids: request.turn_ids.as_deref(),
        })
        .map_err(|_| ManagedError::InvalidResponse("failed to encode session read"))?;
        self.json(
            Method::POST,
            &format!("/v1/history/sessions/{}/read", request.session_id),
            Some(&body),
            None,
        )
        .await
    }

    /// Lists versioned account memories.
    ///
    /// # Errors
    ///
    /// Returns a transport, HTTP, size, response-schema, or memory-key
    /// validation failure.
    pub async fn memory(&self) -> Result<Vec<MemoryRecord>, ManagedError> {
        let response: MemoryListResponse = self.json(Method::GET, "/v1/memory", None, None).await?;
        for memory in &response.memories {
            memory.key.validate()?;
        }
        Ok(response.memories)
    }

    /// Deletes one exact version of an account memory.
    ///
    /// # Errors
    ///
    /// Returns a memory-key validation, transport, or HTTP failure.
    pub async fn delete_memory(&self, key: MemoryKey) -> Result<(), ManagedError> {
        key.validate()?;
        let mut url = self.url(&format!("/v1/memory/{}", key.id))?;
        url.query_pairs_mut()
            .append_pair("version", &key.version.to_string());
        let response = self
            .http
            .delete(url)
            .send()
            .await
            .map_err(ManagedError::Transport)?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        Ok(())
    }

    /// Reads a bounded page of durable managed events.
    ///
    /// Events are returned in strict cursor order. The page limit must be from
    /// 1 through 256 and an optional before cursor must be positive.
    ///
    /// # Errors
    ///
    /// Returns a request-validation, transport, HTTP, size, response-schema,
    /// or event-ordering failure.
    pub async fn history(
        &self,
        agent_id: &str,
        before: Option<&str>,
        limit: u16,
    ) -> Result<EventHistoryPage, ManagedError> {
        validate_id("agent", agent_id)?;
        if limit == 0 || limit > MAX_HISTORY_PAGE {
            return Err(ManagedError::Configuration(
                "managed history limit must be from 1 through 256".to_owned(),
            ));
        }
        if let Some(cursor) = before {
            crate::sse::validate_numeric_cursor(cursor)?;
            if cursor == "0" {
                return Err(ManagedError::Configuration(
                    "managed history cursor must be positive".to_owned(),
                ));
            }
        }
        let mut url = self.url(&format!("{}/events/history", agent_path(agent_id)))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("limit", &limit.to_string());
            if let Some(cursor) = before {
                query.append_pair("before", cursor);
            }
        }
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(ManagedError::Transport)?;
        let page: EventHistoryPage = decode_response(response).await?;
        crate::sse::validate_numeric_cursor(&page.latest_cursor)?;
        if page.data.len() > limit as usize {
            return Err(ManagedError::InvalidResponse(
                "history page exceeds the requested limit",
            ));
        }
        let mut previous = None;
        for event in &page.data {
            crate::sse::validate_numeric_cursor(&event.cursor)?;
            if previous.is_some_and(|cursor| !crate::sse::cursor_before(cursor, &event.cursor))
                || before.is_some_and(|cursor| !crate::sse::cursor_before(&event.cursor, cursor))
            {
                return Err(ManagedError::InvalidResponse(
                    "history events are not strictly ordered",
                ));
            }
            previous = Some(event.cursor.as_str());
        }
        Ok(page)
    }

    /// Durably submits a turn.
    ///
    /// A transport failure is retried until exactly three total attempts have
    /// been made. Every attempt sends the same encoded body and idempotency
    /// key. HTTP and schema failures are never retried; a transport failure
    /// while reading a successful response body is retried like a send failure.
    ///
    /// # Errors
    ///
    /// Returns a request-validation, transport, HTTP, size, or response-schema
    /// failure.
    pub async fn submit(
        &self,
        agent_id: &str,
        turn_id: Option<&str>,
        idempotency_key: &str,
        input: &PromptInput,
    ) -> Result<TurnView, ManagedError> {
        validate_id("agent", agent_id)?;
        if let Some(turn_id) = turn_id {
            validate_id("turn", turn_id)?;
        }
        validate_idempotency_key(idempotency_key)?;
        let body = serde_json::to_vec(&TurnSubmission { id: turn_id, input })
            .map_err(|_| ManagedError::InvalidResponse("failed to encode prompt"))?;
        let path = format!("{}/turns", agent_path(agent_id));
        let mut last_transport = None;
        for _ in 0..SUBMIT_ATTEMPTS {
            match self
                .request(Method::POST, &path, Some(&body), Some(idempotency_key))
                .await
            {
                Ok(response) => match decode_response(response).await {
                    Err(ManagedError::Transport(error)) => last_transport = Some(error),
                    result => return result,
                },
                Err(ManagedError::Transport(error)) => last_transport = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(ManagedError::Transport(last_transport.ok_or(
            ManagedError::InvalidResponse("submission retry lost its transport error"),
        )?))
    }

    /// Reads the current durable state of one managed turn.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation, transport, HTTP, size, or
    /// response-schema failure.
    pub async fn turn_state(
        &self,
        agent_id: &str,
        turn_id: &str,
    ) -> Result<TurnView, ManagedError> {
        validate_id("agent", agent_id)?;
        validate_id("turn", turn_id)?;
        self.json(
            Method::GET,
            &format!("{}/turns/{turn_id}", agent_path(agent_id)),
            None,
            None,
        )
        .await
    }

    /// Adds input to an active managed turn.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation, transport, HTTP, size, or
    /// response-schema failure.
    pub async fn steer(
        &self,
        agent_id: &str,
        turn_id: &str,
        input: &PromptInput,
    ) -> Result<TurnAction, ManagedError> {
        self.turn_action(agent_id, turn_id, "steer", Some(input))
            .await
    }

    /// Requests cancellation of an active managed turn.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation, transport, HTTP, size, or
    /// response-schema failure.
    pub async fn cancel(&self, agent_id: &str, turn_id: &str) -> Result<TurnAction, ManagedError> {
        self.turn_action(agent_id, turn_id, "cancel", None).await
    }

    /// Opens a resumable durable event stream starting at a validated cursor.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation failure before any network work.
    pub fn events(
        &self,
        agent_id: &str,
        cursor: EventCursor,
    ) -> Result<ManagedEventStream, ManagedError> {
        validate_id("agent", agent_id)?;
        Ok(ManagedEventStream::new(
            self.clone(),
            agent_id.to_owned(),
            cursor,
        ))
    }

    async fn turn_action(
        &self,
        agent_id: &str,
        turn_id: &str,
        action: &str,
        input: Option<&PromptInput>,
    ) -> Result<TurnAction, ManagedError> {
        validate_id("agent", agent_id)?;
        validate_id("turn", turn_id)?;
        let body = input
            .map(|input| serde_json::to_vec(&TurnSteer { input }))
            .transpose()
            .map_err(|_| ManagedError::InvalidResponse("failed to encode steer"))?;
        self.json(
            Method::POST,
            &format!("{}/turns/{turn_id}/{action}", agent_path(agent_id)),
            body.as_deref(),
            None,
        )
        .await
    }

    #[cfg(feature = "tools")]
    /// Resolves the authenticated reverse-tool endpoint for one owned agent.
    ///
    /// The returned target redacts its bearer credential from debug output and
    /// can be passed directly to [`nanocodex_tools::Tools::attach`].
    ///
    /// # Errors
    ///
    /// Rejects malformed agent identifiers or an origin that cannot form a
    /// WebSocket endpoint.
    #[cfg_attr(docsrs, doc(cfg(feature = "tools")))]
    pub fn attachment_target(
        &self,
        agent_id: &str,
    ) -> Result<nanocodex_tools::attachment::AttachmentTarget, ManagedError> {
        validate_id("agent", agent_id)?;
        let mut endpoint = self.base_url.clone();
        endpoint
            .set_scheme(if endpoint.scheme() == "https" {
                "wss"
            } else {
                "ws"
            })
            .map_err(|_| {
                ManagedError::Configuration("invalid managed attachment URL".to_owned())
            })?;
        endpoint.set_path(&format!("/v1/agents/{agent_id}/tool-host"));
        nanocodex_tools::attachment::AttachmentTarget::new(
            endpoint.as_str(),
            self.bearer.to_string(),
        )
        .map_err(|error| ManagedError::Configuration(error.to_string()))
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&[u8]>,
        idempotency_key: Option<&str>,
    ) -> Result<T, ManagedError> {
        let response = self.request(method, path, body, idempotency_key).await?;
        decode_response(response).await
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&[u8]>,
        idempotency_key: Option<&str>,
    ) -> Result<Response, ManagedError> {
        let mut request = self.http.request(method, self.url(path)?);
        if let Some(body) = body {
            request = request
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_vec());
        }
        if let Some(key) = idempotency_key {
            request = request.header("idempotency-key", key);
        }
        request.send().await.map_err(ManagedError::Transport)
    }

    pub(crate) fn url(&self, path: &str) -> Result<Url, ManagedError> {
        self.base_url
            .join(path)
            .map_err(|_| ManagedError::InvalidResponse("invalid managed route"))
    }
}

fn install_default_rustls_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

fn validate_origin(origin: &Url) -> Result<(), ManagedError> {
    if !matches!(origin.scheme(), "http" | "https")
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.query().is_some()
        || origin.fragment().is_some()
        || !matches!(origin.path(), "" | "/")
        || origin.host().is_none()
    {
        return Err(ManagedError::Configuration(
            "managed URL must be an HTTP(S) origin".to_owned(),
        ));
    }
    if origin.scheme() == "http" && !is_literal_loopback(origin) {
        return Err(ManagedError::Configuration(
            "managed URL requires HTTPS unless its host is literal loopback".to_owned(),
        ));
    }
    Ok(())
}

fn is_literal_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

pub(crate) async fn decode_response<T: DeserializeOwned>(
    response: Response,
) -> Result<T, ManagedError> {
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    let bytes = response_body(response).await?;
    serde_json::from_slice(&bytes).map_err(|_| ManagedError::InvalidResponse("invalid JSON"))
}

pub(crate) async fn response_error(response: Response) -> ManagedError {
    let status = response.status();
    let body = response_body(response).await.ok();
    let parsed = body
        .as_deref()
        .and_then(|body| serde_json::from_slice::<ErrorBody>(body).ok());
    ManagedError::Http {
        status,
        code: parsed
            .as_ref()
            .and_then(|body| body.error.clone())
            .unwrap_or_else(|| format!("http_{}", status.as_u16())),
        message: parsed
            .and_then(|body| body.message)
            .unwrap_or_else(|| format!("managed request failed ({})", status.as_u16())),
    }
}

async fn response_body(response: Response) -> Result<Vec<u8>, ManagedError> {
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(ManagedError::Transport)
}

#[derive(Deserialize)]
struct ErrorBody {
    error: Option<String>,
    message: Option<String>,
}

pub(crate) fn validate_id(kind: &str, value: &str) -> Result<(), ManagedError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(ManagedError::Configuration(format!(
            "managed {kind} id must be 1-128 safe ASCII characters"
        )));
    }
    Ok(())
}

pub(crate) fn validate_idempotency_key(value: &str) -> Result<(), ManagedError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(ManagedError::Configuration(
            "managed idempotency key must be 1-128 visible ASCII characters".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn agent_path(agent_id: &str) -> String {
    format!("v1/agents/{agent_id}")
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::Body,
        http::{Response, StatusCode},
        routing::get,
    };
    use tokio::io::AsyncReadExt;

    use super::{ManagedClient, decode_response, install_default_rustls_crypto_provider};
    use crate::{ManagedApiKey, ManagedError, PromptInput};

    fn key() -> String {
        format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))
    }

    #[test]
    fn plaintext_managed_origins_require_literal_loopback_hosts() {
        for origin in [
            "http://localhost",
            "http://127.0.0.1",
            "http://127.255.255.254",
            "http://[::1]",
        ] {
            assert!(
                ManagedClient::new(origin, ManagedApiKey::parse(key()).unwrap()).is_ok(),
                "{origin}"
            );
        }

        for origin in [
            "http://example.com",
            "http://localhost.example",
            "http://[::ffff:127.0.0.1]",
            "http://192.168.1.10",
        ] {
            assert!(
                ManagedClient::new(origin, ManagedApiKey::parse(key()).unwrap()).is_err(),
                "{origin}"
            );
        }

        assert!(
            ManagedClient::new("https://example.com", ManagedApiKey::parse(key()).unwrap(),)
                .is_ok()
        );
    }

    #[test]
    fn rejects_non_origin_urls_and_redacts_clients() {
        for origin in [
            "ftp://example.com",
            "https://user@example.com",
            "https://example.com/path",
            "https://example.com?query",
            "https://example.com#fragment",
        ] {
            assert!(
                ManagedClient::new(origin, ManagedApiKey::parse(key()).unwrap()).is_err(),
                "{origin}"
            );
        }

        let secret = key();
        let client = ManagedClient::new(
            "https://example.com",
            ManagedApiKey::parse(secret.clone()).unwrap(),
        )
        .unwrap();
        assert!(!format!("{client:?}").contains(&secret));
    }

    #[tokio::test]
    async fn ordinary_responses_have_no_client_byte_limit() {
        install_default_rustls_crypto_provider();
        let payload = "x".repeat(1024 * 1024 + 1);
        let encoded = serde_json::to_string(&serde_json::json!({ "payload": payload })).unwrap();
        let app = Router::new().route(
            "/large",
            get(move || {
                let encoded = encoded.clone();
                async move {
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Body::from(encoded))
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = reqwest::get(format!("http://{address}/large"))
            .await
            .unwrap();
        let decoded: serde_json::Value = decode_response(response).await.unwrap();
        assert_eq!(decoded["payload"].as_str().unwrap().len(), 1024 * 1024 + 1);
        server.abort();
    }

    #[tokio::test]
    async fn redirects_are_returned_instead_of_followed() {
        let app = Router::new()
            .route(
                "/v1/agents",
                get(|| async {
                    Response::builder()
                        .status(StatusCode::TEMPORARY_REDIRECT)
                        .header("location", "/redirect-target")
                        .body(Body::empty())
                        .unwrap()
                }),
            )
            .route(
                "/redirect-target",
                get(|| async {
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Body::from(r#"{"data":[]}"#))
                        .unwrap()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(key()).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            client.list().await,
            Err(ManagedError::Http {
                status: StatusCode::TEMPORARY_REDIRECT,
                ..
            })
        ));
        server.abort();
    }

    #[tokio::test]
    async fn submit_makes_exactly_three_byte_identical_transport_attempts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let (header_end, content_length) = loop {
                    let mut chunk = [0_u8; 4096];
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(read, 0);
                    request.extend_from_slice(&chunk[..read]);
                    if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let header_end = index + 4;
                        let headers = std::str::from_utf8(&request[..index]).unwrap();
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        break (header_end, content_length);
                    }
                };
                while request.len() < header_end + content_length {
                    let mut chunk = [0_u8; 4096];
                    let read = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(read, 0);
                    request.extend_from_slice(&chunk[..read]);
                }
                bodies.push(request[header_end..header_end + content_length].to_vec());
                drop(stream);
            }
            bodies
        });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(key()).unwrap(),
        )
        .unwrap();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            client.submit(
                "agent-1",
                Some("turn-1"),
                "stable-attempt",
                &PromptInput::Text("byte-identical".to_owned()),
            ),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(ManagedError::Transport(_))));
        let bodies = server.await.unwrap();
        assert_eq!(bodies.len(), 3);
        assert!(bodies.windows(2).all(|pair| pair[0] == pair[1]));
    }
}
