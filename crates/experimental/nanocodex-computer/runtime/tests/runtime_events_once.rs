//! The ordinary events.once slice uses real per-Host listener owners.
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
const ORACLE: &str = include_str!("oracles/runtime_events_once.json");
fn answer(method: &str, args: &Value) -> Value {
    assert_eq!(
        method, "sky.setup",
        "owned event helpers must not dispatch a provider"
    );
    assert_eq!(args, &json!({}));
    json!({"target":"mac"})
}
fn host(runtime: RuntimeBackend) -> (Host, Rc<Cell<usize>>) {
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
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
    (host, calls)
}
fn evaluate(host: &mut Host, code: &str) -> Value {
    let result = host.evaluate(code, Duration::from_secs(5)).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    result["value"].clone()
}
fn worker_cell(worker: &mut Worker, code: &str, calls: &mut usize) -> Value {
    let ticket = worker.start(code, Duration::from_secs(5)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("bounded Worker response");
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
fn events_once_matches_twenty_original_kernel_cases() {
    assert_eq!(
        format!("{:x}", Sha256::digest(ORACLE)),
        "89e737afe02308b57f691f56bd5513b061a9f4fd5ec632f5d71157d0db79743c"
    );
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle["cases"].as_array().unwrap().len(), 20);
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime);
        for case in oracle["cases"].as_array().unwrap() {
            let result = host
                .evaluate(case["code"].as_str().unwrap(), Duration::from_secs(5))
                .unwrap();
            assert!(
                result.get("error").is_none(),
                "{runtime:?} {case}: {result}"
            );
            let outputs: Vec<_> = result["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["channel"] == "output")
                .collect();
            assert_eq!(outputs.len(), 1, "{}: {result}", case["name"]);
            let actual: Value =
                serde_json::from_str(outputs[0]["value"].as_str().unwrap()).unwrap();
            assert_eq!(actual, case["expected"], "{runtime:?}: {}", case["name"]);
        }
        assert_eq!(calls.get(), 1, "mandatory bootstrap only");
    }
}

#[test]
fn failed_abort_registration_releases_wait_listeners_and_preserves_other_nodes() {
    const CODE: &str = r#"
await(async()=>{
  const h=await import('node:events'),Target=Object.getPrototypeOf(AbortSignal);
  const source=new AbortController(),slots=Array.from({length:1024},()=>()=>{});
  slots.forEach(fn=>source.signal.addEventListener('abort',fn));
  const origin=new AbortController();let EventClass;
  origin.signal.addEventListener('abort',event=>{EventClass=event.constructor},{once:true});origin.abort();
  const rows=[];
  for(const kind of ['emitter','target']){
    const target=kind==='emitter'?new h.EventEmitter():new Target();
    const pending=h.once(target,'owned',{signal:source.signal});
    let failure;try{await pending;failure=null;}catch(error){failure=error.message;}
    const counts=[h.listenerCount(target,'owned'),h.listenerCount(target,'error'),h.listenerCount(source.signal,'abort')];
    let capacity=null;
    if(kind==='target'){
      const listeners=Array.from({length:1024},()=>()=>{});listeners.forEach(fn=>target.addEventListener('capacity',fn));
      let overflow;try{target.addEventListener('capacity',()=>{});}catch(error){overflow=error.message;}
      capacity={count:h.listenerCount(target,'capacity'),overflow};
      listeners.forEach(fn=>target.removeEventListener('capacity',fn));
    }
    rows.push({kind,failure,counts,capacity});
  }
  const originalNodes=h.getEventListeners(source.signal,'abort');
  const unchanged=originalNodes.length===1024&&originalNodes.every((fn,index)=>fn===slots[index]);
  source.signal.removeEventListener('abort',slots[0]);
  const emitter=new h.EventEmitter(),pending=h.once(emitter,'owned',{signal:source.signal});
  const during=h.listenerCount(source.signal,'abort');emitter.emit('owned',7);const value=await pending;
  const after=[h.listenerCount(emitter,'owned'),h.listenerCount(emitter,'error'),h.listenerCount(source.signal,'abort')];
  slots.slice(1).forEach(fn=>source.signal.removeEventListener('abort',fn));
  return {rows,unchanged,during,value,after,final:h.listenerCount(source.signal,'abort')};
})()
"#;
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime);
        assert_eq!(
            evaluate(&mut host, CODE),
            json!({
                "rows":[
                    {"kind":"emitter","failure":"Abort listener budget exceeded","counts":[0,0,1024],"capacity":null},
                    {"kind":"target","failure":"Abort listener budget exceeded","counts":[0,0,1024],
                     "capacity":{"count":1024,"overflow":"Abort listener budget exceeded"}}
                ],"unchanged":true,"during":1024,"value":[7],"after":[0,0,1023],"final":0
            }),
            "{runtime:?}"
        );
        assert_eq!(calls.get(), 1);
    }
}

const FIRST: &str = r#"
var h=await import('node:events'),emitter=new h.EventEmitter(),callbackState={values:[]};
var pending=h.once(emitter,'owned');emitter.emit('owned',1);callbackState.values.push((await pending)[0]);
({same:h.once===h.default.once,values:callbackState.values,counts:[emitter.listenerCount('owned'),emitter.listenerCount('error')]})
"#;
const LATER: &str = r#"
var alias=await import('events'),next=alias.once(emitter,'owned');
emitter.emit('owned',2);callbackState.values.push((await next)[0]);
({same:alias===h&&alias.once===h.once,values:callbackState.values,counts:[emitter.listenerCount('owned'),emitter.listenerCount('error')]})
"#;
fn settled(values: Value) -> Value {
    json!({"same":true,"values":values,"counts":[0,0]})
}
#[test]
fn settled_waits_and_module_identity_remain_in_their_host() {
    for runtime in RuntimeBackend::available() {
        let (mut first, first_calls) = host(runtime);
        let (mut second, second_calls) = host(runtime);
        assert_eq!(evaluate(&mut first, FIRST), settled(json!([1])));
        assert_eq!(evaluate(&mut first, LATER), settled(json!([1, 2])));
        assert_eq!(evaluate(&mut second, FIRST), settled(json!([1])));
        assert_eq!(first_calls.get(), 1);
        assert_eq!(second_calls.get(), 1);
    }
}
#[test]
fn production_worker_reuses_settled_wait_owner_and_reset_drops_cell_objects() {
    for runtime in RuntimeBackend::available() {
        let mut worker = Worker::with_options_and_executable(
            HostOptions {
                runtime,
                ..Default::default()
            },
            env!("CARGO_BIN_EXE_nanocodex-computer").into(),
        );
        let mut calls = 0;
        assert_eq!(
            worker_cell(&mut worker, FIRST, &mut calls),
            settled(json!([1]))
        );
        let pid = worker.runtime_child_pid();
        assert_eq!(pid.is_some(), runtime == RuntimeBackend::V8);
        assert_eq!(
            worker_cell(&mut worker, LATER, &mut calls),
            settled(json!([1, 2]))
        );
        assert_eq!(worker.runtime_child_pid(), pid);
        assert!(!worker.take_kernel_reset());
        assert_eq!(calls, 1);
        worker.reset().unwrap();
        assert_eq!(
            worker_cell(
                &mut worker,
                "var n=await import('node:events');[typeof emitter,typeof callbackState,n.once===n.default.once]",
                &mut calls
            ),
            json!(["undefined", "undefined", true])
        );
        assert_eq!(calls, 2);
    }
}
