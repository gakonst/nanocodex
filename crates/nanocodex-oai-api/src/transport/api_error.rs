use std::{collections::HashMap, time::Duration};

use serde::Deserialize;

pub(super) fn retryable_api_error(event: &str) -> Option<(&'static str, Option<Duration>)> {
    let event: ApiErrorEnvelope = serde_json::from_str(event).ok()?;
    let error = event.error();
    let code = error.and_then(|error| error.code.as_deref());
    let discriminator = code.or_else(|| error.and_then(|error| error.kind.as_deref()));

    let class = match event.event_type.as_deref() {
        Some("response.incomplete") => "api_incomplete",
        Some("response.failed") => {
            if code.is_some_and(is_terminal_response_failure) {
                return None;
            }
            match discriminator {
                Some("server_is_overloaded" | "slow_down") => "api_overload",
                Some("rate_limit_exceeded") => "api_rate_limit",
                Some("server_error" | "websocket_connection_limit_reached") => "api_server",
                _ => "api_failed",
            }
        }
        _ => match discriminator {
            Some("server_is_overloaded" | "slow_down") => "api_overload",
            Some("server_error" | "websocket_connection_limit_reached") => "api_server",
            Some("rate_limit_exceeded") => "api_rate_limit",
            _ => return None,
        },
    };

    let server_delay = error
        .and_then(|error| error.retry_after)
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .or_else(|| retry_after_header(&event.headers));
    Some((class, server_delay))
}

pub(super) fn api_error_has_code(event: &str, expected: &str) -> bool {
    let Ok(event) = serde_json::from_str::<ApiErrorEnvelope>(event) else {
        return false;
    };
    event.error().and_then(|error| error.code.as_deref()) == Some(expected)
}

pub(super) fn api_error_is_checkpoint_missing(event: &str) -> bool {
    let Ok(event) = serde_json::from_str::<ApiErrorEnvelope>(event) else {
        return false;
    };
    let Some(error) = event.error() else {
        return false;
    };
    if error.code.as_deref() == Some("previous_response_not_found") {
        return true;
    }
    error.kind.as_deref() == Some("invalid_request_error")
        && error
            .message
            .as_deref()
            .is_some_and(|message| message.eq_ignore_ascii_case("Invalid `previous_response_id`."))
}

fn is_terminal_response_failure(code: &str) -> bool {
    matches!(
        code,
        "context_length_exceeded"
            | "insufficient_quota"
            | "usage_not_included"
            | "cyber_policy"
            | "misalignment_policy_violation"
            | "invalid_prompt"
            | "bio_policy"
    )
}

fn retry_after_header(headers: &HashMap<String, RetryAfterValue>) -> Option<Duration> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| value.seconds())
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
}

#[derive(Deserialize)]
struct ApiErrorEnvelope {
    #[serde(default, rename = "type")]
    event_type: Option<Box<str>>,
    #[serde(default)]
    error: Option<ApiErrorDetail>,
    #[serde(default)]
    response: Option<ApiErrorResponse>,
    #[serde(default)]
    headers: HashMap<String, RetryAfterValue>,
}

impl ApiErrorEnvelope {
    fn error(&self) -> Option<&ApiErrorDetail> {
        self.error
            .as_ref()
            .or_else(|| self.response.as_ref()?.error.as_ref())
    }
}

#[derive(Deserialize)]
struct ApiErrorResponse {
    #[serde(default)]
    error: Option<ApiErrorDetail>,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    #[serde(default, rename = "type")]
    kind: Option<Box<str>>,
    #[serde(default)]
    code: Option<Box<str>>,
    #[serde(default)]
    message: Option<Box<str>>,
    #[serde(default)]
    retry_after: Option<f64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RetryAfterValue {
    Number(f64),
    String(Box<str>),
}

impl RetryAfterValue {
    fn seconds(&self) -> Option<f64> {
        match self {
            Self::Number(seconds) => Some(*seconds),
            Self::String(seconds) => seconds.parse().ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{api_error_is_checkpoint_missing, retryable_api_error};

    #[test]
    fn recognizes_both_checkpoint_missing_error_shapes() {
        let coded = r#"{
            "type": "error",
            "error": {
                "code": "previous_response_not_found",
                "message": "checkpoint expired"
            }
        }"#;
        let current = r#"{
            "type": "error",
            "status": 400,
            "error": {
                "type": "invalid_request_error",
                "message": "Invalid `previous_response_id`."
            }
        }"#;

        assert!(api_error_is_checkpoint_missing(coded));
        assert!(api_error_is_checkpoint_missing(current));
        assert!(!api_error_is_checkpoint_missing(
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"Invalid model."}}"#
        ));
    }

    #[test]
    fn retries_incomplete_responses() {
        let event = r#"{
            "type": "response.incomplete",
            "response": {
                "status": "incomplete",
                "incomplete_details": { "reason": "max_output_tokens" }
            },
            "headers": { "Retry-After": "1.25" }
        }"#;

        assert_eq!(
            retryable_api_error(event),
            Some(("api_incomplete", Some(Duration::from_millis(1_250))))
        );
    }

    #[test]
    fn retries_unknown_and_missing_failed_response_errors() {
        let unknown = r#"{
            "type": "response.failed",
            "response": {
                "error": {
                    "code": "new_provider_failure",
                    "message": "temporary failure"
                }
            }
        }"#;
        let missing = r#"{
            "type": "response.failed",
            "response": { "status": "failed", "error": null }
        }"#;

        assert_eq!(
            retryable_api_error(unknown).map(|(class, _)| class),
            Some("api_failed")
        );
        assert_eq!(
            retryable_api_error(missing).map(|(class, _)| class),
            Some("api_failed")
        );
    }

    #[test]
    fn overload_failures_are_retryable_and_retain_server_delay() {
        for code in ["server_is_overloaded", "slow_down"] {
            for discriminator in ["code", "type"] {
                let failed = format!(
                    r#"{{
                        "type": "response.failed",
                        "response": {{
                            "error": {{ "{discriminator}": "{code}", "retry_after": 1.25 }}
                        }}
                    }}"#
                );
                let error = format!(
                    r#"{{
                        "type": "error",
                        "error": {{ "{discriminator}": "{code}" }},
                        "headers": {{ "Retry-After": "2.5" }}
                    }}"#
                );
                assert_eq!(
                    retryable_api_error(&failed),
                    Some(("api_overload", Some(Duration::from_millis(1_250)))),
                    "failed: {discriminator}={code}"
                );
                assert_eq!(
                    retryable_api_error(&error),
                    Some(("api_overload", Some(Duration::from_millis(2_500)))),
                    "error: {discriminator}={code}"
                );
            }
        }
    }

    #[test]
    fn known_response_failures_remain_terminal() {
        for code in [
            "context_length_exceeded",
            "insufficient_quota",
            "usage_not_included",
            "cyber_policy",
            "misalignment_policy_violation",
            "invalid_prompt",
            "bio_policy",
        ] {
            let event = format!(
                r#"{{
                    "type": "response.failed",
                    "response": {{ "error": {{ "code": "{code}" }} }}
                }}"#
            );
            assert_eq!(retryable_api_error(&event), None, "{code}");
        }
    }

    #[test]
    fn top_level_unknown_errors_remain_terminal() {
        let event = r#"{
            "type": "error",
            "error": {
                "code": "invalid_request_error",
                "message": "reject this logical turn"
            }
        }"#;

        assert_eq!(retryable_api_error(event), None);
    }
}
