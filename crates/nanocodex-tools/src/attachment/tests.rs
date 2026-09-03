use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
async fn catalog_call_result_and_drain_use_exact_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let catalog = recv_json(&mut socket).await;
        assert_eq!(catalog["type"], "catalog");
        assert_eq!(catalog.as_object().unwrap().len(), 2);
        assert_eq!(catalog["tools"][0]["definition"]["name"], "echo");
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
        .connect()
        .await
        .unwrap();
    assert_eq!(attachment.status(), AttachmentStatus::Ready);
    completed_rx.await.unwrap();
    attachment.detach().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn catalog_publishes_one_validated_machine_snapshot() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let catalog = recv_json(&mut socket).await;
        assert_eq!(
            catalog["machines"],
            json!([{
                "id": "vm",
                "name": "Build VM",
                "workspace": "/workspace",
                "capabilities": ["cpu:4", "filesystem", "vm"]
            }])
        );
        send_json(&mut socket, json!({"type":"ready"})).await;
        assert_eq!(recv_json(&mut socket).await, json!({"type":"drain"}));
        send_json(&mut socket, json!({"type":"draining"})).await;
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(EchoTool)
        .build()
        .unwrap();
    let machine = AttachmentMachine::new(
        "vm",
        " Build VM ",
        "/workspace",
        ["cpu:4", "filesystem", "vm"],
    )
    .unwrap();
    let (attachment, _) = tools
        .attach(AttachmentTarget::new(endpoint, "bearer").unwrap())
        .machines([machine])
        .unwrap()
        .connect()
        .await
        .unwrap();
    attachment.detach().await.unwrap();
    server.await.unwrap();
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
