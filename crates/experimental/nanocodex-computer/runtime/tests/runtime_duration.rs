//! Phase expectations come from the installed 42-trial duration oracle.
use serde_json::{Value, json};
use skyre::{
    Result,
    runtime::{Host, HostOptions, RuntimeBackend},
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
fn host(backend: RuntimeBackend, calls: Rc<RefCell<Vec<String>>>) -> Host {
    Host::with_dispatch_options(
        move |method, input| -> Result<Value> {
            calls.borrow_mut().push(method.into());
            if method == "sky.setup" {
                return Ok(json!({"target":"mac","methods":[]}));
            }
            if method == "owned.rpc" {
                std::thread::sleep(Duration::from_millis(input["delay"].as_u64().unwrap_or(0)));
                if input["fail"] == true {
                    return Err(skyre::Error::action("owned deferred failure"));
                }
                return Ok(json!(42));
            }
            panic!("unexpected provider {method}")
        },
        Arc::new(AtomicBool::new(false)),
        HostOptions {
            runtime: backend,
            ..Default::default()
        },
    )
    .unwrap()
}
#[test]
fn submitted_duration_excludes_compile_and_private_drain_but_includes_awaited_work() {
    for backend in RuntimeBackend::available() {
        let mut host = host(backend, Rc::new(RefCell::new(vec![])));
        host.evaluate("0", Duration::from_secs(3)).unwrap();
        for (code, timed, error) in [
            (
                "nodeRepl.rpc('owned',{delay:180});nodeRepl.write(42)",
                false,
                false,
            ),
            (
                "nodeRepl.write(await nodeRepl.rpc('owned',{delay:180}))",
                true,
                false,
            ),
            (
                "(async()=>await nodeRepl.rpc('owned',{delay:180}))();nodeRepl.write(42)",
                false,
                false,
            ),
            (
                "nodeRepl.rpc('owned',{delay:180});throw new Error('owned cell failure')",
                false,
                true,
            ),
            (
                "nodeRepl.rpc('owned',{delay:180,fail:true});throw new Error('owned cell failure')",
                false,
                true,
            ),
            (
                "await new Promise(resolve=>setTimeout(resolve,180));nodeRepl.write(42)",
                true,
                false,
            ),
            (
                "nodeRepl.emitImage(new Promise(resolve=>setTimeout(()=>resolve(Uint8Array.from([137,80,78,71,13,10,26,10])),180)))",
                false,
                false,
            ),
            (
                "nodeRepl.emitImage(new Promise(resolve=>setTimeout(()=>resolve(Uint8Array.from([137,80,78,71,13,10,26,10])),180)));throw new Error('owned cell failure')",
                false,
                true,
            ),
        ] {
            let start = Instant::now();
            let result = host.evaluate(code, Duration::from_secs(3)).unwrap();
            let wall = start.elapsed().as_millis() as u64;
            let code_ms = result["executionDurationMs"].as_u64().unwrap();
            assert!(wall >= 160, "{backend:?} {code}: {result}");
            assert_eq!(
                result.get("error").is_some(),
                error,
                "{backend:?} {code}: {result}"
            );
            if timed {
                assert!(code_ms >= 160, "{backend:?} {code}: {result}");
            } else {
                assert!(
                    code_ms + 120 < wall,
                    "{backend:?} phase includes drain: {code}: {result}"
                );
            }
            if error {
                assert_eq!(result["exceptionMessage"], "owned cell failure");
            }
        }
        for code in ["let = ;", "import anything from 'node:path';"] {
            let result = host.evaluate(code, Duration::from_secs(3)).unwrap();
            assert!(result.get("error").is_some());
            assert!(
                result.get("executionDurationMs").is_none(),
                "{backend:?}: {result}"
            );
        }
        for code in [
            "Promise.resolve().then(()=>{const end=Date.now()+50;while(Date.now()<end){}})",
            "Promise.resolve().then(()=>Promise.resolve().then(()=>{const end=Date.now()+50;while(Date.now()<end){}}))",
        ] {
            let result = host.evaluate(code, Duration::from_secs(3)).unwrap();
            assert!(
                result["executionDurationMs"].as_u64().unwrap() >= 40,
                "{backend:?}: {result}"
            );
        }
    }
}
#[test]
fn queued_host_requests_yield_to_submitted_microtasks_and_remain_fifo() {
    for backend in RuntimeBackend::available() {
        let calls = Rc::new(RefCell::new(vec![]));
        let observed = calls.clone();
        let mut host = Host::with_dispatch_options(
            move |method, input| {
                observed
                    .borrow_mut()
                    .push(format!("{method}:{}", input["id"]));
                if method == "sky.setup" {
                    Ok(json!({"target":"mac","methods":[]}))
                } else {
                    Ok(input["id"].clone())
                }
            },
            Arc::new(AtomicBool::new(false)),
            HostOptions {
                runtime: backend,
                ..Default::default()
            },
        )
        .unwrap();
        host.evaluate("0", Duration::from_secs(3)).unwrap();
        calls.borrow_mut().clear();
        let result=host.evaluate("let callOrder=[];nodeRepl.rpc('owned',{id:1}).then(v=>callOrder.push(v));nodeRepl.rpc('owned',{id:2}).then(v=>callOrder.push(v));Promise.resolve().then(()=>callOrder.push('microtask'));",Duration::from_secs(3)).unwrap();
        assert!(result.get("error").is_none(), "{backend:?}: {result}");
        assert_eq!(*calls.borrow(), vec!["owned.rpc:1", "owned.rpc:2"]);
        assert_eq!(
            host.evaluate("callOrder", Duration::from_secs(3)).unwrap()["value"],
            json!(["microtask", 1, 2])
        );
    }
}

#[test]
fn queued_calls_accept_large_inputs_and_timeout_discards_pending_actions() {
    for backend in RuntimeBackend::available() {
        let calls = Rc::new(RefCell::new(vec![]));
        let mut host = host(backend, calls.clone());
        host.evaluate("0", Duration::from_secs(3)).unwrap();
        calls.borrow_mut().clear();
        let result = host
            .evaluate(
                "await nodeRepl.rpc('owned',{text:'x'.repeat(4*1024*1024)})",
                Duration::from_secs(3),
            )
            .unwrap();
        assert_eq!(result["value"], 42, "{backend:?}: {result}");
        assert_eq!(calls.borrow().as_slice(), ["owned.rpc"]);
        calls.borrow_mut().clear();
        let result = host
            .evaluate(
                "nodeRepl.rpc('owned',{delay:0});while(true){}",
                Duration::from_millis(60),
            )
            .unwrap();
        assert_eq!(
            result["exceptionMessage"],
            "js execution timed out; kernel reset, rerun your request"
        );
        assert!(
            calls.borrow().is_empty(),
            "{backend:?}: an undispatched action escaped the timeout"
        );
    }
}

#[test]
fn cancellation_discards_the_rest_of_an_already_started_provider_queue() {
    use skyre::worker::{Event, Worker};
    for backend in RuntimeBackend::available() {
        let mut worker = Worker::with_options_and_executable(
            HostOptions {
                runtime: backend,
                ..Default::default()
            },
            env!("CARGO_BIN_EXE_nanocodex-computer").into(),
        );
        worker
            .start(
                "nodeRepl.rpc('owned',{id:1});nodeRepl.rpc('owned',{id:2});",
                Duration::from_secs(3),
            )
            .unwrap();
        let limit = Instant::now() + Duration::from_secs(5);
        let mut cancelled = false;
        loop {
            assert!(
                Instant::now() < limit,
                "{backend:?}: cancellation did not complete"
            );
            match worker.event(Duration::from_millis(20)).unwrap() {
                Some(Event::Call { method, reply, .. }) if method == "sky.setup" => {
                    reply
                        .send(Ok(json!({"target":"mac","methods":[]})))
                        .unwrap();
                }
                Some(Event::Call {
                    method,
                    args,
                    reply,
                    ..
                }) => {
                    assert_eq!(method, "owned.rpc");
                    assert_eq!(
                        args["id"], 1,
                        "cancelled queued action reached the provider"
                    );
                    assert!(!cancelled);
                    cancelled = true;
                    worker.cancel();
                    drop(reply);
                }
                Some(Event::Done { .. }) => break,
                None => {}
            }
        }
        assert!(cancelled);
        assert!(worker.take_kernel_reset());
    }
}
