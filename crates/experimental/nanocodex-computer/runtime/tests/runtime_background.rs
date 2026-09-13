//! Original-kernel timer/late-output fixtures use fresh owned workers only.
use serde_json::{Value, json};
use skyre::{
    runtime::{HostOptions, RuntimeBackend},
    worker::{Event, Worker},
};
use std::time::{Duration, Instant};
fn worker(backend: RuntimeBackend) -> Worker {
    Worker::with_options_and_executable(
        HostOptions {
            runtime: backend,
            ..Default::default()
        },
        env!("CARGO_BIN_EXE_nanocodex-computer").into(),
    )
}
fn evaluate(worker: &mut Worker, code: &str) -> Value {
    evaluate_timed(worker, code, Duration::from_secs(3))
}
fn evaluate_timed(worker: &mut Worker, code: &str, timeout: Duration) -> Value {
    worker.start(code, timeout).unwrap();
    let bound = Instant::now() + Duration::from_secs(6);
    loop {
        assert!(
            Instant::now() < bound,
            "bounded background regression completion"
        );
        match worker.event(Duration::from_millis(25)).unwrap() {
            Some(Event::Call { method, reply, .. }) => {
                assert_eq!(method, "sky.setup");
                reply
                    .send(Ok(json!({"target":"mac","methods":[]})))
                    .unwrap();
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
        .filter(|item| item["channel"] == "output")
        .map(|item| item["value"].as_str().unwrap_or(""))
        .collect()
}
#[test]
fn background_timers_and_saved_promises_survive_normal_and_ordinary_error_cells() {
    for backend in RuntimeBackend::available() {
        let mut worker = worker(backend);
        evaluate(
            &mut worker,
            "let delayedScalar=0;let pendingLater=new Promise(resolve=>setTimeout(()=>{delayedScalar=7;resolve(42)},30));",
        );
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            evaluate(&mut worker, "[delayedScalar,await pendingLater]")["value"],
            json!([7, 42]),
            "{backend:?}"
        );
        let result = evaluate(
            &mut worker,
            "let survivedCellError=0;setTimeout(()=>{survivedCellError=9},30);throw new Error('owned cell failure')",
        );
        assert!(result.get("error").is_some());
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            evaluate(&mut worker, "survivedCellError")["value"],
            9,
            "{backend:?}"
        );
    }
}
#[test]
fn late_outputs_and_rpc_keep_originating_execution_and_console_is_discarded() {
    for backend in RuntimeBackend::available() {
        let mut worker = worker(backend);
        evaluate(
            &mut worker,
            r#"let late={};setTimeout(async()=>{try{nodeRepl.write('late');late.write='wrote'}catch(error){late.write=error.message}try{console.log('late-log');late.console='logged'}catch(error){late.console=error.message}try{await nodeRepl.rpc('owned',{});late.rpc='called'}catch(error){late.rpc=error.message}},30);"#,
        );
        std::thread::sleep(Duration::from_millis(150));
        let result = evaluate(&mut worker, "late");
        assert_eq!(
            result["value"],
            json!({"write":"node_repl exec context not found","console":"logged","rpc":"node_repl exec context not found"}),
            "{backend:?}: {result}"
        );
        assert!(output(&result).is_empty());
        evaluate(
            &mut worker,
            "let overlapping={};setTimeout(()=>{Promise.resolve().then(()=>{try{nodeRepl.write('wrong-cell');overlapping.write='wrote'}catch(error){overlapping.write=error.message}})},50);",
        );
        let result = evaluate(
            &mut worker,
            "await new Promise(resolve=>setTimeout(resolve,140));nodeRepl.write(overlapping.write)",
        );
        assert_eq!(
            output(&result),
            "node_repl exec context not found",
            "{backend:?}: {result}"
        );
    }
}
#[test]
fn reaction_registered_in_new_cell_on_old_promise_uses_new_execution() {
    for backend in RuntimeBackend::available() {
        let mut worker = worker(backend);
        evaluate(
            &mut worker,
            "let oldPromise=new Promise(resolve=>setTimeout(()=>resolve('ready'),50));",
        );
        let result = evaluate(
            &mut worker,
            "oldPromise.then(value=>nodeRepl.write(value));await new Promise(resolve=>setTimeout(resolve,140));",
        );
        assert_eq!(output(&result), "ready", "{backend:?}: {result}");
    }
}
#[test]
fn unhandled_idle_rejection_or_throw_discards_only_the_failed_kernel() {
    for backend in RuntimeBackend::available() {
        for task in [
            "throw new Error('owned timer throw')",
            "Promise.reject(new Error('owned unhandled rejection'))",
            "while(true){}",
        ] {
            let mut worker = worker(backend);
            evaluate(
                &mut worker,
                &format!("let beforeUncaught=7;setTimeout(()=>{{{task}}},30);"),
            );
            let wait = Instant::now() + Duration::from_secs(2);
            while !worker.kernel_reset_pending() && Instant::now() < wait {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(worker.take_kernel_reset(), "{backend:?}: {task}");
            assert_eq!(
                evaluate(&mut worker, "typeof beforeUncaught")["value"],
                "undefined",
                "{backend:?}: {task}"
            );
        }
    }
}
#[test]
fn reset_drops_pending_timers_and_observed_rejection_preserves_kernel() {
    for backend in RuntimeBackend::available() {
        let mut worker = worker(backend);
        evaluate(
            &mut worker,
            "let laterRejected=new Promise((_,reject)=>setTimeout(()=>reject(new Error('owned rejection')),30));laterRejected.catch(()=>{});",
        );
        std::thread::sleep(Duration::from_millis(150));
        assert!(!worker.take_kernel_reset());
        assert_eq!(
            evaluate(
                &mut worker,
                "try{await laterRejected}catch(error){nodeRepl.write(error.message)}"
            )["error"],
            Value::Null
        );
        evaluate(
            &mut worker,
            "setTimeout(()=>{throw new Error('must be cancelled')},80);",
        );
        worker.reset().unwrap();
        std::thread::sleep(Duration::from_millis(180));
        assert_eq!(evaluate(&mut worker, "21*2")["value"], 42);
        assert!(!worker.take_kernel_reset());
    }
}

#[test]
fn late_callbacks_keep_original_metadata_and_new_reactions_use_new_metadata() {
    for backend in RuntimeBackend::available() {
        let mut worker = worker(backend);
        worker
            .set_request_meta(Some(
                json!({"x-codex-turn-metadata":{"call_id":"owned-old"}}),
            ))
            .unwrap();
        evaluate(
            &mut worker,
            "let taskMetadata={};let metadataPromise=new Promise(resolve=>setTimeout(()=>{taskMetadata.old=nodeRepl.requestMeta['x-codex-turn-metadata'].call_id;resolve()},60));",
        );
        worker
            .set_request_meta(Some(
                json!({"x-codex-turn-metadata":{"call_id":"owned-new"}}),
            ))
            .unwrap();
        let result = evaluate(
            &mut worker,
            "metadataPromise.then(()=>{taskMetadata.new=nodeRepl.requestMeta['x-codex-turn-metadata'].call_id});await new Promise(resolve=>setTimeout(resolve,140));taskMetadata",
        );
        assert_eq!(
            result["value"],
            json!({"old":"owned-old","new":"owned-new"}),
            "{backend:?}: {result}"
        );
    }
}

#[test]
fn async_await_thenable_and_subclass_reactions_retain_registration_context() {
    // Exact fresh installed-kernel cases: 20260907T012810164666Z.
    for backend in RuntimeBackend::available() {
        let mut worker = worker(backend);
        evaluate(
            &mut worker,
            "let overlappingAwait={};setTimeout(async()=>{await Promise.resolve();await{then(resolve){resolve()}};try{nodeRepl.write('wrong-cell');overlappingAwait.write='wrote'}catch(error){overlappingAwait.write=error.message}},50)",
        );
        let result = evaluate(
            &mut worker,
            "await new Promise(resolve=>setTimeout(resolve,140));nodeRepl.write(overlappingAwait.write)",
        );
        assert_eq!(
            output(&result),
            "node_repl exec context not found",
            "{backend:?}: {result}"
        );
        evaluate(
            &mut worker,
            "let subclassResult={};class OwnedPromise extends Promise{};let sharedObject={cycle:null};sharedObject.cycle=sharedObject;let oldSubclass=new OwnedPromise(resolve=>setTimeout(()=>resolve(sharedObject),50));",
        );
        let result = evaluate(
            &mut worker,
            "oldSubclass.then(value=>{subclassResult.same=value===value.cycle;nodeRepl.write('new-cell')});await new Promise(resolve=>setTimeout(resolve,140));nodeRepl.write(subclassResult.same)",
        );
        assert_eq!(output(&result), "new-celltrue", "{backend:?}: {result}");
    }
}
#[test]
fn native_reaction_tokens_survive_gc_and_do_not_call_mutated_promise_constructor() {
    for backend in RuntimeBackend::available() {
        let mut worker = worker(backend);
        let result = evaluate(
            &mut worker,
            r#"let retainedIntrinsic=Promise;let gcResults=[];let pendingGc=[];for(let i=0;i<2000;i++){let cycle={value:i};cycle.self=cycle;pendingGc.push(retainedIntrinsic.resolve(cycle).then(async value=>{await 0;return value===value.self?value.value:-1}))}for(let i=0;i<40000;i++){let garbage={};garbage.self=garbage};globalThis.Promise=function(){throw new Error('must not call model constructor')};gcResults=await retainedIntrinsic.all(pendingGc);globalThis.Promise=retainedIntrinsic;[gcResults.length,gcResults.reduce((sum,value)=>sum+value,0)]"#,
        );
        assert_eq!(
            result["value"],
            json!([2000, 1999000]),
            "{backend:?}: {result}"
        );
        assert!(!worker.take_kernel_reset(), "{backend:?}");
    }
}

#[test]
fn ordinary_error_drains_started_operations_and_preserves_primary_error() {
    // Original kernel.js catch-path drain; pinned six RPC and three image cases.
    for backend in RuntimeBackend::available() {
        for image in ["Uint8Array.from([137,80,78,71,13,10,26,10])", "'bad-image'"] {
            let mut worker = worker(backend);
            evaluate(&mut worker, "0");
            let now = Instant::now();
            let result = evaluate(
                &mut worker,
                &format!(
                    "let operationAfterFailure=nodeRepl.emitImage(new Promise(resolve=>setTimeout(()=>resolve({image}),180)));throw new Error('owned cell failure')"
                ),
            );
            assert!(
                now.elapsed() >= Duration::from_millis(150),
                "Ignored error-path operation: {backend:?}: {result}"
            );
            assert_eq!(
                result["exceptionMessage"], "owned cell failure",
                "{backend:?}: {result}"
            );
            assert!(
                !worker.kernel_reset_pending(),
                "Ordinary operation error discarded kernel: {backend:?}"
            );
            let later = evaluate(
                &mut worker,
                "try{await operationAfterFailure;nodeRepl.write('done')}catch(error){nodeRepl.write(error.message)}",
            );
            assert_eq!(
                output(&later),
                if image == "'bad-image'" {
                    "nodeRepl.emitImage only accepts data or file URLs"
                } else {
                    "done"
                },
                "{backend:?}: {later}"
            );
        }
        let mut worker = worker(backend);
        evaluate(&mut worker, "0");
        let result = evaluate_timed(
            &mut worker,
            "nodeRepl.emitImage(new Promise(resolve=>setTimeout(()=>resolve(Uint8Array.from([137,80,78,71,13,10,26,10])),400)));throw new Error('owned cell failure')",
            Duration::from_millis(100),
        );
        assert_eq!(
            result["exceptionMessage"], "js execution timed out; kernel reset, rerun your request",
            "{backend:?}: {result}"
        );
        assert!(result.get("executionDurationMs").is_none());
        assert!(worker.kernel_reset_pending());
    }
}
#[test]
fn tracked_finally_and_then_use_native_unhandled_rejection_rules() {
    for backend in RuntimeBackend::available() {
        for (suffix, reset) in [
            (".finally(()=>{})", true),
            (".finally(()=>{}).catch(()=>{})", false),
            (".then(()=>{})", true),
            (".then(undefined,()=>{})", false),
        ] {
            let mut worker = worker(backend);
            evaluate(&mut worker, "0");
            let result = evaluate(
                &mut worker,
                &format!(
                    "let observationMarker=7;nodeRepl.emitImage(new Promise(resolve=>setTimeout(()=>resolve('bad-image'),30))){suffix};nodeRepl.write('armed')"
                ),
            );
            assert_eq!(
                worker.kernel_reset_pending(),
                reset,
                "{backend:?} {suffix}: {result}"
            );
            if reset {
                assert!(
                    result["exceptionMessage"].as_str().is_some_and(
                        |text| text.starts_with("node_repl kernel unhandled rejection:")
                    ),
                    "{result}"
                );
                assert!(output(&result).is_empty());
            } else {
                assert!(result.get("error").is_none(), "{result}");
                assert_eq!(output(&result), "armed");
            }
            assert_eq!(
                evaluate(&mut worker, "typeof observationMarker")["value"],
                if reset { "undefined" } else { "number" }
            );
        }
    }
}
