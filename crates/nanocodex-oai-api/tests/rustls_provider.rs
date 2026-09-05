use nanocodex_oai_api::OpenAi;

#[test]
fn standard_session_installs_ring_provider() {
    assert!(rustls::crypto::CryptoProvider::get_default().is_none());

    let openai = OpenAi::new("test-api-key").unwrap();
    let _session = openai.instructions("Answer concisely.").build().unwrap();

    let provider = rustls::crypto::CryptoProvider::get_default()
        .expect("standard native session should install a Rustls provider");
    let expected = rustls::crypto::ring::default_provider();
    assert_eq!(provider.cipher_suites, expected.cipher_suites);
    assert_eq!(
        provider
            .signature_verification_algorithms
            .supported_schemes(),
        expected
            .signature_verification_algorithms
            .supported_schemes()
    );

    let _config = rustls::ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
}
