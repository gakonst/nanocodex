use super::*;
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

fn oracle(group: &str) -> Vec<Value> {
    let fixture: Value =
        serde_json::from_str(include_str!("../tests/oracles/browser_activation.json")).unwrap();
    fixture["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["group"] == group)
        .cloned()
        .collect()
}
fn preference_value(name: &str) -> Result<Value> {
    Ok(match name {
        "absent" => return Ok(json!({})),
        "null" => Value::Null,
        "true" => json!(true),
        "false" => json!(false),
        "zero" => json!(0),
        "one" => json!(1),
        "empty-string" => json!(""),
        "true-string" => json!("true"),
        "array" => json!([]),
        "object" => json!({}),
        "read-error" => return Err(Error::action("fixture read failure")),
        _ => panic!("Unknown preference fixture {name}"),
    })
    .map(|value| json!({"webmcp_enabled":value}))
}
fn model_metadata(name: &str) -> Value {
    let value = match name {
        "absent" => return json!({}),
        "null" => Value::Null,
        "array" => json!([]),
        "number" => json!(7),
        "invalid-json" => json!("{"),
        "json-null" => json!("null"),
        "json-array" => json!("[]"),
        "json-number" => json!("7"),
        "empty-object" => json!({}),
        "model-number" => json!({"model":5}),
        "compatible" => json!({"model":"GPT-6-ASTRA"}),
        "luna" | "incompatible" => json!({"model":"GPT-6-LUNA"}),
        "embedded-luna" => json!({"model":"prefix-LUNA-suffix"}),
        "bare-luna" => json!({"model":"LUNA"}),
        "leading-spaces" => json!({"model":"  GPT-6-LUNA "}),
        "unicode" => json!({"model":"İΣ-LUNA"}),
        "json-string" => json!(r#"{"model":"GPT-6-LUNA"}"#),
        _ => panic!("Unknown model fixture {name}"),
    };
    json!({"x-codex-turn-metadata":value})
}

#[test]
fn original_preference_and_model_scalar_partitions() {
    let rows = oracle("preference");
    assert_eq!(rows.len(), 11);
    for row in rows {
        let name = row["input"]["value"].as_str().unwrap().to_owned();
        let mut owner = Owner::new(Some(Box::new(move || preference_value(&name))), || 0.0);
        assert_eq!(json!(owner.enabled()), row["observed"]["result"], "{row}");
    }
    let rows = oracle("model");
    assert_eq!(rows.len(), 17);
    for row in rows {
        let metadata = model_metadata(row["input"]["metadata"].as_str().unwrap());
        assert_eq!(
            json!(normalized_model(&metadata)),
            row["observed"]["model"],
            "{row}"
        );
        let model = Model::from_task_metadata(&metadata.to_string());
        assert_eq!(
            matches!(model, Model::Incompatible { .. }),
            row["observed"]["model"]
                .as_str()
                .is_some_and(|s| s.contains("-luna"))
        );
    }
}

#[test]
fn original_common_command_gate_108_partitions_and_preference_order() {
    let rows = oracle("command-gate");
    assert_eq!(rows.len(), 108);
    for row in rows {
        let input = &row["input"];
        let model = Model::from_task_metadata(
            &model_metadata(input["model"].as_str().unwrap()).to_string(),
        );
        let preference = input["preference"].as_str().unwrap().to_owned();
        let reads = Arc::new(Mutex::new(0usize));
        let captured = reads.clone();
        let mut owner = Owner::new(
            Some(Box::new(move || {
                *captured.lock().unwrap() += 1;
                preference_value(&preference)
            })),
            || 0.0,
        );
        let mut info = json!({"name":"Owned fixture provider"});
        match input["advertisement"].as_str().unwrap() {
            "absent" => {}
            "no-tab" => info["capabilities"] = json!({}),
            "empty" => info["capabilities"] = json!({"tab":[]}),
            "cdp" => info["capabilities"] = json!({"tab":[{"id":"cdp"}]}),
            "webmcp" => info["capabilities"] = json!({"tab":[{"id":"webmcp"}]}),
            "duplicate" => info["capabilities"] = json!({"tab":[{"id":"webmcp"},{"id":"webmcp"}]}),
            value => panic!("Unknown advertisement {value}"),
        }
        let result = match owner.admit(&model, &info, input["command"].as_str().unwrap()) {
            Ok(()) => json!({"ok":true}),
            Err(error) => json!({"error":error.message}),
        };
        assert_eq!(result, row["observed"]["result"], "{row}");
        let original_reads = row["observed"]["trace"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| **event == "preference:get:webmcp_enabled")
            .count();
        assert_eq!(*reads.lock().unwrap(), original_reads, "{row}");
    }
}

#[test]
fn original_preference_cache_clock_order_and_failed_refresh_partitions() {
    let rows = oracle("preference-cache");
    assert_eq!(rows.len(), 12);
    let mut current = String::new();
    let reads = Arc::new(Mutex::new(0usize));
    let clock = Arc::new(Mutex::new(VecDeque::<f64>::new()));
    let mut owner = Owner::default();
    for row in rows {
        let input = &row["input"];
        let scenario = input["scenario"].as_str().unwrap();
        if scenario != current {
            current = scenario.to_owned();
            *reads.lock().unwrap() = 0;
            let returns = match scenario {
                "first-then-before-expiry" | "rollback-retains-cache" => {
                    vec![Ok(json!({"webmcp_enabled":false}))]
                }
                "exact-expiry-reloads" | "forced-refresh-bypasses-clock-read" => vec![
                    Ok(json!({"webmcp_enabled":false})),
                    Ok(json!({"webmcp_enabled":true})),
                ],
                "expired-read-error-keeps-old-entry" => vec![
                    Ok(json!({"webmcp_enabled":false})),
                    Err(Error::action("fixture read failure")),
                ],
                "non-object-settings" => vec![Ok(json!([]))],
                "read-error-before-first-clock" => vec![Err(Error::action("fixture read failure"))],
                value => panic!("Unknown scenario {value}"),
            };
            let mut returns: VecDeque<_> = returns.into();
            let captured = reads.clone();
            let times = clock.clone();
            owner = Owner::new(
                Some(Box::new(move || {
                    *captured.lock().unwrap() += 1;
                    returns.pop_front().expect("bounded scripted reader")
                })),
                move || {
                    times
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("bounded scripted clock")
                },
            );
        }
        clock.lock().unwrap().extend(
            input["times"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap()),
        );
        let force = scenario == "forced-refresh-bypasses-clock-read" && input["step"] == 2;
        let mut observed = match owner.settings(force) {
            Ok(value) => json!({"value":value}),
            Err(error) => json!({"error":error.message}),
        };
        observed["reads"] = json!(*reads.lock().unwrap());
        observed["expires"] = json!(owner.cached.as_ref().map(|(_, expires)| *expires as i64));
        assert_eq!(observed, row["observed"], "{row}");
        assert!(clock.lock().unwrap().is_empty());
    }
}

#[test]
fn native_unavailable_reader_fails_closed_and_rollback_can_reuse_failed_refresh_cache() {
    let mut missing = Owner::new(None, || panic!("unavailable reader must not read clock"));
    assert!(!missing.enabled());
    assert!(missing.cached.is_none());
    let times = Arc::new(Mutex::new(VecDeque::from([100.0, 300100.0, 50.0])));
    let mut reads = 0;
    let mut owner = Owner::new(
        Some(Box::new(move || {
            reads += 1;
            if reads == 1 {
                Ok(json!({"webmcp_enabled":true}))
            } else {
                Err(Error::action("owned failure"))
            }
        })),
        move || times.lock().unwrap().pop_front().unwrap(),
    );
    assert!(owner.enabled());
    assert!(!owner.enabled());
    assert!(owner.enabled());
}

#[test]
fn native_projection_is_bounded_strict_and_retired_with_its_control() {
    use crate::runtime::ProviderControl;
    for wire in [
        json!({}),
        json!({"kind":"unknown"}),
        json!({"kind":"compatible","model":"gpt-luna"}),
        json!({"kind":"incompatible"}),
        json!({"kind":"incompatible","model":null}),
        json!({"kind":"incompatible","model":"GPT-LUNA"}),
        json!({"kind":"incompatible","model":"gpt-astra"}),
        json!({"kind":"incompatible","model":"x".repeat(MAX_MODEL_BYTES)+"-luna"}),
        json!({"kind":"unavailable"}),
        json!({"kind":"unavailable","reason":"unknown"}),
        json!({"kind":"unavailable","reason":"model_too_large","model":"gpt-luna"}),
    ] {
        assert!(
            serde_json::from_value::<Model>(wire.clone()).is_err(),
            "{wire}"
        );
    }
    assert_eq!(
        Model::from_task_metadata("{"),
        Model::Unavailable {
            reason: Unavailable::InvalidMetadata
        }
    );
    assert_eq!(
        Model::from_task_metadata(
            &json!({"x-codex-turn-metadata":{"model":"x".repeat(MAX_MODEL_BYTES)+"-LUNA"}})
                .to_string()
        ),
        Model::Unavailable {
            reason: Unavailable::ModelTooLarge
        }
    );
    let model = Model::from_task_metadata(&model_metadata("unicode").to_string());
    assert_eq!(
        serde_json::from_value::<Model>(serde_json::to_value(&model).unwrap()).unwrap(),
        model
    );
    let control = ProviderControl::new_with_activation_model(|_| Ok(()), None, model.clone());
    assert_eq!(control.activation_model().unwrap(), &model);
    assert!(control.execution_validity().is_none());
    control.lifetime().finish().unwrap();
    assert!(control.activation_model().is_err());
}

#[test]
fn native_engine_registration_and_policy_precede_activation_without_provider_access() {
    use crate::{engine::Engine, fixture::Fixture, runtime::ProviderControl};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut engine = Engine::new(Box::new(Fixture::default()));
    engine
        .browsers
        .register("owned", &format!("ws://{}", listener.local_addr().unwrap()))
        .unwrap();
    let model = Model::from_task_metadata(&model_metadata("luna").to_string());
    let control = ProviderControl::new_with_activation_model(|_| Ok(()), None, model);
    let fetch = json!({"browser_id":"owned","tab_id":"t","browser":"foreign","model":"gpt-astra"});
    let error = engine
        .execute_from_js_controlled("browser.webmcp_list_tools", &fetch, &control)
        .unwrap_err();
    assert_eq!(
        error.message,
        "gpt-6-luna does not support command \"webmcp_list_tools\"."
    );
    let error = engine
        .execute("browser.webmcp_list_tools", &fetch)
        .unwrap_err();
    assert_eq!(
        error.message,
        "owned does not support command \"webmcp_list_tools\"."
    );
    for args in [
        json!({"browser":"owned","tab":"t","name":"echo"}),
        json!({"browser":"owned","tab":"t","name":"echo","registrationId":"never-fetched"}),
    ] {
        let error = engine
            .execute_from_js_controlled("browser.webmcp_invoke", &args, &control)
            .unwrap_err();
        assert!(error.message.contains("registration"), "{error:?}");
        assert!(!error.message.contains("does not support"));
    }
    engine.security = crate::security::Security::new(crate::security::SecurityConfig {
        allowed_origins: vec!["https://owned.test".into()],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        engine
            .execute_from_js_controlled("browser.webmcp_invoke_tool", &json!({}), &control)
            .unwrap_err()
            .code,
        -32010
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn native_unavailable_projection_preserves_ordinary_calls_and_registration_precedence() {
    use crate::{engine::Engine, fixture::Fixture, runtime::ProviderControl};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut engine = Engine::new(Box::new(Fixture::default()));
    engine
        .browsers
        .register("owned", &format!("ws://{}", listener.local_addr().unwrap()))
        .unwrap();
    for (model, message) in [
        (
            Model::from_task_metadata("{"),
            "Invalid native task metadata for WebMCP",
        ),
        (
            Model::from_task_metadata(
                &json!({"x-codex-turn-metadata":{"model":"x".repeat(MAX_MODEL_BYTES)+"-LUNA"}})
                    .to_string(),
            ),
            "Native WebMCP model projection exceeds limit",
        ),
    ] {
        assert!(matches!(model, Model::Unavailable { .. }));
        let wire = serde_json::to_value(&model).unwrap();
        assert!(wire.to_string().len() < 100);
        assert_eq!(serde_json::from_value::<Model>(wire).unwrap(), model);
        let control = ProviderControl::new_with_activation_model(|_| Ok(()), None, model);
        assert_eq!(
            engine
                .execute_from_js_controlled("browser.info", &json!({"browser":"owned"}), &control)
                .unwrap()["id"],
            "owned"
        );
        assert_eq!(
            engine
                .execute_from_js_controlled(
                    "browser.webmcp_list",
                    &json!({"browser":"owned","tab":"t"}),
                    &control
                )
                .unwrap_err()
                .message,
            message
        );
        for args in [
            json!({"browser":"owned","tab":"t","name":"echo"}),
            json!({"browser":"owned","tab":"t","name":"echo","registrationId":"stale"}),
        ] {
            let error = engine
                .execute_from_js_controlled("browser.webmcp_invoke", &args, &control)
                .unwrap_err();
            assert!(error.message.contains("registration"), "{error:?}");
        }
        control.lifetime().finish().unwrap();
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn native_runtime_model_projection_uses_captured_metadata_and_call_lifetime() {
    use crate::runtime::{Host, HostOptions, ProviderControl, RuntimeBackend};
    use std::{cell::RefCell, rc::Rc, sync::atomic::AtomicBool, time::Duration};
    for runtime in RuntimeBackend::available() {
        let observations = Rc::new(RefCell::new(Vec::<(String, Model, ProviderControl)>::new()));
        let captured = observations.clone();
        let mut host = Host::with_controlled_dispatch(
            move |method, args, control| {
                control.execution_validity().unwrap().validate()?;
                captured.borrow_mut().push((
                    method.to_owned(),
                    control.activation_model()?.clone(),
                    control.clone(),
                ));
                match method {
                    "sky.setup" => Ok(json!({"target":"mac"})),
                    "browser.info" => Ok(json!({"id":args["browser"],"type":"cdp"})),
                    _ => panic!("Unexpected native fixture method {method}"),
                }
            },
            Arc::new(AtomicBool::new(false)),
            HostOptions {
                runtime,
                request_meta: Some(model_metadata("luna")),
                ..Default::default()
            },
        )
        .unwrap();
        let result = host.evaluate("try{nodeRepl.requestMeta['x-codex-turn-metadata'].model='GPT-6-ASTRA'}catch{}; await agent.browsers.get('owned'); 42", Duration::from_secs(5)).unwrap();
        assert_eq!(result["value"], 42, "{runtime:?}: {result}");
        let first = observations.borrow().len();
        assert!(first >= 2);
        assert!(observations.borrow().iter().all(|(_,model,control)| matches!(model, Model::Incompatible { model } if model=="gpt-6-luna") && control.activation_model().is_err()));
        host.set_request_meta(Some(model_metadata("compatible")))
            .unwrap();
        let result = host
            .evaluate(
                "await agent.browsers.get('owned-next'); 43",
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(result["value"], 43, "{runtime:?}: {result}");
        assert!(observations.borrow().len() > first);
        assert!(
            observations.borrow()[first..]
                .iter()
                .all(|(_, model, control)| *model == Model::Compatible
                    && control.activation_model().is_err())
        );
        host.set_request_meta(Some(model_metadata("json-string")))
            .unwrap();
        let result = host
            .evaluate(
                "await agent.browsers.get('owned-last'); 44",
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(result["value"], 44, "{runtime:?}: {result}");
        assert!(matches!(
            observations.borrow().last().unwrap().1,
            Model::Incompatible { .. }
        ));
        host.set_request_meta(Some(
            json!({"x-codex-turn-metadata":{"model":"x".repeat(MAX_MODEL_BYTES)+"-LUNA"}}),
        ))
        .unwrap();
        let result = host
            .evaluate(
                "await agent.browsers.get('owned-oversized'); 45",
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(result["value"], 45, "{runtime:?}: {result}");
        assert_eq!(
            observations.borrow().last().unwrap().1,
            Model::Unavailable {
                reason: Unavailable::ModelTooLarge
            }
        );
    }
}
