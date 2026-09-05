use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    WebSocketStream, accept_async,
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
    tokio::time::timeout(Duration::from_secs(1), attachment.detach())
        .await
        .unwrap()
        .unwrap();
    server.await.unwrap();
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
    assert!(accepted.windows(2).all(|pair| pair[0].1 == pair[1].1));
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
async fn catalog_call_result_and_drain_use_exact_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let catalog = recv_json(&mut socket).await;
        assert_eq!(catalog["type"], "catalog");
        assert_eq!(catalog.as_object().unwrap().len(), 4);
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
        let result = recv_json(&mut socket).await;
        assert_eq!(result["type"], "result");
        assert_eq!(result["call_id"], "call-1");
        assert_eq!(result["outcome"]["status"], "completed");
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
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .metadata(machine_metadata("machine-1", "/workspace/project"))
        .connect()
        .await
        .unwrap();
    assert_eq!(attachment.status(), AttachmentStatus::Ready);
    completed_rx.await.unwrap();
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
        let result = recv_json(&mut socket).await;
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
async fn disconnect_after_dispatch_does_not_replay_receipts_into_a_new_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut first = ready(&listener).await;
        send_json(&mut first, call("lost-ack", "echo")).await;
        let result = recv_json(&mut first).await;
        assert_eq!(result["call_id"], "lost-ack");
        first.close(None).await.unwrap();

        let mut second = ready(&listener).await;
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
async fn graceful_drain_keeps_the_attachment_alive_until_calls_are_acknowledged() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = ready(&listener).await;
        send_json(&mut socket, call("draining-call", "block")).await;
        assert_eq!(recv_json(&mut socket).await, json!({"type":"drain"}));
        send_json(&mut socket, json!({"type":"draining"})).await;

        let ping = recv_json(&mut socket).await;
        assert_eq!(ping["type"], "ping");
        send_json(&mut socket, json!({"type":"pong","nonce":ping["nonce"]})).await;
        send_json(
            &mut socket,
            json!({"type":"cancel","call_id":"draining-call"}),
        )
        .await;
        let result = recv_json(&mut socket).await;
        assert_eq!(result["call_id"], "draining-call");
        send_json(&mut socket, json!({"type":"ack","call_id":"draining-call"})).await;
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
    tokio::time::sleep(Duration::from_millis(10)).await;
    attachment.detach().await.unwrap();
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
    accept_async(stream).await.unwrap()
}

async fn ready(listener: &TcpListener) -> WebSocketStream<TcpStream> {
    let mut socket = accept(listener).await;
    let catalog = recv_json(&mut socket).await;
    assert_eq!(catalog["type"], "catalog");
    send_json(&mut socket, json!({"type":"ready"})).await;
    socket
}

fn call(call_id: &str, name: &str) -> Value {
    json!({
        "type":"call",
        "session_id":"session-1",
        "call_id":call_id,
        "model":"gpt-5.6-sol",
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
        match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
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

struct TestObservation {
    surfaces: [ObservationSurface; 1],
    started: tokio::sync::Notify,
    stopped: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

struct CaptureGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl ObservationProvider for TestObservation {
    fn surfaces(&self) -> &[ObservationSurface] {
        &self.surfaces
    }
    async fn capture(&self, _surface_id: &str) -> Result<ObservationFrame, AttachmentError> {
        let _guard = CaptureGuard(self.stopped.clone());
        self.started.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn observation_capture_cancellation_does_not_block_tool_calls_or_drain() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let source = Arc::new(TestObservation {
        surfaces: [ObservationSurface::new("screen", "Desktop", ObservationKind::Desktop).unwrap()],
        started: tokio::sync::Notify::new(),
        stopped: Arc::new(AtomicUsize::new(0)),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();
    let observed = source.clone();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let catalog = recv_json(&mut socket).await;
        assert_eq!(
            catalog["observation_surfaces"],
            json!([{"id":"screen","name":"Desktop","kind":"desktop"}])
        );
        send_json(&mut socket, json!({"type":"ready"})).await;
        assert_eq!(observed.stopped.load(Ordering::SeqCst), 0);
        send_json(
            &mut socket,
            json!({"type":"observe","request_id":"view-1","surface_id":"screen"}),
        )
        .await;
        observed.started.notified().await;
        send_json(&mut socket, call("call-1", "echo")).await;
        let result = recv_json(&mut socket).await;
        assert_eq!(result["type"], "result");
        assert_eq!(result["outcome"]["status"], "completed");
        send_json(&mut socket, json!({"type":"ack","call_id":"call-1"})).await;
        send_json(
            &mut socket,
            json!({"type":"observe","request_id":"view-2","surface_id":"screen"}),
        )
        .await;
        let busy = recv_json(&mut socket).await;
        assert_eq!(busy["result"]["status"], "unavailable");
        send_json(
            &mut socket,
            json!({"type":"observe_cancel","request_id":"view-1"}),
        )
        .await;
        send_json(&mut socket, call("call-2", "echo")).await;
        assert_eq!(recv_json(&mut socket).await["call_id"], "call-2");
        send_json(&mut socket, json!({"type":"ack","call_id":"call-2"})).await;
        assert_eq!(observed.stopped.load(Ordering::SeqCst), 1);
        let _ = complete_tx.send(());
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
        .metadata(machine_metadata("observed", "/work"))
        .observation(source)
        .connect()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), complete_rx)
        .await
        .unwrap()
        .unwrap();
    attachment.detach().await.unwrap();
    server.await.unwrap();
}

#[test]
fn observation_bounds_match_the_javascript_contract() {
    assert!(ObservationSurface::new("bad/id", "Screen", ObservationKind::Phone).is_err());
    assert!(ObservationSurface::new("screen", "é".repeat(65), ObservationKind::Phone).is_err());
    assert!(ObservationFrame::new(1, 0, 1, ObservationImageFormat::Png, &[1]).is_err());
    assert!(
        ObservationFrame::new(1, 1, 1, ObservationImageFormat::Png, &vec![0; 180_001]).is_err()
    );
    let frame = ObservationFrame::new(1, 1, 1, ObservationImageFormat::Png, &[0, 0, 0]).unwrap();
    assert_eq!(
        serde_json::to_value(frame).unwrap(),
        json!({"captured_at":1,"width":1,"height":1,"mime_type":"image/png","data":"AAAA"})
    );
    assert!(
        protocol::RemoteFrame::parse(
            r#"{"type":"observe","request_id":"r","surface_id":"screen","command":"evil"}"#
        )
        .is_err()
    );
    assert!(
        protocol::RemoteFrame::parse(r#"{"type":"observe_cancel","request_id":"bad/id"}"#).is_err()
    );
}
