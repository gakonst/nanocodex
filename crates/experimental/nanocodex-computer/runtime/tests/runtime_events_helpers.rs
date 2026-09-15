//! Finite synchronous helper contracts. The EventTarget branch stays unsupported.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use skyre::{
    runtime::{Host, HostOptions, RuntimeBackend},
    worker::{Event, Worker},
};
use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const CASES: &str = include_str!("runtime_events_helpers_cases.mjs");
const ORACLE: &str = include_str!("oracles/runtime_events_helpers_node24.json");
fn answer(method: &str, args: &Value) -> Value {
    assert_eq!(method, "sky.setup", "helper must not access a provider");
    assert_eq!(args, &json!({}));
    json!({"target":"mac"})
}
fn host(runtime: RuntimeBackend) -> (Host, Rc<Cell<usize>>) {
    let count = Rc::new(Cell::new(0));
    let observed = count.clone();
    let host = Host::with_dispatch_options(
        move |method, args| {
            observed.set(observed.get() + 1);
            Ok(answer(method, args))
        },
        Arc::new(AtomicBool::new(false)),
        HostOptions {
            runtime,
            ..Default::default()
        },
    )
    .unwrap();
    (host, count)
}
fn evaluate(host: &mut Host, code: &str) -> Value {
    let result = host.evaluate(code, Duration::from_secs(5)).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    result["value"].clone()
}

#[test]
fn synchronous_events_helpers_match_64_node_cases_and_sampled_descriptors() {
    assert_eq!(
        format!("{:x}", Sha256::digest(CASES)),
        "48758fd9de84fed5f4d65cf44f26e13054c27297af7f6920b4d8976f3c09fe4a"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(ORACLE)),
        "788ffa731a970ee5b9390b55906878a4185fa458b7b2ea61fe3d30296e7787a8"
    );
    let expected: Value = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(expected["cases"].as_array().unwrap().len(), 64);
    assert!(
        expected["cases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|case| case.get("unexpected").is_none())
    );
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
        let (mut host, count) = host(runtime);
        let result = host.evaluate(&code, Duration::from_secs(10)).unwrap();
        assert!(result.get("error").is_none(), "{runtime:?}: {result}");
        let output: Vec<_> = result["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["channel"] == "output")
            .collect();
        assert_eq!(output.len(), 1, "{result}");
        let actual: Value = serde_json::from_str(output[0]["value"].as_str().unwrap()).unwrap();
        assert_eq!(
            actual["cases"], expected["cases"],
            "{runtime:?}: complete finite synchronous matrix"
        );
        assert_eq!(
            actual["inventory"], expected["inventory"],
            "{runtime:?}: helper/static identity and descriptors"
        );
        // Node's genuine EventTarget/AbortSignal observations remain in the
        // oracle. They are not silently treated as native successes.
        assert_eq!(expected["eventTargets"].as_array().unwrap().len(), 2);
        assert_eq!(actual["eventTargets"], json!([]));
        assert_eq!(count.get(), 1, "ordinary mandatory bootstrap only");
    }
}

#[test]
fn events_helpers_observe_owned_abort_store_without_disrupting_abort() {
    for runtime in RuntimeBackend::available() {
        let (mut host, count) = host(runtime);
        assert_eq!(
            evaluate(
                &mut host,
                r#"
var h=await import('node:events');
var controller=new AbortController(),called=0;
controller.signal.addEventListener('abort',()=>{called++});
var refused=[];
for (const invoke of [()=>h.listenerCount(controller.signal,'abort'),
 ()=>h.getEventListeners(controller.signal,'abort'),()=>h.getMaxListeners(controller.signal),
 ()=>h.setMaxListeners(3,controller.signal)]) {
 try {invoke();refused.push({refused:false})}catch(error){refused.push({refused:true,name:error.name,code:error.code})}
}
var before=called;controller.abort('owned reason');
({refused,before,after:called,aborted:controller.signal.aborted,reason:controller.signal.reason,
 defaultMaximum:h.EventEmitter.defaultMaxListeners,eventTargetGlobal:typeof EventTarget})
"#
            ),
            json!({"refused":[
            {"refused":false},
            {"refused":false},
            {"refused":false},
            {"refused":false}],
            "before":0,"after":1,"aborted":true,"reason":"owned reason","defaultMaximum":10,"eventTargetGlobal":"undefined"})
        );
        assert_eq!(count.get(), 1);
    }
}
const FIRST: &str = r#"
var h=await import('node:events'),e=new h.EventEmitter(),other=new h.EventEmitter();
var callbackState={calls:0};function listener(){callbackState.calls++}
e.on('owned',listener).once('owned',listener);
h.setMaxListeners(7);h.setMaxListeners(3,e);
({count:h.listenerCount(e,'owned'),same:h.getEventListeners(e,'owned').every(x=>x===listener),
 max:h.getMaxListeners(e),other:h.getMaxListeners(other),namedDefault:h.defaultMaxListeners,calls:callbackState.calls})
"#;
const LATER: &str = r#"
var again=await import('events');
var same=again===h&&again.listenerCount===h.EventEmitter.listenerCount;
var prior=again.listenerCount(e,'owned');e.emit('owned');
({same,prior,count:again.listenerCount(e,'owned'),calls:callbackState.calls,max:again.getMaxListeners(e),other:again.getMaxListeners(other)})
"#;
fn first() -> Value {
    json!({"count":2,"same":true,"max":3,"other":7,"namedDefault":10,"calls":0})
}
fn later() -> Value {
    json!({"same":true,"prior":2,"count":1,"calls":2,"max":3,"other":7})
}
#[test]
fn events_helpers_keep_state_per_host_and_across_ordinary_cells() {
    for runtime in RuntimeBackend::available() {
        let (mut a, a_count) = host(runtime);
        let (mut b, b_count) = host(runtime);
        assert_eq!(evaluate(&mut a, FIRST), first());
        assert_eq!(
            evaluate(
                &mut b,
                "var h=await import('events');[h.EventEmitter.defaultMaxListeners,h.getMaxListeners(new h.EventEmitter())]"
            ),
            json!([10, 10])
        );
        assert_eq!(evaluate(&mut a, LATER), later());
        assert_eq!(a_count.get(), 1);
        assert_eq!(b_count.get(), 1);
    }
}
fn worker_cell(worker: &mut Worker, code: &str, calls: &mut usize) -> Value {
    let ticket = worker.start(code, Duration::from_secs(5)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("bounded Worker progress");
        match worker.event(remaining).unwrap().expect("Worker event") {
            Event::Call {
                method,
                args,
                reply,
                ..
            } => {
                *calls += 1;
                reply.send(Ok(answer(&method, &args))).unwrap();
            }
            Event::Done {
                ticket: actual,
                result,
            } => {
                assert_eq!(ticket, actual);
                let result = result.unwrap();
                assert!(result.get("error").is_none(), "{result}");
                return result["value"].clone();
            }
        }
    }
}
#[test]
fn events_helpers_keep_state_in_the_production_worker_without_reset() {
    for runtime in RuntimeBackend::available() {
        let mut worker = Worker::with_options_and_executable(
            HostOptions {
                runtime,
                ..Default::default()
            },
            env!("CARGO_BIN_EXE_nanocodex-computer").into(),
        );
        let mut count = 0;
        assert_eq!(worker_cell(&mut worker, FIRST, &mut count), first());
        let pid = worker.runtime_child_pid();
        assert_eq!(pid.is_some(), runtime == RuntimeBackend::V8);
        assert_eq!(worker_cell(&mut worker, LATER, &mut count), later());
        assert_eq!(worker.runtime_child_pid(), pid);
        assert!(!worker.take_kernel_reset());
        assert_eq!(count, 1);
    }
}
