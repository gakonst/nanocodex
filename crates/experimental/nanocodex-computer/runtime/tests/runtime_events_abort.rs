//! Real signal-owner helper contracts, separate from generic EventTarget support.
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
const ORACLE: &str = include_str!("oracles/runtime_events_abort.json");
fn answer(method: &str, args: &Value) -> Value {
    assert_eq!(
        method, "sky.setup",
        "signal helpers must not dispatch a provider"
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
#[test]
fn owned_signal_helpers_match_nineteen_installed_kernel_cases() {
    assert_eq!(
        format!("{:x}", Sha256::digest(ORACLE)),
        "b2d2928a591a118a331c8c1a22b3eb073de9b8835c50d8b73ac150bd758775f8"
    );
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle["cases"].as_array().unwrap().len(), 19);
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime);
        for case in oracle["cases"].as_array().unwrap() {
            let result = host
                .evaluate(case["code"].as_str().unwrap(), Duration::from_secs(5))
                .unwrap();
            assert!(
                result.get("error").is_none(),
                "{runtime:?} {}: {result}",
                case["name"]
            );
            let output: Vec<_> = result["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["channel"] == "output")
                .collect();
            assert_eq!(output.len(), 1, "{}: {result}", case["name"]);
            let actual: Value = serde_json::from_str(output[0]["value"].as_str().unwrap()).unwrap();
            assert_eq!(actual, case["expected"], "{runtime:?}: {}", case["name"]);
        }
        assert_eq!(calls.get(), 1, "mandatory bootstrap only");
    }
}
const FIRST: &str = r#"
var events=await import('node:events'),controller=new AbortController(),signal=controller.signal;
var callbackState={calls:0};
signal.addEventListener('abort',()=>callbackState.calls++,{once:true});
signal.onabort=()=>callbackState.calls++;
events.setMaxListeners(7,signal);
({count:events.listenerCount(signal,'abort'),max:events.getMaxListeners(signal),calls:callbackState.calls})
"#;
const LATER: &str = r#"
var alias=await import('events');controller.abort('owned');
({same:alias===events,count:alias.listenerCount(signal,'abort'),list:alias.getEventListeners(signal,'abort').length,max:alias.getMaxListeners(signal),calls:callbackState.calls,reason:signal.reason})
"#;
fn first() -> Value {
    json!({"count":2,"max":7,"calls":0})
}
fn later() -> Value {
    json!({"same":true,"count":1,"list":1,"max":7,"calls":2,"reason":"owned"})
}
#[test]
fn signal_owner_is_retained_across_cells_and_isolated_between_hosts() {
    for runtime in RuntimeBackend::available() {
        let (mut first_host, first_calls) = host(runtime);
        let (mut second_host, second_calls) = host(runtime);
        assert_eq!(evaluate(&mut first_host, FIRST), first());
        assert_eq!(
            evaluate(
                &mut second_host,
                "var e=await import('events'),s=new AbortController().signal;[e.listenerCount(s,'abort'),e.getMaxListeners(s),typeof callbackState]"
            ),
            json!([0, 0, "undefined"])
        );
        assert_eq!(evaluate(&mut first_host, LATER), later());
        assert_eq!(first_calls.get(), 1);
        assert_eq!(second_calls.get(), 1);
    }
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
fn production_worker_preserves_signal_identity_and_discards_it_on_reset() {
    for runtime in RuntimeBackend::available() {
        let mut worker = Worker::with_options_and_executable(
            HostOptions {
                runtime,
                ..Default::default()
            },
            env!("CARGO_BIN_EXE_nanocodex-computer").into(),
        );
        let mut calls = 0;
        assert_eq!(worker_cell(&mut worker, FIRST, &mut calls), first());
        let pid = worker.runtime_child_pid();
        assert_eq!(pid.is_some(), runtime == RuntimeBackend::V8);
        assert_eq!(worker_cell(&mut worker, LATER, &mut calls), later());
        assert_eq!(worker.runtime_child_pid(), pid);
        assert!(!worker.take_kernel_reset());
        assert_eq!(calls, 1);
        worker.reset().unwrap();
        assert_eq!(
            worker_cell(
                &mut worker,
                "var e=await import('events'),s=new AbortController().signal;[typeof controller,typeof callbackState,e.listenerCount(s,'abort'),e.getMaxListeners(s)]",
                &mut calls
            ),
            json!(["undefined", "undefined", 0, 0])
        );
        assert_eq!(calls, 2);
    }
}
#[test]
fn warning_maximum_cannot_remove_allocation_bound_and_timer_abort_still_cleans_up() {
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime);
        assert_eq!(
            evaluate(
                &mut host,
                r#"
var e=await import('node:events'),timers=await import('node:timers/promises'),c=new AbortController(),s=c.signal;
e.setMaxListeners(0,s);var listeners=Array.from({length:1024},()=>()=>{});
for(const listener of listeners)s.addEventListener('abort',listener);
var refused;try{s.addEventListener('abort',()=>{})}catch(error){refused=error.message}
var full=e.listenerCount(s,'abort');s.removeEventListener('abort',listeners[0]);
var pending=timers.setTimeout(1000,0,{signal:s});var active=e.listenerCount(s,'abort');
c.abort('bounded');var errorCode;try{await pending}catch(error){errorCode=error.code}
({refused,full,active,after:e.listenerCount(s,'abort'),maximum:e.getMaxListeners(s),errorCode})
"#
            ),
            json!({"refused":"Abort listener budget exceeded","full":1024,"active":1024,"after":1023,"maximum":0,"errorCode":"ABORT_ERR"})
        );
        assert_eq!(calls.get(), 1);
    }
}
#[test]
fn helper_bridge_is_immutable_and_only_brands_genuine_targets() {
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime);
        assert_eq!(
            evaluate(
                &mut host,
                r#"
var holder=__skyreTimers,bridge=holder.eventTargetObservers;
var replacement=Reflect.defineProperty(holder,'eventTargetObservers',{value:{has(){return true}}});
var e=await import('events'),s=new AbortController().signal;
var generic=new (Object.getPrototypeOf(AbortSignal))();var refused=[];
for(const value of [{aborted:false},Object.create(AbortSignal.prototype),generic]){
 var row=[];for(const call of [()=>e.listenerCount(value,'abort'),()=>e.getEventListeners(value,'abort'),()=>e.getMaxListeners(value),()=>e.setMaxListeners(3,value)])try{call();row.push(false)}catch(error){row.push(error.code==='ERR_INVALID_ARG_TYPE')}
 refused.push(row)
}
e.setMaxListeners(4,s);var invalid=false;try{bridge.setMaximum(s,-1)}catch{invalid=true}
({replacement,frozen:Object.isFrozen(bridge),same:bridge===holder.eventTargetObservers,refused,invalid,maximum:e.getMaxListeners(s),count:e.listenerCount(s,'abort'),eventTarget:typeof EventTarget})
"#
            ),
            json!({"replacement":false,"frozen":true,"same":true,"refused":[[true,true,true,true],[true,true,true,true],[false,false,false,false]],"invalid":true,"maximum":4,"count":0,"eventTarget":"undefined"})
        );
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn full_signal_budget_rejects_timer_registration_and_releases_native_slots() {
    for runtime in RuntimeBackend::available() {
        for kind in ["timeout", "immediate", "interval"] {
            let (mut host, calls) = host(runtime);
            let code = format!(
                "var registrationKind = {};\n{}",
                serde_json::to_string(kind).unwrap(),
                r#"
var e=await import('node:events'),timers=await import('node:timers/promises');
var c=new AbortController(),s=c.signal;
e.setMaxListeners(0,s);
var listeners=Array.from({length:1024},()=>()=>{});
for(const listener of listeners)s.addEventListener('abort',listener);
var before=e.listenerCount(s,'abort'),iterator;
var pending=registrationKind==='timeout'?timers.setTimeout(10000,'owned',{signal:s})
 :registrationKind==='immediate'?timers.setImmediate('owned',{signal:s})
 :(iterator=timers.setInterval(10000,'owned',{signal:s})).next();
var observed=pending.then(()=>({status:'fulfilled'}),error=>({status:'rejected',name:error.name,message:error.message}));
var afterRegistration=e.listenerCount(s,'abort');
// Probe before awaiting: a leaked immediate must not run and free its slot.
// The interval generator starts allocation/registration when next() is called.
var callbackState={calls:0};
function timerCapacity(){
 var allocated=[],capacityError=null,allocatedCount;
 try{
  for(var i=0;i<1025;i++)allocated.push(setInterval(()=>callbackState.calls++,10000));
 }catch(error){capacityError=error.message}
 finally{
  allocatedCount=allocated.length;
  for(const timer of allocated)clearInterval(timer);
 }
 return {allocatedCount,capacityError};
}
var beforeSettlement=timerCapacity();
var outcome=await observed;
var afterSettlement=timerCapacity();
var snapshot=e.getEventListeners(s,'abort');
var closed=registrationKind==='interval'?(await iterator.next()).done:null;
({before,afterRegistration,afterCount:e.listenerCount(s,'abort'),
 list:snapshot.length,sameListeners:snapshot.every((listener,index)=>listener===listeners[index]),
 maximum:e.getMaxListeners(s),outcome,beforeSettlement,afterSettlement,
 callbackCalls:callbackState.calls,closed,aborted:s.aborted})
"#
            );
            assert_eq!(
                evaluate(&mut host, &code),
                json!({
                    "before":1024,"afterRegistration":1024,"afterCount":1024,
                    "list":1024,"sameListeners":true,"maximum":0,
                    "outcome":{"status":"rejected","name":"Error","message":"Abort listener budget exceeded"},
                    "beforeSettlement":{"allocatedCount":1025,"capacityError":Value::Null},
                    "afterSettlement":{"allocatedCount":1025,"capacityError":Value::Null},
                    "callbackCalls":0,"closed":if kind == "interval" {json!(true)} else {Value::Null},
                    "aborted":false
                }),
                "{runtime:?}: {kind} registration failure cleanup"
            );
            assert_eq!(calls.get(), 1, "{runtime:?}: {kind} bootstrap only");
        }
    }
}
