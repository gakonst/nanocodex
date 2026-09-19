//! Four existing formatter criteria through installed Host/Worker CUA owners.
//! Native dispatch supplies only owned DTOs. No desktop, source eval or test hook.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use skyre::{
    Result,
    runtime::{Host, HostOptions, RuntimeBackend},
    worker::{Event, Worker},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

const ORIGINAL: &str = include_str!("oracles/app_state_original_observations.json");
const FORMATTER: &[u8] = include_bytes!("oracles/app_state_window_result.js");
const CANONICAL: &str = "/owned/Fixture.app";
const BUNDLE: &str = "owned.app.state";
const TREE: &str = "fixture tree";
const GUIDANCE: &str = "Fixture instructions";

fn original_first() -> String {
    assert_eq!(
        format!("{:x}", Sha256::digest(ORIGINAL.as_bytes())),
        "fe2034e171a20fba5ed712c78b7bf4b8905277fa28b7b91541fc518049a0f402"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(FORMATTER)),
        "e1ac0e53f8f8102fbebba5ce8ab0e1648d1399cb7580fb8e98423a469c7d2701"
    );
    let original: Value = serde_json::from_str(ORIGINAL).unwrap();
    assert_eq!(original["passed"], 14);
    assert_eq!(original["failed"], 0);
    original["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "instruction-prefix-exact-and-only-once-per-bundle")
        .unwrap()["evidence"]["first"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn state(app: Value, guidance: Value) -> Value {
    json!({"app":app,"skyshot":{"text":TREE,"screenshot":null},"appSpecificInstructions":guidance})
}
fn ordinary_state() -> Value {
    state(json!({"bundleIdentifier":BUNDLE}), json!(GUIDANCE))
}
fn metadata(id: &str) -> Value {
    json!({"x-codex-turn-metadata":{"call_id":id}})
}
fn policy() -> Value {
    json!({"decision":"allowed","allowPersistentApproval":false,
        "target":{"bundleIdentifier":BUNDLE,"displayName":"Owned app-state fixture","appPath":CANONICAL,"risk":"low"}})
}
fn approval(id: &str) -> Value {
    json!({"message":"Allow Computer Use to use \"Owned app-state fixture\"?","meta":{
        "codex_approval_kind":"mcp_tool_call","connector_id":"computer-use","connector_name":"Computer Use",
        "persist":["session"],"riskLevel":"low","tool_call_id":id,"tool_name":"get_app_state",
        "tool_params":{"app":BUNDLE},"tool_params_display":[{"name":"app","display_name":"App","value":"Owned app-state fixture"}]}})
}
#[derive(Clone)]
struct Reply {
    requested: String,
    disable_diff: Option<bool>,
    value: Value,
}
#[derive(Default)]
struct Trace {
    replies: VecDeque<Reply>,
    stage: u8,
    calls: Vec<(String, Value)>,
    expected_calls: Vec<(String, Value)>,
    cell: String,
    setup_count: usize,
}
impl Trace {
    fn answer(&mut self, method: &str, args: &Value) -> Result<Value> {
        self.calls.push((method.into(), args.clone()));
        if method == "sky.setup" {
            assert_eq!(args, &json!({}));
            assert_eq!(self.stage, 0);
            self.setup_count += 1;
            self.expected_calls.push((method.into(), json!({})));
            return Ok(json!({"target":"mac","methods":["get_app_state"]}));
        }
        let reply = self
            .replies
            .front()
            .expect("only an expected owned app call");
        match self.stage {
            0 => {
                assert_eq!(method, "sky.app_policy");
                let expected = json!({"app":reply.requested});
                assert_eq!(*args, expected);
                self.expected_calls.push((method.into(), expected));
                self.stage = 1;
                Ok(policy())
            }
            1 => {
                assert_eq!(method, "host.elicitation");
                let expected = approval(&self.cell);
                assert_eq!(*args, expected);
                self.expected_calls.push((method.into(), expected));
                self.stage = 2;
                Ok(json!({"action":"accept"}))
            }
            2 => {
                assert_eq!(method, "sky.execute");
                let reply = self.replies.pop_front().unwrap();
                let mut input = json!({"app":CANONICAL});
                if let Some(value) = reply.disable_diff {
                    input["disableDiff"] = json!(value);
                }
                let expected = json!({"method":"get_app_state","args":[input]});
                assert_eq!(*args, expected);
                self.expected_calls.push((method.into(), expected));
                self.stage = 0;
                Ok(reply.value)
            }
            _ => unreachable!(),
        }
    }
}
enum Backend {
    Host(Box<Host>),
    Worker(Box<Worker>),
}
struct Owner {
    backend: Backend,
    trace: Rc<RefCell<Trace>>,
    cell: usize,
}
impl Owner {
    fn new(runtime: RuntimeBackend, supervised: bool) -> Self {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let options = HostOptions {
            runtime,
            ..Default::default()
        };
        let backend = if supervised {
            Backend::Worker(Box::new(Worker::with_options_and_executable(
                options,
                env!("CARGO_BIN_EXE_nanocodex-computer").into(),
            )))
        } else {
            let owned = trace.clone();
            Backend::Host(Box::new(
                Host::with_controlled_dispatch(
                    move |method, args, control| {
                        // Provider-call liveness remains valid during the existing
                        // elicitation/execute suspension; this is not a clock probe.
                        control.validate().expect("live native call");
                        owned.borrow_mut().answer(method, args)
                    },
                    Arc::new(AtomicBool::new(false)),
                    options,
                )
                .unwrap(),
            ))
        };
        let mut owner = Self {
            backend,
            trace,
            cell: 0,
        };
        owner.warm();
        assert_eq!(
            owner.child().is_some(),
            supervised && runtime == RuntimeBackend::V8
        );
        owner
    }
    fn warm(&mut self) {
        let value = self.evaluate("var appStateOwner=cua.computer;({present:typeof appStateOwner.get_app_state==='function'})");
        assert_eq!(value["value"], json!({"present":true}));
    }
    fn child(&self) -> Option<u32> {
        match &self.backend {
            Backend::Host(_) => None,
            Backend::Worker(worker) => worker.runtime_child_pid(),
        }
    }
    fn evaluate(&mut self, code: &str) -> Value {
        self.cell += 1;
        let id = format!("owned-app-state-{}", self.cell);
        self.trace.borrow_mut().cell = id.clone();
        let before = self.trace.borrow().calls.len();
        let result = match &mut self.backend {
            Backend::Host(host) => {
                host.set_request_meta(Some(metadata(&id))).unwrap();
                let result = host.evaluate(code, Duration::from_secs(5)).unwrap();
                assert!(!host.interrupted());
                result
            }
            Backend::Worker(worker) => {
                worker.set_request_meta(Some(metadata(&id))).unwrap();
                let ticket = worker.start(code, Duration::from_secs(5)).unwrap();
                let deadline = Instant::now() + Duration::from_secs(15);
                let result = loop {
                    let left = deadline
                        .checked_duration_since(Instant::now())
                        .expect("bounded owned Worker progress");
                    match worker.event(left).unwrap().expect("owned Worker event") {
                        Event::Call {
                            method,
                            args,
                            control,
                            reply,
                        } => {
                            // Provider-call liveness remains valid during the existing
                            // elicitation/execute suspension; this is not a clock probe.
                            control.validate().expect("live Worker call");
                            let result = self.trace.borrow_mut().answer(&method, &args);
                            reply.send(result).unwrap();
                        }
                        Event::Done {
                            ticket: actual,
                            result,
                        } => {
                            assert_eq!(actual, ticket);
                            break result.unwrap();
                        }
                    }
                };
                assert!(!worker.cancelled());
                assert!(!worker.kernel_reset_pending());
                result
            }
        };
        assert!(result.get("error").is_none(), "{code}\n{result}");
        let trace = self.trace.borrow();
        assert_eq!(
            trace.stage,
            0,
            "owned app protocol incomplete; cell={id}; child={:?}; result={result}; calls={:?}; expected_calls={:?}; pending_replies={}; code={code}",
            self.child(),
            trace.calls,
            trace.expected_calls,
            trace.replies.len()
        );
        assert!(trace.replies.is_empty());
        assert_eq!(trace.calls, trace.expected_calls);
        if trace.calls[before..]
            .iter()
            .any(|(method, _)| method == "sky.execute")
        {
            assert_eq!(
                result["responseMeta"]["codex/toolSurface"],
                json!({"kind":"computerUse","app":{"appId":BUNDLE,"kind":"appId"}})
            );
        }
        result
    }
    fn enqueue(&mut self, requested: &str, disable_diff: Option<bool>, value: Value) {
        self.trace.borrow_mut().replies.push_back(Reply {
            requested: requested.into(),
            disable_diff,
            value,
        });
    }
    fn get(&mut self, requested: &str, value: Value) -> Value {
        self.enqueue(requested, None, value);
        let request = json!({"app":requested});
        let result = self.evaluate(&format!(
            "await (async()=>{{try{{var value=await cua.computer.get_app_state({request});return {{ok:true,value,sameOwner:appStateOwner===cua.computer}};}}catch(error){{return {{ok:false,message:error.message,sameOwner:appStateOwner===cua.computer}};}}}})()"
        ));
        assert_eq!(result["value"]["sameOwner"], true);
        result["value"].clone()
    }
    fn text(&mut self, requested: &str, value: Value) -> String {
        let result = self.get(requested, value);
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["value"]["app"], CANONICAL);
        result["value"]["text"].as_str().unwrap().to_owned()
    }
    fn setup_count(&self) -> usize {
        self.trace.borrow().setup_count
    }
    fn reset_selected_worker(&mut self) {
        match &self.backend {
            Backend::Worker(worker) => worker.reset().unwrap(),
            Backend::Host(_) => panic!("Host independence uses an actual fresh owner"),
        }
        let absent = self.evaluate("({old:typeof appStateOwner})");
        assert_eq!(absent["value"], json!({"old":"undefined"}));
        self.warm();
    }
}

#[test]
fn native_app_state_validation_order_in_host_and_worker() {
    let first = original_first();
    for runtime in RuntimeBackend::available() {
        for supervised in [false, true] {
            let mut owner = Owner::new(runtime, supervised);
            // Simultaneous invalid fields prove order. Expected messages and
            // successful null/empty branches come from the pinned formatter.
            for (mut value, message) in [
                (
                    json!({"appSpecificInstructions":17}),
                    "computer-use service did not return a screenshot",
                ),
                (
                    json!({"skyshot":{"screenshot":{"url":17},"text":null},"appSpecificInstructions":17}),
                    "computer-use service did not return a screenshot URL",
                ),
                (
                    json!({"skyshot":{"screenshot":null,"text":null},"appSpecificInstructions":17}),
                    "computer-use service did not return screenshot text",
                ),
                (
                    json!({"skyshot":{"text":TREE},"appSpecificInstructions":17}),
                    "computer-use service returned invalid app-specific instructions",
                ),
            ] {
                value["app"] = json!({"bundleIdentifier":BUNDLE});
                let result = owner.get("Owned", value);
                assert_eq!(
                    result,
                    json!({"ok":false,"message":message,"sameOwner":true})
                );
            }
            // The same returned bundle is present even in the invalid replies.
            // No earlier validation failure consumed that key.
            assert_eq!(owner.text("Owned", ordinary_state()), first);
            for screenshot in [
                Value::Null,
                json!({}),
                json!({"url":null}),
                json!({"url":""}),
            ] {
                let value = json!({"skyshot":{"text":"","screenshot":screenshot}});
                let result = owner.get("Owned", value);
                assert_eq!(
                    result["value"],
                    json!({"app":CANONICAL,"text":"","screenshot":null})
                );
            }
            let absent = owner.get("Owned", json!({"skyshot":{"text":""}}));
            assert_eq!(
                absent["value"],
                json!({"app":CANONICAL,"text":"","screenshot":null})
            );
            let result = owner.get(
                "Owned",
                json!({"skyshot":{"text":TREE,"screenshot":{"url":"opaque:owned"}}}),
            );
            assert_eq!(result["value"]["screenshot"], json!({"url":"opaque:owned"}));
            assert_eq!(owner.setup_count(), 1);
        }
    }
}

#[test]
fn native_numbers_instructions_never_insert_suppressed_cache_key() {
    let first = original_first();
    for runtime in RuntimeBackend::available() {
        for supervised in [false, true] {
            let mut owner = Owner::new(runtime, supervised);
            let bundle = json!({"bundleIdentifier":"com.apple.iWork.Numbers"});
            let invalid = owner.get("Owned", state(bundle.clone(), json!(17)));
            assert_eq!(
                invalid["message"],
                "computer-use service returned invalid app-specific instructions"
            );
            assert_eq!(owner.text("Owned", state(bundle, json!(GUIDANCE))), TREE);
            // The string is a cache key but not the suppressed bundle object.
            // Its first prefix proves that suppression did not insert the key.
            let fallback = state(json!("com.apple.iWork.Numbers"), json!(GUIDANCE));
            assert_eq!(owner.text("Owned", fallback.clone()), first);
            assert_eq!(owner.text("Owned", fallback), TREE);
            assert_eq!(owner.setup_count(), 1);
        }
    }
}

#[test]
fn native_app_instruction_prefix_and_key_precedence_survive_cells_and_reset() {
    let first = original_first();
    for runtime in RuntimeBackend::available() {
        for supervised in [false, true] {
            let mut owner = Owner::new(runtime, supervised);
            let child = owner.child();
            assert_eq!(
                owner.text(
                    "Alias A",
                    state(json!({"bundleIdentifier":BUNDLE}), json!(""))
                ),
                TREE
            );
            assert_eq!(
                owner.text(
                    "Alias A",
                    state(json!({"bundleIdentifier":BUNDLE}), Value::Null)
                ),
                TREE
            );
            assert_eq!(owner.text("Alias A", ordinary_state()), first);
            assert_eq!(owner.text("Alias B", ordinary_state()), TREE);
            assert_eq!(
                owner.text("Alias A", state(json!("returned.app"), json!(GUIDANCE))),
                first
            );
            assert_eq!(
                owner.text("Alias B", state(json!("returned.app"), json!(GUIDANCE))),
                TREE
            );
            // With no returned bundle or app string, policy's canonical request
            // app is the final fallback; raw submitted aliases cannot split it.
            assert_eq!(
                owner.text("Alias A", state(json!({}), json!(GUIDANCE))),
                first
            );
            assert_eq!(
                owner.text("Alias B", state(json!(""), json!(GUIDANCE))),
                TREE
            );
            assert_eq!(
                owner.text(
                    "Alias B",
                    state(json!({"bundleIdentifier":17}), json!(GUIDANCE))
                ),
                TREE
            );
            assert_eq!(
                owner.text(
                    "Alias B",
                    state(json!({"bundleIdentifier":""}), json!(GUIDANCE))
                ),
                TREE
            );
            assert_eq!(
                owner.text(
                    "Owned",
                    state(json!({"bundleIdentifier":"whitespace"}), json!(" \t"))
                ),
                "<app_specific_instructions>\n \t\n</app_specific_instructions>\nfixture tree"
            );
            assert_eq!(owner.child(), child);
            assert_eq!(owner.setup_count(), 1);
            let mut independent = Owner::new(runtime, supervised);
            assert_eq!(independent.text("Owned", ordinary_state()), first);
            assert_eq!(owner.text("Owned", ordinary_state()), TREE);
            if supervised {
                owner.reset_selected_worker();
                assert_eq!(owner.text("Owned", ordinary_state()), first);
                assert_eq!(owner.setup_count(), 2);
                assert_eq!(independent.text("Owned", ordinary_state()), TREE);
                assert_eq!(independent.setup_count(), 1);
            }
        }
    }
}

#[test]
fn native_app_instruction_cache_survives_many_prior_outputs() {
    let first = original_first();
    for runtime in RuntimeBackend::available() {
        for supervised in [false, true] {
            let mut owner = Owner::new(runtime, supervised);
            let child = owner.child();
            owner.enqueue("Owned", Some(true), ordinary_state());
            let result = owner.evaluate(
                r#"
for(let i=0;i<256;i++)nodeRepl.write('', 'owned-app-state-'+i);
await cua.getApp('Owned');
({sameOwner:appStateOwner===cua.computer})
"#,
            );
            assert_eq!(result["value"], json!({"sameOwner":true}));
            let outputs = result["outputs"].as_array().unwrap();
            assert!(outputs.len() > 256);
            for (i, output) in outputs.iter().take(256).enumerate() {
                assert_eq!(output["channel"], format!("owned-app-state-{i}"));
                assert_eq!(output["value"], "");
            }
            assert!(
                outputs
                    .iter()
                    .skip(256)
                    .any(|output| output["channel"] == "cua.state")
            );
            assert_eq!(owner.child(), child);
            // A later cell retains the installed cache.
            owner.enqueue("Owned", Some(true), ordinary_state());
            let retry = owner
                .evaluate("await cua.getApp('Owned');({sameOwner:appStateOwner===cua.computer})");
            assert_eq!(retry["value"], json!({"sameOwner":true}));
            let states: Vec<_> = retry["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["channel"] == "cua.state")
                .collect();
            assert_eq!(states.len(), 1);
            assert_eq!(states[0]["value"], TREE);
            assert_eq!(owner.setup_count(), 1);
            assert_eq!(owner.child(), child);
            let mut independent = Owner::new(runtime, supervised);
            assert_eq!(independent.text("Owned", ordinary_state()), first);
        }
    }
}
