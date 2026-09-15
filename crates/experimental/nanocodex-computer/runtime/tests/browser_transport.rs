use serde_json::{Value, json};
use skyre::{
    browser::{Browsers, Cdp},
    keys::cdp_key,
};
use std::{net::TcpListener, thread, time::Duration};
use tungstenite::Message;
fn server(script: Vec<(&'static str, Value)>) -> (String, thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let join = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        let mut received = vec![];
        for (method, result) in script {
            let message = loop {
                let m = ws.read().unwrap();
                if m.is_text() {
                    break m;
                }
            };
            let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(request["method"], method);
            let mut response = json!({"id":request["id"]});
            if result.get("__error").is_some() {
                response["error"] = result["__error"].clone();
            } else {
                response["result"] = result;
            }
            received.push(request);
            ws.send(Message::text(response.to_string())).unwrap();
        }
        received
    });
    (format!("ws://{address}"), join)
}
fn ax() -> Value {
    json!({"nodes":[{"nodeId":"root","role":{"value":"RootWebArea"},"name":{"value":"Owned fixture"},"childIds":["field"]},{"nodeId":"field","parentId":"root","role":{"value":"textbox"},"name":{"value":"Input"},"value":{"value":"red alpha blue alpha green"},"backendDOMNodeId":42,"properties":[{"name":"editable","value":{"value":"plaintext"}}]}]})
}
#[test]
fn browser_selection_releases_object_on_javascript_exception() {
    let (endpoint, join) = server(vec![
        ("Target.attachToTarget", json!({"sessionId":"session"})),
        ("Accessibility.enable", json!({})),
        ("Fetch.enable", json!({})),
        ("Accessibility.getFullAXTree", ax()),
        ("Accessibility.getFullAXTree", ax()),
        ("DOM.resolveNode", json!({"object":{"objectId":"object"}})),
        (
            "Runtime.callFunctionOn",
            json!({"exceptionDetails":{"text":"field detached"}}),
        ),
        ("Runtime.releaseObject", json!({})),
    ]);
    let mut browser = Browsers::default();
    browser.register("owned", &endpoint).unwrap();
    browser.execute("snapshot", &json!({"tab":"tab"})).unwrap();
    let result=browser.execute("select_text",&json!({"tab":"tab","element_index":1,"text":"alpha","prefix":"blue ","selectionType":"cursor_after"}));
    assert!(result.unwrap_err().message.contains("field detached"));
    let requests = join.join().unwrap();
    assert_eq!(
        requests[6]["params"]["arguments"],
        json!([{"value":20},{"value":20}])
    );
    for request in &requests[1..] {
        assert_eq!(request["sessionId"], "session");
    }
}
#[test]
fn key_chords_have_modifiers_and_release_no_text() {
    let (endpoint, join) = server(vec![
        ("Target.attachToTarget", json!({"sessionId":"s"})),
        ("Accessibility.enable", json!({})),
        ("Fetch.enable", json!({})),
        ("Input.dispatchKeyEvent", json!({})),
        ("Input.dispatchKeyEvent", json!({})),
    ]);
    let mut b = Browsers::default();
    b.register("owned", &endpoint).unwrap();
    b.execute("press_key", &json!({"tab":"t","key":"shift+a"}))
        .unwrap();
    let r = join.join().unwrap();
    assert_eq!(r[3]["params"]["key"], "A");
    assert_eq!(r[3]["params"]["code"], "KeyA");
    assert_eq!(r[3]["params"]["modifiers"], 8);
    assert_eq!(r[3]["params"]["text"], "A");
    assert_eq!(r[4]["params"]["type"], "keyUp");
    assert!(r[4]["params"].get("text").is_none());
    assert_eq!(cdp_key("CMD+C").unwrap()["modifiers"], 4);
    assert!(cdp_key("madeup+a").is_err());
    assert!(cdp_key("F13").is_err());
}
#[test]
fn events_are_bounded_and_do_not_resolve_a_pending_call() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let join = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        let req: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
        ws.send(Message::Ping(vec![1, 2, 3].into())).unwrap();
        for i in 0..260 {
            ws.send(Message::text(
                json!({"method":"Page.event","params":{"n":i}}).to_string(),
            ))
            .unwrap();
        }
        ws.send(Message::text(
            json!({"id":900,"result":{"wrong":true}}).to_string(),
        ))
        .unwrap();
        ws.send(Message::text(
            json!({"id":req["id"],"result":{"ok":true}}).to_string(),
        ))
        .unwrap();
        assert!(ws.read().unwrap().is_pong());
    });
    let mut c = Cdp::connect(&format!("ws://{address}")).unwrap();
    assert_eq!(
        c.call("Page.test", json!({}), None).unwrap(),
        json!({"ok":true})
    );
    assert_eq!(c.events.len(), 256);
    assert_eq!(c.events[0]["params"]["n"], 4);
    join.join().unwrap();
}
#[test]
fn disconnect_is_not_replayed_and_next_request_reconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let join = thread::spawn(move || {
        let mut methods = vec![];
        for connection in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let req: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            methods.push(req["method"].clone());
            if connection == 0 {
                ws.close(None).unwrap();
            } else {
                ws.send(Message::text(
                    json!({"id":req["id"],"result":{"targetInfos":[]}}).to_string(),
                ))
                .unwrap();
            }
        }
        methods
    });
    let mut b = Browsers::default();
    b.register("owned", &format!("ws://{address}")).unwrap();
    assert!(b.execute("new_tab", &json!({"url":"about:blank"})).is_err());
    assert_eq!(b.execute("list_tabs", &json!({})).unwrap(), json!([]));
    assert_eq!(
        join.join().unwrap(),
        vec![json!("Target.createTarget"), json!("Target.getTargets")]
    );
}
#[test]
fn unsupported_paste_and_invalid_keys_fail_without_connecting() {
    let mut b = Browsers::default();
    b.register("unconnected", "ws://127.0.0.1:1").unwrap();
    assert_eq!(
        b.execute("paste", &json!({"tab":"t","text":"x","format":"html"}))
            .unwrap_err()
            .code,
        -32601
    );
    assert_eq!(
        b.execute("press_key", &json!({"tab":"t","key":"bogus+a"}))
            .unwrap_err()
            .code,
        -32602
    );
}

#[test]
fn invalid_mouse_options_fail_before_dom_side_effects() {
    let mut b = Browsers::default();
    b.register("owned", "ws://127.0.0.1:1").unwrap();
    for (method, args) in [
        (
            "click",
            json!({"tab":"t","element_index":1,"clickCount":"2"}),
        ),
        (
            "click",
            json!({"tab":"t","element_index":1,"mouseButton":"bad"}),
        ),
        (
            "scroll",
            json!({"tab":"t","element_index":1,"direction":"diagonal"}),
        ),
    ] {
        assert_eq!(b.execute(method, &args).unwrap_err().code, -32602);
    }
}
