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
}

impl HarnessFamily {
    /// Every family, in stable presentation order.
    pub const ALL: [Self; 2] = [Self::Codex, Self::Claude];

    /// Stable public family identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    /// Default model within this family, independent of credential availability.
    pub const fn default_model(self) -> HarnessModel {
        match self {
            Self::Codex => HarnessModel::Codex(Model::Sol),
            Self::Claude => HarnessModel::Claude(ClaudeModel::Opus55),
        }
    }
}

impl fmt::Display for HarnessFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HarnessFamily {
    type Err = ParseHarnessError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            _ => Err(ParseHarnessError::new(ParseHarnessErrorKind::Family, value)),
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
    /// Claude Haiku 5.5.
    Haiku55,
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
    pub const ALL: [Self; 7] = [
        Self::Opus55,
        Self::Sonnet55,
        Self::Haiku55,
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
            Self::Haiku55 => "claude-haiku-5-5",
            Self::Fable51 => "claude-fable-5-1",
            Self::Opus46 => "claude-opus-4-6",
            Self::Sonnet46 => "claude-sonnet-4-6",
            Self::Haiku45 => "claude-haiku-4-5",
        }
    }

    /// Default reasoning effort for a new thread.
    pub const fn default_thinking(self) -> Thinking {
        match self {
            Self::Opus55 | Self::Haiku55 => Thinking::Medium,
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
    type Err = ParseHarnessError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "claude-opus-5-5" | "opus" => Ok(Self::Opus55),
            "claude-sonnet-5-5" | "sonnet" => Ok(Self::Sonnet55),
            "claude-haiku-5-5" | "haiku" => Ok(Self::Haiku55),
            "claude-fable-5-1" | "fable" => Ok(Self::Fable51),
            "claude-opus-4-6" => Ok(Self::Opus46),
            "claude-sonnet-4-6" => Ok(Self::Sonnet46),
            "claude-haiku-4-5" | "claude-haiku-4-5-20251001" => Ok(Self::Haiku45),
            _ => Err(ParseHarnessError::new(
                ParseHarnessErrorKind::ClaudeModel,
                value,
            )),
        }
    }
}

impl Serialize for ClaudeModel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for ClaudeModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

/// A model belongs to exactly one native harness family.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HarnessModel {
    /// A model implemented by the Responses harness.
    Codex(Model),
    /// A model implemented by the native Messages harness.
    Claude(ClaudeModel),
}

impl HarnessModel {
    /// The harness capable of running this model.
    pub const fn family(self) -> HarnessFamily {
        match self {
            Self::Codex(_) => HarnessFamily::Codex,
            Self::Claude(_) => HarnessFamily::Claude,
        }
    }

    /// The Responses-harness model, when this is a Codex-family model.
    pub const fn as_codex(self) -> Option<Model> {
        match self {
            Self::Codex(model) => Some(model),
            Self::Claude(_) => None,
        }
    }

    /// The Messages-harness model, when this is a Claude-family model.
    pub const fn as_claude(self) -> Option<ClaudeModel> {
        match self {
            Self::Claude(model) => Some(model),
            Self::Codex(_) => None,
        }
    }

    /// Provider-native model identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex(model) => model.as_str(),
            Self::Claude(model) => model.as_str(),
        }
    }

    /// Default model reasoning policy.
    pub const fn default_thinking(self) -> Thinking {
        match self {
            Self::Codex(model) => model.default_thinking(),
            Self::Claude(model) => model.default_thinking(),
        }
    }

    /// Whether this model supports the requested effort.
    pub const fn supports_thinking(self, thinking: Thinking) -> bool {
        match self {
            Self::Codex(model) => model.supports_thinking(thinking),
            Self::Claude(model) => model.supports_thinking(thinking),
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

    /// Every validated model choice, grouped by [`HarnessFamily::ALL`] order.
    pub fn all() -> impl Iterator<Item = Self> {
        Model::ALL
            .into_iter()
            .map(Self::Codex)
            .chain(ClaudeModel::ALL.into_iter().map(Self::Claude))
    }

    /// Validated model choices within one family.
    pub fn for_family(family: HarnessFamily) -> impl Iterator<Item = Self> {
        Self::all().filter(move |model| model.family() == family)
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
    type Err = ParseHarnessError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<Model>()
            .map(Self::Codex)
            .or_else(|_| value.parse::<ClaudeModel>().map(Self::Claude))
            .map_err(|_| ParseHarnessError::new(ParseHarnessErrorKind::Model, value))
    }
}

/// Which harness identifier failed to parse.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ParseHarnessErrorKind {
    /// A [`HarnessFamily`] name.
    Family,
    /// A [`ClaudeModel`] identifier or alias.
    ClaudeModel,
    /// A [`HarnessModel`] identifier or alias from any family.
    Model,
}

/// Error returned when a harness family or model identifier is not recognized.
///
/// The message names every accepted choice for the identifier that was parsed,
/// so it can be shown directly to an operator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseHarnessError {
    kind: ParseHarnessErrorKind,
    value: String,
}

impl ParseHarnessError {
    fn new(kind: ParseHarnessErrorKind, value: &str) -> Self {
        Self {
            kind,
            value: value.to_owned(),
        }
    }

    /// Which identifier failed to parse.
    pub const fn kind(&self) -> ParseHarnessErrorKind {
        self.kind
    }

    /// The rejected input.
    pub fn value(&self) -> &str {
        &self.value
    }
}

const CLAUDE_CHOICES: &str = "opus, sonnet, haiku, fable or a supported claude-* model ID";

impl fmt::Display for ParseHarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = &self.value;
        match self.kind {
            ParseHarnessErrorKind::Family => {
                write!(
                    f,
                    "unknown harness family {value:?}; expected codex or claude"
                )
            }
            ParseHarnessErrorKind::ClaudeModel => {
                write!(
                    f,
                    "unknown Claude model {value:?}; expected {CLAUDE_CHOICES}"
                )
            }
            ParseHarnessErrorKind::Model => {
                write!(f, "unknown model {value:?}; expected a Codex model (")?;
                for (index, model) in Model::ALL.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    f.write_str(model.as_str())?;
                }
                write!(
                    f,
                    " or a routed model such as glm-5.3, kimi or mimo) or a Claude model ({CLAUDE_CHOICES})"
                )
            }
        }
    }
}

impl std::error::Error for ParseHarnessError {}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_errors_name_the_accepted_choices_for_their_identifier() {
        let error = "gpt-7".parse::<HarnessModel>().unwrap_err();
        assert_eq!(error.kind(), ParseHarnessErrorKind::Model);
        assert_eq!(error.value(), "gpt-7");
        let message = error.to_string();
        assert!(message.starts_with("unknown model \"gpt-7\"; expected a Codex model ("));
        for model in Model::ALL {
            assert!(message.contains(model.as_str()), "{message}");
        }
        assert!(message.contains("opus, sonnet, haiku, fable"), "{message}");
        assert!(!message.contains("unknown Claude model"), "{message}");

        let error = "gemini".parse::<HarnessFamily>().unwrap_err();
        assert_eq!(error.kind(), ParseHarnessErrorKind::Family);
        assert_eq!(
            error.to_string(),
            "unknown harness family \"gemini\"; expected codex or claude"
        );
        let error = "claude-opus-9".parse::<ClaudeModel>().unwrap_err();
        assert_eq!(error.kind(), ParseHarnessErrorKind::ClaudeModel);
        let _: Box<dyn std::error::Error + Send + Sync> = Box::new(error);
    }

    #[test]
    fn catalog_round_trips_and_accessors_select_one_family() {
        assert_eq!(
            HarnessModel::all().count(),
            Model::ALL.len() + ClaudeModel::ALL.len()
        );
        for family in HarnessFamily::ALL {
            assert_eq!(family.as_str().parse::<HarnessFamily>(), Ok(family));
            assert!(HarnessModel::for_family(family).all(|m| m.family() == family));
        }
        for model in HarnessModel::all() {
            assert_eq!(model.as_str().parse::<HarnessModel>(), Ok(model));
            assert_eq!(
                model.as_codex().is_some(),
                model.family() == HarnessFamily::Codex
            );
            assert_eq!(
                model.as_claude().is_some(),
                model.family() == HarnessFamily::Claude
            );
        }
        for model in ClaudeModel::ALL {
            let json = serde_json::to_string(&model).unwrap();
            assert_eq!(json, format!("\"{}\"", model.as_str()));
            assert_eq!(serde_json::from_str::<ClaudeModel>(&json).unwrap(), model);
        }
        assert!(serde_json::from_str::<ClaudeModel>("\"gpt-6-luna\"").is_err());
    }
}
