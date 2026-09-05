use web_time::Instant;

use crate::tower::{
    ResponsesAttempt, ResponsesServiceError, ResponsesServiceResponse, service_error::FailurePhase,
};

use super::super::{AttemptGuard, ResponsesService};

pub(crate) async fn run(
    _service: &ResponsesService,
    _connection: &mut AttemptGuard<'_>,
    _request: &ResponsesAttempt,
    _started_at: Instant,
) -> Result<ResponsesServiceResponse, ResponsesServiceError> {
    Err(ResponsesServiceError::invalid_attempt_state(
        "HTTPS Responses transport is unavailable for hosted WebAssembly",
        FailurePhase::Connect,
        0,
    ))
}
