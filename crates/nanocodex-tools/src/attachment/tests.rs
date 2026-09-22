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
        assert_eq!(catalog.as_object().unwrap().len(), 5);
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
    assert_eq!(catalog["capabilities"], json!(["turn_metadata"]));
    send_json(&mut socket, json!({"type":"ready"})).await;
    socket
}

fn call(call_id: &str, name: &str) -> Value {
    json!({
        "type":"call",
        "session_id":"session-1",
        "call_id":call_id,
        "model":"gpt-6-sol",
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
        Ok(ToolOutput::json(&json!({"finished":true})))
    }
}

#[tokio::test]
async fn reconnect_preserves_running_calls_capacity_and_socket_local_results() {
    tokio::time::timeout(Duration::from_secs(5), async {
        for (parallel, count) in [(false, 1), (true, 32)] {
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
            let mut first = ready(&listener).await;
            for id in 0..count {
                send_json(&mut first, call(&format!("old-{id}"), "gated")).await;
                assert_eq!(starts.recv().await.unwrap(), format!("old-{id}"));
            }
            first.close(None).await.unwrap();
            let mut second = ready(&listener).await;
            send_json(&mut second, call("busy", "echo")).await;
            let busy = recv_json(&mut second).await;
            assert_eq!(busy["call_id"], "busy");
            assert_eq!(busy["outcome"]["status"], "unavailable");
            send_json(&mut second, json!({"type":"ack","call_id":"busy"})).await;

            // Let the replacement loop wait with the old calls still active.
            tokio::time::sleep(Duration::from_millis(25)).await;
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
            // Old completions do not wake the replacement socket.
            tokio::time::sleep(Duration::from_millis(25)).await;
            // Reusing an ID belongs to this socket; no old result may precede it.
            send_json(&mut second, call("old-0", "echo")).await;
            let result = recv_json(&mut second).await;
            assert_eq!(result["call_id"], "old-0");
            assert_eq!(result["outcome"]["status"], "completed");
            assert!(
                result["outcome"]["output"]["output"]
                    .as_str()
                    .unwrap()
                    .contains("hello")
            );
            send_json(&mut second, json!({"type":"ack","call_id":"old-0"})).await;
            let detach = tokio::spawn(async move { attachment.detach().await });
            assert_eq!(recv_json(&mut second).await, json!({"type":"drain"}));
            send_json(&mut second, json!({"type":"draining"})).await;
            detach.await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
}
