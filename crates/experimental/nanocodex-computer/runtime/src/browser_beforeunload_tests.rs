//! Authored native state/loopback contracts; never an original-runtime surrogate.
use super::*;
use crate::{
    browser::CallAdmissions,
    download_elicitation::Context,
    origin_elicitation::{OriginElicitationBroker, Ticket},
    runtime::ProviderControl,
};
use serde_json::json;
use std::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    thread,
};
use tungstenite::{Message, WebSocket};

struct Native {
    control: ProviderControl,
    broker: OriginElicitationBroker,
    ticket: Ticket,
    output: Arc<AtomicBool>,
    execution: Arc<AtomicBool>,
}
impl Native {
    fn new() -> Self {
        let output = Arc::new(AtomicBool::new(true));
        let healthy = output.clone();
        let execution = Arc::new(AtomicBool::new(true));
        let running = execution.clone();
        let control = ProviderControl::new_with_execution_check(
            |_| panic!("Automatic dialog handling must never suspend a clock"),
            Some(Arc::new(move || {
                running
                    .load(Ordering::Acquire)
                    .then_some(())
                    .ok_or_else(|| Error::action("Owned execution deadline/cancellation"))
            })),
        );
        let broker = OriginElicitationBroker::new(
            Arc::new(|_, _| panic!("A native liveness check must not request approval")),
            Arc::new(|_, _| false),
            None,
        )
        .with_output_health(Arc::new(move || {
            healthy
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| Error::action("Owned output failure"))
        }));
        let ticket = broker.activate(Context {
            request_id: json!(1),
            mcp_initialized: true,
            elicitation_supported: false,
            elicitation_timeout: Duration::from_secs(1),
        });
        Self {
            control,
            broker,
            ticket,
            output,
            execution,
        }
    }
    fn admission(&self, kind: Kind) -> RuntimeAdmission {
        RuntimeAdmission::new(
            "owned",
            "tab",
            kind,
            self.control.execution_validity().unwrap(),
            self.ticket.connection_liveness().unwrap(),
        )
        .unwrap()
    }
}
fn opening(kind: &str) -> Value {
    json!({"method":"Page.javascriptDialogOpening","sessionId":"root","params":{"type":kind,"message":"owned"}})
}
fn context(dialogs: &dialog::State, event: &Value) -> raw_events::Context {
    dialogs.raw_event_context(event)
}
fn remember(dialogs: &mut dialog::State, event: &Value) -> Option<Fresh> {
    let context = context(dialogs, event);
    let previous = dialogs
        .get("tab")
        .map(|d| d["id"].as_str().unwrap().to_owned());
    dialogs.event(event);
    State::fresh(dialogs, &context, event, previous.as_deref())
}
fn roots() -> (dialog::State, raw_events::Log) {
    let mut dialogs = dialog::State::default();
    dialogs.root("tab", "root");
    let mut raw = raw_events::Log::default();
    raw.attach("tab", "root").unwrap();
    (dialogs, raw)
}

#[test]
fn beforeunload_fresh_identity_is_required_and_ack_metadata_never_deletes_it() {
    let native = Native::new();
    let runtime = native.admission(Kind::Navigate);
    let mut state = State::default();
    let (mut dialogs, raw) = roots();
    let operation = state
        .bind(&runtime, raw.attachment("tab").unwrap().0, "root".into())
        .unwrap();
    let event = opening("beforeunload");
    let fresh = remember(&mut dialogs, &event).unwrap();
    assert!(state.valid(&operation, &fresh, &dialogs, raw.attachment("tab")));
    assert!(
        State::fresh(
            &dialogs,
            &context(&dialogs, &event),
            &event,
            Some(&fresh.id)
        )
        .is_none()
    );
    let retained = dialogs.get("tab").unwrap().clone();
    for response in [
        json!({"result":{}}),
        json!({"error":{"message":"owned refusal"}}),
    ] {
        state.pending.insert(
            9,
            Pending {
                session: "root".into(),
                expires: Instant::now() + ACK_LIFETIME,
            },
        );
        let mut wrong = response.clone();
        wrong["id"] = json!(9);
        wrong["sessionId"] = json!("child");
        assert!(state.discard_reply(&wrong));
        assert!(state.pending.contains_key(&9));
        wrong["sessionId"] = json!("root");
        assert!(state.discard_reply(&wrong));
        assert!(!state.pending.contains_key(&9));
        assert_eq!(dialogs.get("tab"), Some(&retained));
    }
    state.pending.insert(
        10,
        Pending {
            session: "root".into(),
            expires: Instant::now() - Duration::from_millis(1),
        },
    );
    assert!(!state.discard_reply(&json!({"id":10,"sessionId":"root","result":{}})));
    assert_eq!(dialogs.get("tab"), Some(&retained));
    let replacement = remember(&mut dialogs, &event).unwrap();
    assert!(!state.valid(&operation, &fresh, &dialogs, raw.attachment("tab")));
    assert!(state.valid(&operation, &replacement, &dialogs, raw.attachment("tab")));
    dialogs.event(&json!({"method":"Page.javascriptDialogClosed","sessionId":"root","params":{}}));
    assert!(dialogs.get("tab").is_none());
}

#[test]
fn beforeunload_observed_root_replacement_revokes_without_erasing_dialog() {
    for event in [
        json!({"method":"Page.frameNavigated","sessionId":"root","params":{"frame":{"id":"tab","parentId":null}}}),
        json!({"method":"Runtime.executionContextsCleared","sessionId":"root","params":{}}),
        json!({"method":"Runtime.executionContextCreated","sessionId":"root","params":{}}),
        json!({"method":"Runtime.executionContextDestroyed","sessionId":"root","params":{}}),
        json!({"method":"Page.frameDetached","sessionId":"root","params":{"frameId":"child-frame"}}),
        json!({"method":"Target.detachedFromTarget","params":{"sessionId":"root"}}),
    ] {
        let native = Native::new();
        let runtime = native.admission(Kind::Navigate);
        let mut state = State::default();
        let (mut dialogs, raw) = roots();
        let op = state
            .bind(&runtime, raw.attachment("tab").unwrap().0, "root".into())
            .unwrap();
        let fresh = remember(&mut dialogs, &opening("beforeunload")).unwrap();
        let record = dialogs.get("tab").unwrap().clone();
        state.observe(&context(&dialogs, &event), &event);
        assert!(
            !state.valid(&op, &fresh, &dialogs, raw.attachment("tab")),
            "{event}"
        );
        assert_eq!(
            dialogs.get("tab"),
            Some(&record),
            "Revocation is not dialog deletion"
        );
    }
}

#[test]
fn beforeunload_attachment_connection_and_operation_lifetimes_do_not_revive() {
    let native = Native::new();
    let runtime = native.admission(Kind::Navigate);
    let mut state = State::default();
    let (mut dialogs, mut raw) = roots();
    let op = state
        .bind(&runtime, raw.attachment("tab").unwrap().0, "root".into())
        .unwrap();
    let fresh = remember(&mut dialogs, &opening("beforeunload")).unwrap();
    let child = json!({"method":"Page.frameNavigated","sessionId":"unowned-child","params":{"frame":{"id":"child"}}});
    state.observe(&context(&dialogs, &child), &child);
    assert!(state.valid(&op, &fresh, &dialogs, raw.attachment("tab")));
    raw.attach("tab", "root").unwrap();
    assert!(
        !state.valid(&op, &fresh, &dialogs, raw.attachment("tab")),
        "Same session text does not restore generation"
    );
    assert!(!State::default().valid(&op, &fresh, &dialogs, Some((op.generation, "root".into()))));
    let watcher = state.watchers[0].upgrade().unwrap();
    drop(op);
    assert!(!watcher.live.load(Ordering::Acquire));
    state.prune(Instant::now());
    assert!(state.watchers.is_empty());
}

#[test]
fn beforeunload_native_probes_and_bounded_metadata_refuse_without_clock_credit() {
    let native = Native::new();
    let runtime = native.admission(Kind::Navigate);
    for flag in [&native.execution, &native.output] {
        flag.store(false, Ordering::Release);
        assert!(runtime.validate().is_err());
        assert!(State::default().bind(&runtime, 1, "root".into()).is_none());
        flag.store(true, Ordering::Release);
    }
    let mut state = State::default();
    let now = Instant::now();
    for id in 0..MAX_PENDING as u64 {
        state.pending.insert(
            id,
            Pending {
                session: "root".into(),
                expires: now + ACK_LIFETIME,
            },
        );
    }
    assert!(!state.room("root", now));
    assert!(state.room("root", now + ACK_LIFETIME));
    assert!(state.pending.is_empty());
    // Byte refusal is independent of the count refusal, including defensive malformed-state bounds.
    state.pending.insert(
        0,
        Pending {
            session: "x".repeat(MAX_BYTES),
            expires: now + ACK_LIFETIME,
        },
    );
    assert!(!state.room("root", now));
    assert!(state.bind(&runtime, 1, "x".repeat(MAX_ID + 1)).is_none());
    native.broker.disconnect();
    assert!(runtime.validate().is_err());
}

#[test]
fn beforeunload_close_progress_keeps_original_deadline_and_navigate_does_not() {
    let native = Native::new();
    for kind in [Kind::Navigate, Kind::Close] {
        let runtime = native.admission(kind);
        let mut state = State::default();
        let op = state.bind(&runtime, 1, "root".into()).unwrap();
        assert!(!op.allows_command("Target.getTargets", &json!({}), None));
        let method = if kind == Kind::Navigate {
            "Page.navigate"
        } else {
            "Target.closeTarget"
        };
        let params = json!({"targetId":"tab"});
        let session = (kind == Kind::Navigate).then_some("root");
        assert!(op.allows_command(method, &params, session));
        assert!(!op.allows_command("Runtime.evaluate", &json!({}), session));
        let first = Instant::now() + Duration::from_secs(1);
        op.dispatched(method, first);
        assert!(!op.allows_command(method, &params, session));
        assert_eq!(
            op.allows_command("Target.getTargets", &json!({}), None),
            kind == Kind::Close
        );
        op.dispatched(method, first + Duration::from_secs(10));
        assert_eq!(op.started_deadline.get(), Some(first));
    }
}

#[test]
fn beforeunload_engine_admission_needs_active_cell_ticket_and_unrestricted_method() {
    use crate::{
        engine::Engine,
        fixture::Fixture,
        security::{Security, SecurityConfig},
    };
    let native = Native::new();
    let mut engine = Engine::new(Box::new(Fixture::default()));
    let args = json!({"browser":"owned","tab":"tab","url":"about:blank"});
    assert!(
        engine
            .beforeunload_runtime_admission("navigate", &args, &native.control)
            .is_none()
    );
    engine.begin_chooser_cell(1);
    engine.set_origin_approval(Some(native.ticket.clone()));
    assert!(
        engine
            .beforeunload_runtime_admission("navigate", &args, &native.control)
            .is_some()
    );
    assert!(
        engine
            .beforeunload_runtime_admission("close_tab", &args, &native.control)
            .is_some()
    );
    for method in [
        "cdp_call",
        "new_tab",
        "origin_operation_start",
        "get_tab",
        "webmcp_invoke",
    ] {
        assert!(
            engine
                .beforeunload_runtime_admission(method, &args, &native.control)
                .is_none(),
            "{method}"
        );
    }
    let mut child = args.clone();
    child["frame"] = json!("child");
    assert!(
        engine
            .beforeunload_runtime_admission("navigate", &child, &native.control)
            .is_none()
    );
    engine.security = Security::new(SecurityConfig {
        require_origin_approval: true,
        ..Default::default()
    })
    .unwrap();
    assert!(
        engine
            .beforeunload_runtime_admission("navigate", &args, &native.control)
            .is_none()
    );
    engine.security = Security::default();
    let scope = engine.selected_kernel_scope().to_owned();
    engine.finish_chooser_cell(&scope, 1, false);
    assert!(
        engine
            .beforeunload_runtime_admission("navigate", &args, &native.control)
            .is_none()
    );
}

fn read(ws: &mut WebSocket<TcpStream>) -> Value {
    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap()
}
fn send(ws: &mut WebSocket<TcpStream>, value: Value) {
    ws.send(Message::text(value.to_string())).unwrap();
}
fn wire(
    f: impl FnOnce(&mut WebSocket<TcpStream>) + Send + 'static,
) -> (Cdp, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let join = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        f(&mut ws);
    });
    let client = Cdp::connect(&endpoint).unwrap();
    (client, join)
}
fn attach(client: &mut Cdp) {
    client.dialogs.lock().unwrap().root("tab", "root");
    client
        .raw_events
        .lock()
        .unwrap()
        .attach("tab", "root")
        .unwrap();
}
fn bind<'a>(client: &mut Cdp, runtime: &'a RuntimeAdmission) -> Operation<'a> {
    let (generation, session) = client.raw_events.lock().unwrap().attachment("tab").unwrap();
    client
        .beforeunload
        .bind(runtime, generation, session)
        .unwrap()
}
fn automatic<'a, 'runtime>(op: &'a Operation<'runtime>) -> CallAdmissions<'a, 'runtime> {
    CallAdmissions {
        beforeunload: Some(op),
        ..Default::default()
    }
}

#[test]
fn beforeunload_navigation_sends_once_without_waiting_for_ack_and_retains_record() {
    let (poll_ready, poll_wait) = mpsc::sync_channel(1);
    let (mut client, join) = wire(move |ws| {
        let primary = read(ws);
        assert_eq!(primary["method"], "Page.navigate");
        send(ws, opening("beforeunload"));
        let auto = read(ws);
        assert_eq!(
            auto,
            json!({"id":2,"sessionId":"root","method":"Page.handleJavaScriptDialog","params":{"accept":true}})
        );
        // There is no ACK yet. A synchronous nested wait would deadlock before this result is returned.
        send(ws, json!({"id":primary["id"],"result":{"frameId":"tab"}}));
        let next = read(ws);
        assert_eq!(next["method"], "Target.getTargets");
        send(
            ws,
            json!({"id":auto["id"],"sessionId":"wrong","error":{"message":"wrong session"}}),
        );
        send(
            ws,
            json!({"id":auto["id"],"sessionId":"root","error":{"message":"owned refusal"}}),
        );
        send(ws, json!({"id":next["id"],"result":{"targetInfos":[]}}));
        // The following opening is read only by passive polling, after the operation has ended.
        send(ws, opening("beforeunload"));
        send(ws, json!({"id":auto["id"],"sessionId":"root","result":{}}));
        poll_ready.send(()).unwrap();
        let last = read(ws);
        assert_eq!(
            last["method"], "Target.getTargets",
            "No background automatic command"
        );
        send(ws, json!({"id":last["id"],"result":{}}));
    });
    attach(&mut client);
    let native = Native::new();
    let runtime = native.admission(Kind::Navigate);
    let op = bind(&mut client, &runtime);
    client
        .call_scoped(
            "Page.navigate",
            json!({"url":"about:blank"}),
            Some("root"),
            automatic(&op),
        )
        .unwrap();
    assert!(client.dialogs.lock().unwrap().get("tab").is_some());
    assert_eq!(client.beforeunload.pending.len(), 1);
    drop(op);
    client.call("Target.getTargets", json!({}), None).unwrap();
    assert!(client.dialogs.lock().unwrap().get("tab").is_some());
    assert!(client.beforeunload.pending.is_empty());
    poll_wait.recv_timeout(Duration::from_secs(3)).unwrap();
    client.poll_events().unwrap();
    assert!(client.dialogs.lock().unwrap().get("tab").is_some());
    client.call("Target.getTargets", json!({}), None).unwrap();
    drop(client);
    join.join().unwrap();
}

#[test]
fn beforeunload_close_ack_can_precede_opening_in_the_existing_completion_poll() {
    let (mut client, join) = wire(|ws| {
        let close = read(ws);
        assert_eq!(close["method"], "Target.closeTarget");
        send(ws, json!({"id":close["id"],"result":{"success":true}}));
        let poll = read(ws);
        assert_eq!(poll["method"], "Target.getTargets");
        send(ws, opening("beforeunload"));
        let auto = read(ws);
        assert_eq!(auto["method"], "Page.handleJavaScriptDialog");
        assert_eq!(auto["params"], json!({"accept":true}));
        send(ws, json!({"id":auto["id"],"sessionId":"root","result":{}}));
        send(
            ws,
            json!({"method":"Page.javascriptDialogClosed","sessionId":"root","params":{}}),
        );
        send(ws, json!({"id":poll["id"],"result":{"targetInfos":[]}}));
    });
    attach(&mut client);
    let native = Native::new();
    let runtime = native.admission(Kind::Close);
    let op = bind(&mut client, &runtime);
    client
        .call_scoped(
            "Target.closeTarget",
            json!({"targetId":"tab"}),
            None,
            automatic(&op),
        )
        .unwrap();
    let deadline = op.started_deadline.get();
    client
        .call_scoped("Target.getTargets", json!({}), None, automatic(&op))
        .unwrap();
    assert_eq!(op.started_deadline.get(), deadline);
    assert!(client.dialogs.lock().unwrap().get("tab").is_none());
    assert!(client.beforeunload.pending.is_empty());
    drop(op);
    drop(client);
    join.join().unwrap();
}

#[test]
fn beforeunload_loopback_refuses_replacement_cancellation_and_unadmitted_calls() {
    for refusal in [
        "document",
        "output",
        "execution",
        "connection",
        "uncontrolled",
        "general",
        "expired",
    ] {
        let native = Native::new();
        let output = native.output.clone();
        let execution = native.execution.clone();
        let broker = native.broker.clone();
        let (mut client, join) = wire(move |ws| {
            let primary = read(ws);
            match refusal {
                "document" => send(
                    ws,
                    json!({"method":"Runtime.executionContextsCleared","sessionId":"root","params":{}}),
                ),
                "output" => output.store(false, Ordering::Release),
                "execution" => execution.store(false, Ordering::Release),
                "connection" => broker.disconnect(),
                _ => {}
            }
            send(ws, opening("beforeunload"));
            send(ws, json!({"id":primary["id"],"result":{}}));
            let next = read(ws);
            assert_eq!(
                next["method"], "Target.getTargets",
                "Unexpected automatic send for {refusal}"
            );
            send(ws, json!({"id":next["id"],"result":{}}));
        });
        attach(&mut client);
        let runtime = native.admission(Kind::Navigate);
        let op = bind(&mut client, &runtime);
        if refusal == "expired" {
            op.started_deadline
                .set(Some(Instant::now() - Duration::from_millis(1)));
        }
        let method = if refusal == "general" {
            "Target.getTargets"
        } else {
            "Page.navigate"
        };
        let session = (method == "Page.navigate").then_some("root");
        let admissions = if refusal == "uncontrolled" {
            CallAdmissions::default()
        } else {
            automatic(&op)
        };
        client
            .call_scoped(method, json!({}), session, admissions)
            .unwrap();
        assert!(client.dialogs.lock().unwrap().get("tab").is_some());
        assert!(client.beforeunload.pending.is_empty());
        drop(op);
        client.call("Target.getTargets", json!({}), None).unwrap();
        drop(client);
        join.join().unwrap();
    }
}

#[test]
fn beforeunload_pending_ack_disposal_works_in_passive_polling_without_record_deletion() {
    for error in [false, true] {
        let (ready, wait) = mpsc::sync_channel(1);
        let (mut client, join) = wire(move |ws| {
            let primary = read(ws);
            send(ws, opening("beforeunload"));
            let auto = read(ws);
            assert_eq!(auto["method"], "Page.handleJavaScriptDialog");
            send(ws, json!({"id":primary["id"],"result":{}}));
            send(ws, json!({"id":auto["id"],"sessionId":"child","result":{}}));
            let mut ack = json!({"id":auto["id"],"sessionId":"root"});
            ack[if error { "error" } else { "result" }] = if error {
                json!({"message":"owned failure"})
            } else {
                json!({})
            };
            send(ws, ack);
            ready.send(()).unwrap();
            let next = read(ws);
            assert_eq!(next["method"], "Target.getTargets");
            send(ws, json!({"id":next["id"],"result":{}}));
        });
        attach(&mut client);
        let native = Native::new();
        let runtime = native.admission(Kind::Navigate);
        let op = bind(&mut client, &runtime);
        client
            .call_scoped("Page.navigate", json!({}), Some("root"), automatic(&op))
            .unwrap();
        let record = client.dialogs.lock().unwrap().get("tab").unwrap().clone();
        drop(op);
        assert_eq!(client.beforeunload.pending.len(), 1);
        let expires = client.beforeunload.pending.values().next().unwrap().expires;
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
        // Peer writes may arrive after an idle passive poll. Keep the original
        // expiry as the bound so metadata pruning cannot satisfy ACK disposal.
        loop {
            assert!(
                Instant::now() < expires,
                "ACK was not observed before expiry"
            );
            client.poll_events().unwrap();
            assert!(
                Instant::now() < expires,
                "ACK disposal reached metadata expiry"
            );
            assert_eq!(client.dialogs.lock().unwrap().get("tab"), Some(&record));
            if client.beforeunload.pending.is_empty() {
                break;
            }
        }
        client.call("Target.getTargets", json!({}), None).unwrap();
        drop(client);
        join.join().unwrap();
    }
}

#[test]
fn beforeunload_sender_refuses_expired_full_or_stale_same_receipt_intent() {
    for refusal in ["expired", "full", "replacement", "provider-ended"] {
        let (mut client, join) = wire(move |ws| {
            let next = read(ws);
            assert_eq!(
                next["method"], "Target.getTargets",
                "No automatic send for {refusal}"
            );
            send(ws, json!({"id":next["id"],"result":{}}));
        });
        attach(&mut client);
        let native = Native::new();
        let runtime = native.admission(Kind::Navigate);
        let op = bind(&mut client, &runtime);
        let now = Instant::now();
        let deadline = if refusal == "expired" {
            now - Duration::from_millis(1)
        } else {
            now + Duration::from_secs(1)
        };
        op.dispatched("Page.navigate", deadline);
        let fresh = remember(
            &mut client.dialogs.lock().unwrap(),
            &opening("beforeunload"),
        )
        .unwrap();
        match refusal {
            "full" => {
                for id in 100..100 + MAX_PENDING as u64 {
                    client.beforeunload.pending.insert(
                        id,
                        Pending {
                            session: "root".into(),
                            expires: now + ACK_LIFETIME,
                        },
                    );
                }
            }
            "replacement" => {
                remember(
                    &mut client.dialogs.lock().unwrap(),
                    &opening("beforeunload"),
                )
                .unwrap();
            }
            "provider-ended" => native.control.close().unwrap(),
            _ => {}
        }
        client
            .send_beforeunload(&op, fresh, now + Duration::from_secs(10))
            .unwrap();
        drop(op);
        client.call("Target.getTargets", json!({}), None).unwrap();
        drop(client);
        join.join().unwrap();
    }
}

#[test]
fn beforeunload_only_a_fresh_root_beforeunload_record_can_form_an_intent() {
    let (mut dialogs, _) = roots();
    for kind in ["alert", "confirm", "prompt", "unknown"] {
        assert!(remember(&mut dialogs, &opening(kind)).is_none(), "{kind}");
    }
    let mut other = opening("beforeunload");
    other["sessionId"] = json!("untracked");
    assert!(remember(&mut dialogs, &other).is_none());
    dialogs.event(&json!({"method":"Target.attachedToTarget","sessionId":"root","params":{"sessionId":"child","targetInfo":{"targetId":"child-target","type":"iframe"}}}));
    other["sessionId"] = json!("child");
    assert!(remember(&mut dialogs, &other).is_none());
    assert!(
        dialogs.beforeunload_id("tab", "root").is_none(),
        "A child record is not a root dialog"
    );
    assert!(remember(&mut dialogs, &opening("beforeunload")).is_some());
}

#[test]
fn beforeunload_browser_binding_requires_exact_existing_plain_root_owner() {
    let (mut client, join) = wire(|ws| {
        let next = read(ws);
        assert_eq!(next["method"], "Target.getTargets");
        send(ws, json!({"id":next["id"],"result":{}}));
    });
    attach(&mut client);
    let native = Native::new();
    let runtime = native.admission(Kind::Navigate);
    let mut browsers = crate::browser::Browsers::default();
    // Registration is metadata only. The sole live client is the owned loopback connection above.
    browsers.register("owned", "ws://127.0.0.1:1").unwrap();
    let browser = browsers.providers.get_mut("owned").unwrap();
    browser.surface.dialogs = client.dialogs.clone();
    browser.surface.raw_events = client.raw_events.clone();
    browser.sessions.insert("tab".into(), "root".into());
    browser.client = Some(client);
    assert!(
        browser
            .beforeunload_operation("owned", "tab", &runtime)
            .is_some()
    );
    assert!(
        browser
            .beforeunload_operation("different", "tab", &runtime)
            .is_none()
    );
    assert!(
        browser
            .beforeunload_operation("owned", "different", &runtime)
            .is_none()
    );
    browser.extension = true;
    assert!(
        browser
            .beforeunload_operation("owned", "tab", &runtime)
            .is_none()
    );
    browser.extension = false;
    browser.invalidated = true;
    assert!(
        browser
            .beforeunload_operation("owned", "tab", &runtime)
            .is_none()
    );
    browser.invalidated = false;
    browser.sessions.remove("tab");
    assert!(
        browser
            .beforeunload_operation("owned", "tab", &runtime)
            .is_none()
    );
    let mut client = browser.client.take().unwrap();
    assert!(
        browser
            .beforeunload_operation("owned", "tab", &runtime)
            .is_none()
    );
    client.call("Target.getTargets", json!({}), None).unwrap();
    drop(client);
    join.join().unwrap();
}
