use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async,
    tungstenite::{Message, protocol::frame::coding::CloseCode},
};

use super::*;
use crate::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, contract::async_trait,
};

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "echo",
            "echo one exact value",
            json!({
                "type":"object",
                "properties":{"value":{"type":"string"}},
                "required":["value"],
                "additionalProperties":false
            }),
        )
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        Ok(ToolOutput::json(&input.decode_json::<Value>()?))
    }
}

struct BlockingTool;

#[async_trait]
impl Tool for BlockingTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "block",
            "blocks forever",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        std::future::pending().await
    }
}

#[test]
fn target_is_transport_only_and_redacts_the_bearer() {
    let target = AttachmentTarget::new(
        "wss://example.test/final/path?placement=browser",
        "very-secret",
    )
    .unwrap();
    assert!(!format!("{target:?}").contains("very-secret"));
    assert!(AttachmentTarget::new("https://example.test", "secret").is_err());
    assert!(AttachmentTarget::new("ws://example.test", " ").is_err());
}

#[tokio::test]
async fn start_returns_before_ready_and_detach_owns_initialization_cleanup() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let (catalog_tx, catalog_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        assert_eq!(recv_json(&mut socket).await["type"], "catalog");
        let _ = catalog_tx.send(());
        match socket.next().await {
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {}
            frame => panic!("expected attachment shutdown, received {frame:?}"),
        }
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .start()
        .unwrap();
    assert_eq!(attachment.status(), AttachmentStatus::Connecting);
    catalog_rx.await.unwrap();
    let retained = attachment.clone();
    tokio::time::timeout(Duration::from_secs(1), attachment.detach())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retained.status(), AttachmentStatus::Disconnected);
    server.await.unwrap();

    // Cancelling setup before the driver is first polled must also stop
    // advertising Connecting on a retained handle.
    let (attachment, _) = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap()
        .attach(AttachmentTarget::new("ws://127.0.0.1:9/tools", "bearer").unwrap())
        .start()
        .unwrap();
    let retained = attachment.clone();
    tokio::time::timeout(Duration::from_secs(1), attachment.detach())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retained.status(), AttachmentStatus::Disconnected);
}

#[tokio::test]
async fn fast_ready_disconnects_keep_exponential_reconnect_backoff() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut accepted = Vec::new();
        for _ in 0..4 {
            let mut socket = accept(&listener).await;
            let catalog = recv_json(&mut socket).await;
            send_json(&mut socket, json!({"type":"ready"})).await;
            accepted.push((Instant::now(), catalog));
            socket.close(None).await.unwrap();
        }
        accepted
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .metadata(machine_metadata(
            "machine-reconnect",
            "/workspace/reconnect",
        ))
        .start()
        .unwrap();
    let accepted = tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    assert!(accepted[2].0.duration_since(accepted[1].0) >= Duration::from_millis(170));
    assert!(accepted[3].0.duration_since(accepted[2].0) >= Duration::from_millis(350));
    for pair in accepted.windows(2) {
        assert_catalog_diagnostics(&pair[0].1);
        assert_catalog_diagnostics(&pair[1].1);
        assert_ne!(pair[0].1["connection_id"], pair[1].1["connection_id"]);
        let mut previous = pair[0].1.clone();
        let mut next = pair[1].1.clone();
        previous.as_object_mut().unwrap().remove("connection_id");
        next.as_object_mut().unwrap().remove("connection_id");
        assert_eq!(previous, next, "catalog remains pinned across attempts");
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while attachment.status() != AttachmentStatus::Disconnected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    attachment.detach().await.unwrap();
}

#[tokio::test]
async fn stalled_websocket_handshake_retries_before_provision_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .start()
        .unwrap();
    let (first, _) = listener.accept().await.unwrap();
    // Leave the HTTP upgrade unanswered; the driver should retry the socket.
    let (second, _) = tokio::time::timeout(Duration::from_secs(8), listener.accept())
        .await
        .expect("attachment remained stuck in the first handshake")
        .unwrap();
    drop((first, second));
    attachment.detach().await.unwrap();
}

#[tokio::test]
async fn catalog_call_result_and_drain_use_exact_frames() {
    let evidence_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/hand-boundaries-20261001");
    std::fs::create_dir_all(&evidence_dir).unwrap();
    let evidence_path = evidence_dir.join("rust-native-lifecycle.log");
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("nanocodex_oai_tools::attachment=info")
        .with_ansi(false)
        .with_span_events(
            tracing_subscriber::fmt::format::FmtSpan::NEW
                | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
        )
        .with_writer(std::fs::File::create(&evidence_path).unwrap())
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let catalog = recv_json(&mut socket).await;
        assert_eq!(catalog["type"], "catalog");
        assert_eq!(catalog.as_object().unwrap().len(), 10);
        assert_eq!(catalog["turn_lifecycle"], true);
        assert_catalog_diagnostics(&catalog);
        assert!(
            catalog["runtime_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        );
        assert_eq!(catalog["capabilities"], json!(["turn_metadata"]));
        assert_eq!(catalog["tools"][0]["definition"]["name"], "echo");
        assert_eq!(catalog["attachment_id"], "machine-1");
        assert_eq!(
            catalog["machines"],
            json!([{
                "id": "machine-1",
                "name": "Developer laptop",
                "workspace": "/workspace/project",
                "capabilities": ["native", "filesystem", "process", "package", "server"]
            }])
        );
        send_json(&mut socket, json!({"type":"ready"})).await;

        send_json(&mut socket, call("call-1", "echo")).await;
        let result = recv_result_phases(
            &mut socket,
            "call-1",
            &[
                "received",
                "execution_started",
                "execution_finished",
                "result_prepared",
            ],
        )
        .await;
        assert_eq!(result["type"], "result");
        assert_eq!(result["call_id"], "call-1");
        assert_eq!(result["outcome"]["status"], "completed");
        eprintln!("native completed receipt timing: {}", result["timing"]);
        send_json(&mut socket, json!({"type":"ack","call_id":"call-1"})).await;
        let _ = completed_tx.send(());

        let drain = recv_json(&mut socket).await;
        assert_eq!(drain, json!({"type":"drain"}));
        send_json(&mut socket, json!({"type":"draining"})).await;
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(AttachmentTarget::new(endpoint.clone(), "bearer").unwrap())
        .metadata(machine_metadata("machine-1", "/workspace/project"))
        .connect()
        .await
        .unwrap();
    assert_eq!(attachment.status(), AttachmentStatus::Ready);
    completed_rx.await.unwrap();
    attachment.detach().await.unwrap();
    server.await.unwrap();
    let trace = std::fs::read_to_string(&evidence_path).unwrap();
    assert!(!trace.contains("private-response-marker"));
    assert!(!trace.contains(&endpoint));
    for stage in [
        "received",
        "execution_started",
        "execution_finished",
        "result_prepared",
    ] {
        assert!(
            trace
                .lines()
                .any(|line| line.contains("transport_call_id=\"call-1\"")
                    && line.contains(&format!("stage=\"{stage}\""))),
            "trace records actual stage {stage}"
        );
    }
}

#[tokio::test]
async fn websocket_pongs_preserve_readiness_and_tool_execution() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        assert_eq!(recv_json(&mut socket).await["type"], "catalog");
        socket.send(Message::Pong(vec![1].into())).await.unwrap();
        send_json(&mut socket, json!({"type":"ready"})).await;
        socket.send(Message::Pong(vec![2].into())).await.unwrap();
        for index in 0..3 {
            let Message::Ping(nonce) = socket.next().await.unwrap().unwrap() else {
                panic!("expected native control ping without JSON heartbeat");
            };
            assert_eq!(nonce.len(), 16);
            eprintln!("native control ping {index}: nonce_bytes={}", nonce.len());
            socket.send(Message::Pong(nonce)).await.unwrap();
        }
        send_json(&mut socket, call("call-pong", "echo")).await;
        let result = recv_json(&mut socket).await;
        assert_eq!(result["call_id"], "call-pong");
        assert_eq!(result["outcome"]["status"], "completed");
        send_json(&mut socket, json!({"type":"ack","call_id":"call-pong"})).await;
        let _ = completed_tx.send(());
        assert_eq!(recv_json(&mut socket).await, json!({"type":"drain"}));
        send_json(&mut socket, json!({"type":"draining"})).await;
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .connect()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), completed_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attachment.status(), AttachmentStatus::Ready);
    attachment.detach().await.unwrap();
    server.await.unwrap();
}

#[test]
fn attachment_metadata_enforces_the_machine_wire_contract() {
    let metadata = machine_metadata("machine.valid:1", "/workspace/project");
    assert_eq!(metadata.attachment_id(), "machine.valid:1");
    assert_eq!(
        metadata.attached_machine().unwrap().id(),
        metadata.attachment_id()
    );
    assert!(AttachmentMetadata::named("a".repeat(123)).is_ok());
    assert!(AttachmentMetadata::named("a".repeat(124)).is_err());
    assert!(AttachmentMetadata::named("unsafe/id").is_err());
    assert!(AttachmentMachine::new("machine-1", "name", "/workspace", ["BadCapability"]).is_err());
    assert!(
        AttachmentMachine::new("machine-1", "name", "/workspace", ["process", "process"]).is_err()
    );
    assert!(
        AttachmentMachine::new("machine-1", "é".repeat(65), "/workspace", [] as [&str; 0],)
            .is_err()
    );
}

#[tokio::test]
async fn cancellation_is_only_an_ordinary_result() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = ready(&listener).await;
        send_json(&mut socket, call("call-cancel", "block")).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        send_json(
            &mut socket,
            json!({"type":"cancel","call_id":"call-cancel"}),
        )
        .await;
        let result = recv_result_phases(
            &mut socket,
            "call-cancel",
            &[
                "received",
                "execution_started",
                "execution_finished",
                "result_prepared",
            ],
        )
        .await;
        assert_eq!(result["type"], "result");
        assert_eq!(result["call_id"], "call-cancel");
        assert_eq!(result["outcome"]["status"], "ambiguous");
        send_json(&mut socket, json!({"type":"ack","call_id":"call-cancel"})).await;
        let _ = completed_tx.send(());
        assert_eq!(recv_json(&mut socket).await, json!({"type":"drain"}));
        send_json(&mut socket, json!({"type":"draining"})).await;
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(BlockingTool)
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .connect()
        .await
        .unwrap();
    completed_rx.await.unwrap();
    attachment.detach().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn disconnect_after_dispatch_replays_identical_receipt_until_ack() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut first = ready(&listener).await;
        send_json(&mut first, call("lost-ack", "echo")).await;
        let result = recv_json(&mut first).await;
        assert_eq!(result["call_id"], "lost-ack");
        first.close(None).await.unwrap();

        let mut second = ready(&listener).await;
        assert_eq!(recv_json(&mut second).await, result);
        send_json(
            &mut second,
            json!({"type":"recover","call_ids":["lost-ack"]}),
        )
        .await;
        assert_eq!(recv_json(&mut second).await, result);
        send_json(&mut second, json!({"type":"ack","call_id":"lost-ack"})).await;
        send_json(
            &mut second,
            json!({"type":"recover","call_ids":["lost-ack"]}),
        )
        .await;
        assert_eq!(
            recv_json(&mut second).await,
            json!({"type":"status","call_id":"lost-ack","state":"missing"})
        );
        assert_eq!(recv_json(&mut second).await, json!({"type":"drain"}));
        send_json(&mut second, json!({"type":"draining"})).await;
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .connect()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(180)).await;
    attachment.detach().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn detach_cancels_execution_and_waits_until_results_are_acknowledged() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = ready(&listener).await;
        send_json(&mut socket, call("draining-call", "block")).await;
        assert_eq!(recv_json(&mut socket).await, json!({"type":"drain"}));
        send_json(&mut socket, json!({"type":"draining"})).await;

        let result = recv_json(&mut socket).await;
        assert_eq!(result["call_id"], "draining-call");
        assert_eq!(result["outcome"]["status"], "ambiguous");
        send_json(&mut socket, json!({"type":"ack","call_id":"draining-call"})).await;
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(BlockingTool)
        .build()
        .unwrap();
    let (attachment, mut events) = tools
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .connect()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.recv().await {
            if matches!(event, AttachmentEvent::CallStarted { .. }) {
                return;
            }
        }
        panic!("attachment closed before admitting the call");
    })
    .await
    .expect("call admission");
    let retained = attachment.clone();
    assert_eq!(retained.status(), AttachmentStatus::Ready);
    attachment.detach().await.unwrap();
    retained.closed().await.unwrap();
    eprintln!(
        "native retained handle after detach: {:?}",
        retained.status()
    );
    assert_eq!(retained.status(), AttachmentStatus::Disconnected);
    server.await.unwrap();
}

#[tokio::test]
async fn authentication_rejection_closes_and_marks_retained_handle_fenced() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let rejected = accept_hdr_async(
            stream,
            |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
             _response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                Err(http::Response::builder().status(401).body(None).unwrap())
            },
        )
        .await;
        assert!(rejected.is_err());
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(AttachmentTarget::new(endpoint, "synthetic-rejected-bearer").unwrap())
        .start()
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(2), attachment.closed())
        .await
        .expect("authentication rejection did not close the attachment");
    assert!(matches!(outcome, Err(AttachmentError::Authentication(_))));
    eprintln!(
        "native retained handle after HTTP 401: {:?}",
        attachment.status()
    );
    assert_eq!(attachment.status(), AttachmentStatus::Fenced);
    server.await.unwrap();
}

#[tokio::test]
async fn a_call_crossing_the_socket_is_accepted_until_the_draining_barrier() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = ready(&listener).await;
        assert_eq!(recv_json(&mut socket).await, json!({"type":"drain"}));
        send_json(&mut socket, call("crossed-call", "echo")).await;
        send_json(&mut socket, json!({"type":"draining"})).await;
        let result = recv_json(&mut socket).await;
        assert_eq!(result["call_id"], "crossed-call");
        assert_eq!(result["outcome"]["status"], "completed");
        send_json(&mut socket, json!({"type":"ack","call_id":"crossed-call"})).await;
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .connect()
        .await
        .unwrap();
    attachment.detach().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn legacy_fields_are_protocol_rejections_carried_by_close() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let _catalog = recv_json(&mut socket).await;
        send_json(&mut socket, json!({"type":"ready","protocol_version":1})).await;
        let Message::Close(Some(close)) = socket.next().await.unwrap().unwrap() else {
            panic!("expected close")
        };
        assert_eq!(close.code, CloseCode::Policy);
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap();
    let error = tools
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .connect()
        .await
        .unwrap_err();
    assert!(matches!(error, AttachmentError::Fenced(_)));
    server.await.unwrap();
}

async fn accept(listener: &TcpListener) -> WebSocketStream<TcpStream> {
    let (stream, _) = listener.accept().await.unwrap();
    accept_hdr_async(
        stream,
        |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
         mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let id = uuid::Uuid::parse_str(
                request.headers()["x-nanocodex-request-id"]
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(id.get_version_num(), 4);
            assert_eq!(id.get_variant(), uuid::Variant::RFC4122);
            // An endpoint's arbitrary response correlation is never trusted for logging.
            response.headers_mut().insert(
                "x-nanocodex-request-id",
                "private-response-marker".parse().unwrap(),
            );
            Ok(response)
        },
    )
    .await
    .unwrap()
}

async fn ready(listener: &TcpListener) -> WebSocketStream<TcpStream> {
    ready_with_catalog(listener).await.0
}

async fn ready_with_catalog(listener: &TcpListener) -> (WebSocketStream<TcpStream>, Value) {
    let mut socket = accept(listener).await;
    let catalog = recv_json(&mut socket).await;
    assert_eq!(catalog["type"], "catalog");
    assert_catalog_diagnostics(&catalog);
    assert!(
        catalog["runtime_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert_eq!(catalog["capabilities"], json!(["turn_metadata"]));
    send_json(&mut socket, json!({"type":"ready"})).await;
    (socket, catalog)
}

fn call(call_id: &str, name: &str) -> Value {
    json!({
        "type":"call",
        "session_id":"session-1",
        "call_id":call_id,
        "model":"gpt-6.1-sol",
        "name":name,
        "input":if name == "echo" { json!({"value":"hello"}) } else { json!({}) },
        "output_token_budget":1000,
        "output_byte_budget":131072,
        "deadline_at":now_ms()+10_000
    })
}

async fn send_json(socket: &mut WebSocketStream<TcpStream>, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn recv_json(socket: &mut WebSocketStream<TcpStream>) -> Value {
    loop {
        let frame = recv_wire_json(socket).await;
        if frame["type"] != "diagnostic" {
            return frame;
        }
    }
}

fn assert_catalog_diagnostics(catalog: &Value) {
    assert_eq!(catalog["diagnostics"], true);
    assert_eq!(catalog["command_recovery"], true);
    let id = uuid::Uuid::parse_str(catalog["connection_id"].as_str().unwrap()).unwrap();
    assert_eq!(id.get_version_num(), 4);
    assert_eq!(id.get_variant(), uuid::Variant::RFC4122);
}

async fn recv_result_phases(
    socket: &mut WebSocketStream<TcpStream>,
    call_id: &str,
    expected: &[&str],
) -> Value {
    let mut stages = Vec::new();
    let mut last = 0.0;
    loop {
        let frame = recv_wire_json(socket).await;
        if frame["type"] == "diagnostic" {
            if frame["call_id"] == call_id {
                let elapsed = frame["elapsed_ms"].as_f64().unwrap();
                assert!(elapsed >= last, "phase elapsed must be monotonic: {frame}");
                last = elapsed;
                stages.push(frame["stage"].as_str().unwrap().to_owned());
            }
        } else {
            assert_eq!(frame["type"], "result");
            assert_eq!(frame["call_id"], call_id);
            assert_eq!(
                stages, expected,
                "actual socket phases before result for {call_id}"
            );
            eprintln!(
                "native diagnostic journey {call_id}: {stages:?}; elapsed_ms={last}; status={}",
                frame["outcome"]["status"]
            );
            return frame;
        }
    }
}

async fn recv_wire_json(socket: &mut WebSocketStream<TcpStream>) -> Value {
    loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => {
                let frame: Value = serde_json::from_str(&text).unwrap();
                if frame["type"] == "diagnostic" {
                    assert_eq!(
                        frame.as_object().unwrap().len(),
                        4,
                        "diagnostics contain only phase metadata"
                    );
                    assert!(frame["call_id"].is_string());
                    assert!(matches!(
                        frame["stage"].as_str().unwrap(),
                        "received" | "execution_started" | "execution_finished" | "result_prepared"
                    ));
                    let elapsed = frame["elapsed_ms"].as_f64().unwrap();
                    assert!(
                        elapsed.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(&elapsed)
                    );
                    eprintln!("native socket diagnostic: {frame}");
                }
                if frame["type"] == "result" {
                    let timing = frame["timing"]
                        .as_object()
                        .expect("native receipts carry local timing");
                    let phases = [
                        "scheduler_ms",
                        "execution_gate_ms",
                        "execution_ms",
                        "result_encode_ms",
                        "result_queue_ms",
                    ];
                    assert_eq!(timing.len(), phases.len() + 1);
                    let elapsed = timing["host_elapsed_ms"].as_f64().unwrap();
                    assert!(elapsed.is_finite() && elapsed >= 0.0);
                    let mut sum = 0.0;
                    for phase in phases {
                        let duration = timing[phase].as_f64().unwrap();
                        assert!(duration.is_finite() && duration >= 0.0);
                        sum += duration;
                    }
                    assert!(
                        sum <= elapsed + 0.01,
                        "receipt phases exceed local elapsed: {timing:?}"
                    );
                }
                return frame;
            }
            Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
            frame => panic!("unexpected websocket frame: {frame:?}"),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

fn machine_metadata(id: &str, workspace: &str) -> AttachmentMetadata {
    AttachmentMetadata::machine(
        AttachmentMachine::new(
            id,
            "Developer laptop",
            workspace,
            ["native", "filesystem", "process", "package", "server"],
        )
        .unwrap(),
    )
}

struct TurnIdentityTool;

#[async_trait]
impl Tool for TurnIdentityTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "turn_identity",
            "returns invocation identity",
            json!({"type":"object"}),
        )
    }
    async fn execute(&self, _input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        Ok(ToolOutput::json(
            &json!({"turn_id":context.turn_id(), "call_id":context.call_id(), "session_id":context.session_id()}),
        ))
    }
}

#[tokio::test]
async fn native_attachment_preserves_turn_identity_through_prepared_runtime() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = ready(&listener).await;
        for (id, turn) in [
            ("one", Some("session-1:7")),
            ("two", Some("session-1:7")),
            ("three", Some("session-1:8")),
            ("legacy", None),
        ] {
            let mut frame = call(id, "turn_identity");
            if let Some(turn) = turn {
                frame["turn_id"] = json!(turn);
            }
            send_json(&mut socket, frame).await;
            let result = recv_json(&mut socket).await;
            assert_eq!(result["outcome"]["status"], "completed");
            let output = &result["outcome"]["output"];
            let identity: Value = serde_json::from_str(output["output"].as_str().unwrap()).unwrap();
            assert_eq!(
                identity,
                json!({"session_id":"session-1", "call_id":id, "turn_id":turn})
            );
            send_json(&mut socket, json!({"type":"ack", "call_id":id})).await;
        }
        completed_tx.send(()).unwrap();
        assert_eq!(recv_json(&mut socket).await, json!({"type":"drain"}));
        send_json(&mut socket, json!({"type":"draining"})).await;
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(TurnIdentityTool)
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .connect()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), completed_rx)
        .await
        .unwrap()
        .unwrap();
    attachment.detach().await.unwrap();
    server.await.unwrap();
}

struct GatedTool {
    started: tokio::sync::mpsc::UnboundedSender<String>,
    release: std::sync::Arc<tokio::sync::Semaphore>,
    parallel: bool,
}

#[async_trait]
impl Tool for GatedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function("gated", "wait for release", json!({"type":"object"}))
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.parallel
    }

    async fn execute(&self, _input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        self.started.send(context.call_id().to_owned()).unwrap();
        self.release.acquire().await.unwrap().forget();
        self.started
            .send(format!("finished:{}", context.call_id()))
            .unwrap();
        Ok(ToolOutput::json(&json!({"finished":true})))
    }
}

#[tokio::test]
async fn reconnect_retains_running_calls_without_serialization_or_reexecution() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut prior_runtime = Value::Null;
        for (parallel, count) in [(false, 65), (true, 65)] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
            let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
            let release = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
            let tools = Tools::builder()
                .without_defaults()
                .tool(EchoTool)
                .tool(GatedTool {
                    started,
                    release: release.clone(),
                    parallel,
                })
                .build()
                .unwrap();
            let (attachment, mut events) = tools
                .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
                .start()
                .unwrap();
            let (mut first, catalog) = ready_with_catalog(&listener).await;
            assert_ne!(catalog["runtime_id"], prior_runtime);
            prior_runtime = catalog["runtime_id"].clone();
            for id in 0..count {
                send_json(&mut first, call(&format!("old-{id}"), "gated")).await;
                assert_eq!(starts.recv().await.unwrap(), format!("old-{id}"));
            }
            let first_connection_id = catalog["connection_id"].clone();
            first.close(None).await.unwrap();
            let (mut second, catalog) = ready_with_catalog(&listener).await;
            assert_ne!(catalog["connection_id"], first_connection_id);
            assert_eq!(catalog["runtime_id"], prior_runtime);
            send_json(&mut second, call("busy", "echo")).await;
            let received = recv_wire_json(&mut second).await;
            assert_eq!(received["type"], "diagnostic");
            assert_eq!(received["call_id"], "busy");
            assert_eq!(received["stage"], "received");
            let busy = recv_result_phases(
                &mut second,
                "busy",
                &["execution_started", "execution_finished", "result_prepared"],
            )
            .await;
            assert_eq!(busy["call_id"], "busy");
            assert_eq!(busy["outcome"]["status"], "completed");
            assert_eq!(busy["timing"]["execution_gate_ms"], 0.0);
            send_json(&mut second, json!({"type":"ack","call_id":"busy"})).await;
            release.add_permits(count);
            let mut finished = 0;
            while finished < count {
                if let AttachmentEvent::CallCompleted { call_id, outcome } =
                    events.recv().await.unwrap()
                    && call_id.starts_with("old-")
                {
                    assert_eq!(outcome, AttachmentCallOutcome::Completed);
                    finished += 1;
                }
            }
            let mut received = std::collections::HashSet::new();
            while received.len() < count {
                let result = recv_json(&mut second).await;
                assert_eq!(result["outcome"]["status"], "completed");
                let id = result["call_id"].as_str().unwrap();
                assert!(id.starts_with("old-"));
                assert!(received.insert(id.to_owned()));
                send_json(&mut second, json!({"type":"ack","call_id":id})).await;
            }
            let detach = tokio::spawn(async move { attachment.detach().await });
            assert_eq!(recv_json(&mut second).await, json!({"type":"drain"}));
            send_json(&mut second, json!({"type":"draining"})).await;
            detach.await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
}

#[cfg(feature = "workspace-runtime")]
#[tokio::test]
async fn process_poll_survives_transport_reconnect_without_restarting_the_command() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
        let tools = Tools::builder()
            .without_defaults()
            .add(crate::WorkspaceTools::new(std::env::temp_dir()))
            .build()
            .unwrap();
        let (attachment, _) = tools
            .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
            .start()
            .unwrap();
        let (mut first, catalog) = ready_with_catalog(&listener).await;
        let mut command = call("command", "exec_command");
        command["input"] = json!({"cmd":"sleep 0.5; printf recovered", "yield_time_ms":250});
        send_json(&mut first, command).await;
        let initial = recv_json(&mut first).await;
        let process = initial["outcome"]["output"]["structured_result"]["session_id"]
            .as_i64()
            .unwrap();
        send_json(&mut first, json!({"type":"ack", "call_id":"command"})).await;
        first.close(None).await.unwrap();
        let (mut second, reconnected) = ready_with_catalog(&listener).await;
        assert_eq!(catalog["runtime_id"], reconnected["runtime_id"]);
        assert_ne!(catalog["connection_id"], reconnected["connection_id"]);
        let mut poll = call("poll", "write_stdin");
        poll["input"] = json!({"session_id":process, "yield_time_ms":5000});
        send_json(&mut second, poll).await;
        let completed = recv_json(&mut second).await;
        assert_eq!(
            completed["outcome"]["output"]["structured_result"]["exit_code"],
            0
        );
        assert_eq!(
            completed["outcome"]["output"]["structured_result"]["output"],
            "recovered"
        );
        send_json(&mut second, json!({"type":"ack", "call_id":"poll"})).await;
        let detach = tokio::spawn(async move { attachment.detach().await });
        assert_eq!(recv_json(&mut second).await, json!({"type":"drain"}));
        send_json(&mut second, json!({"type":"draining"})).await;
        detach.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unmatched_control_pong_does_not_mask_failure_and_reconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = AttachmentTarget::new(
        format!("ws://{}/tools", listener.local_addr().unwrap()),
        "synthetic-bearer",
    )
    .unwrap();
    let (attachment, _) = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap()
        .attach(target)
        .start()
        .unwrap();
    let (mut first, catalog) = ready_with_catalog(&listener).await;
    let Message::Ping(nonce) = first.next().await.unwrap().unwrap() else {
        panic!("missing control ping");
    };
    assert_eq!(nonce.len(), 16);
    // An explicit different pong overrides tungstenite's queued automatic reply.
    first.send(Message::Pong(vec![0].into())).await.unwrap();
    let Message::Close(Some(close)) = tokio::time::timeout(Duration::from_secs(1), first.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    else {
        panic!("unmatched pong masked missing peer response");
    };
    assert_eq!(close.code, CloseCode::Restart);
    eprintln!("native unmatched pong: close_code=1012; reconnect bound=1s");
    let (mut second, resumed) =
        tokio::time::timeout(Duration::from_secs(1), ready_with_catalog(&listener))
            .await
            .unwrap();
    assert_eq!(catalog["runtime_id"], resumed["runtime_id"]);
    tokio::time::timeout(Duration::from_secs(1), async {
        while attachment.status() != AttachmentStatus::Ready {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let detach = tokio::spawn(async move { attachment.detach().await });
    assert_eq!(recv_json(&mut second).await, json!({"type":"drain"}));
    send_json(&mut second, json!({"type":"draining"})).await;
    detach.await.unwrap().unwrap();
}

#[tokio::test]
async fn immutable_duplicate_running_call_reuses_task_and_conflict_fences() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let tools = Tools::builder()
        .without_defaults()
        .tool(GatedTool {
            started,
            release: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            parallel: true,
        })
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(
            AttachmentTarget::new(
                format!("ws://{}/tools", listener.local_addr().unwrap()),
                "synthetic-bearer",
            )
            .unwrap(),
        )
        .start()
        .unwrap();
    let mut socket = ready(&listener).await;
    let frame = call("duplicate-running", "gated");
    send_json(&mut socket, frame.clone()).await;
    assert_eq!(starts.recv().await.unwrap(), "duplicate-running");
    send_json(&mut socket, frame.clone()).await;
    assert_eq!(
        recv_json(&mut socket).await,
        json!({"type":"status","call_id":"duplicate-running","state":"running"})
    );
    assert!(
        starts.try_recv().is_err(),
        "duplicate call dispatched a second task"
    );
    let mut conflict = frame;
    conflict["deadline_at"] = json!(now_ms() + 50_000);
    send_json(&mut socket, conflict).await;
    let close = loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Close(Some(close)) => break close,
            Message::Text(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    };
    assert_eq!(close.code, CloseCode::Policy);
    assert!(matches!(
        attachment.closed().await,
        Err(AttachmentError::Fenced(_))
    ));
    eprintln!("native duplicate identity: starts=1, status=running, conflict_close=1008");
}

#[tokio::test]
async fn offline_completion_queue_does_not_block_160_running_commands() {
    tokio::time::timeout(Duration::from_secs(8), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
        let release = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let tools = Tools::builder().without_defaults().tool(GatedTool {
            started, release: release.clone(), parallel: true,
        }).build().unwrap();
        let (attachment, _events) = tools.attach(AttachmentTarget::new(
            format!("ws://{}/tools", listener.local_addr().unwrap()), "synthetic-bearer",
        ).unwrap()).start().unwrap();
        let (mut first, catalog) = ready_with_catalog(&listener).await;
        for id in 0..160 {
            send_json(&mut first, call(&format!("offline-{id}"), "gated")).await;
            assert_eq!(starts.recv().await.unwrap(), format!("offline-{id}"));
        }
        first.close(None).await.unwrap();
        release.add_permits(160);
        for _ in 0..160 {
            assert!(starts.recv().await.unwrap().starts_with("finished:offline-"));
        }
        eprintln!("native offline backpressure: completed=160 before replacement readiness; original_call_frames=160");
        let (mut second, resumed) = ready_with_catalog(&listener).await;
        assert_eq!(catalog["runtime_id"], resumed["runtime_id"]);
        let mut receipts = std::collections::HashSet::new();
        while receipts.len() < 160 {
            let receipt = recv_json(&mut second).await;
            assert_eq!(receipt["outcome"]["status"], "completed");
            let id = receipt["call_id"].as_str().unwrap();
            assert!(receipts.insert(id.to_owned()), "duplicate unsolicited receipt");
            send_json(&mut second, json!({"type":"ack","call_id":id})).await;
        }
        let detach = tokio::spawn(async move { attachment.detach().await });
        assert_eq!(recv_json(&mut second).await, json!({"type":"drain"}));
        send_json(&mut second, json!({"type":"draining"})).await;
        detach.await.unwrap().unwrap();
        eprintln!("native offline backpressure: recovered_receipts=160; replay_call_frames=0");
    }).await.unwrap();
}

#[tokio::test]
async fn silent_peer_without_control_pong_reconnects_within_finite_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(
            AttachmentTarget::new(
                format!("ws://{}/tools", listener.local_addr().unwrap()),
                "synthetic-bearer",
            )
            .unwrap(),
        )
        .start()
        .unwrap();
    let (first, catalog) = ready_with_catalog(&listener).await;
    // Keep the TCP connection open without polling it: no automatic peer pong.
    let (mut second, resumed) =
        tokio::time::timeout(Duration::from_secs(1), ready_with_catalog(&listener))
            .await
            .unwrap();
    assert_eq!(catalog["runtime_id"], resumed["runtime_id"]);
    assert_ne!(catalog["connection_id"], resumed["connection_id"]);
    drop(first);
    tokio::time::timeout(Duration::from_secs(1), async {
        while attachment.status() != AttachmentStatus::Ready {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let detach = tokio::spawn(async move { attachment.detach().await });
    assert_eq!(recv_json(&mut second).await, json!({"type":"drain"}));
    send_json(&mut second, json!({"type":"draining"})).await;
    detach.await.unwrap().unwrap();
    eprintln!("native silent peer: finite reconnect within 1s; same runtime ownership");
}

struct TurnLifecycleTool(std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>);

#[async_trait]
impl Tool for TurnLifecycleTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "lifecycle_probe",
            "Synthetic lifecycle probe",
            json!({"type":"object"}),
        )
    }
    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        Ok(ToolOutput::json(&*self.0.lock().unwrap()))
    }
    async fn end_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        hook_event_name: &str,
    ) -> Result<(), crate::contract::ToolError> {
        self.0
            .lock()
            .unwrap()
            .push((session_id.into(), turn_id.into(), hook_event_name.into()));
        Ok(())
    }
}

#[tokio::test]
async fn native_attachment_delivers_trusted_turn_cleanup_outside_model_catalog() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ended = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let tools = Tools::builder()
        .without_defaults()
        .tool(TurnLifecycleTool(ended.clone()))
        .build()
        .unwrap();
    let (attachment, _) = tools
        .attach(
            AttachmentTarget::new(
                format!("ws://{}/tools", listener.local_addr().unwrap()),
                "synthetic-bearer",
            )
            .unwrap(),
        )
        .start()
        .unwrap();
    let (mut socket, catalog) = ready_with_catalog(&listener).await;
    assert_eq!(catalog["turn_lifecycle"], true);
    assert_eq!(catalog["tools"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["tools"][0]["definition"]["name"], "lifecycle_probe");
    send_json(
        &mut socket,
        json!({"type":"turn_ended","session_id":"session:one","turn_id":"turn:cancelled","hook_event_name":"Interrupt"}),
    )
    .await;
    // A subsequent ordinary call acts as an ordered transport barrier.
    send_json(
        &mut socket,
        json!({"type":"call","session_id":"session:one","turn_id":"turn:next",
        "call_id":"probe","model":"fixture","name":"lifecycle_probe","input":{},
        "output_token_budget":1024,"output_byte_budget":16384,"deadline_at":now_ms()+5000}),
    )
    .await;
    let result = recv_json(&mut socket).await;
    assert_eq!(result["type"], "result");
    assert_eq!(
        *ended.lock().unwrap(),
        vec![(
            "session:one".into(),
            "turn:cancelled".into(),
            "Interrupt".into()
        )]
    );
    send_json(&mut socket, json!({"type":"ack","call_id":"probe"})).await;
    let detach = tokio::spawn(async move { attachment.detach().await });
    assert_eq!(recv_json(&mut socket).await, json!({"type":"drain"}));
    send_json(&mut socket, json!({"type":"draining"})).await;
    detach.await.unwrap().unwrap();
}
