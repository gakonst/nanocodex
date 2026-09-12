//! Native operation metadata ownership; synthetic loopback replies only.
//! Reuses the existing origin fixture, with no capability advertisement.
use super::*;
use crate::{
    browser_activation::{Model, Unavailable},
    fixture::Fixture,
    origin_elicitation::{Context, OriginElicitationBroker},
    runtime::ProviderControl,
    security::{Security, SecurityConfig},
};
use std::{
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use tungstenite::Message;

struct Provider {
    endpoint: String,
    state: Arc<Mutex<Data>>,
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    join: Option<thread::JoinHandle<()>>,
}
struct Data {
    pages: BTreeMap<String, (String, u64)>,
    requests: Vec<Value>,
    replace_after_context_reads: Option<usize>,
    context_reads: usize,
}
impl Provider {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(Data {
            pages: [
                ("t".into(), ("https://start.example/".into(), 1)),
                ("u".into(), ("https://start.example/".into(), 1)),
            ]
            .into(),
            requests: vec![],
            replace_after_context_reads: None,
            context_reads: 0,
        }));
        let shared = state.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let socket = Arc::new(Mutex::new(None));
        let sockets = socket.clone();
        let join = thread::spawn(move || {
            let stream = loop {
                if stopped.load(Ordering::Relaxed) {
                    return;
                }
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            *sockets.lock().unwrap() = Some(stream.try_clone().unwrap());
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            let Ok(mut ws) = tungstenite::accept(stream) else {
                return;
            };
            while let Ok(message) = ws.read() {
                if !message.is_text() {
                    continue;
                }
                let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                let mut data = shared.lock().unwrap();
                data.requests.push(request.clone());
                let tab = request["sessionId"]
                    .as_str()
                    .or(request["params"]["targetId"].as_str())
                    .unwrap_or("t")
                    .to_owned();
                let page = data
                    .pages
                    .get(&tab)
                    .cloned()
                    .unwrap_or(("about:blank".into(), 0));
                let navigation_failure = request["method"] == "Page.navigate"
                    && request["params"]["url"]
                        .as_str()
                        .is_some_and(|url| url.ends_with("/provider-failure"));
                let result = match request["method"].as_str().unwrap() {
                    "Target.getTargets" => {
                        json!({"targetInfos":data.pages.iter().map(|(id,(url,_))|json!({"targetId":id,"type":"page","url":url,"title":"Owned"})).collect::<Vec<_>>()})
                    }
                    "Target.getTargetInfo" => {
                        json!({"targetInfo":{"targetId":tab,"type":"page","url":page.0,"title":"Owned"}})
                    }
                    "Target.attachToTarget" => json!({"sessionId":tab}),
                    "Target.closeTarget" => {
                        data.pages.remove(&tab);
                        json!({"success":true})
                    }
                    "Page.getFrameTree" => {
                        json!({"frameTree":{"frame":{"id":format!("frame-{tab}"),"loaderId":format!("loader-{}",page.1),"url":page.0}}})
                    }
                    "Page.createIsolatedWorld" => json!({"executionContextId":1}),
                    "Runtime.evaluate" => {
                        let expression = request["params"]["expression"].as_str().unwrap_or("");
                        if expression == "({url:location.href,timeOrigin:performance.timeOrigin})" {
                            data.context_reads += 1;
                            if data.replace_after_context_reads == Some(data.context_reads) {
                                data.pages.insert(tab.clone(), (page.0.clone(), page.1 + 1));
                            }
                            json!({"result":{"value":{"url":page.0,"timeOrigin":page.1}}})
                        } else {
                            json!({"result":{"value":1}})
                        }
                    }
                    "Page.navigate" if navigation_failure => json!({}),
                    "Page.navigate" => {
                        data.pages.insert(
                            tab.clone(),
                            (
                                request["params"]["url"].as_str().unwrap().into(),
                                page.1 + 1,
                            ),
                        );
                        json!({"frameId":format!("frame-{tab}"),"loaderId":format!("loader-{}",page.1+1)})
                    }
                    _ => json!({}),
                };
                drop(data);
                if ws
                    .send(Message::text(
                        if navigation_failure { json!({"id":request["id"],"error":{"code":-32000,"message":"Owned fixture navigation failure"}}) } else { json!({"id":request["id"],"result":result}) }.to_string(),
                    ))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            endpoint,
            state,
            stop,
            socket,
            join: Some(join),
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "Owned fixture requests: {}",
                json!(
                    self.state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .requests
                )
            );
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(s) = self.socket.lock().unwrap().as_ref() {
            let _ = s.shutdown(Shutdown::Both);
        }
        if let Some(j) = self.join.take() {
            j.join().unwrap();
        }
    }
}

fn engine(provider: &Provider, preapproved: bool) -> Engine {
    let mut engine = Engine::new(Box::new(Fixture::default()));
    engine
        .browsers
        .register("owned", &provider.endpoint)
        .unwrap();
    engine.security = Security::new(SecurityConfig {
        require_origin_approval: true,
        preapproved_origin_access: if preapproved {
            vec!["https://start.example".into()]
        } else {
            vec![]
        },
        ..Default::default()
    })
    .unwrap();
    engine
}
fn approval(engine: &mut Engine) -> (OriginElicitationBroker, mpsc::Receiver<Value>) {
    let (send, receive) = mpsc::channel();
    let broker = OriginElicitationBroker::new(
        Arc::new(move |value, _| {
            send.send(value.clone()).unwrap();
            let (send, receive) = mpsc::channel();
            send.send(Ok(())).unwrap();
            Ok(receive)
        }),
        Arc::new(|_, _| false),
        None,
    );
    engine.set_origin_approval(Some(broker.activate(Context {
        request_id: json!(1),
        mcp_initialized: true,
        elicitation_supported: true,
        elicitation_timeout: Duration::from_secs(5),
    })));
    engine.begin_chooser_cell(1);
    (broker, receive)
}
fn control(model: Model) -> ProviderControl {
    ProviderControl::new_with_activation_model(|_| Ok(()), None, model)
}
fn incompatible() -> Model {
    Model::from_task_metadata(r#"{"x-codex-turn-metadata":{"model":"GPT-6-LUNA"}}"#)
}
fn start(engine: &mut Engine, control: &ProviderControl, args: Value) -> Result<Value> {
    engine.execute_from_js_controlled(
        "browser.origin_operation_start",
        &json!({
            "method":"webmcp_list", "args":args,
            "activation_model":{"kind":"compatible"}, "model":"gpt-astra",
        }),
        control,
    )
}
fn no_webmcp(provider: &Provider) {
    for request in &provider.state.lock().unwrap().requests {
        assert!(
            !request["method"].as_str().unwrap().starts_with("WebMCP."),
            "{request}"
        );
        if request["method"] == "Runtime.evaluate" {
            assert_eq!(
                request["params"]["expression"],
                "({url:location.href,timeOrigin:performance.timeOrigin})",
                "{request}"
            );
        }
    }
}

#[test]
fn native_origin_immediate_preapproved_and_exempt_preserve_restrictive_model() {
    for mode in [
        "no-ticket-preapproved",
        "ticket-preapproved",
        "ticket-exempt",
    ] {
        let provider = Provider::start();
        let mut engine = engine(&provider, true);
        let broker = if mode.starts_with("ticket") {
            Some(approval(&mut engine))
        } else {
            None
        };
        let control = control(incompatible());
        let args = if mode == "ticket-exempt" {
            json!({"browser":"owned"})
        } else {
            json!({"browser":"owned","tab":"t"})
        };
        let error = start(&mut engine, &control, args).unwrap_err();
        assert_eq!(
            error.message, "gpt-6-luna does not support command \"webmcp_list_tools\".",
            "{mode}"
        );
        assert!(engine.navigation_security.operations.is_empty());
        assert!(engine.navigation_security.queues.is_empty());
        if let Some((_, receive)) = broker {
            assert!(receive.try_recv().is_err());
        }
        no_webmcp(&provider);
    }
    let provider = Provider::start();
    let mut engine = engine(&provider, false);
    let error = start(
        &mut engine,
        &control(incompatible()),
        json!({"browser":"owned","tab":"t"}),
    )
    .unwrap_err();
    assert_eq!(
        error.code, -32011,
        "missing approval precedes model refusal"
    );
    no_webmcp(&provider);
    let before = provider.state.lock().unwrap().requests.len();
    for args in [
        json!({}),
        json!({"browser":"owned","tab":"t","name":"echo","registrationId":"stale"}),
    ] {
        let error = engine
            .execute_from_js_controlled(
                "browser.origin_operation_start",
                &json!({"method":"webmcp_invoke","args":args}),
                &control(Model::Unavailable {
                    reason: Unavailable::ModelTooLarge,
                }),
            )
            .unwrap_err();
        assert_eq!(
            error.code, -32010,
            "operation command policy precedes metadata: {error:?}"
        );
    }
    assert_eq!(provider.state.lock().unwrap().requests.len(), before);
}

#[test]
fn native_origin_pending_model_survives_call_retirement_and_cannot_be_replaced_by_poll() {
    for (model, message) in [
        (
            incompatible(),
            "gpt-6-luna does not support command \"webmcp_list_tools\".",
        ),
        (
            Model::Unavailable {
                reason: Unavailable::ModelTooLarge,
            },
            "Native WebMCP model projection exceeds limit",
        ),
    ] {
        let provider = Provider::start();
        let mut engine = engine(&provider, false);
        let (broker, receive) = approval(&mut engine);
        let originating = control(model.clone());
        let pending = start(
            &mut engine,
            &originating,
            json!({"browser":"owned","tab":"t"}),
        )
        .unwrap();
        assert_eq!(pending["pending"], true);
        let id = pending["id"].as_str().unwrap();
        assert_eq!(
            engine.navigation_security.operations[id].activation_model,
            Some(model)
        );
        let prompt = receive.recv_timeout(Duration::from_secs(1)).unwrap();
        originating.lifetime().finish().unwrap();
        assert!(!originating.is_active());
        let polling = control(Model::Compatible);
        let poll = json!({"id":id,"activation_model":{"kind":"compatible"}});
        assert_eq!(
            engine
                .execute_from_js_controlled("browser.origin_operation_poll", &poll, &polling)
                .unwrap()["pending"],
            true
        );
        broker.route(&json!({"jsonrpc":"2.0","id":prompt["id"],"result":{"action":"accept"}}));
        assert_eq!(
            engine
                .execute_from_js_controlled("browser.origin_operation_poll", &poll, &polling)
                .unwrap_err()
                .message,
            message
        );
        assert!(engine.navigation_security.operations.is_empty());
        assert!(engine.navigation_security.queues.is_empty());
        let repeated = engine
            .execute_from_js_controlled("browser.origin_operation_poll", &poll, &polling)
            .unwrap_err();
        assert_eq!(repeated.code, -32800);
        no_webmcp(&provider);
    }
}

#[test]
fn native_origin_document_change_and_cell_retirement_precede_model_refusal() {
    for cancel in [false, true] {
        let provider = Provider::start();
        let mut engine = engine(&provider, false);
        let (broker, receive) = approval(&mut engine);
        let originating = control(incompatible());
        let pending = start(
            &mut engine,
            &originating,
            json!({"browser":"owned","tab":"t"}),
        )
        .unwrap();
        let prompt = receive.recv_timeout(Duration::from_secs(1)).unwrap();
        originating.lifetime().finish().unwrap();
        if cancel {
            engine.finish_chooser_cell("initial", 1, true);
        } else {
            provider.state.lock().unwrap().pages.get_mut("t").unwrap().1 += 1;
        }
        broker.route(&json!({"jsonrpc":"2.0","id":prompt["id"],"result":{"action":"accept"}}));
        let before = provider.state.lock().unwrap().requests.len();
        let error = engine
            .execute_from_js_controlled(
                "browser.origin_operation_poll",
                &json!({"id":pending["id"]}),
                &control(Model::Compatible),
            )
            .unwrap_err();
        assert_eq!(
            error.code,
            if cancel { -32800 } else { -32014 },
            "{error:?}"
        );
        if cancel {
            assert_eq!(provider.state.lock().unwrap().requests.len(), before);
        }
        assert!(!error.message.contains("does not support"));
        assert!(engine.navigation_security.operations.is_empty());
        no_webmcp(&provider);
    }
}

#[test]
fn native_origin_model_retention_is_bounded_and_absent_from_ordinary_operations() {
    let provider = Provider::start();
    let mut engine = engine(&provider, false);
    let (_broker, receive) = approval(&mut engine);
    let model = Model::from_task_metadata(
        &json!({"x-codex-turn-metadata":{"model":"x".repeat(65530)+"-LUNA"}}).to_string(),
    );
    assert!(matches!(model, Model::Incompatible { .. }));
    let control = control(model);
    let mut accepted = 0;
    loop {
        match start(&mut engine, &control, json!({"browser":"owned","tab":"t"})) {
            Ok(pending) => {
                assert_eq!(pending["pending"], true);
                accepted += 1;
                assert!(accepted < 128);
            }
            Err(error) => {
                assert!(error.message.contains("4 MiB"), "{error:?}");
                break;
            }
        }
    }
    assert!(accepted > 1);
    let retained: usize = engine
        .navigation_security
        .operations
        .values()
        .map(|operation| {
            retained_bytes(&operation.args, operation.activation_model.as_ref()).unwrap()
        })
        .sum();
    assert!(retained <= 4 * 1024 * 1024);
    assert!(receive.recv_timeout(Duration::from_secs(1)).is_ok());
    assert!(receive.try_recv().is_err());
    engine.finish_chooser_cell("initial", 1, true);
    assert!(engine.navigation_security.operations.is_empty());
    let (_broker, _receive) = approval(&mut engine);
    let ordinary = engine.execute_from_js_controlled("browser.origin_operation_start", &json!({"method":"navigate","args":{"browser":"owned","tab":"t","url":"https://dest.example/"}}), &control).unwrap();
    assert!(
        engine.navigation_security.operations[ordinary["id"].as_str().unwrap()]
            .activation_model
            .is_none()
    );
    engine.finish_chooser_cell("initial", 1, true);
    no_webmcp(&provider);
}
