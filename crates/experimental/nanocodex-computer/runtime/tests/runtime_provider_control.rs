//! Original kernel excludes trusted human approval time from its deadline,
//! while submitted-code duration includes the wait. Synthetic services only.
use serde_json::{Value, json};
use skyre::{
    Error,
    runtime::{Host, HostOptions, ProviderControl, RuntimeBackend},
    worker::{Event, Worker},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
fn backends() -> Vec<RuntimeBackend> {
    RuntimeBackend::available()
}
fn worker(runtime: RuntimeBackend) -> Worker {
    let mut worker = Worker::with_options_and_executable(
        HostOptions {
            runtime,
            ..Default::default()
        },
        env!("CARGO_BIN_EXE_nanocodex-computer").into(),
    );
    worker
        .start("let initialized=1", Duration::from_secs(5))
        .unwrap();
    loop {
        match worker.event(Duration::from_secs(5)).unwrap().unwrap() {
            Event::Call { method, reply, .. } => {
                assert_eq!(method, "sky.setup");
                reply.send(Ok(json!({"target":"mac"}))).unwrap();
            }
            Event::Done { result, .. } => {
                assert!(result.unwrap().get("error").is_none());
                break;
            }
        }
    }
    worker
}
fn done(worker: &mut Worker) -> Value {
    match worker
        .event(Duration::from_secs(5))
        .unwrap()
        .expect("bounded worker progress")
    {
        Event::Call { method, .. } => panic!("unexpected call {method}"),
        Event::Done { result, .. } => result.unwrap(),
    }
}

fn call(
    worker: &mut Worker,
) -> (
    ProviderControl,
    std::sync::mpsc::SyncSender<skyre::Result<Value>>,
) {
    match worker
        .event(Duration::from_secs(5))
        .unwrap()
        .expect("provider call")
    {
        Event::Call {
            method,
            control,
            reply,
            ..
        } => {
            assert_eq!(method, "owned.rpc");
            (control, reply)
        }
        Event::Done { result, .. } => panic!("early completion {result:?}"),
    }
}
#[test]
fn trusted_approval_excludes_wait_from_deadline_but_preserves_duration_and_bindings() {
    for runtime in backends() {
        let mut worker = worker(runtime);
        worker
            .start(
                "let approvalMarker=7;await nodeRepl.rpc('owned',{})",
                Duration::from_millis(200),
            )
            .unwrap();
        let (control, reply) = call(&mut worker);
        let outer = control.suspend().unwrap();
        let inner = control.suspend().unwrap();
        thread::sleep(Duration::from_millis(600));
        assert!(inner.resume().unwrap() >= Duration::from_millis(600));
        assert!(outer.resume().unwrap() >= Duration::from_millis(600));
        reply.send(Ok(json!(42))).unwrap();
        let result = done(&mut worker);
        assert!(result.get("error").is_none(), "{runtime:?}: {result}");
        assert_eq!(result["value"], 42);
        assert!(
            result["executionDurationMs"].as_u64().unwrap() >= 600,
            "{result}"
        );
        assert!(!worker.take_kernel_reset());
        assert!(
            control.suspend().is_err(),
            "completed call capability must expire"
        );
        worker
            .start("approvalMarker", Duration::from_secs(1))
            .unwrap();
        assert_eq!(done(&mut worker)["value"], 7);
    }
}
#[test]
fn ordinary_provider_delay_still_times_out_and_expired_control_cannot_revive_it() {
    for runtime in backends() {
        let mut worker = worker(runtime);
        worker
            .start("await nodeRepl.rpc('owned',{})", Duration::from_millis(100))
            .unwrap();
        let (control, reply) = call(&mut worker);
        thread::sleep(Duration::from_millis(200));
        assert!(
            control.suspend().is_err(),
            "{runtime:?}: expired clock revived"
        );
        let _ = reply.send(Ok(json!(42)));
        let result = done(&mut worker);
        assert!(result.get("error").is_some(), "{runtime:?}: {result}");
        assert!(worker.take_kernel_reset());
        worker
            .start("typeof initialized", Duration::from_secs(5))
            .unwrap();
        loop {
            match worker.event(Duration::from_secs(5)).unwrap().unwrap() {
                Event::Call { method, reply, .. } => {
                    assert_eq!(method, "sky.setup");
                    reply.send(Ok(json!({"target":"mac"}))).unwrap();
                }
                Event::Done { result, .. } => {
                    assert_eq!(result.unwrap()["value"], "undefined");
                    break;
                }
            }
        }
    }
}
#[test]
fn retained_guard_is_revoked_at_reply_and_cannot_pause_a_following_cell() {
    for runtime in backends() {
        let mut worker = worker(runtime);
        worker
            .start("await nodeRepl.rpc('owned',{})", Duration::from_millis(200))
            .unwrap();
        let (control, reply) = call(&mut worker);
        let guard = control.suspend().unwrap();
        thread::sleep(Duration::from_millis(250));
        reply.send(Ok(json!(42))).unwrap();
        assert_eq!(done(&mut worker)["value"], 42);
        worker
            .start("while(true){}", Duration::from_millis(50))
            .unwrap();
        assert!(control.suspend().is_err());
        assert!(guard.resume().is_err());
        let began = Instant::now();
        assert!(done(&mut worker).get("error").is_some());
        assert!(began.elapsed() < Duration::from_secs(2));
    }
}
#[test]
fn dropping_approval_guard_resumes_on_provider_error_and_cancel_still_terminates() {
    for runtime in backends() {
        let mut worker = worker(runtime);
        worker
            .start(
                "let caught;try{await nodeRepl.rpc('owned',{})}catch(e){caught=e.message};caught",
                Duration::from_millis(200),
            )
            .unwrap();
        let (control, reply) = call(&mut worker);
        {
            let _guard = control.suspend().unwrap();
            thread::sleep(Duration::from_millis(250));
        }
        reply
            .send(Err(Error::action("owned provider failure")))
            .unwrap();
        assert_eq!(done(&mut worker)["value"], "owned provider failure");
        worker
            .start("await nodeRepl.rpc('owned',{})", Duration::from_millis(200))
            .unwrap();
        let (control, _reply) = call(&mut worker);
        let guard = control.suspend().unwrap();
        worker.cancel();
        let began = Instant::now();
        assert!(done(&mut worker).get("error").is_some());
        drop(guard);
        assert!(began.elapsed() < Duration::from_secs(3));
        assert!(control.suspend().is_err());
    }
}
#[test]
fn direct_host_control_is_thread_safe_and_stale_after_dispatch() {
    for runtime in backends() {
        let retained = Arc::new(Mutex::new(None));
        let captured = retained.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = cancelled.clone();
        let mut host = Host::with_controlled_dispatch(
            move |method, _, control| {
                if method == "sky.setup" {
                    return Ok(json!({"target":"mac"}));
                }
                let provider = control.clone();
                thread::spawn(move || {
                    let _guard = provider.suspend().unwrap();
                    thread::sleep(Duration::from_millis(300));
                })
                .join()
                .unwrap();
                *captured.lock().unwrap() = Some(control);
                assert!(!cancellation.load(Ordering::Acquire));
                Ok(json!(42))
            },
            cancelled,
            HostOptions {
                runtime,
                ..Default::default()
            },
        )
        .unwrap();
        host.evaluate("0", Duration::from_secs(5)).unwrap();
        let result = host
            .evaluate("await nodeRepl.rpc('owned',{})", Duration::from_millis(100))
            .unwrap();
        assert_eq!(result["value"], 42, "{runtime:?}: {result}");
        assert!(
            retained
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .suspend()
                .is_err()
        );
    }
}

#[cfg(all(unix, feature = "v8"))]
#[test]
fn stopped_child_during_trusted_suspension_is_bounded_and_recoverable() {
    let mut worker = worker(RuntimeBackend::V8);
    worker
        .start("await nodeRepl.rpc('owned',{})", Duration::from_millis(200))
        .unwrap();
    let (control, reply) = call(&mut worker);
    let guard = control.suspend().unwrap();
    let pid = worker.runtime_child_pid().unwrap();
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGSTOP) }, 0);
    let began = Instant::now();
    assert!(
        guard.resume().is_err(),
        "stopped child cannot acknowledge resume"
    );
    let _ = reply.send(Ok(json!(42)));
    match worker.event(Duration::from_secs(5)).unwrap().unwrap() {
        Event::Done { result, .. } => {
            assert!(result.is_err() || result.unwrap().get("error").is_some())
        }
        Event::Call { .. } => panic!("no further provider calls"),
    }
    assert!(began.elapsed() < Duration::from_secs(4));
    assert!(worker.take_kernel_reset());
    assert!(control.suspend().is_err());
    worker.start("42", Duration::from_secs(5)).unwrap();
    loop {
        match worker.event(Duration::from_secs(5)).unwrap().unwrap() {
            Event::Call { method, reply, .. } => {
                assert_eq!(method, "sky.setup");
                reply.send(Ok(json!({"target":"mac"}))).unwrap();
            }
            Event::Done { result, .. } => {
                assert_eq!(result.unwrap()["value"], 42);
                break;
            }
        }
    }
}

#[test]
fn same_worker_second_provider_reply_runs_js_before_first_release() {
    for runtime in backends() {
        let mut worker = worker(runtime);
        worker.start(
            "const order=[];await Promise.all([agent.browsers.get('held').then(()=>order.push('held')),agent.browsers.get('fast').then(async()=>{order.push('fast');await agent.browsers.get('release')})]);order",
            Duration::from_secs(10),
        ).unwrap();
        let mut held = None;
        let mut seen_fast = false;
        let mut released = false;
        loop {
            match worker
                .event(Duration::from_secs(12))
                .unwrap()
                .expect("bounded worker progress")
            {
                Event::Call {
                    method,
                    args,
                    reply,
                    control,
                } => {
                    assert_eq!(method, "browser.info");
                    match args["browser"].as_str().unwrap() {
                        "held" => {
                            assert!(held.is_none());
                            // Ordinary pending work keeps a metered clock.
                            // A human approval pause must not run arbitrary JS.
                            held = Some((reply, control));
                        }
                        "fast" => {
                            assert!(held.is_some(), "first provider remains held");
                            seen_fast = true;
                            // Approval control is routed by request ID even
                            // while an earlier provider reply remains held.
                            control.suspend().unwrap().resume().unwrap();
                            reply.send(Ok(json!({"id":"fast","type":"cdp"}))).unwrap();
                        }
                        "release" => {
                            // This call can only originate after B's reply has
                            // settled its JS continuation in the same kernel.
                            assert!(seen_fast);
                            let (first, control) = held.take().unwrap();
                            assert!(control.is_active());
                            control.execution_validity().unwrap().validate().unwrap();
                            reply
                                .send(Ok(json!({"id":"release","type":"cdp"})))
                                .unwrap();
                            first.send(Ok(json!({"id":"held","type":"cdp"}))).unwrap();
                            released = true;
                        }
                        other => panic!("unexpected browser {other}"),
                    }
                }
                Event::Done { result, .. } => {
                    let result = result.unwrap();
                    assert!(released, "{runtime:?}: {result}");
                    assert_eq!(
                        result["value"],
                        json!(["fast", "held"]),
                        "{runtime:?}: {result}"
                    );
                    break;
                }
            }
        }
    }
}

#[test]
fn cancelled_pending_worker_reply_cannot_poison_next_kernel() {
    for runtime in backends() {
        let mut worker = worker(runtime);
        worker
            .start(
                "await agent.browsers.get('cancelled')",
                Duration::from_secs(5),
            )
            .unwrap();
        let (control, late_reply) = match worker.event(Duration::from_secs(5)).unwrap().unwrap() {
            Event::Call { reply, control, .. } => (control, reply),
            _ => panic!("expected held call"),
        };
        worker.cancel();
        assert!(done(&mut worker).get("error").is_some());
        assert!(!control.is_active());
        // Model a provider finishing after cancellation. A disconnected receiver
        // is correct; the old reply must never become a new cell's response.
        let _ = late_reply.send(Ok(json!({"id":"cancelled","type":"cdp"})));
        worker
            .start(
                "await agent.browsers.get('fresh'); 42",
                Duration::from_secs(5),
            )
            .unwrap();
        loop {
            match worker.event(Duration::from_secs(5)).unwrap().unwrap() {
                Event::Call { method, reply, .. } => {
                    let response = if method == "sky.setup" {
                        json!({"target":"mac"})
                    } else {
                        assert_eq!(method, "browser.info");
                        json!({"id":"fresh","type":"cdp"})
                    };
                    reply.send(Ok(response)).unwrap();
                }
                Event::Done { result, .. } => {
                    let result = result.unwrap();
                    assert_eq!(result["value"], 42, "{runtime:?}: {result}");
                    break;
                }
            }
        }
    }
}
