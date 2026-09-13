use serde_json::{Value, json};
use skyre::{
    browser::{Browsers, iab::RouteConfig, persistence::Store},
    engine::Engine,
    fixture::Fixture,
    host_turns::{Binding, Config, Controller, Event, Phase, Route},
    runtime::{Host, HostOptions},
};
use std::{cell::RefCell, rc::Rc, time::Duration};
fn private_tempdir() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    root
}
fn route() -> Route {
    Route {
        conversation_id: "conversation".into(),
        thread_id: None,
    }
}
fn configured() -> Browsers {
    let mut b = Browsers::default();
    b.register_iab(
        "iab",
        "ws://127.0.0.1:9/owned",
        RouteConfig {
            conversation_id: "conversation".into(),
            thread_id: None,
            window_id: "window".into(),
        },
    )
    .unwrap();
    b
}
fn config(root: &std::path::Path) -> Config {
    Config {
        authority_token: "a".repeat(64),
        state_directory: root.into(),
        bindings: vec![Binding {
            browser_id: "iab".into(),
            route: route(),
            extension_authority_token: None,
        }],
    }
}
fn event(id: &str, sequence: u64, phase: Phase, turn: &str) -> Event {
    Event {
        event_id: id.into(),
        sequence,
        phase,
        route: route(),
        turn_id: turn.into(),
    }
}
#[test]
fn capability_host_events_are_idempotent_ordered_and_survive_restart_without_renderer() {
    let root = private_tempdir();
    let mut b = configured();
    let mut host = Controller::open(config(root.path()), &mut b).unwrap();
    assert!(
        host.event("wrong", event("start", 1, Phase::Started, "turn-1"), &mut b)
            .is_err()
    );
    assert!(b.execute("info", &json!({"browser":"iab"})).is_err());
    let started = event("start", 1, Phase::Started, "turn-1");
    let first = host
        .event(&"a".repeat(64), started.clone(), &mut b)
        .unwrap();
    assert_eq!(
        first["requestMeta"]["x-codex-turn-metadata"]["turn_id"],
        "turn-1"
    );
    assert_eq!(
        b.execute("info", &json!({"browser":"iab"})).unwrap()["metadata"]["codexSessionId"],
        "conversation"
    );
    assert_eq!(
        host.event(&"a".repeat(64), started.clone(), &mut b)
            .unwrap(),
        first
    );
    assert!(
        host.event(
            &"a".repeat(64),
            event("next", 2, Phase::Started, "turn-2"),
            &mut b
        )
        .is_err()
    );
    host.event(
        &"a".repeat(64),
        event("end", 2, Phase::Ended, "turn-1"),
        &mut b,
    )
    .unwrap();
    assert!(b.execute("info", &json!({"browser":"iab"})).is_err());
    drop(host);
    drop(b);
    let mut b = configured();
    let mut host = Controller::open(config(root.path()), &mut b).unwrap();
    assert_eq!(
        host.event(&"a".repeat(64), started, &mut b).unwrap()["requestMeta"],
        Value::Null
    );
    assert!(b.execute("info", &json!({"browser":"iab"})).is_err());
    host.event(
        &"a".repeat(64),
        event("next", 3, Phase::Started, "turn-2"),
        &mut b,
    )
    .unwrap();
    assert!(
        host.event(
            &"a".repeat(64),
            event("forged", 2, Phase::Ended, "turn-2"),
            &mut b
        )
        .is_err()
    );
    assert!(
        host.event(
            &"a".repeat(64),
            event("next", 3, Phase::Started, "different"),
            &mut b
        )
        .is_err()
    );
}
#[test]
fn durable_store_rejects_competing_owners_endpoint_changes_and_public_or_symlink_files() {
    let root = private_tempdir();
    let store = Store::open(root.path(), "same").unwrap();
    store.save(&json!({"state":"owned"})).unwrap();
    assert_eq!(store.load::<Value>().unwrap().unwrap()["state"], "owned");
    assert!(Store::open(root.path(), "same").is_err());
    drop(store);
    assert!(Store::open(root.path(), "same").is_ok());
    let mut b = configured();
    b.enable_iab_recovery("iab", root.path()).unwrap();
    drop(b);
    let mut different = Browsers::default();
    different
        .register_iab(
            "iab",
            "ws://127.0.0.1:9/other",
            RouteConfig {
                conversation_id: "conversation".into(),
                thread_id: None,
                window_id: "window".into(),
            },
        )
        .unwrap();
    assert!(
        different
            .enable_iab_recovery("iab", root.path())
            .unwrap_err()
            .message
            .contains("identity differs")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let unsafe_root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(unsafe_root.path(), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(Store::open(unsafe_root.path(), "x").is_err());
        let link = root.path().join("linked");
        symlink(unsafe_root.path(), &link).unwrap();
        assert!(Store::open(&link, "x").is_err());
    }
}
#[test]
fn trusted_metadata_changes_between_cells_without_mutating_bindings_or_exposing_setters() {
    let engine = Rc::new(RefCell::new(Engine::new(Box::new(Fixture::default()))));
    let mut host=Host::with_dispatch_options(move|method,args|engine.borrow_mut().execute_from_js(method,args),Default::default(),HostOptions{request_meta:Some(json!({"x-codex-turn-metadata":{"turn_id":"one"},"openai/confirmation_policies":"always"})),..Default::default()}).unwrap();
    host.evaluate("let retained=7; nodeRepl.write([retained,nodeRepl.requestMeta['x-codex-turn-metadata'].turn_id,Object.isFrozen(nodeRepl.requestMeta)]);",Duration::from_secs(1)).unwrap();
    host.set_request_meta(Some(
        json!({"x-codex-turn-metadata":{"turn_id":"two"},"not-exposed":"value"}),
    ))
    .unwrap();
    let output=host.evaluate("nodeRepl.write([retained,nodeRepl.requestMeta['x-codex-turn-metadata'].turn_id,Object.keys(nodeRepl.requestMeta),Object.getOwnPropertyDescriptor(nodeRepl,'requestMeta').set]);",Duration::from_secs(1)).unwrap();
    assert!(output.to_string().contains("two"));
    assert!(output.to_string().contains('7'));
    assert!(!output.to_string().contains("not-exposed"));
    let denied = host.evaluate(
        "for (const method of ['host/turn','host/status','host/recover']) {try{await nodeRepl.rpc(method,{authorityToken:'guess',event:{}})}catch(error){nodeRepl.write(error.code)}}",
        Duration::from_secs(1),
    ).unwrap();
    assert_eq!(denied["outputs"][0]["value"], "-32003-32003-32003");
}

#[test]
fn unknown_context_acknowledgement_requires_explicit_verified_resolution_without_replay() {
    use std::{
        net::TcpListener,
        sync::{Arc, Mutex},
    };
    use tungstenite::Message;
    let root = private_tempdir();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}/owned", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let provider = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let request: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(request["method"], "Target.createBrowserContext");
        recorded
            .lock()
            .unwrap()
            .push("Target.createBrowserContext".to_owned());
        // The provider executed creation, then lost the response. No guessing or retry.
        drop(socket);
        loop {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let mut commands = 0;
            while let Ok(message) = socket.read() {
                if !message.is_text() {
                    break;
                }
                let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                let method = request["method"].as_str().unwrap();
                commands += 1;
                recorded.lock().unwrap().push(method.into());
                let value = match method {
                    "Target.getBrowserContexts" => {
                        json!({"browserContextIds":["created-but-unacknowledged"]})
                    }
                    "Target.getTargets" => json!({"targetInfos":[]}),
                    other => panic!("Unexpected resource/input replay: {other}"),
                };
                socket
                    .send(Message::text(
                        json!({"id":request["id"],"result":value}).to_string(),
                    ))
                    .unwrap();
            }
            if commands > 0 {
                break;
            }
        }
    });
    let configured = || {
        let mut b = Browsers::default();
        b.register_iab(
            "iab",
            &endpoint,
            RouteConfig {
                conversation_id: "conversation".into(),
                thread_id: None,
                window_id: "window".into(),
            },
        )
        .unwrap();
        b
    };
    let mut browsers = configured();
    let mut host = Controller::open(config(root.path()), &mut browsers).unwrap();
    let start = event("start", 1, Phase::Started, "turn");
    host.event(&"a".repeat(64), start.clone(), &mut browsers)
        .unwrap();
    assert!(
        browsers
            .execute("new_tab", &json!({"browser":"iab","url":"about:blank"}))
            .is_err()
    );
    let pending = browsers.iab_recovery_status("iab").unwrap()["pending"].clone();
    assert_eq!(pending["method"], "Target.createBrowserContext");
    assert!(
        browsers
            .execute("new_tab", &json!({"browser":"iab"}))
            .unwrap_err()
            .message
            .contains("acknowledgement is unknown")
    );
    drop(host);
    drop(browsers);
    let mut browsers = configured();
    let mut host = Controller::open(config(root.path()), &mut browsers).unwrap();
    host.event(&"a".repeat(64), start, &mut browsers).unwrap();
    let mut args = json!({"browserId":"iab","operationId":pending["id"],"contextId":"absent"});
    assert_eq!(
        host.recovery("wrong", &args, &mut browsers, true)
            .unwrap_err()
            .code,
        -32003
    );
    let missing = host
        .recovery(&"a".repeat(64), &args, &mut browsers, true)
        .unwrap_err();
    assert!(missing.message.contains("absent"), "{missing:?}");
    assert_eq!(
        browsers.iab_recovery_status("iab").unwrap()["pending"],
        pending
    );
    args["contextId"] = json!("created-but-unacknowledged");
    let resolved = host
        .recovery(&"a".repeat(64), &args, &mut browsers, true)
        .unwrap();
    assert!(resolved["pending"].is_null());
    assert_eq!(resolved["resolutions"][0]["operation"], pending["id"]);
    host.recovery(&"a".repeat(64), &args, &mut browsers, true)
        .unwrap();
    args["contextId"] = json!("different");
    assert!(
        host.recovery(&"a".repeat(64), &args, &mut browsers, true)
            .unwrap_err()
            .message
            .contains("different")
    );
    drop(host);
    drop(browsers);
    provider.join().unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|method| method.as_str() == "Target.createBrowserContext")
            .count(),
        1
    );
}

#[test]
fn lost_disposal_acknowledgement_reconciles_absence_and_completes_pending_host_end() {
    use std::net::TcpListener;
    use tungstenite::Message;
    let root = private_tempdir();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}/owned", listener.local_addr().unwrap());
    let provider = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        for method in [
            "Target.createBrowserContext",
            "Target.createTarget",
            "Target.getTargets",
            "Target.disposeBrowserContext",
        ] {
            let request: Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(request["method"], method);
            let mut response = json!({"id":request["id"]});
            match method {
                "Target.createBrowserContext" => {
                    response["result"] = json!({"browserContextId":"owned"})
                }
                "Target.createTarget" => {
                    response["error"] =
                        json!({"code":-32000,"message":"injected target creation rejection"})
                }
                "Target.getTargets" => response["result"] = json!({"targetInfos":[]}),
                "Target.disposeBrowserContext" => break,
                _ => unreachable!(),
            }
            socket.send(Message::text(response.to_string())).unwrap();
        }
        drop(socket); // Disposal committed; acknowledgement lost.
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let request: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(request["method"], "Target.getBrowserContexts");
        socket
            .send(Message::text(
                json!({"id":request["id"],"result":{"browserContextIds":[]}}).to_string(),
            ))
            .unwrap();
        if let Ok(message) = socket.read() {
            assert!(
                !message.is_text(),
                "Disposal or another uncertain action was replayed"
            );
        }
    });
    let configured = || {
        let mut b = Browsers::default();
        b.register_iab(
            "iab",
            &endpoint,
            RouteConfig {
                conversation_id: "conversation".into(),
                thread_id: None,
                window_id: "window".into(),
            },
        )
        .unwrap();
        b
    };
    let mut browsers = configured();
    let mut host = Controller::open(config(root.path()), &mut browsers).unwrap();
    host.event(
        &"a".repeat(64),
        event("start", 1, Phase::Started, "turn"),
        &mut browsers,
    )
    .unwrap();
    assert!(
        browsers
            .execute("new_tab", &json!({"browser":"iab","url":"about:blank"}))
            .is_err()
    );
    let ended = event("end", 2, Phase::Ended, "turn");
    assert!(
        host.event(&"a".repeat(64), ended.clone(), &mut browsers)
            .is_err()
    );
    drop(host);
    drop(browsers);
    let mut browsers = configured();
    let mut host = Controller::open(config(root.path()), &mut browsers).unwrap();
    let receipt = host.event(&"a".repeat(64), ended, &mut browsers).unwrap();
    assert!(receipt["requestMeta"].is_null());
    assert!(browsers.iab_recovery_status("iab").unwrap()["contextId"].is_null());
    drop(host);
    drop(browsers);
    provider.join().unwrap();
}

#[test]
fn partial_multi_provider_lifecycle_restores_each_applied_binding_after_restart() {
    use std::net::TcpListener;
    use tungstenite::Message;
    let root = private_tempdir();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!(
        "ws://{}/owned?skyre-provider=extension",
        listener.local_addr().unwrap()
    );
    let provider = std::thread::spawn(move || {
        // One connection per host process. Inject a lost start/end collaborator
        // result after the preceding IAB binding was already acknowledged.
        for script in [
            vec![("started", false)],
            vec![("started", true), ("ended", false)],
            vec![("ended", true)],
        ] {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            for (phase, success) in script {
                let request: Value =
                    serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
                assert_eq!(request["method"], "Skyre.hostLifecycle");
                assert_eq!(request["params"]["phase"], phase);
                let response = if success {
                    json!({"id":request["id"],"result":{}})
                } else {
                    json!({"id":request["id"],"error":{"code":-32000,"message":"injected lifecycle rejection"}})
                };
                socket.send(Message::text(response.to_string())).unwrap();
            }
            let _ = socket.read();
        }
    });
    let configured = || {
        let mut b = configured();
        b.register("extension", &endpoint).unwrap();
        b
    };
    let configuration = || {
        let mut c = config(root.path());
        c.bindings.push(Binding {
            browser_id: "extension".into(),
            route: route(),
            extension_authority_token: Some("b".repeat(64)),
        });
        c
    };
    let started = event("start", 1, Phase::Started, "turn");
    let ended = event("end", 2, Phase::Ended, "turn");
    let mut browsers = configured();
    let mut host = Controller::open(configuration(), &mut browsers).unwrap();
    assert!(
        host.event(&"a".repeat(64), started.clone(), &mut browsers)
            .is_err()
    );
    drop(host);
    drop(browsers);
    let mut browsers = configured();
    let mut host = Controller::open(configuration(), &mut browsers).unwrap();
    host.event(&"a".repeat(64), started.clone(), &mut browsers)
        .unwrap();
    assert!(browsers.execute("info", &json!({"browser":"iab"})).is_ok());
    assert!(
        browsers
            .execute("info", &json!({"browser":"extension"}))
            .is_ok()
    );
    assert!(
        host.event(&"a".repeat(64), ended.clone(), &mut browsers)
            .is_err()
    );
    assert!(
        browsers
            .execute("info", &json!({"browser":"extension"}))
            .is_err()
    );
    assert!(
        host.event(&"a".repeat(64), started.clone(), &mut browsers)
            .is_err()
    );
    drop(host);
    drop(browsers);
    let mut browsers = configured();
    let mut host = Controller::open(configuration(), &mut browsers).unwrap();
    // This is a trusted Rust assertion of binding restoration, not a model API.
    browsers.select_host_route(Some(route().key()));
    assert!(browsers.execute("info", &json!({"browser":"iab"})).is_err());
    host.event(&"a".repeat(64), ended, &mut browsers).unwrap();
    assert!(
        browsers
            .execute("info", &json!({"browser":"extension"}))
            .is_err()
    );
    drop(host);
    drop(browsers);
    provider.join().unwrap();
}

#[test]
fn distinct_routes_cannot_alias_the_same_transfer_approval_identity() {
    for second in [
        Route {
            conversation_id: "other-parent".into(),
            thread_id: Some("conversation".into()),
        },
        Route {
            conversation_id: "other-parent".into(),
            thread_id: Some("shared-child".into()),
        },
    ] {
        let root = private_tempdir();
        let mut configuration = config(root.path());
        if second.thread_id.as_deref() == Some("shared-child") {
            configuration.bindings[0].route.thread_id = Some("shared-child".into());
        }
        configuration.bindings.push(Binding {
            browser_id: "second".into(),
            route: second,
            extension_authority_token: None,
        });
        let error = Controller::open(configuration, &mut Browsers::default())
            .err()
            .unwrap();
        assert!(error.message.contains("approval identity"), "{error}");
    }
}
