//! Native certificate verification and TLS session state shared across connections.
//!
//! On macOS without certificate environment overrides, use the same platform
//! verifier as Reqwest so startup does not enumerate the entire OS root store.
//! Elsewhere, or with explicit overrides, load native roots off the async executor.
//! Subsequent calls refresh cached configurations after five minutes, or sooner if
//! overrides change. Retained configurations and connections are not revoked.
//! Failed refreshes fail closed; this cache contains no application credentials
//! or authorization.

use rustls::{ClientConfig, RootCertStore};
use std::{
    ffi::OsString,
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

const MAX_AGE: Duration = Duration::from_secs(300);
type TrustEnvironment = (Option<OsString>, Option<OsString>);
struct Cached {
    loaded: Instant,
    environment: TrustEnvironment,
    config: Arc<ClientConfig>,
}
impl Cached {
    fn matches(&self, now: Instant, environment: &TrustEnvironment) -> bool {
        now.duration_since(self.loaded) < MAX_AGE && &self.environment == environment
    }
}
static CACHE: Mutex<Option<Cached>> = Mutex::const_new(None);

/// Returns a native-trust TLS configuration, sharing session resumption state.
///
/// Each call reuses a matching configuration for up to five minutes. A subsequent
/// call refreshes it after that interval, or when `SSL_CERT_FILE` or `SSL_CERT_DIR`
/// changes. Files changed at the same override paths are reloaded on expiry.
/// Refreshing creates new session state; retained configurations and established
/// connections remain usable. Resumed sessions do not repeat full chain checks.
///
/// On macOS without certificate environment overrides, full handshakes use
/// Security.framework's SecTrust chain policy through the platform verifier.
/// An explicitly installed Rustls crypto provider is retained for TLS handshake
/// cryptography; SecTrust controls certificate chain validation.
///
/// # Errors
/// Returns an error if the platform verifier cannot be initialized, or native
/// trust roots cannot be loaded or contain no usable certificates. A failed
/// refresh never falls back to an expired configuration. Platform certificate
/// verification errors are reported during the TLS handshake.
pub async fn native_client_config() -> io::Result<Arc<ClientConfig>> {
    let environment = (
        std::env::var_os("SSL_CERT_FILE"),
        std::env::var_os("SSL_CERT_DIR"),
    );
    let mut cache = CACHE.lock().await;
    if let Some(cached) = &*cache
        && cached.matches(Instant::now(), &environment)
    {
        return Ok(cached.config.clone());
    }
    let config = tokio::task::spawn_blocking(load)
        .await
        .map_err(io::Error::other)??;
    let config = Arc::new(config);
    *cache = Some(Cached {
        loaded: Instant::now(),
        environment,
        config: config.clone(),
    });
    Ok(config)
}

fn load() -> io::Result<ClientConfig> {
    let began = Instant::now();
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    #[cfg(target_os = "macos")]
    if std::env::var_os("SSL_CERT_FILE").is_none() && std::env::var_os("SSL_CERT_DIR").is_none() {
        use rustls_platform_verifier::BuilderVerifierExt;

        // This passes the builder's existing CryptoProvider to the verifier.
        // Explicit overrides retain the native-certs replacement-store semantics
        // below; adding them to platform roots would silently broaden trust.
        let config = ClientConfig::builder()
            .with_platform_verifier()
            .map_err(io::Error::other)?
            .with_no_client_auth();
        tracing::info!(target: "nanocodex_tls", stage = "tls.platform_verifier",
            elapsed_ms = began.elapsed().as_secs_f64() * 1000.0);
        return Ok(config);
    }
    let loaded = rustls_native_certs::load_native_certs();
    let mut roots = RootCertStore::empty();
    let (added, rejected) = roots.add_parsable_certificates(loaded.certs);
    tracing::info!(target: "nanocodex_tls", stage = "tls.native_roots", elapsed_ms = began.elapsed().as_secs_f64() * 1000.0,
        certificates = added, rejected, load_errors = loaded.errors.len());
    if roots.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no usable native TLS trust roots",
        ));
    }
    Ok(ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_cache_expires_and_environment_overrides_invalidate_it() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let now = Instant::now();
        let config = Arc::new(
            ClientConfig::builder()
                .with_root_certificates(RootCertStore::empty())
                .with_no_client_auth(),
        );
        let cached = Cached {
            loaded: now,
            environment: (None, None),
            config,
        };
        assert!(cached.matches(now + MAX_AGE - Duration::from_nanos(1), &(None, None)));
        assert!(!cached.matches(now + MAX_AGE, &(None, None)));
        assert!(!cached.matches(now, &(Some("custom.pem".into()), None)));
        assert!(!cached.matches(now, &(None, Some("custom-roots".into()))));
    }
}
