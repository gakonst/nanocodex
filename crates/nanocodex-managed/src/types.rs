use std::collections::BTreeMap;

use nanocodex_oai_api::{Model, ReasoningMode, Thinking, events::AgentEvent};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, DeserializeOwned},
};
use serde_json::{Value, value::RawValue};

use crate::{ManagedError, client::validate_id};

/// User input accepted by a managed turn or live steer operation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PromptInput {
    /// A plain UTF-8 prompt.
    Text(String),
    /// Ordered multimodal prompt content.
    Content(Vec<PromptContent>),
}

/// One item in a multimodal managed prompt.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PromptContent {
    /// UTF-8 text content.
    Text {
        /// Complete text value.
        text: String,
    },
    /// Image content addressed by URL or data URL.
    Image {
        /// Image URL or data URL.
        image_url: String,
        /// Optional provider image-detail hint.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Audio content addressed by URL or data URL.
    Audio {
        /// Audio URL or data URL.
        audio_url: String,
    },
}

/// Receipt returned when an account-owned agent is created.
#[derive(Debug, Deserialize, Serialize)]
pub struct AgentReceipt {
    /// Stable managed agent identifier.
    pub agent_id: String,
    /// Stable managed session identifier.
    pub session_id: String,
    /// Durable event-stream endpoint.
    pub events_url: String,
    /// Managed live endpoint.
    pub websocket_url: String,
    /// Initial durable state when the service can return it atomically with creation.
    #[serde(default)]
    pub initial_state: Option<AgentState>,
}

/// Account-owned managed agents and their available summaries.
#[derive(Debug, Deserialize, Serialize)]
pub struct AgentList {
    /// Stable agent identifiers in service order.
    pub data: Vec<String>,
    /// Summaries keyed by stable agent identifier.
    #[serde(default)]
    pub summaries: BTreeMap<String, AgentSummary>,
}

/// Compact account-owned agent summary.
#[derive(Debug, Deserialize, Serialize)]
pub struct AgentSummary {
    /// Current session title.
    pub title: String,
    /// Creation timestamp supplied by the service.
    pub created_at: f64,
    /// Last-update timestamp supplied by the service.
    pub updated_at: f64,
    /// Number of accepted turns.
    pub turn_count: u64,
}

/// Semantic search request over retained managed sessions.
#[derive(Clone, Debug, Serialize)]
pub struct FindSessionsRequest {
    /// UTF-8 search query containing from 1 through 4,096 bytes after
    /// validation.
    pub query: String,
    /// Optional result count from 1 through 20.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u8>,
}

impl FindSessionsRequest {
    pub(crate) fn validate(&self) -> Result<(), ManagedError> {
        if self.query.trim().is_empty() || self.query.len() > 4_096 {
            return Err(ManagedError::Configuration(
                "managed history query must contain 1-4096 UTF-8 bytes".to_owned(),
            ));
        }
        if self.limit.is_some_and(|limit| !(1..=20).contains(&limit)) {
            return Err(ManagedError::Configuration(
                "managed history limit must be from 1 through 20".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Request to read selected turns from one retained managed session.
#[derive(Clone, Debug)]
pub struct ReadSessionRequest {
    /// UUIDv7 session identifier.
    pub session_id: String,
    /// Optional ordered turn selection with at most 20 entries.
    pub turn_ids: Option<Vec<String>>,
}

impl ReadSessionRequest {
    pub(crate) fn validate(&self) -> Result<(), ManagedError> {
        let session_id = uuid::Uuid::parse_str(&self.session_id).map_err(|_| {
            ManagedError::Configuration(
                "managed history session id must be a canonical lowercase hyphenated RFC-variant UUIDv7"
                    .to_owned(),
            )
        })?;
        if session_id.get_version_num() != 7
            || session_id.get_variant() != uuid::Variant::RFC4122
            || session_id.hyphenated().to_string() != self.session_id
        {
            return Err(ManagedError::Configuration(
                "managed history session id must be a canonical lowercase hyphenated RFC-variant UUIDv7"
                    .to_owned(),
            ));
        }
        if self.turn_ids.as_ref().is_some_and(|ids| ids.len() > 20) {
            return Err(ManagedError::Configuration(
                "managed history turn ids must contain at most 20 entries".to_owned(),
            ));
        }
        for turn_id in self.turn_ids.iter().flatten() {
            validate_id("turn", turn_id)?;
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub(crate) struct ReadSessionBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) turn_ids: Option<&'a [String]>,
}

/// Durable source event cited by a history response.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HistorySource {
    /// Stable source turn identifier.
    pub turn_id: String,
    /// Durable event cursor for the source.
    pub cursor: String,
}

/// Retained session cited by a history response.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HistoryCitation {
    /// Stable retained thread identifier.
    pub thread_id: String,
    /// Current retained thread title.
    pub title: String,
    /// Durable source events supporting the citation.
    pub sources: Vec<HistorySource>,
}

/// One semantic-search hit from retained session history.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SessionSearchHit {
    /// Stable managed session identifier.
    pub session_id: String,
    /// Current session title.
    pub title: String,
    /// Stable matching turn identifier.
    pub turn_id: String,
    /// Durable cursor for the matching event.
    pub cursor: String,
    /// Service-defined similarity score.
    pub score: f64,
    /// UTF-8 result excerpt.
    pub snippet: String,
}

/// Semantic-search results and their durable citations.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FindSessionsResponse {
    /// Normalized query returned by the service.
    pub query: String,
    /// Matching retained sessions.
    pub results: Vec<SessionSearchHit>,
    /// Durable citations supporting the results.
    pub citations: Vec<HistoryCitation>,
}

/// One retained user/assistant turn read from managed history.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SessionTurn {
    /// Stable managed session identifier.
    pub session_id: String,
    /// Current session title.
    pub title: String,
    /// Stable turn identifier.
    pub turn_id: String,
    /// Durable event cursor.
    pub cursor: String,
    /// Complete retained user text.
    pub user: String,
    /// Complete retained assistant text.
    pub assistant: String,
}

/// Selected retained turns and their durable citations.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReadSessionResponse {
    /// Retained turns in service order.
    pub turns: Vec<SessionTurn>,
    /// Durable citations supporting the response.
    pub citations: Vec<HistoryCitation>,
}

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Versioned account-memory identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct MemoryKey {
    /// Positive JavaScript-safe memory identifier.
    pub id: u64,
    /// Positive JavaScript-safe memory version.
    pub version: u64,
}

impl MemoryKey {
    pub(crate) fn validate(self) -> Result<(), ManagedError> {
        if self.id == 0
            || self.version == 0
            || self.id > MAX_SAFE_INTEGER
            || self.version > MAX_SAFE_INTEGER
        {
            return Err(ManagedError::Configuration(
                "managed memory id and version must be positive safe integers".to_owned(),
            ));
        }
        Ok(())
    }
}

/// One versioned account-memory record.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MemoryRecord {
    /// Versioned memory identity.
    pub key: MemoryKey,
    /// Complete retained memory content.
    pub content: String,
    /// Creation time in Unix milliseconds.
    pub created_at_ms: i64,
    /// Last-update time in Unix milliseconds.
    pub updated_at_ms: i64,
    /// Last memory-scan time in Unix milliseconds.
    pub last_scanned_at_ms: Option<i64>,
    /// Number of scans recorded by the service.
    pub scan_count: u64,
    /// Last use time in Unix milliseconds.
    pub last_used_at_ms: Option<i64>,
    /// Number of recorded uses.
    pub use_count: u64,
    /// Optional probation deadline in Unix milliseconds.
    pub probation_until_ms: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct MemoryListResponse {
    pub(crate) memories: Vec<MemoryRecord>,
}

/// Server-advertised capabilities for one managed agent.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentCapabilities {
    /// Whether accepted turns survive client disconnects.
    pub durable_turns: bool,
    /// Whether durable events can resume from a cursor.
    pub resumable_events: bool,
    /// Whether an active turn accepts steering input.
    pub live_steer: bool,
    /// Whether an active turn accepts cancellation.
    pub live_cancel: bool,
    /// Server-advertised workspace mode.
    pub workspace: String,
    /// Whether tools can target explicit sandbox and connected-user environments.
    pub execution_environments: bool,
}

/// Model and reasoning policy owned by one managed agent.
///
/// Model and reasoning mode may only be changed before the first turn is
/// accepted. Thinking and fast mode may be changed throughout the lifecycle;
/// an awaited update applies to subsequently admitted prompts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSettings {
    /// Hosted model selected for this agent.
    #[serde(with = "model_serde")]
    pub model: Model,
    /// Requested reasoning effort.
    pub thinking: Thinking,
    /// Requested reasoning execution mode.
    #[serde(with = "reasoning_mode_serde")]
    pub reasoning_mode: ReasoningMode,
    /// Whether subsequently accepted turns use priority processing.
    pub fast_mode: bool,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            model: Model::Sol,
            thinking: Thinking::High,
            reasoning_mode: ReasoningMode::Standard,
            fast_mode: false,
        }
    }
}

#[derive(Default, Serialize)]
pub(crate) struct AgentSettingsPatch {
    #[serde(skip_serializing_if = "Option::is_none", with = "optional_model_serde")]
    pub(crate) model: Option<Model>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thinking: Option<Thinking>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "optional_reasoning_mode_serde"
    )]
    pub(crate) reasoning_mode: Option<ReasoningMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fast_mode: Option<bool>,
}

impl From<AgentSettings> for AgentSettingsPatch {
    fn from(settings: AgentSettings) -> Self {
        Self {
            model: Some(settings.model),
            thinking: Some(settings.thinking),
            reasoning_mode: Some(settings.reasoning_mode),
            fast_mode: Some(settings.fast_mode),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentSettingsResponse {
    pub(crate) settings: AgentSettings,
}

mod optional_model_serde {
    use nanocodex_oai_api::Model;
    use serde::Serializer;

    pub(super) fn serialize<S>(model: &Option<Model>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(
            model
                .expect("skipped optional model must be present")
                .as_str(),
        )
    }
}

mod optional_reasoning_mode_serde {
    use nanocodex_oai_api::ReasoningMode;
    use serde::Serializer;

    pub(super) fn serialize<S>(
        mode: &Option<ReasoningMode>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(
            mode.expect("skipped optional reasoning mode must be present")
                .as_str(),
        )
    }
}

mod model_serde {
    use nanocodex_oai_api::Model;
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub(super) fn serialize<S>(model: &Model, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(model.as_str())
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Model, D::Error>
    where
        D: Deserializer<'de>,
    {
        match String::deserialize(deserializer)?.as_str() {
            "gpt-5.6-sol" => Ok(Model::Sol),
            "gpt-5.6-terra" => Ok(Model::Terra),
            "gpt-5.6-luna" => Ok(Model::Luna),
            value => Err(de::Error::unknown_variant(
                value,
                &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"],
            )),
        }
    }
}

mod reasoning_mode_serde {
    use nanocodex_oai_api::ReasoningMode;
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub(super) fn serialize<S>(mode: &ReasoningMode, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(mode.as_str())
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<ReasoningMode, D::Error>
    where
        D: Deserializer<'de>,
    {
        match String::deserialize(deserializer)?.as_str() {
            "standard" => Ok(ReasoningMode::Standard),
            "pro" => Ok(ReasoningMode::Pro),
            value => Err(de::Error::unknown_variant(value, &["standard", "pro"])),
        }
    }
}

/// Input for one currently active managed turn.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ActiveTurn {
    /// Stable active turn identifier.
    pub id: String,
    /// Complete accepted input.
    pub input: PromptInput,
}

/// Current durable state for an account-owned agent.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentState {
    /// Stable managed agent identifier.
    pub agent_id: String,
    /// Stable managed session identifier.
    pub session_id: String,
    /// Whether the service has retained agent state.
    pub has_snapshot: bool,
    /// Number of completed turns.
    pub completed_turns: u64,
    /// Last-active timestamp supplied by the service.
    pub last_active: f64,
    /// Stable identifiers of active turns.
    pub active_turns: Vec<String>,
    /// Complete input details for active turns.
    pub active_turn_details: Vec<ActiveTurn>,
    /// Whether the agent runtime is currently loaded.
    pub agent_loaded: bool,
    /// Number of connected event clients.
    pub connected_clients: u64,
    /// Server-advertised managed capabilities.
    pub capabilities: AgentCapabilities,
    /// Current model and reasoning policy.
    pub settings: AgentSettings,
    /// Latest durable event cursor.
    pub latest_event_cursor: String,
    /// Current durable stream failure, when present.
    pub stream_error: Option<String>,
}

/// Durable managed turn state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    /// The turn was durably accepted.
    Accepted,
    /// Cancellation was requested.
    Cancelling,
    /// Execution completed successfully.
    Completed,
    /// Execution was cancelled.
    Cancelled,
    /// Execution failed terminally.
    Failed,
}

/// Current durable view of one managed turn.
#[derive(Debug, Deserialize, Serialize)]
pub struct TurnView {
    /// Stable managed turn identifier.
    pub turn_id: String,
    /// Current durable turn state.
    pub state: TurnState,
    /// Complete accepted input.
    pub input: PromptInput,
    /// Cursor of the durable acceptance event.
    pub accepted_cursor: String,
    /// Cursor of the terminal event, when terminal.
    pub terminal_cursor: Option<String>,
    /// Creation timestamp supplied by the service.
    pub created_at: f64,
    /// Acceptance timestamp supplied by the service.
    pub accepted_at: f64,
    /// Last-update timestamp supplied by the service.
    pub updated_at: f64,
    /// Number of execution attempts.
    pub attempt_count: u64,
    /// Scheduled retry timestamp, when present.
    pub retry_at: Option<f64>,
    /// Current failure detail, when present.
    pub error: Option<String>,
    /// Typed terminal durable event, when present.
    pub terminal: Option<ManagedEventData>,
}

/// Receipt returned by a steer or cancel action.
#[derive(Debug, Deserialize, Serialize)]
pub struct TurnAction {
    /// Stable managed turn identifier.
    pub turn_id: String,
    /// Action state returned by the service.
    pub state: String,
}

#[derive(Serialize)]
pub(crate) struct TurnSubmission<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<&'a str>,
    pub(crate) input: &'a PromptInput,
}

#[derive(Serialize)]
pub(crate) struct TurnSteer<'a> {
    pub(crate) input: &'a PromptInput,
}

/// One bounded page of durable managed events.
#[derive(Debug, Deserialize, Serialize)]
pub struct EventHistoryPage {
    /// Strictly increasing durable events.
    pub data: Vec<ManagedEvent>,
    /// Whether older events remain available.
    pub has_more: bool,
    /// Latest durable cursor currently known by the service.
    pub latest_cursor: String,
}

/// One durable managed event envelope.
#[derive(Clone, Debug, Serialize)]
pub struct ManagedEvent {
    /// Canonical unsigned-decimal durable cursor.
    pub cursor: String,
    /// Creation timestamp supplied by the service.
    pub created_at: Option<f64>,
    /// Associated managed turn identifier, when present.
    pub turn_id: Option<String>,
    /// Typed managed event body.
    #[serde(flatten)]
    pub data: ManagedEventData,
}

impl<'de> Deserialize<'de> for ManagedEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        #[derive(Deserialize)]
        struct Metadata {
            cursor: String,
            created_at: Option<f64>,
            turn_id: Option<String>,
        }
        let metadata: Metadata = decode_raw(&raw).map_err(de::Error::custom)?;
        let data = ManagedEventData::decode(&raw).map_err(de::Error::custom)?;
        Ok(Self {
            cursor: metadata.cursor,
            created_at: metadata.created_at,
            turn_id: metadata.turn_id,
            data,
        })
    }
}

#[cfg(test)]
mod settings_tests {
    use nanocodex_oai_api::{Model, ReasoningMode, Thinking};
    use serde_json::json;

    use super::{AgentSettings, AgentSettingsPatch};

    #[test]
    fn settings_use_canonical_managed_protocol_values() {
        assert_eq!(
            serde_json::to_value(AgentSettings::default()).expect("settings should serialize"),
            json!({
                "model": "gpt-5.6-sol",
                "thinking": "high",
                "reasoning_mode": "standard",
                "fast_mode": false
            })
        );
        assert_eq!(
            serde_json::from_value::<AgentSettings>(json!({
                "model": "gpt-5.6-terra",
                "thinking": "xhigh",
                "reasoning_mode": "pro",
                "fast_mode": true
            }))
            .expect("canonical settings should deserialize"),
            AgentSettings {
                model: Model::Terra,
                thinking: Thinking::Xhigh,
                reasoning_mode: ReasoningMode::Pro,
                fast_mode: true,
            }
        );
    }

    #[test]
    fn settings_reject_aliases_and_patch_only_selected_fields() {
        assert!(
            serde_json::from_value::<AgentSettings>(json!({
                "model": "sol",
                "thinking": "high",
                "reasoning_mode": "standard",
                "fast_mode": false
            }))
            .is_err()
        );
        assert_eq!(
            serde_json::to_value(AgentSettingsPatch {
                model: Some(Model::Luna),
                ..AgentSettingsPatch::default()
            })
            .expect("settings patch should serialize"),
            json!({"model": "gpt-5.6-luna"})
        );
    }
}

/// Typed body of one durable managed event.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ManagedEventData {
    /// The managed agent was created.
    AgentCreated {
        /// Stable managed agent identifier.
        agent_id: String,
        /// Forward-compatible capabilities retained as exact JSON values.
        capabilities: Value,
    },
    /// A turn was durably accepted.
    TurnAccepted {
        /// Stable managed turn identifier.
        id: String,
        /// Complete accepted input.
        input: PromptInput,
        /// Whether an idempotent submission replayed an existing turn.
        replayed: bool,
    },
    /// Cancellation is in progress.
    TurnCancelling {
        /// Stable managed turn identifier.
        id: String,
        /// Current cancellation detail.
        error: Option<String>,
        /// Scheduled retry timestamp, when present.
        retry_at: Option<f64>,
    },
    /// A turn completed successfully.
    TurnCompleted {
        /// Stable managed turn identifier.
        id: String,
        /// Complete final assistant message.
        final_message: String,
        /// Provider usage retained as a forward-compatible JSON value.
        usage: Option<Value>,
        /// Durable history citations used by this turn, in service order.
        citations: Vec<HistoryCitation>,
        /// Usage collection failure, when present.
        usage_error: Option<String>,
    },
    /// A turn was cancelled.
    TurnCancelled {
        /// Stable managed turn identifier.
        id: String,
    },
    /// A turn failed transiently and may retry.
    TurnRetryable {
        /// Stable managed turn identifier.
        id: String,
        /// Retryable failure detail.
        error: String,
    },
    /// A turn failed terminally.
    TurnFailed {
        /// Stable managed turn identifier.
        id: String,
        /// Terminal failure detail.
        error: String,
    },
    /// A canonical nested agent lifecycle event.
    Event {
        /// Exact nested agent-event JSON. The raw object is decoded directly
        /// into AgentEvent only when requested; it never traverses Value.
        event: Box<RawValue>,
        /// Numeric subagent identity. Absent for the root agent.
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<u64>,
    },
    /// The server-side durable stream failed.
    StreamFailed {
        /// Stream failure detail.
        error: String,
    },
}

impl<'de> Deserialize<'de> for ManagedEventData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        Self::decode(&raw).map_err(de::Error::custom)
    }
}

impl ManagedEventData {
    fn decode(raw: &RawValue) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Discriminator {
            #[serde(rename = "type")]
            kind: String,
        }
        #[derive(Deserialize)]
        struct AgentCreated {
            agent_id: String,
            capabilities: Value,
        }
        #[derive(Deserialize)]
        struct TurnAccepted {
            id: String,
            input: PromptInput,
            replayed: bool,
        }
        #[derive(Deserialize)]
        struct TurnCancelling {
            id: String,
            error: Option<String>,
            retry_at: Option<f64>,
        }
        #[derive(Deserialize)]
        struct TurnCompleted {
            id: String,
            final_message: String,
            usage: Option<Value>,
            citations: Vec<HistoryCitation>,
            usage_error: Option<String>,
        }
        #[derive(Deserialize)]
        struct Id {
            id: String,
        }
        #[derive(Deserialize)]
        struct Failure {
            id: String,
            error: String,
        }
        #[derive(Deserialize)]
        struct Event {
            event: Box<RawValue>,
            agent_id: Option<u64>,
        }
        #[derive(Deserialize)]
        struct StreamFailed {
            error: String,
        }

        let kind: Discriminator = decode_raw(raw)?;
        match kind.kind.as_str() {
            "agent_created" => {
                let value: AgentCreated = decode_raw(raw)?;
                Ok(Self::AgentCreated {
                    agent_id: value.agent_id,
                    capabilities: value.capabilities,
                })
            }
            "turn_accepted" => {
                let value: TurnAccepted = decode_raw(raw)?;
                Ok(Self::TurnAccepted {
                    id: value.id,
                    input: value.input,
                    replayed: value.replayed,
                })
            }
            "turn_cancelling" => {
                let value: TurnCancelling = decode_raw(raw)?;
                Ok(Self::TurnCancelling {
                    id: value.id,
                    error: value.error,
                    retry_at: value.retry_at,
                })
            }
            "turn_completed" => {
                let value: TurnCompleted = decode_raw(raw)?;
                Ok(Self::TurnCompleted {
                    id: value.id,
                    final_message: value.final_message,
                    usage: value.usage,
                    citations: value.citations,
                    usage_error: value.usage_error,
                })
            }
            "turn_cancelled" => {
                let value: Id = decode_raw(raw)?;
                Ok(Self::TurnCancelled { id: value.id })
            }
            "turn_retryable" => {
                let value: Failure = decode_raw(raw)?;
                Ok(Self::TurnRetryable {
                    id: value.id,
                    error: value.error,
                })
            }
            "turn_failed" => {
                let value: Failure = decode_raw(raw)?;
                Ok(Self::TurnFailed {
                    id: value.id,
                    error: value.error,
                })
            }
            "event" => {
                let value: Event = decode_raw(raw)?;
                Ok(Self::Event {
                    event: value.event,
                    agent_id: value.agent_id,
                })
            }
            "stream_failed" => {
                let value: StreamFailed = decode_raw(raw)?;
                Ok(Self::StreamFailed { error: value.error })
            }
            other => Err(format!("unknown managed event type {other:?}")),
        }
    }

    /// Decodes the exact nested object of an Event variant into the canonical
    /// typed agent-event contract.
    ///
    /// # Errors
    ///
    /// Returns ManagedError::InvalidEvent when the nested object violates the
    /// canonical agent-event schema.
    pub fn agent_event(&self) -> Result<Option<AgentEvent>, ManagedError> {
        let Self::Event { event, .. } = self else {
            return Ok(None);
        };
        serde_json::from_str(event.get())
            .map(Some)
            .map_err(|error| ManagedError::InvalidEvent(format!("invalid agent event: {error}")))
    }

    /// Returns the exact nested agent-event object without decoding it.
    #[must_use]
    pub fn raw_agent_event(&self) -> Option<&RawValue> {
        let Self::Event { event, .. } = self else {
            return None;
        };
        Some(event)
    }

    /// Returns the associated managed turn identifier, when this event body
    /// carries one.
    #[must_use]
    pub fn turn_id(&self) -> Option<&str> {
        match self {
            Self::TurnAccepted { id, .. }
            | Self::TurnCancelling { id, .. }
            | Self::TurnCompleted { id, .. }
            | Self::TurnCancelled { id }
            | Self::TurnRetryable { id, .. }
            | Self::TurnFailed { id, .. } => Some(id),
            Self::AgentCreated { .. } | Self::Event { .. } | Self::StreamFailed { .. } => None,
        }
    }

    /// Converts a matching terminal event into its final assistant message or
    /// typed terminal error.
    #[must_use]
    pub fn terminal_result(&self, turn_id: &str) -> Option<Result<String, ManagedError>> {
        match self {
            Self::TurnCompleted {
                id, final_message, ..
            } if id == turn_id => Some(Ok(final_message.clone())),
            Self::TurnCancelled { id } if id == turn_id => Some(Err(ManagedError::Turn {
                turn_id: id.clone(),
                state: "cancelled".to_owned(),
                message: "managed turn was cancelled".to_owned(),
            })),
            Self::TurnFailed { id, error } if id == turn_id => Some(Err(ManagedError::Turn {
                turn_id: id.clone(),
                state: "failed".to_owned(),
                message: error.clone(),
            })),
            _ => None,
        }
    }

    pub(crate) const fn event_name(&self) -> &'static str {
        match self {
            Self::AgentCreated { .. } => "agent_created",
            Self::TurnAccepted { .. } => "turn_accepted",
            Self::TurnCancelling { .. } => "turn_cancelling",
            Self::TurnCompleted { .. } => "turn_completed",
            Self::TurnCancelled { .. } => "turn_cancelled",
            Self::TurnRetryable { .. } => "turn_retryable",
            Self::TurnFailed { .. } => "turn_failed",
            Self::Event { .. } => "event",
            Self::StreamFailed { .. } => "stream_failed",
        }
    }
}

fn decode_raw<T: DeserializeOwned>(raw: &RawValue) -> Result<T, String> {
    serde_json::from_str(raw.get()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        FindSessionsRequest, ManagedEvent, ManagedEventData, MemoryKey, ReadSessionRequest,
    };

    #[test]
    fn validates_account_history_requests_before_network_io() {
        assert!(
            FindSessionsRequest {
                query: " ".to_owned(),
                limit: None,
            }
            .validate()
            .is_err()
        );
        assert!(
            FindSessionsRequest {
                query: "memory".to_owned(),
                limit: Some(21),
            }
            .validate()
            .is_err()
        );
        assert!(
            ReadSessionRequest {
                session_id: uuid::Uuid::now_v7().to_string(),
                turn_ids: Some(vec!["turn-1".to_owned(); 20]),
            }
            .validate()
            .is_ok()
        );
        assert!(
            ReadSessionRequest {
                session_id: uuid::Uuid::new_v4().to_string(),
                turn_ids: None,
            }
            .validate()
            .is_err()
        );

        let canonical = uuid::Uuid::now_v7();
        for noncanonical in [
            canonical.hyphenated().to_string().to_uppercase(),
            canonical.simple().to_string(),
            canonical.braced().to_string(),
        ] {
            assert!(
                ReadSessionRequest {
                    session_id: noncanonical,
                    turn_ids: None,
                }
                .validate()
                .is_err()
            );
        }

        let mut non_rfc_bytes = *canonical.as_bytes();
        non_rfc_bytes[8] &= 0x3f;
        assert!(
            ReadSessionRequest {
                session_id: uuid::Uuid::from_bytes(non_rfc_bytes).to_string(),
                turn_ids: None,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn memory_keys_must_be_positive_safe_integers() {
        assert!(MemoryKey { id: 1, version: 1 }.validate().is_ok());
        assert!(MemoryKey { id: 0, version: 1 }.validate().is_err());
        assert!(
            MemoryKey {
                id: 9_007_199_254_740_992,
                version: 1,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn nested_agent_event_retains_its_raw_object() {
        let json = concat!(
            r#"{"cursor":"2","created_at":1,"turn_id":"turn-1","type":"event","agent_id":7,"#,
            r#""event": { "protocol_version":1, "request_id":"request-1", "seq":1,"#,
            r#""type":"assistant.delta", "payload": { "delta" : "hi" } }}"#
        );
        let event: ManagedEvent = serde_json::from_str(json).unwrap();
        let raw = event.data.raw_agent_event().unwrap().get();

        assert!(raw.starts_with(r#"{ "protocol_version""#));
        assert!(raw.contains(r#""payload": { "delta" : "hi" }"#));
        assert_eq!(event.data.agent_event().unwrap().unwrap().seq, 1);
        assert!(matches!(
            event.data,
            ManagedEventData::Event {
                agent_id: Some(7),
                ..
            }
        ));
    }

    #[test]
    fn terminal_result_is_typed() {
        let completed = ManagedEventData::TurnCompleted {
            id: "turn-1".to_owned(),
            final_message: "done".to_owned(),
            usage: None,
            citations: Vec::new(),
            usage_error: None,
        };
        assert_eq!(
            completed.terminal_result("turn-1").unwrap().unwrap(),
            "done"
        );
    }

    #[test]
    fn turn_completed_preserves_ordered_history_citations() {
        let json = concat!(
            r#"{"cursor":"9","created_at":2.0,"turn_id":"turn-3","type":"turn_completed","#,
            r#""id":"turn-3","final_message":"done","usage":null,"citations":["#,
            r#"{"thread_id":"thread-b","title":"Second","sources":["#,
            r#"{"turn_id":"turn-2","cursor":"8"},{"turn_id":"turn-1","cursor":"4"}]},"#,
            r#"{"thread_id":"thread-a","title":"First","sources":[]}],"#,
            r#""usage_error":null}"#
        );
        let event: ManagedEvent = serde_json::from_str(json).unwrap();
        let ManagedEventData::TurnCompleted { citations, .. } = &event.data else {
            panic!("expected completed turn");
        };

        assert_eq!(citations[0].thread_id, "thread-b");
        assert_eq!(citations[0].sources[0].turn_id, "turn-2");
        assert_eq!(citations[0].sources[1].cursor, "4");
        assert_eq!(citations[1].thread_id, "thread-a");

        let encoded = serde_json::to_value(event).unwrap();
        let expected: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(encoded, expected);
    }

    #[test]
    fn turn_completed_requires_protocol_citations() {
        let json = r#"{"cursor":"9","created_at":2,"turn_id":"turn-3","type":"turn_completed","id":"turn-3","final_message":"done","usage":null}"#;
        assert!(serde_json::from_str::<ManagedEvent>(json).is_err());
    }
}
