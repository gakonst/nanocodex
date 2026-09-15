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
