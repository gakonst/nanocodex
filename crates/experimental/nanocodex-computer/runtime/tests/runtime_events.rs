//! Finite synchronous adjacent-Node comparison plus actual native lifetime cases.
//! This does not assert the absent async/static exports or original import owner.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use skyre::{
    Result,
    runtime::{ExecutionValidity, Host, HostOptions, ProviderControl, RuntimeBackend},
    worker::{Event, Worker},
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const CELL: Duration = Duration::from_secs(5);
const PROGRESS: Duration = Duration::from_secs(15);
const CASES: &str = include_str!("runtime_events_cases.mjs");
const ORACLE: &str = include_str!("oracles/runtime_events_node24.json");

fn metadata(call: &str) -> Value {
    json!({"x-codex-turn-metadata":{"call_id":call}})
}

#[derive(Default)]
struct Calls {
    setup: usize,
    info: usize,
    probes: Vec<ExecutionValidity>,
}
impl Calls {
    fn retired(&self) {
        assert!(self.probes.iter().all(|probe| probe.validate().is_err()));
    }
    fn dispatch(&mut self, method: &str, args: &Value, control: ProviderControl) -> Result<Value> {
        self.retired();
        let validity = control
            .execution_validity()
            .expect("native execution validator");
        validity.validate().unwrap();
        self.probes.push(validity);
        match method {
            "sky.setup" => {
                assert_eq!(args, &json!({}));
                self.setup += 1;
                Ok(json!({"target":"mac"}))
            }
            "browser.info" => {
                assert_eq!(args, &json!({"browser":"owned"}));
                self.info += 1;
                Ok(json!({"id":format!("owned-reply-{}", self.info),"type":"cdp"}))
            }
            other => panic!("Unexpected provider call from synchronous events contract: {other}"),
        }
    }
}

fn host(runtime: RuntimeBackend, calls: Rc<RefCell<Calls>>) -> Host {
    Host::with_controlled_dispatch(
        move |method, args, control| calls.borrow_mut().dispatch(method, args, control),
        Arc::new(AtomicBool::new(false)),
        HostOptions {
            runtime,
            ..Default::default()
        },
    )
    .unwrap()
}
fn success(result: Value) -> Value {
    assert!(result.get("error").is_none(), "{result}");
    result["value"].clone()
}
fn worker(runtime: RuntimeBackend) -> Worker {
    Worker::with_options_and_executable(
        HostOptions {
            runtime,
            ..Default::default()
        },
        env!("CARGO_BIN_EXE_nanocodex-computer").into(),
    )
}
fn drain(worker: &mut Worker, calls: &mut Calls, ticket: u64) -> Value {
    let deadline = Instant::now() + PROGRESS;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "bounded native Worker progress");
        match worker.event(remaining).unwrap().expect("worker progress") {
            Event::Call {
                method,
                args,
                control,
                reply,
            } => {
                reply.send(calls.dispatch(&method, &args, control)).unwrap();
            }
            Event::Done {
                ticket: completed,
                result,
            } => {
                assert_eq!(completed, ticket);
                calls.retired();
                return result.unwrap();
            }
        }
    }
}
fn evaluate(worker: &mut Worker, calls: &mut Calls, code: &str) -> Value {
    let ticket = worker.start(code, CELL).unwrap();
    success(drain(worker, calls, ticket))
}

#[test]
fn synchronous_events_match_all_102_adjacent_node_cases_and_supported_inventory() {
    assert_eq!(
        format!("{:x}", Sha256::digest(CASES)),
        "f34175b72672885ec295f832f4d35469629c425deeddc0b5ec21b2cbf0bbeaa9"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(ORACLE)),
        "9de2e0dde6f8580bc4ddfbc4c21f29a1e97211f3d8519ec3f30e416c9d0accd1"
    );
    let expected: Value = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(expected["cases"].as_array().unwrap().len(), 102);
    // Preserve the entire independently authored source. Only adapt the two ESM
    // import statements and the final output sink to the native cell contract.
    let mut code = CASES.to_owned();
    for (before, after) in [
        (
            "import * as prefixed from 'node:events';",
            "const prefixed = await import('node:events');",
        ),
        (
            "import * as bare from 'events';",
            "const bare = await import('events');",
        ),
        (
            "console.log(JSON.stringify(",
            "nodeRepl.write(JSON.stringify(",
        ),
    ] {
        assert_eq!(code.matches(before).count(), 1);
        code = code.replacen(before, after, 1);
    }
    for runtime in RuntimeBackend::available() {
        let calls = Rc::new(RefCell::new(Calls::default()));
        let mut host = host(runtime, calls.clone());
        let result = host.evaluate(&code, Duration::from_secs(15)).unwrap();
        assert!(result.get("error").is_none(), "{runtime:?}: {result}");
        let outputs: Vec<_> = result["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["channel"] == "output")
            .collect();
        assert_eq!(outputs.len(), 1, "{runtime:?}: {result}");
        let actual: Value = serde_json::from_str(outputs[0]["value"].as_str().unwrap()).unwrap();
        let actual_cases = actual["cases"].as_array().unwrap();
        let expected_cases = expected["cases"].as_array().unwrap();
        assert_eq!(actual_cases.len(), expected_cases.len());
        let differences: Vec<_> = expected_cases
            .iter()
            .zip(actual_cases)
            .filter(|(expected, actual)| expected != actual)
            .map(|(expected, actual)| json!({"expected":expected,"actual":actual}))
            .collect();
        assert!(
            differences.is_empty(),
            "{runtime:?}: {}",
            serde_json::to_string_pretty(&differences).unwrap()
        );
        for key in [
            "aliases",
            "namespacePrototypeIsNull",
            "namespaceExtensible",
            "defaultMaxListeners",
        ] {
            assert_eq!(
                actual["inventory"][key], expected["inventory"][key],
                "{runtime:?}: {key}"
            );
        }
        // This is an explicit subset, not a complete namespace comparison. No
        // absent helper is represented by a stub. Retain the full original
        // inventory unchanged in ORACLE for the outstanding export domains.
        let supported = [
            "EventEmitter",
            "default",
            "defaultMaxListeners",
            "errorMonitor",
            "getEventListeners",
            "getMaxListeners",
            "listenerCount",
            "once",
            "setMaxListeners",
        ];
        let namespace = actual["inventory"]["namespace"].as_array().unwrap();
        let string_keys: Vec<_> = namespace
            .iter()
            .filter_map(|row| row["key"].as_str())
            .collect();
        assert_eq!(string_keys, supported);
        let retained: Vec<_> = expected["inventory"]["namespace"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| {
                row["key"]
                    .as_str()
                    .is_none_or(|key| supported.contains(&key))
            })
            .cloned()
            .collect();
        assert_eq!(
            namespace, &retained,
            "{runtime:?}: supported namespace descriptors"
        );
        let retained_prototype: Vec<_> = expected["inventory"]["prototype"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["key"].is_string())
            .cloned()
            .collect();
        assert_eq!(
            actual["inventory"]["prototype"],
            json!(retained_prototype),
            "{runtime:?}: synchronous prototype descriptors; kCapture remains absent"
        );
        let constructor_keys = [
            "length",
            "name",
            "prototype",
            "once",
            "getEventListeners",
            "getMaxListeners",
            "listenerCount",
            "EventEmitter",
            "defaultMaxListeners",
            "setMaxListeners",
            "errorMonitor",
        ];
        let retained_constructor: Vec<_> = expected["inventory"]["constructor"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| {
                row["key"]
                    .as_str()
                    .is_some_and(|key| constructor_keys.contains(&key))
            })
            .cloned()
            .collect();
        assert_eq!(
            actual["inventory"]["constructor"],
            json!(retained_constructor),
            "{runtime:?}: supported constructor descriptors"
        );
        let calls = calls.borrow();
        calls.retired();
        assert_eq!(calls.setup, 1);
        assert_eq!(calls.info, 0, "events itself must not dispatch a provider");
    }
}

#[test]
fn event_callbacks_use_current_host_execution_and_persist_after_caught_errors() {
    for runtime in RuntimeBackend::available() {
        let calls = Rc::new(RefCell::new(Calls::default()));
        let mut host = host(runtime, calls.clone());
        host.set_request_meta(Some(metadata("registered"))).unwrap();
        let code = r#"
            let eventsModule = await import('events');
            let emitter = new eventsModule.EventEmitter();
            let callbackState = {reply:undefined,metadata:[]}, synchronousFailure;
            function retainedCallback() {
                callbackState.metadata.push(nodeRepl.requestMeta['x-codex-turn-metadata'].call_id);
                callbackState.reply = agent.browsers.get('owned');
            }
            emitter.on('current', retainedCallback);
            emitter.listenerCount('current')
        "#;
        assert_eq!(success(host.evaluate(code, CELL).unwrap()), 1);
        for (index, call) in ["later-one", "later-two"].into_iter().enumerate() {
            host.set_request_meta(Some(metadata(call))).unwrap();
            let actual = success(host.evaluate(r#"
                synchronousFailure=undefined;
                try { emitter.on('current', null); } catch (error) { synchronousFailure=error.code; }
                emitter.emit('current');
                ({reply:(await callbackState.reply).browserId, meta:callbackState.metadata.at(-1),
                  same:eventsModule===(await import('node:events')), count:emitter.listenerCount('current'),
                  error:synchronousFailure})
            "#, CELL).unwrap());
            assert_eq!(
                actual,
                json!({"reply":format!("owned-reply-{}",index+1),"meta":call,"same":true,"count":1,"error":"ERR_INVALID_ARG_TYPE"})
            );
            calls.borrow().retired();
        }
        assert_eq!(calls.borrow().setup, 1);
        assert_eq!(calls.borrow().info, 2);
        assert_eq!(
            success(
                host.evaluate(
                    "emitter.off('current', retainedCallback); emitter.emit('current')",
                    CELL
                )
                .unwrap()
            ),
            false
        );
    }
}

#[test]
fn event_workers_isolate_module_defaults_listeners_and_reset() {
    const SETUP: &str = r#"
        let eventsModule=await import('events');
        let emitter=new eventsModule.EventEmitter(), values=[];
        function record(value) { values.push(value); }
        emitter.on('value',record);
        [emitter.listenerCount('value'),emitter.getMaxListeners()]
    "#;
    for runtime in RuntimeBackend::available() {
        let mut first = worker(runtime);
        let mut second = worker(runtime);
        let mut first_calls = Calls::default();
        let mut second_calls = Calls::default();
        assert_eq!(
            evaluate(&mut first, &mut first_calls, SETUP),
            json!([1, 10])
        );
        assert_eq!(
            evaluate(&mut second, &mut second_calls, SETUP),
            json!([1, 10])
        );
        let first_pid = first.runtime_child_pid();
        first
            .set_request_meta(Some(metadata("registration-cell")))
            .unwrap();
        assert_eq!(
            evaluate(
                &mut first,
                &mut first_calls,
                r#"
            let callbackState = {};
            emitter.on('native', () => {
                callbackState.meta=nodeRepl.requestMeta['x-codex-turn-metadata'].call_id;
                callbackState.reply=agent.browsers.get('owned');
            });
            emitter.listenerCount('native')
        "#
            ),
            1
        );
        first
            .set_request_meta(Some(metadata("invoking-cell")))
            .unwrap();
        assert_eq!(
            evaluate(
                &mut first,
                &mut first_calls,
                "emitter.emit('native'); [(await callbackState.reply).browserId,callbackState.meta]"
            ),
            json!(["owned-reply-1", "invoking-cell"])
        );
        first_calls.retired();
        assert_eq!(
            evaluate(
                &mut first,
                &mut first_calls,
                "eventsModule.EventEmitter.defaultMaxListeners=4; emitter.emit('value',7); [values,emitter.getMaxListeners(),eventsModule===(await import('node:events'))]"
            ),
            json!([[7], 4, true])
        );
        assert_eq!(
            evaluate(
                &mut second,
                &mut second_calls,
                "emitter.emit('value',9); [values,emitter.getMaxListeners()]"
            ),
            json!([[9], 10])
        );
        assert_eq!(
            first.runtime_child_pid(),
            first_pid,
            "ordinary event work must retain the same supervised owner"
        );
        first.reset().unwrap();
        assert_eq!(
            evaluate(
                &mut first,
                &mut first_calls,
                "[typeof emitter,typeof values,typeof eventsModule]"
            ),
            json!(["undefined", "undefined", "undefined"])
        );
        assert_eq!(
            evaluate(&mut first, &mut first_calls, SETUP),
            json!([1, 10])
        );
        assert_eq!(
            evaluate(
                &mut second,
                &mut second_calls,
                "[values,emitter.listenerCount('value'),emitter.getMaxListeners()]"
            ),
            json!([[9], 1, 10])
        );
        assert_eq!(first_calls.setup, 2);
        assert_eq!(second_calls.setup, 1);
        assert_eq!(first_calls.info, 1);
        assert_eq!(second_calls.info, 0);
    }
}

#[test]
fn event_listener_work_remains_cancellable_and_recovers_a_fresh_owner() {
    for runtime in RuntimeBackend::available() {
        let mut worker = worker(runtime);
        let mut calls = Calls::default();
        assert_eq!(
            evaluate(
                &mut worker,
                &mut calls,
                "let {EventEmitter}=await import('events'); let emitter=new EventEmitter(); let count=0; emitter.on('work',()=>{count++}); emitter.emit('work'); count"
            ),
            1
        );
        let ticket = worker
            .start("while(true) emitter.emit('work')", Duration::from_secs(30))
            .unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let cancelled_at = Instant::now();
        worker.cancel();
        let result = drain(&mut worker, &mut calls, ticket);
        assert!(result.get("error").is_some(), "{runtime:?}: {result}");
        assert!(
            cancelled_at.elapsed() < Duration::from_secs(2),
            "event dispatch ignored ordinary cancellation"
        );
        assert_eq!(
            evaluate(
                &mut worker,
                &mut calls,
                "[typeof emitter,typeof count,(await import('node:events')).EventEmitter.defaultMaxListeners]"
            ),
            json!(["undefined", "undefined", 10])
        );
        assert_eq!(calls.info, 0);
    }
}

#[test]
fn synchronous_error_monitor_matches_all_27_pinned_node_cases() {
    let cases = include_str!("runtime_events_capture_cases.mjs");
    let oracle = include_str!("runtime_events_capture_fixtures/node24.json");
    assert_eq!(
        format!("{:x}", Sha256::digest(oracle)),
        "382d41d4de29a740ed5991b9bdbec1e0bfbfe3928ca1480f728fae3c6657d301"
    );
    let expected: Value = serde_json::from_str(oracle).unwrap();
    assert_eq!(expected["cases"].as_array().unwrap().len(), 27);
    let code = cases
        .replace(
            "import * as prefixed from 'node:events';",
            "const prefixed = await import('node:events');",
        )
        .replace(
            "import * as bare from 'events';",
            "const bare = await import('events');",
        )
        .replace(
            "console.log(JSON.stringify(",
            "nodeRepl.write(JSON.stringify(",
        );
    for runtime in RuntimeBackend::available() {
        let mut host = host(runtime, Rc::new(RefCell::new(Calls::default())));
        let result = host.evaluate(&code, CELL).unwrap();
        assert!(result.get("error").is_none(), "{runtime:?}: {result}");
        let outputs: Vec<_> = result["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["channel"] == "output")
            .collect();
        assert_eq!(outputs.len(), 1, "{runtime:?}: {result}");
        let actual: Value = serde_json::from_str(outputs[0]["value"].as_str().unwrap()).unwrap();
        assert_eq!(
            actual, expected,
            "{runtime:?}: synchronous errorMonitor corpus"
        );
    }
}
