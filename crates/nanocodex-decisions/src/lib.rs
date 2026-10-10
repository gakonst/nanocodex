#![cfg_attr(all(feature = "openai", feature = "tool"), doc = include_str!("../README.md"))]
#![cfg_attr(
    not(all(feature = "openai", feature = "tool")),
    doc = "Provider-neutral decision models for Nanocodex."
)]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod decision;
#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
pub mod openai;
mod request;
#[cfg(feature = "tool")]
mod tool;

use std::sync::Arc;

use async_trait::async_trait;

pub use decision::{Answer, ChoiceProbability, Decision, DecisionUsage, LevelProbability, Outcome};
pub use request::{
    ChoiceOption, ChoiceValue, DecisionInput, DecisionRequest, ImageDetail, InputPart, Question,
    QuestionKind, ScoreLevel,
};
#[cfg(feature = "tool")]
#[cfg_attr(docsrs, doc(cfg(feature = "tool")))]
pub use tool::DecisionTool;

/// A decision API: a model that answers typed questions about shared evidence
/// instead of generating text.
///
/// Each provider translates the validated, provider-neutral
/// [`DecisionRequest`] into its own wire format and builds its result with
/// [`Decision::new`], which enforces one answer per question in question
/// order. Code written against this trait, including the decision tool, works
/// unchanged with every provider.
///
/// A provider that cannot represent part of a request, such as image input
/// for a text-only model, returns [`DecisionError::InvalidRequest`] without
/// contacting its API.
#[async_trait]
pub trait DecisionModel: Send + Sync {
    /// Answers every question in `request`.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be sent, the provider rejects
    /// it, or the provider's response does not answer the request's questions.
    async fn decide(&self, request: &DecisionRequest) -> Result<Decision, DecisionError>;
}

#[async_trait]
impl<T: DecisionModel + ?Sized> DecisionModel for Arc<T> {
    async fn decide(&self, request: &DecisionRequest) -> Result<Decision, DecisionError> {
        (**self).decide(request).await
    }
}

/// Failure to obtain a decision.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecisionError {
    /// The request is malformed or uses input the provider cannot accept.
    /// Retrying the same request will fail again.
    #[error("invalid decision request: {0}")]
    InvalidRequest(String),
    /// The provider answered with an HTTP error status.
    #[error("decision API returned HTTP {status}: {message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// The provider's error message, or the response body when it has no
        /// structured message.
        message: String,
    },
    /// The request did not complete, for example because of a connection
    /// failure or timeout.
    #[error("decision request failed: {0}")]
    Transport(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The provider answered successfully, but its response does not answer
    /// the request's questions.
    #[error("invalid decision response: {0}")]
    InvalidResponse(String),
}
