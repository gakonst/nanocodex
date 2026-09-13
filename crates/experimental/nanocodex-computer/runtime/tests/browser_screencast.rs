//! Public browser routes against a deterministic, owned loopback CDP provider.
use serde_json::{Value, json};
use skyre::browser::Browsers;
use std::{
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tungstenite::Message;

#[derive(Default)]
struct Data {
    mode: &'static str,
    requests: Vec<Value>,
    request_connections: Vec<usize>,
    id: u64,
    refuse_stop: usize,
    refuse_attach: usize,
    refuse_ack: bool,
}
struct Provider {
    endpoint: String,
    data: Arc<Mutex<Data>>,
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    worker: Option<thread::JoinHandle<()>>,
}
fn frame(id: u64, timestamp: f64) -> Value {
    json!({"sessionId":"main","method":"Page.screencastFrame","params":{"sessionId":id,"data":"owned-frame","metadata":{"timestamp":timestamp}}})
}
impl Provider {
    fn start(mode: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let socket = Arc::new(Mutex::new(None));
        let data = Arc::new(Mutex::new(Data {
            mode,
            ..Default::default()
        }));
        let halted = stop.clone();
        let connected = socket.clone();
        let shared = data.clone();
        let worker = thread::spawn(move || {
            let mut connection = 0;
            loop {
                let stream = loop {
                    if halted.load(Ordering::Relaxed) {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1))
                        }
                        Err(error) => panic!("owned screencast listener: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                *connected.lock().unwrap() = Some(stream.try_clone().unwrap());
                if halted.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(mut ws) = tungstenite::accept(stream) else {
                    return;
                };
                connection += 1;
                while let Ok(message) = ws.read() {
                    if !message.is_text() {
                        continue;
                    }
                    let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                    let mut data = shared.lock().unwrap();
                    data.requests.push(request.clone());
                    data.request_connections.push(connection);
                    let mut error = false;
                    let result = match request["method"].as_str().unwrap() {
                        "Target.attachToTarget" => {
                            if data.refuse_attach > 0 {
                                data.refuse_attach -= 1;
                                error = true;
                            }
                            json!({"sessionId":"main"})
                        }
                        "Runtime.evaluate" => json!({"result":{"value":null}}),
                        "Page.captureScreenshot" => json!({"data":"owned-fallback"}),
                        "Page.startScreencast" => {
                            data.id += 1;
                            let event = match data.mode {
                                "invisible" => Some(
                                    json!({"sessionId":"main","method":"Page.screencastVisibilityChanged","params":{"visible":false}}),
                                ),
                                "idle" => None,
                                "stale" => Some(frame(data.id, 0.)),
                                _ => Some(frame(
                                    data.id,
                                    SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs_f64()
                                        + 1.,
                                )),
                            };
                            if let Some(mut event) = event {
                                if data.mode == "empty" {
                                    event["params"]["data"] = json!("");
                                } else if data.mode == "oversized" {
                                    event["params"]["data"] =
                                        json!("x".repeat(4 * 1024 * 1024 + 1024));
                                }
                                ws.send(Message::text(event.to_string())).unwrap();
                            }
                            if data.mode == "nested_pressure" {
                                ws.send(Message::text(json!({"method":"Fetch.requestPaused","sessionId":"main",
                                    "params":{"requestId":"owned-html","resourceType":"Document","responseStatusCode":200,
                                        "responseHeaders":[{"name":"Content-Type","value":"text/html"}]}}).to_string())).unwrap();
                            }
                            if matches!(
                                data.mode,
                                "pressure"
                                    | "byte_pressure"
                                    | "nested_pressure"
                                    | "pressure_start_refused"
                            ) {
                                // This second ID is never replayed by stop, so its later
                                // retirement proves receipt collection before eviction.
                                ws.send(Message::text(frame(data.id + 100, 0.).to_string()))
                                    .unwrap();
                                let (count, payload) = if data.mode == "byte_pressure" {
                                    (20, "x".repeat(256 * 1024))
                                } else {
                                    (300, String::new())
                                };
                                for index in 0..count {
                                    ws.send(Message::text(json!({"method":"Runtime.consoleAPICalled","sessionId":"main",
                                        "params":{"type":"log","args":[],"fixtureIndex":index,"fixturePayload":payload}}).to_string())).unwrap();
                                }
                            }
                            error = matches!(data.mode, "start_refused" | "pressure_start_refused");
                            json!({})
                        }
                        "Page.stopScreencast" => {
                            if data.refuse_stop > 0 {
                                data.refuse_stop -= 1;
                                error = true;
                            }
                            // A late frame is deliberately delivered while stop's
                            // acknowledgement is still pending.
                            ws.send(Message::text(frame(data.id, 0.).to_string()))
                                .unwrap();
                            json!({})
                        }
                        "Page.screencastFrameAck" => {
                            if data.mode == "stale" {
                                data.mode = "fresh";
                                data.id += 1;
                                ws.send(Message::text(
                                    frame(
                                        data.id,
                                        SystemTime::now()
                                            .duration_since(UNIX_EPOCH)
                                            .unwrap()
                                            .as_secs_f64()
                                            + 1.,
                                    )
                                    .to_string(),
                                ))
                                .unwrap();
                            }
                            error = data.refuse_ack;
                            data.refuse_ack = false;
                            json!({})
                        }
                        "Fixture.emit" => {
                            for event in request["params"]["events"].as_array().unwrap() {
                                ws.send(Message::text(event.to_string())).unwrap();
                            }
                            json!({})
                        }
                        _ => json!({}),
                    };
                    if data.mode == "disconnect" && request["method"] == "Page.startScreencast" {
                        data.mode = "fresh";
                        ws.get_mut().shutdown(Shutdown::Both).unwrap();
                        break;
                    }
                    let response = if error {
                        json!({"id":request["id"],"error":{"code":-32000,"message":"owned provider refusal"}})
                    } else {
                        json!({"id":request["id"],"result":result})
                    };
                    if ws.send(Message::text(response.to_string())).is_err() {
                        break;
                    }
                }
            }
        });
        Self {
            endpoint,
            data,
            stop,
            socket,
            worker: Some(worker),
        }
    }
    fn commands(&self) -> Vec<String> {
        self.data
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter_map(|request| request["method"].as_str())
            .filter(|method| {
                method.contains("Screencast")
                    || *method == "Page.screencastFrameAck"
                    || *method == "Page.captureScreenshot"
            })
            .map(str::to_owned)
            .collect()
    }
    fn driver(&self) -> Browsers {
        let mut browsers = Browsers::default();
        browsers.register("owned", &self.endpoint).unwrap();
        browsers
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(socket) = self.socket.lock().unwrap().as_ref() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
fn call(browsers: &mut Browsers, method: &str, mut args: Value) -> skyre::Result<Value> {
    args["browser"] = json!("owned");
    args["tab"] = json!("one");
    browsers.execute(method, &args)
}
fn raw(browsers: &mut Browsers, method: &str, params: Value) -> skyre::Result<Value> {
    call(
        browsers,
        "cdp_call",
        json!({"method":method,"params":params}),
    )
}
fn original_capture_commands(mode: &str) -> Vec<String> {
    let oracle: Value =
        serde_json::from_str(include_str!("oracles/browser_screencast.json")).unwrap();
    let capture = oracle["captures"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capture| capture["mode"] == mode)
        .unwrap();
    let mut commands = capture["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|command| command["method"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    if capture["value"].is_null() {
        commands.push("Page.captureScreenshot".into());
    }
    commands
}

#[test]
fn viewport_capture_discards_stale_frames_and_stops_before_ack() {
    let provider = Provider::start("stale");
    let mut browsers = provider.driver();
    assert_eq!(
        call(&mut browsers, "screenshot", json!({})).unwrap(),
        json!({"mime_type":"image/png","data":"owned-frame"})
    );
    assert_eq!(provider.commands(), original_capture_commands("stale"));
    let current = provider.data.lock().unwrap().id;
    raw(
        &mut browsers,
        "Fixture.emit",
        json!({"events":[frame(current,0.),frame(current+1,0.)]}),
    )
    .unwrap();
    let events = call(
        &mut browsers,
        "cdp_events",
        json!({"after_sequence":0,"methods":["Page.screencastFrame"]}),
    )
    .unwrap();
    assert_eq!(events["events"].as_array().unwrap().len(), 1);
    assert_eq!(events["events"][0]["params"]["sessionId"], current + 1);
}

#[test]
fn raw_acknowledged_ownership_skips_internal_capture_and_failed_stop_keeps_it() {
    let provider = Provider::start("fresh");
    let mut browsers = provider.driver();
    raw(&mut browsers, "Page.startScreencast", json!({})).unwrap();
    assert_eq!(
        call(&mut browsers, "screenshot", json!({})).unwrap()["data"],
        "owned-fallback"
    );
    provider.data.lock().unwrap().refuse_stop = 1;
    assert!(raw(&mut browsers, "Page.stopScreencast", json!({})).is_err());
    assert_eq!(
        call(&mut browsers, "screenshot", json!({})).unwrap()["data"],
        "owned-fallback"
    );
    raw(&mut browsers, "Page.stopScreencast", json!({})).unwrap();
    assert_eq!(
        call(&mut browsers, "screenshot", json!({})).unwrap()["data"],
        "owned-frame"
    );
    assert_eq!(
        provider.commands(),
        [
            "Page.startScreencast",
            "Page.captureScreenshot",
            "Page.stopScreencast",
            "Page.captureScreenshot",
            "Page.stopScreencast",
            "Page.startScreencast",
            "Page.stopScreencast",
            "Page.screencastFrameAck"
        ]
    );
}

#[test]
fn capture_start_stop_ack_visibility_and_wait_failures_cleanup_and_allow_retry() {
    for mode in [
        "start_refused",
        "stop_refused",
        "ack_refused",
        "invisible",
        "idle",
        "empty",
    ] {
        let provider = Provider::start(mode);
        if mode == "stop_refused" {
            provider.data.lock().unwrap().refuse_stop = 1;
        }
        if mode == "ack_refused" {
            provider.data.lock().unwrap().refuse_ack = true;
        }
        let mut browsers = provider.driver();
        assert_eq!(
            call(&mut browsers, "screenshot", json!({})).unwrap()["data"],
            "owned-fallback",
            "{mode}"
        );
        let commands = provider.commands();
        assert_eq!(
            commands,
            original_capture_commands(mode),
            "captured original u9 command trace for {mode}"
        );
        assert_eq!(commands.first().unwrap(), "Page.startScreencast", "{mode}");
        assert!(
            commands.iter().any(|m| m == "Page.stopScreencast"),
            "{mode}: {commands:?}"
        );
        if mode == "stop_refused" {
            assert_eq!(
                commands,
                [
                    "Page.startScreencast",
                    "Page.stopScreencast",
                    "Page.screencastFrameAck",
                    "Page.stopScreencast",
                    "Page.captureScreenshot"
                ]
            );
        }
        provider.data.lock().unwrap().mode = "fresh";
        assert_eq!(
            call(&mut browsers, "screenshot", json!({})).unwrap()["data"],
            "owned-frame",
            "retry {mode}"
        );
        let events = call(
            &mut browsers,
            "cdp_events",
            json!({"after_sequence":0,"methods":["Page.screencastFrame","Page.screencastVisibilityChanged"]}),
        )
        .unwrap();
        assert!(
            events["events"].as_array().unwrap().is_empty(),
            "{mode}: {events}"
        );
    }
}

#[test]
fn disconnected_internal_cleanup_never_reattaches_a_retired_owner() {
    let provider = Provider::start("disconnect");
    let mut browsers = provider.driver();
    assert_eq!(
        call(&mut browsers, "screenshot", json!({})).unwrap()["data"],
        "owned-fallback"
    );
    let original = original_capture_commands("disconnect");
    assert_eq!(
        original,
        [
            "Page.startScreencast",
            "Page.stopScreencast",
            "Page.captureScreenshot"
        ]
    );
    let actual = provider.commands();
    assert_eq!(actual, ["Page.startScreencast", "Page.captureScreenshot"]);
    eprintln!("retained original disconnect trace: {original:?}; safer native trace: {actual:?}");
    // The original helper attempts a stop after disconnection. Native cleanup
    // cannot create a replacement session after its owned route was retired.
    // Ordinary screenshot fallback may establish its own new attachment.
    let data = provider.data.lock().unwrap();
    let start = data
        .requests
        .iter()
        .position(|r| r["method"] == "Page.startScreencast")
        .unwrap();
    let fallback = data
        .requests
        .iter()
        .position(|r| r["method"] == "Page.captureScreenshot")
        .unwrap();
    let replacement = data.request_connections[fallback];
    assert_ne!(data.request_connections[start], replacement);
    let replacement_requests: Vec<_> = data
        .requests
        .iter()
        .zip(&data.request_connections)
        .filter(|(_, connection)| **connection == replacement)
        .map(|(request, _)| request)
        .collect();
    assert_eq!(
        replacement_requests.first().unwrap()["method"],
        "Target.attachToTarget"
    );
    assert_eq!(
        replacement_requests
            .iter()
            .filter(|r| r["method"] == "Target.attachToTarget")
            .count(),
        1
    );
    assert!(
        replacement_requests
            .iter()
            .all(|r| r["method"] != "Page.stopScreencast"
                && r["method"] != "Page.screencastFrameAck")
    );
    assert_eq!(
        replacement_requests.last().unwrap()["method"],
        "Page.captureScreenshot"
    );
    drop(data);
    provider.data.lock().unwrap().mode = "fresh";
    assert_eq!(
        call(&mut browsers, "screenshot", json!({})).unwrap()["data"],
        "owned-frame"
    );
    let events = call(&mut browsers, "cdp_events", json!({"after_sequence":0,"methods":["Page.screencastFrame","Page.screencastVisibilityChanged"]})).unwrap();
    assert!(events["events"].as_array().unwrap().is_empty());
}

#[test]
fn nested_raw_screencast_does_not_claim_top_level_capture() {
    let provider = Provider::start("fresh");
    let mut browsers = provider.driver();
    call(
        &mut browsers,
        "cdp_call",
        json!({"method":"Page.startScreencast","target":{"sessionId":"child"}}),
    )
    .unwrap();
    assert_eq!(
        call(&mut browsers, "screenshot", json!({})).unwrap()["data"],
        "owned-frame"
    );
    assert_eq!(
        provider
            .commands()
            .iter()
            .filter(|m| m.as_str() == "Page.startScreencast")
            .count(),
        2
    );
}

#[test]
fn receipt_owner_retains_pending_frames_and_ids_before_bounded_history_eviction() {
    for mode in [
        "pressure",
        "byte_pressure",
        "nested_pressure",
        "pressure_start_refused",
        "oversized",
    ] {
        let provider = Provider::start(mode);
        let mut browsers = provider.driver();
        let result = call(&mut browsers, "screenshot", json!({})).unwrap();
        match mode {
            "pressure_start_refused" => assert_eq!(result["data"], "owned-fallback"),
            "oversized" => assert_eq!(
                result["data"].as_str().unwrap().len(),
                4 * 1024 * 1024 + 1024
            ),
            _ => assert_eq!(result["data"], "owned-frame", "{mode}"),
        }
        assert_eq!(
            provider.commands(),
            original_capture_commands(if mode == "pressure_start_refused" {
                "start_refused"
            } else {
                "fresh"
            }),
            "{mode}"
        );
        if mode == "nested_pressure" {
            assert!(
                provider
                    .data
                    .lock()
                    .unwrap()
                    .requests
                    .iter()
                    .any(|request| request["method"] == "Fetch.continueResponse")
            );
        }
        let retired = if mode == "oversized" { 1 } else { 101 };
        raw(
            &mut browsers,
            "Fixture.emit",
            json!({"events":[frame(retired, 0.), frame(999, 0.)]}),
        )
        .unwrap();
        let events = call(
            &mut browsers,
            "cdp_events",
            json!({"after_sequence":0,"methods":["Page.screencastFrame"]}),
        )
        .unwrap();
        assert_eq!(
            events["events"].as_array().unwrap().len(),
            1,
            "{mode}: {events}"
        );
        assert_eq!(events["events"][0]["params"]["sessionId"], 999, "{mode}");
    }
}

#[test]
fn raw_screencast_setup_and_dispatch_failures_release_without_cleanup_dispatch() {
    let provider = Provider::start("fresh");
    provider.data.lock().unwrap().refuse_attach = 1;
    let mut browsers = provider.driver();
    assert!(raw(&mut browsers, "Page.startScreencast", json!({})).is_err());
    assert!(
        provider.commands().is_empty(),
        "attachment refusal must dispatch no screencast command"
    );
    raw(&mut browsers, "Page.startScreencast", json!({})).unwrap();
    raw(&mut browsers, "Page.stopScreencast", json!({})).unwrap();
    provider.data.lock().unwrap().mode = "start_refused";
    assert!(raw(&mut browsers, "Page.startScreencast", json!({})).is_err());
    assert_eq!(
        provider.commands(),
        [
            "Page.startScreencast",
            "Page.stopScreencast",
            "Page.startScreencast"
        ]
    );
    provider.data.lock().unwrap().mode = "fresh";
    assert_eq!(
        call(&mut browsers, "screenshot", json!({})).unwrap()["data"],
        "owned-frame"
    );
    assert_eq!(
        provider.commands(),
        [
            "Page.startScreencast",
            "Page.stopScreencast",
            "Page.startScreencast",
            "Page.startScreencast",
            "Page.stopScreencast",
            "Page.screencastFrameAck"
        ]
    );
}
