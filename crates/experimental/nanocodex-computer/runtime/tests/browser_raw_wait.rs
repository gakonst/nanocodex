//! Native Engine + owned socket evidence. No renderer or foreign target probes.
#[path = "fixtures/raw_wait_provider.rs"]
mod owned;
use owned::{Provider, event};
use serde_json::{Value, json};
use skyre::{
    engine::Engine,
    fixture::Fixture,
    security::{Security, SecurityConfig},
};
use std::{
    thread,
    time::{Duration, Instant},
};
fn setup() -> (Provider, Engine) {
    let provider = Provider::start();
    let mut engine = Engine::new(Box::new(Fixture::default()));
    engine
        .browsers
        .register("owned", &provider.endpoint)
        .unwrap();
    engine.begin_chooser_cell(1);
    (provider, engine)
}
fn start(engine: &mut Engine, options: Value) -> Value {
    let mut args = options;
    args["browser"] = json!("owned");
    args["tab"] = json!("t");
    engine.execute("browser.cdp_events", &args).unwrap()
}
fn packet(value: &Value) -> Value {
    assert!(value["__skyreRawWait"]["id"].is_string(), "{value}");
    json!({"browser":"owned","tab":"t","__skyreRawWait":{"id":value["__skyreRawWait"]["id"],"op":"poll"}})
}
fn finish(engine: &mut Engine, packet: &Value) -> Value {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let value = engine.execute("browser.cdp_events", packet).unwrap();
        if value.get("__skyreRawWait").is_none() {
            return value;
        }
        assert!(Instant::now() < deadline, "Owned wait did not complete");
        thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn raw_wait_native_packet_match_freezing_zero_and_detach_are_owned() {
    let (p, mut e) = setup();
    let pending = start(&mut e, json!({"timeoutMs":1000,"methods":["Owned.match"]}));
    let token = packet(&pending);
    let before = p.commands.lock().unwrap().len();
    for bad in [
        {
            let mut v = token.clone();
            v["target"] = json!({"targetId":"unselected"});
            v
        },
        {
            let mut v = token.clone();
            v["tab"] = json!("other");
            v
        },
        {
            let mut v = token.clone();
            v["__skyreRawWait"]["op"] = json!("start");
            v
        },
        {
            let mut v = token.clone();
            v["__skyreRawWait"]["id"] = json!("not-native");
            v
        },
    ] {
        assert!(e.execute("browser.cdp_events", &bad).is_err());
    }
    assert_eq!(p.commands.lock().unwrap().len(), before);
    p.emit(vec![event("Owned.other", 1)]);
    p.wait_command("Page.enable");
    assert!(
        e.execute("browser.cdp_events", &token)
            .unwrap()
            .get("__skyreRawWait")
            .is_some()
    );
    p.emit(vec![event("Owned.match", 2), event("Owned.later", 3)]);
    let result = finish(&mut e, &token);
    assert_eq!(result["events"].as_array().unwrap().len(), 1);
    assert_eq!(result["events"][0]["method"], "Owned.match");
    assert_eq!(result["events"][0]["params"]["n"], 2);
    assert_eq!(result["cursor"], result["events"][0]["sequence"]);
    assert!(e.execute("browser.cdp_events", &token).is_err());
    let zero = packet(&start(
        &mut e,
        json!({"timeoutMs":20,"after_sequence":0,"limit":0}),
    ));
    let zero = finish(&mut e, &zero);
    assert_eq!(zero["events"], json!([]));
    assert_eq!(zero["hasMore"], true);
    assert_eq!(zero["cursor"], 3);
    let closed = packet(&start(
        &mut e,
        json!({"timeoutMs":1000,"after_sequence":1e100}),
    ));
    e.execute("browser.close_tab", &json!({"browser":"owned","tab":"t"}))
        .unwrap();
    let before = p.commands.lock().unwrap().len();
    let result = finish(&mut e, &closed);
    assert_eq!(
        result,
        json!({"cursor":3,"events":[],"hasMore":false,"truncated":false})
    );
    assert_eq!(
        p.commands.lock().unwrap().len(),
        before,
        "Frozen detach result must not reattach or issue commands"
    );
}
#[test]
fn raw_wait_native_policy_scope_cell_and_connection_retire_without_revival() {
    let (p, mut e) = setup();
    let old_policy = e.security.clone();
    let token = packet(&start(&mut e, json!({"timeoutMs":1000})));
    let before = p.commands.lock().unwrap().len();
    e.security = Security::default();
    assert!(e.execute("browser.cdp_events", &token).is_err());
    e.security = old_policy;
    assert!(e.execute("browser.cdp_events", &token).is_err());
    assert_eq!(p.commands.lock().unwrap().len(), before);
    let token = packet(&start(&mut e, json!({"timeoutMs":1000})));
    e.finish_chooser_cell("initial", 1, false);
    e.begin_chooser_cell(2);
    assert!(e.execute("browser.cdp_events", &token).is_err());
    let token = packet(&start(&mut e, json!({"timeoutMs":1000})));
    e.select_kernel_scope(&skyre::host_turns::Route {
        conversation_id: "next".into(),
        thread_id: None,
    });
    assert!(e.execute("browser.cdp_events", &token).is_err());
    e.begin_chooser_cell(3);
    let token = packet(&start(&mut e, json!({"timeoutMs":1000})));
    e.reset_kernel_resources().unwrap();
    assert!(e.execute("browser.cdp_events", &token).is_err());
    e.begin_chooser_cell(4);
    let token = packet(&start(&mut e, json!({"timeoutMs":1000})));
    p.disconnect();
    assert!(e.execute("browser.cdp_events", &token).is_err());
    let before = p.commands.lock().unwrap().len();
    assert!(e.execute("browser.cdp_events", &token).is_err());
    assert_eq!(
        p.commands.lock().unwrap().len(),
        before,
        "Old nonce must not reconnect"
    );
}
#[test]
fn raw_wait_unsupported_pending_domains_preserve_immediate_admission() {
    let (_p, mut e) = setup();
    assert_eq!(start(&mut e, json!({"timeoutMs":0}))["events"], json!([]));
    assert!(
        e.execute(
            "browser.cdp_events",
            &json!({"browser":"owned","tab":"t","timeoutMs":1e100})
        )
        .is_err()
    );
    e.finish_chooser_cell("initial", 1, false);
    assert!(
        e.execute(
            "browser.cdp_events",
            &json!({"browser":"owned","tab":"t","timeoutMs":10})
        )
        .is_err()
    );
    assert_eq!(start(&mut e, json!({"timeoutMs":0}))["events"], json!([]));
    e.begin_chooser_cell(2);
    e.security = Security::new(SecurityConfig {
        allowed_origins: vec!["https://owned.example".into()],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(start(&mut e, json!({"timeoutMs":0}))["events"], json!([]));
    let error = e
        .execute(
            "browser.cdp_events",
            &json!({"browser":"owned","tab":"t","timeoutMs":10}),
        )
        .unwrap_err();
    assert!(
        error.message.contains("restricted origin policy"),
        "{error}"
    );
    let guessed = json!({"method":"cdp_events","args":{"browser":"owned","tab":"t","__skyreRawWait":{"id":format!("raw-events-{}","a".repeat(64)),"op":"poll"}}});
    e.security = Security::new(SecurityConfig {
        require_origin_approval: true,
        ..Default::default()
    })
    .unwrap();
    assert!(
        e.execute("browser.origin_operation_start", &guessed)
            .unwrap_err()
            .message
            .contains("cannot start a new origin operation")
    );
}

#[cfg(unix)]
#[test]
fn raw_wait_guardian_loss_and_replacement_cannot_revive_old_packet() {
    use std::os::unix::fs::PermissionsExt;
    let (p, mut e) = setup();
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let program = dir.path().join("monitor");
    std::fs::write(&state, r#"{"locked":false,"revision":1}"#).unwrap();
    let path = serde_json::to_string(state.to_str().unwrap()).unwrap();
    std::fs::write(&program,format!("#!/usr/bin/python3\nimport json,sys\njson.load(sys.stdin)\nprint(open({path}).read())\n")).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    e.guardian_monitor =
        Some(skyre::process_rpc::Program::new(&program, Duration::from_secs(2)).unwrap());
    let lease = e
        .execute("guardian.acquire", &json!({"ttl_ms":30000}))
        .unwrap();
    let start = e
        .execute(
            "browser.cdp_events",
            &json!({"browser":"owned","tab":"t","timeoutMs":1000,"guardianLease":lease["lease"]}),
        )
        .unwrap();
    let mut token = packet(&start);
    token["guardianLease"] = lease["lease"].clone();
    let before = p.commands.lock().unwrap().len();
    std::fs::write(&state, r#"{"locked":true,"revision":2}"#).unwrap();
    assert!(e.execute("browser.cdp_events", &token).is_err());
    std::fs::write(&state, r#"{"locked":false,"revision":3}"#).unwrap();
    let fresh = e
        .execute("guardian.acquire", &json!({"ttl_ms":30000}))
        .unwrap();
    token["guardianLease"] = fresh["lease"].clone();
    assert!(e.execute("browser.cdp_events", &token).is_err());
    assert_eq!(p.commands.lock().unwrap().len(), before);
}

#[test]
fn raw_wait_cursor_checkpoint_matches_original_across_actual_attachment_receipt() {
    for (options, included) in [
        (json!({}), false),
        (json!({"timeoutMs":0}), true),
        (json!({"timeoutMs":1e100}), true),
        (json!({"after_sequence":0}), true),
    ] {
        let p = Provider::with_attachment_event(Some(event("Owned.attach", 1)));
        let mut e = Engine::new(Box::new(Fixture::default()));
        e.browsers.register("owned", &p.endpoint).unwrap();
        e.begin_chooser_cell(1);
        let result = start(&mut e, options.clone());
        assert_eq!(result["cursor"], 1, "{options}: {result}");
        assert_eq!(
            result["events"].as_array().unwrap().len(),
            usize::from(included),
            "{options}: {result}"
        );
    }
}

#[cfg(unix)]
#[test]
fn raw_wait_guardian_is_rechecked_after_receipt_before_delivery() {
    use std::os::unix::fs::PermissionsExt;
    for terminal in [false, true] {
        let (p, mut e) = setup();
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("monitor-state.json");
        let program = dir.path().join("monitor");
        let save = |responses: Value| {
            std::fs::write(&state, json!({"responses":responses,"reads":0}).to_string()).unwrap();
        };
        save(json!([{"locked":false,"revision":1}]));
        let path = serde_json::to_string(state.to_str().unwrap()).unwrap();
        std::fs::write(&program,format!("#!/usr/bin/python3\nimport json,sys\njson.load(sys.stdin)\np={path}\ns=json.load(open(p))\nr=s['responses'].pop(0) if len(s['responses'])>1 else s['responses'][0]\ns['reads']+=1\njson.dump(s,open(p,'w'))\nprint(json.dumps(r))\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        e.guardian_monitor =
            Some(skyre::process_rpc::Program::new(&program, Duration::from_secs(2)).unwrap());
        let lease = e
            .execute("guardian.acquire", &json!({"ttl_ms":30000}))
            .unwrap();
        let response = e.execute("browser.cdp_events", &json!({"browser":"owned","tab":"t","timeoutMs":if terminal {1}else{1000},"guardianLease":lease["lease"]})).unwrap();
        let mut token = packet(&response);
        token["guardianLease"] = lease["lease"].clone();
        if !terminal {
            let healthy = e.execute("browser.cdp_events", &token).unwrap();
            assert_eq!(
                healthy, response,
                "Unchanged healthy lease preserves pending reads"
            );
        } else {
            thread::sleep(Duration::from_millis(2));
        }
        // Deterministic trusted monitor transition between the outer admission
        // and final result admission; no target-side timing or access probe.
        save(json!([{"locked":false,"revision":1},{"locked":true,"revision":2}]));
        let before = p.commands.lock().unwrap().len();
        let error = e.execute("browser.cdp_events", &token).unwrap_err();
        assert!(error.message.contains("locked"), "{error}");
        let observed: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
        assert_eq!(
            observed["reads"], 2,
            "Pre-receipt and post-receipt native admission are both required"
        );
        assert_eq!(p.commands.lock().unwrap().len(), before);
        save(json!([{"locked":false,"revision":3}]));
        let fresh = e
            .execute("guardian.acquire", &json!({"ttl_ms":30000}))
            .unwrap();
        token["guardianLease"] = fresh["lease"].clone();
        assert!(
            e.execute("browser.cdp_events", &token).is_err(),
            "Suppressed pending or terminal packets cannot revive"
        );
        assert_eq!(p.commands.lock().unwrap().len(), before);
    }
}

fn iab_route(browsers: &mut skyre::browser::Browsers, endpoint: &str, turn: &str) {
    browsers
        .register_iab(
            "owned",
            endpoint,
            skyre::browser::iab::RouteConfig {
                conversation_id: "owned-session".into(),
                thread_id: None,
                window_id: "owned-window".into(),
            },
        )
        .unwrap();
    browsers
        .set_iab_context(
            "owned",
            Some(&json!({"session_id":"owned-session","turn_id":turn})),
        )
        .unwrap();
}
fn iab_engine(p: &Provider, turn: &str) -> Engine {
    let mut e = Engine::new(Box::new(Fixture::default()));
    iab_route(&mut e.browsers, &p.endpoint, turn);
    e.begin_chooser_cell(1);
    let tab = e
        .execute(
            "browser.new_tab",
            &json!({"browser":"owned","url":"about:blank"}),
        )
        .unwrap();
    assert_eq!(tab["targetId"], "t");
    e
}
#[test]
fn raw_wait_native_iab_context_changes_retire_waits_but_same_turn_and_detach_preserve_data() {
    let p = Provider::start();
    let mut e = iab_engine(&p, "one");
    let response = start(&mut e, json!({"timeoutMs":1000}));
    let token = packet(&response);
    e.browsers.set_iab_context("owned", None).unwrap();
    e.browsers
        .set_iab_context(
            "owned",
            Some(&json!({"session_id":"owned-session","turn_id":"one"})),
        )
        .unwrap();
    e.browsers.set_iab_route_available("owned", true).unwrap();
    assert_eq!(
        e.execute("browser.cdp_events", &token).unwrap(),
        response,
        "Cached/live metadata for the same authority preserves a pending read"
    );
    let mut cancel = token;
    cancel["__skyreRawWait"]["op"] = json!("cancel");
    e.execute("browser.cdp_events", &cancel).unwrap();
    for (index, terminal) in [false, true].into_iter().enumerate() {
        let token = packet(&start(
            &mut e,
            json!({"timeoutMs":if terminal {1}else{1000}}),
        ));
        if terminal {
            thread::sleep(Duration::from_millis(2));
            e.tick();
        }
        let before = p.commands.lock().unwrap().len();
        e.browsers
            .set_iab_context(
                "owned",
                Some(&json!({"session_id":"owned-session","turn_id":format!("next-{index}")})),
            )
            .unwrap();
        assert!(
            e.execute("browser.cdp_events", &token)
                .unwrap_err()
                .message
                .contains("unavailable")
        );
        assert_eq!(p.commands.lock().unwrap().len(), before);
        let token = packet(&start(
            &mut e,
            json!({"timeoutMs":if terminal {1}else{1000}}),
        ));
        if terminal {
            thread::sleep(Duration::from_millis(2));
            e.tick();
        }
        let before = p.commands.lock().unwrap().len();
        e.browsers.set_iab_route_available("owned", false).unwrap();
        e.browsers.set_iab_route_available("owned", true).unwrap();
        assert!(
            e.execute("browser.cdp_events", &token)
                .unwrap_err()
                .message
                .contains("unavailable")
        );
        assert_eq!(p.commands.lock().unwrap().len(), before);
    }
    let token = packet(&start(
        &mut e,
        json!({"timeoutMs":1000,"after_sequence":1e100}),
    ));
    e.execute("browser.close_tab", &json!({"browser":"owned","tab":"t"}))
        .unwrap();
    let before = p.commands.lock().unwrap().len();
    let terminal = e.execute("browser.cdp_events", &token).unwrap();
    assert_eq!(terminal["events"], json!([]));
    assert_eq!(terminal["truncated"], false);
    assert_eq!(
        p.commands.lock().unwrap().len(),
        before,
        "Ordinary same-context detach is data-only and cannot reattach"
    );
}
#[cfg(unix)]
#[test]
fn raw_wait_native_iab_journal_replacement_retires_live_registry() {
    let p = Provider::start();
    let directory = tempfile::tempdir().unwrap();
    let journal = directory.path().join("journal");
    let mut seed = skyre::browser::Browsers::default();
    iab_route(&mut seed, &p.endpoint, "retained");
    seed.enable_iab_recovery("owned", &journal).unwrap();
    drop(seed); // Metadata-only journal setup opened no renderer connection.
    let mut e = iab_engine(&p, "live");
    let token = packet(&start(&mut e, json!({"timeoutMs":1000})));
    let before = p.commands.lock().unwrap().len();
    let recovered = e.browsers.enable_iab_recovery("owned", &journal).unwrap();
    assert_eq!(recovered["recovered"], true);
    let error = e.execute("browser.cdp_events", &token).unwrap_err();
    assert!(error.message.contains("unavailable"), "{error}");
    assert_eq!(
        p.commands.lock().unwrap().len(),
        before,
        "Old registry must be gone before any continuation activity"
    );
}
