//! Harness selection without erasing provider-native builders or transcripts.

use std::{fmt, str::FromStr};

use nanocodex_oai_api::{Model, Thinking};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Agent-loop family selected at a thread boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessFamily {
    /// Nanocodex's Responses agent loop and its configured model providers.
    Codex,
    /// The native Claude Messages agent loop.
    Claude,
    /// Source-adapted xAI Grok Build Responses harness.
    Xai,
}

impl HarnessFamily {
    /// Stable public family identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Xai => "xai",
        }
    }

    /// Default model within this family, independent of credential availability.
    pub const fn default_model(self) -> HarnessModel {
        match self {
            Self::Codex => HarnessModel::Codex(Model::Sol),
            Self::Claude => HarnessModel::Claude(ClaudeModel::Opus55),
            Self::Xai => HarnessModel::Xai(XaiModel::Grok46),
        }
    }
}

impl fmt::Display for HarnessFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HarnessFamily {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "xai" => Ok(Self::Xai),
            _ => Err("expected harness family codex, claude or xai"),
        }
    }
}

/// Known Claude models available to the shared harness router.
///
/// A concrete Claude builder still accepts provider-native model identifiers;
/// this catalog defines the validated choices exposed by shared routing tools.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ClaudeModel {
    /// Claude Opus 5.5.
    Opus55,
    /// Claude Sonnet 5.5.
    Sonnet55,
    /// Claude Fable 5.1.
    Fable51,
    /// Claude Opus 4.6.
    Opus46,
    /// Claude Sonnet 4.6.
    Sonnet46,
    /// Claude Haiku 4.5, with ordinary inference and no adaptive effort.
    Haiku45,
}

impl ClaudeModel {
    /// Known routing models; availability remains the embedding host's policy.
    pub const ALL: [Self; 6] = [
        Self::Opus55,
        Self::Sonnet55,
        Self::Fable51,
        Self::Opus46,
        Self::Sonnet46,
        Self::Haiku45,
    ];

    /// Provider-native model identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Opus55 => "claude-opus-5-5",
            Self::Sonnet55 => "claude-sonnet-5-5",
            Self::Fable51 => "claude-fable-5-1",
            Self::Opus46 => "claude-opus-4-6",
            Self::Sonnet46 => "claude-sonnet-4-6",
            Self::Haiku45 => "claude-haiku-4-5",
        }
    }

    /// Default reasoning effort for a new thread.
    pub const fn default_thinking(self) -> Thinking {
        match self {
            Self::Opus55 => Thinking::Medium,
            Self::Haiku45 => Thinking::None,
            _ => Thinking::High,
        }
    }

    /// Whether the native adaptive-thinking adapter supports this effort.
    pub const fn supports_thinking(self, thinking: Thinking) -> bool {
        match self {
            Self::Haiku45 => matches!(thinking, Thinking::None),
            Self::Opus46 | Self::Sonnet46 => matches!(
                thinking,
                Thinking::Low | Thinking::Medium | Thinking::High | Thinking::Max
            ),
            _ => matches!(
                thinking,
                Thinking::Low | Thinking::Medium | Thinking::High | Thinking::Xhigh | Thinking::Max
            ),
        }
    }
}

impl fmt::Display for ClaudeModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ClaudeModel {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "claude-opus-5-5" | "opus" => Ok(Self::Opus55),
            "claude-sonnet-5-5" | "sonnet" => Ok(Self::Sonnet55),
            "claude-fable-5-1" | "fable" => Ok(Self::Fable51),
            "claude-opus-4-6" => Ok(Self::Opus46),
            "claude-sonnet-4-6" => Ok(Self::Sonnet46),
            "claude-haiku-4-5" | "claude-haiku-4-5-20251001" | "haiku" => Ok(Self::Haiku45),
            _ => Err(
                "unsupported Claude routing model; use opus, sonnet, fable, haiku or a supported Claude model ID",
            ),
        }
    }
}

/// A model belongs to exactly one native harness family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HarnessModel {
    /// A model implemented by the Responses harness.
    Codex(Model),
    /// A model implemented by the native Messages harness.
    Claude(ClaudeModel),
    /// A model implemented by the Grok Build source adaptation.
    Xai(XaiModel),
}

impl HarnessModel {
    /// The harness capable of running this model.
    pub const fn family(self) -> HarnessFamily {
        match self {
            Self::Codex(_) => HarnessFamily::Codex,
            Self::Claude(_) => HarnessFamily::Claude,
            Self::Xai(_) => HarnessFamily::Xai,
        }
    }

    /// Provider-native model identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex(model) => model.as_str(),
            Self::Claude(model) => model.as_str(),
            Self::Xai(model) => model.as_str(),
        }
    }

    /// Default model reasoning policy.
    pub const fn default_thinking(self) -> Thinking {
        match self {
            Self::Codex(model) => model.default_thinking(),
            Self::Claude(model) => model.default_thinking(),
            Self::Xai(model) => model.default_thinking(),
        }
    }

    /// Whether this model supports the requested effort.
    pub const fn supports_thinking(self, thinking: Thinking) -> bool {
        match self {
            Self::Codex(model) => model.supports_thinking(thinking),
            Self::Claude(model) => model.supports_thinking(thinking),
            Self::Xai(model) => model.supports_thinking(thinking),
        }
    }

    /// Whether native fast processing is offered by this model: priority
    /// processing on Responses models and fast mode on Claude Opus.
    pub const fn supports_fast_mode(self) -> bool {
        matches!(
            self,
            Self::Codex(Model::Astra | Model::Sol | Model::Luna)
                | Self::Claude(ClaudeModel::Opus55)
        )
    }

    /// Validated model choices within one family.
    pub fn for_family(family: HarnessFamily) -> impl Iterator<Item = Self> {
        Model::ALL
            .into_iter()
            .map(Self::Codex)
            .chain(ClaudeModel::ALL.into_iter().map(Self::Claude))
            .chain(XaiModel::ALL.into_iter().map(Self::Xai))
            .filter(move |model| model.family() == family)
    }
}

impl From<Model> for HarnessModel {
    fn from(model: Model) -> Self {
        Self::Codex(model)
    }
}
impl Default for HarnessModel {
    fn default() -> Self {
        Self::Codex(Model::default())
    }
}
impl From<ClaudeModel> for HarnessModel {
    fn from(model: ClaudeModel) -> Self {
        Self::Claude(model)
    }
}
impl PartialEq<Model> for HarnessModel {
    fn eq(&self, model: &Model) -> bool {
        *self == Self::Codex(*model)
    }
}
impl PartialEq<HarnessModel> for Model {
    fn eq(&self, model: &HarnessModel) -> bool {
        model == self
    }
}
impl fmt::Display for HarnessModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for HarnessModel {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<Model>()
            .map(Self::Codex)
            .or_else(|_| value.parse::<ClaudeModel>().map(Self::Claude))
            .or_else(|_| value.parse::<XaiModel>().map(Self::Xai))
    }
}

impl Serialize for HarnessModel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for HarnessModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

/// Models in the pinned Grok Build routing catalog.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum XaiModel {
    /// Grok 4.6, upstream default.
    Grok46,
    /// Grok 4.5.
    Grok45,
}
impl XaiModel {
    /// Context capacity in the pinned Grok Build model catalog.
    pub const fn context_window_tokens(self) -> u64 {
        500_000
    }
    /// Model-specific automatic compaction threshold from Grok Build.
    pub const fn auto_compact_threshold_percent(self) -> u32 {
        80
    }
    /// Whether this model advertises provider-hosted search.
    pub const fn supports_backend_search(self) -> bool {
        matches!(self, Self::Grok46)
    }
    /// Known upstream routing choices.
    pub const ALL: [Self; 2] = [Self::Grok46, Self::Grok45];
    /// Native model identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Grok46 => "grok-4.6",
            Self::Grok45 => "grok-4.5",
        }
    }
    /// Upstream default reasoning effort.
    pub const fn default_thinking(self) -> Thinking {
        Thinking::High
    }
    /// Supported explicit reasoning efforts from the upstream catalog.
    pub const fn supports_thinking(self, thinking: Thinking) -> bool {
        matches!(thinking, Thinking::Low | Thinking::Medium | Thinking::High)
            || (matches!(self, Self::Grok46) && matches!(thinking, Thinking::Xhigh))
    }
}
impl fmt::Display for XaiModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for XaiModel {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "grok-4.6" => Ok(Self::Grok46),
            "grok-4.5" => Ok(Self::Grok45),
            _ => Err("unsupported xAI routing model"),
        }
    }
}
impl From<XaiModel> for HarnessModel {
    fn from(model: XaiModel) -> Self {
        Self::Xai(model)
    }
}
