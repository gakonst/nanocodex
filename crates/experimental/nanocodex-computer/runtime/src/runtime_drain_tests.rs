use super::*;
use std::thread;
fn fixture(runtime: RuntimeBackend) -> (Host, Rc<Cell<usize>>) {
    let proofs = Rc::new(Cell::new(0));
    let observed = proofs.clone();
    let mut polls = 0;
    let host = Host::with_controlled_dispatch(
        move |method, args, control| match method {
            "sky.setup" => Ok(json!({"target":"mac"})),
            "browser.info" => Ok(json!({"id":"owned","type":"cdp","_skyreOriginApproval":true})),
            "browser.origin_operation_start" if args["method"] != "navigate" => {
                Ok(json!({"pending":false,"value":{"id":"t"}}))
            }
            "browser.origin_operation_start" => {
                polls = 0;
                control.bind_continuation("owned-navigation".into())?;
                Ok(json!({"pending":true,"id":"owned-navigation"}))
            }
            "browser.origin_operation_poll" => {
                polls += 1;
                control.bind_continuation("owned-navigation".into())?;
                let guard = if let Some(proof) = control.drain_proof() {
                    assert!(proof.continuations.iter().all(|v| v == "owned-navigation"));
                    observed.set(observed.get() + 1);
                    Some(control.suspend()?)
                } else {
                    None
                };
                thread::sleep(Duration::from_millis(10));
                if let Some(guard) = guard {
                    guard.resume()?;
                }
                Ok(if polls < 35 {
                    json!({"pending":true,"id":"owned-navigation"})
                } else {
                    json!({"pending":false,"value":null})
                })
            }
            _ => Ok(Value::Null),
        },
        Arc::new(AtomicBool::new(false)),
        HostOptions {
            runtime,
            ..Default::default()
        },
    )
    .unwrap();
    (host, proofs)
}
const SETUP: &str = "var b=await agent.browsers.get('owned');var t=await b.tabs.get('t');";
#[test]
fn direct_native_goto_drain_gets_bounded_call_credit_in_both_hosts() {
    for runtime in RuntimeBackend::available() {
        let (mut host, proofs) = fixture(runtime);
        let warm = host.evaluate(SETUP, Duration::from_secs(5)).unwrap();
        assert!(warm.get("error").is_none(), "{runtime:?}: {warm}");
        let result = host
            .evaluate(
                "t.goto('https://owned.example'); 42",
                Duration::from_millis(200),
            )
            .unwrap();
        assert_eq!(result["value"], 42, "{runtime:?}: {result}");
        assert!(proofs.get() > 20, "{runtime:?}: {}", proofs.get());
        let next = host.evaluate("43", Duration::from_millis(200)).unwrap();
        assert_eq!(next["value"], 43);
    }
}
#[test]
fn unrelated_root_work_and_manual_drain_do_not_prove_human_wait() {
    for runtime in RuntimeBackend::available() {
        for code in [
            "await t.goto('https://owned.example')",
            "t.goto('https://owned.example'); await new Promise(()=>{})",
            "t.goto('https://owned.example'); await __skyreDrainOutput()",
            "t.goto('https://owned.example'); nodeRepl.emitImage(new Promise(()=>{}));",
            "nodeRepl.emitImage(new Promise(()=>{})); t.goto('https://owned.example');",
            "await Promise.race([t.goto('https://owned.example'),new Promise(()=>{})])",
            "t.goto('https://owned.example'); await Promise.all([new Promise(()=>{}), Promise.resolve(1)])",
            "t.goto('https://owned.example'); await {then(){}}",
            "t.goto('https://owned.example'); while(true){}",
            "t.goto('https://owned.example'); queueMicrotask(function work(){queueMicrotask(work)}); await new Promise(()=>{})",
        ] {
            let (mut host, proofs) = fixture(runtime);
            host.evaluate(SETUP, Duration::from_secs(5)).unwrap();
            let result = host.evaluate(code, Duration::from_millis(120)).unwrap();
            assert!(
                result.get("error").is_some(),
                "{runtime:?} {code}: {result}"
            );
            assert_eq!(proofs.get(), 0, "{runtime:?} {code}: {result}");
        }
    }
}

#[test]
fn future_timer_vetoes_drain_credit_and_private_tags_are_unavailable() {
    for runtime in RuntimeBackend::available() {
        let (mut host, proofs) = fixture(runtime);
        host.evaluate(SETUP, Duration::from_secs(5)).unwrap();
        let hidden=host.evaluate("[typeof __skyre_operation_register,typeof __skyre_operation_finish,typeof __skyre_operation_derive]",Duration::from_secs(1)).unwrap();
        assert_eq!(
            hidden["value"],
            json!(["undefined", "undefined", "undefined"])
        );
        let began = Instant::now();
        let result = host
            .evaluate(
                "t.goto('https://owned.example');setTimeout(()=>{while(true){}},35);42",
                Duration::from_millis(120),
            )
            .unwrap();
        assert!(result.get("error").is_some(), "{runtime:?}: {result}");
        assert_eq!(
            proofs.get(),
            0,
            "{runtime:?}: future runnable work remains charged"
        );
        assert!(began.elapsed() < Duration::from_millis(500));
    }
}

#[test]
fn pending_public_raw_read_remains_nonhuman_between_owned_polls() {
    // Controlled provider callbacks exercise the real public facade and native
    // registries; no Promise tag or drain primitive is exposed to submitted JS.
    for runtime in RuntimeBackend::available() {
        let mixed_polls = Rc::new(Cell::new(0));
        let mixed = mixed_polls.clone();
        let invalid_proofs = Rc::new(Cell::new(0));
        let later_proofs = Rc::new(Cell::new(0));
        let invalid = invalid_proofs.clone();
        let later = later_proofs.clone();
        let mut raw_pending = false;
        let mut raw_polls = 0;
        let mut human_polls = 0;
        let raw_id = format!("raw-events-{}", "a".repeat(64));
        let raw_id_for_calls = raw_id.clone();
        let mut host = Host::with_controlled_dispatch(
            move |method, args, control| Ok(match method {
                "sky.setup" => json!({"target":"mac"}),
                "browser.info" => json!({"id":"owned","type":"cdp","_skyreOriginApproval":true,"capabilities":{"browser":[],"tab":[{"id":"cdp"}]}}),
                "browser.origin_operation_start" if args["method"] == "cdp_events" => {
                    assert!(args["args"].get("__skyreRawWait").is_none());
                    raw_pending = true;
                    json!({"pending":false,"value":{"__skyreRawWait":{"id":raw_id_for_calls}}})
                }
                "browser.origin_operation_start" if args["method"] == "navigate" => {
                    control.bind_continuation("owned-human".into())?;
                    json!({"pending":true,"id":"owned-human"})
                }
                "browser.origin_operation_start" => json!({"pending":false,"value":{"id":"t"}}),
                "browser.cdp_events" => {
                    assert_eq!(args["__skyreRawWait"]["id"], raw_id_for_calls);
                    assert_eq!(args["__skyreRawWait"]["op"], "poll");
                    raw_polls += 1;
                    if raw_polls < 12 {
                        json!({"__skyreRawWait":{"id":raw_id_for_calls}})
                    } else {
                        raw_pending = false;
                        json!({"cursor":0,"events":[],"hasMore":false,"truncated":false})
                    }
                }
                "browser.origin_operation_poll" => {
                    control.bind_continuation("owned-human".into())?;
                    human_polls += 1;
                    if raw_pending { mixed.set(mixed.get() + 1); }
                    if let Some(proof) = control.drain_proof() {
                        assert!(proof.continuations.iter().all(|id| id == "owned-human"));
                        if raw_pending { invalid.set(invalid.get() + 1); }
                        else { later.set(later.get() + 1); }
                    }
                    if human_polls < 24 {
                        json!({"pending":true,"id":"owned-human"})
                    } else {
                        json!({"pending":false,"value":null})
                    }
                }
                _ => Value::Null,
            }),
            Arc::new(AtomicBool::new(false)),
            HostOptions { runtime, ..Default::default() },
        ).unwrap();
        let setup = host.evaluate("var b=await cua.getBrowser({id:'owned'});var t=await b.tabs.get('t');var cdp=await t.capabilities.get('cdp');",Duration::from_secs(5)).unwrap();
        assert!(setup.get("error").is_none(), "{runtime:?}: {setup}");
        let result = host
            .evaluate(
                "cdp.readEvents({timeoutMs:500});t.goto('https://owned.example');42",
                Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(result["value"], 42, "{runtime:?}: {result}");
        assert!(
            mixed_polls.get() >= 4,
            "{runtime:?}: mixed pending phase was not exercised"
        );
        assert_eq!(
            invalid_proofs.get(),
            0,
            "{runtime:?}: pending raw read must veto every human proof"
        );
        assert!(
            later_proofs.get() > 0,
            "{runtime:?}: the fixture must exercise human proof after the raw obligation finishes"
        );
        assert_eq!(
            host.evaluate("43", Duration::from_secs(1)).unwrap()["value"],
            43
        );
    }
}
