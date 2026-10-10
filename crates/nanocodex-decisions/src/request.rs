//! Provider-neutral decision requests: shared evidence plus typed questions.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::DecisionError;

/// Inclusive bounds on the number of options in one choice question.
const CHOICE_OPTIONS: std::ops::RangeInclusive<usize> = 2..=255;

/// A validated set of questions evaluated against one shared input.
///
/// Every question has a unique, non-empty name. Decision models key their
/// answers by that name, so [`Decision`](crate::Decision) can pair each answer
/// with the question that produced it regardless of how a provider orders or
/// labels its response.
///
/// Requests deserialize from the same JSON shape the decision tool accepts and
/// are validated during deserialization:
///
/// ```
/// use nanocodex_decisions::DecisionRequest;
///
/// let request: DecisionRequest = serde_json::from_value(serde_json::json!({
///     "input": "I was charged twice for my order.",
///     "questions": [{
///         "type": "choice",
///         "name": "department",
///         "instructions": "Which department should handle this complaint?",
///         "choices": [
///             { "value": "billing", "description": "Payments and refunds." },
///             { "value": "other" }
///         ]
///     }]
/// }))?;
/// assert_eq!(request.questions()[0].name, "department");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "UncheckedRequest")]
pub struct DecisionRequest {
    input: DecisionInput,
    questions: Vec<Question>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UncheckedRequest {
    input: DecisionInput,
    questions: Vec<Question>,
}

impl TryFrom<UncheckedRequest> for DecisionRequest {
    type Error = DecisionError;

    fn try_from(request: UncheckedRequest) -> Result<Self, Self::Error> {
        Self::new(request.input, request.questions)
    }
}

impl DecisionRequest {
    /// Validates a request.
    ///
    /// # Errors
    ///
    /// Returns [`DecisionError::InvalidRequest`] when there are no questions,
    /// multimodal input has no parts, a question name is empty or repeated, a
    /// choice question does not offer between 2 and 255 distinct options, or a
    /// score question has no levels.
    pub fn new(
        input: impl Into<DecisionInput>,
        questions: impl IntoIterator<Item = Question>,
    ) -> Result<Self, DecisionError> {
        let input = input.into();
        if matches!(&input, DecisionInput::Parts(parts) if parts.is_empty()) {
            return Err(invalid("multimodal input needs at least one part"));
        }
        let questions: Vec<Question> = questions.into_iter().collect();
        if questions.is_empty() {
            return Err(invalid("at least one question is required"));
        }
        let mut names = HashSet::with_capacity(questions.len());
        for question in &questions {
            if question.name.is_empty() {
                return Err(invalid("every question needs a non-empty name"));
            }
            if !names.insert(question.name.as_str()) {
                return Err(invalid(format!(
                    "question name `{}` is used more than once",
                    question.name
                )));
            }
            question.kind.validate(&question.name)?;
        }
        Ok(Self { input, questions })
    }

    /// Evidence shared by every question.
    #[must_use]
    pub const fn input(&self) -> &DecisionInput {
        &self.input
    }

    /// Questions in the order their answers are reported.
    #[must_use]
    pub fn questions(&self) -> &[Question] {
        &self.questions
    }
}

/// Evidence that every question in a request evaluates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DecisionInput {
    /// Plain text.
    Text(String),
    /// Ordered text and image parts presented together as one user message.
    Parts(Vec<InputPart>),
    /// A structured record, such as a support ticket or a pending tool call.
    /// Providers without native structured input receive it as compact JSON
    /// text.
    Record(Map<String, Value>),
}

impl From<String> for DecisionInput {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<&str> for DecisionInput {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<Vec<InputPart>> for DecisionInput {
    fn from(parts: Vec<InputPart>) -> Self {
        Self::Parts(parts)
    }
}

impl From<Map<String, Value>> for DecisionInput {
    fn from(record: Map<String, Value>) -> Self {
        Self::Record(record)
    }
}

/// One part of multimodal decision input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum InputPart {
    /// Text evidence.
    #[serde(rename = "input_text")]
    Text {
        /// Complete text.
        text: String,
    },
    /// Image evidence.
    #[serde(rename = "input_image")]
    Image {
        /// A base64 `data:` URL or a publicly reachable HTTP(S) URL.
        image_url: String,
        /// Resolution at which the model inspects the image; the provider
        /// chooses when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
}

impl InputPart {
    /// Creates a text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Creates an image part at the provider's default detail.
    pub fn image(image_url: impl Into<String>) -> Self {
        Self::Image {
            image_url: image_url.into(),
            detail: None,
        }
    }
}

/// Resolution at which a decision model inspects an image.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    /// Let the model choose.
    Auto,
    /// Reduced resolution.
    Low,
    /// Increased resolution.
    High,
    /// The image's original resolution.
    Original,
}

/// One named question about the shared input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "TaggedQuestion")]
pub struct Question {
    /// Unique key identifying this question's answer.
    pub name: String,
    /// What to evaluate, phrased around observable criteria.
    pub instructions: String,
    /// Answer type and its type-specific options.
    #[serde(flatten)]
    pub kind: QuestionKind,
}

/// Deserialization shape for [`Question`] that rejects fields belonging to
/// another question type, which a flattened struct would silently ignore.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TaggedQuestion {
    Predicate {
        name: String,
        instructions: String,
    },
    Choice {
        name: String,
        instructions: String,
        choices: Vec<ChoiceOption>,
    },
    Score {
        name: String,
        instructions: String,
        levels: Vec<ScoreLevel>,
    },
}

impl From<TaggedQuestion> for Question {
    fn from(question: TaggedQuestion) -> Self {
        match question {
            TaggedQuestion::Predicate { name, instructions } => Self::predicate(name, instructions),
            TaggedQuestion::Choice {
                name,
                instructions,
                choices,
            } => Self::choice(name, instructions, choices),
            TaggedQuestion::Score {
                name,
                instructions,
                levels,
            } => Self::score(name, instructions, levels),
        }
    }
}

impl Question {
    /// Asks for the probability that a condition about the input is true.
    pub fn predicate(name: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            instructions: instructions.into(),
            kind: QuestionKind::Predicate,
        }
    }

    /// Asks the model to select exactly one of a set of unordered options.
    pub fn choice(
        name: impl Into<String>,
        instructions: impl Into<String>,
        choices: impl IntoIterator<Item = ChoiceOption>,
    ) -> Self {
        Self {
            name: name.into(),
            instructions: instructions.into(),
            kind: QuestionKind::Choice {
                choices: choices.into_iter().collect(),
            },
        }
    }

    /// Asks the model to rate the input against levels ordered from lowest
    /// to highest.
    pub fn score(
        name: impl Into<String>,
        instructions: impl Into<String>,
        levels: impl IntoIterator<Item = ScoreLevel>,
    ) -> Self {
        Self {
            name: name.into(),
            instructions: instructions.into(),
            kind: QuestionKind::Score {
                levels: levels.into_iter().collect(),
            },
        }
    }
}

/// The answer type a question asks for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuestionKind {
    /// A probability between 0 and 1 that the instructions' condition holds.
    Predicate,
    /// One value from a fixed, unordered set.
    Choice {
        /// Distinct options. Include a fallback such as `"other"` when the
        /// options might not cover every input.
        choices: Vec<ChoiceOption>,
    },
    /// A probability-weighted position on an ordered rubric.
    Score {
        /// Levels ordered from lowest (index 0) to highest.
        levels: Vec<ScoreLevel>,
    },
}

impl QuestionKind {
    fn validate(&self, name: &str) -> Result<(), DecisionError> {
        match self {
            Self::Predicate => Ok(()),
            Self::Choice { choices } => {
                if !CHOICE_OPTIONS.contains(&choices.len()) {
                    return Err(invalid(format!(
                        "choice question `{name}` needs between {} and {} options",
                        CHOICE_OPTIONS.start(),
                        CHOICE_OPTIONS.end()
                    )));
                }
                let mut values = HashSet::with_capacity(choices.len());
                for choice in choices {
                    if !values.insert(&choice.value) {
                        return Err(invalid(format!(
                            "choice question `{name}` repeats the option {}",
                            choice.value
                        )));
                    }
                }
                Ok(())
            }
            Self::Score { levels } if levels.is_empty() => Err(invalid(format!(
                "score question `{name}` needs at least one level"
            ))),
            Self::Score { .. } => Ok(()),
        }
    }
}

/// One option of a choice question.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceOption {
    /// The value reported when this option is selected.
    pub value: ChoiceValue,
    /// When this option applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ChoiceOption {
    /// Creates an option without a description.
    pub fn new(value: impl Into<ChoiceValue>) -> Self {
        Self {
            value: value.into(),
            description: None,
        }
    }

    /// Explains when this option applies.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// A typed choice value. The string `"true"` and the boolean `true` are
/// distinct values.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChoiceValue {
    /// A boolean option.
    Bool(bool),
    /// A string option.
    Text(String),
}

impl std::fmt::Display for ChoiceValue {
    /// Quotes strings so they stay distinguishable from booleans.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bool(value) => write!(f, "{value}"),
            Self::Text(value) => write!(f, "{value:?}"),
        }
    }
}

impl From<bool> for ChoiceValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<String> for ChoiceValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for ChoiceValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

/// One level of a score question's rubric.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreLevel {
    /// Short name for this level.
    pub label: String,
    /// Criteria that distinguish this level from its neighbors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ScoreLevel {
    /// Creates a level without a description.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            description: None,
        }
    }

    /// Describes the criteria for this level.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

fn invalid(message: impl Into<String>) -> DecisionError {
    DecisionError::InvalidRequest(message.into())
}
