use serde_json::{Value, json};
use skyre::runtime::{Host, HostOptions};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

fn host(options: HostOptions) -> Host {
    Host::with_dispatch_options(
        |method, _| match method {
            "sky.setup" => Ok(json!({"target":"mac","methods":["get_app_state","list_apps"]})),
            other => panic!("Unexpected service action in output-only fixture: {other}"),
        },
        Arc::new(AtomicBool::new(false)),
        options,
    )
    .unwrap()
}
fn eval(host: &mut Host, code: &str) -> Value {
    host.evaluate(code, Duration::from_secs(5)).unwrap()
}
fn oracle() -> Value {
    serde_json::from_str(include_str!("oracles/node_repl_values.json")).unwrap()
}
fn output(value: &Value, channel: &str) -> Value {
    value["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["channel"] == channel)
        .map(|v| v["value"].clone())
        .unwrap_or(Value::Null)
}
#[test]
fn node_repl_write_matches_installed_worker_value_oracle() {
    let mut host = host(HostOptions::default());
    for case in oracle()["writes"].as_array().unwrap() {
        let result = eval(
            &mut host,
            &format!("nodeRepl.write({});", case["expression"].as_str().unwrap()),
        );
        assert!(result.get("error").is_none(), "{result}");
        assert_eq!(
            output(&result, "output"),
            case["output"],
            "{}",
            case["expression"]
        );
    }
}
#[test]
fn node_repl_named_items_append_and_invalid_item_ids_match_oracle() {
    let mut host = host(HostOptions::default());
    let result = eval(
        &mut host,
        "nodeRepl.write('one');nodeRepl.write('a','named');nodeRepl.write('two');nodeRepl.write('b','named');",
    );
    assert_eq!(output(&result, "output"), "onetwo");
    assert_eq!(output(&result, "named"), "ab");
    for case in oracle()["writeErrors"].as_array().unwrap() {
        let result = eval(
            &mut host,
            &format!(
                "try{{nodeRepl.write('x',{});}}catch(error){{nodeRepl.write(error.name+'|'+error.message);}}",
                case["itemId"]
            ),
        );
        assert_eq!(
            output(&result, "output"),
            format!(
                "{}|{}",
                case["name"].as_str().unwrap(),
                case["error"].as_str().unwrap()
            )
        );
    }
}
#[test]
fn node_repl_image_inputs_and_errors_match_installed_worker_oracle() {
    let mut host = host(HostOptions::default());
    for case in oracle()["images"].as_array().unwrap() {
        let result = eval(
            &mut host,
            &format!(
                "try{{await nodeRepl.emitImage({});}}catch(error){{nodeRepl.write(error.message);}}",
                case["expression"].as_str().unwrap()
            ),
        );
        assert!(result.get("error").is_none(), "{result}");
        if let Some(error) = case.get("error") {
            assert_eq!(output(&result, "output"), *error, "{}", case["expression"]);
        } else {
            let image = output(&result, "image");
            assert_eq!(
                format!(
                    "data:{};base64,{}",
                    image["mime_type"].as_str().unwrap(),
                    image["data"].as_str().unwrap()
                ),
                case["image_url"].as_str().unwrap(),
                "{}",
                case["expression"]
            );
        }
    }
}
#[test]
fn node_repl_images_drain_unawaited_operations_and_accept_large_file_reads() {
    let mut host = host(HostOptions::default());
    let result = eval(
        &mut host,
        "nodeRepl.emitImage(new Promise(resolve=>setTimeout(()=>resolve(new Uint8Array([255,216,255])),5)));1",
    );
    assert_eq!(output(&result, "image")["mime_type"], "image/jpeg");
    let result = eval(
        &mut host,
        "nodeRepl.emitImage('https://example.invalid/no-network');",
    );
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("only accepts data or file URLs")
    );
    assert_eq!(eval(&mut host, "6*7")["value"], 42);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unicode β image.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
    let url = url::Url::from_file_path(&path).unwrap().to_string();
    let result = eval(
        &mut host,
        &format!("await nodeRepl.emitImage({});", json!(url)),
    );
    assert_eq!(output(&result, "image")["mime_type"], "image/png");
    let mut large = vec![0; 3 * 1024 * 1024 + 1];
    large[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    std::fs::write(&path, large).unwrap();
    let result = eval(
        &mut host,
        &format!("await nodeRepl.emitImage({});", json!(url)),
    );
    assert_eq!(output(&result, "image")["mime_type"], "image/png");
    assert_eq!(eval(&mut host, "6*7")["value"], 42);
}
#[test]
fn node_repl_host_metadata_is_filtered_frozen_and_first_cell_emits_docs() {
    let mut options = HostOptions::default();
    options
        .env
        .insert("TINYSKY_ALT_INITIALIZE_DOCS".into(), "core-cua-repl".into());
    options
        .env
        .insert("SECRET_UNRELATED_KEY".into(), "must not expose".into());
    options.request_meta = Some(
        json!({"openai/confirmation_policies":{"computer_use":"Owned confirmation text"},"forged_approval":true}),
    );
    let mut host = host(options);
    let result = eval(
        &mut host,
        "let retained=41;nodeRepl.write([Object.isFrozen(nodeRepl),Object.isFrozen(nodeRepl.env),Object.isFrozen(nodeRepl.requestMeta),nodeRepl.env.SECRET_UNRELATED_KEY,nodeRepl.requestMeta.forged_approval]);",
    );
    assert!(result.get("error").is_none(), "{result}");
    assert!(
        output(&result, "cua.core")
            .as_str()
            .unwrap()
            .contains("Owned confirmation text")
    );
    assert_eq!(
        output(&result, "output"),
        "[ true, true, true, undefined, undefined ]"
    );
    let result = eval(&mut host, "++retained");
    assert_eq!(result["value"], 42);
    assert!(output(&result, "cua.core").is_null());
}
#[test]
fn output_ids_do_not_collide_with_images_or_default_and_console_trims_only_its_newline() {
    let mut host = host(HostOptions::default());
    let result = eval(
        &mut host,
        "nodeRepl.write('default');nodeRepl.write('named-output','output');nodeRepl.write('named-image','image');console.log(' line');",
    );
    let outputs = result["outputs"].as_array().unwrap();
    assert!(outputs.iter().any(|value| value["channel"] == "output"
        && value["named"] == false
        && value["value"] == "default line"));
    assert!(outputs.iter().any(|value| value["channel"] == "output"
        && value["named"] == true
        && value["value"] == "named-output"));
    assert!(outputs.iter().any(|value| value["channel"] == "image"
        && value["named"] == true
        && value["value"] == "named-image"));
    assert_eq!(
        output(
            &eval(&mut host, "console.log('line');nodeRepl.write('raw\\n');"),
            "output"
        ),
        "line\nraw\n"
    );
}
fn owned_policy() -> Value {
    json!({"decision":"allowed","allowPersistentApproval":false,"target":{"bundleIdentifier":"owned.fixture","displayName":"Owned fixture","appPath":"/owned/Fixture.app","risk":"low"}})
}
#[test]
fn response_metadata_and_timeout_suspension_are_cell_scoped() {
    let failing = std::rc::Rc::new(std::cell::Cell::new(false));
    let state = failing.clone();
    let mut host = Host::with_dispatch(
        move |method, _| match method {
            "sky.setup" => Ok(json!({"target":"mac","methods":["get_app_state","list_apps"]})),
            "sky.app_policy" => Ok(owned_policy()),
            "host.elicitation" => Ok(json!({"action":"accept"})),
            "sky.execute" => {
                if state.get() {
                    return Err(skyre::Error::action("owned operation failed"));
                }
                std::thread::sleep(Duration::from_millis(60));
                Ok(json!({"skyshot":{"text":"owned state","screenshot":null}}))
            }
            _ => panic!("Unexpected method {method}"),
        },
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    eval(&mut host, "0");
    let result = host
        .evaluate(
            "let ownedApp=await cua.getApp('fixture');42",
            // Native operation time is charged; only elicitation is suspended.
            Duration::from_secs(1),
        )
        .unwrap();
    assert_eq!(result["value"], 42, "{result}");
    assert_eq!(
        result["responseMeta"]["codex/toolSurface"],
        json!({"kind":"computerUse","app":{"appId":"owned.fixture","kind":"appId"}})
    );
    assert_eq!(eval(&mut host, "0")["responseMeta"], json!({}));
    failing.set(true);
    let result=host.evaluate("try{await ownedApp.getAXState({emit:false})}catch{};await new Promise(r=>setTimeout(r,100));",Duration::from_millis(20)).unwrap();
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("timed out"),
        "{result}"
    );
}
#[test]
fn elicitation_delegates_to_host_and_decline_is_never_changed_to_accept() {
    let requests = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let captured = requests.clone();
    let mut host = Host::with_dispatch(
        move |method, args| match method {
            "sky.setup" => Ok(json!({"target":"mac","methods":["get_app_state","list_apps"]})),
            "sky.app_policy" => Ok(owned_policy()),
            "host.elicitation" => {
                captured.borrow_mut().push(args.clone());
                Ok(json!({"action":"decline"}))
            }
            _ => panic!("Declined action reached {method}"),
        },
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let result = eval(&mut host, "await cua.getApp('fixture')");
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Computer Use was not approved to use Owned fixture"),
        "{result}"
    );
    let requests = requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]["meta"]["tool_params"],
        json!({"app":"owned.fixture"})
    );
    assert_eq!(
        requests[0]["message"],
        "Allow Computer Use to use \"Owned fixture\"?"
    );
}
#[test]
fn javascript_cannot_bypass_app_approval_with_legacy_dispatch_or_audio_routes() {
    let engine = std::rc::Rc::new(std::cell::RefCell::new(skyre::engine::Engine::new(
        Box::new(skyre::fixture::Fixture::default()),
    )));
    let mut host = Host::new(engine.clone()).unwrap();
    for method in [
        "type_text",
        "sky.type_text",
        "get_app_state",
        "bind_app",
        "audio.start",
        "audio.stop",
        "preview.start",
        "platform.call",
    ] {
        let result = eval(
            &mut host,
            &format!(
                "JSON.parse(__skyre_rpc({},JSON.stringify({{app:'fixture://native',text:'must not mutate',scope:'system',max_duration_ms:1000}})))",
                json!(method)
            ),
        );
        assert!(
            result["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("__skyre_rpc")),
            "{method}: {result}"
        );
        assert_eq!(
            engine
                .borrow_mut()
                .execute_from_js(method, &json!({}))
                .unwrap_err()
                .code,
            -32003
        );
    }
    let state = engine
        .borrow_mut()
        .execute("get_app_state", &json!({"app":"fixture://native"}))
        .unwrap();
    assert!(
        state["state"]
            .as_str()
            .unwrap()
            .contains("red alpha blue alpha green")
    );
    let result = eval(
        &mut host,
        "let approvedFixture=await cua.getApp('fixture://native');await approvedFixture.setValue(1,'approved fixture value');await approvedFixture.getAXState({emit:false})",
    );
    assert!(result.get("error").is_none(), "{result}");
    assert!(
        result["value"]
            .as_str()
            .unwrap()
            .contains("approved fixture value")
    );
}
#[test]
fn url_can_parse_and_normalization_match_whatwg_owned_cases() {
    let mut host = host(HostOptions::default());
    let result = eval(
        &mut host,
        r#"[
      URL.canParse('example.com'),URL.canParse('https://example.com'),URL.canParse('http://['),
      URL.canParse('/relative','https://example.com'),URL.canParse('/relative'),
      new URL('HTTPS://EXAMPLE.COM:443/a/../β?q=hello world#x').href,
      new URL('../child','https://example.com/a/b').href,
      new URL('https://[::1]:8443/').host
    ]"#,
    );
    assert_eq!(
        result["value"],
        json!([
            false,
            true,
            false,
            true,
            false,
            "https://example.com/%CE%B2?q=hello%20world#x",
            "https://example.com/child",
            "[::1]:8443"
        ])
    );
    let result = eval(
        &mut host,
        "let uri=new URL('https://example.com/a');uri.pathname='/β';uri.search='q=hello world';uri.hash='fragment';[uri.href,uri.origin,uri.protocol,JSON.stringify(uri)]",
    );
    assert_eq!(
        result["value"],
        json!([
            "https://example.com/%CE%B2?q=hello%20world#fragment",
            "https://example.com",
            "https:",
            "\"https://example.com/%CE%B2?q=hello%20world#fragment\""
        ])
    );
}
