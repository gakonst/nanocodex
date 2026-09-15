use serde_json::{Value, json};
use skyre::download_elicitation::{Context, DownloadElicitationBroker, Ticket};
use skyre::{Error, Result};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

fn context() -> Context {
    Context {
        request_id: json!(42),
        mcp_initialized: true,
        elicitation_supported: true,
        elicitation_timeout: Duration::from_secs(1),
    }
}
fn request() -> Value {
    json!({"message":"Allow download from https://owned.test/file?","meta":{"connector_id":"browser-use","tool_name":"download_browser_files","file_transfer":"download","origin":"https://owned.test","persist":["session"]}})
}
fn fixture() -> (DownloadElicitationBroker, mpsc::Receiver<Value>) {
    let (send, receive) = mpsc::channel();
    let broker = DownloadElicitationBroker::new(
        Arc::new(move |value, _| {
            send.send(value.clone()).unwrap();
            Ok(())
        }),
        Arc::new(|message, initialized| {
            initialized
                && message.get("id").is_some()
                && message["method"] == "tools/call"
                && message["params"]["name"] == "js_reset"
                && message["params"]["arguments"] == json!({})
        }),
        Some(Arc::new(|message| {
            message["params"]["authorityToken"] == "owned-host-capability"
        })),
    );
    (broker, receive)
}
fn start(ticket: Ticket) -> thread::JoinHandle<Result<Value>> {
    thread::spawn(move || ticket.request(request(), Instant::now() + Duration::from_secs(1)))
}
fn accept(broker: &DownloadElicitationBroker, outgoing: &Value) {
    assert!(
        broker.route(&json!({"jsonrpc":"2.0","id":outgoing["id"],"result":{"action":"accept"}}))
    );
}

#[test]
fn download_prompt_readiness_requires_a_live_provider_call_and_rejects_revoked_cells() {
    use skyre::runtime::{Host, HostOptions, RuntimeBackend};
    use std::sync::atomic::AtomicBool;

    let backends = RuntimeBackend::available();
    for runtime in backends {
        let (broker, _) = fixture();
        let ticket = broker.activate(context());
        assert!(!ticket.provider_ready().unwrap());
        let callback_ticket = ticket.clone();
        let retained = Arc::new(Mutex::new(None));
        let captured = retained.clone();
        let mut host = Host::with_controlled_dispatch(
            move |method, _, control| {
                if method == "sky.setup" {
                    return Ok(json!({"target":"mac"}));
                }
                let binding = callback_ticket.bind_provider(control)?;
                assert!(callback_ticket.provider_ready().unwrap());
                *captured.lock().unwrap() = Some(binding);
                Ok(json!(42))
            },
            Arc::new(AtomicBool::new(false)),
            HostOptions {
                runtime,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            host.evaluate("await nodeRepl.rpc('owned',{})", Duration::from_secs(5))
                .unwrap()["value"],
            42
        );
        // Holding the Rust binding cannot keep a completed runtime call alive.
        assert_eq!(ticket.provider_ready().unwrap_err().code, -32800);
        drop(retained.lock().unwrap().take());
        assert!(!ticket.provider_ready().unwrap());
        let next = broker.activate(context());
        assert_eq!(ticket.provider_ready().unwrap_err().code, -32800);
        assert!(!next.provider_ready().unwrap());
        next.finish();
        assert_eq!(next.provider_ready().unwrap_err().code, -32800);
    }
}

#[test]
fn download_elicitation_correlates_unpredictable_response_and_preserves_metadata() {
    let (broker, receive) = fixture();
    let ticket = broker.activate(context());
    assert!(
        !broker.route(&json!({"jsonrpc":"2.0","id":"preexisting","result":{"action":"accept"}}))
    );
    let task = start(ticket.clone());
    let outgoing = receive.recv_timeout(Duration::from_secs(1)).unwrap();
    let suffix = outgoing["id"]
        .as_str()
        .unwrap()
        .strip_prefix("skyre-download-elicitation-")
        .unwrap();
    assert_eq!(suffix.len(), 64);
    assert!(suffix.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(outgoing["method"], "elicitation/create");
    assert_eq!(outgoing["params"]["_meta"], request()["meta"]);
    assert_eq!(
        outgoing["params"]["requestedSchema"],
        json!({"type":"object","properties":{}})
    );
    assert!(outgoing["params"].get("meta").is_none());
    assert!(!broker.route(&json!({"jsonrpc":"2.0","id":"unrelated","result":{"action":"accept"}})));
    accept(&broker, &outgoing);
    assert_eq!(task.join().unwrap().unwrap(), json!({"action":"accept"}));
    assert!(
        !broker.route(&json!({"jsonrpc":"2.0","id":outgoing["id"],"result":{"action":"accept"}}))
    );
    ticket.finish();
}

#[test]
fn download_elicitation_revokes_pending_on_exact_control_but_leaves_request_queued() {
    for control in [
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":42}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"js_reset","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"host/turn","params":{"authorityToken":"owned-host-capability"}}),
    ] {
        let (broker, receive) = fixture();
        let ticket = broker.activate(context());
        let task = start(ticket.clone());
        let outgoing = receive.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!broker.route(&control));
        assert_eq!(task.join().unwrap().unwrap_err().code, -32800);
        assert!(
            !broker
                .route(&json!({"jsonrpc":"2.0","id":outgoing["id"],"result":{"action":"accept"}}))
        );
        assert_eq!(
            ticket
                .request(request(), Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .code,
            -32800
        );
    }
}

#[test]
fn download_elicitation_ignores_invalid_or_unrelated_control_messages() {
    let (broker, receive) = fixture();
    let task = start(broker.activate(context()));
    let outgoing = receive.recv_timeout(Duration::from_secs(1)).unwrap();
    for message in [
        json!({"jsonrpc":"1.0","method":"notifications/cancelled","params":{"requestId":42}}),
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":43}}),
        json!({"jsonrpc":"2.0","id":8,"method":"notifications/cancelled","params":{"requestId":42}}),
        json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"js_reset","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"js_reset","arguments":{"unexpected":true}}}),
        json!({"jsonrpc":"2.0","id":8,"method":"host/turn","params":{"authorityToken":"forged"}}),
    ] {
        assert!(!broker.route(&message));
    }
    accept(&broker, &outgoing);
    assert_eq!(task.join().unwrap().unwrap()["action"], "accept");
}

#[test]
fn download_elicitation_old_ticket_finish_and_late_reply_cannot_affect_new_cell() {
    let (broker, receive) = fixture();
    let old = broker.activate(context());
    let old_task = start(old.clone());
    let old_request = receive.recv_timeout(Duration::from_secs(1)).unwrap();
    let current = broker.activate(context());
    assert_eq!(old_task.join().unwrap().unwrap_err().code, -32800);
    assert_eq!(old.validate().unwrap_err().code, -32800);
    current.validate().unwrap();
    old.finish();
    assert!(
        !broker
            .route(&json!({"jsonrpc":"2.0","id":old_request["id"],"result":{"action":"accept"}}))
    );
    let new_task = start(current.clone());
    let new_request = receive.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_ne!(new_request["id"], old_request["id"]);
    accept(&broker, &new_request);
    assert_eq!(new_task.join().unwrap().unwrap()["action"], "accept");
    current.finish();
    assert_eq!(current.validate().unwrap_err().code, -32800);
    assert_eq!(
        current
            .request(request(), Instant::now() + Duration::from_secs(1))
            .unwrap_err()
            .code,
        -32800
    );
}

#[test]
fn download_elicitation_has_one_pending_request_and_validates_matching_response() {
    for response in [
        json!({"jsonrpc":"2.0","result":{"action":"accept"},"error":{"code":1,"message":"ambiguous"}}),
        json!({"jsonrpc":"1.0","result":{"action":"accept"}}),
        json!({"jsonrpc":"2.0","error":{"code":1,"message":"owned peer failure"}}),
    ] {
        let (broker, receive) = fixture();
        let ticket = broker.activate(context());
        let task = start(ticket.clone());
        let outgoing = receive.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            ticket
                .request(request(), Instant::now() + Duration::from_secs(1))
                .unwrap_err()
                .message
                .contains("already pending")
        );
        let mut response = response;
        response["id"] = outgoing["id"].clone();
        assert!(broker.route(&response));
        assert!(task.join().unwrap().is_err());
        assert!(receive.try_recv().is_err());
    }
}

#[test]
fn download_elicitation_disconnect_overflow_and_deadlines_fail_closed() {
    for disconnect in [false, true] {
        let (broker, receive) = fixture();
        let ticket = broker.activate(context());
        let task = start(ticket.clone());
        receive.recv_timeout(Duration::from_secs(1)).unwrap();
        if disconnect {
            broker.disconnect();
        } else {
            broker.abort_active(Error::new(-32004, "Owned queue full"));
        }
        assert!(task.join().unwrap().is_err());
        assert!(
            ticket
                .request(request(), Instant::now() + Duration::from_secs(1))
                .is_err()
        );
    }
    let (broker, receive) = fixture();
    let mut options = context();
    options.elicitation_timeout = Duration::from_millis(15);
    let ticket = broker.activate(options);
    assert_eq!(
        ticket.request(request(), Instant::now()).unwrap_err().code,
        -32008
    );
    assert!(receive.try_recv().is_err());
    let started = Instant::now();
    let task = start(ticket);
    let outgoing = receive.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(task.join().unwrap().unwrap_err().code, -32008);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(
        !broker.route(&json!({"jsonrpc":"2.0","id":outgoing["id"],"result":{"action":"accept"}}))
    );
}

#[test]
fn download_elicitation_emitter_can_route_reply_without_reentering_a_locked_broker() {
    let slot = Arc::new(Mutex::new(None::<DownloadElicitationBroker>));
    let reader = slot.clone();
    let broker = DownloadElicitationBroker::new(
        Arc::new(move |message, _| {
            accept(reader.lock().unwrap().as_ref().unwrap(), message);
            Ok(())
        }),
        Arc::new(|_, _| false),
        None,
    );
    *slot.lock().unwrap() = Some(broker.clone());
    let ticket = broker.activate(context());
    assert_eq!(
        ticket
            .request(request(), Instant::now() + Duration::from_secs(1))
            .unwrap()["action"],
        "accept"
    );
    *slot.lock().unwrap() = None;
}

#[test]
fn download_elicitation_unsupported_or_late_emitter_never_returns_approval() {
    let (broker, receive) = fixture();
    let mut options = context();
    options.elicitation_supported = false;
    assert_eq!(
        broker
            .activate(options)
            .request(request(), Instant::now() + Duration::from_secs(1))
            .unwrap_err()
            .code,
        -32601
    );
    assert!(receive.try_recv().is_err());
    let broker = DownloadElicitationBroker::new(
        Arc::new(|_, _| {
            thread::sleep(Duration::from_millis(20));
            Ok(())
        }),
        Arc::new(|_, _| false),
        None,
    );
    let ticket = broker.activate(context());
    assert_eq!(
        ticket
            .request(request(), Instant::now() + Duration::from_millis(5))
            .unwrap_err()
            .code,
        -32008
    );
}
