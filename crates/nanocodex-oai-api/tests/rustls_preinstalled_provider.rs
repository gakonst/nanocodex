use nanocodex_oai_api::OpenAi;

#[test]
fn standard_session_preserves_host_installed_provider() {
    assert!(
        rustls::crypto::ring::default_provider()
            .install_default()
            .is_ok()
    );
    let installed = rustls::crypto::CryptoProvider::get_default()
        .expect("test should install a Rustls provider");

    let openai = OpenAi::new("test-api-key").unwrap();
    let _session = openai.instructions("Answer concisely.").build().unwrap();

    let preserved = rustls::crypto::CryptoProvider::get_default()
        .expect("host-installed Rustls provider should remain installed");
    assert!(std::sync::Arc::ptr_eq(installed, preserved));
}
