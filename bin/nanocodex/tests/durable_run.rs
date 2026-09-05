use std::{
    path::Path,
    process::{Output, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use eyre::{Result, WrapErr as _, eyre};
use futures_util::{SinkExt as _, StreamExt as _};
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
    assert_completed_model_effect(
        &retained_payload(&database)?.ok_or_else(|| {
            eyre!("recovered run did not retain its completed durable model receipt")
        })?,
        2,
        "recovered durable answer",
    )?;

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
    assert_completed_model_effect(
        &retained_payload(&database)?.ok_or_else(|| {
            eyre!("replacement owner did not retain its completed durable model receipt")
        })?,
        2,
        "replacement durable answer",
    )?;

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
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
    command
        .current_dir(workspace)
        .env_clear()
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
        .arg("false")
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

fn assert_completed_model_effect(
    payload: &Value,
    expected_attempts: u64,
    expected_answer: &str,
) -> Result<()> {
    let step = &payload["nanocodex_durable_state"]["operations"][REQUEST_ID]["steps"]["model-1"];
    if step["kind"] != "model_call"
        || step["status"].get("completed").is_none()
        || step["attempts"] != expected_attempts
        || !step["input"].is_string()
        || !step["status"].to_string().contains(expected_answer)
    {
        return Err(eyre!("unexpected completed durable model receipt: {step}"));
    }
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
        .map(|payload| {
            use base64::Engine as _;
            if let Some(encoded) = payload.strip_prefix("nanocodex-durable-state-gzip-v1:") {
                let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
                serde_json::from_reader(flate2::read::GzDecoder::new(bytes.as_slice()))
                    .map_err(Into::into)
            } else {
                serde_json::from_str(&payload).map_err(Into::into)
            }
        })
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
