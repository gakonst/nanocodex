/// Installs ring as the process-level Rustls provider when the embedding host
/// has not already selected one.
///
/// Nanocodex uses ring throughout its native dependency graph. Calling this
/// before constructing TLS clients also keeps an embedding application's
/// explicit provider choice authoritative.
pub fn install_default_rustls_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        drop(rustls::crypto::ring::default_provider().install_default());
    }
}
