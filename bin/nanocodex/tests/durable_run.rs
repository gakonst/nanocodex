use std::{
    path::Path,
    process::{Output, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use eyre::{Result, WrapErr as _, eyre};
use futures_util::{SinkExt as _, StreamExt as _};
use nanocodex_durability::{DurableSession, OperationStatus, SqliteStore};
use rusqlite::{Connection, OptionalExtension as _};
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    process::{Child, Command},
    sync::oneshot,
    time::{sleep, timeout},
};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

const STATE_ID: &str = "root";
const REQUEST_ID: &str = "turn";
const PROMPT: &str = "return the durable answer";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
const SERVER_TIMEOUT: Duration = Duration::from_secs(10);
const SQLITE_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn completed_terminal_replays_in_a_second_process_without_a_provider_connection() -> Result<()>
{
    let workspace = tempfile::tempdir()?;
    let database = workspace.path().join("durability.sqlite3");
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await?);
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(serve_completed_generation(
        Arc::clone(&listener),
        PROMPT,
        "resp-completed",
        "durable answer",
    ));

    let first = run_command(durable_command(
        &endpoint,
        workspace.path(),
        &database,
        PROMPT,
    ))
    .await?;
    assert_success(&first, "initial durable run")?;
    join_server(server, "completed-generation server").await?;
    let retained = retained_payload(&database)?
        .ok_or_else(|| eyre!("completed run did not retain durable state"))?;
    assert!(
        retained["nanocodex_durable_state"]["operations"][REQUEST_ID]["status"]
            .get("completed")
            .is_some(),
        "operation was not durably completed: {retained}"
    );

    let replay = run_without_provider_connection(
        durable_command(&endpoint, workspace.path(), &database, PROMPT),
        Arc::clone(&listener),
    )
    .await?;
    assert_success(&replay, "replayed durable run")?;
    let events = jsonl_events(&replay.stdout)?;
    assert_eq!(
        events.len(),
        1,
        "replay should emit only its terminal event"
    );
    let terminal = &events[0];
    assert!(
        terminal["request_id"]
            .as_str()
            .is_some_and(|request_id| !request_id.is_empty()),
        "terminal event omitted its runtime routing ID: {terminal}"
    );
    assert_eq!(terminal["type"], "run.completed");
    assert_eq!(terminal["payload"]["status"], "completed");
    assert_eq!(terminal["payload"]["model_calls"], 0);
    assert_eq!(terminal["payload"]["connection_attempts"], 0);
    Ok(())
}

#[tokio::test]
async fn reopened_follow_on_turn_sends_full_committed_history_on_a_fresh_socket() -> Result<()> {
    const NEXT_REQUEST_ID: &str = "turn-2";
    const NEXT_PROMPT: &str = "continue from the durable answer";

    let workspace = tempfile::tempdir()?;
    let database = workspace.path().join("durability.sqlite3");
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await?);
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let first_server = tokio::spawn(serve_completed_generation(
        Arc::clone(&listener),
        PROMPT,
        "resp-first-turn",
        "first durable answer",
    ));
    let first = run_command(durable_command(
        &endpoint,
        workspace.path(),
        &database,
        PROMPT,
    ))
    .await?;
    assert_success(&first, "first durable turn")?;
    join_server(first_server, "first-turn server").await?;

    let second_listener = Arc::clone(&listener);
    let second_server = tokio::spawn(async move {
        let (stream, _) = second_listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let request = next_json(&mut socket).await?;
        assert!(
            request.get("previous_response_id").is_none(),
            "fresh socket incorrectly depended on a provider response chain: {request}"
        );
        let encoded = request["input"].to_string();
        assert!(
            encoded.contains(PROMPT)
                && encoded.contains("first durable answer")
                && encoded.contains(NEXT_PROMPT),
            "fresh-socket continuation omitted committed history: {request}"
        );
        send_completed(&mut socket, "resp-second-turn", "second durable answer").await
    });
    let second = run_command(durable_command_for_request(
        &endpoint,
        workspace.path(),
        &database,
        NEXT_REQUEST_ID,
        NEXT_PROMPT,
    ))
    .await?;
    assert_success(&second, "reopened follow-on turn")?;
    join_server(second_server, "second-turn server").await?;

    let retained = retained_payload(&database)?
        .ok_or_else(|| eyre!("follow-on run did not retain durable state"))?;
    for request_id in [REQUEST_ID, NEXT_REQUEST_ID] {
        assert!(
            retained["nanocodex_durable_state"]["operations"][request_id]["status"]
                .get("completed")
                .is_some(),
            "operation {request_id} was not durably terminal: {retained}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn sigkill_redispatches_only_the_uncommitted_model_effect() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let database = workspace.path().join("durability.sqlite3");
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await?);
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (observed_tx, observed_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(serve_gated_generation(
        Arc::clone(&listener),
        observed_tx,
        release_rx,
    ));
    let child = spawn_command(durable_command(
        &endpoint,
        workspace.path(),
        &database,
        PROMPT,
    ))?;

    let generation = timeout(SERVER_TIMEOUT, observed_rx)
        .await
        .map_err(|_| eyre!("provider did not observe the model generation"))??;
    assert_real_generation(&generation, PROMPT)?;
    wait_for_pending_model_effect(&database).await?;
    send_sigkill(&child).await?;
    let _ = release_tx.send(None);
    let killed = wait_child(child, "SIGKILLed durable run").await?;
    assert!(
        !killed.status.success(),
        "SIGKILLed durable run unexpectedly succeeded"
    );
    assert!(
        killed.status.code().is_none(),
        "SIGKILLed durable run exited normally: {:?}",
        killed.status
    );
    join_server(server, "gated crash server").await?;
    assert_pending_model_effect(
        &retained_payload(&database)?
            .ok_or_else(|| eyre!("SIGKILLed run did not retain its durable model receipt"))?,
    )?;

    let recovery_server = tokio::spawn(serve_completed_generation(
        Arc::clone(&listener),
        PROMPT,
        "resp-recovered",
        "recovered durable answer",
    ));
    let reopened = run_command(durable_command(
        &endpoint,
        workspace.path(),
        &database,
        PROMPT,
    ))
    .await?;
    assert_success(&reopened, "recovered durable run")?;
    join_server(recovery_server, "recovery-generation server").await?;
    assert_completed_operation(&database, "recovered durable answer").await?;

    let replay = run_without_provider_connection(
        durable_command(&endpoint, workspace.path(), &database, PROMPT),
        Arc::clone(&listener),
    )
    .await?;
    assert_success(&replay, "provider-free recovered replay")?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_commits_cancellation_and_exact_reopen_stays_provider_free() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let database = workspace.path().join("durability.sqlite3");
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await?);
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (observed_tx, observed_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(serve_gated_generation(
        Arc::clone(&listener),
        observed_tx,
        release_rx,
    ));
    let child = spawn_command(durable_command(
        &endpoint,
        workspace.path(),
        &database,
        PROMPT,
    ))?;

    let generation = timeout(SERVER_TIMEOUT, observed_rx)
        .await
        .map_err(|_| eyre!("provider did not observe the cancellable model generation"))??;
    assert_real_generation(&generation, PROMPT)?;
    wait_for_pending_model_effect(&database).await?;
    send_signal(&child, "TERM").await?;
    let _ = release_tx.send(None);
    let cancelled = wait_child(child, "SIGTERM-cancelled durable run").await?;
    assert!(
        !cancelled.status.success(),
        "SIGTERM-cancelled durable run unexpectedly succeeded"
    );
    join_server(server, "graceful cancellation server").await?;

    let retained = retained_payload(&database)?
        .ok_or_else(|| eyre!("cancelled run did not retain durable state"))?;
    assert!(
        retained["nanocodex_durable_state"]["operations"][REQUEST_ID]["status"]
            .get("cancelled")
            .is_some(),
        "SIGTERM did not commit a durable cancellation: {retained}"
    );

    let reopened = run_without_provider_connection(
        durable_command(&endpoint, workspace.path(), &database, PROMPT),
        Arc::clone(&listener),
    )
    .await?;
    assert!(
        !reopened.status.success(),
        "a replayed cancellation unexpectedly returned success"
    );
    let events = jsonl_events(&reopened.stdout)?;
    assert_eq!(events.len(), 1, "cancel replay emitted extra events");
    assert_eq!(events[0]["type"], "run.failed");
    Ok(())
}

#[tokio::test]
async fn second_process_fences_the_first_owner_and_redispatches_the_uncommitted_effect()
-> Result<()> {
    let workspace = tempfile::tempdir()?;
    let database = workspace.path().join("durability.sqlite3");
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await?);
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (observed_tx, observed_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(serve_gated_generation(
        Arc::clone(&listener),
        observed_tx,
        release_rx,
    ));
    let first = spawn_command(durable_command(
        &endpoint,
        workspace.path(),
        &database,
        PROMPT,
    ))?;

    let generation = timeout(SERVER_TIMEOUT, observed_rx)
        .await
        .map_err(|_| eyre!("first owner did not dispatch its model generation"))??;
    assert_real_generation(&generation, PROMPT)?;
    wait_for_pending_model_effect(&database).await?;

    let replacement_server = tokio::spawn(serve_completed_generation(
        Arc::clone(&listener),
        PROMPT,
        "resp-replacement",
        "replacement durable answer",
    ));
    let replacement = run_command(durable_command(
        &endpoint,
        workspace.path(),
        &database,
        PROMPT,
    ))
    .await?;
    assert_success(&replacement, "replacement durable owner")?;
    join_server(replacement_server, "replacement-generation server").await?;
    assert!(
        retained_fence(&database)? >= 2,
        "replacement process did not advance the SQLite owner fence"
    );

    release_tx
        .send(Some(("resp-after-fence", "late answer")))
        .map_err(|_| eyre!("first provider connection closed before release"))?;
    let first = wait_child(first, "fenced first owner").await?;
    assert!(
        !first.status.success(),
        "fenced first owner unexpectedly committed its provider result: {}",
        String::from_utf8_lossy(&first.stdout)
    );
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(
        stderr.contains("fenced"),
        "first owner did not report its durability fence:\n{stderr}"
    );
    join_server(server, "fencing server").await?;
    assert_completed_operation(&database, "replacement durable answer").await?;

    let replay = run_without_provider_connection(
        durable_command(&endpoint, workspace.path(), &database, PROMPT),
        Arc::clone(&listener),
    )
    .await?;
    assert_success(&replay, "provider-free replacement replay")?;
    Ok(())
}

fn durable_command(endpoint: &str, workspace: &Path, database: &Path, prompt: &str) -> Command {
    durable_command_for_request(endpoint, workspace, database, REQUEST_ID, prompt)
}

fn durable_command_for_request(
    endpoint: &str,
    workspace: &Path,
    database: &Path,
    request_id: &str,
    prompt: &str,
) -> Command {
    durable_command_with(endpoint, workspace, database, request_id, prompt, false)
}

fn durable_command_with(
    endpoint: &str,
    workspace: &Path,
    database: &Path,
    request_id: &str,
    prompt: &str,
    subagents: bool,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
    command
        .current_dir(workspace)
        .env_clear()
        // Durable replay tests never install or launch a desktop provider.
        .env("NANOCODEX_COMPUTER", "off")
        .env("CODEX_HOME", workspace.join("codex-home"))
        .arg("run")
        .arg("--api-key")
        .arg("test-key")
        .arg("--websocket-url")
        .arg(endpoint)
        .arg("--websocket-warmup")
        .arg("false")
        .arg("--responses-transport")
        .arg("websocket")
        .arg("--store-responses")
        .arg("false")
        .arg("--cwd")
        .arg(workspace)
        .arg("--local-durability")
        .arg(database)
        .arg("--local-durability-state-id")
        .arg(STATE_ID)
        .arg("--request-id")
        .arg(request_id)
        .arg("--rollouts")
        .arg("false")
        .arg("--browser=none")
        .arg("--mcp-defaults")
        .arg("false")
        .arg("--mcp-codex-config")
        .arg("false")
        .arg("--web-search")
        .arg("false")
        .arg("--image-generation")
        .arg("false")
        .arg("--subagents")
        .arg(subagents.to_string())
        .arg("--memory")
        .arg("false")
        .arg(prompt)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn run_command(mut command: Command) -> Result<Output> {
    timeout(PROCESS_TIMEOUT, command.output())
        .await
        .map_err(|_| eyre!("nanocodex process exceeded {PROCESS_TIMEOUT:?}"))?
        .map_err(Into::into)
}

fn spawn_command(mut command: Command) -> Result<Child> {
    command.spawn().wrap_err("failed to spawn nanocodex")
}

async fn wait_child(child: Child, description: &str) -> Result<Output> {
    timeout(PROCESS_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| eyre!("{description} exceeded {PROCESS_TIMEOUT:?}"))?
        .wrap_err_with(|| format!("failed to wait for {description}"))
}

async fn run_without_provider_connection(
    mut command: Command,
    listener: Arc<TcpListener>,
) -> Result<Output> {
    let connection = listener.accept();
    let output = command.output();
    tokio::pin!(connection);
    tokio::pin!(output);
    timeout(PROCESS_TIMEOUT, async {
        tokio::select! {
            biased;
            accepted = &mut connection => {
                let (_, peer) = accepted?;
                Err(eyre!("durable replay unexpectedly connected to provider from {peer}"))
            }
            result = &mut output => Ok(result?),
        }
    })
    .await
    .map_err(|_| eyre!("provider-free durable reopen exceeded {PROCESS_TIMEOUT:?}"))?
}

async fn serve_completed_generation(
    listener: Arc<TcpListener>,
    expected_prompt: &'static str,
    response_id: &'static str,
    answer: &'static str,
) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let mut socket = accept_async(stream).await?;
    let request = next_json(&mut socket).await?;
    assert_real_generation(&request, expected_prompt)?;
    send_completed(&mut socket, response_id, answer).await
}

async fn serve_gated_generation(
    listener: Arc<TcpListener>,
    observed: oneshot::Sender<Value>,
    release: oneshot::Receiver<Option<(&'static str, &'static str)>>,
) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let mut socket = accept_async(stream).await?;
    let request = next_json(&mut socket).await?;
    observed
        .send(request)
        .map_err(|_| eyre!("generation observer dropped"))?;
    if let Ok(Some((response_id, answer))) = release.await {
        send_completed(&mut socket, response_id, answer).await?;
    }
    Ok(())
}

async fn next_json<S>(socket: &mut WebSocketStream<S>) -> Result<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(|| eyre!("client closed before sending a Responses request"))??;
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).map_err(Into::into);
        }
    }
}

async fn send_completed<S>(
    socket: &mut WebSocketStream<S>,
    response_id: &str,
    answer: &str,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "status": "completed",
                    "output": [{
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": answer }]
                    }],
                    "usage": {
                        "input_tokens": 1,
                        "input_tokens_details": { "cached_tokens": 0 },
                        "output_tokens": 1,
                        "output_tokens_details": { "reasoning_tokens": 0 },
                        "total_tokens": 2
                    }
                }
            })
            .to_string()
            .into(),
        ))
        .await?;
    Ok(())
}

fn assert_real_generation(request: &Value, prompt: &str) -> Result<()> {
    assert_ne!(
        request["generate"], false,
        "observed request was only WebSocket warmup: {request}"
    );
    assert!(
        request["input"].to_string().contains(prompt),
        "model generation omitted the prompt: {request}"
    );
    Ok(())
}

async fn wait_for_pending_model_effect(database: &Path) -> Result<Value> {
    let deadline = Instant::now() + SQLITE_TIMEOUT;
    let mut last_payload = None;
    loop {
        if let Ok(Some(payload)) = retained_payload(database) {
            if assert_pending_model_effect(&payload).is_ok() {
                return Ok(payload);
            }
            last_payload = Some(payload);
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "model effect did not become durably pending within {SQLITE_TIMEOUT:?}; last payload: {}",
                last_payload
                    .as_ref()
                    .map_or_else(|| "<none>".to_owned(), Value::to_string)
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

fn assert_pending_model_effect(payload: &Value) -> Result<()> {
    let step = &payload["nanocodex_durable_state"]["operations"][REQUEST_ID]["steps"]["model-1"];
    if step["kind"] != "model_call"
        || step["status"] != "effect_pending"
        || step["attempts"] != 1
        || !step["input"].is_string()
    {
        return Err(eyre!("unexpected durable model receipt: {step}"));
    }
    Ok(())
}

async fn assert_completed_operation(database: &Path, expected_answer: &str) -> Result<()> {
    let session = DurableSession::open(SqliteStore::open(database)?, STATE_ID).await?;
    let state = session.state().await?;
    let operation = state
        .operation(REQUEST_ID)
        .ok_or_else(|| eyre!("missing operation"))?;
    let OperationStatus::Completed { output, .. } = &operation.status else {
        return Err(eyre!("operation was not completed"));
    };
    let output: Value = session.resolve(output).await?.decode()?;
    assert_eq!(output["final_message"], expected_answer);
    assert!(
        operation.steps.is_empty(),
        "settled effects must be retired"
    );
    Ok(())
}

fn retained_payload(database: &Path) -> Result<Option<Value>> {
    let connection = Connection::open(database)?;
    connection.busy_timeout(Duration::from_millis(100))?;
    let payload = connection
        .query_row(
            "SELECT payload FROM nanocodex_durable_states WHERE state_id = ?1",
            [STATE_ID],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    payload
        .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
        .transpose()
}

fn retained_fence(database: &Path) -> Result<u64> {
    let connection = Connection::open(database)?;
    let fence = connection.query_row(
        "SELECT fence FROM nanocodex_durable_owners WHERE state_id = ?1",
        [STATE_ID],
        |row| row.get::<_, String>(0),
    )?;
    fence
        .parse()
        .wrap_err_with(|| format!("invalid retained owner fence `{fence}`"))
}

fn jsonl_events(stdout: &[u8]) -> Result<Vec<Value>> {
    String::from_utf8(stdout.to_vec())?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn assert_success(output: &Output, description: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    Err(eyre!(
        "{description} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

async fn join_server(server: tokio::task::JoinHandle<Result<()>>, description: &str) -> Result<()> {
    timeout(SERVER_TIMEOUT, server)
        .await
        .map_err(|_| eyre!("{description} exceeded {SERVER_TIMEOUT:?}"))?
        .wrap_err_with(|| format!("{description} task failed"))?
        .wrap_err_with(|| description.to_owned())
}

#[cfg(unix)]
async fn send_sigkill(child: &Child) -> Result<()> {
    send_signal(child, "KILL").await
}

#[cfg(unix)]
async fn send_signal(child: &Child, signal: &str) -> Result<()> {
    let pid = child
        .id()
        .ok_or_else(|| eyre!("durable run had no process ID"))?;
    let status = Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status()
        .await?;
    if !status.success() {
        return Err(eyre!(
            "failed to send SIG{signal} to nanocodex process {pid}"
        ));
    }
    Ok(())
}

const SUBAGENT_PROMPT: &str = "SUBAGENT_DURABILITY_ROOT delegate both phases to one child";
const CHILD_TASK: &str = "SUBAGENT_DURABILITY_CHILD complete phase one";
const PHASE_TWO: &str = "SUBAGENT_DURABILITY_PHASE_TWO complete phase two";
const RESUMED: &str = "runtime restarted while your previous turn was running";

/// Scripted Responses provider shared by both CLI processes. Every generation
/// is recorded with its process generation so finished steps can be checked.
struct SubagentProvider {
    generation: usize,
    log: Vec<Value>,
    hung: Option<oneshot::Sender<()>>,
    child_hung: bool,
    root_waiting: bool,
}

enum SubagentReply {
    Code(&'static str, String),
    Text(&'static str),
    Hang,
}

impl SubagentProvider {
    fn reply(&mut self, label: &str, request: &Value) -> SubagentReply {
        let last = last_input_item(request);
        let last_call = last["call_id"].as_str().unwrap_or_default().to_owned();
        let last_text = item_text(&last);
        let reply = match (label, last_call.as_str()) {
            ("root", "") => SubagentReply::Code(
                "root-spawn",
                format!(
                    "text(JSON.stringify(await tools.spawn_agent({{role:'durable-child',task:{task:?},\
                     harness:null,model:null,thinking:null,output_contract:{{kind:'object',fields:\
                     [{{name:'answer',required:true,schema:{{kind:'string'}}}}]}}}})));",
                    task = CHILD_TASK
                ),
            ),
            ("root", "root-spawn") => SubagentReply::Code("root-wait-1", wait_code()),
            ("root", "root-wait-1") => SubagentReply::Code(
                "root-message",
                format!(
                    "text(JSON.stringify(await tools.send_agent_message({{agent_id:1,message:{PHASE_TWO:?}}})));"
                ),
            ),
            ("root", call) if call.starts_with("root-") && !last_text.contains("phase-two") => {
                let next = call
                    .rsplit('-')
                    .next()
                    .and_then(|index| index.parse::<usize>().ok())
                    .map_or(2, |index| index + 1);
                if next > 8 {
                    return self.record(
                        label,
                        &last_call,
                        last_text,
                        request,
                        SubagentReply::Text("SUBAGENT_DURABILITY_GAVE_UP"),
                    );
                }
                let id: &'static str = Box::leak(format!("root-wait-{next}").into_boxed_str());
                SubagentReply::Code(id, wait_code())
            }
            ("root", _) => SubagentReply::Text("SUBAGENT_DURABILITY_ROOT_DONE"),
            ("child", "child-submit-1") => SubagentReply::Text("child finished phase one"),
            ("child", "child-submit-2") => SubagentReply::Text("child finished phase two"),
            ("child", _) if last_text.contains(RESUMED) => SubagentReply::Code(
                "child-submit-2",
                "text(JSON.stringify(await tools.submit_result({output:{answer:'phase-two'}})));"
                    .into(),
            ),
            ("child", _) if last_text.contains(PHASE_TWO) => SubagentReply::Hang,
            ("child", _) => SubagentReply::Code(
                "child-submit-1",
                "text(JSON.stringify(await tools.submit_result({output:{answer:'phase-one'}})));"
                    .into(),
            ),
            _ => SubagentReply::Text("unexpected fixture request"),
        };
        self.record(label, &last_call, last_text, request, reply)
    }

    fn record(
        &mut self,
        label: &str,
        last_call: &str,
        last_text: String,
        request: &Value,
        reply: SubagentReply,
    ) -> SubagentReply {
        if label == "root" && matches!(&reply, SubagentReply::Code("root-wait-2", _)) {
            self.root_waiting = true;
        }
        self.log.push(json!({
            "generation": self.generation,
            "label": label,
            "last_call_id": last_call,
            "last_text": last_text,
            "history": request["input"].to_string(),
            "reply": match &reply {
                SubagentReply::Code(id, _) => json!({"code": id}),
                SubagentReply::Text(text) => json!({"text": text}),
                SubagentReply::Hang => json!("hang"),
            },
        }));
        reply
    }
}

fn wait_code() -> String {
    "text(JSON.stringify(await tools.wait_agent({agent_ids:[1],timeout_ms:60000})));".to_owned()
}

fn last_input_item(request: &Value) -> Value {
    request["input"]
        .as_array()
        .and_then(|items| items.last())
        .cloned()
        .unwrap_or(Value::Null)
}

fn item_text(item: &Value) -> String {
    match (&item["content"], &item["output"]) {
        (Value::Array(blocks), _) | (_, Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        (_, Value::String(text)) => text.clone(),
        (Value::String(text), _) => text.clone(),
        _ => String::new(),
    }
}

fn user_text(request: &Value) -> String {
    request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["role"] == "user")
        .map(item_text)
        .collect::<Vec<_>>()
        .join("\n")
}

async fn serve_subagent_provider(
    listener: Arc<TcpListener>,
    provider: Arc<std::sync::Mutex<SubagentProvider>>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let provider = Arc::clone(&provider);
        tokio::spawn(async move {
            let Ok(mut socket) = accept_async(stream).await else {
                return;
            };
            let mut label = None;
            while let Some(Ok(message)) = socket.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let Ok(request) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                if request["generate"] == false {
                    continue;
                }
                let users = user_text(&request);
                let current = label.get_or_insert_with(|| {
                    if users.contains("SUBAGENT_DURABILITY_ROOT") {
                        "root"
                    } else {
                        "child"
                    }
                });
                let reply = provider.lock().unwrap().reply(current, &request);
                if matches!(reply, SubagentReply::Hang) {
                    provider.lock().unwrap().child_hung = true;
                }
                {
                    // Signal once the child is mid-turn and the root has been
                    // told to wait for it, so the crash interrupts both.
                    let mut provider = provider.lock().unwrap();
                    if provider.child_hung
                        && provider.root_waiting
                        && let Some(hung) = provider.hung.take()
                    {
                        let _ = hung.send(());
                    }
                }
                let output = match reply {
                    SubagentReply::Hang => continue,
                    SubagentReply::Code(id, code) => {
                        json!({"type":"custom_tool_call","name":"exec","call_id":id,"input":code})
                    }
                    SubagentReply::Text(text) => json!({"type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":text}]}),
                };
                let response = json!({"type":"response.completed","response":{
                    "id": format!("resp-{}", uuid::Uuid::new_v4()),
                    "status":"completed","output":[output],
                    "usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},
                        "output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},
                        "total_tokens":2}}});
                if socket
                    .send(Message::Text(response.to_string().into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
    }
}

fn subagent_command(endpoint: &str, workspace: &Path, database: &Path) -> Command {
    durable_command_with(
        endpoint,
        workspace,
        database,
        REQUEST_ID,
        SUBAGENT_PROMPT,
        true,
    )
}

fn subagent_journals(database: &Path) -> Result<Vec<Value>> {
    let connection = Connection::open(database)?;
    connection.busy_timeout(Duration::from_millis(100))?;
    let mut statement = connection.prepare(
        "SELECT payload FROM nanocodex_durable_states WHERE state_id = ?1 AND payload IS NOT NULL",
    )?;
    // The tree is keyed by the durable root state, not its runtime session.
    let rows = statement
        .query_map([format!("{STATE_ID}:subagents")], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
        .collect()
}

fn journaled_child(database: &Path) -> Option<Value> {
    subagent_journals(database)
        .ok()?
        .into_iter()
        .find_map(|journal| journal["agents"].as_array()?.first().cloned())
}

#[cfg(unix)]
#[tokio::test]
async fn sigkilled_durable_root_resumes_its_mid_turn_child_without_resending_finished_steps()
-> Result<()> {
    let evidence = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/durable-subagents")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&evidence)?;
    let workspace = tempfile::tempdir()?;
    let database = workspace.path().join("durability.sqlite3");
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await?);
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let (hung, hung_rx) = oneshot::channel();
    let provider = Arc::new(std::sync::Mutex::new(SubagentProvider {
        generation: 1,
        log: Vec::new(),
        hung: Some(hung),
        child_hung: false,
        root_waiting: false,
    }));
    let server = tokio::spawn(serve_subagent_provider(
        Arc::clone(&listener),
        Arc::clone(&provider),
    ));

    // First process: the root spawns a child, the child completes phase one,
    // and its phase-two turn hangs at the provider while the root waits.
    let first = spawn_command(subagent_command(&endpoint, workspace.path(), &database))?;
    timeout(Duration::from_secs(60), hung_rx)
        .await
        .map_err(|_| {
            eyre!(
                "child never reached its phase-two generation: {:#?}",
                provider.lock().unwrap().log
            )
        })??;
    let deadline = Instant::now() + SQLITE_TIMEOUT;
    let journaled = loop {
        if let Some(child) = journaled_child(&database)
            && child["turn_in_flight"] == true
            && child["turn_input"]
                .as_str()
                .is_some_and(|input| input.contains(PHASE_TWO))
            && child["checkpoint"]["conversation"].is_object()
        {
            break child;
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "mid-turn child was not journaled: {:?}",
                journaled_child(&database)
            ));
        }
        sleep(Duration::from_millis(20)).await;
    };
    // The root's fourth model call (which asked it to wait again) must hold a
    // committed receipt, so recovery can prove it is replayed, not resent.
    let deadline = Instant::now() + SQLITE_TIMEOUT;
    while retained_payload(&database)?.is_none_or(|root| {
        root["nanocodex_durable_state"]["operations"][REQUEST_ID]["steps"]["model-4"]["status"]
            ["completed"]
            .is_null()
    }) {
        if Instant::now() >= deadline {
            return Err(eyre!("root wait step was not durably committed"));
        }
        sleep(Duration::from_millis(20)).await;
    }
    if let Some(root) = retained_payload(&database)? {
        std::fs::write(
            evidence.join("root-at-kill.json"),
            serde_json::to_vec_pretty(&root)?,
        )?;
    }
    send_sigkill(&first).await?;
    let killed = wait_child(first, "SIGKILLed durable root").await?;
    assert!(
        killed.status.code().is_none(),
        "durable root was not killed: {:?}",
        killed.status
    );
    std::fs::write(
        evidence.join("journal-at-kill.json"),
        serde_json::to_vec_pretty(&journaled)?,
    )?;
    provider.lock().unwrap().generation = 2;

    // Second process on the same database: the root's operation resumes, the
    // registry restores the tree and resumes the interrupted child itself.
    let mut restart = subagent_command(&endpoint, workspace.path(), &database);
    restart.env("RUST_LOG", "nanocodex_subagents=debug,warn");
    let second = timeout(Duration::from_secs(60), restart.output()).await;
    let log = provider.lock().unwrap().log.clone();
    std::fs::write(
        evidence.join("provider.json"),
        serde_json::to_vec_pretty(&log)?,
    )?;
    let second = second.map_err(|_| {
        eyre!(
            "restarted durable root did not finish; evidence in {}",
            evidence.display()
        )
    })??;
    std::fs::write(
        evidence.join("provider.json"),
        serde_json::to_vec_pretty(&log)?,
    )?;
    std::fs::write(evidence.join("restart.stdout.jsonl"), &second.stdout)?;
    std::fs::write(evidence.join("restart.stderr.log"), &second.stderr)?;
    server.abort();
    assert_success(&second, "restarted durable root")?;
    let events = jsonl_events(&second.stdout)?;
    assert!(
        events.iter().any(|event| event["type"] == "run.completed")
            && String::from_utf8_lossy(&second.stdout).contains("SUBAGENT_DURABILITY_ROOT_DONE"),
        "restarted root did not complete with the resumed child's result: {events:#?}"
    );

    let restarted = log
        .iter()
        .filter(|entry| entry["generation"] == 2)
        .collect::<Vec<_>>();
    // Finished root model steps replay from durable receipts, never the provider.
    for finished in ["", "root-spawn", "root-wait-1", "root-message"] {
        assert!(
            !restarted
                .iter()
                .any(|entry| entry["label"] == "root" && entry["last_call_id"] == finished),
            "restart resent the finished root step after `{finished}`: {restarted:#?}"
        );
    }
    let child = restarted
        .iter()
        .filter(|entry| entry["label"] == "child")
        .collect::<Vec<_>>();
    // The child's committed phase-one turn is not replayed; only the resumed
    // phase-two turn runs, from its checkpoint and with its original input.
    assert_eq!(
        child
            .iter()
            .map(|entry| entry["reply"].clone())
            .collect::<Vec<_>>(),
        vec![
            json!({"code": "child-submit-2"}),
            json!({"text": "child finished phase two"})
        ],
        "unexpected child generations after restart: {child:#?}"
    );
    let resumed = child[0];
    assert!(
        resumed["last_text"]
            .as_str()
            .is_some_and(|text| text.contains(RESUMED) && text.contains(PHASE_TWO)),
        "resumed child turn omitted its interrupted input: {resumed}"
    );
    assert!(
        resumed["history"]
            .as_str()
            .is_some_and(|history| history.contains("child finished phase one")),
        "resumed child did not start from its committed checkpoint: {resumed}"
    );

    let final_child =
        journaled_child(&database).ok_or_else(|| eyre!("restarted root lost the child journal"))?;
    std::fs::write(
        evidence.join("journal-final.json"),
        serde_json::to_vec_pretty(&final_child)?,
    )?;
    assert_eq!(final_child["turn_in_flight"], false, "{final_child}");
    Ok(())
}
