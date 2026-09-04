use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use eyre::{Result, eyre};
use nanocodex_tools::{
    ToolContext, Tools,
    contract::{DEFAULT_TOOL_OUTPUT_TOKENS, ToolOutputBody, ToolOutputContent},
    runtime::ToolRuntime,
};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::{
    BraveSession, Browser, BrowserAction, BrowserActionName, BrowserActionResult,
    BrowserAfterAction, BrowserBuildError, BrowserClickOptions, BrowserColorScheme, BrowserContext,
    BrowserCookie, BrowserCruxClient, BrowserCruxScope, BrowserDevicePreset,
    BrowserDocumentReadyState, BrowserEgressPolicy, BrowserError, BrowserIosConfig,
    BrowserIosDeviceSelector, BrowserKeyModifier, BrowserLighthouseCategory,
    BrowserLighthouseFormFactor, BrowserLoadState, BrowserNetworkBodyKind, BrowserNetworkContext,
    BrowserOrientation, BrowserOriginStorage, BrowserPasskeyMode, BrowserPerformanceInsight,
    BrowserPostActionSnapshot, BrowserPseudoClass, BrowserReactEventKind, BrowserReducedMotion,
    BrowserRouteHeader, BrowserRouteResponse, BrowserStorageState, BrowserTarget, BrowserTool,
    BrowserViewport, BrowserWaitForSelectorState, HostPasskeyAuthenticator, IosBrowser,
    ReactDiagnostics, VirtualAuthenticator, browser_tool_builder,
};

#[test]
fn browser_tool_enables_virtual_platform_passkeys() {
    assert_eq!(
        browser_tool_builder().virtual_authenticator,
        Some(VirtualAuthenticator::platform_passkey())
    );
}

#[test]
fn host_and_virtual_passkey_policies_are_mutually_exclusive() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let executable = directory.path().join("desktop-browser");
    std::fs::write(&executable, [])?;
    let error = Browser::builder()
        .virtual_authenticator(VirtualAuthenticator::platform_passkey())
        .host_passkey_authenticator(HostPasskeyAuthenticator::new(executable))
        .build()
        .err()
        .ok_or_else(|| eyre!("expected mutually exclusive passkey policies"))?;
    assert!(matches!(
        error,
        BrowserBuildError::Configuration { ref message }
            if message == "host and virtual passkey authenticators cannot be enabled together"
    ));
    Ok(())
}

#[test]
fn remote_browser_accepts_cookie_only_brave_sessions() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let executable = directory.path().join("brave");
    std::fs::write(&executable, [])?;
    let user_data = directory.path().join("user-data");
    std::fs::create_dir(&user_data)?;
    let brave = BraveSession::new(executable, user_data)
        .allow_origin(url::Url::parse("https://console.example.com")?);

    Browser::builder()
        .cdp_endpoint(url::Url::parse("ws://127.0.0.1:9222")?)
        .brave_session(brave)
        .build()?;
    Ok(())
}

#[test]
fn caller_owned_browser_profile_is_created_and_not_deleted() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let profile = directory.path().join("durable-profile");
    let browser = Browser::builder().persistent_profile(&profile).build()?;

    assert!(profile.is_dir());
    drop(browser);
    assert!(profile.is_dir());
    Ok(())
}

#[test]
fn remote_browser_rejects_a_local_persistent_profile() -> Result<()> {
    let directory = tempfile::tempdir()?;
    assert!(matches!(
        Browser::builder()
            .cdp_endpoint(url::Url::parse("ws://127.0.0.1:9222")?)
            .persistent_profile(directory.path())
            .build(),
        Err(BrowserBuildError::Configuration { .. })
    ));
    Ok(())
}

#[test]
fn browser_accepts_an_explicit_all_cookie_brave_session() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let executable = directory.path().join("brave");
    std::fs::write(&executable, [])?;
    let user_data = directory.path().join("user-data");
    std::fs::create_dir(&user_data)?;
    let brave = BraveSession::new(executable, user_data).copy_all_cookies();

    Browser::builder().brave_session(brave).build()?;
    Ok(())
}

#[test]
fn browser_cookie_source_is_independent_from_the_browser_executable() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let brave_executable = directory.path().join("brave");
    let chromium_executable = directory.path().join("chromium");
    std::fs::write(&brave_executable, [])?;
    std::fs::write(&chromium_executable, [])?;
    let user_data = directory.path().join("user-data");
    std::fs::create_dir(&user_data)?;
    let cookies = BraveSession::new(brave_executable, user_data).copy_all_cookies();

    Browser::builder()
        .executable(chromium_executable)
        .cookie_source(cookies)
        .build()?;
    Ok(())
}

#[test]
fn harness_owned_browser_secrets_are_redacted_from_debug_output() {
    let state = BrowserStorageState {
        cookies: vec![BrowserCookie {
            name: "session".to_owned(),
            value: "cookie-secret".to_owned(),
            domain: "example.com".to_owned(),
            path: "/".to_owned(),
            expires_epoch_seconds: None,
            http_only: true,
            secure: true,
            same_site: None,
        }],
        origins: vec![BrowserOriginStorage {
            origin: "https://example.com".to_owned(),
            local_storage: BTreeMap::from([(
                "session".to_owned(),
                "local-storage-secret".to_owned(),
            )]),
            session_storage: BTreeMap::new(),
        }],
    };
    let builder = Browser::builder()
        .storage_state(state.clone())
        .crux_client(BrowserCruxClient::new("crux-secret"));

    let state_debug = format!("{state:?}");
    assert!(!state_debug.contains("cookie-secret"));
    assert!(!state_debug.contains("local-storage-secret"));
    assert!(state_debug.contains("cookie_count"));
    let builder_debug = format!("{builder:?}");
    assert!(!builder_debug.contains("crux-secret"));
    assert!(builder_debug.contains("[configured]"));
}

#[test]
fn remote_browser_rejects_unportable_site_data() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let executable = directory.path().join("brave");
    std::fs::write(&executable, [])?;
    let user_data = directory.path().join("user-data");
    std::fs::create_dir(&user_data)?;
    let brave = BraveSession::new(executable, user_data)
        .allow_origin(url::Url::parse("https://console.example.com")?)
        .include_site_data();

    assert!(matches!(
        Browser::builder()
            .cdp_endpoint(url::Url::parse("ws://127.0.0.1:9222")?)
            .brave_session(brave)
            .build(),
        Err(BrowserBuildError::Configuration { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn virtual_credentials_require_the_first_navigation() -> Result<()> {
    let browser = Browser::builder()
        .virtual_authenticator(VirtualAuthenticator::platform_passkey())
        .build()?;

    assert!(matches!(
        browser.virtual_credentials().await,
        Err(BrowserError::VirtualAuthenticatorNotReady)
    ));
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn streaming_open_returns_before_eof_and_reuses_the_same_target() -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut request = [0_u8; 4096];
        let bytes_read = stream.read(&mut request).await?;
        assert!(bytes_read > 0);
        let body = b"<html><body><main>streaming document is usable</main>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n{:X}\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).await?;
        stream.write_all(body).await?;
        stream.write_all(b"\r\n").await?;
        stream.flush().await?;
        std::future::pending::<()>().await;
        Ok::<(), std::io::Error>(())
    });
    let browser = Browser::new()?;
    browser.start().await?;
    let target_id = active_tab_id(&browser).await?;
    let url = format!("http://127.0.0.1:{}/", address.port());

    let started = Instant::now();
    browser.execute(BrowserAction::Open { url }).await?;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "streaming navigation waited for the load deadline: {:?}",
        started.elapsed()
    );
    let content = browser
        .execute(BrowserAction::Evaluate {
            expression: "document.body.textContent".to_owned(),
        })
        .await?;
    assert!(matches!(
        content,
        BrowserActionResult::Evaluation { ref value, .. }
            if value.as_str().is_some_and(|text| text.contains("streaming document is usable"))
    ));

    browser
        .execute(BrowserAction::Open {
            url: "data:text/html,next%20navigation%20works".to_owned(),
        })
        .await?;
    assert_eq!(active_tab_id(&browser).await?, target_id);
    browser.close().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn same_document_fragment_navigation_preserves_the_document_and_target() -> Result<()> {
    let browser = Browser::new()?;
    browser.start().await?;
    let base = "data:text/html,<title>fragment</title><script>window.__fragmentToken='same-document'</script>";
    browser
        .execute(BrowserAction::Open {
            url: base.to_owned(),
        })
        .await?;
    let target_id = active_tab_id(&browser).await?;

    browser
        .execute(BrowserAction::Open {
            url: format!("{base}#section"),
        })
        .await?;

    assert_eq!(active_tab_id(&browser).await?, target_id);
    assert_eq!(
        evaluate_string(&browser, "`${location.hash}:${window.__fragmentToken}`").await?,
        "#section:same-document"
    );
    browser.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn streaming_reload_returns_before_eof() -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let mut retained_streams = Vec::new();
        for body in [
            b"<html><body><main>first streaming document</main>".as_slice(),
            b"<html><body><main>reloaded streaming document</main>".as_slice(),
        ] {
            let (mut stream, _) = listener.accept().await?;
            let mut request = [0_u8; 4096];
            let bytes_read = stream.read(&mut request).await?;
            assert!(bytes_read > 0);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n{:X}\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).await?;
            stream.write_all(body).await?;
            stream.write_all(b"\r\n").await?;
            stream.flush().await?;
            retained_streams.push(stream);
        }
        std::future::pending::<()>().await;
        Ok::<(), std::io::Error>(())
    });
    let browser = Browser::new()?;
    browser.start().await?;
    let url = format!("http://127.0.0.1:{}/", address.port());
    browser.execute(BrowserAction::Open { url }).await?;

    let started = Instant::now();
    browser.execute(BrowserAction::Reload).await?;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "streaming reload waited for the load deadline: {:?}",
        started.elapsed()
    );
    assert!(
        evaluate_string(&browser, "document.body.textContent")
            .await?
            .contains("reloaded streaming document")
    );
    browser.close().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn ordinary_navigation_failure_keeps_the_target_reusable() -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut request = [0_u8; 4096];
        let bytes_read = stream.read(&mut request).await?;
        assert!(bytes_read > 0);
        drop(stream);
        Ok::<(), std::io::Error>(())
    });
    let browser = Browser::new()?;
    browser.start().await?;
    let target_id = active_tab_id(&browser).await?;

    let error = browser
        .execute(BrowserAction::Open {
            url: format!("http://127.0.0.1:{}/", address.port()),
        })
        .await
        .expect_err("an empty HTTP response must fail navigation");
    assert!(
        matches!(
            error,
            BrowserError::NavigationFailed { ref message, .. }
                if message.contains("net::ERR_")
        ),
        "unexpected navigation error: {error:?}"
    );
    browser
        .execute(BrowserAction::Open {
            url: "data:text/html,<title>reused</title>navigation%20reused".to_owned(),
        })
        .await?;
    assert_eq!(active_tab_id(&browser).await?, target_id);
    assert_eq!(evaluate_string(&browser, "document.title").await?, "reused");

    browser.close().await?;
    server.await??;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn navigation_timeout_stops_a_late_commit_and_keeps_the_target_reusable() -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut request = [0_u8; 4096];
        let bytes_read = stream.read(&mut request).await?;
        assert!(bytes_read > 0);
        tokio::time::sleep(Duration::from_secs(6)).await;
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 44\r\nConnection: close\r\n\r\n<title>late</title>late navigation committed";
        let _ = stream.write_all(response).await;
        let _ = stream.flush().await;
        Ok::<(), std::io::Error>(())
    });
    let browser = Browser::new()?;
    browser.start().await?;
    let target_id = active_tab_id(&browser).await?;

    let started = Instant::now();
    let error = browser
        .execute(BrowserAction::Open {
            url: format!("http://127.0.0.1:{}/", address.port()),
        })
        .await
        .expect_err("a response withheld past the navigation deadline must time out");
    assert!(
        matches!(
            error,
            BrowserError::NavigationTimeout {
                milliseconds: 5_000,
                ..
            }
        ),
        "unexpected navigation error: {error:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(7));

    browser
        .execute(BrowserAction::Open {
            url: "data:text/html,<title>replacement</title>replacement%20owns%20the%20target"
                .to_owned(),
        })
        .await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(active_tab_id(&browser).await?, target_id);
    assert_eq!(
        evaluate_string(&browser, "document.title").await?,
        "replacement"
    );

    browser.close().await?;
    server.await??;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn managed_navigation_routes_open_and_reload_to_the_selected_target() -> Result<()> {
    let browser = Browser::new()?;
    browser.start().await?;
    browser
        .execute(BrowserAction::Open {
            url: "data:text/html,<title>first-base</title>".to_owned(),
        })
        .await?;
    let first_target = active_tab_id(&browser).await?;
    browser
        .execute(BrowserAction::NewTab {
            url: Some("data:text/html,<title>second-base</title>".to_owned()),
        })
        .await?;
    let second_target = active_tab_id(&browser).await?;
    assert_ne!(first_target, second_target);

    browser
        .execute(BrowserAction::SelectTab {
            tab_id: first_target.clone(),
        })
        .await?;
    browser
        .execute(BrowserAction::Open {
            url: "data:text/html,<title>first-routed</title>".to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::SelectTab {
            tab_id: second_target.clone(),
        })
        .await?;
    browser
        .execute(BrowserAction::Open {
            url: "data:text/html,<title>second-routed</title>".to_owned(),
        })
        .await?;
    browser.execute(BrowserAction::Reload).await?;
    assert_eq!(
        evaluate_string(&browser, "document.title").await?,
        "second-routed"
    );
    browser
        .execute(BrowserAction::SelectTab {
            tab_id: first_target,
        })
        .await?;
    assert_eq!(
        evaluate_string(&browser, "document.title").await?,
        "first-routed"
    );

    browser.close().await?;
    Ok(())
}

async fn active_tab_id(browser: &Browser) -> Result<String> {
    let BrowserActionResult::Tabs { tabs, .. } = browser.execute(BrowserAction::ListTabs).await?
    else {
        return Err(eyre!("expected browser tabs"));
    };
    tabs.into_iter()
        .find(|tab| tab.active)
        .map(|tab| tab.tab_id)
        .ok_or_else(|| eyre!("missing active browser tab"))
}

async fn evaluate_string(browser: &Browser, expression: &str) -> Result<String> {
    let BrowserActionResult::Evaluation { value, .. } = browser
        .execute(BrowserAction::Evaluate {
            expression: expression.to_owned(),
        })
        .await?
    else {
        return Err(eyre!("expected browser evaluation"));
    };
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| eyre!("evaluation did not return a string: {value}"))
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn persistent_profile_keeps_auth_and_origin_storage_across_browser_tools() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let profile = directory.path().join("browser-profile");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(serve_persistent_profile_fixture(listener));
    let origin = format!("http://127.0.0.1:{}", address.port());

    let first = Browser::builder().persistent_profile(&profile).build()?;
    first
        .execute(BrowserAction::Open {
            url: format!("{origin}/login"),
        })
        .await?;
    first
        .execute(BrowserAction::Evaluate {
            expression: "localStorage.setItem('durable-login', 'present'); true".to_owned(),
        })
        .await?;

    let competing = Browser::builder().persistent_profile(&profile).build()?;
    assert!(matches!(
        competing
            .execute(BrowserAction::Open {
                url: format!("{origin}/check"),
            })
            .await,
        Err(BrowserError::PersistentProfileInUse)
    ));
    competing.close().await?;
    first.close().await?;

    let second = Browser::builder().persistent_profile(&profile).build()?;
    second
        .execute(BrowserAction::Open {
            url: format!("{origin}/check"),
        })
        .await?;
    let authenticated = second
        .execute(BrowserAction::Evaluate {
            expression: r#"({
  response: document.body.textContent,
  storage: localStorage.getItem('durable-login'),
  visibleCookies: document.cookie
})"#
            .to_owned(),
        })
        .await?;
    assert!(matches!(
        authenticated,
        BrowserActionResult::Evaluation { value, .. }
            if value["response"] == "authenticated"
                && value["storage"] == "present"
                && value["visibleCookies"] == ""
    ));

    second
        .execute(BrowserAction::Open {
            url: format!("{origin}/logout"),
        })
        .await?;
    second
        .execute(BrowserAction::Evaluate {
            expression: "localStorage.removeItem('durable-login'); true".to_owned(),
        })
        .await?;
    second.close().await?;

    let third = Browser::builder().persistent_profile(&profile).build()?;
    third
        .execute(BrowserAction::Open {
            url: format!("{origin}/check"),
        })
        .await?;
    let signed_out = third
        .execute(BrowserAction::Evaluate {
            expression: r#"({
  response: document.body.textContent,
  storage: localStorage.getItem('durable-login')
})"#
            .to_owned(),
        })
        .await?;
    assert!(matches!(
        signed_out,
        BrowserActionResult::Evaluation { value, .. }
            if value["response"] == "signed-out" && value["storage"].is_null()
    ));
    third.close().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn virtual_passkeys_persist_across_browser_sessions() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let credential_store = directory.path().join("browser/passkeys.json");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(serve_passkey_fixture(listener));
    let url = format!("http://localhost:{}/", address.port());

    let first = Browser::builder()
        .virtual_authenticator(
            VirtualAuthenticator::platform_passkey().credential_store(credential_store.clone()),
        )
        .build()?;
    first
        .execute(BrowserAction::Open { url: url.clone() })
        .await?;
    first
        .execute(BrowserAction::Click {
            target: BrowserTarget::role("button").named("Register passkey"),
            options: None,
        })
        .await?;
    let registration_wait = first
        .execute(BrowserAction::WaitForText {
            text: "registered".to_owned(),
            target: Some(BrowserTarget::css("#status")),
            hidden: false,
        })
        .await;
    if let Err(error) = registration_wait {
        let status = first
            .execute(BrowserAction::Evaluate {
                expression: "document.querySelector('#status').textContent".to_owned(),
            })
            .await?;
        return Err(eyre!(
            "passkey registration failed with {status:?}: {error}"
        ));
    }
    let registered = first.virtual_credentials().await?;
    assert_eq!(registered.len(), 1);
    let credential_id = registered[0].credential_id.clone();
    let registration_count = registered[0].sign_count;
    first.close().await?;
    assert!(credential_store.is_file());

    let second = Browser::builder()
        .virtual_authenticator(
            VirtualAuthenticator::platform_passkey().credential_store(credential_store),
        )
        .build()?;
    second.execute(BrowserAction::Open { url }).await?;
    let restored = second.virtual_credentials().await?;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].credential_id, credential_id);
    let listed = second.execute(BrowserAction::Passkeys).await?;
    assert!(matches!(
        listed,
        BrowserActionResult::Passkeys {
            mode: BrowserPasskeyMode::Auto,
            ref credentials,
            ..
        } if credentials.len() == 1 && credentials[0].credential_id == credential_id
    ));
    let selected = second
        .execute(BrowserAction::PasskeyUse {
            credential_id: credential_id.clone(),
            relying_party_id: restored[0].relying_party_id.clone(),
        })
        .await?;
    assert!(matches!(
        selected,
        BrowserActionResult::Passkeys {
            mode: BrowserPasskeyMode::Use { ref credential_id, .. },
            ..
        } if credential_id == &registered[0].credential_id
    ));
    second
        .execute(BrowserAction::Click {
            target: BrowserTarget::role("button").named("Authenticate passkey"),
            options: None,
        })
        .await?;
    let authentication_wait = second
        .execute(BrowserAction::WaitForText {
            text: "authenticated".to_owned(),
            target: Some(BrowserTarget::css("#status")),
            hidden: false,
        })
        .await;
    if let Err(error) = authentication_wait {
        let status = second
            .execute(BrowserAction::Evaluate {
                expression: "document.querySelector('#status').textContent".to_owned(),
            })
            .await?;
        return Err(eyre!(
            "passkey authentication failed with {status:?}: {error}"
        ));
    }
    let authenticated = second.virtual_credentials().await?;
    assert_eq!(authenticated.len(), 1);
    assert_eq!(authenticated[0].credential_id, credential_id);
    assert!(authenticated[0].sign_count > registration_count);
    let fresh = second.execute(BrowserAction::PasskeyNew).await?;
    assert!(matches!(
        fresh,
        BrowserActionResult::Passkeys {
            mode: BrowserPasskeyMode::New,
            ref credentials,
            ..
        } if credentials.len() == 1 && credentials[0].credential_id == credential_id
    ));
    let automatic = second.execute(BrowserAction::PasskeyAuto).await?;
    assert!(matches!(
        automatic,
        BrowserActionResult::Passkeys {
            mode: BrowserPasskeyMode::Auto,
            ref credentials,
            ..
        } if credentials.len() == 1 && credentials[0].credential_id == credential_id
    ));
    second.close().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn virtual_authenticator_is_reused_when_returning_to_a_tab() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let credential_store = directory.path().join("browser/passkeys.json");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(serve_passkey_fixture(listener));
    let url = format!("http://localhost:{}/", address.port());
    let browser = Browser::builder()
        .virtual_authenticator(
            VirtualAuthenticator::platform_passkey().credential_store(credential_store),
        )
        .build()?;

    browser
        .execute(BrowserAction::Open { url: url.clone() })
        .await?;
    let BrowserActionResult::Tabs { tabs, .. } = browser.execute(BrowserAction::ListTabs).await?
    else {
        return Err(eyre!("expected browser tabs"));
    };
    let original_tab = tabs
        .into_iter()
        .find(|tab| tab.active)
        .ok_or_else(|| eyre!("missing active browser tab"))?
        .tab_id;
    browser
        .execute(BrowserAction::NewTab { url: Some(url) })
        .await?;
    browser
        .execute(BrowserAction::SelectTab {
            tab_id: original_tab,
        })
        .await?;
    browser.execute(BrowserAction::Passkeys).await?;
    let BrowserActionResult::Tabs { tabs, .. } = browser.execute(BrowserAction::ListTabs).await?
    else {
        return Err(eyre!("expected browser tabs"));
    };
    let inactive_tab = tabs
        .into_iter()
        .find(|tab| !tab.active)
        .ok_or_else(|| eyre!("missing inactive browser tab"))?
        .tab_id;
    browser
        .execute(BrowserAction::CloseTab {
            tab_id: inactive_tab,
        })
        .await?;
    browser.execute(BrowserAction::Passkeys).await?;

    browser.close().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn virtual_authenticator_reaches_a_window_open_popup() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(serve_passkey_fixture(listener));
    let url = format!("http://localhost:{}/", address.port());
    let browser = Browser::builder()
        .virtual_authenticator(
            VirtualAuthenticator::platform_passkey()
                .credential_store(directory.path().join("passkeys.json")),
        )
        .build()?;

    browser
        .execute(BrowserAction::Open { url: url.clone() })
        .await?;
    let BrowserActionResult::Tabs { tabs, .. } = browser.execute(BrowserAction::ListTabs).await?
    else {
        return Err(eyre!("expected browser tabs"));
    };
    let opener = tabs
        .into_iter()
        .find(|tab| tab.active)
        .ok_or_else(|| eyre!("missing active opener tab"))?
        .tab_id;
    browser
        .execute(BrowserAction::Evaluate {
            expression: "window.open(location.href, 'passkey-popup', 'popup=yes,width=440,height=720') !== null".to_owned(),
        })
        .await?;
    let mut popup = None;
    for _ in 0..100 {
        let BrowserActionResult::Tabs { tabs, .. } =
            browser.execute(BrowserAction::ListTabs).await?
        else {
            return Err(eyre!("expected browser tabs"));
        };
        if let Some(tab) = tabs
            .into_iter()
            .find(|tab| tab.tab_id != opener && tab.url == url)
        {
            popup = Some(tab.tab_id);
            break;
        }
        browser
            .execute(BrowserAction::WaitForTimeout { milliseconds: 50 })
            .await?;
    }
    let popup = popup.ok_or_else(|| eyre!("window.open popup was not discovered"))?;
    browser
        .execute(BrowserAction::SelectTab { tab_id: popup })
        .await?;
    browser.execute(BrowserAction::PasskeyNew).await?;
    browser
        .execute(BrowserAction::Click {
            target: BrowserTarget::role("button").named("Register passkey"),
            options: None,
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForText {
            text: "registered".to_owned(),
            target: Some(BrowserTarget::css("#status")),
            hidden: false,
        })
        .await?;
    assert_eq!(browser.virtual_credentials().await?.len(), 1);

    browser.close().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn code_mode_calls_record_browser_actions_in_order() -> Result<()> {
    let (browser, recording) = BrowserTool::recording();
    let tools = Tools::builder().without_defaults().tool(browser).build()?;
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);

    let execution = runtime
        .execute_code(
            r#"
const opened = await tools.browser({
  action: "open",
  url: "https://example.com"
});
const snapshot = await tools.browser({
  action: "snapshot"
});
const clicked = await tools.browser({
  action: "click",
  target: { by: "ref", reference: "@e1" }
});
const html = await tools.browser({
  action: "get_html",
  target: { by: "css", selector: "main" }
});
const elementContext = await tools.browser({
  action: "element_context",
  target: { by: "css", selector: "main" }
});
text({ opened, snapshot, clicked, html, elementContext });
"#,
            context(),
        )
        .await;

    let calls = execution
        .nested_calls
        .iter()
        .map(|call| {
            serde_json::json!({
                "input": call.input,
                "output": call.output,
                "success": call.success,
            })
        })
        .collect::<Vec<_>>();
    assert!(
        execution.success,
        "nested calls: {}",
        serde_json::to_string_pretty(&calls)?
    );
    assert_eq!(execution.nested_calls.len(), 5);
    let output = execution_text(&execution.output)?;
    let output: Value = serde_json::from_str(output)?;
    assert_eq!(output["opened"]["sequence"], 0);
    assert_eq!(output["opened"]["result"], "action");
    assert_eq!(output["opened"]["action"], "open");
    assert_eq!(output["opened"]["executed"], false);
    assert_eq!(output["snapshot"]["sequence"], 1);
    assert_eq!(output["snapshot"]["result"], "snapshot");
    assert_eq!(output["snapshot"]["origin"], "https://example.com");
    assert_eq!(output["snapshot"]["snapshot"], "");
    assert!(
        output["snapshot"]["refs"]
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
    );
    assert_eq!(output["clicked"]["sequence"], 2);
    assert_eq!(output["html"]["sequence"], 3);
    assert_eq!(output["html"]["result"], "html");
    assert_eq!(output["elementContext"]["sequence"], 4);
    assert_eq!(output["elementContext"]["result"], "element_context");
    assert_eq!(output["elementContext"]["context"]["selector"], "main");

    let actions = recording.actions()?;
    assert_eq!(actions.len(), 5);
    assert_eq!(
        actions[0].action,
        BrowserAction::Open {
            url: "https://example.com".to_owned(),
        }
    );
    assert_eq!(
        actions[1].action,
        BrowserAction::Snapshot {
            interactive: true,
            compact: false,
            depth: None,
            selector: None,
            include_urls: false,
        }
    );
    assert_eq!(actions[2].action.name(), BrowserActionName::Click);
    assert_eq!(actions[3].action.name(), BrowserActionName::GetHtml);
    assert_eq!(
        actions[4].action,
        BrowserAction::ElementContext {
            target: BrowserTarget::css("main"),
        }
    );
    Ok(())
}

#[test]
fn recording_browser_exposes_extension_lifecycle_actions() -> Result<()> {
    let (_browser, recording) = BrowserTool::recording();

    let loaded = recording.record(BrowserAction::LoadExtension {
        path: "extensions/chrome/.output/chrome-mv3".into(),
    })?;
    let triggered = recording.record(BrowserAction::TriggerExtensionAction {
        extension_id: "abcdefghijklmnop".to_owned(),
        tab_id: Some("tab-1".to_owned()),
    })?;

    assert!(matches!(
        loaded,
        BrowserActionResult::Extension {
            extension_id,
            executed: false,
            ..
        } if extension_id.is_empty()
    ));
    assert!(matches!(
        triggered,
        BrowserActionResult::Action {
            action: BrowserActionName::TriggerExtensionAction,
            executed: false,
            ..
        }
    ));
    assert_eq!(recording.actions()?.len(), 2);
    Ok(())
}

#[test]
fn recording_browser_exposes_model_controlled_passkey_modes() -> Result<()> {
    let (_browser, recording) = BrowserTool::recording();

    let listed = recording.record(BrowserAction::Passkeys)?;
    let selected = recording.record(BrowserAction::PasskeyUse {
        credential_id: "credential-id".to_owned(),
        relying_party_id: Some("wallet.example".to_owned()),
    })?;
    let fresh = recording.record(BrowserAction::PasskeyNew)?;
    let automatic = recording.record(BrowserAction::PasskeyAuto)?;
    let host_started = recording.record(BrowserAction::HostPasskeyStart)?;
    let host_resumed = recording.record(BrowserAction::HostPasskeyResume)?;

    assert!(matches!(
        listed,
        BrowserActionResult::Passkeys {
            action: BrowserActionName::Passkeys,
            mode: BrowserPasskeyMode::Auto,
            ..
        }
    ));
    assert!(matches!(
        selected,
        BrowserActionResult::Passkeys {
            action: BrowserActionName::PasskeyUse,
            mode: BrowserPasskeyMode::Use {
                credential_id,
                relying_party_id: Some(relying_party_id),
            },
            ..
        } if credential_id == "credential-id" && relying_party_id == "wallet.example"
    ));
    assert!(matches!(
        fresh,
        BrowserActionResult::Passkeys {
            action: BrowserActionName::PasskeyNew,
            mode: BrowserPasskeyMode::New,
            ..
        }
    ));
    assert!(matches!(
        automatic,
        BrowserActionResult::Passkeys {
            action: BrowserActionName::PasskeyAuto,
            mode: BrowserPasskeyMode::Auto,
            ..
        }
    ));
    assert!(matches!(
        host_started,
        BrowserActionResult::Action {
            action: BrowserActionName::HostPasskeyStart,
            ..
        }
    ));
    assert!(matches!(
        host_resumed,
        BrowserActionResult::Action {
            action: BrowserActionName::HostPasskeyResume,
            ..
        }
    ));
    Ok(())
}

#[test]
fn mobile_device_profiles_are_pinned_and_orientation_aware() {
    let portrait = BrowserDevicePreset::Iphone15Pro.descriptor(BrowserOrientation::Portrait);
    let landscape = BrowserDevicePreset::Iphone15Pro.descriptor(BrowserOrientation::Landscape);

    assert_eq!((portrait.width, portrait.height), (393, 852));
    assert_eq!((landscape.width, landscape.height), (852, 393));
    assert_eq!(portrait.device_scale_factor, 3.0);
    assert!(portrait.mobile && portrait.touch);
    assert_eq!(portrait.max_touch_points, 5);
    assert_eq!(portrait.platform, "iPhone");
}

#[test]
fn recording_browser_exposes_mobile_state_and_audit_contracts() -> Result<()> {
    let (_browser, recording) = BrowserTool::recording();

    let configured = recording.record(BrowserAction::SetDevice {
        device: BrowserDevicePreset::Pixel8,
        orientation: BrowserOrientation::Landscape,
    })?;
    let state = recording.record(BrowserAction::MobileState)?;
    let audit = recording.record(BrowserAction::MobileAudit {
        devices: vec![BrowserDevicePreset::IphoneSe],
        orientations: vec![BrowserOrientation::Portrait],
        ready: None,
    })?;

    assert!(matches!(
        configured,
        BrowserActionResult::Action {
            action: BrowserActionName::SetDevice,
            executed: false,
            ..
        }
    ));
    assert!(matches!(
        state,
        BrowserActionResult::MobileState {
            executed: false,
            state,
            ..
        } if state.provider == "chromium_emulation" && !state.verified
    ));
    assert!(matches!(
        audit,
        BrowserActionResult::MobileAudit {
            executed: false,
            audit,
            ..
        } if audit.samples.is_empty() && !audit.passed
    ));
    Ok(())
}

#[tokio::test]
async fn ios_backend_uses_explicit_appium_session_and_reports_real_engine() -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let endpoint = url::Url::parse(&format!("http://{}/", listener.local_addr()?))?;
    let server = tokio::spawn(serve_appium_fixture(listener));
    let browser = IosBrowser::new(BrowserIosConfig::new(
        endpoint,
        BrowserIosDeviceSelector::ExactName("iPhone 16 Pro".to_owned()),
    )?)?;

    let BrowserActionResult::MobileState { state, .. } =
        browser.execute(BrowserAction::MobileState).await?
    else {
        return Err(eyre!("expected iOS mobile state"));
    };
    assert_eq!(state.provider, "ios_webdriver");
    assert_eq!(state.engine, "webkit");
    assert!(state.verified);
    browser.close().await?;
    server.await??;
    Ok(())
}

#[tokio::test]
async fn code_mode_description_exposes_browser_action_schema() -> Result<()> {
    let (browser, _recording) = BrowserTool::recording();
    let tools = Tools::builder().without_defaults().tool(browser).build()?;
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
    let specs = runtime.model_specs("test-session");
    let description = specs
        .first()
        .map(nanocodex_tools::ToolDefinition::description)
        .ok_or_else(|| eyre!("missing Code Mode tool definition"))?;
    assert!(
        description.len() <= 128 * 1024,
        "browser Code Mode description grew to {} bytes",
        description.len()
    );

    assert!(description.contains("await tools.browser"));
    assert!(description.contains("declare const tools: { browser("));
    assert!(description.contains(r#"action: "open""#));
    assert!(description.contains(r#"action: "reload""#));
    assert!(description.contains(r#"action: "snapshot""#));
    assert!(description.contains(r#"action: "snapshot_find""#));
    assert!(description.contains(r#"action: "dom_snapshot""#));
    assert!(description.contains(r#"action: "go_back""#));
    assert!(description.contains(r#"action: "wait_for_text""#));
    assert!(description.contains(r#"action: "wait_for_function""#));
    assert!(description.contains("interactive?: boolean"));
    assert!(description.contains(r#"action: "get_html""#));
    assert!(description.contains(r#"action: "get_styles""#));
    assert!(description.contains(r#"action: "screenshot""#));
    assert!(description.contains("device_scale_factor?: number"));
    assert!(description.contains(r#"action: "set_device""#));
    assert!(description.contains(r#"action: "mobile_state""#));
    assert!(description.contains(r#"action: "mobile_audit""#));
    assert!(description.contains(r#"device: "iphone_se" | "iphone15_pro""#));
    assert!(description.contains("horizontalOverflow: number"));
    assert!(description.contains("target?:"));
    assert!(description.contains(r#"action: "pdf""#));
    assert!(description.contains(r#"action: "session_trace_start""#));
    assert!(description.contains(r#"action: "mouse_move""#));
    assert!(description.contains(r#"action: "matched_styles""#));
    assert!(description.contains(r#"action: "event_listeners""#));
    assert!(description.contains(r#"action: "storage_inspect""#));
    assert!(description.contains(r#"action: "console""#));
    assert!(description.contains(r#"action: "errors""#));
    assert!(description.contains(r#"action: "network_requests""#));
    assert!(description.contains(r#"action: "network_body""#));
    assert!(description.contains(r#"action: "web_socket_messages""#));
    assert!(description.contains(r#"action: "react_events""#));
    assert!(description.contains(r#"action: "element_context""#));
    assert!(description.contains(r#"action: "visual_diff""#));
    assert!(description.contains(r#"action: "visual_trace_start""#));
    assert!(description.contains(r#"action: "web_vitals""#));
    assert!(description.contains(r#"action: "performance_trace_start""#));
    assert!(description.contains(r#"action: "cpu_profile_start""#));
    assert!(description.contains(r#"action: "coverage_start""#));
    assert!(description.contains(r#"action: "heap_snapshot""#));
    assert!(description.contains(r#"action: "heap_retainers""#));
    assert!(description.contains(r#"action: "heap_inspect""#));
    assert!(description.contains(r#"action: "video_start""#));
    assert!(description.contains("Defaults to 30 and accepts values from 1 through 60"));
    assert!(description.contains(r#"action: "accessibility_audit""#));
    assert!(description.contains(r#"action: "axe_audit""#));
    assert!(description.contains(r#"action: "lighthouse_audit""#));
    assert!(description.contains(r#"action: "crux""#));
    assert!(description.contains(r#"action: "passkeys""#));
    assert!(description.contains(r#"action: "passkey_use""#));
    assert!(description.contains(r#"action: "passkey_new""#));
    assert!(description.contains(r#"action: "passkey_auto""#));
    assert!(description.contains(r#"action: "host_passkey_start""#));
    assert!(description.contains(r#"action: "host_passkey_resume""#));
    assert!(description.contains(r#"action: "export_har""#));
    assert!(description.contains(r#"action: "list_frames""#));
    assert!(description.contains(r#"action: "list_tabs""#));
    assert!(description.contains(r#"action: "load_extension""#));
    assert!(description.contains(r#"action: "trigger_extension_action""#));
    assert!(description.contains("after?: number"));
    assert!(description.contains("shadowTreeNodeCount: number"));
    assert!(description.contains("bodyAvailable: boolean"));
    assert!(description.contains("last_sequence"));
    assert!(description.contains("limit?: number"));
    assert!(description.contains("backgroundColor: string"));
    assert!(description.contains("refs:"));
    assert!(description.contains("modelImage?:"));
    assert!(description.contains("maximumRetainedSize"));
    assert!(description.contains("outcome?:"));
    assert!(description.contains(r#"by: "role""#));
    assert!(description.contains("Promise.all"));
    assert!(description.contains("Promise<{"));
    Ok(())
}

#[tokio::test]
async fn deferred_browser_is_advertised_without_action_schema_bytes() -> Result<()> {
    let baseline_tools = Tools::builder().without_defaults().build()?;
    let baseline =
        ToolRuntime::new_with_tools(".", None, None, &baseline_tools).model_specs("test-session");
    let (browser, recording) = BrowserTool::recording();
    let tools = Tools::builder()
        .without_defaults()
        .provider(browser)
        .build()?;
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
    let specs = runtime.model_specs("test-session");

    let baseline = serde_json::to_vec(&baseline)?;
    let serialized = serde_json::to_vec(&specs)?;
    let model_contract = String::from_utf8(serialized.clone())?;
    assert!(model_contract.contains("tools.browser"));
    assert!(model_contract.contains("host-managed browser session"));
    assert!(!model_contract.contains("detect_gate"));
    assert!(serialized.len() - baseline.len() < 512);
    assert!(runtime.contains("browser"));

    let execution = runtime
        .execute_code(
            r#"
const metadata = ALL_TOOLS.find((tool) => tool.name === "browser");
if (!metadata) throw new Error("browser metadata missing");
if (typeof metadata.description !== "string") {
  throw new Error("browser description missing");
}
const schema = toolSchema(metadata.name)?.inputSchema;
const variants = schema?.oneOf ?? [];
const screenshot = variants.find((variant) =>
  variant.properties?.action?.enum?.[0] === "screenshot"
);
const viewport = variants.find((variant) =>
  variant.properties?.action?.enum?.[0] === "set_viewport"
);
if (!screenshot?.properties?.target) {
  throw new Error("targeted screenshot schema missing");
}
if (!viewport?.properties?.device_scale_factor) {
  throw new Error("device scale factor schema missing");
}
const opened = await tools[metadata.name]({
  action: "open",
  url: "https://example.com"
});
text({
  name: metadata.name,
  hasDescription: metadata.description.length > 0,
  hasPixelCalibrationSchema: true,
  opened
});
"#,
            context(),
        )
        .await;

    assert!(execution.success, "{:#?}", execution.output);
    assert_eq!(execution.nested_calls.len(), 1);
    let output: Value = serde_json::from_str(execution_text(&execution.output)?)?;
    assert_eq!(output["name"], "browser");
    assert_eq!(output["hasDescription"], true);
    assert_eq!(output["hasPixelCalibrationSchema"], true);
    assert_eq!(output["opened"]["action"], "open");
    assert_eq!(recording.actions()?.len(), 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn targeted_screenshots_honor_device_pixel_ratio() -> Result<()> {
    let browser = Browser::new()?;
    browser
        .execute(BrowserAction::SetViewport {
            width: 320,
            height: 240,
            device_scale_factor: Some(2.0),
        })
        .await?;
    browser
        .execute(BrowserAction::Open {
            url: r#"data:text/html,<body style='margin:0'><div id='target' style='width:120px;height:80px;background:red'></div><iframe style='border:0;width:100px;height:100px' srcdoc="<body style='margin:0'><div id='frame-target' style='width:60px;height:40px;background:green'></div>"></iframe>"#.to_owned(),
        })
        .await?;
    let target = BrowserTarget::css("#target");
    let screenshot = browser
        .execute(BrowserAction::Screenshot {
            full_page: false,
            annotate: false,
            target: Some(target.clone()),
        })
        .await?;
    let BrowserActionResult::Screenshot {
        image: Some(image), ..
    } = screenshot
    else {
        return Err(eyre!("expected targeted screenshot"));
    };
    assert_eq!((image.width, image.height), (240, 160));
    let frame_screenshot = browser
        .execute(BrowserAction::Screenshot {
            full_page: false,
            annotate: false,
            target: Some(BrowserTarget::css("#frame-target")),
        })
        .await?;
    assert!(matches!(
        frame_screenshot,
        BrowserActionResult::Screenshot { image: Some(image), .. }
            if (image.width, image.height) == (120, 80)
    ));

    let baseline = browser
        .execute(BrowserAction::VisualBaseline {
            full_page: false,
            target: Some(target.clone()),
        })
        .await?;
    let BrowserActionResult::VisualBaseline { image, .. } = baseline else {
        return Err(eyre!("expected targeted visual baseline"));
    };
    browser
        .execute(BrowserAction::Evaluate {
            expression: "document.querySelector('#target').style.background = 'blue'; true"
                .to_owned(),
        })
        .await?;
    let diff = browser
        .execute(BrowserAction::VisualDiff {
            baseline_id: image.artifact_id,
            threshold: Some(0),
            full_page: false,
            target: Some(target.clone()),
        })
        .await?;
    assert!(matches!(
        diff,
        BrowserActionResult::VisualDiff { diff, .. }
            if diff.dimensions_match && diff.changed_pixel_ratio > 0.99
    ));
    assert!(matches!(
        browser
            .execute(BrowserAction::Screenshot {
                full_page: true,
                annotate: false,
                target: Some(target),
            })
            .await,
        Err(BrowserError::TargetWithFullPage)
    ));
    browser.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn mobile_device_state_and_audit_are_page_observable() -> Result<()> {
    let macos_chrome =
        std::path::Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    let test_chrome = std::env::var_os("NANOCODEX_TEST_CHROME").map(std::path::PathBuf::from);
    let browser = if let Some(test_chrome) = test_chrome {
        Browser::with_executable(test_chrome)?
    } else if macos_chrome.is_file() {
        Browser::with_executable(macos_chrome)?
    } else {
        Browser::new()?
    };
    browser
        .execute(BrowserAction::SetDevice {
            device: BrowserDevicePreset::IphoneSe,
            orientation: BrowserOrientation::Portrait,
        })
        .await?;
    browser
        .execute(BrowserAction::Open {
            url: "data:text/html,<meta name='viewport' content='width=device-width,initial-scale=1'><style>body{margin:0}button{width:20px;height:20px}input{font-size:12px}.wide{width:450px;height:1px}</style><button id='small'>x</button><input aria-label='message'><div class='wide'></div>".to_owned(),
        })
        .await?;

    let BrowserActionResult::Box {
        bounds: Some(bounds),
        ..
    } = browser
        .execute(BrowserAction::GetBox {
            target: BrowserTarget::css("input[aria-label='message']"),
        })
        .await?
    else {
        return Err(eyre!("expected input bounds"));
    };
    browser
        .execute(BrowserAction::TouchTap {
            x: (bounds.x + bounds.width / 2.0).round() as i32,
            y: (bounds.y + bounds.height / 2.0).round() as i32,
        })
        .await?;
    browser
        .execute(BrowserAction::InsertText {
            text: "touch input proof".to_owned(),
        })
        .await?;
    let BrowserActionResult::Value { value, .. } = browser
        .execute(BrowserAction::GetValue {
            target: BrowserTarget::css("input[aria-label='message']"),
        })
        .await?
    else {
        return Err(eyre!("expected input value"));
    };
    assert_eq!(value.as_deref(), Some("touch input proof"));

    let BrowserActionResult::MobileState { state, .. } =
        browser.execute(BrowserAction::MobileState).await?
    else {
        return Err(eyre!("expected mobile state"));
    };
    assert!(state.verified, "{:?}", state.mismatches);
    assert_eq!(state.screen_width, 375.0);
    assert_eq!(state.screen_height, 667.0);
    assert_eq!(state.device_pixel_ratio, 2.0);
    assert_eq!(state.max_touch_points, 5);
    assert!(state.coarse_pointer && state.no_hover);

    let BrowserActionResult::MobileAudit { audit, .. } = browser
        .execute(BrowserAction::MobileAudit {
            devices: vec![BrowserDevicePreset::IphoneSe],
            orientations: vec![BrowserOrientation::Portrait],
            ready: None,
        })
        .await?
    else {
        return Err(eyre!("expected mobile audit"));
    };
    assert_eq!(audit.samples.len(), 1);
    assert!(audit.error_count >= 1, "{audit:#?}");
    assert!(audit.warning_count >= 2, "{audit:#?}");
    let rules = audit.samples[0]
        .findings
        .iter()
        .map(|finding| finding.rule.as_str())
        .collect::<Vec<_>>();
    assert!(rules.contains(&"horizontal-overflow"));
    assert!(rules.contains(&"touch-target-size"));
    assert!(rules.contains(&"input-font-size"));
    browser.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local Chromium or NANOCODEX_TEST_REMOTE_CDP_ENDPOINT"]
#[allow(
    clippy::too_many_lines,
    reason = "one live vertical slice validates the Code Mode browser contract end to end"
)]
async fn managed_browser_executes_code_mode_against_chromium() -> Result<()> {
    let browser = match std::env::var("NANOCODEX_TEST_REMOTE_CDP_ENDPOINT") {
        Ok(endpoint) => Browser::builder()
            .cdp_endpoint(url::Url::parse(&endpoint)?)
            .build()?,
        Err(std::env::VarError::NotPresent) => Browser::new()?,
        Err(error) => return Err(error.into()),
    };
    let tools = Tools::builder()
        .without_defaults()
        .tool(BrowserTool::from_browser(browser.clone()))
        .build()?;
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
    let execution = runtime
        .execute_code(MANAGED_BROWSER_SOURCE, context())
        .await;

    if !execution.success {
        let calls = execution
            .nested_calls
            .iter()
            .map(|call| {
                serde_json::json!({
                    "input": call.input,
                    "output": call.output,
                    "success": call.success,
                })
            })
            .collect::<Vec<_>>();
        return Err(eyre!(
            "managed browser Code Mode execution failed: {}",
            serde_json::to_string_pretty(&calls)?
        ));
    }
    assert_eq!(execution.nested_calls.len(), 16);
    let ToolOutputBody::Content(content) = &execution.output else {
        return Err(eyre!("expected multimodal Code Mode output"));
    };
    assert!(
        content
            .iter()
            .any(|item| matches!(item, ToolOutputContent::InputImage { .. }))
    );
    let output: Value = serde_json::from_str(execution_text(&execution.output)?)?;
    assert_eq!(output["opened"]["executed"], true);
    assert_eq!(output["snapshot"]["executed"], true);
    assert!(
        output["snapshot"]["snapshot"]
            .as_str()
            .is_some_and(|snapshot| snapshot.contains("textbox \"Name\""))
    );
    assert_eq!(output["value"]["value"], "Nanocodex");
    assert!(
        output["textResult"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Save"))
    );
    assert!(
        output["html"]["html"]
            .as_str()
            .is_some_and(|html| html.contains("aria-label=\"Name\""))
    );
    assert_eq!(output["label"]["value"], "Name");
    assert_eq!(output["count"]["count"], 1);
    assert!(output["bounds"]["bounds"]["width"].as_f64().is_some());
    assert!(output["styles"]["styles"].as_object().is_some());
    assert_eq!(output["evaluation"]["value"]["inputs"], 1);
    assert!(
        output["consoleEntries"]["entries"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| {
                entry["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("nanocodex-browser-probe"))
            }))
    );
    assert!(
        output["errors"]["errors"]
            .as_array()
            .is_some_and(|errors| errors.iter().any(|error| {
                error["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("nanocodex-browser-error"))
            }))
    );
    assert!(
        output["requests"]["requests"]
            .as_array()
            .is_some_and(|requests| requests.iter().any(|request| {
                request["url"]
                    .as_str()
                    .is_some_and(|url| url.starts_with("data:text/html"))
                    && request["method"] == "GET"
                    && request["resourceType"] == "Document"
            }))
    );
    let screenshot = output["screenshot"]["path"]
        .as_str()
        .ok_or_else(|| eyre!("missing screenshot path"))?;
    assert!(std::path::Path::new(screenshot).is_file());
    std::fs::remove_file(screenshot)?;
    drop(runtime);
    drop(tools);
    browser.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
#[allow(
    clippy::too_many_lines,
    reason = "one live vertical slice validates the browser-debugging contract end to end"
)]
async fn browser_debugging_contract_runs_in_process() -> Result<()> {
    let files = tempfile::tempdir()?;
    std::fs::write(files.path().join("probe.txt"), b"nanocodex upload")?;
    let browser = Browser::builder().file_root(files.path()).build()?;
    browser
        .execute(BrowserAction::Open {
            url: r#"data:text/html,
<select id='select' multiple><option value='a'>A</option><option value='b'>B</option></select>
<input id='check' type='checkbox'>
<input id='file' type='file'>
<img src='data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw=='>
<button id='nameless'></button>
<div id='source' draggable='true'>source</div><div id='target'>target</div>
<div id='scroller' style='height:20px;overflow:auto'><div style='height:200px'>scroll</div></div>
<iframe name='fixture' srcdoc="<main id='frame-content'>child frame</main>"></iframe>"#
                .to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::Evaluate {
            expression: r##"(() => {
const target = document.querySelector("#target");
target.addEventListener("dragover", event => event.preventDefault());
target.addEventListener("drop", () => target.dataset.dropped = "yes");
return true;
})()"##
                .to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::SelectOption {
            target: BrowserTarget::css("#select"),
            values: vec!["b".to_owned()],
        })
        .await?;
    browser
        .execute(BrowserAction::SetChecked {
            target: BrowserTarget::css("#check"),
            checked: true,
        })
        .await?;
    browser
        .execute(BrowserAction::Drag {
            source: BrowserTarget::css("#source"),
            destination: BrowserTarget::css("#target"),
        })
        .await?;
    browser
        .execute(BrowserAction::UploadFiles {
            target: BrowserTarget::css("#file"),
            paths: vec!["probe.txt".into()],
        })
        .await?;
    browser
        .execute(BrowserAction::Scroll {
            target: Some(BrowserTarget::css("#scroller")),
            x: 0,
            y: 100,
        })
        .await?;
    let state = browser
        .execute(BrowserAction::Evaluate {
            expression: r##"({
  selected: Array.from(document.querySelector("#select").selectedOptions, option => option.value),
  checked: document.querySelector("#check").checked,
  dropped: document.querySelector("#target").dataset.dropped,
  files: document.querySelector("#file").files.length,
  scrollTop: document.querySelector("#scroller").scrollTop
})"##
                .to_owned(),
        })
        .await?;
    let BrowserActionResult::Evaluation { value, .. } = state else {
        return Err(eyre!("expected input state"));
    };
    assert_eq!(value["selected"], serde_json::json!(["b"]));
    assert_eq!(value["checked"], true);
    assert_eq!(value["dropped"], "yes");
    assert_eq!(value["files"], 1);
    assert!(value["scrollTop"].as_f64().is_some_and(|value| value > 0.0));

    let frames = browser.execute(BrowserAction::ListFrames).await?;
    let BrowserActionResult::Frames { frames, .. } = frames else {
        return Err(eyre!("expected frames"));
    };
    assert!(frames.iter().any(|frame| frame.main));
    let child = frames
        .iter()
        .find(|frame| !frame.main)
        .ok_or_else(|| eyre!("missing child frame"))?;
    let frame_value = browser
        .execute(BrowserAction::EvaluateFrame {
            frame_id: child.frame_id.clone(),
            expression: "document.querySelector('#frame-content').textContent".to_owned(),
        })
        .await?;
    assert!(matches!(
        frame_value,
        BrowserActionResult::FrameEvaluation { value, .. }
            if value == "child frame"
    ));

    let accessibility = browser.execute(BrowserAction::AccessibilityAudit).await?;
    let BrowserActionResult::Accessibility { audit, .. } = accessibility else {
        return Err(eyre!("expected accessibility audit"));
    };
    assert!(
        audit
            .violations
            .iter()
            .any(|violation| violation.rule == "image-alt")
    );
    assert!(
        audit
            .violations
            .iter()
            .any(|violation| violation.rule == "interactive-name")
    );
    let axe = browser.execute(BrowserAction::AxeAudit).await?;
    let BrowserActionResult::Axe { audit, .. } = axe else {
        return Err(eyre!("expected axe audit"));
    };
    assert!(!audit.engine_version.is_empty());
    assert!(
        audit
            .violations
            .iter()
            .any(|finding| finding.id == "image-alt")
    );
    let pdf = browser
        .execute(BrowserAction::Pdf {
            landscape: false,
            print_background: true,
            prefer_css_page_size: false,
            tagged: true,
            document_outline: true,
        })
        .await?;
    let BrowserActionResult::Pdf { pdf, .. } = pdf else {
        return Err(eyre!("expected PDF artifact"));
    };
    assert!(pdf.path.is_file());
    assert!(pdf.bytes > 0);

    browser
        .execute(BrowserAction::Evaluate {
            expression: r#"(() => {
const link = document.createElement("a");
link.href = "data:text/plain,nanocodex-download";
link.download = "nanocodex-download.txt";
link.click();
return true;
})()"#
                .to_owned(),
        })
        .await?;
    let mut downloads = Vec::new();
    for _ in 0..40 {
        let result = browser.execute(BrowserAction::Downloads).await?;
        let BrowserActionResult::Downloads {
            downloads: current, ..
        } = result
        else {
            return Err(eyre!("expected downloads"));
        };
        downloads = current;
        if downloads.iter().any(|download| download.completed) {
            break;
        }
        browser
            .execute(BrowserAction::WaitForTimeout { milliseconds: 25 })
            .await?;
    }
    assert!(
        downloads.iter().any(|download| {
            download.completed && download.path.as_ref().is_some_and(|path| path.is_file())
        }),
        "{downloads:#?}"
    );

    let baseline = browser
        .execute(BrowserAction::VisualBaseline {
            full_page: false,
            target: None,
        })
        .await?;
    let BrowserActionResult::VisualBaseline { image, .. } = baseline else {
        return Err(eyre!("expected visual baseline"));
    };
    browser
        .execute(BrowserAction::Evaluate {
            expression: "document.body.style.background = 'rgb(255, 0, 255)'; true".to_owned(),
        })
        .await?;
    let diff = browser
        .execute(BrowserAction::VisualDiff {
            baseline_id: image.artifact_id,
            threshold: Some(8),
            full_page: false,
            target: None,
        })
        .await?;
    let BrowserActionResult::VisualDiff { diff, .. } = diff else {
        return Err(eyre!("expected visual diff"));
    };
    assert!(diff.changed_pixel_ratio > 0.1);
    assert!(diff.diff_path.is_file());

    let vitals = browser.execute(BrowserAction::WebVitals).await?;
    assert!(matches!(
        vitals,
        BrowserActionResult::WebVitals { vitals, .. } if vitals.url.starts_with("data:text/html")
    ));
    browser
        .execute(BrowserAction::PerformanceTraceStart)
        .await?;
    browser
        .execute(BrowserAction::Evaluate {
            expression:
                "(() => { const end = performance.now() + 75; while (performance.now() < end) {} return true; })()"
                    .to_owned(),
        })
        .await?;
    let trace = browser.execute(BrowserAction::PerformanceTraceStop).await?;
    let BrowserActionResult::PerformanceTrace { trace, .. } = trace else {
        return Err(eyre!("expected performance trace"));
    };
    assert!(trace.event_count > 0);
    assert!(trace.path.is_file());

    browser
        .execute(BrowserAction::Evaluate {
            expression:
                "document.documentElement.innerHTML = '<body style=\"margin:0;background:white\"></body>'; true"
                    .to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::VisualTraceStart {
            frames_per_second: Some(20),
            max_frames: Some(20),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForTimeout { milliseconds: 100 })
        .await?;
    browser
        .execute(BrowserAction::Evaluate {
            expression: "document.body.style.background = 'black'; true".to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForTimeout { milliseconds: 100 })
        .await?;
    browser
        .execute(BrowserAction::Evaluate {
            expression: "document.body.style.background = 'white'; true".to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForTimeout { milliseconds: 100 })
        .await?;
    let visual_trace = browser.execute(BrowserAction::VisualTraceStop).await?;
    let BrowserActionResult::VisualTrace { trace, .. } = visual_trace else {
        return Err(eyre!("expected visual trace"));
    };
    assert!(trace.frame_count >= 3);
    assert!(trace.maximum_changed_pixel_ratio > 0.5);
    assert!(!trace.anomalies.is_empty());

    browser
        .execute(BrowserAction::Evaluate {
            expression: "setTimeout(() => alert('nanocodex dialog'), 0); true".to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForTimeout { milliseconds: 50 })
        .await?;
    let dialog = browser.execute(BrowserAction::Dialog).await?;
    assert!(matches!(
        dialog,
        BrowserActionResult::Dialog {
            dialog: Some(dialog),
            ..
        } if dialog.message == "nanocodex dialog"
    ));
    browser
        .execute(BrowserAction::HandleDialog {
            accept: true,
            prompt_text: None,
        })
        .await?;

    browser
        .execute(BrowserAction::NewTab {
            url: Some("data:text/html,<title>second</title>".to_owned()),
        })
        .await?;
    let tabs = browser.execute(BrowserAction::ListTabs).await?;
    let BrowserActionResult::Tabs { tabs, .. } = tabs else {
        return Err(eyre!("expected tabs"));
    };
    assert!(tabs.len() >= 2);
    let active = tabs
        .iter()
        .find(|tab| tab.active)
        .ok_or_else(|| eyre!("missing active tab"))?;
    assert_eq!(active.title, "second");
    browser
        .execute(BrowserAction::CloseTab {
            tab_id: active.tab_id.clone(),
        })
        .await?;
    browser.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
#[allow(
    clippy::too_many_lines,
    reason = "one adversarial page proves actionability, settling, frame input, waits, and source maps together"
)]
async fn browser_actions_are_native_settled_and_source_mapped() -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(serve_action_fixture(listener));
    let browser = Browser::builder()
        .after_action(BrowserAfterAction::Snapshot)
        .build()?;

    let opened = browser
        .execute(BrowserAction::Open {
            url: format!("http://{address}/"),
        })
        .await?;
    let BrowserActionResult::Action {
        outcome: Some(opened),
        ..
    } = opened
    else {
        return Err(eyre!("expected an open action outcome"));
    };
    assert_eq!(opened.page.ready_state, BrowserDocumentReadyState::Complete);
    assert!(opened.network.request_count >= 2);
    assert!(!opened.network.timed_out);

    let found = browser
        .execute(BrowserAction::SnapshotFind {
            query: "Mapped Action".to_owned(),
            max_results: Some(5),
        })
        .await?;
    let BrowserActionResult::SnapshotFind {
        matches,
        refs,
        truncated,
        ..
    } = found
    else {
        return Err(eyre!("expected snapshot search results"));
    };
    assert!(!matches.is_empty());
    assert!(!truncated);
    assert!(refs.values().any(|element| element.name == "Mapped Action"));

    browser
        .execute(BrowserAction::Evaluate {
            expression: r##"(() => {
const overlay = document.createElement("div");
overlay.id = "overlay";
document.body.append(overlay);
document.querySelector("#action").disabled = true;
setTimeout(() => {
  overlay.remove();
  document.querySelector("#action").disabled = false;
}, 175);
return true;
})()"##
                .to_owned(),
        })
        .await?;
    let started = Instant::now();
    let clicked = browser
        .execute(BrowserAction::Click {
            target: BrowserTarget::role("button").named("Mapped Action"),
            options: None,
        })
        .await?;
    assert!(started.elapsed() >= std::time::Duration::from_millis(150));
    let BrowserActionResult::Action {
        outcome: Some(outcome),
        ..
    } = clicked
    else {
        return Err(eyre!("expected a click action outcome"));
    };
    assert_eq!(outcome.network.request_count, 1);
    assert_eq!(outcome.network.completed_count, 1);
    assert!(!outcome.network.timed_out);
    assert!(
        outcome
            .console
            .iter()
            .any(|entry| entry.text.contains("action console"))
    );
    assert!(
        outcome.errors.iter().any(|error| {
            error.text.contains("mapped boom")
                && error.stack.iter().any(|frame| {
                    frame
                        .original
                        .as_ref()
                        .is_some_and(|location| location.url == "src/fixture.ts")
                })
        }),
        "{:#?}",
        outcome.errors
    );
    assert!(matches!(
        outcome.snapshot,
        Some(BrowserPostActionSnapshot::Captured { ref snapshot, .. })
            if snapshot.contains("trusted:done")
    ));
    let status = browser
        .execute(BrowserAction::GetText {
            target: BrowserTarget::css("#status"),
        })
        .await?;
    assert!(matches!(
        status,
        BrowserActionResult::Text { text, .. } if text == "trusted:done"
    ));

    browser
        .execute(BrowserAction::Click {
            target: BrowserTarget::css("#double"),
            options: Some(BrowserClickOptions {
                click_count: 2,
                ..Default::default()
            }),
        })
        .await?;
    browser
        .execute(BrowserAction::Click {
            target: BrowserTarget::css("#modified"),
            options: Some(BrowserClickOptions {
                modifiers: vec![BrowserKeyModifier::Shift],
                ..Default::default()
            }),
        })
        .await?;
    let input_state = browser
        .execute(BrowserAction::Evaluate {
            expression: "({ double: globalThis.doubleClicked, shift: globalThis.shiftClicked })"
                .to_owned(),
        })
        .await?;
    assert!(matches!(
        input_state,
        BrowserActionResult::Evaluation { value, .. }
            if value["double"] == true && value["shift"] == true
    ));

    let child = browser
        .execute(BrowserAction::SnapshotFind {
            query: "Child Action".to_owned(),
            max_results: Some(5),
        })
        .await?;
    let BrowserActionResult::SnapshotFind { refs, .. } = child else {
        return Err(eyre!("expected child-frame snapshot search"));
    };
    let child = refs
        .iter()
        .find(|(_, element)| element.name == "Child Action")
        .map(|(reference, _)| format!("@{reference}"))
        .ok_or_else(|| eyre!("missing child-frame button reference"))?;
    browser
        .execute(BrowserAction::Click {
            target: BrowserTarget::reference(child),
            options: None,
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForText {
            text: "child:true".to_owned(),
            target: Some(BrowserTarget::css("#status")),
            hidden: false,
        })
        .await?;

    browser
        .execute(BrowserAction::Evaluate {
            expression: r#"(() => {
setTimeout(() => {
  const ready = document.createElement("div");
  ready.id = "later";
  ready.textContent = "later ready";
  document.body.append(ready);
}, 75);
return true;
})()"#
                .to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForSelector {
            target: BrowserTarget::text("later ready").exact(),
            state: Some(BrowserWaitForSelectorState::Visible),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForFunction {
            expression: "document.querySelector('#later')?.textContent === 'later ready'"
                .to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForLoadState {
            state: BrowserLoadState::Load,
        })
        .await?;
    browser
        .execute(BrowserAction::Evaluate {
            expression:
                "document.querySelector('#later').remove(); history.pushState({}, '', '/next'); true"
                    .to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForSelector {
            target: BrowserTarget::css("#later"),
            state: Some(BrowserWaitForSelectorState::Detached),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForUrl {
            url_contains: "/next".to_owned(),
        })
        .await?;
    browser.execute(BrowserAction::GoBack).await?;
    browser.execute(BrowserAction::GoForward).await?;
    browser
        .execute(BrowserAction::WaitForUrl {
            url_contains: "/next".to_owned(),
        })
        .await?;

    browser.close().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "requires local Chromium, ffmpeg with libvpx, and ffprobe"]
async fn browser_video_records_constant_rate_60_fps_webm() -> Result<()> {
    let browser = Browser::new()?;
    browser
        .execute(BrowserAction::Open {
            url: "data:text/html,<main>60%20fps%20recording</main>".to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::VideoStart {
            frames_per_second: Some(60),
            quality: Some(80),
        })
        .await?;
    browser
        .execute(BrowserAction::Evaluate {
            expression: r"globalThis.recordingFrame = 0;
globalThis.recordingTimer = setInterval(() => {
  document.body.style.backgroundColor = `hsl(${recordingFrame++ % 360} 70% 60%)`;
}, 16); true"
                .to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForTimeout {
            milliseconds: 1_100,
        })
        .await?;
    let stopped = browser.execute(BrowserAction::VideoStop).await?;
    let BrowserActionResult::Video { video, .. } = stopped else {
        return Err(eyre!("expected browser video"));
    };
    assert_eq!(video.frames_per_second, 60);
    assert!(video.frame_count >= 30, "{video:?}");
    assert!(video.captured_frame_count > 0, "{video:?}");
    assert!(video.duration_ms >= 1_000, "{video:?}");
    assert!(video.path.is_file());
    assert!(std::fs::metadata(&video.path)?.len() > 0);
    let probe = tokio::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=avg_frame_rate,width,height:format=duration",
            "-of",
            "json",
        ])
        .arg(&video.path)
        .output()
        .await?;
    if !probe.status.success() {
        return Err(eyre!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&probe.stderr)
        ));
    }
    let probe: Value = serde_json::from_slice(&probe.stdout)?;
    let stream = &probe["streams"][0];
    assert_eq!(stream["avg_frame_rate"], "60/1", "{probe}");
    assert_eq!(stream["width"], video.width, "{probe}");
    assert_eq!(stream["height"], video.height, "{probe}");
    let encoded_duration = probe["format"]["duration"]
        .as_str()
        .ok_or_else(|| eyre!("ffprobe omitted duration: {probe}"))?
        .parse::<f64>()?;
    assert!(encoded_duration >= 1.0, "{probe}");

    browser
        .execute(BrowserAction::VideoStart {
            frames_per_second: Some(30),
            quality: Some(80),
        })
        .await?;
    let stopped = browser.execute(BrowserAction::VideoStop).await?;
    let BrowserActionResult::Video { video, .. } = stopped else {
        return Err(eyre!("expected immediate browser video"));
    };
    assert!(video.frame_count >= 1, "{video:?}");
    assert!(video.captured_frame_count >= 1, "{video:?}");
    assert!(video.path.is_file());
    browser.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local Chromium and ffmpeg with libvpx"]
#[allow(
    clippy::too_many_lines,
    reason = "one real diagnostic workload verifies every file-backed profiler lifecycle"
)]
async fn browser_profilers_and_video_produce_typed_artifacts() -> Result<()> {
    let browser = Browser::new()?;
    browser.execute(BrowserAction::CoverageStart).await?;
    browser
        .execute(BrowserAction::Open {
            url: r#"data:text/html,<main>diagnostics</main><script>
function coveredWork() { return Array.from({ length: 100 }, (_, index) => index).reduce((a, b) => a + b, 0); }
function unusedWork() { return "unused branch"; }
</script>"#
                .to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::Evaluate {
            expression: "coveredWork()".to_owned(),
        })
        .await?;
    let coverage = browser.execute(BrowserAction::CoverageStop).await?;
    let BrowserActionResult::Coverage { coverage, .. } = coverage else {
        return Err(eyre!("expected JavaScript coverage"));
    };
    assert!(coverage.path.is_file());
    assert!(coverage.total_bytes > 0);
    assert!(coverage.used_bytes > 0);
    assert!(coverage.unused_bytes > 0, "{coverage:#?}");

    browser
        .execute(BrowserAction::PerformanceTraceStart)
        .await?;
    browser
        .execute(BrowserAction::Evaluate {
            expression: r"(() => {
const end = performance.now() + 100;
while (performance.now() < end) coveredWork();
return true;
})()"
                .to_owned(),
        })
        .await?;
    let trace = browser.execute(BrowserAction::PerformanceTraceStop).await?;
    let BrowserActionResult::PerformanceTrace { trace, .. } = trace else {
        return Err(eyre!("expected performance trace"));
    };
    assert!(trace.path.is_file());
    assert!(trace.event_count > 0);
    assert!(trace.long_task_count > 0);
    assert!(
        trace
            .insights
            .iter()
            .any(|insight| matches!(insight, BrowserPerformanceInsight::LongTask { .. }))
    );
    assert!(
        trace
            .insights
            .iter()
            .any(|insight| matches!(insight, BrowserPerformanceInsight::DomSize { .. }))
    );

    browser.execute(BrowserAction::CpuProfileStart).await?;
    browser
        .execute(BrowserAction::Evaluate {
            expression: r"(() => {
const end = performance.now() + 125;
while (performance.now() < end) coveredWork();
return true;
})()"
                .to_owned(),
        })
        .await?;
    let cpu = browser.execute(BrowserAction::CpuProfileStop).await?;
    let BrowserActionResult::CpuProfile { profile, .. } = cpu else {
        return Err(eyre!("expected CPU profile"));
    };
    assert!(profile.path.is_file());
    assert!(profile.sample_count > 0);
    assert!(
        profile
            .functions
            .iter()
            .any(|function| function.function_name == "coveredWork")
    );

    let before = browser
        .execute(BrowserAction::HeapSnapshot {
            collect_garbage: true,
        })
        .await?;
    let BrowserActionResult::HeapSnapshot {
        snapshot: before, ..
    } = before
    else {
        return Err(eyre!("expected first heap snapshot"));
    };
    assert!(before.path.is_file());
    browser
        .execute(BrowserAction::Evaluate {
            expression: r"class RetainedFixture {
  constructor(index) {
    this.index = index;
    this.payload = `retained-${index}`.repeat(4);
  }
}
globalThis.retainedFixture = Array.from(
  { length: 10_000 },
  (_, index) => new RetainedFixture(index)
); retainedFixture.length"
                .to_owned(),
        })
        .await?;
    let after = browser
        .execute(BrowserAction::HeapSnapshot {
            collect_garbage: true,
        })
        .await?;
    let BrowserActionResult::HeapSnapshot {
        snapshot: after, ..
    } = after
    else {
        return Err(eyre!("expected second heap snapshot"));
    };
    assert!(after.path.is_file());
    let retained_object = after
        .classes
        .iter()
        .find(|class| class.name == "Object")
        .ok_or_else(|| {
            eyre!(
                "expected Object heap class; got {:?}",
                after
                    .classes
                    .iter()
                    .map(|class| class.name.as_str())
                    .collect::<Vec<_>>()
            )
        })?;
    let retainers = browser
        .execute(BrowserAction::HeapRetainers {
            artifact_id: after.artifact_id.clone(),
            node_id: retained_object.maximum_retained_node_id,
            max_depth: Some(8),
            max_nodes: Some(200),
        })
        .await?;
    let BrowserActionResult::HeapRetainers { retainers, .. } = retainers else {
        return Err(eyre!("expected heap retainers"));
    };
    assert_eq!(
        retainers.target_node_id,
        retained_object.maximum_retained_node_id
    );
    assert!(retainers.nodes.len() > 1);
    assert!(
        retainers
            .nodes
            .iter()
            .skip(1)
            .any(|node| node.retains_node_id.is_some())
    );
    let inspection = browser
        .execute(BrowserAction::HeapInspect {
            artifact_id: after.artifact_id.clone(),
            class_name: Some("Object".to_owned()),
            minimum_retained_size: Some(1),
            max_nodes: Some(25),
            include_duplicate_strings: true,
        })
        .await?;
    let BrowserActionResult::HeapInspection { inspection, .. } = inspection else {
        return Err(eyre!("expected detailed heap inspection"));
    };
    assert_eq!(inspection.artifact_id, after.artifact_id);
    assert!(inspection.matching_node_count > 0);
    assert!(!inspection.nodes.is_empty());
    assert!(inspection.nodes.len() <= 25);
    let comparison = browser
        .execute(BrowserAction::HeapCompare {
            before_id: before.artifact_id,
            after_id: after.artifact_id,
        })
        .await?;
    let BrowserActionResult::HeapComparison { comparison, .. } = comparison else {
        return Err(eyre!("expected heap comparison"));
    };
    assert!(comparison.node_count_delta > 5_000);
    assert!(comparison.self_size_delta > 0);
    assert!(!comparison.growing_classes.is_empty());

    browser
        .execute(BrowserAction::VideoStart {
            frames_per_second: Some(20),
            quality: Some(75),
        })
        .await?;
    for color in ["red", "green", "blue"] {
        browser
            .execute(BrowserAction::Evaluate {
                expression: format!("document.body.style.backgroundColor = {color:?}; true"),
            })
            .await?;
        browser
            .execute(BrowserAction::WaitForTimeout { milliseconds: 150 })
            .await?;
    }
    let video = browser.execute(BrowserAction::VideoStop).await?;
    let BrowserActionResult::Video { video, .. } = video else {
        return Err(eyre!("expected browser video"));
    };
    assert!(video.path.is_file());
    assert!(video.frame_count >= 3);
    assert!(video.captured_frame_count >= 3);
    assert_eq!(video.frames_per_second, 20);
    assert!(std::fs::metadata(&video.path)?.len() > 0);

    browser.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
#[allow(
    clippy::too_many_lines,
    reason = "one live scenario covers routes, blocked foreground and background egress, diagnostics, and HAR"
)]
async fn restricted_browser_routes_without_network_egress() -> Result<()> {
    let browser = Browser::builder()
        .egress_policy(BrowserEgressPolicy::deny_by_default())
        .build()?;
    browser
        .execute(BrowserAction::NetworkRoute {
            route_id: "fixture".to_owned(),
            url_contains: "fixture.invalid".to_owned(),
            response: BrowserRouteResponse {
                status: 200,
                headers: vec![BrowserRouteHeader {
                    name: "content-type".to_owned(),
                    value: "text/html; charset=utf-8".to_owned(),
                }],
                body: "<main>contained fixture</main>".to_owned(),
            },
        })
        .await?;
    browser
        .execute(BrowserAction::Open {
            url: "https://fixture.invalid/test".to_owned(),
        })
        .await?;
    let text = browser
        .execute(BrowserAction::GetText {
            target: BrowserTarget::css("main"),
        })
        .await?;
    assert!(matches!(
        text,
        BrowserActionResult::Text { text, .. } if text == "contained fixture"
    ));
    let blocked = browser
        .execute(BrowserAction::Evaluate {
            expression: "fetch('https://blocked.invalid/').then(() => false, () => true)"
                .to_owned(),
        })
        .await?;
    assert!(matches!(
        blocked,
        BrowserActionResult::Evaluation {
            value: Value::Bool(true),
            ..
        }
    ));
    let errors = browser
        .execute(BrowserAction::Errors { limit: None })
        .await?;
    assert!(matches!(
        errors,
        BrowserActionResult::Errors { errors, .. }
            if errors.iter().any(|error| error.text.contains("blocked network request"))
    ));
    let har = browser
        .execute(BrowserAction::ExportHar {
            include_bodies: false,
        })
        .await?;
    let BrowserActionResult::Har { har, .. } = har else {
        return Err(eyre!("expected HAR artifact"));
    };
    assert!(har.entry_count >= 2);
    assert!(har.path.is_file());
    let exported: Value = serde_json::from_slice(&std::fs::read(&har.path)?)?;
    assert_eq!(exported["log"]["version"], "1.2");
    let tabs = browser.execute(BrowserAction::ListTabs).await?;
    let BrowserActionResult::Tabs { tabs, .. } = tabs else {
        return Err(eyre!("expected tabs"));
    };
    let original_tab = tabs
        .iter()
        .find(|tab| tab.active)
        .ok_or_else(|| eyre!("missing original active tab"))?
        .tab_id
        .clone();
    browser
        .execute(BrowserAction::Evaluate {
            expression: r#"
window.__nanocodexBackgroundEgress = "pending";
setTimeout(() => {
  fetch("https://background-blocked.invalid/")
    .then(
      () => { window.__nanocodexBackgroundEgress = "escaped"; },
      () => { window.__nanocodexBackgroundEgress = "blocked"; },
    );
}, 100);
true
"#
            .to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::NewTab {
            url: Some("data:text/html,<title>foreground</title>".to_owned()),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForTimeout { milliseconds: 500 })
        .await?;
    browser
        .execute(BrowserAction::SelectTab {
            tab_id: original_tab,
        })
        .await?;
    let background = browser
        .execute(BrowserAction::Evaluate {
            expression: "window.__nanocodexBackgroundEgress".to_owned(),
        })
        .await?;
    assert!(matches!(
        background,
        BrowserActionResult::Evaluation {
            value: Value::String(value),
            ..
        } if value == "blocked"
    ));
    browser
        .execute(BrowserAction::SetOffline { offline: true })
        .await?;
    browser
        .execute(BrowserAction::SetOffline { offline: false })
        .await?;
    browser.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
#[allow(
    clippy::too_many_lines,
    reason = "one synthetic document validates pre-React injection, Fiber events, and element context"
)]
async fn react_diagnostics_are_installed_before_application_code() -> Result<()> {
    let browser = Browser::builder()
        .react_diagnostics(ReactDiagnostics::default())
        .build()?;
    browser
        .execute(BrowserAction::Open {
            url: r#"data:text/html,<main id="ready">synthetic react</main><script>
const hook = globalThis.__REACT_DEVTOOLS_GLOBAL_HOOK__;
function SyntheticChild() {}
function SyntheticApp() {}
const ready = document.getElementById("ready");
const host = {
  tag: 5,
  type: "main",
  elementType: "main",
  stateNode: ready,
  child: null,
  sibling: null,
  return: null,
  alternate: null,
  actualDuration: 1,
  actualStartTime: 102,
  selfBaseDuration: 1,
  treeBaseDuration: 1,
  _debugSource: { fileName: "src/SyntheticChild.tsx", lineNumber: 13, columnNumber: 6 }
};
const child = {
  tag: 0,
  type: SyntheticChild,
  elementType: SyntheticChild,
  child: host,
  sibling: null,
  return: null,
  alternate: null,
  actualDuration: 3,
  actualStartTime: 101,
  selfBaseDuration: 3,
  treeBaseDuration: 3,
  _debugSource: { fileName: "src/SyntheticChild.tsx", lineNumber: 12, columnNumber: 4 }
};
host.return = child;
host._debugOwner = child;
const rootFiber = {
  tag: 0,
  type: SyntheticApp,
  elementType: SyntheticApp,
  child,
  sibling: null,
  return: null,
  alternate: null,
  actualDuration: 8,
  actualStartTime: 100,
  selfBaseDuration: 5,
  treeBaseDuration: 8,
  _debugSource: { fileName: "src/SyntheticApp.tsx", lineNumber: 7, columnNumber: 2 }
};
child.return = rootFiber;
child._debugOwner = rootFiber;
const rendererId = hook.inject({
  version: "19.2.6",
  bundleType: 1,
  rendererPackageName: "react-dom",
  findFiberByHostInstance(element) {
    return element === ready ? host : null;
  }
});
hook.onCommitFiberRoot(rendererId, { current: rootFiber }, 3, false);
</script>"#
                .to_owned(),
        })
        .await?;

    let result = browser
        .execute(BrowserAction::ReactEvents {
            after: Some(0),
            limit: Some(100),
        })
        .await?;
    let BrowserActionResult::ReactEvents { status, events, .. } = result else {
        return Err(eyre!("expected React diagnostics result"));
    };
    assert!(status.enabled);
    assert!(status.active);
    assert_eq!(
        status.renderer_count, 1,
        "unexpected React status: {status:#?}; events: {events:#?}"
    );
    assert_eq!(status.renderers[0].version, "19.2.6");
    let commit = events
        .iter()
        .find(|event| event.kind == BrowserReactEventKind::Commit)
        .ok_or_else(|| eyre!("missing synthetic React commit: {events:#?}"))?;
    let app = commit
        .tree
        .iter()
        .find(|fiber| fiber.name == "SyntheticApp")
        .ok_or_else(|| eyre!("missing SyntheticApp fiber: {commit:#?}"))?;
    assert!((app.actual_duration_ms - 8.0).abs() < f64::EPSILON);
    assert_eq!(
        app.source.as_ref().map(|source| source.file_name.as_str()),
        Some("src/SyntheticApp.tsx")
    );
    assert!(
        app.change_description
            .as_ref()
            .is_some_and(|change| change.is_first_mount)
    );

    let result = browser
        .execute(BrowserAction::ElementContext {
            target: BrowserTarget::css("#ready"),
        })
        .await?;
    let BrowserActionResult::ElementContext { context, .. } = result else {
        return Err(eyre!("expected element context result"));
    };
    assert_eq!(context.component_name.as_deref(), Some("SyntheticChild"));
    assert_eq!(
        context
            .source
            .as_ref()
            .map(|source| source.file_name.as_str()),
        Some("src/SyntheticChild.tsx")
    );
    assert!(context.html_preview.contains("synthetic react"));
    assert!(context.selector.is_some());
    assert!(!context.styles.is_empty());

    browser.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn browser_is_usable_without_the_nanocodex_tool_adapter() -> Result<()> {
    let browser = Browser::new()?;
    browser.start().await?;
    browser.start().await?;
    browser
        .execute(BrowserAction::Open {
            url: "data:text/html,<main><button>Save</button></main>".to_owned(),
        })
        .await?;
    let snapshot = browser
        .execute(BrowserAction::Snapshot {
            interactive: true,
            compact: false,
            depth: None,
            selector: None,
            include_urls: false,
        })
        .await?;
    let BrowserActionResult::Snapshot { snapshot, refs, .. } = snapshot else {
        return Err(eyre!("expected snapshot result"));
    };
    assert!(snapshot.contains(r#"button "Save""#));
    assert!(
        refs.values()
            .any(|element| element.role == "button" && element.name == "Save")
    );

    browser.close().await?;
    assert!(browser.execute(BrowserAction::GetUrl).await.is_err());
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
#[allow(
    clippy::too_many_lines,
    reason = "one live browser scenario validates semantic, complete DOM, and diagnostics contracts"
)]
async fn snapshot_references_cross_open_shadow_roots() -> Result<()> {
    let browser = Browser::new()?;
    browser
        .execute(BrowserAction::Open {
            url: r#"data:text/html,<shadow-app></shadow-app><closed-app></closed-app><script>
customElements.define("shadow-app",class extends HTMLElement{
  connectedCallback(){
    const root=this.attachShadow({mode:"open"});
    root.innerHTML="<button aria-label='Shadow Save'>Save</button><input aria-label='Shadow Name'><output>idle</output>";
    root.querySelector("button").onclick=()=>root.querySelector("output").textContent="saved";
  }
});
customElements.define("closed-app",class extends HTMLElement{
  connectedCallback(){
    const root=this.attachShadow({mode:"closed"});
    root.innerHTML="<section data-closed-shadow='present'>Closed content</section>";
  }
});
</script>"#
                .to_owned(),
        })
        .await?;
    let snapshot = browser
        .execute(BrowserAction::Snapshot {
            interactive: true,
            compact: true,
            depth: None,
            selector: None,
            include_urls: false,
        })
        .await?;
    let BrowserActionResult::Snapshot { snapshot, refs, .. } = snapshot else {
        return Err(eyre!("expected snapshot result"));
    };
    assert!(snapshot.contains(r#"button "Shadow Save""#));
    assert!(snapshot.contains(r#"textbox "Shadow Name""#));
    let complete = browser
        .execute(BrowserAction::DomSnapshot {
            computed_styles: vec!["display".to_owned(), "visibility".to_owned()],
            include_dom_rects: true,
            include_paint_order: true,
        })
        .await?;
    let BrowserActionResult::DomSnapshot { snapshot, .. } = complete else {
        return Err(eyre!("expected complete DOM snapshot result"));
    };
    let nodes = snapshot
        .documents
        .iter()
        .flat_map(|document| document.nodes.iter())
        .collect::<Vec<_>>();
    assert!(snapshot.shadow_tree_node_count > 0);
    assert!(nodes.iter().any(|node| node.node_name == "CLOSED-APP"));
    assert!(nodes.iter().any(|node| {
        node.attributes
            .get("data-closed-shadow")
            .is_some_and(|value| value == "present")
    }));
    assert!(nodes.iter().any(|node| {
        node.layout
            .as_ref()
            .is_some_and(|layout| layout.styles.contains_key("display"))
    }));
    let button = refs
        .iter()
        .find(|(_, element)| element.name == "Shadow Save")
        .map(|(reference, _)| format!("@{reference}"))
        .ok_or_else(|| eyre!("missing shadow button reference"))?;
    let input = refs
        .iter()
        .find(|(_, element)| element.name == "Shadow Name")
        .map(|(reference, _)| format!("@{reference}"))
        .ok_or_else(|| eyre!("missing shadow input reference"))?;

    browser
        .execute(BrowserAction::Fill {
            target: BrowserTarget::reference(input.clone()),
            text: "Nanocodex".to_owned(),
        })
        .await?;
    let value = browser
        .execute(BrowserAction::GetValue {
            target: BrowserTarget::reference(input),
        })
        .await?;
    assert!(matches!(
        value,
        BrowserActionResult::Value {
            value: Some(value),
            ..
        } if value == "Nanocodex"
    ));
    browser
        .execute(BrowserAction::Click {
            target: BrowserTarget::reference(button),
            options: None,
        })
        .await?;
    let output = browser
        .execute(BrowserAction::GetText {
            target: BrowserTarget::css("output"),
        })
        .await?;
    assert!(matches!(
        output,
        BrowserActionResult::Text { text, .. } if text == "saved"
    ));
    let count = browser
        .execute(BrowserAction::GetCount {
            target: BrowserTarget::role("button"),
        })
        .await?;
    assert!(matches!(count, BrowserActionResult::Count { count: 1, .. }));
    browser.execute(BrowserAction::Reload).await?;
    let reloaded = browser
        .execute(BrowserAction::Snapshot {
            interactive: true,
            compact: true,
            depth: None,
            selector: None,
            include_urls: false,
        })
        .await?;
    assert!(matches!(
        reloaded,
        BrowserActionResult::Snapshot { snapshot, .. }
            if snapshot.contains(r#"button "Shadow Save""#)
    ));
    browser
        .execute(BrowserAction::Evaluate {
            expression:
                "fetch('http://127.0.0.1:65534/nanocodex-browser-test').then(() => true, () => false)"
                    .to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::WaitForTimeout { milliseconds: 50 })
        .await?;
    let failed = browser
        .execute(BrowserAction::NetworkRequests {
            filter: Some("nanocodex-browser-test".to_owned()),
            after: None,
            limit: Some(10),
        })
        .await?;
    assert!(matches!(
        failed,
        BrowserActionResult::NetworkRequests { requests, .. }
            if requests
                .iter()
                .any(|request| request.failure.is_some())
    ));

    browser.close().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
async fn worker_network_is_captured_before_module_execution() -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(serve_worker_fixture(listener));
    let browser = Browser::builder()
        .egress_policy(BrowserEgressPolicy::deny_by_default().allow_loopback(true))
        .build()?;
    browser
        .execute(BrowserAction::NetworkRoute {
            route_id: "worker-dependency".to_owned(),
            url_contains: "/dep.js".to_owned(),
            response: BrowserRouteResponse {
                status: 200,
                headers: vec![BrowserRouteHeader {
                    name: "content-type".to_owned(),
                    value: "text/javascript".to_owned(),
                }],
                body: "export const value = 'worker-routed';".to_owned(),
            },
        })
        .await?;
    browser
        .execute(BrowserAction::Open {
            url: format!("http://{address}/"),
        })
        .await?;

    let mut worker_ready = false;
    for _ in 0..50 {
        let state = browser
            .execute(BrowserAction::GetText {
                target: BrowserTarget::css("#state"),
            })
            .await?;
        if matches!(state, BrowserActionResult::Text { text, .. } if text == "worker-routed") {
            worker_ready = true;
            break;
        }
        browser
            .execute(BrowserAction::WaitForTimeout { milliseconds: 50 })
            .await?;
    }
    assert!(worker_ready, "module worker did not finish initialization");

    let requests = browser
        .execute(BrowserAction::NetworkRequests {
            filter: Some("/dep.js".to_owned()),
            after: None,
            limit: Some(10),
        })
        .await?;
    let BrowserActionResult::NetworkRequests { requests, .. } = requests else {
        return Err(eyre!("expected network request result"));
    };
    let dependency = requests
        .iter()
        .find(|request| {
            request.context == BrowserNetworkContext::ChildTarget
                && request.url.ends_with("/dep.js")
                && request.completed
        })
        .ok_or_else(|| eyre!("missing completed worker dependency request: {requests:#?}"))?;
    let body = browser
        .execute(BrowserAction::NetworkBody {
            request_id: dependency.request_id.clone(),
            kind: BrowserNetworkBodyKind::Response,
        })
        .await?;
    assert!(matches!(
        body,
        BrowserActionResult::NetworkBody {
            body,
            base64_encoded: false,
            ..
        } if body.contains("worker-routed")
    ));

    browser.close().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local Chrome or Chromium installation"]
#[allow(
    clippy::too_many_lines,
    reason = "one real session proves deterministic policy, native input, DevTools reads, trace replay, and state transfer together"
)]
async fn browser_context_trace_and_devtools_contract() -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(serve_action_fixture(listener));
    let origin = format!("http://{address}");
    let initial_state = BrowserStorageState {
        cookies: Vec::new(),
        origins: vec![BrowserOriginStorage {
            origin: origin.clone(),
            local_storage: BTreeMap::from([("seed".to_owned(), "from-harness".to_owned())]),
            session_storage: BTreeMap::new(),
        }],
    };
    let context = BrowserContext::default()
        .viewport(BrowserViewport::desktop(1024, 640))
        .locale("fr-FR")
        .timezone("Europe/Paris")
        .color_scheme(BrowserColorScheme::Dark)
        .reduced_motion(BrowserReducedMotion::Reduce)
        .init_script("globalThis.__nanocodexContextInstalled = true;");
    let browser = Browser::builder()
        .context(context.clone())
        .storage_state(initial_state)
        .crux_client(
            BrowserCruxClient::new("crux-secret")
                .endpoint(url::Url::parse(&format!("{origin}/crux"))?),
        )
        .build()?;

    browser
        .execute(BrowserAction::SessionTraceStart {
            screenshots: true,
            dom_snapshots: true,
            max_actions: Some(10),
        })
        .await?;
    browser
        .execute(BrowserAction::Open {
            url: format!("{origin}/"),
        })
        .await?;
    let trace = browser.execute(BrowserAction::SessionTraceStop).await?;
    let BrowserActionResult::SessionTrace { trace, .. } = trace else {
        return Err(eyre!("expected session trace"));
    };
    assert_eq!(trace.action_count, 2);
    assert_eq!(trace.screenshot_count, 2);
    assert!(trace.events_path.is_file());
    assert!(trace.network_path.is_file());
    assert!(trace.diagnostics_path.is_file());
    let entries = trace.entries().await?;
    assert_eq!(entries.len(), 2);
    assert_eq!(trace.dom_snapshot_count, 2, "{entries:#?}");
    assert!(entries.iter().all(|entry| {
        entry
            .screenshot_path
            .as_ref()
            .is_some_and(|path| path.is_file())
            && entry
                .dom_snapshot_path
                .as_ref()
                .is_some_and(|path| path.is_file())
    }));
    let retained_trace_root = tempfile::tempdir()?;
    let retained_trace = trace
        .persist(retained_trace_root.path().join("trace"))
        .await?;
    assert!(retained_trace.events_path.is_file());
    assert!(retained_trace.entries().await?.iter().all(|entry| {
        entry
            .screenshot_path
            .as_ref()
            .is_some_and(|path| path.starts_with(&retained_trace.directory))
            && entry
                .dom_snapshot_path
                .as_ref()
                .is_some_and(|path| path.starts_with(&retained_trace.directory))
    }));

    let policy = browser
        .execute(BrowserAction::Evaluate {
            expression: r#"({
  width: innerWidth,
  height: innerHeight,
  language: navigator.language,
  timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
  dark: matchMedia("(prefers-color-scheme: dark)").matches,
  reduced: matchMedia("(prefers-reduced-motion: reduce)").matches,
  initialized: globalThis.__nanocodexContextInstalled,
  seed: localStorage.getItem("seed")
})"#
            .to_owned(),
        })
        .await?;
    let BrowserActionResult::Evaluation { value: policy, .. } = policy else {
        return Err(eyre!("expected context policy result"));
    };
    assert_eq!(policy["width"], 1024);
    assert_eq!(policy["height"], 640);
    assert_eq!(policy["language"], "fr-FR");
    assert_eq!(policy["timezone"], "Europe/Paris");
    assert_eq!(policy["dark"], true);
    assert_eq!(policy["reduced"], true);
    assert_eq!(policy["initialized"], true);
    assert_eq!(policy["seed"], "from-harness");

    browser
        .execute(BrowserAction::Evaluate {
            expression: r##"(() => {
const style = document.createElement("style");
style.textContent = "#action:hover { color: rgb(1, 2, 3); }";
document.head.append(style);
const label = document.createElement("label");
label.textContent = "Account";
const input = document.createElement("input");
input.placeholder = "Native text";
input.dataset.testid = "account-input";
input.addEventListener("keydown", event => globalThis.__lastRawKey = event.key);
label.append(input);
document.body.append(label);
localStorage.setItem("captured", "yes");
return true;
})()"##
                .to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::Fill {
            target: BrowserTarget::label("Account").exact(),
            text: "semantic".to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::Click {
            target: BrowserTarget::test_id("account-input"),
            options: None,
        })
        .await?;
    browser
        .execute(BrowserAction::InsertText {
            text: " native".to_owned(),
        })
        .await?;
    browser
        .execute(BrowserAction::KeyboardDown {
            key: "Enter".to_owned(),
            modifiers: Vec::new(),
        })
        .await?;
    browser
        .execute(BrowserAction::KeyboardUp {
            key: "Enter".to_owned(),
            modifiers: Vec::new(),
        })
        .await?;
    let raw_input = browser
        .execute(BrowserAction::Evaluate {
            expression: r#"({
  value: document.querySelector("[data-testid=account-input]").value,
  key: globalThis.__lastRawKey
})"#
            .to_owned(),
        })
        .await?;
    assert!(matches!(
        raw_input,
        BrowserActionResult::Evaluation { value, .. }
            if value["value"] == "semantic native" && value["key"] == "Enter"
    ));

    browser
        .execute(BrowserAction::ForcePseudoState {
            target: BrowserTarget::role("button").named("Mapped Action").exact(),
            pseudo_classes: vec![BrowserPseudoClass::Hover],
        })
        .await?;
    let styles = browser
        .execute(BrowserAction::MatchedStyles {
            target: BrowserTarget::role("button").named("Mapped Action").exact(),
        })
        .await?;
    let BrowserActionResult::MatchedStyles { styles, .. } = styles else {
        return Err(eyre!("expected authored styles"));
    };
    assert!(
        styles
            .rules
            .iter()
            .any(|rule| rule.selector.contains("#action:hover")),
        "{styles:#?}"
    );
    let listeners = browser
        .execute(BrowserAction::EventListeners {
            target: BrowserTarget::css("#action"),
            depth: Some(1),
            pierce: true,
        })
        .await?;
    assert!(matches!(
        listeners,
        BrowserActionResult::EventListeners { listeners, .. }
            if listeners.iter().any(|listener| listener.event_type == "click")
    ));
    let storage = browser.execute(BrowserAction::StorageInspect).await?;
    assert!(matches!(
        storage,
        BrowserActionResult::Storage { storage, .. }
            if storage.local_storage_keys
                == vec!["captured".to_owned(), "seed".to_owned()]
    ));

    assert!(matches!(
        browser
            .execute(BrowserAction::LighthouseAudit {
                categories: Vec::new(),
                form_factor: None,
            })
            .await,
        Err(BrowserError::LighthouseNotConfigured)
    ));
    let crux = browser
        .execute(BrowserAction::Crux {
            scope: BrowserCruxScope::default(),
            form_factor: None,
        })
        .await?;
    assert!(matches!(
        crux,
        BrowserActionResult::Crux { report, .. }
            if report.metrics.len() == 1
                && report.metrics[0].name == "largest_contentful_paint"
                && report.metrics[0].p75 == Some(2_500.0)
    ));

    let captured_state = browser.storage_state().await?;
    browser.close().await?;
    let replay = Browser::builder().context(context).build()?;
    let replayed = replay.replay(&retained_trace).await?;
    assert_eq!(replayed.len(), 1);
    replay.restore_storage_state(captured_state).await?;
    replay.execute(BrowserAction::Reload).await?;
    let restored = replay
        .execute(BrowserAction::Evaluate {
            expression: "localStorage.getItem('captured')".to_owned(),
        })
        .await?;
    assert!(matches!(
        restored,
        BrowserActionResult::Evaluation {
            value: Value::String(value),
            ..
        } if value == "yes"
    ));

    replay.close().await?;
    server.abort();
    Ok(())
}

#[test]
fn browser_context_rejects_unbounded_viewports() {
    let result = Browser::builder()
        .context(BrowserContext::default().viewport(BrowserViewport::desktop(
            super::MAX_VIEWPORT_DIMENSION + 1,
            720,
        )))
        .build();
    assert!(matches!(
        result,
        Err(BrowserBuildError::Configuration { .. })
    ));
}

#[tokio::test]
#[ignore = "requires local Chrome and NANOCODEX_TEST_LIGHTHOUSE pointing to the Chrome Lighthouse CLI"]
async fn exact_lighthouse_audit_attaches_to_the_owned_chrome_session() -> Result<()> {
    let executable = std::env::var_os("NANOCODEX_TEST_LIGHTHOUSE")
        .ok_or_else(|| eyre!("NANOCODEX_TEST_LIGHTHOUSE is not configured"))?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(serve_action_fixture(listener));
    let page_url = format!("http://{address}/");
    let browser = Browser::builder()
        .lighthouse_executable(executable)
        .build()?;
    browser
        .execute(BrowserAction::Open {
            url: page_url.clone(),
        })
        .await?;

    let result = browser
        .execute(BrowserAction::LighthouseAudit {
            categories: vec![BrowserLighthouseCategory::Accessibility],
            form_factor: Some(BrowserLighthouseFormFactor::Desktop),
        })
        .await?;
    let BrowserActionResult::Lighthouse { report, .. } = result else {
        return Err(eyre!("expected Lighthouse report"));
    };
    assert!(report.path.is_file());
    assert!(!report.lighthouse_version.is_empty());
    assert_eq!(report.final_url, page_url);
    assert_eq!(report.categories.len(), 1);
    assert_eq!(
        report.categories[0].category,
        BrowserLighthouseCategory::Accessibility
    );
    assert!(report.categories[0].score.is_some());

    browser.close().await?;
    server.abort();
    Ok(())
}

async fn serve_action_fixture(listener: TcpListener) -> std::io::Result<()> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            let mut request = [0_u8; 4_096];
            let bytes = stream.read(&mut request).await?;
            let request = String::from_utf8_lossy(&request[..bytes]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let (status, content_type, body) = match path {
                "/app.js" => (
                    "200 OK",
                    "text/javascript",
                    ACTION_FIXTURE_JAVASCRIPT.to_owned(),
                ),
                "/slow" => {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    ("200 OK", "text/plain", "settled".to_owned())
                }
                "/crux?key=crux-secret" => ("200 OK", "application/json", CRUX_FIXTURE.to_owned()),
                path if path.starts_with("/crux") => {
                    ("401 Unauthorized", "application/json", "{}".to_owned())
                }
                _ => ("200 OK", "text/html", ACTION_FIXTURE_HTML.to_owned()),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await?;
            stream.shutdown().await
        });
    }
}

async fn serve_persistent_profile_fixture(listener: TcpListener) -> std::io::Result<()> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            let mut request = [0_u8; 4_096];
            let bytes = stream.read(&mut request).await?;
            if bytes == 0 {
                return Ok(());
            }
            let request = String::from_utf8_lossy(&request[..bytes]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let authenticated = request.lines().any(|line| {
                line.to_ascii_lowercase().starts_with("cookie:")
                    && line.contains("durable_auth=opaque")
            });
            let (cookie, body) = match path {
                "/login" => (
                    "Set-Cookie: durable_auth=opaque; Path=/; Max-Age=3600; HttpOnly; SameSite=Lax\r\n",
                    "signed-in",
                ),
                "/logout" => (
                    "Set-Cookie: durable_auth=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax\r\n",
                    "signed-out",
                ),
                _ if authenticated => ("", "authenticated"),
                _ => ("", "signed-out"),
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n{cookie}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await?;
            stream.shutdown().await
        });
    }
}

async fn serve_passkey_fixture(listener: TcpListener) -> std::io::Result<()> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            let mut request = [0_u8; 4_096];
            if stream.read(&mut request).await? == 0 {
                return Ok(());
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PASSKEY_FIXTURE_HTML}",
                PASSKEY_FIXTURE_HTML.len()
            );
            stream.write_all(response.as_bytes()).await?;
            stream.shutdown().await
        });
    }
}

const PASSKEY_FIXTURE_HTML: &str = r##"<!doctype html>
<button id="register">Register passkey</button>
<button id="authenticate">Authenticate passkey</button>
<output id="status" role="status">idle</output>
<script>
const challenge = () => crypto.getRandomValues(new Uint8Array(32));
const status = document.querySelector("#status");
document.querySelector("#register").addEventListener("click", async () => {
  try {
    const credential = await navigator.credentials.create({ publicKey: {
      challenge: challenge(),
      rp: { id: location.hostname, name: "Nanocodex passkey fixture" },
      user: {
        id: new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]),
        name: "tester@nanocodex.invalid",
        displayName: "Nanocodex Tester"
      },
      pubKeyCredParams: [{ type: "public-key", alg: -7 }],
      authenticatorSelection: {
        authenticatorAttachment: "platform",
        residentKey: "required",
        userVerification: "required"
      },
      attestation: "none",
      timeout: 5000
    }});
    status.textContent = credential ? "registered" : "registration failed";
  } catch (error) {
    status.textContent = `error:${error.name}:${error.message}`;
  }
});
document.querySelector("#authenticate").addEventListener("click", async () => {
  try {
    const credential = await navigator.credentials.get({ publicKey: {
      challenge: challenge(),
      rpId: location.hostname,
      userVerification: "required",
      timeout: 5000
    }});
    status.textContent = credential ? "authenticated" : "authentication failed";
  } catch (error) {
    status.textContent = `error:${error.name}:${error.message}`;
  }
});
</script>"##;

const ACTION_FIXTURE_HTML: &str = r#"<!doctype html>
<style>
body { min-height: 600px; }
#overlay { position: fixed; inset: 0; z-index: 1000; background: rgb(1 2 3 / 0.01); }
#source, #target { display: inline-block; width: 100px; height: 40px; }
</style>
<button id="action">Mapped Action</button>
<button id="double">Double Action</button>
<button id="modified">Modified Action</button>
<output id="status" role="status" tabindex="0">idle</output>
<div id="source" draggable="true">source</div>
<div id="target">target</div>
<iframe title="fixture frame" srcdoc="<button id='child' onclick=&quot;parent.postMessage('child:' + event.isTrusted, '*')&quot;>Child Action</button>"></iframe>
<script src="/app.js"></script>"#;

const ACTION_FIXTURE_JAVASCRIPT: &str = r##"function mappedFailure(){throw new Error("mapped boom")}function coveredWork(){return 42}function unusedWork(){return 7}document.querySelector("#action").addEventListener("click",event=>{document.querySelector("#status").textContent=event.isTrusted?"trusted":"synthetic";console.log("action console",coveredWork());fetch("/slow").then(()=>document.querySelector("#status").textContent+=":done");setTimeout(mappedFailure,10)});document.querySelector("#double").addEventListener("dblclick",()=>globalThis.doubleClicked=true);document.querySelector("#modified").addEventListener("click",event=>globalThis.shiftClicked=event.shiftKey);document.querySelector("#target").addEventListener("dragover",event=>event.preventDefault());document.querySelector("#target").addEventListener("drop",()=>globalThis.dropped=true);addEventListener("message",event=>document.querySelector("#status").textContent=event.data);
//# sourceMappingURL=data:application/json;base64,eyJ2ZXJzaW9uIjozLCJmaWxlIjoiYXBwLmpzIiwic291cmNlcyI6WyJzcmMvZml4dHVyZS50cyJdLCJzb3VyY2VzQ29udGVudCI6WyJleHBvcnQgZnVuY3Rpb24gbWFwcGVkRmFpbHVyZSgpOiBuZXZlciB7IHRocm93IG5ldyBFcnJvcihcIm1hcHBlZCBib29tXCIpOyB9Il0sIm5hbWVzIjpbIm1hcHBlZEZhaWx1cmUiXSwibWFwcGluZ3MiOiJBQUFBQSJ9"##;

const CRUX_FIXTURE: &str = r#"{
  "record": {
    "key": {
      "url": "http://fixture.test/",
      "formFactor": "DESKTOP"
    },
    "metrics": {
      "largest_contentful_paint": {
        "histogram": [
          { "start": 0, "end": 2500, "density": 0.75 },
          { "start": 2500, "density": 0.25 }
        ],
        "percentiles": { "p75": 2500 }
      }
    },
    "collectionPeriod": {
      "firstDate": { "year": 2026, "month": 6, "day": 1 },
      "lastDate": { "year": 2026, "month": 6, "day": 28 }
    }
  }
}"#;

async fn serve_worker_fixture(listener: TcpListener) -> std::io::Result<()> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut request = [0_u8; 4_096];
        let bytes = stream.read(&mut request).await?;
        let request = String::from_utf8_lossy(&request[..bytes]);
        let path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/");
        let (content_type, body) = match path {
            "/worker.js" => (
                "text/javascript",
                "import { value } from './dep.js'; postMessage(value);",
            ),
            "/dep.js" => ("text/javascript", "export const value = 'worker-ready';"),
            _ => (
                "text/html",
                r##"<!doctype html><main id="state">waiting</main><script>
const worker = new Worker("/worker.js", { type: "module" });
worker.onmessage = ({ data }) => {
  document.querySelector("#state").textContent = data;
};
</script>"##,
            ),
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
    }
}

async fn serve_appium_fixture(listener: TcpListener) -> std::io::Result<()> {
    let responses = [
        serde_json::json!({"value":{"ready":true}}),
        serde_json::json!({"value":{"sessionId":"ios-session","capabilities":{}}}),
        serde_json::json!({"value":{
            "provider":"ios_webdriver","engine":"webkit","url":"https://example.com/",
            "viewportWidth":393.0,"viewportHeight":852.0,
            "visualViewportWidth":393.0,"visualViewportHeight":852.0,"visualViewportScale":1.0,
            "screenWidth":393.0,"screenHeight":852.0,"devicePixelRatio":3.0,
            "userAgent":"Mozilla/5.0 (iPhone) AppleWebKit/605.1.15 Mobile Safari/604.1",
            "platform":"iPhone","maxTouchPoints":5,"coarsePointer":true,"noHover":true,
            "orientation":"portrait","metaViewport":"width=device-width,initial-scale=1",
            "verified":true,"mismatches":[]
        }}),
        serde_json::json!({"value":null}),
    ];
    for (index, response) in responses.into_iter().enumerate() {
        let (mut stream, _) = listener.accept().await?;
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let body_start = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map_or(request.len(), |position| position + 4);
                if request.len().saturating_sub(body_start) >= content_length {
                    break;
                }
            }
        }
        let request = String::from_utf8_lossy(&request);
        match index {
            0 => assert!(request.starts_with("GET /status ")),
            1 => {
                assert!(request.starts_with("POST /session "));
                assert!(request.contains(r#""appium:automationName":"XCUITest""#));
                assert!(request.contains(r#""appium:deviceName":"iPhone 16 Pro""#));
            }
            2 => assert!(request.starts_with("POST /session/ios-session/execute/sync ")),
            3 => assert!(request.starts_with("DELETE /session/ios-session ")),
            _ => unreachable!(),
        }
        let body = response.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await?;
    }
    Ok(())
}

const MANAGED_BROWSER_SOURCE: &str = r#"
const opened = await tools.browser({
  action: "open",
  url: "data:text/html,<main><button>Save</button><input aria-label='Name'></main>"
});
const snapshot = await tools.browser({ action: "snapshot" });
const name = Object.entries(snapshot.refs)
  .find(([, element]) => element.role === "textbox")?.[0];
if (name === undefined) throw new Error("missing textbox");
await tools.browser({
  action: "fill",
  target: { by: "ref", reference: `@${name}` },
  text: "Nanocodex"
});
const value = await tools.browser({
  action: "get_value",
  target: { by: "ref", reference: `@${name}` }
});
const textResult = await tools.browser({
  action: "get_text",
  target: { by: "css", selector: "main" }
});
const html = await tools.browser({
  action: "get_html",
  target: { by: "css", selector: "main" }
});
const label = await tools.browser({
  action: "get_attribute",
  target: { by: "ref", reference: `@${name}` },
  name: "aria-label"
});
const count = await tools.browser({
  action: "get_count",
  target: { by: "role", role: "textbox" }
});
const bounds = await tools.browser({
  action: "get_box",
  target: { by: "ref", reference: `@${name}` }
});
const styles = await tools.browser({
  action: "get_styles",
  target: { by: "ref", reference: `@${name}` }
});
const evaluation = await tools.browser({
  action: "evaluate",
  expression: "console.log('nanocodex-browser-probe'); setTimeout(() => { throw new Error('nanocodex-browser-error'); }, 0); ({ inputs: document.querySelectorAll('input').length })"
});
await tools.browser({ action: "wait_for_timeout", milliseconds: 50 });
const consoleEntries = await tools.browser({ action: "console" });
const errors = await tools.browser({ action: "errors" });
const requests = await tools.browser({
  action: "network_requests",
});
const screenshot = await tools.browser({ action: "screenshot" });
const { modelImage, ...screenshotImage } = screenshot.image;
if (modelImage === null) throw new Error("missing model-visible screenshot");
image(modelImage);
text({
  opened,
  snapshot,
  value,
  textResult,
  html,
  label,
  count,
  bounds,
  styles,
  evaluation,
  consoleEntries,
  errors,
  requests,
  screenshot: { ...screenshot, image: screenshotImage }
});
"#;

fn context() -> ToolContext<'static> {
    ToolContext::new(
        "test-model",
        "test-session",
        "test-call",
        &[],
        DEFAULT_TOOL_OUTPUT_TOKENS,
    )
}

fn execution_text(output: &ToolOutputBody) -> Result<&str> {
    let ToolOutputBody::Content(content) = output else {
        return Err(eyre!("expected Code Mode content output"));
    };
    content
        .iter()
        .rev()
        .find_map(|content| match content {
            ToolOutputContent::InputText { text } => Some(text.as_str()),
            ToolOutputContent::InputImage { .. }
            | ToolOutputContent::InputAudio { .. }
            | ToolOutputContent::EncryptedContent { .. } => None,
        })
        .ok_or_else(|| eyre!("missing Code Mode text output"))
}
