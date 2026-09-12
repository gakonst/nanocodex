use super::*;

fn context(tab: &str, top_level: bool) -> raw_events::Context {
    raw_events::Context {
        source: Some(raw_events::Source {
            tab: tab.into(),
            source: json!({"sessionId":if top_level {"root"} else {"child"}}),
            tracked_target: Some(if top_level { tab } else { "child-target" }.into()),
            top_level,
        }),
        discard: None,
    }
}
fn tools(id: &str) -> Vec<Tool> {
    State::fetched_page_tools(
        json!([{"name":"echo","registrationId":id,"description":"owned descriptor"}]),
    )
    .unwrap()
}
fn args(id: &str) -> Value {
    json!({"tab":"1","name":"echo","registrationId":id})
}
fn store(state: &mut State, owner: SessionKey, id: &str) {
    let fetch = state.begin_fetch("1").unwrap();
    state.finish_fetch("1", owner, &fetch, tools(id)).unwrap();
}

#[test]
fn webmcp_canonical_projection_matches_actual_original_schema() {
    let corpus: Value =
        serde_json::from_str(include_str!("../tests/oracles/browser_webmcp_schema.json")).unwrap();
    for row in corpus["schema"].as_array().unwrap() {
        let normalized = normalize("webmcp_invoke_tool", &row["payload"]);
        let expected = row["observed"]["accepted"]
            .as_bool()
            .unwrap_or_else(|| row["observed"]["success"].as_bool().unwrap());
        assert_eq!(normalized.is_ok(), expected, "{row}");
        if let Ok(Some((method, actual))) = normalized {
            assert_eq!(method, "webmcp_invoke");
            let parsed = if row["observed"].get("parsed").is_some() {
                &row["observed"]["parsed"]
            } else {
                &row["observed"]["data"]
            };
            let mut expected = json!({"browser":parsed["browser_id"],"tab":parsed["tab_id"],"name":parsed["tool_name"],"registrationId":parsed["registration_id"]});
            for (from, to) in [
                ("input", "arguments"),
                ("timeout_ms", "timeoutMs"),
                ("tool_title", "tool_title"),
                ("tool_description", "tool_description"),
            ] {
                if let Some(value) = parsed.get(from) {
                    expected[to] = value.clone();
                }
            }
            assert_eq!(actual, expected, "{row}");
        }
    }
}

#[test]
fn webmcp_original_owner_lookup_and_lifecycle_partitions_match() {
    let corpus: Value =
        serde_json::from_str(include_str!("../tests/oracles/browser_webmcp_owner.json")).unwrap();
    for case in corpus["cases"].as_array().unwrap() {
        let mut state = State::default();
        let mut owner = SessionKey::Host("a".into());
        for row in case["observations"].as_array().unwrap() {
            let operation = &row["operation"];
            match operation[0].as_str().unwrap() {
                "fetch" => {
                    // This adapter compares admitted fetched lookups. The
                    // helper-only disabled Su call has no successful Wp caller.
                    if let Ok(fetch) = state.begin_fetch("1") {
                        let values: Vec<_> = operation[1]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|tool| {
                                let mut tool = tool.clone();
                                tool["registrationId"] = tool
                                    .as_object_mut()
                                    .unwrap()
                                    .remove("registration_id")
                                    .unwrap();
                                tool
                            })
                            .collect();
                        let tools = State::fetched_page_tools(json!(values)).unwrap();
                        state
                            .finish_fetch("1", owner.clone(), &fetch, tools)
                            .unwrap();
                    }
                }
                "lookup" => {
                    let expected = &row["observed"]["result"];
                    let mut request = json!({"name":operation[1],"registrationId":expected["registration_id"].as_str().unwrap_or("absent")});
                    let actual = state.admit("1", owner.clone(), &request);
                    assert_eq!(
                        actual.is_ok(),
                        !expected.is_null(),
                        "{} {operation}",
                        case["name"]
                    );
                    if let Ok(admission) = actual {
                        admission.apply_description(&mut request);
                        assert_eq!(request["tool_description"], expected["description"]);
                        assert_eq!(
                            admission.tool.registration(),
                            expected["registration_id"].as_str().unwrap()
                        );
                    }
                }
                "session" => owner = SessionKey::Host(operation[1].as_str().unwrap().into()),
                "disable" => state.tabs.entry("1".into()).or_default().disabled = true,
                "enabled" => assert_eq!(
                    state.begin_fetch("1").is_ok(),
                    row["observed"]["result"]["allowed"] == true
                ),
                "mutate-input" => {} // State already stores an independent clone.
                "event" => {
                    let kind = operation[1].as_str().unwrap();
                    let mut source = context(
                        if kind == "other-tab" { "2" } else { "1" },
                        kind != "child-session",
                    );
                    if kind == "other-tab" {
                        source.source.as_mut().unwrap().source["sessionId"] = json!("other");
                    }
                    let mut event = json!({"method":if kind == "same-document" {"Page.navigatedWithinDocument"} else {"Page.frameNavigated"},"params":{"frame":{}}});
                    if kind == "child-frame" {
                        event["params"]["frame"]["parentId"] = json!("parent");
                    }
                    state.event(&source, &event);
                }
                other => panic!("unknown original owner operation {other}"),
            }
        }
    }
}

#[test]
fn webmcp_document_owner_refresh_and_bounded_storage_never_restore_old_admission() {
    let mut state = State::default();
    let owner = SessionKey::LocalConnection;
    assert!(state.admit("1", owner.clone(), &args("r1")).is_err());
    assert!(state.tabs.is_empty());
    store(&mut state, owner.clone(), "r1");
    let old = state.admit("1", owner.clone(), &args("r1")).unwrap();
    assert!(
        state
            .admit("1", SessionKey::Host("other".into()), &args("r1"))
            .is_err()
    );
    let fetch = state.begin_fetch("1").unwrap();
    state.event(
        &context("1", true),
        &json!({"method":"Page.frameNavigated","params":{"frame":{}}}),
    );
    assert!(state.validate(&old).is_err());
    assert!(!state.fetch_is_current("1", &fetch).unwrap());
    assert_eq!(
        state
            .finish_fetch("1", owner.clone(), &fetch, tools("old-result"))
            .unwrap(),
        json!([])
    );
    store(&mut state, owner.clone(), "r1");
    assert!(
        state.validate(&old).is_err(),
        "reusing a renderer string cannot restore document identity"
    );
    let current = state.admit("1", owner.clone(), &args("r1")).unwrap();
    state.clear_fetched();
    assert!(state.validate(&current).is_err());
    let fetch = state.begin_fetch("1").unwrap();
    assert!(
        state
            .finish_fetch(
                "1",
                owner.clone(),
                &fetch,
                vec![tools("r2")[0].clone(); MAX_TOOLS + 1]
            )
            .is_err()
    );
    assert!(state.begin_fetch("1").is_err());
    state.event(
        &context("1", true),
        &json!({"method":"Page.frameNavigated","params":{"frame":{"parentId":null}}}),
    );
    store(&mut state, owner, "r2");
    state.disconnect();
    assert!(state.tabs.is_empty());
    assert!(state.validate(&old).is_err());
}

#[test]
fn webmcp_returned_descriptors_and_failed_fetches_do_not_retain_authority() {
    let mut state = State::default();
    for index in 0..2048 {
        let tab = index.to_string();
        let fetch = state.begin_fetch(&tab).unwrap();
        state.abandon_fetch(&tab, &fetch);
        assert!(state.tabs.is_empty());
    }
    let fetch = state.begin_fetch("1").unwrap();
    let mut returned = state
        .finish_fetch("1", SessionKey::LocalConnection, &fetch, tools("r1"))
        .unwrap();
    returned[0]["registrationId"] = json!("changed by caller");
    returned[0]["description"] = json!("changed by caller");
    let admission = state
        .admit("1", SessionKey::LocalConnection, &args("r1"))
        .unwrap();
    assert_eq!(admission.tool.descriptor["description"], "owned descriptor");
    state.abandon_fetch("1", &fetch);
    state.validate(&admission).unwrap();
}

#[test]
fn webmcp_catalog_receipt_identity_removal_and_retired_routes_fail_closed() {
    let mut state = State::default();
    let source = context("1", true);
    let added =
        json!({"method":"WebMCP.toolsAdded","params":{"tools":[{"frameId":"f","name":"echo"}]}});
    let fetch = state.begin_fetch("1").unwrap();
    state.event(&source, &added);
    let tools = state.catalog("1").unwrap();
    let id = tools[0].registration().to_owned();
    state
        .finish_fetch("1", SessionKey::LocalConnection, &fetch, tools)
        .unwrap();
    let admitted = state
        .admit("1", SessionKey::LocalConnection, &args(&id))
        .unwrap();
    let mut dialogs = super::super::dialog::State::default();
    dialogs.root("1", "root");
    admitted.check_source(&dialogs).unwrap();
    state.event(
        &context("1", false),
        &json!({"method":"WebMCP.toolsRemoved","params":{"tools":[{"frameId":"f","name":"echo"}]}}),
    );
    state.validate(&admitted).unwrap();
    state.event(&source, &added);
    assert!(state.validate(&admitted).is_err());
    let replaced = state.catalog("1").unwrap()[0].registration().to_owned();
    assert_ne!(replaced, id);
    dialogs.remove("1");
    assert!(admitted.check_source(&dialogs).is_err());
    state.next_registration = u64::MAX;
    state.event(&source, &added);
    assert!(state.catalog("1").is_err());
    assert!(state.tabs["1"].catalog.is_empty());
}

#[test]
fn webmcp_native_owner_uses_trusted_route_not_model_or_kernel_fields() {
    let mut browsers = Browsers::default();
    browsers
        .register("owned", "ws://127.0.0.1:1?skyre-provider=extension")
        .unwrap();
    assert_eq!(
        browsers.providers["owned"].webmcp_session().unwrap(),
        SessionKey::LocalConnection
    );
    let route = crate::host_turns::Route {
        conversation_id: "conversation".into(),
        thread_id: Some("subagent".into()),
    };
    browsers
        .bind_host_route("owned", route.clone(), Some("a".repeat(64)))
        .unwrap();
    assert!(browsers.providers["owned"].webmcp_session().is_err());
    browsers
        .providers
        .get_mut("owned")
        .unwrap()
        .host_binding
        .as_mut()
        .unwrap()
        .active = true;
    browsers.select_host_route(Some(route.key()));
    assert_eq!(
        browsers.providers["owned"].webmcp_session().unwrap(),
        SessionKey::Host("subagent".into())
    );
    let browser = browsers.providers.get_mut("owned").unwrap();
    store(
        &mut browser.surface.webmcp.lock().unwrap(),
        SessionKey::Host("subagent".into()),
        "r1",
    );
    let mut request = args("r1");
    request["browser"] = json!("owned");
    request["session_id"] = json!("foreign");
    request["kernel_scope"] = json!("foreign");
    browsers
        .prepare_webmcp("webmcp_invoke", &mut request)
        .unwrap();
    browsers.select_host_route(None);
    assert!(
        browsers
            .prepare_webmcp("webmcp_invoke", &mut request)
            .is_err()
    );
}

#[test]
fn webmcp_predispatch_native_context_recheck_rejects_changed_owner_and_availability() {
    let mut browsers = Browsers::default();
    browsers
        .register("owned", "ws://127.0.0.1:1?skyre-provider=extension")
        .unwrap();
    let browser = browsers.providers.get_mut("owned").unwrap();
    store(
        &mut browser.surface.webmcp.lock().unwrap(),
        SessionKey::LocalConnection,
        "r1",
    );
    let local = browser.webmcp_admit(&args("r1")).unwrap();
    browser.validate_webmcp_context(&local).unwrap();
    let route = crate::host_turns::Route {
        conversation_id: "conversation".into(),
        thread_id: None,
    };
    browsers
        .bind_host_route("owned", route, Some("a".repeat(64)))
        .unwrap();
    let browser = browsers.providers.get_mut("owned").unwrap();
    assert!(browser.validate_webmcp_context(&local).is_err());
    browser.host_binding.as_mut().unwrap().active = true;
    assert!(browser.validate_webmcp_context(&local).is_err());
    store(
        &mut browser.surface.webmcp.lock().unwrap(),
        SessionKey::Host("conversation".into()),
        "r1",
    );
    let bound = browser.webmcp_admit(&args("r1")).unwrap();
    browser.validate_webmcp_context(&bound).unwrap();
    browser.host_binding.as_mut().unwrap().active = false;
    assert!(browser.validate_webmcp_context(&bound).is_err());
    assert!(browser.client.is_none(), "native rechecks must not connect");

    browsers
        .register_iab(
            "iab",
            "ws://127.0.0.1:1",
            super::super::iab::RouteConfig {
                conversation_id: "iab-session".into(),
                thread_id: None,
                window_id: "owned-window".into(),
            },
        )
        .unwrap();
    browsers
        .set_iab_context(
            "iab",
            Some(&json!({"session_id":"iab-session","turn_id":"turn1"})),
        )
        .unwrap();
    let browser = browsers.providers.get_mut("iab").unwrap();
    browser.iab.as_mut().unwrap().tabs.insert(
        "1".into(),
        super::super::iab::Tab {
            id: "1".into(),
            logical_id: "owned-page".into(),
            active: true,
            mark: None,
            mark_turn: None,
            completed_turn: None,
        },
    );
    store(
        &mut browser.surface.webmcp.lock().unwrap(),
        SessionKey::Host("iab-session".into()),
        "r1",
    );
    let admitted = browser.webmcp_admit(&args("r1")).unwrap();
    browser.validate_webmcp_context(&admitted).unwrap();
    browser.iab.as_mut().unwrap().set_route_available(false);
    assert!(browser.validate_webmcp_context(&admitted).is_err());
    browser.iab.as_mut().unwrap().set_route_available(true);
    browser.validate_webmcp_context(&admitted).unwrap();
    browser.iab.as_mut().unwrap().remove("1");
    assert!(browser.validate_webmcp_context(&admitted).is_err());
    assert!(browser.webmcp_admit(&args("r1")).is_err());
    assert!(browser.client.is_none(), "native rechecks must not connect");
}
