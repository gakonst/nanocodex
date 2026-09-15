//! The exact named module on ordinary installed Host/Worker cells.
//! First standalone options are covered only by cfg(test) pre-bootstrap Host tests.
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
var {setupCUA} = await import('@oai/cua/tinyskyAlt');
var namespace=await import('@oai/cua/tinyskyAlt');
var facade=cua,initialize=cua.initialize,stateMethod=cua.getState;
var ignored=0,options={get browser(){ignored++;throw Error('late browser')},get computer(){ignored++;throw Error('late computer')}};
var promise=setupCUA(options),result=await promise;
({exports:Object.keys(namespace),sameFunction:namespace.setupCUA===setupCUA,
  sameOwner:setupCUA===__skyreInitialize,samePromise:promise===__skyreInitialize(),
  resultUndefined:result===undefined,ignored,browser:typeof cua.browsers,computer:typeof cua.computer})
"#;
const LATER: &str = r#"
var {setupCUA:laterSetup} = await import('@oai/cua/tinyskyAlt');
var laterNamespace=await import('@oai/cua/tinyskyAlt');
var a=await cua.initialize(),b=await cua.initialize();
({sameNamespace:laterNamespace===namespace,sameFunction:laterSetup===setupCUA,
  samePromise:laterSetup({browser:false,computer:false})===promise,
  sameFacade:cua===facade,sameInitialize:cua.initialize===initialize,sameStateMethod:cua.getState===stateMethod,
  freshInventory:a!==b,states:[a,b],ignored,
  browser:typeof cua.browsers,computer:typeof cua.computer})
"#;
fn options(runtime: RuntimeBackend, surface: &str) -> HostOptions {
    let mut value = HostOptions {
        runtime,
        ..Default::default()
    };
    value
        .env
        .insert("CUA_REPL_ENABLED_SURFACES".into(), surface.into());
    value
}
fn answer(method: &str, args: &Value) -> Value {
    match method {
        "sky.setup" => json!({"target":"mac","methods":["list_apps"]}),
        "sky.execute" => {
            assert_eq!(args["method"], "list_apps");
            json!([])
        }
        "browser.list" => json!([]),
        other => panic!("Unexpected installed setup provider call: {other}"),
    }
}
fn observed(value: Value) -> Value {
    assert!(value.get("error").is_none(), "{value}");
    value["value"].clone()
}
fn expected_first(browser: bool, computer: bool) -> Value {
    json!({"exports":["setupCUA"],"sameFunction":true,"sameOwner":true,"samePromise":true,
        "resultUndefined":true,"ignored":0,"browser":if browser {"object"}else{"undefined"},
        "computer":if computer {"object"}else{"undefined"}})
}
fn expected_later(browser: bool, computer: bool) -> Value {
    json!({"sameNamespace":true,"sameFunction":true,"samePromise":true,"sameFacade":true,
        "sameInitialize":true,"sameStateMethod":true,"freshInventory":true,
        "states":[{"apps":[],"browsers":[]},{"apps":[],"browsers":[]}],"ignored":0,
        "browser":if browser {"object"}else{"undefined"},"computer":if computer {"object"}else{"undefined"}})
}
fn counts(calls: &[String], browser: bool, computer: bool) {
    assert_eq!(
        calls.len(),
        usize::from(computer) * 3 + usize::from(browser) * 2,
        "{calls:?}"
    );
    for (name, count) in [
        ("sky.setup", usize::from(computer)),
        ("sky.execute", usize::from(computer) * 2),
        ("browser.list", usize::from(browser) * 2),
    ] {
        assert_eq!(
            calls.iter().filter(|v| v.as_str() == name).count(),
            count,
            "{calls:?}"
        );
    }
}
#[test]
fn installed_setup_module_shares_the_trusted_host_selection_across_cells() {
    for runtime in RuntimeBackend::available() {
        for (surface, browser, computer) in [
            ("browser", true, false),
            ("computer", false, true),
            ("browser,computer", true, true),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let recorded = calls.clone();
            let mut host = Host::with_dispatch_options(
                move |method, args| {
                    recorded.lock().unwrap().push(method.to_owned());
                    Ok(answer(method, args))
                },
                Arc::new(AtomicBool::new(false)),
                options(runtime, surface),
            )
            .unwrap();
            assert_eq!(
                observed(host.evaluate(FIRST, Duration::from_secs(3)).unwrap()),
                expected_first(browser, computer)
            );
            assert_eq!(
                observed(host.evaluate(LATER, Duration::from_secs(3)).unwrap()),
                expected_later(browser, computer)
            );
            counts(&calls.lock().unwrap(), browser, computer);
        }
    }
}
fn run(worker: &mut Worker, code: &str, calls: &mut Vec<String>) -> Value {
    let ticket = worker.start(code, Duration::from_secs(3)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("bounded worker setup");
        match worker
            .event(remaining)
            .unwrap()
            .expect("worker event before setup deadline")
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
                assert_eq!(ticket, actual);
                return observed(result.unwrap());
            }
        }
    }
}
#[test]
fn installed_setup_module_shares_the_trusted_worker_selection_without_reset() {
    for runtime in RuntimeBackend::available() {
        for (surface, browser, computer) in [
            ("browser", true, false),
            ("computer", false, true),
            ("browser,computer", true, true),
        ] {
            let mut worker = Worker::with_options_and_executable(
                options(runtime, surface),
                env!("CARGO_BIN_EXE_nanocodex-computer").into(),
            );
            let mut calls = Vec::new();
            assert_eq!(
                run(&mut worker, FIRST, &mut calls),
                expected_first(browser, computer)
            );
            let pid = worker.runtime_child_pid();
            assert_eq!(pid.is_some(), runtime == RuntimeBackend::V8);
            assert_eq!(
                run(&mut worker, LATER, &mut calls),
                expected_later(browser, computer)
            );
            assert_eq!(worker.runtime_child_pid(), pid);
            assert!(!worker.take_kernel_reset());
            counts(&calls, browser, computer);
        }
    }
}
#[test]
fn setup_module_exports_only_the_exact_captured_package_subpath() {
    for runtime in RuntimeBackend::available() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let mut host = Host::with_dispatch_options(
            move |method, args| {
                recorded.lock().unwrap().push(method.to_owned());
                Ok(answer(method, args))
            },
            Arc::new(AtomicBool::new(false)),
            options(runtime, "browser"),
        )
        .unwrap();
        assert_eq!(observed(host.evaluate(r#"
var accepted=await import('@oai/cua/tinyskyAlt');
var denied=[];
for (const name of ['@oai/cua','@oai/cua/tinyskyAlt.js','@oai/cua/tinyskyalt','node:@oai/cua/tinyskyAlt']) {
 try {await import(name);denied.push(false)}catch {denied.push(true)}
}
({exports:Object.keys(accepted),denied})
"#,Duration::from_secs(3)).unwrap()),json!({"exports":["setupCUA"],"denied":[true,true,true,true]}));
        assert!(calls.lock().unwrap().is_empty());
    }
}

#[test]
fn first_module_import_cannot_capture_a_replaced_native_initializer_projection() {
    for runtime in RuntimeBackend::available() {
        let mut host = Host::with_dispatch_options(
            |method, _| panic!("Unexpected provider call: {method}"),
            Arc::new(AtomicBool::new(false)),
            options(runtime, "browser"),
        )
        .unwrap();
        assert_eq!(observed(host.evaluate(r#"
var ownerBeforeImport=__skyreInitialize;
var replaced=Reflect.set(globalThis,'__skyreInitialize',()=>42);
var deleted=Reflect.deleteProperty(globalThis,'__skyreInitialize');
var moduleAfterAttempt=await import('@oai/cua/tinyskyAlt');
var descriptor=Object.getOwnPropertyDescriptor(globalThis,'__skyreInitialize');
({replaced,deleted,sameOwner:moduleAfterAttempt.setupCUA===ownerBeforeImport,
  samePromise:moduleAfterAttempt.setupCUA()===ownerBeforeImport(),
  property:{writable:descriptor.writable,enumerable:descriptor.enumerable,configurable:descriptor.configurable},
  nativeFunctionName:moduleAfterAttempt.setupCUA.name,nativeFunctionLength:moduleAfterAttempt.setupCUA.length})
"#,Duration::from_secs(3)).unwrap()),json!({"replaced":false,"deleted":false,
            "sameOwner":true,"samePromise":true,"property":{"writable":false,"enumerable":false,"configurable":false},
            "nativeFunctionName":"initialize","nativeFunctionLength":0}));
    }
}
