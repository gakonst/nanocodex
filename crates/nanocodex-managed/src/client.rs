use std::{
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

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
    AutoRoutingStatus, EventCursor, EventHistoryPage, FindSessionsRequest, FindSessionsResponse,
    ManagedApiKey, ManagedError, ManagedEventStream, MemoryKey, MemoryListResponse, MemoryRecord,
    PromptInput, ReadSessionBody, ReadSessionRequest, ReadSessionResponse, RoutingStatus,
    SteerReceipt, SteerReceiptState, SteerWithdrawal, TurnAction, TurnSteer, TurnSubmission,
    TurnView,
};

const MAX_HISTORY_PAGE: u16 = 256;
const SUBMIT_ATTEMPTS: usize = 3;
const READ_ATTEMPTS: usize = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

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
/// policy. Reads retry transient failures, turn submission retries with its
/// durable identity, and steering retries only an explicit recovery rejection
/// that guarantees no input was delivered.
#[derive(Clone)]
pub struct ManagedClient {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: Url,
    pub(crate) bearer: Arc<str>,
    pub(crate) request_origin: Option<HeaderValue>,
    access: Arc<Mutex<Option<ManagedAccess>>>,
}

struct ManagedAccess {
    token: HeaderValue,
    until: Instant,
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

    /// Adds descriptive client and logical Hand context to HTTP and WebSocket requests.
    /// This metadata never grants authority or changes command placement.
    ///
    /// # Errors
    /// Returns a configuration error for oversized or invalid header data.
    pub fn with_request_origin(
        mut self,
        client: &str,
        hand: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<Self, ManagedError> {
        if client.is_empty()
            || client.len() > 128
            || !client
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
            || hand.is_some_and(|value| {
                value.is_empty()
                    || value.len() > 128
                    || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
            })
            || cwd.is_some_and(|value| {
                value.len() > 512
                    || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
                    || !value.starts_with('/')
                    || value.contains('\\')
                    || value.split('/').any(|part| part == "." || part == "..")
            })
        {
            return Err(ManagedError::Configuration(
                "invalid request origin".to_owned(),
            ));
        }
        let mut context = serde_json::json!({ "client": client });
        if let Some(hand) = hand {
            context["hand"] = hand.into();
        }
        if let Some(cwd) = cwd {
            context["cwd"] = cwd.into();
        }
        self.request_origin = Some(HeaderValue::from_str(&context.to_string()).map_err(|_| {
            ManagedError::Configuration("invalid request origin header".to_owned())
        })?);
        Ok(self)
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
            .connect_timeout(Duration::from_secs(10))
            // SSE sends keepalives every 15 seconds. Bound a dead connection
            // without limiting the lifetime of a healthy event stream.
            .read_timeout(Duration::from_secs(45))
            .build()
            .map_err(ManagedError::Transport)?;
        drop(builder.api_key);

        Ok(Self {
            http,
            base_url: builder.origin,
            bearer: api_bearer,
            request_origin: None,
            access: Arc::new(Mutex::new(None)),
        })
    }

    /// Creates a new account-owned managed agent.
    ///
    /// # Errors
    ///
    /// Returns a transport, HTTP, size, or response-schema failure.
    pub async fn create(&self) -> Result<AgentReceipt, ManagedError> {
        let receipt = self.json(Method::POST, "v1/agents", None, None).await?;
        validate_agent_receipt(receipt)
    }

    /// Creates an agent with its initial model and reasoning policy.
    ///
    /// # Errors
    ///
    /// Returns a settings-validation, transport, HTTP, or response-schema failure.
    pub async fn create_with_settings(
        &self,
        settings: AgentSettings,
    ) -> Result<AgentReceipt, ManagedError> {
        let settings = settings.validate()?;
        let body = serde_json::to_vec(&serde_json::json!({ "settings": settings }))
            .map_err(|_| ManagedError::InvalidResponse("failed to encode agent settings"))?;
        let receipt = self
            .json(Method::POST, "v1/agents", Some(&body), None)
            .await?;
        validate_agent_receipt(receipt)
    }

    /// Creates an agent pinned to one connected ChatGPT account.
    ///
    /// The pin is retained for the session and disables automatic account failover.
    ///
    /// # Errors
    ///
    /// Returns an identifier/settings-validation, transport, HTTP, or response-schema failure.
    pub async fn create_with_chatgpt_account(
        &self,
        settings: AgentSettings,
        account_id: &str,
    ) -> Result<AgentReceipt, ManagedError> {
        if account_id.is_empty()
            || account_id.len() > 256
            || !account_id.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(ManagedError::InvalidResponse("invalid ChatGPT account ID"));
        }
        let settings = settings.validate()?;
        let body = serde_json::to_vec(&serde_json::json!({
            "settings": settings,
            "configuration": { "chatgpt_account_id": account_id },
        }))
        .map_err(|_| ManagedError::InvalidResponse("failed to encode agent configuration"))?;
        let receipt = self
            .json(Method::POST, "v1/agents", Some(&body), None)
            .await?;
        validate_agent_receipt(receipt)
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
        if !state.settings.is_valid() {
            return Err(ManagedError::InvalidResponse(
                "agent state contains incompatible model and reasoning settings",
            ));
        }
        crate::sse::validate_numeric_cursor(&state.latest_event_cursor).map_err(|_| {
            ManagedError::InvalidResponse("agent state latest event cursor is invalid")
        })?;
        Ok(state)
    }

    /// Enables automatic routing on an empty managed session before its first message.
    /// Repeating a successful opt-in leaves the retained route unchanged.
    ///
    /// # Errors
    ///
    /// Returns validation, transport, HTTP, or response-schema failures, including
    /// rejection when routing is unavailable or the session already has history.
    pub async fn enable_auto_routing(
        &self,
        agent_id: &str,
    ) -> Result<AutoRoutingStatus, ManagedError> {
        validate_id("agent", agent_id)?;
        let status: AutoRoutingStatus = self
            .json(
                Method::POST,
                &format!("{}/routing", agent_path(agent_id)),
                None,
                None,
            )
            .await?;
        if !status.enabled || !status.settings.is_valid() {
            return Err(ManagedError::InvalidResponse(
                "invalid automatic routing receipt",
            ));
        }
        Ok(status)
    }

    /// Reads the actual retained provider/model without altering the thread.
    ///
    /// # Errors
    /// Returns validation, transport, HTTP, or malformed route failures.
    pub async fn routing_status(&self, agent_id: &str) -> Result<RoutingStatus, ManagedError> {
        validate_id("agent", agent_id)?;
        let mut status: RoutingStatus = self
            .json(Method::GET, &agent_path(agent_id), None, None)
            .await?;
        if let Some(route) = &status.route {
            if !route.model.supports_thinking(route.thinking) {
                return Err(ManagedError::InvalidResponse(
                    "invalid routed thinking effort",
                ));
            }
            status.enabled = true;
        }
        Ok(status)
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
        self.patch_settings(agent_id, AgentSettingsPatch::from(settings.validate()?))
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

    /// Enables or disables model-specific fast processing for subsequent turns.
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
        if !response.settings.is_valid() {
            return Err(ManagedError::InvalidResponse(
                "settings response contains incompatible model and reasoning settings",
            ));
        }
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

    /// Lists an agent's durable cron schedules.
    ///
    /// # Errors
    /// Returns an identifier, transport, HTTP, or response-schema failure.
    pub async fn triggers(&self, agent_id: &str) -> Result<crate::CronTriggerList, ManagedError> {
        validate_id("agent", agent_id)?;
        self.json(
            Method::GET,
            &format!("{}/triggers", agent_path(agent_id)),
            None,
            None,
        )
        .await
    }

    /// Reads one durable cron schedule.
    ///
    /// # Errors
    /// Returns an identifier, transport, HTTP, or response-schema failure.
    pub async fn trigger(
        &self,
        agent_id: &str,
        trigger_id: &str,
    ) -> Result<crate::CronTrigger, ManagedError> {
        self.json(
            Method::GET,
            &trigger_path(agent_id, trigger_id)?,
            None,
            None,
        )
        .await
    }

    /// Creates or replaces one named cron schedule with an idempotent PUT.
    ///
    /// # Errors
    /// Returns a configuration, transport, HTTP, or response-schema failure.
    pub async fn put_trigger(
        &self,
        agent_id: &str,
        trigger_id: &str,
        config: &crate::CronTriggerConfig,
    ) -> Result<crate::CronTrigger, ManagedError> {
        let path = trigger_path(agent_id, trigger_id)?;
        config.validate()?;
        let body = serde_json::to_vec(config)
            .map_err(|_| ManagedError::InvalidResponse("failed to encode cron trigger"))?;
        self.json(Method::PUT, &path, Some(&body), None).await
    }

    /// Deletes a durable cron schedule.
    ///
    /// # Errors
    /// Returns an identifier, transport, or HTTP failure.
    pub async fn delete_trigger(
        &self,
        agent_id: &str,
        trigger_id: &str,
    ) -> Result<(), ManagedError> {
        let response = self
            .request(
                Method::DELETE,
                &trigger_path(agent_id, trigger_id)?,
                None,
                None,
            )
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
            .timeout(REQUEST_TIMEOUT)
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
        let page: EventHistoryPage = self.read_json(url).await?;
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
        self.steer_identified(agent_id, turn_id, input, None).await
    }

    /// Adds input with a caller-selected identity for durable receipt recovery and withdrawal.
    ///
    /// # Errors
    /// Returns a validation, transport, HTTP, or response-schema failure.
    pub async fn steer_with_id(
        &self,
        agent_id: &str,
        turn_id: &str,
        message_id: &str,
        input: &PromptInput,
    ) -> Result<TurnAction, ManagedError> {
        validate_id("message", message_id)?;
        self.steer_identified(agent_id, turn_id, input, Some(message_id))
            .await
    }

    async fn steer_identified(
        &self,
        agent_id: &str,
        turn_id: &str,
        input: &PromptInput,
        message_id: Option<&str>,
    ) -> Result<TurnAction, ManagedError> {
        loop {
            let result = self
                .turn_action(agent_id, turn_id, "steer", Some(input), message_id)
                .await;
            if matches!(&result, Err(ManagedError::Http { status, code, .. })
                if *status == reqwest::StatusCode::SERVICE_UNAVAILABLE && code == "turn_recovering")
            {
                // The service returns this only before calling turn.steer.
                // A transport failure is ambiguous and must never be retried
                // against an older server. Ambiguous outcomes are reconciled by reads.
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            if result.as_ref().is_err_and(|error| !matches!(error, ManagedError::Http { status, .. } if status.is_client_error()))
                && let Some(message_id) = message_id
                && let Ok(receipt) = self.steer_receipt(agent_id, turn_id, message_id).await
                && receipt.state == SteerReceiptState::Accepted
                && receipt.matches_input(input)
            {
                return Ok(TurnAction { turn_id: turn_id.to_owned(), state: "steering".to_owned() });
            }
            return result;
        }
    }

    /// Reads a durable receipt without repeating the steering POST.
    /// Older servers reject this endpoint; callers must preserve an unknown outcome.
    ///
    /// # Errors
    /// Returns validation, transport, HTTP, or correlation errors.
    pub async fn steer_receipt(
        &self,
        agent_id: &str,
        turn_id: &str,
        message_id: &str,
    ) -> Result<SteerReceipt, ManagedError> {
        validate_id("agent", agent_id)?;
        validate_id("turn", turn_id)?;
        validate_id("message", message_id)?;
        let mut url = self.url(&format!(
            "{}/turns/{turn_id}/steer-receipt",
            agent_path(agent_id)
        ))?;
        url.query_pairs_mut().append_pair("message_id", message_id);
        let receipt: SteerReceipt = self.read_json(url).await?;
        if receipt.protocol != 1 || receipt.turn_id != turn_id || receipt.message_id != message_id {
            return Err(ManagedError::Configuration(
                "managed steer receipt did not match the request".to_owned(),
            ));
        }
        Ok(receipt)
    }

    /// Requests cancellation of an active managed turn.
    ///
    /// # Errors
    ///
    /// Returns an identifier-validation, transport, HTTP, size, or
    /// response-schema failure.
    pub async fn cancel(&self, agent_id: &str, turn_id: &str) -> Result<TurnAction, ManagedError> {
        self.turn_action(agent_id, turn_id, "cancel", None, None)
            .await
    }

    /// Atomically withdraws an identified steer if it is still pending.
    ///
    /// # Errors
    /// Returns a validation, transport, HTTP, or response-schema failure.
    pub async fn withdraw_steer(
        &self,
        agent_id: &str,
        turn_id: &str,
        message_id: &str,
    ) -> Result<SteerWithdrawal, ManagedError> {
        validate_id("agent", agent_id)?;
        validate_id("turn", turn_id)?;
        validate_id("message", message_id)?;
        let body = serde_json::to_vec(&serde_json::json!({"message_id": message_id}))
            .map_err(|_| ManagedError::InvalidResponse("failed to encode withdrawal"))?;
        let receipt: SteerWithdrawal = self
            .json(
                Method::POST,
                &format!("{}/turns/{turn_id}/withdraw-steer", agent_path(agent_id)),
                Some(&body),
                None,
            )
            .await?;
        if receipt.turn_id != turn_id || receipt.message_id != message_id {
            return Err(ManagedError::InvalidResponse(
                "withdrawal acknowledged a different steer",
            ));
        }
        Ok(receipt)
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
        message_id: Option<&str>,
    ) -> Result<TurnAction, ManagedError> {
        validate_id("agent", agent_id)?;
        validate_id("turn", turn_id)?;
        let body = input
            .map(|input| serde_json::to_vec(&TurnSteer { input, message_id }))
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
        self.attachment_target_at(&format!("/v1/agents/{agent_id}/tool-host"))
    }

    #[cfg(feature = "tools")]
    /// Resolves the authenticated reverse-tool endpoint for the account.
    ///
    /// The returned target redacts its bearer credential from debug output and
    /// can be passed directly to [`nanocodex_tools::Tools::attach`].
    ///
    /// # Errors
    ///
    /// Rejects an origin that cannot form a WebSocket endpoint.
    #[cfg_attr(docsrs, doc(cfg(feature = "tools")))]
    pub fn account_attachment_target(
        &self,
    ) -> Result<nanocodex_tools::attachment::AttachmentTarget, ManagedError> {
        self.attachment_target_at("/v1/account/tool-host")
    }

    #[cfg(feature = "tools")]
    fn attachment_target_at(
        &self,
        path: &str,
    ) -> Result<nanocodex_tools::attachment::AttachmentTarget, ManagedError> {
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
        endpoint.set_path(path);
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
        if method == Method::GET && body.is_none() && idempotency_key.is_none() {
            return self.read_json(self.url(path)?).await;
        }
        let response = self.request(method, path, body, idempotency_key).await?;
        decode_response(response).await
    }

    async fn read_json<T: DeserializeOwned>(&self, url: Url) -> Result<T, ManagedError> {
        for attempt in 0..READ_ATTEMPTS {
            let result = match self
                .send_with_access(self.http.get(url.clone()).timeout(REQUEST_TIMEOUT), &url)
                .await
            {
                Ok(response) => decode_response(response).await,
                Err(error) => Err(ManagedError::Transport(error)),
            };
            let retry = match &result {
                Err(ManagedError::Transport(_)) => true,
                Err(ManagedError::Http { status, .. }) => {
                    status.is_server_error() || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
                }
                _ => false,
            };
            if !retry || attempt + 1 == READ_ATTEMPTS {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(250 << attempt)).await;
        }
        unreachable!("the last read attempt always returns")
    }

    pub(crate) fn prepare_active_conversation(&self, agent_id: &str) {
        let client = self.clone();
        let path = format!("{}/prepare", agent_path(agent_id));
        // Bound the best-effort activation request; submission never joins it.
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                client.request(Method::POST, &path, None, None),
            )
            .await;
        });
    }

    /// Downloads an agent's logical absolute file path into a new local file.
    ///
    /// Streams bytes without buffering the whole file. The destination is
    /// published only after completion and is never overwritten. Failed or
    /// cancelled downloads remove their temporary file.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid paths or identifiers, HTTP/transport
    /// failures, or local filesystem failures (including an existing target).
    pub async fn download_file(
        &self,
        agent_id: &str,
        path: &str,
        destination: &std::path::Path,
    ) -> Result<(), ManagedError> {
        use tokio::io::AsyncWriteExt;

        validate_id("agent", agent_id)?;
        if !path.starts_with('/') || path.contains('\0') {
            return Err(ManagedError::Configuration(
                "managed file path must be a logical absolute path without NUL".to_owned(),
            ));
        }
        let mut url = self.url(&format!("{}/files", agent_path(agent_id)))?;
        url.query_pairs_mut().append_pair("path", path);
        let request = self.http.get(url.clone()).timeout(DOWNLOAD_TIMEOUT);
        let mut response = self
            .send_with_access(request, &url)
            .await
            .map_err(ManagedError::Transport)?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let temporary = tempfile::NamedTempFile::new_in(parent)?.into_temp_path();
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&temporary)
            .await?;
        while let Some(chunk) = response.chunk().await.map_err(ManagedError::Transport)? {
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        drop(file);
        temporary
            .persist_noclobber(destination)
            .map_err(|error| error.error)?;
        Ok(())
    }

    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&[u8]>,
        idempotency_key: Option<&str>,
    ) -> Result<Response, ManagedError> {
        let url = self.url(path)?;
        let mut request = self
            .http
            .request(method, url.clone())
            .timeout(REQUEST_TIMEOUT);
        if let Some(body) = body {
            request = request
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_vec());
        }
        if let Some(key) = idempotency_key {
            request = request.header("idempotency-key", key);
        }
        self.send_with_access(request, &url)
            .await
            .map_err(ManagedError::Transport)
    }

    pub(crate) async fn send_with_access(
        &self,
        mut request: reqwest::RequestBuilder,
        url: &Url,
    ) -> Result<Response, reqwest::Error> {
        if let Some(origin) = &self.request_origin {
            request = request.header("x-nanocodex-client-context", origin.clone());
        }
        let eligible = (url.path() == "/v1/agents" || url.path().starts_with("/v1/agents/"))
            && !["ws", "events", "tool-host", "device-host", "sideband"]
                .contains(&url.path().rsplit('/').next().unwrap_or_default());
        let began = Instant::now();
        let token = if eligible {
            self.access
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .filter(|entry| entry.until > began + Duration::from_secs(5))
                .map(|entry| entry.token.clone())
        } else {
            None
        };
        let retry = request.try_clone();
        let mut response = match &token {
            Some(token) => {
                request
                    .header("x-nanocodex-access", token.clone())
                    .send()
                    .await?
            }
            None => request.send().await?,
        };
        if let Some(token) = &token
            && response.status() == reqwest::StatusCode::UNAUTHORIZED
            && response
                .headers()
                .get("x-nanocodex-access-rejected")
                .is_some_and(|value| value == "1")
        {
            {
                let mut cache = self
                    .access
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if cache.as_ref().is_some_and(|entry| entry.token == *token) {
                    *cache = None;
                }
            }
            if let Some(retry) = retry {
                response = retry.send().await?;
            }
        }
        if eligible && response.status().is_success() {
            let token = response.headers().get("x-nanocodex-access");
            let ttl = response
                .headers()
                .get("x-nanocodex-access-ttl-ms")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            if let (Some(token), Some(ttl @ 1..=120_000)) = (token, ttl)
                && token.as_bytes().starts_with(b"ncx_access_v1.")
                && token.as_bytes().len() <= 16_384
            {
                let until = began + Duration::from_millis(ttl);
                let mut cache = self
                    .access
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if cache.as_ref().is_none_or(|entry| entry.until < until) {
                    let mut token = token.clone();
                    token.set_sensitive(true);
                    *cache = Some(ManagedAccess { token, until });
                }
            }
        }
        Ok(response)
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

pub(crate) fn validate_origin(origin: &Url) -> Result<(), ManagedError> {
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

fn trigger_path(agent_id: &str, trigger_id: &str) -> Result<String, ManagedError> {
    validate_id("agent", agent_id)?;
    if trigger_id.is_empty()
        || trigger_id.len() > 64
        || !trigger_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(ManagedError::Configuration(
            "trigger id must be 1-64 letters, digits, underscores or hyphens".to_owned(),
        ));
    }
    Ok(format!("{}/triggers/{trigger_id}", agent_path(agent_id)))
}

pub(crate) fn agent_path(agent_id: &str) -> String {
    format!("v1/agents/{agent_id}")
}

fn validate_agent_receipt(receipt: AgentReceipt) -> Result<AgentReceipt, ManagedError> {
    if receipt
        .initial_state
        .as_ref()
        .is_some_and(|state| !state.settings.is_valid())
    {
        return Err(ManagedError::InvalidResponse(
            "created agent state contains incompatible model and reasoning settings",
        ));
    }
    Ok(receipt)
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

    #[tokio::test]
    async fn download_file_encodes_path_and_preserves_bytes_without_overwriting() {
        use axum::{
            extract::Query,
            http::{HeaderMap, Uri},
        };
        let path = "/brain/outputs/a #?%&+ ü.bin";
        let payload: Vec<u8> = (0..=255).cycle().take(1024 * 1024 + 19).collect();
        let served = payload.clone();
        let app = Router::new().route("/v1/agents/agent-1/files", get(
            move |headers: HeaderMap, uri: Uri, Query(query): Query<std::collections::HashMap<String, String>>| {
                let served = served.clone();
                async move {
                    assert_eq!(headers["authorization"], format!("Bearer {}", key()));
                    assert_eq!(query.len(), 1);
                    assert_eq!(query["path"], path);
                    assert!(uri.query().unwrap().contains("%23%3F%25%26%2B"));
                    Body::from(served)
                }
            }
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(key()).unwrap(),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("download.bin");
        client
            .download_file("agent-1", path, &destination)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), payload);
        assert!(matches!(
            client.download_file("agent-1", path, &destination).await,
            Err(ManagedError::Io(_))
        ));
        assert_eq!(std::fs::read(&destination).unwrap(), payload);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn download_file_returns_http_errors_and_rejects_invalid_inputs() {
        let app = Router::new().route(
            "/v1/agents/agent-1/files",
            get(|| async {
                (
                    StatusCode::NOT_FOUND,
                    axum::Json(
                        serde_json::json!({"error":"file_not_found", "message":"missing file"}),
                    ),
                )
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
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("download.bin");
        assert!(
            matches!(client.download_file("agent-1", "/brain/missing", &destination).await,
            Err(ManagedError::Http { status: StatusCode::NOT_FOUND, code, message }) if code == "file_not_found" && message == "missing file")
        );
        for (agent, path) in [
            ("bad/id", "/brain/file"),
            ("agent-1", "relative"),
            ("agent-1", "/bad\0path"),
        ] {
            assert!(matches!(
                client.download_file(agent, path, &destination).await,
                Err(ManagedError::Configuration(_))
            ));
        }
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        server.abort();
    }

    #[tokio::test]
    async fn download_file_cleans_up_incomplete_body() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            // The fixture only needs a request to arrive before it sends the
            // deliberately truncated response; EOF is not a valid request.
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort",
                )
                .await
                .unwrap();
        });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(key()).unwrap(),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            client
                .download_file(
                    "agent-1",
                    "/brain/file",
                    &directory.path().join("download.bin")
                )
                .await,
            Err(ManagedError::Transport(_))
        ));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn request_origin_is_descriptive_and_sent_over_http() {
        use axum::http::{HeaderMap, header::AUTHORIZATION};
        let expected =
            serde_json::json!({ "client": "nanocodex2", "hand": "user:laptop", "cwd": "/laptop" });
        let app = Router::new().route(
            "/v1/agents",
            get(move |headers: HeaderMap| async move {
                assert!(headers.contains_key(AUTHORIZATION));
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(
                        headers["x-nanocodex-client-context"].as_bytes()
                    )
                    .unwrap(),
                    expected
                );
                axum::Json(serde_json::json!({ "data": [] }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let key = ManagedApiKey::parse(format!("ncx_live_{}_{}", "k".repeat(12), "s".repeat(43)))
            .unwrap();
        let client = ManagedClient::new(format!("http://{address}"), key)
            .unwrap()
            .with_request_origin("nanocodex2", Some("user:laptop"), Some("/laptop"))
            .unwrap();
        assert!(client.list().await.unwrap().data.is_empty());
        assert!(
            client
                .clone()
                .with_request_origin("bad\nname", None, None)
                .is_err()
        );
        assert!(
            client
                .with_request_origin("cli", None, Some("/laptop/../other"))
                .is_err()
        );
        server.abort();
    }

    #[tokio::test]
    async fn access_reuse_preserves_operation_and_recovers_rejection_once() {
        use axum::http::HeaderMap;
        use std::sync::{Arc, Mutex};
        let observations = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&observations);
        let app = Router::new().route(
            "/v1/agents",
            axum::routing::post(move |headers: HeaderMap, body: String| {
                let seen = Arc::clone(&seen);
                async move {
                    let mut seen = seen.lock().unwrap();
                    seen.push((headers, body));
                    match seen.len() {
                        1 => Response::builder()
                            .header("content-type", "application/json")
                            .header("x-nanocodex-access", "ncx_access_v1.fixture.signature")
                            .header("x-nanocodex-access-ttl-ms", "120000")
                            .body(Body::from("{}"))
                            .unwrap(),
                        2 => Response::builder()
                            .status(401)
                            .header("x-nanocodex-access-rejected", "1")
                            .body(Body::empty())
                            .unwrap(),
                        _ => Response::builder()
                            .header("content-type", "application/json")
                            .body(Body::from("{}"))
                            .unwrap(),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(key()).unwrap(),
        )
        .unwrap();
        client
            .request(reqwest::Method::POST, "v1/agents", Some(b"{}"), None)
            .await
            .unwrap();
        client
            .clone()
            .request(
                reqwest::Method::POST,
                "v1/agents",
                Some(b"{\"input\":\"hello\"}"),
                Some("same-operation"),
            )
            .await
            .unwrap();
        server.abort();
        let seen = observations.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(
            seen[1].0["x-nanocodex-access"],
            "ncx_access_v1.fixture.signature"
        );
        assert!(!seen[2].0.contains_key("x-nanocodex-access"));
        assert_eq!(seen[1].0["idempotency-key"], seen[2].0["idempotency-key"]);
        assert_eq!(seen[1].1, seen[2].1);
        assert_eq!(seen[1].0["authorization"], seen[2].0["authorization"]);
    }

    #[tokio::test]
    async fn history_retries_transient_failures_with_the_same_page_cursor() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        for (status, expected_attempts) in [
            (StatusCode::SERVICE_UNAVAILABLE, 3),
            (StatusCode::TOO_MANY_REQUESTS, 3),
            (StatusCode::FORBIDDEN, 1),
        ] {
            let attempts = Arc::new(AtomicUsize::new(0));
            let observed = attempts.clone();
            let app = Router::new().route("/v1/agents/agent-1/events/history", get(move |uri: axum::http::Uri| {
                let observed = observed.clone();
                async move {
                    assert_eq!(uri.query(), Some("limit=256&before=500"));
                    if observed.fetch_add(1, Ordering::SeqCst) < 2 {
                        (status, axum::Json(serde_json::json!({"error": "fixture", "message": "temporarily unavailable"})))
                    } else {
                        (StatusCode::OK, axum::Json(serde_json::json!({"data": [], "has_more": false, "latest_cursor": "500"})))
                    }
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = ManagedClient::new(
                format!("http://{address}"),
                ManagedApiKey::parse(key()).unwrap(),
            )
            .unwrap();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                client.history("agent-1", Some("500"), 256),
            )
            .await
            .unwrap();
            assert_eq!(result.is_ok(), expected_attempts == 3);
            assert_eq!(attempts.load(Ordering::SeqCst), expected_attempts);
            server.abort();
        }
    }

    #[tokio::test]
    async fn identified_steers_and_withdrawals_preserve_identity() {
        let app = Router::new()
            .route("/v1/agents/agent-1/turns/turn-1/steer", axum::routing::post(|axum::Json(body): axum::Json<serde_json::Value>| async move {
                assert_eq!(body, serde_json::json!({"input": "correction", "message_id": "pending"}));
                axum::Json(serde_json::json!({"turn_id": "turn-1", "state": "steering"}))
            }))
            .route("/v1/agents/agent-1/turns/turn-1/withdraw-steer", axum::routing::post(|axum::Json(body): axum::Json<serde_json::Value>| async move {
                let id = body["message_id"].as_str().unwrap();
                axum::Json(serde_json::json!({"turn_id": "turn-1", "message_id": if id == "mismatch" { "other" } else { id }, "withdrawn": id == "pending"}))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(key()).unwrap(),
        )
        .unwrap();
        client
            .steer_with_id(
                "agent-1",
                "turn-1",
                "pending",
                &PromptInput::Text("correction".to_owned()),
            )
            .await
            .unwrap();
        assert!(
            client
                .withdraw_steer("agent-1", "turn-1", "pending")
                .await
                .unwrap()
                .withdrawn
        );
        assert!(
            !client
                .withdraw_steer("agent-1", "turn-1", "consumed")
                .await
                .unwrap()
                .withdrawn
        );
        assert!(matches!(
            client.withdraw_steer("agent-1", "turn-1", "mismatch").await,
            Err(ManagedError::InvalidResponse(_))
        ));
        assert!(
            client
                .withdraw_steer("agent-1", "turn-1", "../invalid")
                .await
                .is_err()
        );
        server.abort();
    }

    #[tokio::test]
    async fn lost_steer_ack_uses_only_correlated_read_receipts_and_never_repeats_post() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        for (post_status, receipt_status, receipt_id, receipt_state, receipt_input, confirmed) in [
            (
                StatusCode::BAD_GATEWAY,
                StatusCode::OK,
                "message",
                "accepted",
                Some("once"),
                true,
            ),
            (
                StatusCode::BAD_GATEWAY,
                StatusCode::OK,
                "other",
                "accepted",
                Some("once"),
                false,
            ),
            (
                StatusCode::BAD_GATEWAY,
                StatusCode::OK,
                "message",
                "unknown",
                Some("once"),
                false,
            ),
            (
                StatusCode::BAD_GATEWAY,
                StatusCode::OK,
                "message",
                "withdrawn",
                Some("once"),
                false,
            ),
            (
                StatusCode::BAD_GATEWAY,
                StatusCode::NOT_FOUND,
                "message",
                "unknown",
                Some("once"),
                false,
            ),
            (
                StatusCode::CONFLICT,
                StatusCode::OK,
                "message",
                "accepted",
                Some("once"),
                false,
            ),
            (
                StatusCode::BAD_GATEWAY,
                StatusCode::OK,
                "message",
                "accepted",
                Some("previous payload"),
                false,
            ),
            (
                StatusCode::BAD_GATEWAY,
                StatusCode::OK,
                "message",
                "accepted",
                None,
                false,
            ),
        ] {
            let posts = Arc::new(AtomicUsize::new(0));
            let count = posts.clone();
            let app = Router::new()
                .route("/v1/agents/agent-1/turns/completed/steer", axum::routing::post(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    async move { (post_status, axum::Json(serde_json::json!({"error": "fixture", "message": "lost reply or conflict"}))) }
                }))
                .route("/v1/agents/agent-1/turns/completed/steer-receipt", axum::routing::get(move |axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
                    assert_eq!(query.get("message_id").unwrap(), "message");
                    (receipt_status, axum::Json(serde_json::json!({"protocol":1,"turn_id":"completed","message_id":receipt_id,"state":receipt_state,"input_key":receipt_input.map(|text| { use sha2::{Digest as _, Sha256}; Sha256::digest(serde_json::to_vec(&serde_json::json!({"instruction":text})).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect::<String>() }),"terminal":true})))
                }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = ManagedClient::new(
                format!("http://{address}"),
                ManagedApiKey::parse(key()).unwrap(),
            )
            .unwrap();
            let result = client
                .steer_with_id(
                    "agent-1",
                    "completed",
                    "message",
                    &PromptInput::Text("once".into()),
                )
                .await;
            assert_eq!(result.is_ok(), confirmed);
            assert_eq!(posts.load(Ordering::SeqCst), 1);
            server.abort();
        }
    }

    #[tokio::test]
    async fn steering_retries_only_explicit_pre_delivery_recovery_rejections() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        for (status, code, expected_attempts) in [
            (StatusCode::SERVICE_UNAVAILABLE, "turn_recovering", 2),
            (StatusCode::SERVICE_UNAVAILABLE, "retryable", 1),
            (StatusCode::CONFLICT, "turn_not_steerable", 1),
        ] {
            let attempts = Arc::new(AtomicUsize::new(0));
            let observed = attempts.clone();
            let app = Router::new().route(
                "/v1/agents/agent-1/turns/turn-1/steer",
                axum::routing::post(move |body: axum::body::Bytes| {
                    let observed = observed.clone();
                    async move {
                        assert_eq!(
                            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                            serde_json::json!({"input": "once"})
                        );
                        if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                            (
                                status,
                                axum::Json(
                                    serde_json::json!({"error": code, "message": "fixture"}),
                                ),
                            )
                        } else {
                            (
                                StatusCode::ACCEPTED,
                                axum::Json(
                                    serde_json::json!({"turn_id": "turn-1", "state": "steering"}),
                                ),
                            )
                        }
                    }
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
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                client.steer("agent-1", "turn-1", &PromptInput::Text("once".to_owned())),
            )
            .await
            .unwrap();
            assert_eq!(result.is_ok(), expected_attempts == 2);
            assert_eq!(attempts.load(Ordering::SeqCst), expected_attempts);
            server.abort();
        }
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

    #[cfg(feature = "tools")]
    #[test]
    fn attachment_targets_preserve_scope_scheme_and_bearer_safety() {
        let secret = key();
        let client = ManagedClient::new(
            "https://example.com",
            ManagedApiKey::parse(secret.clone()).unwrap(),
        )
        .unwrap();

        let agent = client.attachment_target("agent-1").unwrap();
        assert_eq!(
            agent.endpoint().as_str(),
            "wss://example.com/v1/agents/agent-1/tool-host"
        );
        let account = client.account_attachment_target().unwrap();
        assert_eq!(
            account.endpoint().as_str(),
            "wss://example.com/v1/account/tool-host"
        );
        assert!(!format!("{agent:?}{account:?}").contains(&secret));

        let loopback = ManagedClient::new(
            "http://127.0.0.1:8787",
            ManagedApiKey::parse(key()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            loopback
                .account_attachment_target()
                .unwrap()
                .endpoint()
                .as_str(),
            "ws://127.0.0.1:8787/v1/account/tool-host"
        );
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
