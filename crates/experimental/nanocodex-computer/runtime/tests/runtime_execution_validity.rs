//! Actual worker construction and native observations; no new wire/test grant.
use serde_json::{Value, json};
use skyre::{
    runtime::{HostOptions, ProviderControl, RuntimeBackend},
    worker::{Event, Worker},
};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

fn worker(runtime: RuntimeBackend) -> Worker {
    Worker::with_options_and_executable(
        HostOptions {
            runtime,
            ..Default::default()
        },
        env!("CARGO_BIN_EXE_nanocodex-computer").into(),
    )
}
fn next(worker: &mut Worker) -> Event {
    worker
        .event(Duration::from_secs(12))
        .unwrap()
        .expect("bounded native worker progress")
}
fn info_call(worker: &mut Worker) -> (ProviderControl, mpsc::SyncSender<skyre::Result<Value>>) {
    loop {
        match next(worker) {
            Event::Call {
                method,
                control,
                reply,
                ..
            } if method == "sky.setup" => {
                control
                    .execution_validity()
                    .expect("native setup validator")
                    .validate()
                    .unwrap();
                reply.send(Ok(json!({"target":"mac"}))).unwrap();
            }
            Event::Call {
                method,
                control,
                reply,
                ..
            } if method == "browser.info" => return (control, reply),
            _ => panic!("Expected owned browser.info provider call"),
        }
    }
}
fn complete(worker: &mut Worker) -> Value {
    match next(worker) {
        Event::Done { result, .. } => result.unwrap(),
        _ => panic!("Expected native completion without additional provider calls"),
    }
}

#[test]
fn native_worker_execution_validity_tracks_suspend_reply_reset_and_next_cell() {
    for runtime in RuntimeBackend::available() {
        let mut worker = worker(runtime);
        worker
            .start(
                "await agent.browsers.get('owned'); 42",
                Duration::from_secs(5),
            )
            .unwrap();
        let (control, reply) = info_call(&mut worker);
        let validity = control
            .execution_validity()
            .expect("native execution validator");
        validity.validate().unwrap();
        let suspension = control.suspend().unwrap();
        assert!(validity.validate().is_err());
        suspension.resume().unwrap();
        validity.validate().unwrap();
        reply.send(Ok(json!({"id":"owned","type":"cdp"}))).unwrap();
        let result = complete(&mut worker);
        assert_eq!(result["value"], 42, "{runtime:?}: {result}");
        assert!(validity.validate().is_err());
        worker.reset().unwrap();
        worker
            .start(
                "await agent.browsers.get('owned-next'); 43",
                Duration::from_secs(5),
            )
            .unwrap();
        let (new_control, reply) = info_call(&mut worker);
        assert!(validity.validate().is_err());
        let new_validity = new_control
            .execution_validity()
            .expect("fresh native validator");
        new_validity.validate().unwrap();
        reply
            .send(Ok(json!({"id":"owned-next","type":"cdp"})))
            .unwrap();
        let result = complete(&mut worker);
        assert_eq!(result["value"], 43, "{runtime:?}: {result}");
        assert!(new_validity.validate().is_err());
        assert!(validity.validate().is_err());
    }
}

#[test]
fn native_worker_execution_validity_rejects_cancel_while_provider_reply_is_pending() {
    for runtime in RuntimeBackend::available() {
        let mut worker = worker(runtime);
        worker
            .start(
                "await agent.browsers.get('owned'); 42",
                Duration::from_secs(5),
            )
            .unwrap();
        let (control, reply) = info_call(&mut worker);
        let validity = control
            .execution_validity()
            .expect("native execution validator");
        validity.validate().unwrap();
        worker.cancel();
        assert!(validity.validate().is_err());
        drop(reply);
        assert!(complete(&mut worker).get("error").is_some());
        assert!(validity.validate().is_err());
        worker
            .start(
                "await agent.browsers.get('owned-next'); 43",
                Duration::from_secs(5),
            )
            .unwrap();
        let (control, reply) = info_call(&mut worker);
        control.execution_validity().unwrap().validate().unwrap();
        assert!(validity.validate().is_err());
        reply
            .send(Ok(json!({"id":"owned-next","type":"cdp"})))
            .unwrap();
        assert_eq!(complete(&mut worker)["value"], 43);
    }
}

#[test]
fn native_worker_execution_validity_observes_expiry_without_human_credit() {
    for runtime in RuntimeBackend::available() {
        let mut worker = worker(runtime);
        // Initialize the owned runtime under its ordinary setup budget first.
        worker
            .start(
                "await agent.browsers.get('owned-warm'); 0",
                Duration::from_secs(5),
            )
            .unwrap();
        let (_, reply) = info_call(&mut worker);
        reply
            .send(Ok(json!({"id":"owned-warm","type":"cdp"})))
            .unwrap();
        assert_eq!(complete(&mut worker)["value"], 0);
        worker
            .start(
                "await agent.browsers.get('owned-expiry'); 42",
                Duration::from_millis(150),
            )
            .unwrap();
        let (control, reply) = info_call(&mut worker);
        let validity = control
            .execution_validity()
            .expect("native execution validator");
        validity.validate().unwrap();
        let bound = Instant::now() + Duration::from_secs(2);
        while validity.validate().is_ok() {
            assert!(
                Instant::now() < bound,
                "native expiry remained charged: {runtime:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        // The async event loop may retire the expired provider before this
        // thread samples it. Both clock expiry and retirement must deny action.
        let error = validity.validate().unwrap_err();
        assert!(
            error.code == -32800 || !control.is_active(),
            "expiry must fail closed: {runtime:?}: {error:?}"
        );
        let _ = reply.send(Ok(json!({"id":"owned-expiry","type":"cdp"})));
        let result = complete(&mut worker);
        assert!(result.get("error").is_some(), "{runtime:?}: {result}");
        assert!(validity.validate().is_err());
    }
}
