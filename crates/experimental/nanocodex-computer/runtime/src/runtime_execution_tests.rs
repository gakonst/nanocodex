//! Native liveness observations only; these tests do not authorize browser work.
use super::*;

#[test]
fn native_quickjs_execution_validity_reads_current_clock_without_mutation() {
    let deadline = Mutex::new(Instant::now() + Duration::from_secs(30));
    let suspended = Mutex::new((0, None));
    let cancelled = AtomicBool::new(false);
    let original_deadline = *deadline.lock().unwrap();
    for _ in 0..4 {
        validate_quickjs_execution(&deadline, &suspended, &cancelled).unwrap();
    }
    assert_eq!(*deadline.lock().unwrap(), original_deadline);
    assert_eq!(*suspended.lock().unwrap(), (0, None));
    *deadline.lock().unwrap() = Instant::now();
    assert!(validate_quickjs_execution(&deadline, &suspended, &cancelled).is_err());
    *deadline.lock().unwrap() = original_deadline;
    validate_quickjs_execution(&deadline, &suspended, &cancelled).unwrap();
    *suspended.lock().unwrap() = (1, Some(Instant::now()));
    assert!(validate_quickjs_execution(&deadline, &suspended, &cancelled).is_err());
    *suspended.lock().unwrap() = (0, None);
    validate_quickjs_execution(&deadline, &suspended, &cancelled).unwrap();
    cancelled.store(true, Ordering::Release);
    assert!(validate_quickjs_execution(&deadline, &suspended, &cancelled).is_err());
    assert_eq!(*deadline.lock().unwrap(), original_deadline);
    assert_eq!(*suspended.lock().unwrap(), (0, None));
}

#[test]
fn native_runtime_execution_validity_is_installed_and_call_scoped() {
    for runtime in RuntimeBackend::available() {
        let captured = Rc::new(RefCell::new(Vec::<ExecutionValidity>::new()));
        let observed = captured.clone();
        let mut host = Host::with_controlled_dispatch(
            move |method, args, control| {
                let validity = control
                    .execution_validity()
                    .expect("runtime validator installed");
                validity.validate()?;
                let suspension = control.suspend()?;
                assert!(validity.validate().is_err());
                suspension.resume()?;
                validity.validate()?;
                observed.borrow_mut().push(validity);
                match method {
                    "sky.setup" => Ok(json!({"target":"mac"})),
                    "browser.info" => Ok(json!({"id":args["browser"],"type":"cdp"})),
                    _ => panic!("Unexpected owned fixture method {method}"),
                }
            },
            Arc::new(AtomicBool::new(false)),
            HostOptions {
                runtime,
                ..Default::default()
            },
        )
        .unwrap();
        let result = host
            .evaluate(
                "await agent.browsers.get('owned'); 42",
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(result["value"], 42, "{runtime:?}: {result}");
        let first_count = captured.borrow().len();
        assert!(first_count >= 2);
        assert!(
            captured
                .borrow()
                .iter()
                .all(|validity| validity.validate().is_err())
        );
        let result = host
            .evaluate(
                "await agent.browsers.get('owned-next'); 43",
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(result["value"], 43, "{runtime:?}: {result}");
        assert!(captured.borrow().len() > first_count);
        assert!(
            captured
                .borrow()
                .iter()
                .all(|validity| validity.validate().is_err())
        );
    }
}

#[test]
fn native_runtime_execution_validity_observes_native_cancel_before_dispatch_returns() {
    for runtime in RuntimeBackend::available() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = cancelled.clone();
        let captured = Rc::new(RefCell::new(None::<ExecutionValidity>));
        let observed = captured.clone();
        let mut host = Host::with_controlled_dispatch(
            move |method, _, control| match method {
                "sky.setup" => Ok(json!({"target":"mac"})),
                "browser.info" => {
                    let validity = control
                        .execution_validity()
                        .expect("runtime validator installed");
                    validity.validate()?;
                    cancel.store(true, Ordering::Release);
                    assert!(validity.validate().is_err());
                    *observed.borrow_mut() = Some(validity);
                    Err(Error::new(-32800, "Owned native cancellation"))
                }
                _ => panic!("Unexpected owned fixture method {method}"),
            },
            cancelled,
            HostOptions {
                runtime,
                ..Default::default()
            },
        )
        .unwrap();
        let result = host
            .evaluate("await agent.browsers.get('owned')", Duration::from_secs(5))
            .unwrap();
        assert!(result.get("error").is_some(), "{runtime:?}: {result}");
        let old = captured
            .borrow_mut()
            .take()
            .expect("observed provider callback");
        assert!(old.validate().is_err());
        host.clear_cancel();
        let result = host.evaluate("42", Duration::from_secs(5)).unwrap();
        assert_eq!(result["value"], 42, "{runtime:?}: {result}");
        assert!(old.validate().is_err());
    }
}

#[test]
fn native_async_rpc_second_reply_settles_before_first_is_released() {
    for runtime in RuntimeBackend::available() {
        let released = Rc::new(Cell::new(false));
        let release = released.clone();
        let admitted = Rc::new(RefCell::new(Vec::new()));
        let calls = admitted.clone();
        let mut host = Host::with_async_dispatch(
            move |method, args, control| match method {
                "sky.setup" => Ok(ProviderResponse::ready(Ok(json!({"target":"mac"})))),
                "browser.info" => {
                    let id = args["browser"].as_str().unwrap().to_owned();
                    calls.borrow_mut().push(id.clone());
                    match id.as_str() {
                        "held" => {
                            let release = release.clone();
                            Ok(ProviderResponse::pending(move || {
                                control.execution_validity().unwrap().validate().unwrap();
                                release.get().then(|| Ok(json!({"id":"held","type":"cdp"})))
                            }))
                        }
                        "fast" => {
                            assert!(!release.get());
                            Ok(ProviderResponse::ready(Ok(
                                json!({"id":"fast","type":"cdp"}),
                            )))
                        }
                        "release" => {
                            assert!(!release.replace(true));
                            Ok(ProviderResponse::ready(Ok(
                                json!({"id":"release","type":"cdp"}),
                            )))
                        }
                        _ => panic!("Unexpected browser {id}"),
                    }
                }
                _ => panic!("Unexpected fixture method {method}"),
            },
            Arc::new(AtomicBool::new(false)),
            HostOptions {
                runtime,
                ..Default::default()
            },
        )
        .unwrap();
        let result = host.evaluate(
            "const order=[]; await Promise.all([agent.browsers.get('held').then(()=>order.push('held')), agent.browsers.get('fast').then(async()=>{order.push('fast');await agent.browsers.get('release')})]); order",
            Duration::from_secs(5),
        ).unwrap();
        assert_eq!(
            result["value"],
            json!(["fast", "held"]),
            "{runtime:?}: {result}"
        );
        assert_eq!(*admitted.borrow(), ["held", "fast", "release"]);
        assert!(released.get());
    }
}

#[test]
fn native_async_rpc_cancellation_closes_pending_control_and_recovers() {
    for runtime in RuntimeBackend::available() {
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let captured = Rc::new(RefCell::new(None));
        let capture = captured.clone();
        let mut host = Host::with_async_dispatch(
            move |method, _, control| {
                if method == "sky.setup" {
                    return Ok(ProviderResponse::ready(Ok(json!({"target":"mac"}))));
                }
                *capture.borrow_mut() = Some(control.clone());
                let cancelled = cancelled.clone();
                Ok(ProviderResponse::pending(move || {
                    cancelled.store(true, Ordering::Release);
                    None
                }))
            },
            cancel.clone(),
            HostOptions {
                runtime,
                ..Default::default()
            },
        )
        .unwrap();
        let result = host
            .evaluate("await agent.browsers.get('held')", Duration::from_secs(5))
            .unwrap();
        assert!(result.get("error").is_some(), "{runtime:?}: {result}");
        assert!(!captured.borrow().as_ref().unwrap().is_active());
        drop(host);
    }
}

#[test]
fn native_approved_app_execution_is_actionable_and_charged() {
    for runtime in RuntimeBackend::available() {
        let slow = Rc::new(Cell::new(false));
        let delay = slow.clone();
        let mut host = Host::with_controlled_dispatch(
            move |method, _, control| match method {
                "sky.setup" => Ok(json!({"target":"mac","methods":["get_app_state"]})),
                "sky.app_policy" => {
                    Ok(json!({"decision":"allowed","allowPersistentApproval":false,
                    "target":{"bundleIdentifier":"owned.fixture","displayName":"Owned fixture",
                    "appPath":"/owned/Fixture.app","risk":"low"}}))
                }
                "host.elicitation" => {
                    // Approval alone retains the trusted timeout exclusion.
                    assert!(control.execution_validity().unwrap().validate().is_err());
                    Ok(json!({"action":"accept"}))
                }
                "sky.execute" => {
                    let validity = control.execution_validity().unwrap();
                    validity
                        .validate()
                        .expect("approved native operation must be actionable");
                    if delay.get() {
                        std::thread::sleep(Duration::from_millis(100));
                        assert!(validity.validate().is_err(), "native time remains charged");
                    }
                    Ok(json!({"skyshot":{"text":"owned state","screenshot":null}}))
                }
                _ => panic!("unexpected fixture method {method}"),
            },
            Arc::new(AtomicBool::new(false)),
            HostOptions {
                runtime,
                ..Default::default()
            },
        )
        .unwrap();
        let code = "await cua.computer.get_app_state({app:'owned.fixture',screenshot:false});42";
        let result = host.evaluate(code, Duration::from_secs(5)).unwrap();
        assert_eq!(result["value"], 42, "{runtime:?}: {result}");
        slow.set(true);
        let result = host.evaluate(code, Duration::from_millis(50)).unwrap();
        assert!(result.get("error").is_some(), "{runtime:?}: {result}");
    }
}

#[test]
fn native_execution_probe_survives_sibling_approval_but_not_call_end() {
    for runtime in RuntimeBackend::available() {
        let held = Rc::new(RefCell::new(None::<ProviderControl>));
        let captured = held.clone();
        let released = Rc::new(Cell::new(false));
        let release = released.clone();
        let mut host = Host::with_async_dispatch(
            move |method, _, control| match method {
                "sky.setup" => Ok(ProviderResponse::ready(Ok(json!({"target":"mac","methods":["get_app_state"]})))),
                "native-held.rpc" => {
                    *captured.borrow_mut() = Some(control.clone());
                    let release = release.clone();
                    Ok(ProviderResponse::pending(move || {
                        if !release.get() { return None; }
                        Some(control.native_execution_validity().unwrap().validate().map(|()| json!(42)))
                    }))
                }
                "sky.app_policy" => Ok(ProviderResponse::ready(Ok(json!({"decision":"allowed","allowPersistentApproval":false,"target":{"bundleIdentifier":"owned.fixture","displayName":"Owned fixture","appPath":"/owned/Fixture.app","risk":"low"}})))),
                "host.elicitation" => {
                    let native = captured.borrow().as_ref().unwrap().clone();
                    assert!(native.execution_validity().unwrap().validate().is_err(), "strict browser probe must refuse suspended clock");
                    native.native_execution_validity().unwrap().validate().unwrap();
                    release.set(true);
                    Ok(ProviderResponse::ready(Ok(json!({"action":"accept"}))))
                }
                "sky.execute" => Ok(ProviderResponse::ready(Ok(json!({"skyshot":{"text":"owned state","screenshot":null}})))),
                _ => panic!("Unexpected fixture method {method}"),
            },
            Arc::new(AtomicBool::new(false)),
            HostOptions { runtime, ..Default::default() },
        ).unwrap();
        let result = host.evaluate(
            "await Promise.all([nodeRepl.rpc('native-held',{}), cua.computer.get_app_state({app:'owned.fixture',screenshot:false})]);42",
            Duration::from_secs(5),
        ).unwrap();
        assert_eq!(result["value"], json!(42), "{runtime:?}: {result}");
        assert!(!held.borrow().as_ref().unwrap().is_active());
    }
}

#[test]
fn native_completion_during_approval_does_not_run_js_with_a_paused_clock() {
    for runtime in RuntimeBackend::available() {
        let approving = Rc::new(Cell::new(false));
        let native_done = Rc::new(Cell::new(false));
        let released = Rc::new(Cell::new(false));
        let completed = released.clone();
        let mut host = Host::with_async_dispatch(move |method, _, control| {
            match method {
                "sky.setup" => Ok(ProviderResponse::ready(Ok(json!({"target":"mac","methods":["get_app_state"]})))),
                "owned-native.rpc" => {
                    let approving = approving.clone();
                    let done = native_done.clone();
                    Ok(ProviderResponse::pending(move || {
                        if !approving.get() { return None; }
                        control.native_execution_validity().unwrap().validate().unwrap();
                        done.set(true);
                        Some(Ok(json!(1)))
                    }))
                }
                "sky.app_policy" => Ok(ProviderResponse::ready(Ok(json!({"decision":"allowed","allowPersistentApproval":false,"target":{"bundleIdentifier":"owned.fixture","displayName":"Owned fixture","appPath":"/owned/Fixture.app","risk":"low"}})))),
                "host.elicitation" => {
                    approving.set(true);
                    let done = native_done.clone();
                    let released = released.clone();
                    let mut polls = 0;
                    Ok(ProviderResponse::pending(move || {
                        if !done.get() { return None; }
                        polls += 1;
                        if polls < 20 { return None; }
                        released.set(true);
                        Some(Ok(json!({"action":"accept"})))
                    }))
                }
                "owned-after.rpc" => {
                    assert!(released.get(), "JavaScript continuation ran under approval timeout credit");
                    control.execution_validity().unwrap().validate().unwrap();
                    Ok(ProviderResponse::ready(Ok(json!(2))))
                }
                "sky.execute" => Ok(ProviderResponse::ready(Ok(json!({"skyshot":{"text":"owned state","screenshot":null}})))),
                _ => panic!("unexpected method {method}"),
            }
        }, Arc::new(AtomicBool::new(false)), HostOptions { runtime, ..Default::default() }).unwrap();
        let result = host.evaluate("await Promise.all([nodeRepl.rpc('owned-native',{}).then(()=>nodeRepl.rpc('owned-after',{})),cua.computer.get_app_state({app:'owned.fixture',screenshot:false})]);42",Duration::from_secs(5)).unwrap();
        assert_eq!(result["value"], json!(42), "{runtime:?}: {result}");
        assert!(completed.get());
    }
}
