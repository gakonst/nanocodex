use std::{collections::BTreeMap, fmt, str::FromStr};

use nanocodex_oai_api::{Model, ReasoningMode, Thinking};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Model identity for the account-managed, provider-neutral control plane.
///
/// Native Responses clients continue to own [`Model`]. Claude identities belong
/// only to the managed service and are never coerced into a Responses model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ManagedModel {
    /// A native Responses-compatible model identity.
    Oai(Model),
    /// Claude Sonnet 4.6 through the managed subscription transport.
    ClaudeSonnet46,
    /// Claude Opus 4.6 through the managed subscription transport.
    ClaudeOpus46,
    /// Claude Sonnet 5.5 through the managed subscription transport.
    ClaudeSonnet55,
    /// Claude Opus 5.5 through the managed subscription transport.
    ClaudeOpus55,
}

impl ManagedModel {
    /// Canonical managed wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Oai(model) => model.as_str(),
            Self::ClaudeSonnet46 => "claude-sonnet-4-6",
            Self::ClaudeOpus46 => "claude-opus-4-6",
            Self::ClaudeSonnet55 => "claude-sonnet-5-5",
            Self::ClaudeOpus55 => "claude-opus-5-5",
        }
    }

    /// Native agent-loop family whose conversation format this model uses.
    #[must_use]
    pub const fn family(self) -> nanocodex_agent::HarnessFamily {
        match self {
            Self::Oai(_) => nanocodex_agent::HarnessFamily::Codex,
            Self::ClaudeSonnet46
            | Self::ClaudeOpus46
            | Self::ClaudeSonnet55
            | Self::ClaudeOpus55 => nanocodex_agent::HarnessFamily::Claude,
        }
    }

    /// Managed identity of a shared harness model, when the service hosts it.
    #[must_use]
    pub fn from_harness(model: nanocodex_agent::HarnessModel) -> Option<Self> {
        match model {
            nanocodex_agent::HarnessModel::Codex(model) => Some(Self::Oai(model)),
            nanocodex_agent::HarnessModel::Claude(model) => model.as_str().parse().ok(),
        }
    }

    /// Default effort for a newly selected model (not account availability).
    #[must_use]
    pub const fn default_thinking(self) -> Thinking {
        match self {
            Self::Oai(model) => model.default_thinking(),
            Self::ClaudeSonnet46
            | Self::ClaudeOpus46
            | Self::ClaudeSonnet55
            | Self::ClaudeOpus55 => Thinking::Medium,
        }
    }

    /// Whether this known model supports an effort; availability requires the catalog.
    #[must_use]
    pub const fn supports_thinking(self, thinking: Thinking) -> bool {
        match self {
            Self::Oai(model) => model.supports_thinking(thinking),
            Self::ClaudeSonnet46
            | Self::ClaudeOpus46
            | Self::ClaudeSonnet55
            | Self::ClaudeOpus55 => {
                matches!(thinking, Thinking::Low | Thinking::Medium | Thinking::High)
            }
        }
    }

    /// Whether this known model supports a reasoning execution mode.
    #[must_use]
    pub const fn supports_reasoning_mode(self, mode: ReasoningMode) -> bool {
        match self {
            Self::Oai(model) => model.supports_reasoning_mode(mode),
            Self::ClaudeSonnet46
            | Self::ClaudeOpus46
            | Self::ClaudeSonnet55
            | Self::ClaudeOpus55 => matches!(mode, ReasoningMode::Standard),
        }
    }

    /// Whether fast processing can be requested; only the three GPT models support it.
    #[must_use]
    pub const fn supports_fast_mode(self) -> bool {
        matches!(self, Self::Oai(Model::Astra | Model::Sol | Model::Luna))
    }

    /// The native Responses identity, if this is not a Claude model.
    #[must_use]
    pub const fn oai(self) -> Option<Model> {
        match self {
            Self::Oai(model) => Some(model),
            Self::ClaudeSonnet46
            | Self::ClaudeOpus46
            | Self::ClaudeSonnet55
            | Self::ClaudeOpus55 => None,
        }
    }
}

impl Default for ManagedModel {
    fn default() -> Self {
        Self::Oai(Model::default())
    }
}

impl From<Model> for ManagedModel {
    fn from(model: Model) -> Self {
        Self::Oai(model)
    }
}

impl PartialEq<Model> for ManagedModel {
    fn eq(&self, other: &Model) -> bool {
        self.oai() == Some(*other)
    }
}

impl PartialEq<ManagedModel> for Model {
    fn eq(&self, other: &ManagedModel) -> bool {
        other == self
    }
}

impl fmt::Display for ManagedModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ManagedModel {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "claude-sonnet-4-6" => Ok(Self::ClaudeSonnet46),
            "claude-opus-4-6" => Ok(Self::ClaudeOpus46),
            "claude-sonnet-5-5" => Ok(Self::ClaudeSonnet55),
            "claude-opus-5-5" => Ok(Self::ClaudeOpus55),
            "gpt-6.1-sol" => Ok(Model::Sol.into()),
            "gpt-6-luna" => Ok(Model::Luna.into()),
            "gpt-6-astra" => Ok(Model::Astra.into()),
            "@cf/zai-org/glm-5.3" => Ok(Model::Glm53.into()),
            "kimi-k3" => Ok(Model::Kimi.into()),
            "mimo-v2.6-pro" => Ok(Model::Mimo.into()),
            _ => Err("unsupported managed model identity"),
        }
    }
}

impl Serialize for ManagedModel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ManagedModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

/// One model the authenticated account may currently select.
///
/// This server projection is authoritative; a known local identity alone does
/// not imply that its provider credentials are connected.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AvailableModel {
    /// Canonical managed model identity.
    pub id: ManagedModel,
    /// Server presentation label.
    pub name: String,
    /// Provider credential family (for example `claude` or `openai`).
    pub provider: String,
    /// Currently offered reasoning efforts.
    pub thinking: Vec<Thinking>,
    /// Whether the account may request fast processing.
    pub fast_mode: bool,
    /// Currently offered reasoning execution modes.
    #[serde(with = "reasoning_modes_serde")]
    pub reasoning_modes: Vec<ReasoningMode>,
}

/// Authenticated, server-filtered model availability from `GET /v1/models`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelCatalog {
    /// Catalog envelope kind (`list`).
    pub object: String,
    /// Models offered to this account under its actual authorization grant.
    pub data: Vec<AvailableModel>,
    /// Server-selected default, or none when no model is available.
    pub default_model: Option<ManagedModel>,
    /// Whether one provider catalog is unavailable while others remain usable.
    #[serde(default)]
    pub partial: bool,
    /// Public provider availability projections; absent on older servers.
    #[serde(default)]
    pub availability: BTreeMap<String, CatalogProviderAvailability>,
}

/// Safe credential/catalog availability projection without auth material.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CatalogProviderAvailability {
    /// Whether the provider subscription is connected.
    pub connected: bool,
    /// Whether its model catalog is presently usable.
    pub available: bool,
    /// Closed, public failure reason when catalog discovery failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CatalogAvailabilityError>,
}

/// Public catalog failure codes, never arbitrary provider response text.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogAvailabilityError {
    /// Claude credentials are connected but model discovery is unavailable.
    ClaudeModelsUnavailable,
}

mod reasoning_modes_serde {
    use nanocodex_oai_api::ReasoningMode;
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

    pub(super) fn serialize<S: Serializer>(
        modes: &[ReasoningMode],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        modes
            .iter()
            .map(|mode| mode.as_str())
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<ReasoningMode>, D::Error> {
        Vec::<String>::deserialize(deserializer)?
            .into_iter()
            .map(|mode| match mode.as_str() {
                "standard" => Ok(ReasoningMode::Standard),
                "pro" => Ok(ReasoningMode::Pro),
                _ => Err(de::Error::custom("unsupported managed reasoning mode")),
            })
            .collect()
    }
}
