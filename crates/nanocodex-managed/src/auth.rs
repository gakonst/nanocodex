use std::{fmt, str::FromStr};

use zeroize::Zeroize;

use crate::ManagedError;

/// An account API key accepted by the Nanocodex managed service.
///
/// The owned key is zeroized when dropped and is always redacted from debug
/// output. Constructing a key performs syntax validation but does not contact
/// the managed service.
pub struct ManagedApiKey(String);

impl ManagedApiKey {
    /// Parses and validates an ncx_live account API key.
    ///
    /// # Errors
    ///
    /// Returns ManagedError::Configuration when the key does not have the
    /// exact managed account-key shape.
    pub fn parse(value: impl Into<String>) -> Result<Self, ManagedError> {
        let value = value.into();
        if !valid_api_key(&value) {
            return Err(ManagedError::Configuration(
                "managed API key must be an ncx_live account API key".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ManagedApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManagedApiKey([REDACTED])")
    }
}

impl FromStr for ManagedApiKey {
    type Err = ManagedError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value.to_owned())
    }
}

impl Drop for ManagedApiKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

fn valid_api_key(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("ncx_live_") else {
        return false;
    };
    if rest.len() != 12 + 1 + 43 || !rest.is_ascii() {
        return false;
    }
    let (id, separator_and_secret) = rest.split_at(12);
    let Some(secret) = separator_and_secret.strip_prefix('_') else {
        return false;
    };
    id.bytes().all(base64url_byte) && secret.bytes().all(base64url_byte)
}

const fn base64url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use super::{ManagedApiKey, valid_api_key};

    fn key() -> String {
        format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))
    }

    #[test]
    fn validates_account_api_keys_exactly() {
        assert!(valid_api_key(&key()));
        assert!(valid_api_key(&format!(
            "ncx_live_{}_{}",
            "abc_defghijk",
            "b".repeat(43)
        )));
        assert!(!valid_api_key("sk-provider-key"));
        assert!(!valid_api_key(&format!(
            "ncx_live_{}_{}",
            "a".repeat(11),
            "b".repeat(43)
        )));
        assert!(!valid_api_key(&format!(
            "ncx_live_{}_{}",
            "a".repeat(12),
            "b".repeat(44)
        )));
    }

    #[test]
    fn api_key_debug_is_redacted() {
        let secret = key();
        let key = ManagedApiKey::parse(secret.clone()).unwrap();
        let debug = format!("{key:?}");
        assert_eq!(debug, "ManagedApiKey([REDACTED])");
        assert!(!debug.contains(&secret));
    }
}
