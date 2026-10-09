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
    attachment::{AttachmentMachine, AttachmentMetadata, AttachmentStatus, AttachmentTarget},
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
            &nanocodex_oai_tools::SessionEnvironment::root("synthetic-session"),
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
