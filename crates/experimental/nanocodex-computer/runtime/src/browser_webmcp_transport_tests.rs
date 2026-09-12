//! Private transport partitions retained from the former public WebMCP tests.
//! Current providers are unadvertised: public activation is tested separately.
//! These tests call existing private dispatch, after registration admission;
//! they add neither a production bypass nor synthetic capability metadata.
use super::Browsers;
use serde_json::{Value, json};
use std::{
    io::ErrorKind,
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Error as WsError, Message};

struct Data {
    commands: Vec<Value>,
    invocations: usize,
    cdp_mode: bool,
    page_tools: Value,
    on_frame: Vec<Value>,
    on_pump: Vec<Value>,
    errors: Vec<String>,
}
struct Provider {
    endpoint: String,
    data: Arc<Mutex<Data>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}
fn added() -> Value {
    json!({"sessionId":"s","method":"WebMCP.toolsAdded","params":{"tools":[{"frameId":"f","name":"echo","description":"owned tool"}]}})
}
impl Provider {
    fn new(cdp_mode: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let data = Arc::new(Mutex::new(Data {
            commands: vec![],
            invocations: 0,
            cdp_mode,
            page_tools: json!([{"name":"echo","registrationId":"page-1","description":"owned tool"}]),
            on_frame: vec![],
            on_pump: vec![],
            errors: vec![],
        }));
        let shared = data.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let join = thread::spawn(move || {
            let setup = Instant::now() + Duration::from_secs(5);
            let stream = loop {
                if stopped.load(Ordering::Acquire) {
                    return;
                }
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == ErrorKind::WouldBlock && Instant::now() < setup =>
                    {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => {
                        shared.lock().unwrap().errors.push(error.to_string());
                        return;
                    }
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(
                    setup
                        .saturating_duration_since(Instant::now())
                        .max(Duration::from_millis(1)),
                ))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.get_ref()
                .set_read_timeout(Some(Duration::from_millis(10)))
                .unwrap();
            while !stopped.load(Ordering::Acquire) {
                let message = match ws.read() {
                    Ok(Message::Text(message)) => message,
                    Ok(Message::Close(_))
                    | Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => break,
                    Err(WsError::Io(error))
                        if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                    {
                        continue;
                    }
                    Ok(_) => continue,
                    Err(error) => {
                        shared.lock().unwrap().errors.push(error.to_string());
                        break;
                    }
                };
                let request: Value = serde_json::from_str(&message).unwrap();
                let (events, response) = {
                    let mut data = shared.lock().unwrap();
                    data.commands.push(request.clone());
                    let mut events = vec![];
                    let mut error = None;
                    let result = match request["method"].as_str().unwrap() {
                        "Target.attachToTarget" => json!({"sessionId":"s"}),
                        "Page.getFrameTree" => {
                            events.append(&mut data.on_frame);
                            json!({"frameTree":{"frame":{"id":"f","loaderId":"l","url":"https://owned.test/"}}})
                        }
                        "WebMCP.enable" => {
                            if data.cdp_mode {
                                events.push(added())
                            } else {
                                error = Some(
                                    json!({"code":-32601,"message":"Owned fixture uses page registry"}),
                                )
                            };
                            json!({})
                        }
                        "WebMCP.invokeTool" => {
                            data.invocations += 1;
                            events.push(json!({"sessionId":"s","method":"WebMCP.toolResponded","params":{"invocationId":"invocation","status":"Completed","output":{"ok":true}}}));
                            json!({"invocationId":"invocation"})
                        }
                        "Runtime.evaluate" => {
                            let expression = request["params"]["expression"].as_str().unwrap_or("");
                            if expression == "void 0" {
                                events.append(&mut data.on_pump);
                                json!({"result":{}})
                            } else if expression.contains("codexGetTools") {
                                json!({"result":{"value":data.page_tools}})
                            } else if expression.contains("codexExecuteTool") {
                                data.invocations += 1;
                                json!({"result":{"value":{"ok":true}}})
                            } else {
                                json!({"result":{}})
                            }
                        }
                        "Accessibility.enable"
                        | "Fetch.enable"
                        | "Fetch.disable"
                        | "Page.enable"
                        | "Runtime.enable" => json!({}),
                        method => {
                            data.errors
                                .push(format!("Unexpected fixture command {method}"));
                            json!({})
                        }
                    };
                    (
                        events,
                        if let Some(error) = error {
                            json!({"id":request["id"],"error":error})
                        } else {
                            json!({"id":request["id"],"result":result})
                        },
                    )
                };
                for event in events {
                    ws.send(Message::text(event.to_string())).unwrap();
                }
                ws.send(Message::text(response.to_string())).unwrap();
            }
        });
        Self {
            endpoint,
            data,
            stop,
            join: Some(join),
        }
    }
    fn browser(&self) -> Browsers {
        let mut browsers = Browsers::default();
        browsers.register("owned", &self.endpoint).unwrap();
        browsers
    }
    fn invoke_args(id: &Value) -> Value {
        json!({"browser":"owned","tab":"t","name":"echo","registrationId":id,"arguments":{"value":"yes"}})
    }
    fn clean(&self) {
        assert!(self.data.lock().unwrap().errors.is_empty());
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let joined = self.join.take().unwrap().join();
        if !thread::panicking() {
            joined.unwrap();
        }
    }
}

// Only this cfg(test) module can call the existing private transport owner.
trait Transport {
    fn transport(&mut self, method: &str, args: &Value) -> crate::Result<Value>;
}
impl Transport for Browsers {
    fn transport(&mut self, method: &str, args: &Value) -> crate::Result<Value> {
        let (method, mut args) = Browsers::normalize_request(method, args)?;
        self.prepare_webmcp(&method, &mut args)?;
        self.execute_inner(&method, &args)
    }
}

#[test]
fn webmcp_canonical_and_legacy_paths_require_the_fetched_current_registration() {
    for cdp in [false, true] {
        let fixture = Provider::new(cdp);
        let mut b = fixture.browser();
        let tools = b
            .transport(
                "webmcp_list_tools",
                &json!({"browser_id":"owned","tab_id":"t","browser":"foreign","tab":"foreign"}),
            )
            .unwrap();
        let id = &tools[0]["registrationId"];
        let count = fixture.data.lock().unwrap().commands.len();
        assert!(b.transport("webmcp_invoke_tool",&json!({"browser_id":"owned","tab_id":"t","tool_name":"echo","registration_id":"stale","registrationId":id,"name":"echo"})).is_err());
        assert!(
            b.transport("webmcp_invoke", &Provider::invoke_args(&json!("stale")))
                .is_err()
        );
        assert_eq!(fixture.data.lock().unwrap().commands.len(), count);
        assert_eq!(b.transport("webmcp_invoke_tool",&json!({"browser_id":"owned","tab_id":"t","tool_name":"echo","registration_id":id,"registrationId":"stale","input":{"value":"yes"},"arguments":{"ignored":true},"tool_title":"untrusted title"})).unwrap(),json!({"ok":true}));
        assert_eq!(fixture.data.lock().unwrap().invocations, 1);
        fixture.clean();
    }
}

#[test]
fn webmcp_document_and_catalog_revocation_survive_history_pressure_before_wire_send() {
    for kind in ["document", "registration"] {
        for noise in [400, 1200] {
            let fixture = Provider::new(true);
            let mut b = fixture.browser();
            let tools = b.transport("webmcp_list", &json!({"tab":"t"})).unwrap();
            let event = if kind == "document" {
                json!({"sessionId":"s","method":"Page.frameNavigated","params":{"frame":{"id":"f","loaderId":"new"}}})
            } else {
                json!({"sessionId":"s","method":"WebMCP.toolsRemoved","params":{"tools":[{"frameId":"f","name":"echo"}]}})
            };
            let mut events = vec![event];
            events.extend((0..noise).map(
                |n| json!({"sessionId":"s","method":"Runtime.consoleAPICalled","params":{"n":n}}),
            ));
            fixture.data.lock().unwrap().on_frame = events;
            let error = b
                .transport(
                    "webmcp_invoke",
                    &Provider::invoke_args(&tools[0]["registrationId"]),
                )
                .unwrap_err();
            assert!(
                error.message.contains("registration is stale"),
                "{kind}/{noise}: {error:?}"
            );
            assert_eq!(fixture.data.lock().unwrap().invocations, 0);
            fixture.data.lock().unwrap().on_pump = vec![added()];
            let fresh = b.transport("webmcp_list", &json!({"tab":"t"})).unwrap();
            assert_ne!(fresh[0]["registrationId"], tools[0]["registrationId"]);
            assert_eq!(
                b.transport(
                    "webmcp_invoke",
                    &Provider::invoke_args(&fresh[0]["registrationId"])
                )
                .unwrap(),
                json!({"ok":true})
            );
            fixture.clean();
        }
    }
}

#[test]
fn webmcp_end_session_clears_fetched_authority_without_clearing_plain_cdp_catalog() {
    let fixture = Provider::new(true);
    let mut b = fixture.browser();
    let tools = b.transport("webmcp_list", &json!({"tab":"t"})).unwrap();
    assert!(b.end_session().is_empty());
    let count = fixture.data.lock().unwrap().commands.len();
    assert!(
        b.transport(
            "webmcp_invoke",
            &Provider::invoke_args(&tools[0]["registrationId"])
        )
        .is_err()
    );
    assert_eq!(fixture.data.lock().unwrap().commands.len(), count);
    let fresh = b.transport("webmcp_list", &json!({"tab":"t"})).unwrap();
    assert_eq!(fresh, tools);
    assert_eq!(
        b.transport(
            "webmcp_invoke",
            &Provider::invoke_args(&fresh[0]["registrationId"])
        )
        .unwrap(),
        json!({"ok":true})
    );
    fixture.clean();
}

#[test]
fn webmcp_page_registry_refuses_unbound_legacy_frames_before_evaluation() {
    let fixture = Provider::new(false);
    let mut b = fixture.browser();
    let tools = b.transport("webmcp_list", &json!({"tab":"t"})).unwrap();
    let count = fixture.data.lock().unwrap().commands.len();
    for extra in [json!({"frame":"f"}), json!({"isolated":true})] {
        let mut args = Provider::invoke_args(&tools[0]["registrationId"]);
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(
            b.transport("webmcp_invoke", &args)
                .unwrap_err()
                .message
                .contains("top-level document")
        );
    }
    assert_eq!(fixture.data.lock().unwrap().commands.len(), count);
    assert_eq!(fixture.data.lock().unwrap().invocations, 0);
    fixture.clean();
}

// Existing scripted invocation-ID filtering partition, moved from browser_surface.
fn server(script: Vec<(&'static str, Value)>) -> (String, thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        let mut received = vec![];
        for (method, result) in script {
            let request: Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(request["method"], method);
            for event in result["__events"].as_array().into_iter().flatten() {
                ws.send(Message::text(event.to_string())).unwrap();
            }
            if let Some(ms) = result["__delay_ms"].as_u64() {
                thread::sleep(Duration::from_millis(ms));
            }
            let response = if result.get("__error").is_some() {
                json!({"id":request["id"],"error":result["__error"]})
            } else {
                json!({"id":request["id"],"result":result.get("__result").unwrap_or(&result)})
            };
            ws.send(Message::text(response.to_string())).unwrap();
            for event in result["__events_after"].as_array().into_iter().flatten() {
                ws.send(Message::text(event.to_string())).unwrap();
            }
            received.push(request);
        }
        received
    });
    (format!("ws://{address}"), handle)
}
fn attach() -> Vec<(&'static str, Value)> {
    vec![
        ("Target.attachToTarget", json!({"sessionId":"s"})),
        ("Accessibility.enable", json!({})),
        ("Fetch.enable", json!({})),
    ]
}
fn frame() -> Value {
    json!({"frameTree":{"frame":{"id":"f","loaderId":"loader","url":"https://owned.test/","name":""}}})
}
fn browser(endpoint: &str) -> Browsers {
    let mut b = Browsers::default();
    b.register("owned", endpoint).unwrap();
    b
}

#[test]
fn native_webmcp_event_driven_invocation_matches_id_and_registration() {
    let mut script = attach();
    script.extend([("Page.enable",json!({})),("Runtime.enable",json!({})),("WebMCP.enable",json!({"__events":[{"sessionId":"s","method":"WebMCP.toolsAdded","params":{"tools":[{"name":"echo","frameId":"f","inputSchema":{"type":"object"}}]}}],"__result":{}})),("Runtime.evaluate",json!({"result":{}})),("Runtime.evaluate",json!({"result":{}})),("Page.getFrameTree",frame()),("WebMCP.invokeTool",json!({"invocationId":"invoke-1"})),("Runtime.evaluate",json!({"__events":[{"sessionId":"s","method":"WebMCP.toolResponded","params":{"invocationId":"other","status":"Completed","output":"wrong"}},{"sessionId":"s","method":"WebMCP.toolResponded","params":{"invocationId":"invoke-1","status":"Completed","output":{"echo":"yes"}}}],"__result":{"result":{}}}))]);
    let (endpoint, join) = server(script);
    let mut b = browser(&endpoint);
    let tools = b.transport("webmcp_list", &json!({"tab":"t"})).unwrap();
    assert_eq!(tools[0]["name"], "echo");
    assert_eq!(b.transport("webmcp_invoke",&json!({"tab":"t","name":"echo","registrationId":tools[0]["registrationId"],"arguments":{"value":"yes"}})).unwrap(),json!({"echo":"yes"}));
    let requests = join.join().unwrap();
    assert_eq!(requests[9]["params"]["input"], json!({"value":"yes"}));
}
