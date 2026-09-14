use super::*;

fn key() -> String {
    format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43))
}

#[test]
fn origins_are_canonical_and_credentials_cannot_select_insecure_transports() {
    for (input, expected) in [
        ("https://EXAMPLE.com:443/", "https://example.com"),
        ("http://127.0.0.1:3000/", "http://127.0.0.1:3000"),
        ("http://localhost:3000/", "http://localhost:3000"),
        ("http://[::1]:3000", "http://[::1]:3000"),
    ] {
        assert_eq!(canonical_origin(input).unwrap(), expected);
    }
    for input in [
        "http://example.com",
        "https://example.com/path",
        "https://user:password@example.com",
        "https://example.com?key=secret",
        "https://example.com#secret",
        "file:///tmp/key",
        "http://localhost.example.com",
        "",
    ] {
        let error = canonical_origin(input).unwrap_err().to_string();
        assert!(!error.contains(input) || input.is_empty());
    }
}

#[test]
fn phone_normalization_matches_the_native_apps() {
    assert_eq!(
        normalize_phone(" +1 (415) 555-0123 ").unwrap(),
        "+14155550123"
    );
    for value in [
        "4155550123",
        "+0123456789",
        "+123",
        "+1234567890123456",
        "+١٢٣٤٥٦٧٨٩",
    ] {
        assert!(normalize_phone(value).is_err());
    }
}

#[test]
fn service_errors_are_safe_and_rate_limits_are_bounded() {
    let secret = key();
    let error = Error::response(
        500,
        &json!({"error": secret, "message": secret}),
        &reqwest::header::HeaderMap::new(),
    );
    assert!(!format!("{error:?}").contains(&secret));
    let error = Error::response(
        429,
        &json!({"retry_after": 99999}),
        &reqwest::header::HeaderMap::new(),
    );
    assert!(error.to_string().contains("3600 seconds"));
    let error = Error::response(
        400,
        &json!({"error": "invalid_or_expired_otp"}),
        &reqwest::header::HeaderMap::new(),
    );
    assert!(error.retry_code);
}

#[test]
fn account_store_is_private_atomic_and_preserves_other_origins() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("auth/accounts.json");
    let lock = store::lock(&path).unwrap();
    assert!(store::lock(&path).is_err());
    let mut stored = store::load(&path).unwrap();
    for origin in ["https://one.example", "https://two.example"] {
        stored
            .accounts
            .insert(origin.to_owned(), store::Credential { api_key: key() });
    }
    store::save(&path, &stored).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let mut loaded = store::load(&path).unwrap();
    loaded.accounts.remove("https://one.example");
    store::save(&path, &loaded).unwrap();
    let loaded = store::load(&path).unwrap();
    assert_eq!(loaded.accounts.len(), 1);
    assert!(loaded.accounts.contains_key("https://two.example"));
    drop(lock);
    assert!(store::lock(&path).is_ok());
}

#[test]
fn malformed_or_insecure_store_is_not_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("accounts.json");
    let _lock = store::lock(&path).unwrap();
    std::fs::write(&path, "not json").unwrap();
    assert!(store::load(&path).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            store::load(&path)
                .err()
                .unwrap()
                .to_string()
                .contains("malformed")
        );
        let link = directory.path().join("link.json");
        symlink(&path, &link).unwrap();
        assert!(store::load(&link).is_err());
        assert!(store::save(&link, &store::Store::default()).is_err());
    }
}
