#![cfg(unix)]

use std::{
    env,
    ffi::OsStr,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use eyre::{Result, WrapErr as _, eyre};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::{
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    sync::oneshot,
    time::{sleep, timeout},
};
use tokio_tungstenite::{
    WebSocketStream, accept_async, connect_async,
    tungstenite::{Message, client::IntoClientRequest as _},
};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(15);
const STATE_TIMEOUT: Duration = Duration::from_secs(10);
const FIRST_PROMPT: &str = "return the first managed answer";
const FIRST_KEY: &str = "managed-first-operation";
const DETACH_PROMPT: &str = "wait while the client detaches, then answer";
const DETACH_KEY: &str = "managed-detach-operation";
const STEER_PROMPT: &str = "wait at the provider boundary for steering";
const STEER_KEY: &str = "managed-steer-operation";
const STEER_INPUT: &str = "answer with the steered result";
const CANCEL_PROMPT: &str = "wait at the provider boundary until cancelled";
const CANCEL_KEY: &str = "managed-cancel-operation";
const SERVER_CRASH_PROMPT: &str = "hold this model call across a server crash";
const SERVER_CRASH_KEY: &str = "managed-server-crash-operation";

struct ProviderSignals {
    detach_seen: oneshot::Sender<Value>,
    detach_release: oneshot::Receiver<()>,
    steer_seen: oneshot::Sender<Value>,
    steer_release: oneshot::Receiver<()>,
    cancel_seen: oneshot::Sender<Value>,
    cancel_issued: oneshot::Receiver<()>,
}

struct ClientHarness {
    binary: PathBuf,
    origin: String,
    bearer: String,
    home: PathBuf,
    workspace: PathBuf,
}

impl ClientHarness {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env("NANOCODEX_MANAGED_URL", &self.origin)
            .env("NANOCODEX_API_KEY", &self.bearer)
            .env_remove("NC_API_KEY")
            .env("NANOCODEX_HOME", &self.home)
            .env_remove("OPENAI_API_KEY")
            .current_dir(&self.workspace)
            .kill_on_drop(true);
        command
    }

    async fn output<I, S>(&self, arguments: I, description: &str) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = self.command();
        command.args(arguments);
        timeout(PROCESS_TIMEOUT, command.output())
            .await
            .map_err(|_| eyre!("{description} exceeded {PROCESS_TIMEOUT:?}"))?
            .wrap_err_with(|| format!("failed to run {description}"))
    }

    fn spawn<I, S>(&self, arguments: I) -> Result<Child>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = self.command();
        command
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.spawn().wrap_err("failed to spawn nanocodex2")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual compiled-binary E2E for the loopback managed server and nanocodex2"]
async fn nanocodex2_drives_durable_replay_detach_steer_and_cancel() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    let client_home = fixture.path().join("nanocodex2-home");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&client_home)?;
    write_client_config(&client_home, &workspace)?;

    let provider_listener = TcpListener::bind("127.0.0.1:0").await?;
    let provider_endpoint = format!("ws://{}", provider_listener.local_addr()?);
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let (detach_seen, detach_seen_rx) = oneshot::channel();
    let (detach_release, detach_release_rx) = oneshot::channel();
    let (steer_seen, steer_seen_rx) = oneshot::channel();
    let (steer_release, steer_release_rx) = oneshot::channel();
    let (cancel_seen, cancel_seen_rx) = oneshot::channel();
    let (cancel_issued, cancel_issued_rx) = oneshot::channel();
    let provider = tokio::spawn(serve_provider(
        provider_listener,
        Arc::clone(&provider_calls),
        ProviderSignals {
            detach_seen,
            detach_release: detach_release_rx,
            steer_seen,
            steer_release: steer_release_rx,
            cancel_seen,
            cancel_issued: cancel_issued_rx,
        },
    ));

    let managed_address = unused_loopback_address()?;
    let managed_sqlite = fixture.path().join("managed.sqlite3");
    let bearer = format!("ncx_live_{}_{}", "m".repeat(12), "n".repeat(43));
    let mut managed_server = spawn_managed_server(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &bearer,
    )?;
    wait_for_listener(&mut managed_server, managed_address).await?;

    let client = ClientHarness {
        binary: nanocodex2_binary()?,
        origin: format!("http://{managed_address}"),
        bearer: bearer.clone(),
        home: client_home,
        workspace,
    };

    let created = client.output(["new"], "nanocodex2 new").await?;
    assert_success(&created, "nanocodex2 new")?;
    assert_secret_absent(&created, &bearer)?;
    let receipt: Value = serde_json::from_slice(&created.stdout)?;
    let agent_id = receipt["agent_id"]
        .as_str()
        .ok_or_else(|| eyre!("new receipt omitted agent_id: {receipt}"))?
        .to_owned();

    let first = client
        .output(
            [
                "run",
                FIRST_PROMPT,
                "--agent",
                &agent_id,
                "--idempotency-key",
                FIRST_KEY,
            ],
            "initial managed run",
        )
        .await?;
    assert_success(&first, "initial managed run")?;
    assert_final_answer(&first, "first managed answer")?;
    assert!(
        jsonl_events(&first.stdout)?
            .iter()
            .any(|event| event["type"] == "run.completed"),
        "initial managed run omitted its streamed run.completed event"
    );
    assert_secret_absent(&first, &bearer)?;
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        1,
        "initial managed turn dispatched an unexpected number of provider calls"
    );

    let replay = client
        .output(
            [
                "run",
                FIRST_PROMPT,
                "--agent",
                &agent_id,
                "--idempotency-key",
                FIRST_KEY,
            ],
            "exact idempotent replay",
        )
        .await?;
    assert_success(&replay, "exact idempotent replay")?;
    assert_final_answer(&replay, "first managed answer")?;
    assert!(
        jsonl_events(&replay.stdout)?.is_empty(),
        "retained managed replay unexpectedly synthesized live nested events"
    );
    assert_secret_absent(&replay, &bearer)?;
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        1,
        "exact completed replay made a second provider call"
    );

    let detached = client.spawn([
        "run",
        DETACH_PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        DETACH_KEY,
    ])?;
    let detached_request = timeout(PROVIDER_TIMEOUT, detach_seen_rx)
        .await
        .map_err(|_| eyre!("detach generation was not observed"))??;
    assert_request_contains(&detached_request, DETACH_PROMPT)?;
    let detached_turn = wait_for_active_turn(&client, &agent_id, DETACH_PROMPT).await?;
    send_sigkill(&detached).await?;
    let killed = wait_output(detached, "SIGKILLed nanocodex2 run").await?;
    assert!(
        !killed.status.success() && killed.status.code().is_none(),
        "detached nanocodex2 did not die from SIGKILL: {:?}",
        killed.status
    );

    let reconnect_baseline = managed_observations(&managed_sqlite, &agent_id, &detached_turn)?;
    let reconnected = client.spawn([
        "run",
        DETACH_PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        DETACH_KEY,
    ])?;
    let rejoined_turn = wait_for_active_turn(&client, &agent_id, DETACH_PROMPT).await?;
    assert_eq!(
        rejoined_turn, detached_turn,
        "same-key reconnect did not rejoin the retained turn"
    );
    wait_for_reconnect_barriers(
        &managed_sqlite,
        &agent_id,
        &detached_turn,
        reconnect_baseline,
    )
    .await?;
    detach_release
        .send(())
        .map_err(|()| eyre!("detach provider gate was already closed"))?;
    let reconnected = wait_output(reconnected, "same-key reconnect").await?;
    assert_success(&reconnected, "same-key reconnect")?;
    assert_final_answer(&reconnected, "detached client answer")?;
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        2,
        "detach/reconnect duplicated the in-flight provider call"
    );

    let steered = client.spawn([
        "run",
        STEER_PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        STEER_KEY,
    ])?;
    let steer_request = timeout(PROVIDER_TIMEOUT, steer_seen_rx)
        .await
        .map_err(|_| eyre!("steer boundary generation was not observed"))??;
    assert_request_contains(&steer_request, STEER_PROMPT)?;
    let steer_turn = wait_for_active_turn(&client, &agent_id, STEER_PROMPT).await?;
    let steer = client
        .output(
            ["steer", &agent_id, &steer_turn, STEER_INPUT],
            "nanocodex2 steer",
        )
        .await?;
    assert_success(&steer, "nanocodex2 steer")?;
    let steer_receipt: Value = serde_json::from_slice(&steer.stdout)?;
    assert_eq!(steer_receipt["turn_id"], steer_turn);
    assert_eq!(steer_receipt["state"], "steered");
    steer_release
        .send(())
        .map_err(|()| eyre!("steer provider gate was already closed"))?;
    let steered = wait_output(steered, "steered managed run").await?;
    assert_success(&steered, "steered managed run")?;
    assert_final_answer(&steered, "steered managed answer")?;
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        4,
        "live steer did not join at exactly one subsequent provider boundary"
    );

    let cancelled = client.spawn([
        "run",
        CANCEL_PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        CANCEL_KEY,
    ])?;
    let cancel_request = timeout(PROVIDER_TIMEOUT, cancel_seen_rx)
        .await
        .map_err(|_| eyre!("cancellable generation was not observed"))??;
    assert_request_contains(&cancel_request, CANCEL_PROMPT)?;
    let cancel_turn = wait_for_active_turn(&client, &agent_id, CANCEL_PROMPT).await?;
    let cancel = client
        .output(["cancel", &agent_id, &cancel_turn], "nanocodex2 cancel")
        .await?;
    assert_success(&cancel, "nanocodex2 cancel")?;
    let cancel_receipt: Value = serde_json::from_slice(&cancel.stdout)?;
    assert_eq!(cancel_receipt["turn_id"], cancel_turn);
    assert_eq!(cancel_receipt["state"], "cancelling");
    cancel_issued
        .send(())
        .map_err(|()| eyre!("cancel provider observer was already closed"))?;
    let cancelled = wait_output(cancelled, "cancelled managed run").await?;
    assert!(
        !cancelled.status.success(),
        "cancelled managed run unexpectedly succeeded"
    );
    assert_cancelled_terminal(&cancelled)?;
    wait_for_no_active_turns(&client, &agent_id).await?;
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        5,
        "live cancel caused an unexpected provider dispatch"
    );

    timeout(PROVIDER_TIMEOUT, provider)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))?
        .wrap_err("mock Responses task failed")??;
    managed_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, managed_server.wait())
        .await
        .map_err(|_| eyre!("managed server did not stop"))??;

    let mut restarted_server = spawn_managed_server(
        managed_address,
        &managed_sqlite,
        &client.workspace,
        &provider_endpoint,
        &bearer,
    )?;
    wait_for_listener(&mut restarted_server, managed_address).await?;
    let cold_replay = client
        .output(
            [
                "run",
                FIRST_PROMPT,
                "--agent",
                &agent_id,
                "--idempotency-key",
                FIRST_KEY,
            ],
            "cold-server terminal replay",
        )
        .await?;
    assert_success(&cold_replay, "cold-server terminal replay")?;
    assert_final_answer(&cold_replay, "first managed answer")?;
    assert!(
        jsonl_events(&cold_replay.stdout)?.is_empty(),
        "cold terminal replay unexpectedly synthesized live nested events"
    );
    restarted_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, restarted_server.wait())
        .await
        .map_err(|_| eyre!("restarted managed server did not stop"))??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual compiled-binary server-crash recovery gate"]
async fn cold_server_recovery_redispatches_only_the_uncommitted_provider_effect() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    let client_home = fixture.path().join("nanocodex2-home");
    let managed_sqlite = fixture.path().join("managed.sqlite3");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&client_home)?;
    write_client_config(&client_home, &workspace)?;

    let provider_listener = TcpListener::bind("127.0.0.1:0").await?;
    let provider_endpoint = format!("ws://{}", provider_listener.local_addr()?);
    let managed_address = unused_loopback_address()?;
    let bearer = format!("ncx_live_{}_{}", "c".repeat(12), "d".repeat(43));
    let mut managed_server = spawn_managed_server(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &bearer,
    )?;
    wait_for_listener(&mut managed_server, managed_address).await?;
    let client = ClientHarness {
        binary: nanocodex2_binary()?,
        origin: format!("http://{managed_address}"),
        bearer: bearer.clone(),
        home: client_home,
        workspace: workspace.clone(),
    };
    let created = client.output(["new"], "crash-test new").await?;
    assert_success(&created, "crash-test new")?;
    let receipt: Value = serde_json::from_slice(&created.stdout)?;
    let agent_id = receipt["agent_id"]
        .as_str()
        .ok_or_else(|| eyre!("crash-test receipt omitted agent_id"))?
        .to_owned();

    let mut live = client.spawn([
        "run",
        SERVER_CRASH_PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        SERVER_CRASH_KEY,
    ])?;
    let (provider_stream, _) = timeout(PROVIDER_TIMEOUT, provider_listener.accept())
        .await
        .map_err(|_| eyre!("crash-test provider connection was not opened"))??;
    let mut provider_socket = accept_async(provider_stream).await?;
    let calls = AtomicUsize::new(0);
    let generation = next_generation(&mut provider_socket, &calls).await?;
    assert_request_contains(&generation, SERVER_CRASH_PROMPT)?;
    let turn_id = wait_for_active_turn(&client, &agent_id, SERVER_CRASH_PROMPT).await?;

    managed_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, managed_server.wait())
        .await
        .map_err(|_| eyre!("crashed managed server did not exit"))??;
    drop(provider_socket);
    assert!(
        live.try_wait()?.is_none(),
        "attached client exited before the managed server could restart"
    );
    let reconnect_baseline = managed_observations(&managed_sqlite, &agent_id, &turn_id)?;

    let mut restarted_server = spawn_managed_server(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &bearer,
    )?;
    wait_for_listener(&mut restarted_server, managed_address).await?;
    wait_for_connection_reopen(&managed_sqlite, &agent_id, &turn_id, reconnect_baseline).await?;
    let (replayed_stream, _) = timeout(PROVIDER_TIMEOUT, provider_listener.accept())
        .await
        .map_err(|_| eyre!("cold recovery did not redispatch its uncommitted provider call"))??;
    let mut replayed_socket = accept_async(replayed_stream).await?;
    let replayed_generation = next_generation(&mut replayed_socket, &calls).await?;
    assert_request_contains(&replayed_generation, SERVER_CRASH_PROMPT)?;
    send_completed(
        &mut replayed_socket,
        "resp-managed-server-recovery",
        "server crash recovery answer",
    )
    .await?;
    let attached = wait_output(live, "client reattached after managed server crash").await?;
    assert_success(&attached, "client reattached after managed server crash")?;
    assert_final_answer(&attached, "server crash recovery answer")?;
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let mut replay_command = client.command();
    replay_command.args([
        "run",
        SERVER_CRASH_PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        SERVER_CRASH_KEY,
    ]);
    let unexpected_provider = provider_listener.accept();
    let replay = replay_command.output();
    tokio::pin!(unexpected_provider);
    tokio::pin!(replay);
    let replay = timeout(PROCESS_TIMEOUT, async {
        tokio::select! {
            accepted = &mut unexpected_provider => {
                let (_, peer) = accepted?;
                Err(eyre!("completed cold recovery redispatched provider work from {peer}"))
            }
            output = &mut replay => Ok(output?),
        }
    })
    .await
    .map_err(|_| eyre!("cold provider recovery did not settle"))??;
    assert_success(&replay, "completed cold recovery replay")?;
    assert_final_answer(&replay, "server crash recovery answer")?;
    let turn = client
        .output(["turn", &agent_id, &turn_id], "crash-test turn state")
        .await?;
    assert_success(&turn, "crash-test turn state")?;
    let turn: Value = serde_json::from_slice(&turn.stdout)?;
    assert_eq!(turn["state"], "completed");
    assert!(turn["error"].is_null());
    restarted_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, restarted_server.wait())
        .await
        .map_err(|_| eyre!("restarted crash-test server did not stop"))??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual compiled-binary concurrent cold idempotency gate"]
async fn concurrent_cold_same_key_runs_converge_without_duplicate_admission() -> Result<()> {
    const PROMPT: &str = "converge two cold managed submissions";
    const KEY: &str = "managed-concurrent-cold-operation";

    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    let client_home = fixture.path().join("nanocodex2-home");
    let managed_sqlite = fixture.path().join("managed.sqlite3");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&client_home)?;
    write_client_config(&client_home, &workspace)?;

    let provider_listener = TcpListener::bind("127.0.0.1:0").await?;
    let provider_endpoint = format!("ws://{}", provider_listener.local_addr()?);
    let provider_generations = Arc::new(AtomicUsize::new(0));
    let observed_generations = Arc::clone(&provider_generations);
    let (generation_seen, generation_seen_rx) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    let provider = tokio::spawn(async move {
        let (stream, _) = provider_listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let generation = next_generation(&mut socket, &observed_generations).await?;
        assert_request_contains(&generation, PROMPT)?;
        generation_seen
            .send(())
            .map_err(|_| eyre!("concurrent generation observer dropped"))?;
        release_rx
            .await
            .map_err(|_| eyre!("concurrent generation release dropped"))?;
        send_completed(&mut socket, "resp-managed-concurrent", "converged answer").await
    });

    let managed_address = unused_loopback_address()?;
    let bearer = format!("ncx_live_{}_{}", "q".repeat(12), "r".repeat(43));
    let mut managed_server = spawn_managed_server(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &bearer,
    )?;
    wait_for_listener(&mut managed_server, managed_address).await?;
    let client = ClientHarness {
        binary: nanocodex2_binary()?,
        origin: format!("http://{managed_address}"),
        bearer,
        home: client_home,
        workspace,
    };
    let created = client.output(["new"], "concurrent-test new").await?;
    assert_success(&created, "concurrent-test new")?;
    let receipt: Value = serde_json::from_slice(&created.stdout)?;
    let agent_id = receipt["agent_id"]
        .as_str()
        .ok_or_else(|| eyre!("concurrent-test receipt omitted agent_id"))?
        .to_owned();

    let first = client.spawn([
        "run",
        PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        KEY,
    ])?;
    let second = client.spawn([
        "run",
        PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        KEY,
    ])?;
    timeout(PROVIDER_TIMEOUT, generation_seen_rx)
        .await
        .map_err(|_| eyre!("concurrent provider generation was not observed"))??;
    wait_for_submission_count(&managed_sqlite, &agent_id, 2).await?;
    release
        .send(())
        .map_err(|_| eyre!("concurrent provider gate was already closed"))?;

    let (first, second) = tokio::try_join!(
        wait_output(first, "first concurrent run"),
        wait_output(second, "second concurrent run")
    )?;
    assert_success(&first, "first concurrent run")?;
    assert_success(&second, "second concurrent run")?;
    assert_final_answer(&first, "converged answer")?;
    assert_final_answer(&second, "converged answer")?;
    assert_eq!(provider_generations.load(Ordering::SeqCst), 1);
    assert_single_managed_admission(&managed_sqlite, &agent_id)?;

    timeout(PROVIDER_TIMEOUT, provider)
        .await
        .map_err(|_| eyre!("concurrent provider did not finish"))??
        .wrap_err("concurrent provider failed")?;
    managed_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, managed_server.wait()).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual managed protocol cancel-before-admission gate"]
async fn cancel_before_explicit_turn_admission_never_dispatches_provider_work() -> Result<()> {
    const PROMPT: &str = "this pre-cancelled turn must never execute";
    const KEY: &str = "managed-pre-admission-cancel-operation";
    const TURN: &str = "managed-pre-admission-cancel-turn";

    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    let client_home = fixture.path().join("nanocodex2-home");
    let managed_sqlite = fixture.path().join("managed.sqlite3");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&client_home)?;
    write_client_config(&client_home, &workspace)?;
    let provider_listener = TcpListener::bind("127.0.0.1:0").await?;
    let provider_endpoint = format!("ws://{}", provider_listener.local_addr()?);
    let managed_address = unused_loopback_address()?;
    let bearer = format!("ncx_live_{}_{}", "w".repeat(12), "x".repeat(43));
    let mut managed_server = spawn_managed_server(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &bearer,
    )?;
    wait_for_listener(&mut managed_server, managed_address).await?;
    let client = ClientHarness {
        binary: nanocodex2_binary()?,
        origin: format!("http://{managed_address}"),
        bearer: bearer.clone(),
        home: client_home,
        workspace,
    };
    let created = client.output(["new"], "pre-cancel new").await?;
    assert_success(&created, "pre-cancel new")?;
    let receipt: Value = serde_json::from_slice(&created.stdout)?;
    let agent_id = receipt["agent_id"]
        .as_str()
        .ok_or_else(|| eyre!("pre-cancel receipt omitted agent_id"))?;
    nanocodex::oai::transport::install_default_rustls_crypto_provider();
    let http = reqwest::Client::new();
    let cancel = http
        .post(format!(
            "{}/v1/agents/{agent_id}/turns/{TURN}/cancel",
            client.origin
        ))
        .bearer_auth(&bearer)
        .send()
        .await?;
    assert_eq!(cancel.status(), reqwest::StatusCode::OK);
    assert_eq!(cancel.json::<Value>().await?["state"], "cancelling");

    let submission_url = format!("{}/v1/agents/{agent_id}/turns", client.origin);
    let body = json!({"id":TURN,"input":PROMPT});
    let submitted = http
        .post(&submission_url)
        .bearer_auth(&bearer)
        .header("idempotency-key", KEY)
        .json(&body)
        .send()
        .await?;
    assert_eq!(submitted.status(), reqwest::StatusCode::ACCEPTED);
    let submitted = submitted.json::<Value>().await?;
    assert_eq!(submitted["turn_id"], TURN);
    assert_eq!(submitted["state"], "cancelled");
    assert_eq!(submitted["terminal"]["type"], "turn_cancelled");

    let replay = http
        .post(&submission_url)
        .bearer_auth(&bearer)
        .header("idempotency-key", KEY)
        .json(&body)
        .send()
        .await?;
    assert_eq!(replay.status(), reqwest::StatusCode::OK);
    assert_eq!(replay.json::<Value>().await?["state"], "cancelled");
    assert!(
        timeout(Duration::from_millis(500), provider_listener.accept())
            .await
            .is_err(),
        "pre-admission cancellation still dispatched provider work"
    );
    assert_pre_admission_cancel_order(&managed_sqlite, TURN)?;

    managed_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, managed_server.wait()).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual compiled-binary persisted cancellation recovery gate"]
async fn cold_recovery_redelivers_persisted_cancellation_intent() -> Result<()> {
    const PROMPT: &str = "cancel this turn across the managed server crash";
    const KEY: &str = "managed-cancel-recovery-operation";

    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    let client_home = fixture.path().join("nanocodex2-home");
    let managed_sqlite = fixture.path().join("managed.sqlite3");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&client_home)?;
    write_client_config(&client_home, &workspace)?;

    let provider_listener = TcpListener::bind("127.0.0.1:0").await?;
    let provider_endpoint = format!("ws://{}", provider_listener.local_addr()?);
    let managed_address = unused_loopback_address()?;
    let bearer = format!("ncx_live_{}_{}", "u".repeat(12), "v".repeat(43));
    let mut managed_server = spawn_managed_server_with_faults(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &bearer,
        10_000,
        0,
        0,
    )?;
    wait_for_listener(&mut managed_server, managed_address).await?;
    let client = ClientHarness {
        binary: nanocodex2_binary()?,
        origin: format!("http://{managed_address}"),
        bearer,
        home: client_home,
        workspace: workspace.clone(),
    };
    let created = client.output(["new"], "cancel-recovery new").await?;
    assert_success(&created, "cancel-recovery new")?;
    let receipt: Value = serde_json::from_slice(&created.stdout)?;
    let agent_id = receipt["agent_id"]
        .as_str()
        .ok_or_else(|| eyre!("cancel-recovery receipt omitted agent_id"))?
        .to_owned();

    let live = client.spawn([
        "run",
        PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        KEY,
    ])?;
    let (provider_stream, _) = timeout(PROVIDER_TIMEOUT, provider_listener.accept())
        .await
        .map_err(|_| eyre!("cancel-recovery provider connection was not opened"))??;
    let mut provider_socket = accept_async(provider_stream).await?;
    let generation = next_generation(&mut provider_socket, &AtomicUsize::new(0)).await?;
    assert_request_contains(&generation, PROMPT)?;
    let turn_id = wait_for_active_turn(&client, &agent_id, PROMPT).await?;

    let cancelling = client.spawn(["cancel", &agent_id, &turn_id])?;
    wait_for_managed_turn_state(&managed_sqlite, &turn_id, "cancelling").await?;
    managed_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, managed_server.wait()).await??;
    send_sigkill(&cancelling).await?;
    let _ = wait_output(cancelling, "interrupted cancellation request").await?;
    send_sigkill(&live).await?;
    let _ = wait_output(live, "cancel-recovery attached run").await?;
    drop(provider_socket);

    let mut restarted_server = spawn_managed_server(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &client.bearer,
    )?;
    wait_for_listener(&mut restarted_server, managed_address).await?;
    let mut replay = client.command();
    replay.args([
        "run",
        PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        KEY,
    ]);
    let unexpected_provider = provider_listener.accept();
    let replay = replay.output();
    tokio::pin!(unexpected_provider);
    tokio::pin!(replay);
    let replay = timeout(PROCESS_TIMEOUT, async {
        tokio::select! {
            accepted = &mut unexpected_provider => {
                let (_, peer) = accepted?;
                Err(eyre!("cancellation recovery redispatched the provider call from {peer}"))
            }
            output = &mut replay => Ok(output?),
        }
    })
    .await
    .map_err(|_| eyre!("persisted cancellation recovery did not settle"))??;
    assert!(!replay.status.success());
    let turn = client
        .output(["turn", &agent_id, &turn_id], "cancel-recovery turn state")
        .await?;
    assert_success(&turn, "cancel-recovery turn state")?;
    let turn: Value = serde_json::from_slice(&turn.stdout)?;
    assert_eq!(
        turn["state"], "cancelled",
        "persisted cancellation was lost: {turn}"
    );

    restarted_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, restarted_server.wait()).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual compiled-binary post-cancellation projection crash gate"]
async fn cold_recovery_projects_an_already_committed_inner_cancellation() -> Result<()> {
    const PROMPT: &str = "persist cancellation before the managed projection crashes";
    const KEY: &str = "managed-post-cancel-projection-operation";

    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    let client_home = fixture.path().join("nanocodex2-home");
    let managed_sqlite = fixture.path().join("managed.sqlite3");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&client_home)?;
    write_client_config(&client_home, &workspace)?;
    let provider_listener = TcpListener::bind("127.0.0.1:0").await?;
    let provider_endpoint = format!("ws://{}", provider_listener.local_addr()?);
    let managed_address = unused_loopback_address()?;
    let bearer = format!("ncx_live_{}_{}", "y".repeat(12), "z".repeat(43));
    let mut managed_server = spawn_managed_server_with_faults(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &bearer,
        0,
        10_000,
        0,
    )?;
    wait_for_listener(&mut managed_server, managed_address).await?;
    let client = ClientHarness {
        binary: nanocodex2_binary()?,
        origin: format!("http://{managed_address}"),
        bearer,
        home: client_home,
        workspace: workspace.clone(),
    };
    let created = client.output(["new"], "post-cancel new").await?;
    assert_success(&created, "post-cancel new")?;
    let receipt: Value = serde_json::from_slice(&created.stdout)?;
    let agent_id = receipt["agent_id"]
        .as_str()
        .ok_or_else(|| eyre!("post-cancel receipt omitted agent_id"))?
        .to_owned();

    let live = client.spawn([
        "run",
        PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        KEY,
    ])?;
    let (provider_stream, _) = timeout(PROVIDER_TIMEOUT, provider_listener.accept())
        .await
        .map_err(|_| eyre!("post-cancel provider connection was not opened"))??;
    let mut provider_socket = accept_async(provider_stream).await?;
    let generation = next_generation(&mut provider_socket, &AtomicUsize::new(0)).await?;
    assert_request_contains(&generation, PROMPT)?;
    let turn_id = wait_for_active_turn(&client, &agent_id, PROMPT).await?;
    let cancel = client
        .output(["cancel", &agent_id, &turn_id], "post-cancel cancellation")
        .await?;
    assert_success(&cancel, "post-cancel cancellation")?;
    wait_for_pending_terminal_kind(&managed_sqlite, &turn_id, "run.failed").await?;

    managed_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, managed_server.wait()).await??;
    send_sigkill(&live).await?;
    let _ = wait_output(live, "post-cancel attached run").await?;
    drop(provider_socket);

    let mut restarted_server = spawn_managed_server(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &client.bearer,
    )?;
    wait_for_listener(&mut restarted_server, managed_address).await?;
    let unexpected_provider = provider_listener.accept();
    let replay = client.output(
        [
            "run",
            PROMPT,
            "--agent",
            &agent_id,
            "--idempotency-key",
            KEY,
        ],
        "post-cancel replay",
    );
    tokio::pin!(unexpected_provider);
    tokio::pin!(replay);
    let replay = timeout(PROCESS_TIMEOUT, async {
        tokio::select! {
            accepted = &mut unexpected_provider => {
                let (_, peer) = accepted?;
                Err(eyre!("already-cancelled recovery redispatched provider work from {peer}"))
            }
            output = &mut replay => Ok(output?),
        }
    })
    .await
    .map_err(|_| eyre!("already-cancelled recovery did not settle"))??;
    assert!(!replay.status.success());
    assert!(
        String::from_utf8_lossy(&replay.stderr).contains("the turn was cancelled"),
        "already-cancelled replay did not return its retained cancellation"
    );
    assert_cancel_projection_counts(&managed_sqlite, &turn_id)?;

    restarted_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, restarted_server.wait()).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual compiled-binary terminal projection crash gate"]
async fn cold_replay_deduplicates_nested_and_managed_terminal_projection() -> Result<()> {
    const PROMPT: &str = "complete immediately before terminal projection crashes";
    const KEY: &str = "managed-terminal-projection-operation";

    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    let client_home = fixture.path().join("nanocodex2-home");
    let managed_sqlite = fixture.path().join("managed.sqlite3");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&client_home)?;
    write_client_config(&client_home, &workspace)?;

    let provider_listener = TcpListener::bind("127.0.0.1:0").await?;
    let provider_endpoint = format!("ws://{}", provider_listener.local_addr()?);
    let managed_address = unused_loopback_address()?;
    let bearer = format!("ncx_live_{}_{}", "x".repeat(12), "y".repeat(43));
    let mut managed_server = spawn_managed_server_with_faults(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &bearer,
        0,
        10_000,
        0,
    )?;
    wait_for_listener(&mut managed_server, managed_address).await?;
    let client = ClientHarness {
        binary: nanocodex2_binary()?,
        origin: format!("http://{managed_address}"),
        bearer,
        home: client_home,
        workspace: workspace.clone(),
    };
    let created = client.output(["new"], "terminal-projection new").await?;
    assert_success(&created, "terminal-projection new")?;
    let receipt: Value = serde_json::from_slice(&created.stdout)?;
    let agent_id = receipt["agent_id"]
        .as_str()
        .ok_or_else(|| eyre!("terminal-projection receipt omitted agent_id"))?
        .to_owned();

    let live = client.spawn([
        "run",
        PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        KEY,
    ])?;
    let (provider_stream, _) = timeout(PROVIDER_TIMEOUT, provider_listener.accept())
        .await
        .map_err(|_| eyre!("terminal-projection provider connection was not opened"))??;
    let mut provider_socket = accept_async(provider_stream).await?;
    let generation = next_generation(&mut provider_socket, &AtomicUsize::new(0)).await?;
    assert_request_contains(&generation, PROMPT)?;
    send_completed(
        &mut provider_socket,
        "resp-terminal-projection",
        "terminal projection answer",
    )
    .await?;
    let turn_id = wait_for_active_turn(&client, &agent_id, PROMPT).await?;
    wait_for_nested_terminal_count(&managed_sqlite, &turn_id, 1).await?;
    wait_for_terminal_transaction_barrier(&managed_sqlite, &turn_id).await?;

    managed_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, managed_server.wait()).await??;
    send_sigkill(&live).await?;
    let _ = wait_output(live, "terminal-projection attached run").await?;
    drop(provider_socket);

    let mut restarted_server = spawn_managed_server(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &client.bearer,
    )?;
    wait_for_listener(&mut restarted_server, managed_address).await?;
    let mut replay = client.command();
    replay.args([
        "run",
        PROMPT,
        "--agent",
        &agent_id,
        "--idempotency-key",
        KEY,
    ]);
    let unexpected_provider = provider_listener.accept();
    let replay = replay.output();
    tokio::pin!(unexpected_provider);
    tokio::pin!(replay);
    let replay = timeout(PROCESS_TIMEOUT, async {
        tokio::select! {
            accepted = &mut unexpected_provider => {
                let (_, peer) = accepted?;
                Err(eyre!("terminal projection replay redispatched the provider from {peer}"))
            }
            output = &mut replay => Ok(output?),
        }
    })
    .await
    .map_err(|_| eyre!("terminal projection replay did not settle"))??;
    assert_success(&replay, "terminal projection replay")?;
    assert_final_answer(&replay, "terminal projection answer")?;
    assert_terminal_projection_counts(&managed_sqlite, &turn_id)?;

    restarted_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, restarted_server.wait()).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual compiled-binary tool-host authority gate"]
async fn invalid_tool_host_candidate_cannot_fence_ready_incumbent() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    let client_home = fixture.path().join("nanocodex2-home");
    let managed_sqlite = fixture.path().join("managed.sqlite3");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&client_home)?;
    write_client_config(&client_home, &workspace)?;

    let managed_address = unused_loopback_address()?;
    let bearer = format!("ncx_live_{}_{}", "u".repeat(12), "v".repeat(43));
    let mut managed_server = spawn_managed_server_with_faults(
        managed_address,
        &managed_sqlite,
        &workspace,
        "ws://127.0.0.1:9",
        &bearer,
        0,
        0,
        500,
    )?;
    wait_for_listener(&mut managed_server, managed_address).await?;
    let client = ClientHarness {
        binary: nanocodex2_binary()?,
        origin: format!("http://{managed_address}"),
        bearer: bearer.clone(),
        home: client_home,
        workspace,
    };
    let created = client.output(["new"], "tool-host authority new").await?;
    assert_success(&created, "tool-host authority new")?;
    let receipt: Value = serde_json::from_slice(&created.stdout)?;
    let agent_id = receipt["agent_id"]
        .as_str()
        .ok_or_else(|| eyre!("tool-host authority receipt omitted agent_id"))?;
    let url = format!("ws://{managed_address}/v1/agents/{agent_id}/tool-host");

    let mut incumbent_request = url.as_str().into_client_request()?;
    incumbent_request
        .headers_mut()
        .insert("authorization", format!("Bearer {bearer}").parse()?);
    let (mut incumbent, _) = connect_async(incumbent_request).await?;
    incumbent
        .send(Message::Text(
            json!({"type":"catalog", "tools":[]}).to_string().into(),
        ))
        .await?;
    let ready = timeout(PROVIDER_TIMEOUT, incumbent.next())
        .await
        .map_err(|_| eyre!("incumbent tool host was not readied"))?
        .ok_or_else(|| eyre!("incumbent tool host closed before ready"))??;
    assert_eq!(
        serde_json::from_str::<Value>(ready.to_text()?)?["type"],
        "ready"
    );

    let mut candidate_request = url.as_str().into_client_request()?;
    candidate_request
        .headers_mut()
        .insert("authorization", format!("Bearer {bearer}").parse()?);
    let (mut candidate, _) = connect_async(candidate_request).await?;
    candidate.send(Message::Text("{}".into())).await?;
    let candidate_end = timeout(PROVIDER_TIMEOUT, candidate.next())
        .await
        .map_err(|_| eyre!("invalid tool-host candidate was left open"))?;
    assert!(
        candidate_end.is_none()
            || matches!(candidate_end, Some(Ok(Message::Close(_))) | Some(Err(_))),
        "invalid candidate received an unexpected frame: {candidate_end:?}"
    );

    incumbent
        .send(Message::Text(
            json!({"type":"ping", "nonce":"incumbent-still-authoritative"})
                .to_string()
                .into(),
        ))
        .await?;
    let pong = timeout(PROVIDER_TIMEOUT, incumbent.next())
        .await
        .map_err(|_| eyre!("incumbent did not answer after invalid replacement"))?
        .ok_or_else(|| eyre!("invalid replacement fenced the incumbent"))??;
    let pong: Value = serde_json::from_str(pong.to_text()?)?;
    assert_eq!(pong["type"], "pong");
    assert_eq!(pong["nonce"], "incumbent-still-authoritative");

    let mut replacement_request = url.as_str().into_client_request()?;
    replacement_request
        .headers_mut()
        .insert("authorization", format!("Bearer {bearer}").parse()?);
    let (mut replacement, _) = connect_async(replacement_request).await?;
    let ready_baseline = tool_host_connections(&managed_sqlite, agent_id)?;
    replacement
        .send(Message::Text(
            json!({"type":"catalog", "tools":[]}).to_string().into(),
        ))
        .await?;
    wait_for_tool_host_count(&managed_sqlite, agent_id, ready_baseline + 1).await?;
    incumbent
        .send(Message::Text(
            json!({"type":"ping", "nonce":"before-replacement-ready"})
                .to_string()
                .into(),
        ))
        .await?;
    let pong = timeout(PROVIDER_TIMEOUT, incumbent.next())
        .await
        .map_err(|_| eyre!("incumbent did not answer before replacement ready"))?
        .ok_or_else(|| eyre!("stalled valid candidate fenced the incumbent"))??;
    let pong: Value = serde_json::from_str(pong.to_text()?)?;
    assert_eq!(pong["nonce"], "before-replacement-ready");
    let ready = timeout(PROVIDER_TIMEOUT, replacement.next())
        .await
        .map_err(|_| eyre!("valid replacement was not readied"))?
        .ok_or_else(|| eyre!("valid replacement closed before ready"))??;
    assert_eq!(
        serde_json::from_str::<Value>(ready.to_text()?)?["type"],
        "ready"
    );
    wait_for_websocket_close(&mut incumbent).await?;
    replacement
        .send(Message::Text(
            json!({"type":"ping", "nonce":"replacement-authoritative"})
                .to_string()
                .into(),
        ))
        .await?;
    let pong = timeout(PROVIDER_TIMEOUT, replacement.next())
        .await
        .map_err(|_| eyre!("ready replacement did not answer"))?
        .ok_or_else(|| eyre!("ready replacement was not authoritative"))??;
    let pong: Value = serde_json::from_str(pong.to_text()?)?;
    assert_eq!(pong["nonce"], "replacement-authoritative");

    managed_server.start_kill()?;
    timeout(PROCESS_TIMEOUT, managed_server.wait()).await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual compiled-binary graceful shutdown gate"]
async fn sigterm_closes_live_sse_tool_host_and_turn_tasks() -> Result<()> {
    const PROMPT: &str = "hold open every managed connection during shutdown";
    const KEY: &str = "managed-graceful-shutdown-operation";

    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    let client_home = fixture.path().join("nanocodex2-home");
    let managed_sqlite = fixture.path().join("managed.sqlite3");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&client_home)?;
    write_client_config(&client_home, &workspace)?;
    let provider_listener = TcpListener::bind("127.0.0.1:0").await?;
    let provider_endpoint = format!("ws://{}", provider_listener.local_addr()?);
    let managed_address = unused_loopback_address()?;
    let bearer = format!("ncx_live_{}_{}", "s".repeat(12), "t".repeat(43));
    let mut managed_server = spawn_managed_server(
        managed_address,
        &managed_sqlite,
        &workspace,
        &provider_endpoint,
        &bearer,
    )?;
    wait_for_listener(&mut managed_server, managed_address).await?;
    let client = ClientHarness {
        binary: nanocodex2_binary()?,
        origin: format!("http://{managed_address}"),
        bearer,
        home: client_home,
        workspace,
    };
    let created = client.output(["new"], "shutdown-test new").await?;
    assert_success(&created, "shutdown-test new")?;
    let receipt: Value = serde_json::from_slice(&created.stdout)?;
    let agent_id = receipt["agent_id"]
        .as_str()
        .ok_or_else(|| eyre!("shutdown-test receipt omitted agent_id"))?
        .to_owned();
    let tool_url = format!("ws://{managed_address}/v1/agents/{agent_id}/tool-host");
    let mut tool_request = tool_url.as_str().into_client_request()?;
    tool_request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", client.bearer).parse()?,
    );
    let (mut tool_socket, _) = connect_async(tool_request).await?;
    tool_socket
        .send(Message::Text(
            json!({"type":"catalog", "tools":[]}).to_string().into(),
        ))
        .await?;
    let ready = timeout(PROVIDER_TIMEOUT, tool_socket.next())
        .await
        .map_err(|_| eyre!("shutdown-test tool host was not readied"))?
        .ok_or_else(|| eyre!("shutdown-test tool host closed before ready"))??;
    assert_eq!(
        serde_json::from_str::<Value>(ready.to_text()?)?["type"],
        "ready"
    );

    nanocodex::oai::transport::install_default_rustls_crypto_provider();
    let http = reqwest::Client::new();
    let sse = http
        .get(format!(
            "{}/v1/agents/{agent_id}/events?cursor=0",
            client.origin
        ))
        .bearer_auth(&client.bearer)
        .send()
        .await?;
    assert_eq!(sse.status(), reqwest::StatusCode::OK);
    let submitted = http
        .post(format!("{}/v1/agents/{agent_id}/turns", client.origin))
        .bearer_auth(&client.bearer)
        .header("idempotency-key", KEY)
        .json(&json!({"input":PROMPT}))
        .send()
        .await?;
    assert_eq!(submitted.status(), reqwest::StatusCode::ACCEPTED);
    let submitted = submitted.json::<Value>().await?;
    let turn_id = submitted["turn_id"]
        .as_str()
        .ok_or_else(|| eyre!("shutdown-test submit omitted turn_id"))?
        .to_owned();
    let (provider_stream, _) = timeout(PROVIDER_TIMEOUT, provider_listener.accept()).await??;
    let mut provider_socket = accept_async(provider_stream).await?;
    let generation = next_generation(&mut provider_socket, &AtomicUsize::new(0)).await?;
    assert_request_contains(&generation, PROMPT)?;

    send_signal(&managed_server, "-TERM").await?;
    let status = timeout(PROCESS_TIMEOUT, managed_server.wait())
        .await
        .map_err(|_| eyre!("managed server hung while draining live connections"))??;
    if !status.success() {
        return Err(eyre!("managed server SIGTERM shutdown failed: {status}"));
    }
    wait_for_websocket_close(&mut tool_socket).await?;
    wait_for_http_stream_close(sse).await?;
    wait_for_websocket_close(&mut provider_socket).await?;
    assert_managed_turn_is_terminal(&managed_sqlite, &turn_id)?;
    Ok(())
}

async fn serve_provider(
    listener: TcpListener,
    calls: Arc<AtomicUsize>,
    signals: ProviderSignals,
) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let mut socket = accept_async(stream).await?;

    let first = next_generation(&mut socket, &calls).await?;
    assert_request_contains(&first, FIRST_PROMPT)?;
    send_completed(&mut socket, "resp-managed-first", "first managed answer").await?;

    let detached = next_generation(&mut socket, &calls).await?;
    signals
        .detach_seen
        .send(detached)
        .map_err(|_| eyre!("detach request observer dropped"))?;
    signals
        .detach_release
        .await
        .map_err(|_| eyre!("detach release sender dropped"))?;
    send_completed(&mut socket, "resp-managed-detach", "detached client answer").await?;

    let steer_boundary = next_generation(&mut socket, &calls).await?;
    signals
        .steer_seen
        .send(steer_boundary)
        .map_err(|_| eyre!("steer request observer dropped"))?;
    signals
        .steer_release
        .await
        .map_err(|_| eyre!("steer release sender dropped"))?;
    send_completed(
        &mut socket,
        "resp-managed-steer-boundary",
        "boundary answer before steering",
    )
    .await?;

    let steered = next_generation(&mut socket, &calls).await?;
    assert_request_contains(&steered, STEER_INPUT)?;
    assert_eq!(
        steered["previous_response_id"], "resp-managed-steer-boundary",
        "steered generation did not continue from the provider boundary: {steered}"
    );
    send_completed(
        &mut socket,
        "resp-managed-steered",
        "steered managed answer",
    )
    .await?;

    let cancellable = next_generation(&mut socket, &calls).await?;
    signals
        .cancel_seen
        .send(cancellable)
        .map_err(|_| eyre!("cancel request observer dropped"))?;
    signals
        .cancel_issued
        .await
        .map_err(|_| eyre!("cancel-issued sender dropped"))?;
    Ok(())
}

async fn next_generation<S>(socket: &mut WebSocketStream<S>, calls: &AtomicUsize) -> Result<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(|| eyre!("managed server closed before the next Responses request"))??;
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(text.as_str())?;
            calls.fetch_add(1, Ordering::SeqCst);
            return Ok(value);
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

fn spawn_managed_server(
    address: SocketAddr,
    sqlite: &Path,
    workspace: &Path,
    provider_endpoint: &str,
    bearer: &str,
) -> Result<Child> {
    spawn_managed_server_with_faults(
        address,
        sqlite,
        workspace,
        provider_endpoint,
        bearer,
        0,
        0,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_managed_server_with_faults(
    address: SocketAddr,
    sqlite: &Path,
    workspace: &Path,
    provider_endpoint: &str,
    bearer: &str,
    cancel_delay_ms: u64,
    terminal_delay_ms: u64,
    tool_ready_delay_ms: u64,
) -> Result<Child> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
    command
        .arg("managed-server")
        .arg("--bind")
        .arg(address.to_string())
        .arg("--sqlite")
        .arg(sqlite)
        .arg("--workspace")
        .arg(workspace)
        .arg("--openai-api-key")
        .arg("test-openai-key")
        .arg("--openai-websocket-url")
        .arg(provider_endpoint)
        .arg("--bearer")
        .arg(bearer)
        .arg("--fault-cancel-delay-ms")
        .arg(cancel_delay_ms.to_string())
        .arg("--fault-terminal-delay-ms")
        .arg(terminal_delay_ms.to_string())
        .arg("--fault-tool-ready-delay-ms")
        .arg(tool_ready_delay_ms.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    command.spawn().wrap_err("failed to spawn managed-server")
}

fn nanocodex2_binary() -> Result<PathBuf> {
    let path = env::var_os("NANOCODEX2_TEST_BINARY")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("target/debug/nanocodex2")
        });
    if !path.is_file() {
        return Err(eyre!(
            "nanocodex2 test binary does not exist at {}; set NANOCODEX2_TEST_BINARY or prebuild workspace target/debug/nanocodex2",
            path.display()
        ));
    }
    Ok(path)
}

fn write_client_config(home: &Path, workspace: &Path) -> Result<()> {
    let workspace = workspace
        .to_str()
        .ok_or_else(|| eyre!("workspace path is not UTF-8"))?;
    let quoted = serde_json::to_string(workspace)?;
    std::fs::write(
        home.join("config.toml"),
        format!("[agent]\nworkspace = {quoted}\n"),
    )?;
    Ok(())
}

fn unused_loopback_address() -> Result<SocketAddr> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.local_addr().map_err(Into::into)
}

async fn wait_for_listener(child: &mut Child, address: SocketAddr) -> Result<()> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(eyre!(
                "managed-server exited before becoming ready: {status}"
            ));
        }
        if TcpStream::connect(address).await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "managed-server did not listen on {address} within {STATE_TIMEOUT:?}"
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_active_turn(
    client: &ClientHarness,
    agent_id: &str,
    prompt: &str,
) -> Result<String> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let output = client
            .output(["state", agent_id], "nanocodex2 state")
            .await?;
        assert_success(&output, "nanocodex2 state")?;
        let state: Value = serde_json::from_slice(&output.stdout)?;
        if let Some(turn_id) = state["active_turn_details"].as_array().and_then(|turns| {
            turns.iter().find_map(|turn| {
                (turn["input"] == prompt)
                    .then(|| turn["id"].as_str().map(str::to_owned))
                    .flatten()
            })
        }) {
            return Ok(turn_id);
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "turn for prompt {prompt:?} did not become active within {STATE_TIMEOUT:?}; last state: {}",
                state
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_no_active_turns(client: &ClientHarness, agent_id: &str) -> Result<()> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let output = client
            .output(["state", agent_id], "nanocodex2 state")
            .await?;
        assert_success(&output, "nanocodex2 state")?;
        let state: Value = serde_json::from_slice(&output.stdout)?;
        if state["active_turns"].as_array().is_some_and(Vec::is_empty) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "cancelled turn remained active within {STATE_TIMEOUT:?}: {state}"
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

fn managed_observations(sqlite: &Path, agent: &str, turn: &str) -> Result<(i64, i64, i64)> {
    let connection = rusqlite::Connection::open(sqlite)?;
    connection.busy_timeout(Duration::from_secs(1))?;
    let (sse, tool_hosts) = connection.query_row(
        "SELECT sse_connections,tool_host_connections FROM local_managed_agents WHERE agent_id=?1",
        [agent],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let submissions = connection.query_row(
        "SELECT submission_count FROM local_managed_turns WHERE turn_id=?1",
        [turn],
        |row| row.get(0),
    )?;
    Ok((submissions, sse, tool_hosts))
}

fn tool_host_connections(sqlite: &Path, agent: &str) -> Result<i64> {
    let connection = rusqlite::Connection::open(sqlite)?;
    connection
        .query_row(
            "SELECT tool_host_connections FROM local_managed_agents WHERE agent_id=?1",
            [agent],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

async fn wait_for_tool_host_count(sqlite: &Path, agent: &str, expected: i64) -> Result<()> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let count = tool_host_connections(sqlite, agent)?;
        if count >= expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "tool-host ready barrier remained at {count}, expected {expected}"
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_submission_count(sqlite: &Path, agent: &str, expected: i64) -> Result<()> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let connection = rusqlite::Connection::open(sqlite)?;
        connection.busy_timeout(Duration::from_secs(1))?;
        let count = connection
            .query_row(
                "SELECT submission_count FROM local_managed_turns WHERE agent_id=?1",
                [agent],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0);
        if count >= expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "managed submission count remained {count}, expected at least {expected}"
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_managed_turn_state(sqlite: &Path, turn: &str, expected: &str) -> Result<()> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let connection = rusqlite::Connection::open(sqlite)?;
        connection.busy_timeout(Duration::from_secs(1))?;
        let state = connection
            .query_row(
                "SELECT state FROM local_managed_turns WHERE turn_id=?1",
                [turn],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_default();
        if state == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "managed turn {turn} remained in state {state:?}, expected {expected:?}"
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_nested_terminal_count(sqlite: &Path, turn: &str, expected: i64) -> Result<()> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let connection = rusqlite::Connection::open(sqlite)?;
        connection.busy_timeout(Duration::from_secs(1))?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM local_managed_turns WHERE turn_id=?1 AND pending_terminal_json LIKE '%run.completed%'",
            [turn],
            |row| row.get(0),
        )?;
        if count >= expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "nested terminal count remained {count}, expected at least {expected}"
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_terminal_transaction_barrier(sqlite: &Path, turn: &str) -> Result<()> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let connection = rusqlite::Connection::open(sqlite)?;
        connection.busy_timeout(Duration::ZERO)?;
        match connection.execute_batch("BEGIN IMMEDIATE") {
            Ok(()) => connection.execute_batch("ROLLBACK")?,
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                let (state, pending): (String, Option<String>) = connection.query_row(
                    "SELECT state,pending_terminal_json FROM local_managed_turns WHERE turn_id=?1",
                    [turn],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let visible_terminals: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM local_managed_events WHERE turn_id=?1 AND kind IN ('event','turn_completed') AND (kind='turn_completed' OR body_json LIKE '%run.completed%')",
                    [turn],
                    |row| row.get(0),
                )?;
                if !matches!(state.as_str(), "accepted" | "cancelling")
                    || pending
                        .as_deref()
                        .is_none_or(|value| !value.contains("run.completed"))
                    || visible_terminals != 0
                {
                    return Err(eyre!(
                        "uncommitted terminal transaction leaked: state={state}, pending={pending:?}, visible_terminals={visible_terminals}"
                    ));
                }
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "terminal projection never entered its faulted SQLite transaction"
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_pending_terminal_kind(sqlite: &Path, turn: &str, kind: &str) -> Result<()> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let connection = rusqlite::Connection::open(sqlite)?;
        connection.busy_timeout(Duration::from_secs(1))?;
        let pending: Option<String> = connection.query_row(
            "SELECT pending_terminal_json FROM local_managed_turns WHERE turn_id=?1",
            [turn],
            |row| row.get(0),
        )?;
        if pending.as_deref().is_some_and(|value| value.contains(kind)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "managed turn {turn} did not persist pending terminal {kind:?}"
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

fn assert_terminal_projection_counts(sqlite: &Path, turn: &str) -> Result<()> {
    let connection = rusqlite::Connection::open(sqlite)?;
    let (nested, nested_cursor): (i64, Option<i64>) = connection.query_row(
        "SELECT COUNT(*),MIN(cursor) FROM local_managed_events WHERE turn_id=?1 AND kind='event' AND body_json LIKE '%run.completed%'",
        [turn],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (managed, managed_cursor): (i64, Option<i64>) = connection.query_row(
        "SELECT COUNT(*),MIN(cursor) FROM local_managed_events WHERE turn_id=?1 AND kind='turn_completed'",
        [turn],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if nested != 1 || managed != 1 {
        return Err(eyre!(
            "terminal replay produced nested={nested}, managed={managed}; expected one of each"
        ));
    }
    let nested_cursor = nested_cursor.ok_or_else(|| eyre!("nested terminal omitted cursor"))?;
    let managed_cursor = managed_cursor.ok_or_else(|| eyre!("managed terminal omitted cursor"))?;
    if nested_cursor >= managed_cursor {
        return Err(eyre!(
            "terminal lifecycle order was inverted: nested={nested_cursor}, managed={managed_cursor}"
        ));
    }
    let (state, row_cursor, terminal_json, pending): (
        String,
        Option<i64>,
        Option<String>,
        Option<String>,
    ) = connection.query_row(
        "SELECT state,terminal_cursor,terminal_json,pending_terminal_json FROM local_managed_turns WHERE turn_id=?1",
        [turn],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let terminal: Value = serde_json::from_str(
        terminal_json
            .as_deref()
            .ok_or_else(|| eyre!("completed turn omitted terminal_json"))?,
    )?;
    let managed_body: String = connection.query_row(
        "SELECT body_json FROM local_managed_events WHERE turn_id=?1 AND kind='turn_completed'",
        [turn],
        |row| row.get(0),
    )?;
    let managed_body: Value = serde_json::from_str(&managed_body)?;
    if state != "completed"
        || row_cursor != Some(managed_cursor)
        || terminal != managed_body
        || pending.is_some()
    {
        return Err(eyre!(
            "terminal row/event projection was not atomic: state={state}, row_cursor={row_cursor:?}, managed_cursor={managed_cursor}, terminal={terminal}, pending={pending:?}"
        ));
    }
    Ok(())
}

fn assert_cancel_projection_counts(sqlite: &Path, turn: &str) -> Result<()> {
    let connection = rusqlite::Connection::open(sqlite)?;
    let (nested, nested_cursor): (i64, Option<i64>) = connection.query_row(
        "SELECT COUNT(*),MIN(cursor) FROM local_managed_events WHERE turn_id=?1 AND kind='event' AND body_json LIKE '%run.failed%'",
        [turn],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (managed, managed_cursor): (i64, Option<i64>) = connection.query_row(
        "SELECT COUNT(*),MIN(cursor) FROM local_managed_events WHERE turn_id=?1 AND kind='turn_cancelled'",
        [turn],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if nested != 1 || managed != 1 || nested_cursor >= managed_cursor {
        return Err(eyre!(
            "cancellation recovery projected nested={nested}@{nested_cursor:?}, managed={managed}@{managed_cursor:?}"
        ));
    }
    Ok(())
}

fn assert_pre_admission_cancel_order(sqlite: &Path, turn: &str) -> Result<()> {
    let connection = rusqlite::Connection::open(sqlite)?;
    let cursors = connection
        .prepare("SELECT kind,cursor FROM local_managed_events WHERE turn_id=?1 ORDER BY cursor")?
        .query_map([turn], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let kinds = cursors
        .iter()
        .map(|(kind, _)| kind.as_str())
        .collect::<Vec<_>>();
    if kinds != ["turn_accepted", "turn_cancelling", "turn_cancelled"] {
        return Err(eyre!(
            "pre-admission cancellation lifecycle was not ordered exactly once: {cursors:?}"
        ));
    }
    Ok(())
}

fn assert_single_managed_admission(sqlite: &Path, agent: &str) -> Result<()> {
    let connection = rusqlite::Connection::open(sqlite)?;
    let turns: i64 = connection.query_row(
        "SELECT COUNT(*) FROM local_managed_turns WHERE agent_id=?1",
        [agent],
        |row| row.get(0),
    )?;
    let accepted_events: i64 = connection.query_row(
        "SELECT COUNT(*) FROM local_managed_events WHERE agent_id=?1 AND kind='turn_accepted'",
        [agent],
        |row| row.get(0),
    )?;
    if turns != 1 || accepted_events != 1 {
        return Err(eyre!(
            "concurrent cold submissions produced turns={turns}, accepted_events={accepted_events}"
        ));
    }
    Ok(())
}

async fn wait_for_reconnect_barriers(
    sqlite: &Path,
    agent: &str,
    turn: &str,
    baseline: (i64, i64, i64),
) -> Result<()> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let observed = managed_observations(sqlite, agent, turn)?;
        if observed.0 > baseline.0 && observed.1 > baseline.1 && observed.2 > baseline.2 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "replacement client did not reopen the retained POST/SSE/tool-host boundaries; baseline={baseline:?}, observed={observed:?}"
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_connection_reopen(
    sqlite: &Path,
    agent: &str,
    turn: &str,
    baseline: (i64, i64, i64),
) -> Result<()> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let observed = managed_observations(sqlite, agent, turn)?;
        if observed.1 > baseline.1 && observed.2 > baseline.2 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(eyre!(
                "attached client did not reopen its SSE/tool-host boundaries; baseline={baseline:?}, observed={observed:?}"
            ));
        }
        sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_websocket_close<S>(socket: &mut WebSocketStream<S>) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    timeout(PROVIDER_TIMEOUT, async {
        loop {
            match socket.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return Ok(()),
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .map_err(|_| eyre!("WebSocket stayed open after its owner shut down"))?
}

async fn wait_for_http_stream_close(response: reqwest::Response) -> Result<()> {
    let mut stream = response.bytes_stream();
    timeout(PROVIDER_TIMEOUT, async {
        loop {
            match stream.next().await {
                None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .map_err(|_| eyre!("SSE response stayed open after managed shutdown"))
}

fn assert_managed_turn_is_terminal(sqlite: &Path, turn: &str) -> Result<()> {
    let connection = rusqlite::Connection::open(sqlite)?;
    let state: String = connection.query_row(
        "SELECT state FROM local_managed_turns WHERE turn_id=?1",
        [turn],
        |row| row.get(0),
    )?;
    if matches!(state.as_str(), "accepted" | "cancelling") {
        return Err(eyre!(
            "managed shutdown left turn {turn} nonterminal in state {state}"
        ));
    }
    Ok(())
}

async fn wait_output(child: Child, description: &str) -> Result<Output> {
    timeout(PROCESS_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| eyre!("{description} exceeded {PROCESS_TIMEOUT:?}"))?
        .wrap_err_with(|| format!("failed to wait for {description}"))
}

async fn send_sigkill(child: &Child) -> Result<()> {
    send_signal(child, "-KILL").await
}

async fn send_signal(child: &Child, signal: &str) -> Result<()> {
    let pid = child.id().ok_or_else(|| eyre!("child had no process ID"))?;
    let status = Command::new("kill")
        .args([signal, &pid.to_string()])
        .status()
        .await?;
    if !status.success() {
        return Err(eyre!("failed to send {signal} to process {pid}"));
    }
    Ok(())
}

fn assert_request_contains(request: &Value, expected: &str) -> Result<()> {
    if request["generate"] == false || !request["input"].to_string().contains(expected) {
        return Err(eyre!(
            "Responses generation did not contain {expected:?}: {request}"
        ));
    }
    Ok(())
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

fn assert_final_answer(output: &Output, expected: &str) -> Result<()> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.lines().any(|line| line == expected) {
        return Err(eyre!(
            "managed run did not print final answer {expected:?}:\n{stderr}"
        ));
    }
    Ok(())
}

fn assert_cancelled_terminal(output: &Output) -> Result<()> {
    let events = jsonl_events(&output.stdout)?;
    if !events
        .iter()
        .any(|event| event["type"] == "run.failed" && event["payload"]["status"] == "cancelled")
    {
        return Err(eyre!(
            "cancelled managed run omitted its cancelled terminal: {events:?}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

fn assert_secret_absent(output: &Output, bearer: &str) -> Result<()> {
    if output
        .stdout
        .windows(bearer.len())
        .any(|bytes| bytes == bearer.as_bytes())
        || output
            .stderr
            .windows(bearer.len())
            .any(|bytes| bytes == bearer.as_bytes())
    {
        return Err(eyre!("nanocodex2 output exposed its managed bearer"));
    }
    Ok(())
}

fn jsonl_events(bytes: &[u8]) -> Result<Vec<Value>> {
    String::from_utf8(bytes.to_vec())?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}
