//! Native setup-owner identity after the ordinary default bootstrap.
//! This does not expose setupCUA, select first options, or prove import/reset parity.
use serde_json::{Value, json};
use skyre::{
    runtime::{Host, HostOptions, RuntimeBackend},
    worker::{Event, Worker},
};
use std::{
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};

const FIRST: &str = r#"
var setupOwnerPromise = __skyreInitialize();
var setupFacade = cua;
var setupStateMethod = cua.getState;
var setupInitializeMethod = cua.initialize;
var setupFirstResult = await setupOwnerPromise;
nodeRepl.write(JSON.stringify({
  samePromise: setupOwnerPromise === __skyreInitialize(),
  settledUndefined: setupFirstResult === undefined,
  keys: Object.keys(cua).sort(),
  installed: ['getState','getApp','listApps','getBrowser','getTab','createBrowserTab','listBrowsers','listTabs'].every(k => typeof cua[k] === 'function'),
  providers: [typeof cua.computer, typeof cua.browsers]
}));
"#;
const LATER: &str = r#"
var setupLaterPromise = __skyreInitialize();
await setupLaterPromise;
var setupStateA = await cua.initialize();
var setupStateB = await cua.initialize();
nodeRepl.write(JSON.stringify({
  samePromise: setupLaterPromise === setupOwnerPromise,
  sameFacade: cua === setupFacade,
  sameStateMethod: cua.getState === setupStateMethod,
  sameInitializeMethod: cua.initialize === setupInitializeMethod,
  freshInventory: setupStateA !== setupStateB,
  states: [setupStateA, setupStateB]
}));
"#;

fn expected_first() -> Value {
    json!({"samePromise":true,"settledUndefined":true,"installed":true,
        "providers":["object","object"],
        "keys":["browsers","computer","createBrowserTab","getApp","getBrowser","getState","getTab","initialize","listApps","listBrowsers","listTabs"]})
}
fn expected_later() -> Value {
    json!({"samePromise":true,"sameFacade":true,"sameStateMethod":true,
        "sameInitializeMethod":true,"freshInventory":true,
        "states":[{"apps":[],"browsers":[]},{"apps":[],"browsers":[]}]})
}
fn observed(value: Value) -> Value {
    assert!(value.get("error").is_none(), "{value}");
    let rows: Vec<_> = value["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["channel"] == "output")
        .collect();
    assert_eq!(rows.len(), 1, "{value}");
    serde_json::from_str(rows[0]["value"].as_str().unwrap()).unwrap()
}
fn answer(method: &str, args: &Value) -> Value {
    match method {
        "sky.setup" => json!({"target":"mac","methods":["list_apps"]}),
        "sky.execute" => {
            assert_eq!(args["method"], "list_apps");
            json!([])
        }
        "browser.list" => json!([]),
        other => panic!("Unexpected provider call: {other}"),
    }
}
fn assert_calls(calls: &[String]) {
    assert_eq!(calls.len(), 5, "{calls:?}");
    for (method, count) in [("sky.setup", 1), ("sky.execute", 2), ("browser.list", 2)] {
        assert_eq!(
            calls.iter().filter(|name| name.as_str() == method).count(),
            count,
            "{calls:?}"
        );
    }
}

#[test]
fn default_cua_setup_owner_reuses_promise_and_methods_in_host_runtimes() {
    for runtime in RuntimeBackend::available() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured = calls.clone();
        let mut host = Host::with_dispatch_options(
            move |method, args| {
                captured.lock().unwrap().push(method.to_owned());
                Ok(answer(method, args))
            },
            Arc::new(AtomicBool::new(false)),
            HostOptions {
                runtime,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            observed(host.evaluate(FIRST, Duration::from_secs(3)).unwrap()),
            expected_first(),
            "{runtime:?}"
        );
        assert_eq!(
            observed(host.evaluate(LATER, Duration::from_secs(3)).unwrap()),
            expected_later(),
            "{runtime:?}"
        );
        assert_calls(&calls.lock().unwrap());
    }
}
fn worker_result(worker: &mut Worker, calls: &mut Vec<String>, ticket: u64) -> Value {
    // Preserve the existing cold V8 child allowance; each cell still has a 3 s budget.
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("bounded worker progress");
        match worker
            .event(remaining)
            .unwrap()
            .expect("worker event before deadline")
        {
            Event::Call {
                method,
                args,
                reply,
                ..
            } => {
                calls.push(method.clone());
                reply.send(Ok(answer(&method, &args))).unwrap();
            }
            Event::Done {
                ticket: actual,
                result,
            } => {
                assert_eq!(actual, ticket);
                return observed(result.unwrap());
            }
        }
    }
}
#[test]
fn default_cua_setup_owner_reuses_promise_and_methods_in_workers() {
    for runtime in RuntimeBackend::available() {
        let mut worker = Worker::with_options_and_executable(
            HostOptions {
                runtime,
                ..Default::default()
            },
            env!("CARGO_BIN_EXE_nanocodex-computer").into(),
        );
        let mut calls = Vec::new();
        let first = worker.start(FIRST, Duration::from_secs(3)).unwrap();
        assert_eq!(
            worker_result(&mut worker, &mut calls, first),
            expected_first(),
            "{runtime:?}"
        );
        let child = worker.runtime_child_pid();
        assert_eq!(child.is_some(), runtime == RuntimeBackend::V8);
        let later = worker.start(LATER, Duration::from_secs(3)).unwrap();
        assert_eq!(
            worker_result(&mut worker, &mut calls, later),
            expected_later(),
            "{runtime:?}"
        );
        assert_eq!(worker.runtime_child_pid(), child);
        assert!(!worker.take_kernel_reset());
        assert_calls(&calls);
    }
}
