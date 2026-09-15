//! Public WebMCP admission for unadvertised native providers.
use serde_json::json;
use skyre::browser::Browsers;

#[test]
fn webmcp_unadvertised_providers_refuse_both_fetch_ingresses_without_connecting() {
    use std::{io::ErrorKind, net::TcpListener};
    for extension in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let suffix = if extension {
            "?skyre-provider=extension"
        } else {
            ""
        };
        let mut browsers = Browsers::default();
        browsers
            .register(
                "owned",
                &format!("ws://{}{suffix}", listener.local_addr().unwrap()),
            )
            .unwrap();
        let before = browsers
            .execute("info", &json!({"browser":"owned"}))
            .unwrap();
        assert!(
            before["capabilities"]["tab"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["id"] != "webmcp")
        );
        for (method, args) in [
            (
                "webmcp_list",
                json!({"browser":"owned","tab":"t","capabilities":{"tab":[{"id":"webmcp"}]},"webmcp_enabled":true}),
            ),
            ("webmcp_list", json!({"tab":"t"})),
            (
                "webmcp_list_tools",
                json!({"browser_id":"owned","tab_id":"t","browser":"foreign","tab":"foreign"}),
            ),
        ] {
            let error = browsers.execute(method, &args).unwrap_err();
            assert_eq!(
                error.message,
                "owned does not support command \"webmcp_list_tools\"."
            );
            assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        }
        let after = browsers
            .execute("info", &json!({"browser":"owned"}))
            .unwrap();
        assert_eq!(after, before);
    }
}

#[test]
fn webmcp_missing_and_canonical_alias_registration_refuse_before_connection() {
    let mut b = Browsers::default();
    b.register("owned", "ws://127.0.0.1:1").unwrap();
    for (method, args) in [
        (
            "webmcp_invoke",
            json!({"browser":"owned","tab":"t","name":"echo"}),
        ),
        (
            "webmcp_invoke",
            json!({"browser":"owned","tab":"t","name":"echo","registrationId":"r1"}),
        ),
        (
            "webmcp_invoke_tool",
            json!({"browser_id":"owned","tab_id":"t","tool_name":"echo","registrationId":"r1"}),
        ),
        (
            "webmcp_invoke_tool",
            json!({"browser_id":"owned","tab_id":"t","tool_name":"echo","registration_id":"stale","browser":"other","registrationId":"r1"}),
        ),
    ] {
        let error = b.execute(method, &args).unwrap_err();
        assert_ne!(error.code, -32006, "{error:?}");
    }
}

#[test]
fn webmcp_restricted_command_policy_precedes_schema_and_fetched_state() {
    use skyre::security::{Security, SecurityConfig};
    let mut engine = skyre::engine::Engine::new(Box::new(skyre::fixture::Fixture::default()));
    engine.security = Security::new(SecurityConfig {
        allowed_origins: vec!["https://owned.test".into()],
        ..Default::default()
    })
    .unwrap();
    for args in [
        json!({}),
        json!({"browser_id":"absent","tab_id":"t","tool_name":"echo"}),
        json!({"browser_id":"absent","tab_id":"t","tool_name":"echo","registration_id":"unfetched"}),
    ] {
        assert_eq!(
            engine
                .execute("browser.webmcp_invoke_tool", &args)
                .unwrap_err()
                .code,
            -32010
        );
    }
}
