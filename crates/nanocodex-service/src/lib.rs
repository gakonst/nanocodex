mod attempt;
mod client;
mod error;
mod middleware;
mod service;
mod service_error;
mod socket;
mod stream;
mod telemetry;

pub use attempt::{
    ResponsesAttempt, ResponsesAttemptFactory, ResponsesAttemptKind, ResponsesOutput,
    ResponsesServiceResponse, TransportStats, TransportStatsDelta, TransportStatsSnapshot,
};
pub use client::ResponsesClient;
pub use error::{ResponsesError, RetryAdvice};
pub use middleware::{DefaultResponsesService, ResponsesRetryPolicy};
pub use nanocodex_core::responses::{
    InputTokenDetails, OutputTokenDetails, RequestProfile, Usage, WarmupResponse,
};
pub use service::ResponsesService;
pub use service_error::ResponsesServiceError;
pub use socket::EncodedRequest;
pub use stream::{CodeCall, CodeCallKind, CompactionResult, TurnResult};
pub use telemetry::TRANSPORT;
