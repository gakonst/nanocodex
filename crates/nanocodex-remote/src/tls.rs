//! Native trust roots and TLS session state shared across WebSocket connections.
//!
//! Loading the OS certificate store can take hundreds of milliseconds on macOS.
//! Reuse it for at most five minutes, reloading sooner if certificate environment
//! overrides change. Loading runs off the async executor. Failed refreshes fail
//! closed; this cache contains no application credentials or authorization.

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

/// Returns a native-root TLS configuration, sharing session resumption state.
///
/// System root changes are picked up within five minutes on new connections.
/// Changes to `SSL_CERT_FILE` or `SSL_CERT_DIR` invalidate the cache immediately.
/// An explicitly installed Rustls crypto provider remains authoritative.
///
/// # Errors
/// Returns an error if native trust roots cannot be loaded or contain no usable
/// certificates. A failed refresh never falls back to expired trust roots.
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

// HTTP-only loopback publishers also construct reqwest clients. Initialize the
// provider independently of native-root loading and preserve explicit providers.
pub(crate) fn ensure_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

fn load() -> io::Result<ClientConfig> {
    let began = Instant::now();
    ensure_crypto_provider();
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
