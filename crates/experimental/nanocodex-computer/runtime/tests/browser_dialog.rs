//! Reachable dialog ownership through the production Browser/CDP router.
use serde_json::{Value, json};
use skyre::browser::Browsers;
use std::{
    collections::VecDeque,
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use tungstenite::Message;

fn accept_owned_websocket(stream: TcpStream) -> tungstenite::WebSocket<TcpStream> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut socket = tungstenite::accept(stream).unwrap();
    socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_millis(5)))
        .unwrap();
    socket
}

#[test]
fn dialog_fixture_handshake_precedes_short_event_polling_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, ready) = mpsc::channel();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        accepted.send(()).unwrap();
        let socket = accept_owned_websocket(stream);
        assert_eq!(
            socket.get_ref().read_timeout().unwrap(),
            Some(Duration::from_millis(5))
        );
    });
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    // The old five-millisecond setup timeout expired before this owned client
    // wrote its upgrade request. No dialog operation or ACK budget is changed.
    thread::sleep(Duration::from_millis(30));
    let (_, response) = tungstenite::client(format!("ws://{address}"), stream).unwrap();
    assert_eq!(response.status(), 101);
    server.join().unwrap();
}

#[derive(Default)]
struct Reply {
    method: Option<&'static str>,
    before: Vec<Value>,
    after: Vec<Value>,
    error: bool,
    ack_gate: Option<mpsc::Receiver<()>>,
}
enum Control {
    Events(Vec<Value>, mpsc::Sender<()>),
    Stop,
}
struct Fixture {
    browser: Browsers,
    control: mpsc::Sender<Control>,
    requests: Arc<Mutex<Vec<Value>>>,
    replies: Arc<Mutex<VecDeque<Reply>>>,
    acknowledgements: Arc<std::sync::atomic::AtomicUsize>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (control, receive) = mpsc::channel();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let replies: Arc<Mutex<VecDeque<Reply>>> = Arc::default();
        let acknowledgements = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ack_count = acknowledgements.clone();
        let (record, pending) = (requests.clone(), replies.clone());
        let thread = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = accept_owned_websocket(stream);
            loop {
                if let Ok(action) = receive.try_recv() {
                    match action {
                        Control::Events(events, done) => {
                            for event in events {
                                socket.send(Message::text(event.to_string())).unwrap();
                            }
                            done.send(()).unwrap();
                        }
                        Control::Stop => break,
                    }
                }
                let message = match socket.read() {
                    Ok(message) => message,
                    Err(tungstenite::Error::Io(error))
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(
                        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed,
                    ) => break,
                    Err(error) => panic!("Owned CDP fixture: {error}"),
                };
                let Message::Text(text) = message else {
                    continue;
                };
                let request: Value = serde_json::from_str(&text).unwrap();
                record.lock().unwrap().push(request.clone());
                let planned = pending.lock().unwrap().front().is_some_and(|reply| {
                    request["method"] == reply.method.unwrap_or("Page.handleJavaScriptDialog")
                });
                let mut reply = if planned || request["method"] == "Page.handleJavaScriptDialog" {
                    pending
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("unexpected dialog dispatch")
                } else {
                    Reply::default()
                };
                for event in reply.before.drain(..) {
                    socket.send(Message::text(event.to_string())).unwrap();
                }
                if let Some(gate) = reply.ack_gate {
                    let _ = gate.recv_timeout(Duration::from_secs(3));
                }
                let response = if reply.error {
                    json!({"id":request["id"],"error":{"code":-32000,"message":"owned handler rejection"}})
                } else {
                    let result = match request["method"].as_str().unwrap() {
                        "Target.attachToTarget" => json!({"sessionId":"root"}),
                        "Target.getTargetInfo" => json!({"targetInfo":{"targetId":"tab"}}),
                        _ => json!({}),
                    };
                    json!({"id":request["id"],"result":result})
                };
                socket.send(Message::text(response.to_string())).unwrap();
                if request["method"] == "Page.handleJavaScriptDialog" {
                    ack_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                if !reply.after.is_empty() {
                    thread::sleep(Duration::from_millis(15));
                }
                for event in reply.after {
                    socket.send(Message::text(event.to_string())).unwrap();
                }
            }
        });
        let mut browser = Browsers::default();
        browser.register("owned", &endpoint).unwrap();
        browser.execute("get_tab", &json!({"tab":"tab"})).unwrap();
        Self {
            browser,
            control,
            requests,
            replies,
            acknowledgements,
            thread: Some(thread),
        }
    }
    fn emit(&self, events: Vec<Value>) {
        let (send, receive) = mpsc::channel();
        self.control.send(Control::Events(events, send)).unwrap();
        receive.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    fn dialog(&mut self) -> Value {
        self.browser
            .execute("dialog_get", &json!({"tab":"tab"}))
            .unwrap()
    }
    fn observed_opening(&mut self, kind: &Value, case_index: usize) -> Value {
        let previous_id = self.dialog()["id"].clone();
        self.emit(vec![event(
            "root",
            "Page.javascriptDialogOpening",
            json!({"type":kind}),
        )]);
        // emit acknowledges the fixture socket write, not native receipt. A
        // short poll may still return the previous remembered record. Establish
        // this case's opening before testing its unchanged action semantics.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let current = self.dialog();
            if current["id"].is_string() && current["id"] != previous_id && current["type"] == *kind
            {
                eprintln!(
                    "dialog fixture receipt: case={case_index} prior_id={} observed_id={} type={kind}",
                    previous_id, current["id"]
                );
                return current;
            }
            assert!(
                Instant::now() < deadline,
                "case {case_index} opening not observed: prior={previous_id} current={current} type={kind}"
            );
            thread::yield_now();
        }
    }
    fn handle(&mut self, id: &Value) -> skyre::Result<Value> {
        self.browser.execute(
            "dialog_handle",
            &json!({"tab":"tab","dialogId":id,"accept":true,"promptText":"owned answer"}),
        )
    }
    fn plan(&self, reply: Reply) {
        self.replies.lock().unwrap().push_back(reply);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Stop);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
fn event(session: &str, method: &str, params: Value) -> Value {
    json!({"sessionId":session,"method":method,"params":params})
}
fn open(session: &str, message: &str) -> Value {
    event(
        session,
        "Page.javascriptDialogOpening",
        json!({"type":"prompt","message":message,"defaultPrompt":"initial"}),
    )
}
fn close(session: &str) -> Value {
    event(
        session,
        "Page.javascriptDialogClosed",
        json!({"result":true}),
    )
}
fn child(parent: &str, session: &str, target: &str) -> Value {
    event(
        parent,
        "Target.attachedToTarget",
        json!({"sessionId":session,"targetInfo":{"type":"iframe","targetId":target}}),
    )
}

#[test]
fn dialog_modal_gate_blocks_top_level_and_nested_commands_before_dispatch() {
    for session in ["root", "child"] {
        let mut fixture = Fixture::start();
        fixture.emit(vec![
            child("root", "child", "frame"),
            open(session, "owned modal"),
        ]);
        let current = fixture.dialog();
        let expected = "A prompt JavaScript dialog is active in this tab. Use `tab.getJsDialog()` to get it and dismiss it first.";
        for (method, args) in [
            ("reload", json!({"tab":"tab"})),
            ("frames", json!({"tab":"tab"})),
            ("get_tab", json!({"tab":"tab"})),
            ("close_tab", json!({"tab":"tab"})),
            ("press_key", json!({"tab":"tab","key":"Enter"})),
            ("screenshot", json!({"tab":"tab"})),
            (
                "cdp_call",
                json!({"tab":"tab","method":"Runtime.evaluate","params":{"expression":"0"}}),
            ),
            (
                "cdp_call",
                json!({"tab":"tab","method":"Page.getFrameTree","target":{"sessionId":"child"}}),
            ),
            (
                "cdp_call",
                json!({"tab":"tab","method":"Page.getFrameTree","target":{"targetId":"unattached"}}),
            ),
            (
                "cdp_call",
                json!({"tab":"tab","method":"Page.stopScreencast"}),
            ),
            (
                "cdp_call",
                json!({"tab":"tab","method":"Runtime.releaseObject","params":{"objectId":"unowned"}}),
            ),
        ] {
            let before = fixture.requests.lock().unwrap().len();
            let error = fixture.browser.execute(method, &args).unwrap_err();
            assert_eq!(error.message, expected, "{method}");
            assert_eq!(fixture.requests.lock().unwrap().len(), before, "{method}");
            assert_eq!(fixture.dialog(), current);
        }
        if session == "root" {
            // Native interception revocation remains available. Closing the
            // remembered dialog must not re-enable Fetch before its handler.
            fixture
                .browser
                .execute("downloads_disable", &json!({"tab":"tab"}))
                .unwrap();
        }
        let before_handle = fixture.requests.lock().unwrap().len();
        fixture.plan(Reply {
            before: vec![close(session)],
            ..Default::default()
        });
        fixture.handle(&current["id"]).unwrap();
        assert!(
            fixture.requests.lock().unwrap()[before_handle..]
                .iter()
                .all(|request| request["method"] != "Fetch.enable")
        );
        assert!(fixture.dialog().is_null());
        fixture
            .browser
            .execute("reload", &json!({"tab":"tab"}))
            .unwrap();
        assert_eq!(
            fixture.requests.lock().unwrap().last().unwrap()["method"],
            "Page.reload"
        );
    }
}

#[test]
fn dialog_modal_gate_releases_only_an_attempted_key_press() {
    let mut fixture = Fixture::start();
    fixture.plan(Reply {
        method: Some("Input.dispatchKeyEvent"),
        before: vec![open("root", "opened during key press")],
        ..Default::default()
    });
    fixture
        .browser
        .execute("press_key", &json!({"tab":"tab","key":"Enter"}))
        .unwrap();
    let keys: Vec<_> = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["method"] == "Input.dispatchKeyEvent")
        .map(|r| r["params"]["type"].clone())
        .collect();
    assert_eq!(keys, vec![json!("keyDown"), json!("keyUp")]);
    let before = fixture.requests.lock().unwrap().len();
    assert!(
        fixture
            .browser
            .execute("press_key", &json!({"tab":"tab","key":"Enter"}))
            .is_err()
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), before);
}

#[test]
fn dialog_modal_gate_preserves_started_native_screencast_cleanup() {
    let mut fixture = Fixture::start();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        + 1.0;
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jTukAAAAASUVORK5CYII=";
    fixture.plan(Reply {
        method: Some("Page.startScreencast"),
        before: vec![
            open("root", "opened after native capture start"),
            event(
                "root",
                "Page.screencastFrame",
                json!({"sessionId":1,"data":png,"metadata":{"timestamp":timestamp}}),
            ),
        ],
        ..Default::default()
    });
    let image = fixture
        .browser
        .execute("screenshot", &json!({"tab":"tab"}))
        .unwrap();
    assert_eq!(image["data"], png);
    let methods: Vec<_> = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| {
            r["method"].as_str().is_some_and(|name| {
                name.contains("Screencast") || name.contains("screencastFrameAck")
            })
        })
        .map(|r| r["method"].clone())
        .collect();
    assert_eq!(
        methods,
        vec![
            json!("Page.startScreencast"),
            json!("Page.stopScreencast"),
            json!("Page.screencastFrameAck")
        ]
    );
    let before = fixture.requests.lock().unwrap().len();
    assert!(
        fixture
            .browser
            .execute("screenshot", &json!({"tab":"tab"}))
            .is_err()
    );
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        before,
        "Refused start must not trigger cleanup stop"
    );
}

#[test]
fn dialog_canonical_identity_and_schema_reject_before_dispatch() {
    let mut fixture = Fixture::start();
    fixture.emit(vec![open("root", "first")]);
    let first = fixture.dialog();
    fixture.emit(vec![open("root", "replacement")]);
    let current = fixture.dialog();
    assert_ne!(first["id"], current["id"]);
    for action in ["accept", "dismiss"] {
        let args = json!({"browser_id":"owned","tab_id":"tab","dialog_id":first["id"],
            "action":action,"prompt_text":"owned",
            "browser":"foreign","tab":"foreign","dialogId":current["id"]});
        let before = fixture.requests.lock().unwrap().len();
        let error = fixture
            .browser
            .execute("tab_handle_js_dialog", &args)
            .unwrap_err();
        assert_eq!(error.message, "JavaScript dialog is no longer active");
        assert_eq!(fixture.requests.lock().unwrap().len(), before);
        assert_eq!(fixture.dialog(), current);
    }
    let valid =
        json!({"browser_id":"owned","tab_id":"tab","dialog_id":current["id"],"action":"dismiss"});
    for (key, value) in [
        ("action", json!("unknown")),
        ("action", Value::Null),
        ("dialog_id", Value::Null),
        ("dialog_id", json!(3)),
        ("prompt_text", Value::Null),
        ("prompt_text", json!(3)),
        ("browser_id", Value::Null),
        ("tab_id", Value::Null),
    ] {
        let mut args = valid.clone();
        args[key] = value;
        let before = fixture.requests.lock().unwrap().len();
        assert_eq!(
            fixture
                .browser
                .execute("tab_handle_js_dialog", &args)
                .unwrap_err()
                .code,
            -32602
        );
        assert_eq!(fixture.requests.lock().unwrap().len(), before, "{key}");
        assert_eq!(fixture.dialog(), current);
    }
    for key in ["browser_id", "tab_id", "dialog_id", "action"] {
        let mut args = valid.clone();
        args["browser"] = json!("owned");
        args["tab"] = json!("tab");
        args["dialogId"] = current["id"].clone();
        args["accept"] = json!(false);
        args.as_object_mut().unwrap().remove(key);
        let before = fixture.requests.lock().unwrap().len();
        assert!(
            fixture
                .browser
                .execute("tab_handle_js_dialog", &args)
                .is_err()
        );
        assert_eq!(fixture.requests.lock().unwrap().len(), before);
        assert_eq!(fixture.dialog(), current);
    }
    // The native embedding's boolean alias now also requires captured identity.
    let before = fixture.requests.lock().unwrap().len();
    assert_eq!(
        fixture
            .browser
            .execute("dialog_handle", &json!({"tab":"tab","accept":false}))
            .unwrap_err()
            .message,
        "handleJsDialog requires a dialog_id"
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), before);
    assert_eq!(fixture.dialog(), current);
}

#[test]
fn dialog_canonical_fields_own_route_identity_and_prompt_arguments() {
    let mut fixture = Fixture::start();
    for (kind, action, text, expected) in [
        ("alert", "dismiss", None, json!({"accept":true})),
        ("confirm", "accept", None, json!({"accept":true})),
        (
            "prompt",
            "accept",
            Some(""),
            json!({"accept":true,"promptText":""}),
        ),
        (
            "prompt",
            "accept",
            Some("καλημέρα 🌍"),
            json!({"accept":true,"promptText":"καλημέρα 🌍"}),
        ),
        (
            "prompt",
            "dismiss",
            Some("ignored"),
            json!({"accept":false}),
        ),
        ("beforeunload", "dismiss", None, json!({"accept":false})),
    ] {
        fixture.emit(vec![event(
            "root",
            "Page.javascriptDialogOpening",
            json!({"type":kind}),
        )]);
        let current = fixture.dialog();
        let mut args = json!({"browser_id":"owned","tab_id":"tab","dialog_id":current["id"],
            "action":action,"browser":"foreign","tab":"foreign","dialogId":"foreign",
            "accept":false,"promptText":"ignored extra"});
        if let Some(text) = text {
            args["prompt_text"] = json!(text);
        }
        fixture.plan(Reply {
            before: vec![close("root")],
            ..Default::default()
        });
        fixture
            .browser
            .execute("tab_handle_js_dialog", &args)
            .unwrap();
        let request = fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|request| request["method"] == "Page.handleJavaScriptDialog")
            .unwrap()
            .clone();
        assert_eq!(request["sessionId"], "root");
        assert_eq!(request["params"], expected);
        assert!(fixture.dialog().is_null());
        let before = fixture.requests.lock().unwrap().len();
        assert_eq!(
            fixture
                .browser
                .execute("tab_handle_js_dialog", &args)
                .unwrap_err()
                .message,
            "JavaScript dialog is no longer active"
        );
        assert_eq!(fixture.requests.lock().unwrap().len(), before);
    }
}

#[test]
fn dialog_semantic_action_partitions_match_source_pinned_original() {
    let oracle: Value =
        serde_json::from_str(include_str!("oracles/browser_dialog_actions.json")).unwrap();
    let mut fixture = Fixture::start();
    for (case_index, case) in oracle["action_cases"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let current = fixture.observed_opening(&case["type"], case_index);
        let mut args = json!({"tab":"tab","action":case["args"]["action"],
            "dialogId":if case["args"]["dialog_id"] == "1" {current["id"].clone()} else {case["args"]["dialog_id"].clone()}});
        if let Some(text) = case["args"].get("prompt_text") {
            args["promptText"] = text.clone();
        }
        let before = fixture.requests.lock().unwrap().len();
        if case["outcome"].get("result").is_some() {
            fixture.plan(Reply {
                before: vec![close("root")],
                ..Default::default()
            });
            let result = fixture.browser.execute("dialog_handle", &args).unwrap();
            assert_eq!(result, case["outcome"]["result"]);
            let requests = fixture.requests.lock().unwrap();
            let dispatched: Vec<_> = requests[before..]
                .iter()
                .filter(|r| r["method"] == "Page.handleJavaScriptDialog")
                .collect();
            assert_eq!(dispatched.len(), 1);
            assert_eq!(
                dispatched[0]["params"], case["calls"][0]["params"],
                "{case}"
            );
            drop(requests);
            assert!(fixture.dialog().is_null());
        } else {
            let error = fixture.browser.execute("dialog_handle", &args).unwrap_err();
            assert_eq!(json!(error.message), case["outcome"]["error"], "{case}");
            assert_eq!(fixture.requests.lock().unwrap().len(), before, "{case}");
            assert_eq!(fixture.dialog(), current, "{case}");
        }
    }
}

#[test]
fn dialog_close_and_detach_are_bound_to_owned_session_and_frame() {
    for removed in [
        "foreign-close",
        "root-close",
        "matching-close",
        "foreign-detach",
        "matching-detach",
        "foreign-frame",
        "matching-frame",
        "matching-frame-alias",
        "target-destroyed",
        "ancestor-detach",
    ] {
        let mut fixture = Fixture::start();
        fixture.emit(vec![
            child("root", "a", "frame-a"),
            child("root", "b", "frame-b"),
            child("a", "nested", "frame-nested"),
            event(
                "nested",
                "Page.frameNavigated",
                json!({"frame":{"id":"nested-alias"}}),
            ),
            open("nested", "owned"),
        ]);
        let original = fixture.dialog();
        assert_eq!(original, json!({"id":"1","type":"prompt"}));
        let removal = match removed {
            "foreign-close" => close("b"),
            "root-close" => close("root"),
            "matching-close" => close("nested"),
            "foreign-detach" => event(
                "root",
                "Target.detachedFromTarget",
                json!({"sessionId":"b"}),
            ),
            "matching-detach" => event(
                "a",
                "Target.detachedFromTarget",
                json!({"sessionId":"nested"}),
            ),
            "foreign-frame" => event("root", "Page.frameDetached", json!({"frameId":"frame-b"})),
            "matching-frame" => event("a", "Page.frameDetached", json!({"frameId":"frame-nested"})),
            "matching-frame-alias" => {
                event("a", "Page.frameDetached", json!({"frameId":"nested-alias"}))
            }
            "target-destroyed" => {
                json!({"method":"Target.targetDestroyed","params":{"targetId":"frame-nested"}})
            }
            "ancestor-detach" => event(
                "root",
                "Target.detachedFromTarget",
                json!({"sessionId":"a"}),
            ),
            _ => unreachable!(),
        };
        fixture.emit(vec![removal]);
        let actual = fixture.dialog();
        if matches!(
            removed,
            "foreign-close" | "root-close" | "foreign-detach" | "foreign-frame" | "ancestor-detach"
        ) {
            assert_eq!(actual, original, "{removed}");
        } else {
            assert!(actual.is_null(), "{removed}: {actual}");
        }
    }
}

#[test]
fn dialog_orphan_record_retains_identity_without_dispatch_authority() {
    for ending in ["close", "session", "frame", "tab", "replacement"] {
        let mut fixture = Fixture::start();
        fixture.emit(vec![
            child("root", "a", "frame-a"),
            child("a", "nested", "frame-nested"),
            event(
                "nested",
                "Page.frameNavigated",
                json!({"frame":{"id":"nested-alias"}}),
            ),
            open("nested", "remembered"),
        ]);
        let remembered = fixture.dialog();
        fixture.emit(vec![event(
            "root",
            "Target.detachedFromTarget",
            json!({"sessionId":"a"}),
        )]);
        assert_eq!(fixture.dialog(), remembered, "{ending}");
        let before = fixture.requests.lock().unwrap().len();
        assert!(
            fixture
                .handle(&remembered["id"])
                .unwrap_err()
                .message
                .contains("no longer active")
        );
        assert_eq!(
            fixture.requests.lock().unwrap().len(),
            before,
            "An orphaned handle must fail before every CDP command"
        );
        fixture.emit(vec![
            open("nested", "unowned late opening"),
            event(
                "foreign",
                "Target.detachedFromTarget",
                json!({"sessionId":"nested"}),
            ),
            event(
                "foreign",
                "Page.frameDetached",
                json!({"frameId":"nested-alias"}),
            ),
        ]);
        assert_eq!(fixture.dialog(), remembered, "{ending}");
        if ending == "replacement" {
            // A new attachment alone cannot revive a remembered handle. A new
            // opening on that verified route creates a distinct live identity.
            fixture.emit(vec![child("root", "nested", "new-target")]);
            assert_eq!(fixture.dialog(), remembered);
            let before = fixture.requests.lock().unwrap().len();
            assert!(
                fixture
                    .handle(&remembered["id"])
                    .unwrap_err()
                    .message
                    .contains("no longer active")
            );
            assert_eq!(fixture.requests.lock().unwrap().len(), before);
            fixture.emit(vec![open("nested", "new opening")]);
            let replacement = fixture.dialog();
            assert_ne!(replacement["id"], remembered["id"]);
            let before = fixture.requests.lock().unwrap().len();
            assert!(
                fixture
                    .handle(&remembered["id"])
                    .unwrap_err()
                    .message
                    .contains("no longer active")
            );
            assert_eq!(fixture.requests.lock().unwrap().len(), before);
            fixture.plan(Reply {
                before: vec![close("nested")],
                ..Default::default()
            });
            fixture.handle(&replacement["id"]).unwrap();
        } else {
            let event = match ending {
                "close" => close("nested"),
                "session" => event(
                    "a",
                    "Target.detachedFromTarget",
                    json!({"sessionId":"nested"}),
                ),
                "frame" => event("a", "Page.frameDetached", json!({"frameId":"nested-alias"})),
                "tab" => event(
                    "root",
                    "Target.detachedFromTarget",
                    json!({"sessionId":"root"}),
                ),
                _ => unreachable!(),
            };
            fixture.emit(vec![event]);
        }
        assert!(fixture.dialog().is_null(), "{ending}");
    }
}

#[test]
fn dialog_pending_handler_preserves_replacement_and_rejects_stale_actions() {
    let mut fixture = Fixture::start();
    fixture.emit(vec![
        child("root", "child", "frame"),
        open("child", "first"),
    ]);
    let first = fixture.dialog();
    let mut before = vec![close("child"), open("child", "second")];
    before.extend((0..1200).map(|index| event("root", "Owned.noise", json!({"index":index}))));
    fixture.plan(Reply {
        before,
        ..Default::default()
    });
    fixture.handle(&first["id"]).unwrap();
    let second = fixture.dialog();
    assert_eq!(second, json!({"id":"2","type":"prompt"}));
    assert_ne!(second["id"], first["id"]);
    let dispatched = fixture.requests.lock().unwrap().len();
    assert!(
        fixture
            .handle(&first["id"])
            .unwrap_err()
            .message
            .contains("no longer active")
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), dispatched);
    assert_eq!(fixture.dialog(), second);
    let request = fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|r| r["method"] == "Page.handleJavaScriptDialog")
        .unwrap()
        .clone();
    assert_eq!(request["sessionId"], "child");
    fixture.plan(Reply {
        after: vec![close("child")],
        ..Default::default()
    });
    fixture.handle(&second["id"]).unwrap();
    assert!(fixture.dialog().is_null());
}

#[test]
fn dialog_ack_waits_for_matching_close_and_failures_release_the_watch() {
    let mut fixture = Fixture::start();
    fixture.emit(vec![
        child("root", "child", "frame"),
        open("child", "first"),
    ]);
    let first = fixture.dialog();
    fixture.plan(Reply {
        error: true,
        ..Default::default()
    });
    assert!(
        fixture
            .handle(&first["id"])
            .unwrap_err()
            .message
            .contains("owned handler rejection")
    );
    assert_eq!(fixture.dialog(), first);
    fixture.plan(Reply {
        before: vec![close("root")],
        after: vec![close("child")],
        error: false,
        ..Default::default()
    });
    fixture.handle(&first["id"]).unwrap();
    assert!(fixture.dialog().is_null());
    fixture.emit(vec![open("root", "after failure")]);
    let next = fixture.dialog();
    fixture.plan(Reply {
        before: vec![close("root")],
        ..Default::default()
    });
    fixture.handle(&next["id"]).unwrap();
    assert!(fixture.dialog().is_null());
}

#[test]
fn dialog_close_finishes_before_late_ack_and_ignores_its_late_error() {
    for error in [false, true] {
        let mut fixture = Fixture::start();
        fixture.emit(vec![open("root", "first")]);
        let first = fixture.dialog();
        let (release, gate) = mpsc::channel();
        fixture.plan(Reply {
            before: vec![close("root")],
            error,
            ack_gate: Some(gate),
            ..Default::default()
        });
        fixture.handle(&first["id"]).unwrap();
        // The server cannot send its real ACK until this test releases it.
        assert_eq!(
            fixture
                .acknowledgements
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(fixture.dialog().is_null());
        release.send(()).unwrap();
        fixture.emit(vec![open("root", "second")]);
        let second = fixture.dialog();
        assert_eq!(second, json!({"id":"2","type":"prompt"}));
        fixture.plan(Reply {
            before: vec![close("root")],
            ..Default::default()
        });
        fixture.handle(&second["id"]).unwrap();
        assert!(fixture.dialog().is_null());
        assert_eq!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r["method"] == "Page.handleJavaScriptDialog")
                .count(),
            2
        );
    }
}

#[test]
fn dialog_control_state_survives_eviction_while_raw_handle_retains_ack_contract() {
    for noise in [400, 1200] {
        let mut fixture = Fixture::start();
        fixture.emit(vec![open("root", "first")]);
        let first = fixture.dialog();
        let cursor = fixture
            .browser
            .execute("cdp_events", &json!({"tab":"tab"}))
            .unwrap()["cursor"]
            .clone();
        let mut before = vec![close("root"), open("root", "second")];
        before.extend((0..noise).map(|index| event("root", "Owned.noise", json!({"index":index}))));
        fixture.plan(Reply {
            before,
            ..Default::default()
        });
        fixture
        .browser
        .execute(
            "cdp_call",
            &json!({"tab":"tab","method":"Page.handleJavaScriptDialog","params":{"accept":true}}),
        )
        .unwrap();
        let second = fixture.dialog();
        assert_eq!(second, json!({"id":"2","type":"prompt"}));
        assert_ne!(second["id"], first["id"]);
        let history=fixture.browser.execute("cdp_events",&json!({"tab":"tab","after_sequence":cursor,"methods":["Page.javascriptDialogOpening","Page.javascriptDialogClosed"]})).unwrap();
        assert_eq!(
            history["events"].as_array().unwrap().len(),
            if noise == 400 { 2 } else { 0 },
            "{history}"
        );
        assert_eq!(fixture.dialog(), second);
    }
}

#[test]
fn dialog_handler_race_outcomes_match_source_pinned_original_helper() {
    let oracle: Value = serde_json::from_str(include_str!("oracles/browser_dialog.json")).unwrap();
    for case in oracle["handlerCases"].as_array().unwrap() {
        let mut fixture = Fixture::start();
        let owner = case["owner"].as_str().unwrap_or("root");
        fixture.emit(vec![
            child("root", "child-a", "frame-child-a"),
            child("root", "child-b", "frame-child-b"),
            open(owner, "initial"),
        ]);
        let first = fixture.dialog();
        let mut reply = Reply::default();
        let mut acknowledged = false;
        for action in case["phases"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|phase| phase.as_array().unwrap())
        {
            let session = action["session"].as_str().unwrap_or("root");
            let event = match action["op"].as_str().unwrap() {
                "ack" => {
                    acknowledged = true;
                    continue;
                }
                "error" => {
                    acknowledged = true;
                    reply.error = true;
                    continue;
                }
                "open" => open(session, "replacement"),
                "close" => close(session),
                "removeSession" => event(
                    "root",
                    "Target.detachedFromTarget",
                    json!({"sessionId":session}),
                ),
                op => panic!("Unknown original helper action: {op}"),
            };
            if acknowledged {
                reply.after.push(event);
            } else {
                reply.before.push(event);
            }
        }
        fixture.plan(reply);
        let result = fixture.handle(&first["id"]);
        let expected = case["rows"].as_array().unwrap().last().unwrap();
        assert_eq!(
            result.is_err(),
            expected["settlement"] == "rejected",
            "{}: {result:?}",
            case["name"]
        );
        if let Err(error) = result {
            assert_eq!(
                error.message,
                expected["error"].as_str().unwrap(),
                "{}",
                case["name"]
            );
        }
        // Fence server delivery through the last scheduled event, then ask the
        // actual provider to poll. No expected state is injected into it.
        fixture.emit(vec![]);
        let current = fixture.dialog();
        let final_dialog = &expected["dialog"];
        if final_dialog.is_null() {
            assert!(current.is_null(), "{}: {current}", case["name"]);
        } else {
            assert_eq!(current["id"], final_dialog["id"], "{}", case["name"]);
            assert_eq!(current["type"], final_dialog["type"], "{}", case["name"]);
        }
        let handles: Vec<_> = fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["method"] == "Page.handleJavaScriptDialog")
            .cloned()
            .collect();
        assert_eq!(handles.len(), 1, "{}", case["name"]);
        assert_eq!(handles[0]["sessionId"], owner, "{}", case["name"]);
        if !final_dialog.is_null() {
            let target = final_dialog["session"].as_str().unwrap_or("root");
            fixture.plan(Reply {
                before: vec![close(target)],
                ..Default::default()
            });
            fixture.handle(&current["id"]).unwrap();
            let requests = fixture.requests.lock().unwrap();
            let last = requests
                .iter()
                .rfind(|request| request["method"] == "Page.handleJavaScriptDialog")
                .unwrap();
            assert_eq!(last["sessionId"], target, "{}", case["name"]);
        }
    }
}

#[test]
fn dialog_events_in_polling_remain_visible_without_double_replay() {
    let mut fixture = Fixture::start();
    fixture.emit(vec![open("root", "one"), open("root", "two")]);
    let dialog = fixture.dialog();
    assert_eq!(dialog["id"], "2");
    let events = fixture
        .browser
        .execute(
            "cdp_events",
            &json!({"tab":"tab","after_sequence":0,"methods":["Page.javascriptDialogOpening"]}),
        )
        .unwrap();
    assert_eq!(events["events"].as_array().unwrap().len(), 2);
    assert_eq!(fixture.dialog(), dialog);
    fixture.emit(vec![event(
        "root",
        "Target.detachedFromTarget",
        json!({"sessionId":"root"}),
    )]);
    assert!(fixture.dialog().is_null());
}
