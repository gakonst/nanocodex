use std::{
    collections::HashSet,
    fs::File,
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eyre::{Result, WrapErr, ensure};
use futures_util::{SinkExt, StreamExt};
use nanocodex_oai_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolResult, Tools, WorkspaceTools,
    attachment::{
        AttachmentError, AttachmentMachine, AttachmentMetadata, AttachmentStatus, AttachmentTarget,
    },
    contract::async_trait,
};
use serde_json::{Value, json};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Notify,
};
use tokio_tungstenite::{WebSocketStream, accept_hdr_async, tungstenite::Message};

// Only the external CUA provider is substituted. Workspace commands, process
// retention and attachment transport run their shipped code.
struct PendingCua {
    name: &'static str,
    parallel: bool,
    started: Arc<Notify>,
    active: Arc<AtomicBool>,
}

struct ActiveCall(Arc<AtomicBool>);

impl Drop for ActiveCall {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[async_trait]
impl Tool for PendingCua {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name,
            "Synthetic external CUA provider waiting for its response",
            json!({"type":"object", "properties":{}, "additionalProperties":false}),
        )
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.parallel
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        self.active.store(true, Ordering::SeqCst);
        let _active = ActiveCall(Arc::clone(&self.active));
        self.started.notify_one();
        std::future::pending().await
    }
}

#[derive(Clone)]
struct Evidence {
    file: Arc<Mutex<File>>,
    started: Instant,
}

impl Evidence {
    fn record(&self, connection: &str, direction: &str, value: &Value) {
        let mut file = self.file.lock().unwrap();
        writeln!(
            file,
            "{}",
            json!({
                "elapsed_ms": self.started.elapsed().as_millis(),
                "connection": connection,
                "direction": direction,
                "frame": value,
            })
        )
        .unwrap();
        file.flush().unwrap();
    }
}

struct Wire {
    socket: WebSocketStream<TcpStream>,
    evidence: Evidence,
    connection: &'static str,
    pending: HashSet<String>,
    catalog: Value,
    upgrade: Value,
}

impl Wire {
    async fn ready(
        listener: &TcpListener,
        evidence: Evidence,
        connection: &'static str,
    ) -> Result<Self> {
        let (stream, _) = listener.accept().await?;
        Self::upgrade(stream, evidence, connection).await
    }

    async fn upgrade(
        stream: TcpStream,
        evidence: Evidence,
        connection: &'static str,
    ) -> Result<Self> {
        let mut upgrade = json!({});
        let socket = accept_hdr_async(
            stream,
            |request: &http::Request<()>, response: http::Response<()>| {
                for name in [
                    "x-nanocodex-request-id",
                    "x-nanocodex-hand-machine-id",
                    "x-nanocodex-hand-runtime-id",
                    "x-nanocodex-hand-region",
                ] {
                    upgrade[name] = request
                        .headers()
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .into();
                }
                upgrade["path"] = request.uri().path().into();
                Ok(response)
            },
        )
        .await?;
        evidence.record(connection, "upgrade", &upgrade);
        let mut wire = Self {
            socket,
            evidence,
            connection,
            pending: HashSet::new(),
            catalog: Value::Null,
            upgrade,
        };
        wire.catalog = wire.recv(Duration::from_secs(5)).await?;
        ensure!(wire.catalog["type"] == "catalog", "missing catalog");
        wire.send(json!({"type":"ready"})).await?;
        Ok(wire)
    }

    async fn send(&mut self, frame: Value) -> Result<()> {
        self.evidence
            .record(self.connection, "remote_to_executor", &frame);
        self.socket
            .send(Message::Text(frame.to_string().into()))
            .await?;
        Ok(())
    }

    async fn call(&mut self, id: &str, name: &str, input: Value) -> Result<()> {
        self.call_before(id, name, input, now_ms() + 30_000).await
    }

    async fn call_before(
        &mut self,
        id: &str,
        name: &str,
        input: Value,
        deadline: u64,
    ) -> Result<()> {
        self.pending.insert(id.to_owned());
        self.send(json!({
            "type":"call", "session_id":"synthetic-session", "turn_id":"synthetic-turn:1",
            "call_id":id, "model":"synthetic-model", "name":name, "input":input,
            "output_token_budget":1000, "output_byte_budget":131072,
            "deadline_at":deadline,
        }))
        .await
    }

    async fn recv(&mut self, limit: Duration) -> Result<Value> {
        tokio::time::timeout(limit, async {
            loop {
                let message = self
                    .socket
                    .next()
                    .await
                    .ok_or_else(|| eyre::eyre!("socket closed"))??;
                match message {
                    Message::Text(text) => {
                        let frame: Value = serde_json::from_str(&text)?;
                        self.evidence
                            .record(self.connection, "executor_to_remote", &frame);
                        ensure!(
                            frame["type"] != "ping" && frame["type"] != "pong",
                            "JSON heartbeat is forbidden"
                        );
                        if frame["type"] == "diagnostic" {
                            continue;
                        }
                        return Ok(frame);
                    }
                    Message::Ping(payload) => {
                        self.evidence.record(
                            self.connection,
                            "control_ping",
                            &json!({"bytes":payload.len()}),
                        );
                        self.socket.send(Message::Pong(payload)).await?;
                    }
                    Message::Pong(_) => {}
                    other => eyre::bail!("unexpected socket frame {other:?}"),
                }
            }
        })
        .await
        .wrap_err("attachment response did not arrive within the progress bound")?
    }

    async fn result(&mut self, expected: &str, limit: Duration) -> Result<Value> {
        let frame = self.recv(limit).await?;
        ensure!(frame["type"] == "result", "expected result, got {frame}");
        let id = frame["call_id"]
            .as_str()
            .ok_or_else(|| eyre::eyre!("missing call id"))?;
        self.pending.remove(id);
        self.send(json!({"type":"ack", "call_id":id})).await?;
        ensure!(
            id == expected,
            "expected {expected} to progress first, got {frame}"
        );
        Ok(frame)
    }

    async fn cancel_pending(&mut self) -> Result<()> {
        for id in self.pending.clone() {
            self.send(json!({"type":"cancel", "call_id":id})).await?;
        }
        while !self.pending.is_empty() {
            let frame = self.recv(Duration::from_secs(5)).await?;
            ensure!(
                frame["type"] == "result",
                "expected cancellation result: {frame}"
            );
            let id = frame["call_id"]
                .as_str()
                .ok_or_else(|| eyre::eyre!("missing call id"))?;
            ensure!(
                self.pending.remove(id),
                "unexpected cancellation result: {frame}"
            );
            self.send(json!({"type":"ack", "call_id":id})).await?;
        }
        Ok(())
    }

    async fn drain(&mut self) -> Result<()> {
        ensure!(
            self.recv(Duration::from_secs(5)).await? == json!({"type":"drain"}),
            "missing drain"
        );
        self.send(json!({"type":"draining"})).await
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

fn shell(command: &str) -> Value {
    json!({"cmd":command, "shell":"/bin/sh", "login":false, "yield_time_ms":1000})
}

fn successful_process(frame: &Value) -> Result<&Value> {
    ensure!(
        frame["outcome"]["status"] == "completed",
        "call failed: {frame}"
    );
    ensure!(
        frame["outcome"]["output"]["success"] == true,
        "tool failed: {frame}"
    );
    Ok(&frame["outcome"]["output"]["structured_result"])
}

async fn journey(
    wire: &mut Wire,
    workspace: &std::path::Path,
    started: &Notify,
    active: &AtomicBool,
) -> Result<()> {
    let catalog = wire.catalog["tools"]
        .as_array()
        .ok_or_else(|| eyre::eyre!("missing tools catalog"))?;
    for (name, timeout) in [("exec_command", 40_000), ("write_stdin", 310_000)] {
        let entry = catalog
            .iter()
            .find(|entry| entry["definition"]["name"] == name)
            .ok_or_else(|| eyre::eyre!("missing {name} in catalog"))?;
        ensure!(
            entry["parallel_safe"] == true && entry["timeout_ms"] == timeout,
            "unexpected public shell execution contract: {entry}"
        );
    }
    wire.call("cua", "cua_pending", json!({})).await?;
    tokio::time::timeout(Duration::from_secs(2), started.notified()).await?;
    ensure!(active.load(Ordering::SeqCst), "CUA never started");
    wire.evidence
        .record("owner", "observation", &json!({"cua_active":true}));

    wire.call(
        "printf",
        "exec_command",
        shell("printf 'attachment-progress'"),
    )
    .await?;
    let frame = wire
        .result("printf", Duration::from_secs(2))
        .await
        .wrap_err("real printf stalled while the parallel CUA call was active")?;
    let process = successful_process(&frame)?;
    ensure!(
        process["exit_code"] == 0 && process["output"] == "attachment-progress",
        "unexpected printf: {frame}"
    );
    ensure!(
        active.load(Ordering::SeqCst),
        "CUA completed before shell progress"
    );

    wire.call(
        "session",
        "exec_command",
        shell("printf 'session-start\n'; sleep 5; printf 'session-finish\n'"),
    )
    .await?;
    let frame = wire.result("session", Duration::from_secs(2)).await?;
    let process = successful_process(&frame)?;
    ensure!(
        process["output"] == "session-start\n",
        "missing initial process output: {frame}"
    );
    let session = process["session_id"]
        .as_i64()
        .ok_or_else(|| eyre::eyre!("process was not retained: {frame}"))?;

    // The process belongs to this attachment runtime. A second public Tools
    // recipe at the same workspace must not address it by the numeric ID.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let other_tools = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(workspace))
        .build()?;
    let target = AttachmentTarget::new(
        format!("ws://{}/tools", listener.local_addr()?),
        "synthetic-bearer",
    )?;
    let (other_wire, other_attachment) = tokio::join!(
        Wire::ready(&listener, wire.evidence.clone(), "other-runtime"),
        other_tools.attach(target).connect(),
    );
    let mut other_wire = other_wire?;
    let (other_attachment, _events) = other_attachment?;
    other_wire
        .call(
            "foreign-poll",
            "write_stdin",
            json!({"session_id":session, "chars":"", "yield_time_ms":5000}),
        )
        .await?;
    let ownership = other_wire
        .result("foreign-poll", Duration::from_secs(2))
        .await?;
    let (drain, detach) = tokio::join!(other_wire.drain(), other_attachment.detach());
    drain?;
    detach?;
    ensure!(
        ownership["outcome"]["status"] == "completed"
            && ownership["outcome"]["output"]["success"] == false,
        "foreign process was accessible: {ownership}"
    );
    ensure!(
        ownership.to_string().contains("Unknown process id"),
        "unexpected ownership error: {ownership}"
    );

    wire.call(
        "poll",
        "write_stdin",
        json!({"session_id":session, "chars":"", "yield_time_ms":5000}),
    )
    .await?;
    wire.call(
        "concurrent-printf",
        "exec_command",
        shell("printf 'during-poll'"),
    )
    .await?;
    let concurrent = wire
        .result("concurrent-printf", Duration::from_secs(2))
        .await?;
    let process = successful_process(&concurrent)?;
    ensure!(
        process["exit_code"] == 0 && process["output"] == "during-poll",
        "concurrent command failed: {concurrent}"
    );
    let polled = wire.result("poll", Duration::from_secs(6)).await?;
    let process = successful_process(&polled)?;
    ensure!(
        process["exit_code"] == 0
            && process["output"] == "session-finish\n"
            && process["session_id"].is_null(),
        "retained process did not finish: {polled}"
    );
    ensure!(
        active.load(Ordering::SeqCst),
        "CUA did not remain active through both shell calls"
    );

    // A shell yield longer than the hosted call deadline returns the live
    // session before that deadline rather than letting the broker expire it.
    let clamped_started = Instant::now();
    wire.call_before(
        "clamped",
        "exec_command",
        json!({"cmd":"printf 'clamped-start\n'; sleep 30", "shell":"/bin/sh", "login":false, "yield_time_ms":20_000}),
        now_ms() + 5_000,
    )
    .await?;
    let clamped = wire.result("clamped", Duration::from_secs(5)).await?;
    let process = successful_process(&clamped)?;
    ensure!(
        clamped_started.elapsed() < Duration::from_secs(5)
            && process["output"] == "clamped-start\n"
            && process["session_id"].is_i64()
            && process["exit_code"].is_null(),
        "shell yield was not clamped before the call deadline: {clamped}"
    );

    wire.send(json!({"type":"cancel", "call_id":"cua"})).await?;
    let cancelled = wire.result("cua", Duration::from_secs(2)).await?;
    ensure!(
        cancelled["outcome"]["status"] == "ambiguous",
        "unexpected cancellation receipt: {cancelled}"
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while active.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    wire.call("exclusive-cua", "cua_exclusive", json!({}))
        .await?;
    tokio::time::timeout(Duration::from_secs(2), started.notified()).await?;
    ensure!(active.load(Ordering::SeqCst), "exclusive CUA never started");
    wire.call(
        "concurrent-declared-serial",
        "exec_command",
        shell("printf 'during-declared-serial'"),
    )
    .await?;
    let concurrent = wire
        .result("concurrent-declared-serial", Duration::from_secs(2))
        .await?;
    let process = successful_process(&concurrent)?;
    ensure!(
        process["exit_code"] == 0 && process["output"] == "during-declared-serial",
        "provider metadata blocked the concurrent shell: {concurrent}"
    );
    ensure!(
        active.load(Ordering::SeqCst),
        "declared-serial CUA stopped before the concurrent shell finished"
    );
    wire.call_before(
        "expired-printf",
        "exec_command",
        shell("printf 'expired-should-not-run' > expired-must-not-exist"),
        now_ms() - 1,
    )
    .await?;
    let expired = wire
        .result("expired-printf", Duration::from_secs(2))
        .await?;
    ensure!(
        expired["outcome"]["status"] == "unavailable",
        "expired call was dispatched: {expired}"
    );
    ensure!(
        !workspace.join("expired-must-not-exist").exists(),
        "expired shell executed its side effect"
    );
    wire.send(json!({"type":"cancel", "call_id":"exclusive-cua"}))
        .await?;
    let cancelled = wire.result("exclusive-cua", Duration::from_secs(2)).await?;
    ensure!(
        cancelled["outcome"]["status"] == "ambiguous",
        "unexpected exclusive cancellation: {cancelled}"
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while active.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    wire.call(
        "recovered-printf",
        "exec_command",
        shell("printf 'after-cancel'"),
    )
    .await?;
    let recovered = wire
        .result("recovered-printf", Duration::from_secs(2))
        .await?;
    let process = successful_process(&recovered)?;
    ensure!(
        process["exit_code"] == 0 && process["output"] == "after-cancel",
        "shell did not recover after CUA cancellation: {recovered}"
    );
    ensure!(
        !workspace.join("expired-must-not-exist").exists(),
        "expired shell ran after provider cancellation"
    );
    wire.evidence.record(
        "owner",
        "observation",
        &json!({"cua_active":false, "journey":"passed"}),
    );
    Ok(())
}

#[tokio::test]
async fn pending_parallel_cua_does_not_stall_workspace_shell_or_session_poll() {
    let _runtime_lock = crate::TOOL_RUNTIME_TEST_LOCK.lock().await;
    let workspace = tempfile::tempdir().unwrap();
    let output =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/attachment-progress");
    std::fs::create_dir_all(&output).unwrap();
    let path = output.join(format!("wire-{}.jsonl", now_ms()));
    let evidence = Evidence {
        file: Arc::new(Mutex::new(File::create(&path).unwrap())),
        started: Instant::now(),
    };
    eprintln!("Attachment journey wire evidence: {}", path.display());
    let started = Arc::new(Notify::new());
    let active = Arc::new(AtomicBool::new(false));
    let tools = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(workspace.path()))
        .tool(PendingCua {
            name: "cua_pending",
            parallel: true,
            started: Arc::clone(&started),
            active: Arc::clone(&active),
        })
        .tool(PendingCua {
            name: "cua_exclusive",
            parallel: false,
            started: Arc::clone(&started),
            active: Arc::clone(&active),
        })
        .build()
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = AttachmentTarget::new(
        format!("ws://{}/tools", listener.local_addr().unwrap()),
        "synthetic-bearer",
    )
    .unwrap();
    let (wire, attachment) = tokio::join!(
        Wire::ready(&listener, evidence.clone(), "owner"),
        tools.attach(target).connect()
    );
    let mut wire = wire.unwrap();
    let (attachment, _events) = attachment.unwrap();
    let outcome = journey(&mut wire, workspace.path(), &started, &active).await;
    if let Err(error) = &outcome {
        evidence.record(
            "owner",
            "observation",
            &json!({"journey":"failed", "error":format!("{error:#}")}),
        );
    }
    // Even the failing baseline cancels queued/in-flight calls, acknowledges
    // every receipt, and drains the socket before reporting the failure.
    let cleanup = wire.cancel_pending().await;
    let (drain, detach) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(wire.drain(), attachment.detach())
    })
    .await
    .expect("attachment cleanup exceeded its progress bound");
    cleanup.unwrap();
    drain.unwrap();
    detach.unwrap();
    assert!(
        !active.load(Ordering::SeqCst),
        "CUA survived attachment shutdown"
    );
    outcome.unwrap();
}

#[tokio::test]
async fn retained_shell_receipts_recover_offline_without_reexecution() -> Result<()> {
    let _runtime_lock = crate::TOOL_RUNTIME_TEST_LOCK.lock().await;
    let workspace = tempfile::tempdir()?;
    let output =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/command-recovery");
    std::fs::create_dir_all(&output)?;
    let path = output.join(format!("native-recovery-{}.jsonl", now_ms()));
    let evidence = Evidence {
        file: Arc::new(Mutex::new(File::create(&path)?)),
        started: Instant::now(),
    };
    eprintln!("Native recovery public wire evidence: {}", path.display());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let target = AttachmentTarget::new(
        format!("ws://{}/tools", listener.local_addr()?),
        "synthetic-bearer",
    )?;
    let tools = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(workspace.path()))
        .build()?;
    let (attachment, _events) = tools.clone().attach(target.clone()).start()?;
    let mut first = Wire::ready(&listener, evidence.clone(), "dispatch").await?;
    ensure!(
        first.catalog["command_recovery"] == true,
        "native recovery not advertised"
    );
    let runtime_id = first.catalog["runtime_id"].clone();
    let command = json!({
        "type":"call", "session_id":"synthetic-session", "turn_id":"synthetic-turn:1",
        "call_id":"offline-shell", "model":"synthetic-model", "name":"exec_command",
        "input":{"cmd":"printf 'effect\n' >> effects; touch started; while [ ! -f release ]; do sleep 0.02; done; printf recovered-output; touch finished", "shell":"/bin/sh", "login":false, "yield_time_ms":30000},
        "output_token_budget":1000, "output_byte_budget":131072, "deadline_at":now_ms()+60_000,
    });
    first.send(command.clone()).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !workspace.path().join("started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    first.socket.close(None).await?;
    // The real shell commits its only effect and exits while no socket is ready.
    std::fs::write(workspace.path().join("release"), "release")?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !workspace.path().join("finished").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    evidence.record("dispatch", "observation", &json!({"shell_finished":true, "replacement_ready":false, "effect_count":std::fs::read_to_string(workspace.path().join("effects"))?.lines().count()}));
    let mut second = Wire::ready(&listener, evidence.clone(), "offline-recovery").await?;
    ensure!(
        second.catalog["runtime_id"] == runtime_id,
        "runtime identity changed"
    );
    let receipt = second.recv(Duration::from_secs(5)).await?;
    let process = successful_process(&receipt)?;
    ensure!(
        process["output"] == "recovered-output" && process["exit_code"] == 0,
        "offline output was lost: {receipt}"
    );
    ensure!(
        std::fs::read_to_string(workspace.path().join("effects"))? == "effect\n",
        "shell executed more than once"
    );
    // Lose ACK, reconnect, and ask for recovery. Every terminal replay is identical.
    second.socket.close(None).await?;
    let mut third = Wire::ready(&listener, evidence.clone(), "lost-ack-recovery").await?;
    ensure!(
        third.recv(Duration::from_secs(5)).await? == receipt,
        "ready replay changed terminal receipt"
    );
    third
        .send(json!({"type":"recover","call_ids":["offline-shell","never-dispatched"]}))
        .await?;
    ensure!(
        third.recv(Duration::from_secs(5)).await? == receipt,
        "recover changed terminal receipt"
    );
    ensure!(
        third.recv(Duration::from_secs(5)).await?
            == json!({"type":"status","call_id":"never-dispatched","state":"missing"}),
        "missing recovery executed a command"
    );
    third.send(command).await?;
    ensure!(
        third.recv(Duration::from_secs(5)).await? == receipt,
        "duplicate immutable call was reexecuted"
    );
    third
        .send(json!({"type":"ack","call_id":"offline-shell"}))
        .await?;
    third
        .send(json!({"type":"recover","call_ids":["offline-shell"]}))
        .await?;
    ensure!(
        third.recv(Duration::from_secs(5)).await?
            == json!({"type":"status","call_id":"offline-shell","state":"missing"}),
        "ACK did not release terminal journal entry"
    );

    // Cancellation arriving after transport recovery aborts the retained task.
    third.call("cancel-offline", "exec_command", json!({
        "cmd":"touch cancel-started; while [ ! -f cancel-release ]; do sleep 0.02; done; touch cancel-must-not-finish",
        "shell":"/bin/sh", "login":false, "yield_time_ms":30000,
    })).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !workspace.path().join("cancel-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    third.socket.close(None).await?;
    let mut fourth = Wire::ready(&listener, evidence.clone(), "offline-cancel").await?;
    fourth
        .send(json!({"type":"recover","call_ids":["cancel-offline"]}))
        .await?;
    ensure!(
        fourth.recv(Duration::from_secs(5)).await?
            == json!({"type":"status","call_id":"cancel-offline","state":"running"}),
        "running task not retained"
    );
    // ACK cannot delete running work.
    fourth
        .send(json!({"type":"ack","call_id":"cancel-offline"}))
        .await?;
    fourth
        .send(json!({"type":"cancel","call_id":"cancel-offline"}))
        .await?;
    let cancelled = fourth
        .result("cancel-offline", Duration::from_secs(5))
        .await?;
    ensure!(
        cancelled["outcome"]["status"] == "ambiguous",
        "offline cancellation lost: {cancelled}"
    );
    let (drain, detach) = tokio::join!(fourth.drain(), attachment.detach());
    drain?;
    detach?;
    std::fs::write(workspace.path().join("cancel-release"), "release")?;
    ensure!(
        !workspace.path().join("cancel-must-not-finish").exists(),
        "cancelled task survived runtime shutdown"
    );

    // A new executor runtime has no proof for calls from the previous runtime.
    let (replacement, _events) = tools.attach(target).start()?;
    let mut fresh = Wire::ready(&listener, evidence.clone(), "new-runtime").await?;
    ensure!(
        fresh.catalog["runtime_id"] != runtime_id,
        "new executor reused ownership epoch"
    );
    fresh
        .send(json!({"type":"recover","call_ids":["offline-shell","cancel-offline"]}))
        .await?;
    for id in ["offline-shell", "cancel-offline"] {
        ensure!(
            fresh.recv(Duration::from_secs(5)).await?
                == json!({"type":"status","call_id":id,"state":"missing"}),
            "new runtime claimed previous proof"
        );
    }
    fresh
        .call(
            "detach-no-ack",
            "exec_command",
            shell("printf bounded-detach"),
        )
        .await?;
    let no_ack = fresh.recv(Duration::from_secs(5)).await?;
    ensure!(
        no_ack["type"] == "result",
        "missing terminal receipt before bounded detach"
    );
    let retained_handle = replacement.clone();
    let (drain, detach) = tokio::time::timeout(Duration::from_secs(12), async {
        // The peer deliberately withholds terminal ACK while acknowledging drain.
        tokio::join!(fresh.drain(), replacement.detach())
    })
    .await
    .wrap_err("native detach hung waiting for receipt ACK")?;
    drain?;
    detach?;
    ensure!(
        retained_handle.status() == nanocodex_oai_tools::attachment::AttachmentStatus::Disconnected,
        "detached handle still advertises ready"
    );
    evidence.record(
        "new-runtime",
        "assertion",
        &json!({"detach_without_receipt_ack":"closed within 12s", "status":"disconnected"}),
    );
    let effect_count = std::fs::read_to_string(workspace.path().join("effects"))?
        .lines()
        .count();
    ensure!(effect_count == 1, "recovery duplicated shell side effects");
    evidence.record(
        "observation",
        "assertion",
        &json!({
            "journey":"passed", "effect_count":effect_count, "recovery_call_frames":0,
            "initial_call_frames":1, "intentional_duplicate_call_frames":1,
            "exit_code":process["exit_code"], "output":process["output"],
            "receipt_replays_identical":true, "new_runtime_proof":"missing",
        }),
    );
    Ok(())
}

// A reproducible native executable benchmark at the public attachment boundary.
// Every sample runs a real shell and retains/acknowledges its actual wire receipt.
#[tokio::test]
async fn native_shell_call_latency_over_public_websocket() -> Result<()> {
    let _runtime_lock = crate::TOOL_RUNTIME_TEST_LOCK.lock().await;
    let workspace = tempfile::tempdir()?;
    let output =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/native-shell-latency");
    std::fs::create_dir_all(&output)?;
    let path = output.join(format!("wire-{}.jsonl", now_ms()));
    let evidence = Evidence {
        file: Arc::new(Mutex::new(File::create(&path)?)),
        started: Instant::now(),
    };
    eprintln!("Native shell latency wire evidence: {}", path.display());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let target = AttachmentTarget::new(
        format!("ws://{}/tools", listener.local_addr()?),
        "synthetic-bearer",
    )?;
    let tools = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(workspace.path()))
        .build()?;
    let (wire, attachment) = tokio::join!(
        Wire::ready(&listener, evidence.clone(), "benchmark"),
        tools.attach(target).connect()
    );
    let mut wire = wire?;
    let (attachment, _events) = attachment?;
    let measured = async {
        for tty in [false, true] {
            let mut samples = Vec::new();
            for index in 0..63 {
                let id = format!("shell-{}-{index}", if tty { "pty" } else { "pipe" });
                let expected = format!("sample-{index}");
                let mut input = shell(&format!("printf '{expected}'; exit 23"));
                input["tty"] = json!(tty);
                let started = Instant::now();
                wire.call(&id, "exec_command", input).await?;
                let receipt = wire.result(&id, Duration::from_secs(5)).await?;
                let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                let process = successful_process(&receipt)?;
                ensure!(
                    process["exit_code"] == 23,
                    "lost native exit status: {receipt}"
                );
                ensure!(
                    process["output"] == expected,
                    "lost native output: {receipt}"
                );
                ensure!(
                    process["session_id"].is_null(),
                    "completed shell retained as running"
                );
                evidence.record(
                    "benchmark",
                    "sample",
                    &json!({
                        "tty":tty, "index":index, "warmup":index < 3,
                        "roundtrip_ms":elapsed_ms,
                        "shell_wall_ms":process["wall_time_seconds"].as_f64().unwrap() * 1000.0,
                        "timing":receipt["timing"],
                    }),
                );
                if index >= 3 {
                    samples.push(elapsed_ms);
                }
            }
            samples.sort_by(f64::total_cmp);
            let summary = json!({
                "tty":tty, "samples":samples.len(),
                "p50_ms":samples[samples.len()/2],
                "p95_ms":samples[samples.len()*95/100],
                "min_ms":samples[0], "max_ms":samples[samples.len()-1],
            });
            evidence.record("benchmark", "summary", &summary);
            eprintln!("Native shell latency {summary}");
        }
        // Child notifications are coalesced and shared process-wide. A burst
        // must still complete each real child with its own output/exit status.
        for index in 0..24 {
            let id = format!("concurrent-{index}");
            let mut input = shell(&format!("sleep 0.02; printf '{id}'; exit 23"));
            input["tty"] = json!(index % 2 == 0);
            wire.call(&id, "exec_command", input).await?;
        }
        let mut observed = HashSet::new();
        while observed.len() < 24 {
            let receipt = wire.recv(Duration::from_secs(5)).await?;
            ensure!(
                receipt["type"] == "result",
                "missing concurrent receipt: {receipt}"
            );
            let id = receipt["call_id"].as_str().unwrap();
            ensure!(
                wire.pending.remove(id),
                "unexpected concurrent receipt: {receipt}"
            );
            ensure!(
                observed.insert(id.to_owned()),
                "duplicate concurrent receipt: {receipt}"
            );
            let process = successful_process(&receipt)?;
            ensure!(
                process["output"] == id && process["exit_code"] == 23,
                "crossed or incomplete concurrent child result: {receipt}"
            );
            wire.send(json!({"type":"ack", "call_id":id})).await?;
        }
        evidence.record(
            "benchmark",
            "observation",
            &json!({"concurrent_children":24, "status":"passed"}),
        );
        Ok::<(), eyre::Report>(())
    }
    .await;
    let cleanup = wire.cancel_pending().await;
    let (drain, detach) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(wire.drain(), attachment.detach())
    })
    .await
    .wrap_err("benchmark cleanup exceeded its progress bound")?;
    cleanup?;
    drain?;
    detach?;
    measured
}

// Portable PTY execution does not require Tokio's I/O/signal reactor. Exercise
// the public workspace API on a time-only runtime, not a private waiter helper.
#[cfg(unix)]
#[test]
fn portable_pty_on_time_only_runtime_retains_output_status_and_session() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime.block_on(async {
        let _runtime_lock = crate::TOOL_RUNTIME_TEST_LOCK.lock().await;
        let workspace = tempfile::tempdir()?;
        let tools = nanocodex_oai_tools::workspace_runtime::WorkspaceToolRuntime::new(
            workspace.path().to_path_buf(),
        );
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/native-shell-latency");
        std::fs::create_dir_all(&output)?;
        let path = output.join(format!("time-only-pty-{}.jsonl", now_ms()));
        let evidence = Evidence {
            file: Arc::new(Mutex::new(File::create(&path)?)),
            started: Instant::now(),
        };
        eprintln!("Time-only public PTY evidence: {}", path.display());
        for (id, command, yield_ms) in [
            ("completed", "printf time-only; exit 23", 1000),
            (
                "yielded",
                "printf retained; sleep 0.6; printf done; exit 17",
                250,
            ),
        ] {
            let input = json!({
                "cmd":command, "shell":"/bin/sh", "login":false,
                "tty":true, "yield_time_ms":yield_ms,
            });
            evidence.record("time-only", "input", &input);
            let result = tools
                .execute_tool(
                    "exec_command",
                    ToolInput::Function(serde_json::value::to_raw_value(&input)?),
                    ToolContext::new("synthetic-model", "synthetic-session", id, &[], 1000),
                )
                .await;
            let result = result.structured_result();
            evidence.record("time-only", "result", &result);
            if id == "completed" {
                ensure!(
                    result["exit_code"] == 23 && result["output"] == "time-only",
                    "time-only PTY completion failed: {result}"
                );
            } else {
                let session = result["session_id"].as_i64().ok_or_else(|| {
                    eyre::eyre!("time-only PTY did not retain its session: {result}")
                })?;
                let initial = result["output"].as_str().unwrap_or_default().to_owned();
                // Let the real child finish with no active execution/poll call.
                tokio::time::sleep(Duration::from_millis(700)).await;
                let input = json!({"session_id":session, "chars":"", "yield_time_ms":5000});
                evidence.record("time-only", "input", &input);
                let polled = tools
                    .execute_tool(
                        "write_stdin",
                        ToolInput::Function(serde_json::value::to_raw_value(&input)?),
                        ToolContext::new("synthetic-model", "synthetic-session", "poll", &[], 1000),
                    )
                    .await
                    .structured_result();
                evidence.record("time-only", "result", &polled);
                ensure!(
                    polled["exit_code"] == 17
                        && initial + polled["output"].as_str().unwrap_or_default()
                            == "retaineddone",
                    "time-only retained PTY output/status lost: {polled}"
                );
            }
        }
        tools.control().cancel().await;
        evidence.record(
            "time-only",
            "observation",
            &json!({"status":"passed", "io_driver":false}),
        );
        Ok(())
    })
}

// Each case gets an isolated process environment, avoiding global environment
// mutation while the integration harness runs other native journeys concurrently.
#[test]
fn regional_hand_upgrade_identity_journey() -> Result<()> {
    const CASE: &str = "NANOCODEX_TEST_REGIONAL_HAND_CASE";
    if let Ok(case) = std::env::var(CASE) {
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(regional_hand_upgrade_case(&case));
    }
    for (case, flag) in [
        ("default", None),
        ("regional", Some("1")),
        ("scoped", Some("1")),
        ("named", Some("1")),
    ] {
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                "attachment::regional_hand_upgrade_identity_journey",
                "--nocapture",
            ])
            .env(CASE, case)
            .env_remove("NANOCODEX_REGIONAL_HAND_RELAYS");
        if let Some(flag) = flag {
            command.env("NANOCODEX_REGIONAL_HAND_RELAYS", flag);
        }
        let output = command.output()?;
        eprintln!(
            "case={case}\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        ensure!(
            output.status.success(),
            "regional Hand journey failed: {case}"
        );
    }
    Ok(())
}

async fn regional_hand_upgrade_case(case: &str) -> Result<()> {
    let output =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/regional-hand-upgrade");
    std::fs::create_dir_all(&output)?;
    let path = output.join(format!("{case}-{}.jsonl", now_ms()));
    let evidence = Evidence {
        file: Arc::new(Mutex::new(File::create(&path)?)),
        started: Instant::now(),
    };
    eprintln!("Regional Hand upgrade wire evidence: {}", path.display());
    tokio::time::timeout(Duration::from_secs(10), async {
        let workspace = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = if case == "scoped" {
            "/v1/agents/synthetic-agent/tool-host"
        } else {
            "/v1/account/tool-host"
        };
        let metadata = if case == "named" {
            AttachmentMetadata::named("synthetic-machine")?
        } else {
            AttachmentMetadata::machine(AttachmentMachine::new(
                "synthetic-machine",
                "Synthetic Hand",
                workspace.path().display().to_string(),
                ["shell"],
            )?)
        };
        let (attachment, _events) = Tools::builder()
            .without_defaults()
            .add(WorkspaceTools::new(workspace.path()))
            .build()?
            .attach(AttachmentTarget::new(
                format!("ws://{}{endpoint}", listener.local_addr()?),
                "synthetic-bearer",
            )?)
            .metadata(metadata)
            .start()?;
        let mut first = Wire::ready(&listener, evidence.clone(), "initial").await?;
        first.socket.close(None).await?;
        drop(first.socket);
        let mut second = Wire::ready(&listener, evidence.clone(), "reconnect").await?;
        while attachment.status() != AttachmentStatus::Ready {
            tokio::task::yield_now().await;
        }
        for (catalog, upgrade) in [
            (&first.catalog, &first.upgrade),
            (&second.catalog, &second.upgrade),
        ] {
            ensure!(
                upgrade["path"] == endpoint,
                "wrong upgrade endpoint: {upgrade}"
            );
            ensure!(
                upgrade["x-nanocodex-hand-region"].is_null(),
                "caller supplied region"
            );
            ensure!(
                upgrade["x-nanocodex-request-id"] == catalog["connection_id"],
                "connection identity mismatch"
            );
            ensure!(
                catalog["attachment_id"] == "synthetic-machine",
                "wrong attachment identity"
            );
            if case == "regional" || case == "default" {
                ensure!(
                    catalog["machines"].as_array().map(Vec::len) == Some(1),
                    "expected one machine"
                );
                ensure!(
                    upgrade["x-nanocodex-hand-machine-id"] == catalog["machines"][0]["id"],
                    "upgrade machine differs from catalog"
                );
                ensure!(
                    upgrade["x-nanocodex-hand-machine-id"] == catalog["attachment_id"],
                    "upgrade machine differs from attachment"
                );
                ensure!(
                    upgrade["x-nanocodex-hand-runtime-id"] == catalog["runtime_id"],
                    "upgrade runtime differs from catalog"
                );
                ensure!(
                    catalog["runtime_id"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty()),
                    "missing runtime"
                );
            } else {
                ensure!(
                    upgrade["x-nanocodex-hand-machine-id"].is_null(),
                    "unexpected machine header: {case}"
                );
                ensure!(
                    upgrade["x-nanocodex-hand-runtime-id"].is_null(),
                    "unexpected runtime header: {case}"
                );
            }
        }
        ensure!(
            first.catalog["runtime_id"] == second.catalog["runtime_id"],
            "runtime changed on reconnect"
        );
        ensure!(
            first.upgrade["x-nanocodex-hand-machine-id"]
                == second.upgrade["x-nanocodex-hand-machine-id"],
            "machine header changed on reconnect"
        );
        ensure!(
            first.upgrade["x-nanocodex-hand-runtime-id"]
                == second.upgrade["x-nanocodex-hand-runtime-id"],
            "runtime header changed on reconnect"
        );
        ensure!(
            first.catalog["connection_id"] != second.catalog["connection_id"],
            "transport identity did not change"
        );
        let (drain, detached) = tokio::join!(second.drain(), attachment.detach());
        drain?;
        detached?;
        evidence.record(
            "reconnect",
            "observation",
            &json!({"case":case, "journey":"passed"}),
        );
        Ok(())
    })
    .await?
}

/// One WebSocket upgrade attempt observed by a scripted public endpoint.
#[derive(Clone)]
struct Attempt {
    at: Instant,
    status: u16,
    runtime_id: Option<String>,
}

/// Decides each upgrade attempt: a raw HTTP refusal, or None to accept it.
type Script = Arc<dyn Fn(usize, Instant) -> Option<String> + Send + Sync>;

struct ScriptedEndpoint {
    target: AttachmentTarget,
    attempts: Arc<Mutex<Vec<Attempt>>>,
    accepted: tokio::sync::mpsc::UnboundedReceiver<Result<Wire>>,
}

impl ScriptedEndpoint {
    async fn next_wire(&mut self, limit: Duration) -> Result<Wire> {
        tokio::time::timeout(limit, self.accepted.recv())
            .await
            .wrap_err("no accepted attachment within the bound")?
            .ok_or_else(|| eyre::eyre!("endpoint stopped"))?
    }

    fn attempts(&self) -> Vec<Attempt> {
        self.attempts.lock().unwrap().clone()
    }
}

// A real TCP listener speaking HTTP/1.1: refusals are genuine upgrade responses
// with status, headers and body, exactly what the account proxy returns.
async fn scripted_endpoint(
    path: &str,
    evidence: Evidence,
    script: Script,
) -> Result<ScriptedEndpoint> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let target = AttachmentTarget::new(
        format!("ws://{}{path}", listener.local_addr()?),
        "synthetic-bearer",
    )?;
    let attempts = Arc::new(Mutex::new(Vec::<Attempt>::new()));
    let recorded = Arc::clone(&attempts);
    let (accepted_tx, accepted) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let at = Instant::now();
            let index = recorded.lock().unwrap().len();
            let Some(response) = script(index, at) else {
                let wire = Wire::upgrade(stream, evidence.clone(), "accepted").await;
                if let Ok(wire) = &wire {
                    let attempt = Attempt {
                        at,
                        status: 101,
                        runtime_id: wire.upgrade["x-nanocodex-hand-runtime-id"]
                            .as_str()
                            .map(str::to_owned),
                    };
                    evidence.record("endpoint", "attempt", &attempt.json(&evidence));
                    recorded.lock().unwrap().push(attempt);
                }
                if accepted_tx.send(wire).is_err() {
                    break;
                }
                continue;
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&buffer[..read]),
                }
            }
            let head = String::from_utf8_lossy(&request).into_owned();
            let attempt = Attempt {
                at,
                status: response
                    .split_whitespace()
                    .nth(1)
                    .and_then(|code| code.parse().ok())
                    .unwrap_or_default(),
                runtime_id: head.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("x-nanocodex-hand-runtime-id")
                        .then(|| value.trim().to_owned())
                }),
            };
            evidence.record("endpoint", "attempt", &attempt.json(&evidence));
            recorded.lock().unwrap().push(attempt);
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    Ok(ScriptedEndpoint {
        target,
        attempts,
        accepted,
    })
}

impl Attempt {
    fn json(&self, evidence: &Evidence) -> Value {
        json!({
            "at_ms": millis(self.at.saturating_duration_since(evidence.started)),
            "status": self.status,
            "runtime_id": self.runtime_id,
        })
    }
}

fn http_refusal(status: &str, headers: &str, error: &str) -> String {
    let body = json!({ "error": error }).to_string();
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncache-control: no-store\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn millis(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1_000_000.0).round() / 1000.0
}

fn gaps(attempts: &[Attempt]) -> Vec<Duration> {
    attempts
        .windows(2)
        .map(|pair| pair[1].at.duration_since(pair[0].at))
        .collect()
}

fn reattach_evidence(name: &str) -> Result<Evidence> {
    let output =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/attachment-reattach");
    std::fs::create_dir_all(&output)?;
    let path = output.join(format!("{name}-{}.jsonl", now_ms()));
    eprintln!("Attachment reattach evidence: {}", path.display());
    Ok(Evidence {
        file: Arc::new(Mutex::new(File::create(&path)?)),
        started: Instant::now(),
    })
}

// Production delay policy (this binary links the non-test library build): a
// deploy that answers upgrades with HTTP 503 for several seconds must not grow
// the reconnect gap exponentially. The same runtime reattaches within the fast
// service cap, keeps its identity, and replays the receipt of a command that
// finished offline without executing it twice.
#[tokio::test]
async fn transient_service_outage_reattaches_quickly_with_same_runtime_and_receipt() -> Result<()> {
    const OUTAGE: Duration = Duration::from_secs(7);
    const FAST_BOUND: Duration = Duration::from_millis(2_500);
    let _runtime_lock = crate::TOOL_RUNTIME_TEST_LOCK.lock().await;
    let evidence = reattach_evidence("transient-503-outage")?;
    let workspace = tempfile::tempdir()?;
    let outage = Arc::new(Mutex::new(None::<Instant>));
    let scripted = Arc::clone(&outage);
    let mut endpoint = scripted_endpoint(
        "/v1/account/tool-host",
        evidence.clone(),
        Arc::new(move |_, at| {
            let started = (*scripted.lock().unwrap())?;
            (at.saturating_duration_since(started) < OUTAGE)
                .then(|| http_refusal("503 Service Unavailable", "", "managed_service_unavailable"))
        }),
    )
    .await?;
    let (attachment, _events) = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(workspace.path()))
        .build()?
        .attach(endpoint.target.clone())
        .metadata(AttachmentMetadata::machine(AttachmentMachine::new(
            "synthetic-machine",
            "Synthetic Hand",
            workspace.path().display().to_string(),
            ["shell"],
        )?))
        .start()?;
    let mut first = endpoint.next_wire(Duration::from_secs(5)).await?;
    let runtime_id = first.catalog["runtime_id"].clone();
    let command = json!({
        "type":"call", "session_id":"synthetic-session", "turn_id":"synthetic-turn:1",
        "call_id":"deploy-shell", "model":"synthetic-model", "name":"exec_command",
        "input":{"cmd":"printf 'effect\n' >> effects; touch started; while [ ! -f release ]; do sleep 0.02; done; printf survived-deploy; touch finished", "shell":"/bin/sh", "login":false, "yield_time_ms":30000},
        "output_token_budget":1000, "output_byte_budget":131072, "deadline_at":now_ms()+120_000,
    });
    first.send(command).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !workspace.path().join("started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    // Deploy: the service drops the socket and refuses upgrades for OUTAGE.
    let outage_started = Instant::now();
    *outage.lock().unwrap() = Some(outage_started);
    evidence.record("endpoint", "observation", &json!({"outage_started_ms": millis(outage_started.duration_since(evidence.started)), "outage_ms": millis(OUTAGE)}));
    first.socket.close(None).await?;
    drop(first);
    std::fs::write(workspace.path().join("release"), "release")?;
    let mut second = endpoint.next_wire(OUTAGE + Duration::from_secs(60)).await?;
    let attempts = endpoint.attempts();
    let outage_attempts: Vec<_> = attempts
        .iter()
        .filter(|attempt| attempt.at >= outage_started)
        .cloned()
        .collect();
    let recovered = outage_attempts
        .last()
        .ok_or_else(|| eyre::eyre!("no reattach attempt"))?;
    let latency = recovered
        .at
        .saturating_duration_since(outage_started + OUTAGE);
    let refused = outage_attempts.iter().filter(|a| a.status == 503).count();
    let gaps = gaps(&outage_attempts);
    let min_gap = gaps.iter().min().copied().unwrap_or_default();
    let max_gap = gaps.iter().max().copied().unwrap_or_default();
    let receipt = second.recv(Duration::from_secs(10)).await?;
    let effects = std::fs::read_to_string(workspace.path().join("effects"))?;
    let summary = json!({
        "scenario": "HTTP 503 deploy outage",
        "outage_ms": millis(OUTAGE),
        "refused_503_attempts": refused,
        "first_attempt_after_close_ms": outage_attempts.first().map(|a| millis(a.at.duration_since(outage_started))),
        "recovered_after_outage_start_ms": millis(recovered.at.duration_since(outage_started)),
        "recovery_latency_after_service_ready_ms": millis(latency),
        "attempt_gaps_ms": gaps.iter().copied().map(millis).collect::<Vec<_>>(),
        "min_gap_ms": millis(min_gap),
        "max_gap_ms": millis(max_gap),
        "effect_count": effects.lines().count(),
        "runtime_id_stable": attempts.iter().all(|a| a.runtime_id.as_deref() == runtime_id.as_str()),
    });
    evidence.record("endpoint", "summary", &summary);
    eprintln!("transient 503 outage metrics: {summary}");
    ensure!(
        second.catalog["runtime_id"] == runtime_id,
        "runtime identity changed across the outage"
    );
    ensure!(
        attempts
            .iter()
            .all(|attempt| attempt.runtime_id.as_deref() == runtime_id.as_str()),
        "a refused upgrade carried another runtime identity"
    );
    ensure!(refused >= 3, "the outage was not exercised: {summary}");
    ensure!(
        min_gap >= Duration::from_millis(90),
        "reconnect hot loop: {summary}"
    );
    ensure!(
        max_gap <= FAST_BOUND && latency <= FAST_BOUND,
        "transient 503 grew the reattach gap past the fast bound: {summary}"
    );
    ensure!(
        receipt["call_id"] == "deploy-shell"
            && successful_process(&receipt)?["output"] == "survived-deploy",
        "offline receipt was not replayed: {receipt}"
    );
    ensure!(effects == "effect\n", "command executed more than once");
    second
        .send(json!({"type":"ack","call_id":"deploy-shell"}))
        .await?;
    let (drain, detached) = tokio::join!(second.drain(), attachment.detach());
    drain?;
    detached?;
    Ok(())
}

// HTTP 429 and 503 Retry-After, as delta-seconds or an HTTP-date, are lower
// bounds: the driver never reconnects sooner than the service asked, then
// attaches normally. An absurd value parks the attempt without panicking and
// detach still interrupts it immediately.
#[tokio::test]
async fn transient_refusals_honor_retry_after() -> Result<()> {
    let evidence = reattach_evidence("retry-after")?;
    let workspace = tempfile::tempdir()?;
    let mut endpoint = scripted_endpoint(
        "/tools",
        evidence.clone(),
        Arc::new(|index, _| match index {
            0 => Some(http_refusal(
                "429 Too Many Requests",
                "retry-after: 1\r\n",
                "rate_limited",
            )),
            1 => Some(http_refusal(
                "503 Service Unavailable",
                "retry-after: 2\r\n",
                "tool_router_unavailable",
            )),
            2 => Some(http_refusal(
                "503 Service Unavailable",
                &format!(
                    "retry-after: {}\r\n",
                    httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(3))
                ),
                "managed_service_unavailable",
            )),
            _ => None,
        }),
    )
    .await?;
    let (attachment, _events) = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(workspace.path()))
        .build()?
        .attach(endpoint.target.clone())
        .start()?;
    let wire = endpoint.next_wire(Duration::from_secs(10)).await?;
    let attempts = endpoint.attempts();
    let gaps = gaps(&attempts);
    let summary = json!({
        "statuses": attempts.iter().map(|a| a.status).collect::<Vec<_>>(),
        "attempt_gaps_ms": gaps.iter().copied().map(millis).collect::<Vec<_>>(),
        "retry_after": ["1", "2", "HTTP-date now+3s (whole-second resolution)"],
    });
    evidence.record("endpoint", "summary", &summary);
    eprintln!("Retry-After metrics: {summary}");
    ensure!(
        attempts.iter().map(|a| a.status).collect::<Vec<_>>() == [429, 503, 503, 101],
        "unexpected attempts: {summary}"
    );
    ensure!(
        gaps[0] >= Duration::from_millis(990) && gaps[0] <= Duration::from_millis(1_500),
        "429 Retry-After was not honored: {summary}"
    );
    ensure!(
        gaps[1] >= Duration::from_millis(1_990) && gaps[1] <= Duration::from_millis(2_500),
        "503 Retry-After was not honored: {summary}"
    );
    // The date truncates now+3s to whole seconds, so the wait lies in (2s, 3s].
    ensure!(
        gaps[2] >= Duration::from_millis(1_990) && gaps[2] <= Duration::from_millis(3_500),
        "503 HTTP-date Retry-After was not honored: {summary}"
    );
    // Detach only after the executor observed ready, so teardown drains.
    tokio::time::timeout(Duration::from_secs(5), async {
        while attachment.status() != AttachmentStatus::Ready {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    let mut wire = wire;
    let (drain, detached) = tokio::join!(wire.drain(), attachment.detach());
    drain?;
    detached?;

    let endpoint = scripted_endpoint(
        "/tools",
        evidence.clone(),
        Arc::new(|_, _| {
            Some(http_refusal(
                "503 Service Unavailable",
                &format!("retry-after: {}\r\n", u64::MAX),
                "managed_service_unavailable",
            ))
        }),
    )
    .await?;
    let (attachment, _events) = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(workspace.path()))
        .build()?
        .attach(endpoint.target.clone())
        .start()?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while endpoint.attempts().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let parked = endpoint.attempts().len();
    let detach_started = Instant::now();
    let detached = tokio::time::timeout(Duration::from_secs(1), attachment.detach()).await;
    let detach_ms = millis(detach_started.elapsed());
    let summary =
        json!({"retry_after":"u64::MAX", "attempts_after_700ms": parked, "detach_ms": detach_ms});
    evidence.record("endpoint", "summary", &summary);
    eprintln!("huge Retry-After metrics: {summary}");
    ensure!(parked == 1, "huge Retry-After was not honored: {summary}");
    detached.map_err(|_| eyre::eyre!("detach did not interrupt Retry-After: {summary}"))??;
    Ok(())
}

// Credential and revocation refusals are terminal: one attempt. Other refusals,
// including 404 and 409 (a superseded runtime, or a conflict that may clear),
// keep the long exponential schedule rather than the fast 5xx cap: no hot loop
// and no restart that would mint a new runtime and contend for authority.
#[tokio::test]
async fn terminal_refusals_stop_while_permanent_refusals_keep_long_backoff() -> Result<()> {
    let evidence = reattach_evidence("terminal-and-permanent")?;
    let workspace = tempfile::tempdir()?;
    let tools = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(workspace.path()))
        .build()?;
    for (status, error, terminal) in [
        ("401 Unauthorized", "unauthorized", "authentication"),
        ("403 Forbidden", "forbidden", "authentication"),
        ("410 Gone", "attachment_revoked", "revoked"),
    ] {
        let refusal = http_refusal(status, "", error);
        let endpoint = scripted_endpoint(
            "/tools",
            evidence.clone(),
            Arc::new(move |_, _| Some(refusal.clone())),
        )
        .await?;
        let connected = tokio::time::timeout(
            Duration::from_secs(5),
            tools.clone().attach(endpoint.target.clone()).connect(),
        )
        .await
        .map_err(|_| eyre::eyre!("HTTP {status} kept reconnecting instead of stopping"))?;
        let error = connected.err();
        let matched = match (&error, terminal) {
            (Some(AttachmentError::Authentication(_)), "authentication") => true,
            (Some(error @ AttachmentError::Fenced(_)), "revoked") => error.is_revoked(),
            _ => false,
        };
        tokio::time::sleep(Duration::from_millis(700)).await;
        let attempts = endpoint.attempts().len();
        evidence.record("endpoint", "summary", &json!({"status":status, "terminal":terminal, "error":error.as_ref().map(ToString::to_string), "attempts_after_700ms":attempts}));
        ensure!(matched, "HTTP {status} was not {terminal}: {error:?}");
        ensure!(attempts == 1, "HTTP {status} reconnected {attempts} times");
    }

    let mut refused = Vec::new();
    for (status, error) in [
        ("404 Not Found", "not_found"),
        ("409 Conflict", "hand_runtime_superseded"),
    ] {
        let refusal = http_refusal(status, "", error);
        let endpoint = scripted_endpoint(
            "/tools",
            evidence.clone(),
            Arc::new(move |_, _| Some(refusal.clone())),
        )
        .await?;
        let (attachment, _events) = tools.clone().attach(endpoint.target.clone()).start()?;
        refused.push((status, endpoint, attachment));
    }
    tokio::time::sleep(Duration::from_millis(6_600)).await;
    for (status, endpoint, attachment) in refused {
        let attempts = endpoint.attempts();
        let gaps = gaps(&attempts);
        let summary = json!({
            "status": status,
            "attempts_in_6600ms": attempts.len(),
            "attempt_gaps_ms": gaps.iter().copied().map(millis).collect::<Vec<_>>(),
            "runtime_ids": attempts.iter().map(|a| a.runtime_id.clone()).collect::<HashSet<_>>().len(),
        });
        evidence.record("endpoint", "summary", &summary);
        eprintln!("long-backoff refusal metrics: {summary}");
        ensure!(
            attachment.status() != AttachmentStatus::Fenced,
            "HTTP {status} fenced the attachment"
        );
        ensure!(
            gaps.iter().all(|gap| *gap >= Duration::from_millis(90)),
            "HTTP {status} hot loop: {summary}"
        );
        ensure!(
            attempts.len() <= 7
                && gaps
                    .iter()
                    .max()
                    .is_some_and(|gap| *gap >= Duration::from_secs(3)),
            "HTTP {status} lost its long backoff: {summary}"
        );
        attachment.detach().await?;
    }
    Ok(())
}
