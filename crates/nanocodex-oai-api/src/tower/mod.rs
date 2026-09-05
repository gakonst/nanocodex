//! Generic Tower attempt, retry, and completed streamed-output contracts.
//!
//! Construct and customize services through
//! [`OpenAiBuilder::layer`](crate::OpenAiBuilder::layer) and
//! [`OpenAiBuilder::service`](crate::OpenAiBuilder::service). Managed sessions
//! own attempt construction and mutable transport state.
//!
//! The resulting factory type is deliberately public and remains generic:
//!
//! ```
//! use std::time::Duration;
//!
//! use nanocodex_oai_api::{
//!     OpenAi, OpenAiError,
//!     tower::{
//!         LayeredServiceFactory, ResponsesServiceFactory,
//!         StandardServiceFactory,
//!     },
//! };
//! use tower::timeout::TimeoutLayer;
//!
//! type TimedFactory =
//!     LayeredServiceFactory<StandardServiceFactory, TimeoutLayer>;
//!
//! fn client() -> Result<OpenAi<TimedFactory>, OpenAiError> {
//!     OpenAi::builder("test-api-key")
//!         .layer(TimeoutLayer::new(Duration::from_secs(45)))
//!         .build()
//! }
//!
//! fn accepts_any_stack<F: ResponsesServiceFactory>(_: &OpenAi<F>) {}
//!
//! let openai = client()?;
//! accepts_any_stack(&openai);
//! # Ok::<(), OpenAiError>(())
//! ```

pub(crate) mod attempt;
pub(crate) mod client;
pub(crate) mod middleware;
pub(crate) mod service;
pub(crate) mod service_error;
pub(crate) mod stream;
mod transport_policy;

#[doc(inline)]
pub use crate::openai::{
    CallerServiceFactory, LayeredServiceFactory, ResponsesServiceConfig, ResponsesServiceFactory,
    StandardServiceFactory,
};
#[doc(inline)]
pub use attempt::{
    ResponsesAttempt, ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse,
};
#[doc(inline)]
pub use client::ResponsesClient;
#[doc(inline)]
pub use middleware::{DefaultResponsesService, ResponsesRetryPolicy};
#[doc(inline)]
pub use service_error::ResponsesServiceError;
#[doc(inline)]
pub use stream::{
    CodeCall, CodeCallKind, CompactionOutput, GenerationOutput, ResponsePipelineStats,
};
