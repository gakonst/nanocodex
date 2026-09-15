//! Genuine inherited EventTarget/Event owners; no global constructor publication.
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
const ORACLE: &str = include_str!("oracles/runtime_eventtarget.json");
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
fn inherited_eventtarget_matches_seventeen_original_kernel_cases() {
    assert_eq!(
        format!("{:x}", Sha256::digest(ORACLE)),
        "7b5bbd8e9ac25d12223f2dc08ef0a4eca0d2659923fd2f19385eec31536aed86"
    );
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle["cases"].as_array().unwrap().len(), 17);
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
const FIRST_TARGET: &str = r#"
var Target=Object.getPrototypeOf(AbortSignal),target=new Target();
var controller=new AbortController(),EventClass;
controller.signal.addEventListener('abort',event=>{EventClass=event.constructor},{once:true});controller.abort();
var events=await import('node:events'),callbackState={calls:0};
target.addEventListener('owned',()=>callbackState.calls++);
({maximum:events.getMaxListeners(target),count:events.listenerCount(target,'owned'),calls:callbackState.calls,
 globals:[typeof EventTarget,typeof Event]})
"#;
const LATER_TARGET: &str = r#"
var alias=await import('events');alias.setMaxListeners(4);
var result=target.dispatchEvent(new EventClass('owned'));
({same:alias===events,result,maximum:alias.getMaxListeners(target),fresh:alias.getMaxListeners(new Target()),
 signal:alias.getMaxListeners(new AbortController().signal),count:alias.listenerCount(target,'owned'),calls:callbackState.calls})
"#;
fn first_target() -> Value {
    json!({"maximum":10,"count":1,"calls":0,"globals":["undefined","undefined"]})
}
fn later_target() -> Value {
    json!({"same":true,"result":true,"maximum":10,"fresh":4,"signal":0,"count":1,"calls":1})
}
#[test]
fn inherited_targets_initialize_before_import_and_remain_host_owned() {
    for runtime in RuntimeBackend::available() {
        let (mut first, first_calls) = host(runtime);
        let (mut second, second_calls) = host(runtime);
        assert_eq!(evaluate(&mut first, FIRST_TARGET), first_target());
        assert_eq!(evaluate(&mut first, LATER_TARGET), later_target());
        assert_eq!(evaluate(&mut second, FIRST_TARGET), first_target());
        assert_eq!(first_calls.get(), 1);
        assert_eq!(second_calls.get(), 1);
    }
}
#[test]
fn production_worker_retains_target_callbacks_and_resets_common_default() {
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
            worker_cell(&mut worker, FIRST_TARGET, &mut calls),
            first_target()
        );
        let pid = worker.runtime_child_pid();
        assert_eq!(pid.is_some(), runtime == RuntimeBackend::V8);
        assert_eq!(
            worker_cell(&mut worker, LATER_TARGET, &mut calls),
            later_target()
        );
        assert_eq!(worker.runtime_child_pid(), pid);
        assert!(!worker.take_kernel_reset());
        assert_eq!(calls, 1);
        worker.reset().unwrap();
        assert_eq!(
            worker_cell(
                &mut worker,
                "var h=await import('events');[typeof target,typeof callbackState,h.getMaxListeners(new (Object.getPrototypeOf(AbortSignal))()),h.getMaxListeners(new AbortController().signal)]",
                &mut calls
            ),
            json!(["undefined", "undefined", 10, 0])
        );
        assert_eq!(calls, 2);
    }
}
#[test]
fn event_timestamps_use_a_private_monotonic_host_clock_and_remain_stable() {
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime);
        let before = evaluate(
            &mut host,
            r#"
var c=new AbortController(),EventClass;c.signal.addEventListener('abort',event=>{EventClass=event.constructor});c.abort();
var clockEvent=new EventClass('clock'),savedStamp=clockEvent.timeStamp;
({bridge:typeof __skyre_event_now,ownBridge:Object.hasOwn(globalThis,'__skyre_event_now'),finite:Number.isFinite(savedStamp),nonnegative:savedStamp>=0,stable:clockEvent.timeStamp===savedStamp})
"#,
        );
        assert_eq!(
            before,
            json!({"bridge":"undefined","ownBridge":false,"finite":true,"nonnegative":true,"stable":true})
        );
        assert_eq!(
            evaluate(
                &mut host,
                r#"
await new Promise(resolve=>setTimeout(resolve,5));
var laterEvent=new EventClass('later');
({advanced:laterEvent.timeStamp>savedStamp,stable:clockEvent.timeStamp===savedStamp,bridge:typeof __skyre_event_now,
 globals:[typeof EventTarget,typeof Event]})
"#,
            ),
            json!({"advanced":true,"stable":true,"bridge":"undefined","globals":["undefined","undefined"]})
        );
        assert_eq!(calls.get(), 1);
    }
}
#[test]
fn dispatch_retirement_and_signal_option_rollback_preserve_allocation_bounds() {
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime);
        assert_eq!(
            evaluate(
                &mut host,
                r#"
var h=await import('node:events'),Target=Object.getPrototypeOf(AbortSignal),target=new Target();h.setMaxListeners(0,target);
var c=new AbortController(),EventClass;c.signal.addEventListener('abort',event=>{EventClass=event.constructor},{once:true});c.abort();
var state={churn:null,calls:0},listeners=[];
function first(){
 state.calls++;
 for(const listener of listeners)target.removeEventListener('owned',listener);
 try{target.addEventListener('owned',()=>{})}catch(error){state.churn=error.message}
}
target.addEventListener('owned',first);
for(var i=0;i<1023;i++){var listener=()=>state.calls++;listeners.push(listener);target.addEventListener('owned',listener)}
var full=h.listenerCount(target,'owned');target.dispatchEvent(new EventClass('owned'));
var afterDispatch=h.listenerCount(target,'owned');
for(const listener of listeners)target.addEventListener('owned',listener);
var restored=h.listenerCount(target,'owned'),source=new AbortController(),failure;
try{target.addEventListener('owned',()=>{},{signal:source.signal})}catch(error){failure=error.message}
({full,afterDispatch,restored,churn:state.churn,calls:state.calls,failure,
 sourceCount:h.listenerCount(source.signal,'abort'),sourceList:h.getEventListeners(source.signal,'abort').length,
 sourceAborted:source.signal.aborted,maximum:h.getMaxListeners(target)})
"#,
            ),
            json!({"full":1024,"afterDispatch":1,"restored":1024,"churn":"Abort listener budget exceeded","calls":1,
                "failure":"Abort listener budget exceeded","sourceCount":0,"sourceList":0,"sourceAborted":false,"maximum":0})
        );
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn signal_cancellation_uses_the_once_normalized_listener_type() {
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime);
        assert_eq!(
            evaluate(
                &mut host,
                r#"
var h=await import('node:events'),Target=Object.getPrototypeOf(AbortSignal),target=new Target();
var source=new AbortController(),typeState={reads:0,text:'owned',calls:0};
var type={toString(){typeState.reads++;return typeState.text}},listener=()=>typeState.calls++;
target.addEventListener(type,listener,{signal:source.signal});
var before={reads:typeState.reads,count:h.listenerCount(target,'owned'),same:h.getEventListeners(target,'owned')[0]===listener};
typeState.text='changed';source.abort();
({before,reads:typeState.reads,owned:h.listenerCount(target,'owned'),changed:h.listenerCount(target,'changed'),
 sourceCount:h.listenerCount(source.signal,'abort'),aborted:source.signal.aborted,
 reasonTag:Object.prototype.toString.call(source.signal.reason),calls:typeState.calls})
"#,
            ),
            json!({"before":{"reads":1,"count":1,"same":true},"reads":1,"owned":0,"changed":0,
                "sourceCount":0,"aborted":true,"reasonTag":"[object DOMException]","calls":0})
        );
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn event_construction_clock_precedes_owned_option_getters() {
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime);
        assert_eq!(
            evaluate(
                &mut host,
                r#"
var source=new AbortController(),EventClass;
source.signal.addEventListener('abort',event=>{EventClass=event.constructor},{once:true});source.abort();
var state={order:[],nested:null};
var outer=new EventClass({toString(){state.order.push('type');return 'outer'}},{
 get bubbles(){state.order.push('bubbles');state.nested=new EventClass('nested');return false},
 get cancelable(){state.order.push('cancelable');return false},
 get composed(){state.order.push('composed');return false}
});
({order:state.order,finite:Number.isFinite(outer.timeStamp)&&Number.isFinite(state.nested.timeStamp),
 nonnegative:outer.timeStamp>=0&&state.nested.timeStamp>=0,beforeNested:outer.timeStamp<=state.nested.timeStamp,
 bridge:typeof __skyre_event_now,outerType:outer.type,nestedType:state.nested.type})
"#,
            ),
            json!({"order":["bubbles","cancelable","composed","type"],"finite":true,"nonnegative":true,
                "beforeNested":true,"bridge":"undefined","outerType":"outer","nestedType":"nested"})
        );
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn target_symbol_names_use_domstring_errors_without_changing_event_coercion() {
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime);
        assert_eq!(
            evaluate(
                &mut host,
                r#"
var h=await import('node:events'),Target=Object.getPrototypeOf(AbortSignal),target=new Target();
var source=new AbortController(),EventClass;source.signal.addEventListener('abort',event=>{EventClass=event.constructor},{once:true});source.abort();
var symbol=Symbol('owned'),listener=()=>{},state={removeCapture:0},event=new EventClass('kept');
function failure(fn){try{fn();return {ok:true}}catch(error){return {ok:false,name:error.name,code:error.code??null,message:error.message}}}
var add=failure(()=>target.addEventListener(symbol,listener));
var remove=failure(()=>target.removeEventListener(symbol,listener,{get capture(){state.removeCapture++;return false}}));
var construct=failure(()=>new EventClass(symbol)),init=failure(()=>event.initEvent(symbol));
({add,remove,construct,init,removeCapture:state.removeCapture,type:event.type,
 count:h.listenerCount(target,symbol),list:h.getEventListeners(target,symbol).length,
 globals:[typeof EventTarget,typeof Event]})
"#,
            ),
            json!({
                "add":{"ok":false,"name":"TypeError","code":"ERR_INVALID_ARG_TYPE","message":"Value is a Symbol and cannot be converted to a string."},
                "remove":{"ok":false,"name":"TypeError","code":"ERR_INVALID_ARG_TYPE","message":"Value is a Symbol and cannot be converted to a string."},
                "construct":{"ok":false,"name":"TypeError","code":null,"message":"Cannot convert a Symbol value to a string"},
                "init":{"ok":false,"name":"TypeError","code":null,"message":"Cannot convert a Symbol value to a string"},
                "removeCapture":0,"type":"kept","count":0,"list":0,"globals":["undefined","undefined"]
            })
        );
        assert_eq!(calls.get(), 1);
    }
}
