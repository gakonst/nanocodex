//! Provider-neutral decision results.

use serde::Serialize;

use crate::{ChoiceValue, DecisionError, DecisionRequest, QuestionKind};

/// Answers to every question in one [`DecisionRequest`].
///
/// A decision always holds exactly one answer per question, in question
/// order, and each answer matches its question's type or is a refusal.
/// [`Decision::new`] enforces this for every provider, so consumers can index
/// answers by question position or look them up by name.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Decision {
    model: String,
    answers: Vec<Answer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<DecisionUsage>,
}

impl Decision {
    /// Pairs provider answers with the questions in `request`.
    ///
    /// Providers that key answers by name rather than position must first
    /// arrange them in question order.
    ///
    /// # Errors
    ///
    /// Returns [`DecisionError::InvalidResponse`] when the answers do not
    /// correspond one-to-one with the request's questions, an answer's type
    /// differs from its question's, or a choice or score answer refers to an
    /// option or level the question did not offer.
    pub fn new(
        request: &DecisionRequest,
        model: impl Into<String>,
        answers: Vec<Answer>,
        usage: Option<DecisionUsage>,
    ) -> Result<Self, DecisionError> {
        let questions = request.questions();
        if answers.len() != questions.len() {
            return Err(DecisionError::InvalidResponse(format!(
                "expected {} answers but received {}",
                questions.len(),
                answers.len()
            )));
        }
        for (question, answer) in questions.iter().zip(&answers) {
            if answer.name != question.name {
                return Err(DecisionError::InvalidResponse(format!(
                    "expected an answer to `{}` but received `{}`",
                    question.name, answer.name
                )));
            }
            let matches = match (&question.kind, &answer.outcome) {
                (_, Outcome::Refusal) | (QuestionKind::Predicate, Outcome::Predicate { .. }) => {
                    true
                }
                (
                    QuestionKind::Choice { choices },
                    Outcome::Choice {
                        choice,
                        probabilities,
                        ..
                    },
                ) => {
                    let offered =
                        |value: &ChoiceValue| choices.iter().any(|option| option.value == *value);
                    offered(choice) && probabilities.iter().all(|entry| offered(&entry.value))
                }
                (QuestionKind::Score { levels }, Outcome::Score { probabilities, .. }) => {
                    probabilities.iter().all(|entry| {
                        usize::try_from(entry.index).is_ok_and(|index| index < levels.len())
                    })
                }
                _ => false,
            };
            if !matches {
                return Err(DecisionError::InvalidResponse(format!(
                    "the answer to `{}` does not match its question",
                    question.name
                )));
            }
        }
        Ok(Self {
            model: model.into(),
            answers,
            usage,
        })
    }

    /// Provider model identifier that produced the answers.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Answers in question order.
    #[must_use]
    pub fn answers(&self) -> &[Answer] {
        &self.answers
    }

    /// The answer to the named question.
    #[must_use]
    pub fn answer(&self, name: &str) -> Option<&Answer> {
        self.answers.iter().find(|answer| answer.name == name)
    }

    /// Token usage, when the provider reports it.
    #[must_use]
    pub const fn usage(&self) -> Option<DecisionUsage> {
        self.usage
    }

    /// Consumes the decision and returns its answers in question order.
    #[must_use]
    pub fn into_answers(self) -> Vec<Answer> {
        self.answers
    }
}

/// The result for one named question.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Answer {
    /// Name of the question this answers.
    pub name: String,
    /// Typed result.
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// The typed result of one question.
///
/// Providers report probabilities and confidences between 0 and 1. Treat them
/// as model estimates and set application thresholds from labeled examples.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    /// Answer to a predicate question.
    Predicate {
        /// Estimated probability that the condition is true.
        probability: f64,
    },
    /// Answer to a choice question.
    Choice {
        /// The most likely option.
        choice: ChoiceValue,
        /// The model's confidence in `choice`.
        confidence: f64,
        /// Probability of each option.
        probabilities: Vec<ChoiceProbability>,
    },
    /// Answer to a score question.
    Score {
        /// Probability-weighted average of level indices, so it may fall
        /// between levels.
        score: f64,
        /// The model's confidence in the score.
        confidence: f64,
        /// Probability of each level.
        probabilities: Vec<LevelProbability>,
    },
    /// The model declined to answer this question. Other questions in the
    /// same request may still have answers.
    Refusal,
}

/// Probability assigned to one choice option.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChoiceProbability {
    /// The option.
    pub value: ChoiceValue,
    /// Probability that this option is correct.
    pub probability: f64,
}

/// Probability assigned to one score level.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LevelProbability {
    /// Zero-based position of the level in the question's rubric.
    pub index: u32,
    /// The level's label.
    pub label: String,
    /// Probability that the input belongs at this level.
    pub probability: f64,
}

/// Tokens consumed by one decision request.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct DecisionUsage {
    /// Tokens of input evidence and questions.
    pub input_tokens: u64,
    /// Tokens generated by the provider, if it bills any.
    pub output_tokens: u64,
}
