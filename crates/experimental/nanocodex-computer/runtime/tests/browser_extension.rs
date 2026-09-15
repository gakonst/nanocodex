#![cfg(unix)]
use serde_json::{Value, json};
use skyre::{browser_extension, protocol};
use std::{
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
};
use tungstenite::Message;
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn bridge_requires_capability_and_relays_real_native_frames() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = directory.path().join("bridge.sock");
    let mut bridge = Process(
        Command::new(env!("CARGO_BIN_EXE_nanocodex-computer"))
            .args(["extension-bridge", "--socket", socket.to_str().unwrap()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(bridge.0.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let config: Value = serde_json::from_str(&line).unwrap();
    let endpoint = config["endpoint"].as_str().unwrap();
    let parsed = url::Url::parse(endpoint).unwrap();
    assert_eq!(parsed.path().len(), 65);
    let denied = format!(
        "ws://{}/wrong-token",
        parsed.socket_addrs(|| None).unwrap()[0]
    );
    assert!(tungstenite::connect(&denied).is_err());
    let mut host = Process(
        Command::new(env!("CARGO_BIN_EXE_nanocodex-computer"))
            .args(["extension-host", "--socket", socket.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let (mut ws, _) = tungstenite::connect(endpoint).unwrap();
    let request = json!({"id":1,"method":"Target.getTargets","params":{}});
    ws.send(Message::text(request.to_string())).unwrap();
    let mut native_output = host.0.stdout.take().unwrap();
    let forwarded = protocol::read_frame(&mut native_output).unwrap().unwrap();
    let context = forwarded["_skyreContext"].clone();
    assert_eq!(context["sessionId"].as_str().unwrap().len(), 64);
    assert_eq!(context["turnId"].as_str().unwrap().len(), 64);
    assert_eq!(context["mode"], "current");
    let mut bound = request.clone();
    bound["_skyreContext"] = context.clone();
    assert_eq!(forwarded, bound);
    let mut native_input = host.0.stdin.take().unwrap();
    let response = json!({"id":1,"result":{"targetInfos":[{"targetId":"7","type":"page","url":"about:blank"}]}});
    protocol::write_frame(&mut native_input, &response).unwrap();
    let message = ws.read().unwrap();
    if !message.is_text() {
        let _ = bridge.0.kill();
        let _ = host.0.kill();
        use std::io::Read;
        let mut errors = String::new();
        bridge
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut errors)
            .unwrap();
        host.0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut errors)
            .unwrap();
        panic!("Unexpected WS {message:?}, errors: {errors}");
    }
    let received: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
    assert_eq!(received, response);
    let event = json!({"method":"Target.targetDestroyed","params":{"targetId":"7"}});
    protocol::write_frame(&mut native_input, &event).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(ws.read().unwrap().to_text().unwrap()).unwrap(),
        event
    );
    // A >1 MiB CDP command is split before the Chrome-facing stdout pipe.
    let request =
        json!({"id":2,"method":"Runtime.evaluate","params":{"expression":"α🧪".repeat(250000)}});
    ws.send(Message::text(request.to_string())).unwrap();
    let mut joined = vec![];
    let mut index = 0;
    loop {
        let message = protocol::read_frame(&mut native_output).unwrap().unwrap();
        assert!(message.to_string().len() < 1024 * 1024);
        let chunk = &message["__skyre_chunk"];
        assert_eq!(chunk["part"], index);
        use base64::Engine;
        joined.extend(
            base64::engine::general_purpose::STANDARD
                .decode(chunk["data"].as_str().unwrap())
                .unwrap(),
        );
        index += 1;
        if chunk["total"] == index {
            break;
        }
    }
    let mut bound = request.clone();
    bound["_skyreContext"] = context.clone();
    assert_eq!(serde_json::from_slice::<Value>(&joined).unwrap(), bound);
    // Caller-supplied ownership cannot escape the bridge's private session.
    ws.send(Message::text(json!({"id":3,"method":"Skyre.beginTurn","params":{},"_skyreContext":{"sessionId":"victim","turnId":"stale","mode":"cached"}}).to_string())).unwrap();
    let next = protocol::read_frame(&mut native_output).unwrap().unwrap();
    assert_eq!(next["_skyreContext"]["sessionId"], context["sessionId"]);
    assert_ne!(next["_skyreContext"]["turnId"], context["turnId"]);
    assert_eq!(next["_skyreContext"]["mode"], "current");
    let saved: Value = serde_json::from_slice(
        &std::fs::read(PathBuf::from(format!("{}.owner.json", socket.display()))).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["sessionId"], context["sessionId"]);
    assert_eq!(saved["turnId"], next["_skyreContext"]["turnId"]);
    // Transport ownership is not host-turn authority. A caller cannot forge the
    // extension-only authorization marker, even with a valid CDP capability.
    let event = json!({"eventId":"host-start","sequence":1,"phase":"started","route":{"conversationId":"conversation","threadId":"child"},"turnId":"turn-1"});
    let mut forged = event.clone();
    forged["authorityToken"] = json!("wrong");
    ws.send(Message::text(
        json!({"id":4,"method":"Skyre.hostLifecycle","params":forged,"_skyreHostAuthorized":true})
            .to_string(),
    ))
    .unwrap();
    let rejected: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(rejected["error"]["code"], -32003);
    let mut authorized = event.clone();
    authorized["authorityToken"] = saved["hostAuthority"].clone();
    let mut trusted_context = Value::Null;
    for id in [5, 6] {
        ws.send(Message::text(
            json!({"id":id,"method":"Skyre.hostLifecycle","params":authorized}).to_string(),
        ))
        .unwrap();
        let forwarded = protocol::read_frame(&mut native_output).unwrap().unwrap();
        assert_eq!(forwarded["_skyreHostAuthorized"], true);
        assert_eq!(forwarded["params"], event);
        assert_eq!(
            forwarded["_skyreContext"]["sessionId"],
            context["sessionId"]
        );
        if id == 5 {
            trusted_context = forwarded["_skyreContext"].clone();
        } else {
            assert_eq!(forwarded["_skyreContext"], trusted_context);
        }
        protocol::write_frame(&mut native_input, &json!({"id":id,"result":{}})).unwrap();
        ws.read().unwrap();
    }
    ws.send(Message::text(
        json!({"id":7,"method":"Skyre.beginTurn","params":{}}).to_string(),
    ))
    .unwrap();
    let rejected: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(rejected["error"]["code"], -32003);
    ws.close(None).unwrap();
}
#[test]
fn extension_manifest_is_exclusive_private_and_origin_bound() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_nanocodex-computer"));
    let socket = directory.path().join("bridge.sock");
    let id = "a".repeat(32);
    let prepared =
        browser_extension::prepare_manifest(directory.path(), &executable, &socket, &id).unwrap();
    let manifest = PathBuf::from(prepared["manifest"].as_str().unwrap());
    let contents = std::fs::read_to_string(&manifest).unwrap();
    let v: Value = serde_json::from_str(&contents).unwrap();
    assert_eq!(
        v["allowed_origins"],
        json!([format!("chrome-extension://{id}/")])
    );
    assert_eq!(
        std::fs::metadata(&manifest).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        browser_extension::prepare_manifest(directory.path(), &executable, &socket, &id).is_err()
    );
    assert_eq!(std::fs::read_to_string(manifest).unwrap(), contents);
}
#[test]
fn bridge_rejects_public_bind_and_insecure_directory() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = directory.path().join("bridge.sock");
    assert!(
        browser_extension::serve("0.0.0.0:0", &socket)
            .unwrap_err()
            .message
            .contains("loopback")
    );
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        browser_extension::serve("127.0.0.1:0", &socket)
            .unwrap_err()
            .message
            .contains("private")
    );
}
#[test]
fn extension_javascript_dispatch_contracts() {
    let status = Command::new("node")
        .args(["--test", "tests/browser_extension.mjs"])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn bridge_restart_preserves_private_owner_and_rejects_symlink_state() {
    fn start(socket: &std::path::Path) -> (Process, String) {
        let mut bridge = Process(
            Command::new(env!("CARGO_BIN_EXE_nanocodex-computer"))
                .args(["extension-bridge", "--socket", socket.to_str().unwrap()])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut line = String::new();
        BufReader::new(bridge.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(
            !line.is_empty(),
            "bridge did not publish its private endpoint"
        );
        let config: Value = serde_json::from_str(&line).unwrap();
        (bridge, config["endpoint"].as_str().unwrap().into())
    }
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = directory.path().join("bridge.sock");
    let owner_path = directory.path().join("bridge.sock.owner.json");
    let (mut first, first_endpoint) = start(&socket);
    let owner = std::fs::read(&owner_path).unwrap();
    assert_eq!(
        std::fs::metadata(&owner_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    first.0.kill().unwrap();
    first.0.wait().unwrap(); // Deliberately leaves a stale Unix socket.
    let (mut second, second_endpoint) = start(&socket);
    assert_ne!(first_endpoint, second_endpoint); // Endpoint capabilities never survive process restart.
    assert_eq!(std::fs::read(&owner_path).unwrap(), owner);
    second.0.kill().unwrap();
    second.0.wait().unwrap();
    std::fs::remove_file(&owner_path).unwrap();
    let victim = directory.path().join("unrelated.json");
    std::fs::write(&victim, &owner).unwrap();
    std::os::unix::fs::symlink(&victim, &owner_path).unwrap();
    let denied = Command::new(env!("CARGO_BIN_EXE_nanocodex-computer"))
        .args(["extension-bridge", "--socket", socket.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert_eq!(std::fs::read(victim).unwrap(), owner);
}

#[test]
fn extension_provider_metadata_routes_marks_and_connection_cleanup() {
    use skyre::browser::Browsers;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!(
        "ws://{}/fixture?skyre-provider=extension",
        listener.local_addr().unwrap()
    );
    let worker = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        for expected in [
            "Skyre.beginTurn",
            "Skyre.claimTab",
            "Skyre.markTab",
            "Skyre.nameSession",
            "Skyre.setViewport",
            "Skyre.turnEnded",
        ] {
            let request: Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(request["method"], expected);
            if expected == "Skyre.markTab" {
                assert_eq!(request["params"], json!({"tab":"7","status":"handoff"}));
            }
            if expected == "Skyre.setViewport" {
                assert_eq!(
                    request["params"],
                    json!({"value":{"width":640,"height":480}})
                );
            }
            socket
                .send(Message::text(
                    json!({"id":request["id"],"result":{"id":"7"}}).to_string(),
                ))
                .unwrap();
        }
    });
    let mut browsers = Browsers::default();
    browsers.register("owned", &endpoint).unwrap();
    assert_eq!(
        browsers
            .execute("info", &json!({"browser":"owned"}))
            .unwrap()["type"],
        "extension"
    );
    for (method, params) in [
        ("user_claim_tab", json!({"browser":"owned","tab":"7"})),
        (
            "mark_tab",
            json!({"browser":"owned","tab":"7","status":"handoff"}),
        ),
        ("name_session", json!({"browser":"owned","name":"Example"})),
        (
            "viewport_set",
            json!({"browser":"owned","width":640,"height":480}),
        ),
    ] {
        browsers.execute(method, &params).unwrap();
    }
    assert!(browsers.end_session().is_empty());
    assert!(browsers.end_session().is_empty()); // Closed authority never ends twice.
    worker.join().unwrap();
}
