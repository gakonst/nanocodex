//! Stable, recoverable completion diagnostics. Never include submitted values.
use serde::Serialize;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionErrorCode {
    NotChild,
    MissingInstructionRevision,
    InactiveTurn,
    /// Legacy diagnostic retained for native API compatibility. Supersession is now an outcome.
    SteeringInProgress,
    /// Legacy diagnostic retained for native API compatibility; models no longer supply tokens.
    StaleTurnToken,
    AlreadyAccepted,
    SchemaValidation,
    MissingResult,
}

/// `recoverable` means the active turn can correct its submission, not that task
/// execution may be retried. Kept inside `io::Error` for typed native inspection;
/// Display stays readable in user-facing failure cards; native callers can
/// inspect this type or serialize it to retain structured recovery metadata.
#[derive(Clone, Debug, Serialize)]
pub struct CompletionError {
    pub code: CompletionErrorCode,
    pub recoverable: bool,
    pub message: &'static str,
    pub recovery: &'static str,
    /// Legacy native API field; new diagnostics never expose instruction revisions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_turn_token: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

impl CompletionError {
    pub(crate) fn new(
        code: CompletionErrorCode,
        recoverable: bool,
        message: &'static str,
        recovery: &'static str,
    ) -> Self {
        Self {
            code,
            recoverable,
            message,
            recovery,
            current_turn_token: None,
            details: Vec::new(),
        }
    }

    pub(crate) fn with_details(mut self, details: Vec<String>) -> Self {
        // Schema paths can contain caller-supplied property names. Bound bytes,
        // as well as count, without ever echoing rejected instance values.
        self.details = details
            .into_iter()
            .take(4)
            .map(|mut detail| {
                if detail.len() > 512 {
                    let mut end = 512;
                    while !detail.is_char_boundary(end) {
                        end -= 1;
                    }
                    detail.truncate(end);
                }
                detail
            })
            .collect();
        self
    }
}

impl fmt::Display for CompletionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.message, self.recovery)?;
        if let Some(token) = self.current_turn_token {
            write!(formatter, " Current turn_token: {token}.")?;
        }
        for detail in &self.details {
            write!(formatter, " {detail}.")?;
        }
        Ok(())
    }
}

impl std::error::Error for CompletionError {}

impl From<CompletionError> for std::io::Error {
    fn from(error: CompletionError) -> Self {
        Self::new(std::io::ErrorKind::InvalidInput, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_bounds_paths_and_preserves_unicode_and_machine_readable_codes() {
        let error = CompletionError::new(
            CompletionErrorCode::SchemaValidation,
            true,
            "schema mismatch",
            "Correct output.",
        )
        .with_details(vec!["界".repeat(1000); 8]);
        assert_eq!(error.details.len(), 4);
        assert!(error.details.iter().all(|detail| detail.len() <= 512));
        let json = serde_json::to_value(&error).unwrap();
        assert!(
            error
                .to_string()
                .starts_with("schema mismatch Correct output.")
        );
        assert!(!error.to_string().starts_with('{'));
        assert_eq!(json["code"], "schema_validation");
        assert_eq!(json["recoverable"], true);
    }
}
