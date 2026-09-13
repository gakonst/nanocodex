//! Original browser-client Promise/error cases through both production workers.
use serde_json::{Value, json};
use skyre::{
    Error,
    runtime::{HostOptions, RuntimeBackend},
    worker::{Event, Worker},
};
use std::time::{Duration, Instant};
fn evaluate(worker: &mut Worker, code: &str) -> Value {
    worker.start(code, Duration::from_secs(3)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut failure = false;
    loop {
        assert!(Instant::now() < deadline, "bounded owned navigation worker");
        match worker.event(Duration::from_millis(25)).unwrap() {
            Some(Event::Call {
                method,
                args,
                reply,
                ..
            }) => {
                let result = match method.as_str() {
                    "sky.setup" => Ok(json!({"target":"mac","methods":[]})),
                    "browser.list" => Ok(json!([{"id":"owned"}])),
                    "browser.info" => Ok(
                        json!({"id":"owned","type":"cdp","capabilities":{"browser":[],"tab":[]}}),
                    ),
                    "browser.get_tab" => Ok(json!({"id":"t"})),
                    "browser.documentation" => Ok(json!("Owned fixture browser")),
                    "browser.navigation_arm" => {
                        failure = args["url"] == "https://owned.invalid/fail";
                        Ok(json!({"id":"owned-watch"}))
                    }
                    "browser.navigation_poll" => {
                        std::thread::sleep(Duration::from_millis(180));
                        if failure {
                            Err(Error::action("owned wait failure"))
                        } else {
                            Ok(json!({"pending":false,"value":null}))
                        }
                    }
                    "browser.navigation_cancel" => Ok(Value::Null),
                    _ => panic!("unexpected owned fixture call {method}: {args}"),
                };
                let _ = reply.send(result);
            }
            Some(Event::Done { result, .. }) => return result.unwrap(),
            None => {}
        }
    }
}
fn output(value: &Value) -> String {
    value["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["channel"] == "output")
        .filter_map(|v| v["value"].as_str())
        .collect()
}
#[test]
fn navigation_outer_error_precedence_and_unawaited_drain_match_original_kernel() {
    let oracle: Value =
        serde_json::from_str(include_str!("oracles/browser_navigation_outer.json")).unwrap();
    for backend in RuntimeBackend::available() {
        for row in oracle["cases"].as_array().unwrap() {
            let mut worker = Worker::with_options_and_executable(
                HostOptions {
                    runtime: backend,
                    ..Default::default()
                },
                env!("CARGO_BIN_EXE_nanocodex-computer").into(),
            );
            evaluate(&mut worker, "0");
            let setup = evaluate(
                &mut worker,
                "var b=await cua.browsers.get('owned');var tab=await b.tabs.get('t');",
            );
            assert!(setup.get("error").is_none(), "{setup}");
            let result = evaluate(&mut worker, row["first"].as_str().unwrap());
            let expected = &row["first_result"];
            let text = expected["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v["text"].as_str())
                .collect::<String>();
            if expected["isError"] == true {
                assert_eq!(
                    result["exceptionMessage"], text,
                    "{backend:?}: {row}; actual: {result}"
                );
                assert!(worker.kernel_reset_pending());
            } else {
                assert!(
                    result.get("error").is_none(),
                    "{backend:?}: {}: {result}",
                    row["name"]
                );
                assert_eq!(output(&result), text, "{backend:?}: {}", row["name"]);
                assert!(!worker.kernel_reset_pending());
                let followup = evaluate(&mut worker, row["second"].as_str().unwrap());
                assert_eq!(
                    output(&followup),
                    row["second_result"]["content"][0]["text"]
                );
            }
        }
    }
}
