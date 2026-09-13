use serde_json::{Value, json};
use skyre::{
    Result,
    auth::{Auth, Page, Request, validate_request},
    qr::{self, Decode, Watch},
    security::{Document, Security, SecurityConfig, automated_decision, origin},
};
#[test]
fn origin_canonicalization_and_deny_precedence() {
    assert_eq!(
        origin("https://EXAMPLE.com:443/a?b#c").unwrap(),
        "https://example.com"
    );
    for u in [
        "file:///tmp/x",
        "https://user@example.com",
        "https://user:pw@example.com",
        "data:text/plain,x",
        "javascript:1",
    ] {
        assert!(origin(u).is_err());
    }
    let s = Security::new(SecurityConfig {
        allowed_origins: vec!["https://example.com".into()],
        denied_origins: vec!["https://example.com".into()],
        ..Default::default()
    })
    .unwrap();
    assert!(s.check_url("https://example.com/path").is_err());
    assert!(s.check_url("https://other.example").is_err());
    assert!(s.check_url("about:blank").is_err());
}
#[test]
fn recovered_automated_review_matrix() {
    for reviewer in [
        None,
        Some("user"),
        Some("auto_review"),
        Some("guardian_subagent"),
    ] {
        for action in ["accept", "decline", "cancel"] {
            let r = json!({"action":action,"reviewer":reviewer});
            let out = automated_decision(&r);
            if action == "accept" && matches!(reviewer, Some("auto_review" | "guardian_subagent")) {
                assert!(out.is_ok());
            } else {
                let expected = match (reviewer, action) {
                    (Some("auto_review" | "guardian_subagent") | None, "decline") => -32012,
                    (Some("auto_review" | "guardian_subagent"), "cancel") => -32013,
                    _ => -32011,
                };
                assert_eq!(out.unwrap_err().code, expected, "{r}");
            }
        }
    }
}
fn document() -> Document {
    Document {
        browser: "fixture".into(),
        tab: "tab".into(),
        url: "https://example.com/login".into(),
        token: "document-1".into(),
        frame: "main".into(),
    }
}
#[test]
fn document_binding_and_error_precedence() {
    let original = document();
    for (field, expected) in [
        ("tab", "target_changed"),
        ("url", "origin_changed"),
        ("token", "page_changed"),
        ("frame", "page_changed"),
    ] {
        let mut current = original.clone();
        match field {
            "tab" => {
                current.tab = "other".into();
                current.url = "https://other.example".into();
            }
            "url" => current.url = "https://other.example".into(),
            "token" => current.token = "new".into(),
            _ => current.frame = "other".into(),
        };
        assert_eq!(original.validate(&current).unwrap_err().message, expected);
    }
    let mut fragment = original.clone();
    fragment.url.push_str("#new");
    assert!(original.validate(&fragment).is_err());
}
#[test]
fn auth_shape_rejects_conflicts_before_browser_access() {
    for request in [
        json!({"browser":"b","tab":"t","fields":[{"id":"a","selector":"#a"},{"id":"a","selector":"#b"}]}),
        json!({"browser":"b","tab":"t","fields":[{"id":"a","selector":"#a"}],"submit":{"selector":"#a"}}),
        json!({"browser":"b","tab":"t","fields":[],"options":[{"id":"pick","fields":["missing"]}]}),
        json!({"browser":"b","tab":"t"}),
    ] {
        let parsed: Request = serde_json::from_value(request).unwrap();
        assert!(validate_request(&parsed).is_err());
    }
    assert!(
        serde_json::from_value::<Request>(
            json!({"browser":"b","tab":"t","values":{"password":"must not be public"}})
        )
        .is_err()
    );
}
fn image_png(qr_data: Option<&str>) -> Vec<u8> {
    let mut image = image::GrayImage::from_pixel(320, 320, image::Luma([255]));
    if let Some(data) = qr_data {
        let code = qrcode::QrCode::new(data).unwrap();
        let width = code.width();
        let scale = 6;
        for y in 0..width {
            for x in 0..width {
                if code[(x, y)] == qrcode::Color::Dark {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            image.put_pixel(
                                (x * scale + dx + 24) as u32,
                                (y * scale + dy + 24) as u32,
                                image::Luma([0]),
                            );
                        }
                    }
                }
            }
        }
    }
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}
#[test]
fn decodes_real_pixels_and_distinguishes_absence() {
    let Decode::Present(symbol) = qr::decode(&image_png(Some("https://example.com/qr"))).unwrap()
    else {
        panic!("QR recognition failed")
    };
    assert_eq!(symbol.payload, "https://example.com/qr");
    assert!(symbol.bounds[2] > 0 && symbol.bounds[3] > 0);
    assert_eq!(symbol.mobile_url.as_deref(), Some("https://example.com/qr"));
    assert_eq!(qr::decode(&image_png(None)).unwrap(), Decode::Absent);
    assert!(qr::mobile_url("http://example.com").is_none());
    assert!(qr::mobile_url("https://user@example.com").is_none());
    assert!(qr::decode(b"bad").is_err());
}
#[test]
fn qr_watch_binding_cadence_and_terminal_rules() {
    let mut watch = Watch::default();
    assert!(watch.needs_binding_check());
    for i in 0..8 {
        watch.observe(Decode::Failed, false, true).unwrap();
        assert_eq!(watch.needs_binding_check(), i == 7);
    }
    watch.observe(Decode::Absent, false, true).unwrap();
    watch.observe(Decode::Ambiguous, false, true).unwrap();
    assert_eq!(watch.absent, 0);
    for _ in 0..3 {
        watch.observe(Decode::Absent, true, true).unwrap();
    }
    assert!(watch.terminal);
    assert!(watch.observe(Decode::Absent, false, true).is_err());
    let mut watch = Watch::default();
    let symbol = qr::Symbol {
        payload: "qr".into(),
        bounds: [0, 0, 1, 1],
        mobile_url: None,
    };
    assert!(
        watch
            .observe(Decode::Present(symbol.clone()), false, false)
            .unwrap()
            .is_some()
    );
    assert!(watch.observe(Decode::Present(symbol), true, false).is_err());
}
#[derive(Default)]
struct FakePage {
    actions: Vec<(String, Value)>,
    changed: bool,
    label_changed: bool,
    replaced: bool,
}
impl Page for FakePage {
    fn execute(&mut self, method: &str, args: &Value) -> Result<Value> {
        match method {
            "document_context" => {
                let mut d = document();
                if self.changed {
                    d.token = "changed".into();
                }
                Ok(serde_json::to_value(d).unwrap())
            }
            "locator_inspect" => Ok(
                json!({"nodeIdentity":if self.replaced{"replacement"}else{"node-1"},"formIdentity":"form-1","count":1,"visible":true,"enabled":true,"editable":args["selector"]!="#submit","tag":if args["selector"]=="#submit"{"button"}else{"input"},"type":"password","autocomplete":"","inputMode":"","label":if self.label_changed{"Changed"}else{"Password"},"submissionOrigin":"https://example.com"}),
            ),
            "dom_snapshot" => Ok(json!({"text":"Owned login fixture"})),
            "locator_fill" | "locator_click" | "locator_press" => {
                assert_eq!(args["expectedNodeIdentity"], "node-1");
                assert_eq!(args["expectedFormIdentity"], "form-1");
                self.actions.push((method.into(), args.clone()));
                Ok(json!({"ok":true}))
            }
            _ => panic!("unexpected {method}"),
        }
    }
}
#[cfg(unix)]
fn programs() -> (tempfile::TempDir, Security) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let broker = dir.path().join("broker");
    let reviewer = dir.path().join("reviewer");
    // These local programs exchange synthetic values only. No credential store or UI.
    std::fs::write(&broker,"#!/usr/bin/python3\nimport json,sys\nx=json.load(sys.stdin)\nprint(json.dumps({'status':'pending'} if x['type']=='begin' else {'status':'submitted','values':{'password':'fixture-secret'},'submit':True} if x['type']=='poll' else {'ok':True}))\n").unwrap();
    std::fs::write(&reviewer,"#!/usr/bin/python3\nimport json,sys\nx=json.load(sys.stdin)\nassert x['metadata']['automated'] and x['context']['visibleDom']['text']=='Owned login fixture'\nprint(json.dumps({'action':'accept','reviewer':'auto_review'}))\n").unwrap();
    for p in [&broker, &reviewer] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let policy = Security::new(SecurityConfig {
        broker: Some(broker.to_str().unwrap().into()),
        reviewer: Some(reviewer.to_str().unwrap().into()),
        review_instructions: "Review this owned login fixture".into(),
        ..Default::default()
    })
    .unwrap();
    (dir, policy)
}
fn request() -> Value {
    json!({"browser":"fixture","tab":"tab","fields":[{"id":"password","selector":"#password"}],"submit":{"selector":"#submit"}})
}
#[cfg(unix)]
#[test]
fn broker_handoff_fills_with_bound_document_without_returning_secret() {
    let (_dir, policy) = programs();
    let mut page = FakePage::default();
    let mut auth = Auth::default();
    let start = auth
        .execute("begin", &request(), &mut page, &policy)
        .unwrap();
    assert!(!start.to_string().contains("#password"));
    let result = auth
        .execute("poll", &json!({"id":start["id"]}), &mut page, &policy)
        .unwrap();
    assert_eq!(result["status"], "submitted");
    assert!(!result.to_string().contains("fixture-secret"));
    assert_eq!(page.actions.len(), 2);
    assert_eq!(page.actions[0].1["expectedDocumentToken"], "document-1");
    assert_eq!(page.actions[0].1["value"], "fixture-secret");
    let cancelled = auth
        .execute("cancel", &json!({"id":start["id"]}), &mut page, &policy)
        .unwrap();
    assert_eq!(cancelled["status"], "submitted");
}
#[cfg(unix)]
#[test]
fn changed_document_or_prompt_prevents_any_credential_action() {
    for change_label in [false, true] {
        let (_dir, policy) = programs();
        let mut page = FakePage::default();
        let mut auth = Auth::default();
        let start = auth
            .execute("begin", &request(), &mut page, &policy)
            .unwrap();
        if change_label {
            page.label_changed = true;
        } else {
            page.changed = true;
        }
        let error = auth
            .execute("poll", &json!({"id":start["id"]}), &mut page, &policy)
            .unwrap_err();
        assert_eq!(error.code, -32014);
        assert!(page.actions.is_empty());
    }
}
#[test]
fn otp_requires_one_uniform_detection_strategy() {
    let mut fields =
        vec![json!({"type":"text","autocomplete":"one-time-code","inputMode":"text"}); 4];
    assert!(skyre::auth::is_otp(&fields));
    fields[3]["autocomplete"] = json!("");
    fields[3]["inputMode"] = json!("numeric");
    assert!(!skyre::auth::is_otp(&fields));
    for f in &mut fields {
        f["inputMode"] = json!("numeric");
    }
    assert!(skyre::auth::is_otp(&fields));
    fields[0]["type"] = json!("password");
    assert!(!skyre::auth::is_otp(&fields));
}
#[test]
fn restricted_policy_rejects_raw_execution_and_history_navigation() {
    let security = Security::new(SecurityConfig {
        allowed_origins: vec!["https://example.com".into()],
        ..Default::default()
    })
    .unwrap();
    for method in ["evaluate", "cdp_call", "back", "forward"] {
        assert_eq!(
            security.check_browser_command(method).unwrap_err().code,
            -32010
        );
    }
    for method in ["navigate", "new_tab", "readonly_evaluate", "locator_fill"] {
        assert!(security.check_browser_command(method).is_ok());
    }
}

#[cfg(unix)]
#[test]
fn same_selector_replacement_is_not_the_reviewed_control() {
    let (_dir, policy) = programs();
    let mut page = FakePage::default();
    let mut auth = Auth::default();
    let start = auth
        .execute("begin", &request(), &mut page, &policy)
        .unwrap();
    page.replaced = true;
    assert_eq!(
        auth.execute("poll", &json!({"id":start["id"]}), &mut page, &policy)
            .unwrap_err()
            .code,
        -32014
    );
    assert!(page.actions.is_empty());
}
#[test]
fn configured_app_alias_is_enforced_before_native_action() {
    let mut engine = skyre::engine::Engine::new(Box::new(skyre::fixture::Fixture::default()));
    engine.security = Security::new(SecurityConfig {
        allowed_apps: vec!["org.skyre.fixture".into()],
        ..Default::default()
    })
    .unwrap();
    assert!(
        engine
            .execute("bind_app", &json!({"app":"fixture://native"}))
            .is_ok()
    );
    assert_eq!(
        engine
            .execute("bind_app", &json!({"app":"not-allowed"}))
            .unwrap_err()
            .code,
        -32010
    );
}
#[cfg(unix)]
#[test]
fn host_monitor_revision_revokes_real_engine_dispatch_lease() {
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let program = dir.path().join("monitor");
    std::fs::write(&state, r#"{"locked":false,"revision":1}"#).unwrap();
    let literal = serde_json::to_string(state.to_str().unwrap()).unwrap();
    std::fs::write(&program,format!("#!/usr/bin/python3\nimport json,sys\njson.load(sys.stdin)\nprint(open({literal}).read())\n")).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut engine = skyre::engine::Engine::new(Box::new(skyre::fixture::Fixture::default()));
    engine.guardian_monitor =
        Some(skyre::process_rpc::Program::new(&program, Duration::from_secs(2)).unwrap());
    assert_eq!(
        engine
            .execute("bind_app", &json!({"app":"fixture://native"}))
            .unwrap_err()
            .code,
        -32003
    );
    let lease = engine
        .execute("guardian.acquire", &json!({"ttl_ms":30000}))
        .unwrap();
    assert!(
        engine
            .execute(
                "bind_app",
                &json!({"app":"fixture://native","guardianLease":lease["lease"]})
            )
            .is_ok()
    );
    std::fs::write(&state, r#"{"locked":true,"revision":2}"#).unwrap();
    assert_eq!(engine.execute("set_value",&json!({"app":"fixture://native","element_index":1,"value":"must not write","guardianLease":lease["lease"]})).unwrap_err().code,-32003);
    assert!(
        engine
            .execute(
                "guardian.set_host_state",
                &json!({"locked":false,"revision":3})
            )
            .is_err()
    );
}

fn restricted_engine() -> skyre::engine::Engine {
    let mut engine = skyre::engine::Engine::new(Box::new(skyre::fixture::Fixture::default()));
    engine.security = Security::new(SecurityConfig {
        allowed_origins: vec!["https://example.com".into()],
        denied_origins: vec!["https://denied.test".into()],
        ..Default::default()
    })
    .unwrap();
    engine
}

#[test]
fn restricted_policy_normalizes_aliases_and_denies_unbound_targets_before_connection() {
    let mut engine = restricted_engine();
    for (method, args) in [
        (
            "tab_cdp_call",
            json!({"method":"Runtime.evaluate","params":{"expression":"evil()"}}),
        ),
        ("navigate_tab_back", json!({})),
        ("navigate_tab_url", json!({"url":"https://denied.test/"})),
        ("create_tab", json!({"url":"https://denied.test/"})),
        ("evaluate", json!({"expression":"evil()"})),
        (
            "tab_ax_action",
            json!({"action":{"kind":"type_text","text":"synthetic"}}),
        ),
        ("cua_click", json!({"x":1,"y":1})),
        (
            "playwright_locator_press",
            json!({"selector":"input","key":"a"}),
        ),
        ("playwright_locator_click", json!({"selector":"input"})),
        ("playwright_file_chooser_set_files", json!({"files":[]})),
        ("webmcp_invoke_tool", json!({"tool_name":"action"})),
        ("tabs_content", json!({"urls":["https://denied.test/"]})),
        ("tab_clipboard_read_text", json!({})),
        ("tab_screenshot", json!({"frame":"child"})),
        (
            "locator_fill",
            json!({"frame":null,"selector":"input","value":"synthetic"}),
        ),
    ] {
        let mut args = args;
        args["browser_id"] = json!("unregistered");
        args["tab_id"] = json!("tab");
        assert_eq!(
            engine
                .execute(&format!("browser.{method}"), &args)
                .unwrap_err()
                .code,
            -32010,
            "{method}"
        );
    }
}

// A synthetic CDP peer exposes independently controlled main/child documents.
// It never connects to a browser and records any attempted DOM/input dispatch.
fn policy_browser(
    main_url: Value,
    child_url: Value,
    replace_child: bool,
) -> (String, std::thread::JoinHandle<Vec<Value>>) {
    use std::{net::TcpListener, thread, time::Duration};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let join = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        let mut requests = vec![];
        let mut documents = 0;
        while let Ok(message) = socket.read() {
            if !message.is_text() {
                continue;
            }
            let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "Target.attachToTarget" => json!({"sessionId":"session"}),
                "Accessibility.enable" => json!({}),
                "Fetch.enable" => {
                    assert_eq!(request["sessionId"], "session");
                    assert_eq!(
                        request["params"],
                        json!({"patterns":[{"requestStage":"Response","resourceType":"Document"}]})
                    );
                    json!({})
                }
                "Fetch.disable" => {
                    assert_eq!(request["sessionId"], "session");
                    assert_eq!(request["params"], json!({}));
                    json!({})
                }
                "Page.getFrameTree" => json!({"frameTree":{
                    "frame":{"id":"main","loaderId":"main-loader","url":main_url},
                    "childFrames":[{"frame":{"id":"child","parentId":"main","loaderId":if replace_child && documents >= 2 {"replacement-loader"} else {"child-loader"},"url":child_url}}]
                }}),
                "Page.createIsolatedWorld" => {
                    json!({"executionContextId":if request["params"]["frameId"] == "child" {2} else {1}})
                }
                "Runtime.evaluate" => {
                    let expression = request["params"]["expression"].as_str().unwrap();
                    if expression == "({url:location.href,timeOrigin:performance.timeOrigin})" {
                        documents += 1;
                        json!({"result":{"value":{"url":if request["params"]["contextId"] == 2 {&child_url} else {&main_url},"timeOrigin":123}}})
                    } else {
                        json!({"result":{"value":null}})
                    }
                }
                other => panic!("Unexpected browser operation: {other}"),
            };
            socket
                .send(tungstenite::Message::text(
                    json!({"id":request["id"],"result":result}).to_string(),
                ))
                .unwrap();
            requests.push(request);
        }
        requests
    });
    (format!("ws://{address}"), join)
}
fn attempted_dom_action(requests: &[Value]) -> bool {
    requests.iter().any(|request| {
        request["params"]["expression"]
            .as_str()
            .is_some_and(|expression| expression.contains("__skyre_dom_registry"))
    })
}

#[test]
fn restricted_policy_denies_actual_child_origin_with_allowed_main_and_encoded_aliases() {
    for encoded in [false, true] {
        let (endpoint, join) = policy_browser(
            json!("https://example.com/main"),
            json!("https://denied.test/frame"),
            false,
        );
        let mut engine = restricted_engine();
        engine.browsers.register("owned", &endpoint).unwrap();
        let args = if encoded {
            json!({"browser_id":"owned","tab_id":"tab","selector":"child >> internal:control=enter-frame >> input","value":"synthetic","expectedUrl":"https://example.com/main"})
        } else {
            json!({"browser":"owned","tab":"tab","frame":"child","selector":"input","value":"synthetic"})
        };
        let error = engine
            .execute("browser.playwright_locator_fill", &args)
            .unwrap_err();
        assert_eq!(error.code, -32010);
        drop(engine);
        let requests = join.join().unwrap();
        assert!(requests.iter().any(|r| r["params"]["contextId"] == 2));
        assert!(!attempted_dom_action(&requests));
    }
}

#[test]
fn restricted_policy_binds_allowed_frame_to_provider_identity() {
    let (endpoint, join) = policy_browser(
        json!("https://example.com/main"),
        json!("https://example.com/frame"),
        false,
    );
    let mut engine = restricted_engine();
    engine.browsers.register("owned", &endpoint).unwrap();
    engine.execute("browser.playwright_locator_fill", &json!({"browser_id":"owned","tab_id":"tab","frame":"child","selector":"input","value":"synthetic"})).unwrap();
    drop(engine);
    let requests = join.join().unwrap();
    let action = requests
        .iter()
        .find(|r| {
            r["params"]["expression"]
                .as_str()
                .is_some_and(|e| e.contains("__skyre_dom_registry"))
        })
        .unwrap();
    let expression = action["params"]["expression"].as_str().unwrap();
    assert_eq!(action["params"]["contextId"], 2);
    assert!(expression.contains("\"frame\":\"child\""));
    assert!(expression.contains("\"expectedUrl\":\"https://example.com/frame\""));
    assert!(expression.contains("\"expectedTimeOrigin\":123"));
    assert!(expression.contains("child-loader"));
}

#[test]
fn restricted_policy_rejects_unknown_frame_origin_and_document_replacement() {
    for (main, child, replace, code) in [
        (
            json!("https://example.com/main"),
            Value::Null,
            false,
            -32010,
        ),
        (
            json!("https://example.com/main"),
            json!("about:blank"),
            false,
            -32010,
        ),
        (
            json!("https://denied.test/main"),
            json!("https://example.com/frame"),
            false,
            -32010,
        ),
        (
            json!("https://example.com/main"),
            json!("https://example.com/frame"),
            true,
            -10005,
        ),
    ] {
        let (endpoint, join) = policy_browser(main, child, replace);
        let mut engine = restricted_engine();
        engine.browsers.register("owned", &endpoint).unwrap();
        let error = engine.execute("browser.locator_fill", &json!({"browser":"owned","tab":"tab","frame":"child","selector":"input","value":"synthetic"})).unwrap_err();
        assert_eq!(error.code, code, "{}", error.message);
        drop(engine);
        assert!(!attempted_dom_action(&join.join().unwrap()));
    }
}

#[test]
fn restricted_policy_does_not_expand_aliases_after_checking_them() {
    let (endpoint, join) = policy_browser(
        json!("https://example.com/main"),
        json!("https://denied.test/frame"),
        false,
    );
    let mut engine = restricted_engine();
    engine.browsers.register("owned", &endpoint).unwrap();
    let error = engine
        .execute(
            "browser.playwright_playwright_cdp_call",
            &json!({"browser":"owned","tab":"tab","method":"Runtime.evaluate","params":{"expression":"must_not_execute()"}}),
        )
        .unwrap_err();
    assert_eq!(error.code, -32601);
    drop(engine);
    assert!(
        !join
            .join()
            .unwrap()
            .iter()
            .any(|request| request["params"]["expression"] == "must_not_execute()")
    );
}
