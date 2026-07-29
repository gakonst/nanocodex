//! Anthropic credential resolution.
//!
//! Nanocodex accepts explicit environment credentials or its owned, refreshing OAuth
//! login. Claude Code does not expose a supported command or profile store for another
//! client to borrow its access token, so no CLI credential scraping is attempted.

use super::{
    AnthropicAuth, AnthropicAuthMode, AnthropicOAuthConfig, default_anthropic_auth_file,
    load_stored_anthropic_auth_with_config, stored_anthropic_status,
};

/// Non-secret information about the resolved Anthropic authorization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnthropicAuthStatus {
    /// Selected authentication mode.
    pub mode: AnthropicAuthMode,
    /// Where the credential came from, for display.
    pub source: String,
    /// Account email or identifier for a Nanocodex-owned login.
    pub profile: Option<String>,
    /// Seconds until the access token expires, when the store records an expiry.
    pub expires_in_seconds: Option<i64>,
}

/// Failure while resolving Anthropic credentials.
#[derive(Debug, thiserror::Error)]
pub enum AnthropicSetupError {
    /// No explicit or stored Anthropic credential was found.
    #[error(
        "no Anthropic credentials found. Use one of:\n  \
         export ANTHROPIC_API_KEY=sk-ant-...\n  \
         nanocodex auth login --anthropic   (sign in with an Anthropic subscription)"
    )]
    Missing,
    /// Both mutually exclusive environment credentials were configured.
    #[error(
        "both ANTHROPIC_API_KEY and ANTHROPIC_AUTH_TOKEN are set; the API rejects requests carrying both - unset one"
    )]
    Conflicting,
}

/// Resolves Anthropic credentials from the environment or Nanocodex's login store.
///
/// Resolution order is `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, then the
/// Nanocodex-owned OAuth login.
///
/// # Errors
///
/// Returns an error when no credential source is available, or when both environment
/// credential variables are set at once.
pub async fn load_anthropic_auth() -> Result<AnthropicAuth, AnthropicSetupError> {
    match environment_credential()? {
        Some(EnvironmentCredential::ApiKey(key)) => return Ok(AnthropicAuth::api_key(key)),
        Some(EnvironmentCredential::Token(token)) => return Ok(AnthropicAuth::oauth_token(token)),
        None => {}
    }

    stored_login().ok_or(AnthropicSetupError::Missing)
}

/// Loads a Nanocodex-owned login when one is both configured and present.
fn stored_login() -> Option<AnthropicAuth> {
    let config = AnthropicOAuthConfig::from_env();
    let path = default_anthropic_auth_file()?;
    if !path.is_file() {
        return None;
    }
    match load_stored_anthropic_auth_with_config(&path, config) {
        Ok(auth) => Some(auth),
        Err(error) => {
            tracing::warn!(error = %error, "ignoring unreadable Anthropic login store");
            None
        }
    }
}

/// Inspects the resolved Anthropic authorization without exposing any token.
///
/// # Errors
///
/// Returns an error when no credential source is available.
pub async fn anthropic_auth_status() -> Result<AnthropicAuthStatus, AnthropicSetupError> {
    match environment_credential()? {
        Some(EnvironmentCredential::ApiKey(_)) => {
            return Ok(AnthropicAuthStatus {
                mode: AnthropicAuthMode::ApiKey,
                source: "ANTHROPIC_API_KEY".to_owned(),
                profile: None,
                expires_in_seconds: None,
            });
        }
        Some(EnvironmentCredential::Token(_)) => {
            return Ok(AnthropicAuthStatus {
                mode: AnthropicAuthMode::OAuth,
                source: "ANTHROPIC_AUTH_TOKEN".to_owned(),
                profile: None,
                expires_in_seconds: None,
            });
        }
        None => {}
    }

    if let Some(path) = default_anthropic_auth_file()
        && path.is_file()
        && let Ok(status) = stored_anthropic_status(&path)
    {
        return Ok(AnthropicAuthStatus {
            mode: AnthropicAuthMode::OAuth,
            source: format!("nanocodex login ({})", path.display()),
            profile: status.email.or(status.account_id),
            expires_in_seconds: status.expires_in_seconds,
        });
    }

    Err(AnthropicSetupError::Missing)
}

enum EnvironmentCredential {
    ApiKey(String),
    Token(String),
}

fn environment_credential() -> Result<Option<EnvironmentCredential>, AnthropicSetupError> {
    let api_key = non_empty_env("ANTHROPIC_API_KEY");
    let auth_token = non_empty_env("ANTHROPIC_AUTH_TOKEN");
    if api_key.is_some() && auth_token.is_some() {
        return Err(AnthropicSetupError::Conflicting);
    }
    if let Some(api_key) = api_key {
        return Ok(Some(EnvironmentCredential::ApiKey(api_key)));
    }
    Ok(auth_token.map(EnvironmentCredential::Token))
}

fn non_empty_env(name: &str) -> Option<String> {
    non_empty(std::env::var(name).ok())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::non_empty;

    #[test]
    fn blank_credential_values_are_treated_as_unset() {
        assert!(non_empty(Some("   ".to_owned())).is_none());
        assert!(non_empty(Some(String::new())).is_none());
        assert!(non_empty(None).is_none());
        assert_eq!(
            non_empty(Some("value".to_owned())).as_deref(),
            Some("value")
        );
    }
}
