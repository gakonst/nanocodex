//! Existing native dispatch recovery only: no imported-service/cache surrogate.
//! QuickJS Worker is in-process; V8 uses the actual owned skyre runtime child.
use serde_json::{Value, json};
use skyre::{
    Error, Result,
    runtime::{ExecutionValidity, Host, HostOptions, ProviderControl, RuntimeBackend},
    worker::{Event, Worker},
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const CELL_BUDGET: Duration = Duration::from_secs(5);
const EVENT_BUDGET: Duration = Duration::from_secs(15);
const FAILURE: &str = "owned ordinary handler failure";
const DISPATCHER: &str = "owned-retained-native-dispatcher";
const WARM: &str = r#"
let handlerMarker=40;
let retainedDispatcher=agent.browsers;
({marker:handlerMarker,meta:nodeRepl.requestMeta['x-codex-turn-metadata'].call_id})
"#;
const CAUGHT: &str = r#"
let handlerFailure;
try { await retainedDispatcher.get('owned'); }
catch(error) { handlerFailure={message:error.message,code:error.code}; }
let firstRecovered=await retainedDispatcher.get('owned');
({error:handlerFailure,browser:firstRecovered.browserId,marker:++handlerMarker,
  sameDispatcher:retainedDispatcher===agent.browsers,
  meta:nodeRepl.requestMeta['x-codex-turn-metadata'].call_id})
"#;
const SUCCESS: &str = r#"
({browser:(await retainedDispatcher.get('owned')).browserId,marker:++handlerMarker,
  sameDispatcher:retainedDispatcher===agent.browsers,
  meta:nodeRepl.requestMeta['x-codex-turn-metadata'].call_id})
"#;

fn metadata(call: &str) -> Value {
    json!({"x-codex-turn-metadata":{"call_id":call}})
}
#[derive(Default)]
struct Dispatcher {
    setup: usize,
    info: usize,
    calls: Vec<(String, Value)>,
    probes: Vec<ExecutionValidity>,
}
impl Dispatcher {
    fn dispatch(&mut self, method: &str, args: &Value, control: ProviderControl) -> Result<Value> {
        // Ordinary recovery must not retain a completed call's native lifetime.
        self.assert_completed_calls();
        let probe = control
            .execution_validity()
            .expect("actual native validator");
        probe.validate().unwrap();
        self.probes.push(probe);
        self.calls.push((method.to_owned(), args.clone()));
        match method {
            "sky.setup" => {
                assert_eq!(args, &json!({}));
                self.setup += 1;
                assert_eq!(self.setup, 1, "ordinary failure repeated setup");
                Ok(json!({"target":"mac"}))
            }
            "browser.info" => {
                assert_eq!(args, &json!({"browser":"owned"}));
                self.info += 1;
                if self.info == 1 {
                    Err(Error::action(FAILURE))
                } else {
                    // The public wrapper must use the new native reply. The
                    // identity and ordinal are not supplied by submitted JS.
                    Ok(json!({"id":format!("{DISPATCHER}-{}",self.info),"type":"cdp"}))
                }
            }
            other => panic!("Unexpected request in inert native dispatcher: {other}"),
        }
    }
    fn assert_completed_calls(&self) {
        assert!(self.probes.iter().all(|probe| probe.validate().is_err()));
    }
    fn assert_trace(&self, calls: usize) {
        self.assert_completed_calls();
        assert_eq!(self.setup, 1);
        assert_eq!(self.info, calls);
        assert_eq!(self.calls.len(), calls + 1);
        assert_eq!(self.calls[0], ("sky.setup".into(), json!({})));
        assert!(
            self.calls[1..]
                .iter()
                .all(|call| { call == &("browser.info".into(), json!({"browser":"owned"})) })
        );
    }
}

enum Harness {
    Host(Box<Host>),
    Worker(Box<Worker>),
}
impl Harness {
    fn new(runtime: RuntimeBackend, supervised: bool, owner: Rc<RefCell<Dispatcher>>) -> Self {
        let options = HostOptions {
            runtime,
            request_meta: Some(metadata("warm")),
            ..Default::default()
        };
        if supervised {
            Self::Worker(Box::new(Worker::with_options_and_executable(
                options,
                env!("CARGO_BIN_EXE_nanocodex-computer").into(),
            )))
        } else {
            Self::Host(Box::new(
                Host::with_controlled_dispatch(
                    move |method, args, control| owner.borrow_mut().dispatch(method, args, control),
                    Arc::new(AtomicBool::new(false)),
                    options,
                )
                .unwrap(),
            ))
        }
    }
    fn evaluate(&mut self, code: &str, owner: &Rc<RefCell<Dispatcher>>) -> Value {
        let result = match self {
            Self::Host(host) => host.evaluate(code, CELL_BUDGET).unwrap(),
            Self::Worker(worker) => {
                let ticket = worker.start(code, CELL_BUDGET).unwrap();
                // This includes the existing cold-child startup budget; each
                // received call cannot renew the outer progress deadline.
                let deadline = Instant::now() + EVENT_BUDGET;
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    assert!(!remaining.is_zero(), "bounded actual Worker progress");
                    match worker.event(remaining).unwrap().expect("worker progress") {
                        Event::Call {
                            method,
                            args,
                            control,
                            reply,
                        } => {
                            let result = owner.borrow_mut().dispatch(&method, &args, control);
                            reply.send(result).unwrap();
                        }
                        Event::Done {
                            ticket: done,
                            result,
                        } => {
                            assert_eq!(done, ticket);
                            break result.unwrap();
                        }
                    }
                }
            }
        };
        self.assert_healthy();
        owner.borrow().assert_completed_calls();
        result
    }
    fn metadata(&mut self, call: &str) {
        match self {
            Self::Host(host) => host.set_request_meta(Some(metadata(call))).unwrap(),
            Self::Worker(worker) => worker.set_request_meta(Some(metadata(call))).unwrap(),
        }
    }
    fn assert_healthy(&self) {
        match self {
            Self::Host(host) => {
                assert!(!host.interrupted(), "ordinary handler error poisoned Host")
            }
            Self::Worker(worker) => {
                assert!(!worker.cancelled());
                assert!(
                    !worker.kernel_reset_pending(),
                    "ordinary handler error reset Worker"
                );
            }
        }
    }
    fn child(&self) -> Option<u32> {
        match self {
            Self::Host(_) => None,
            Self::Worker(worker) => worker.runtime_child_pid(),
        }
    }
}

fn success(result: &Value, ordinal: usize, marker: usize, call: &str) {
    assert!(result.get("error").is_none(), "{result}");
    assert_eq!(
        result["value"]["browser"],
        format!("{DISPATCHER}-{ordinal}")
    );
    assert_eq!(result["value"]["marker"], marker);
    assert_eq!(result["value"]["sameDispatcher"], true);
    assert_eq!(result["value"]["meta"], call);
}
fn recovery(supervised: bool, caught: bool) {
    for runtime in RuntimeBackend::available() {
        let owner = Rc::new(RefCell::new(Dispatcher::default()));
        let mut harness = Harness::new(runtime, supervised, owner.clone());
        let warm = harness.evaluate(WARM, &owner);
        assert!(warm.get("error").is_none(), "{runtime:?}: {warm}");
        assert_eq!(warm["value"], json!({"marker":40,"meta":"warm"}));
        let child = harness.child();
        assert_eq!(child.is_some(), supervised && runtime == RuntimeBackend::V8);
        owner.borrow().assert_trace(0);
        harness.metadata("failure-request");
        if caught {
            let first = harness.evaluate(CAUGHT, &owner);
            success(&first, 2, 41, "failure-request");
            assert_eq!(
                first["value"]["error"],
                json!({"message":FAILURE,"code":-10005})
            );
            owner.borrow().assert_trace(2);
        } else {
            let first = harness.evaluate("await retainedDispatcher.get('owned')", &owner);
            assert!(
                first["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(FAILURE),
                "{first}"
            );
            owner.borrow().assert_trace(1);
            harness.metadata("recovery-request");
            let second = harness.evaluate(SUCCESS, &owner);
            success(&second, 2, 41, "recovery-request");
            owner.borrow().assert_trace(2);
        }
        assert_eq!(
            harness.child(),
            child,
            "ordinary failure replaced the owned child"
        );
        harness.metadata("following-request");
        let next = harness.evaluate(SUCCESS, &owner);
        success(&next, 3, 42, "following-request");
        owner.borrow().assert_trace(3);
        assert_eq!(harness.child(), child);
    }
}

#[test]
fn native_host_caught_handler_error_recovers_in_same_cell_and_retains_dispatcher() {
    recovery(false, true);
}
#[test]
fn native_host_uncaught_handler_error_recovers_in_next_cell_with_current_metadata() {
    recovery(false, false);
}
#[test]
fn actual_worker_caught_handler_error_recovers_in_same_cell_without_setup_or_child_reset() {
    recovery(true, true);
}
#[test]
fn actual_worker_uncaught_handler_error_recovers_in_next_cell_with_live_native_dispatch() {
    recovery(true, false);
}
