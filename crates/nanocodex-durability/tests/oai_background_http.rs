//! Real HTTP Responses and SQLite exercise owned summary overlap and recovery.
#![cfg(feature = "sqlite")]
use axum::{Json, Router, response::IntoResponse, routing::post};
use nanocodex_agent::{Nanocodex, OpenAi, PromptRequest, transport::ResponsesTransport};
use nanocodex_durability::{DurableAgentExt, DurableSession, SqliteStore};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Notify, mpsc},
    time::timeout,
};

#[derive(Clone)]
struct Provider {
    requests: Arc<Mutex<Vec<Value>>>,
    generations: Arc<AtomicUsize>,
    summaries: Arc<AtomicUsize>,
    arrived: mpsc::UnboundedSender<(bool, usize)>,
    release_summary: Arc<Notify>,
    release_final: Arc<Notify>,
    hard: bool,
}
fn completed(id: &str, text: &str, end_turn: bool, tokens: u64) -> String {
    let event = json!({"type":"response.completed","response":{
        "id":id,"status":"completed","end_turn":end_turn,
        "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}],
        "usage":{"input_tokens":tokens-2,"output_tokens":2,"total_tokens":tokens}
    }});
    let created = json!({"type":"response.created","response":{"id":id}});
    let item = json!({"type":"response.output_item.done","output_index":0,"item":{
        "id":format!("msg_{id}"),"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}});
    format!("data: {created}\n\ndata: {item}\n\ndata: {event}\n\ndata: [DONE]\n\n")
}
fn compacted() -> String {
    let item = json!({"type":"response.output_item.done","output_index":0,"item":{
        "id":"cmp_http-exact","type":"compaction","encrypted_content":"opaque-http-summary"}});
    let done = json!({"type":"response.completed","response":{"id":"resp-summary-exact","status":"completed","output":[],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}});
    let created = json!({"type":"response.created","response":{"id":"resp-summary-exact"}});
    format!("data: {created}\n\ndata: {item}\n\ndata: {done}\n\ndata: [DONE]\n\n")
}
fn assert_summary_tail(items: &Value, final_generation: usize) {
    let items = items.as_array().expect("provider history must be an array");
    let summary = items
        .iter()
        .position(|item| item["type"] == "compaction")
        .expect("installed summary missing");
    assert_eq!(
        items[summary],
        json!({"id":"cmp_http-exact","type":"compaction","encrypted_content":"opaque-http-summary"})
    );
    assert_eq!(
        &items[summary + 1..],
        &[
            json!({"id":"msg_resp-http-2","type":"message","role":"assistant","content":[{"type":"output_text","text":"TAIL-ONE"}]}),
            json!({"id":format!("msg_resp-http-{final_generation}"),"type":"message","role":"assistant","content":[{"type":"output_text","text":"TAIL-FINAL"}]}),
        ],
        "summary must precede the exact complete foreground tail"
    );
}
async fn serve(provider: Provider, Json(body): Json<Value>) -> axum::response::Response {
    let summary = body["input"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["type"] == "compaction_trigger")
    });
    provider.requests.lock().unwrap().push(body);
    if summary {
        let n = provider.summaries.fetch_add(1, Ordering::SeqCst) + 1;
        provider.arrived.send((true, n)).ok();
        provider.release_summary.notified().await;
        return ([("content-type", "text/event-stream")], compacted()).into_response();
    }
    let n = provider.generations.fetch_add(1, Ordering::SeqCst) + 1;
    provider.arrived.send((false, n)).ok();
    let (text, end, tokens) = match n {
        1 => ("SEED-CUTOFF", false, 75_000),
        2 => (
            "TAIL-ONE",
            false,
            if provider.hard { 92_000 } else { 80_000 },
        ),
        _ => {
            provider.release_final.notified().await;
            ("TAIL-FINAL", true, 100)
        }
    };
    (
        [("content-type", "text/event-stream")],
        completed(&format!("resp-http-{n}"), text, end, tokens),
    )
        .into_response()
}
async fn fixture(
    hard: bool,
) -> eyre::Result<(
    Provider,
    mpsc::UnboundedReceiver<(bool, usize)>,
    String,
    tokio::task::JoinHandle<()>,
)> {
    let (arrived, receiver) = mpsc::unbounded_channel();
    let provider = Provider {
        requests: Default::default(),
        generations: Default::default(),
        summaries: Default::default(),
        arrived,
        release_summary: Default::default(),
        release_final: Default::default(),
        hard,
    };
    let copy = provider.clone();
    let router = Router::new().route("/responses", post(move |body| serve(copy.clone(), body)));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Ok((provider, receiver, url, server))
}
fn openai(url: &str) -> eyre::Result<OpenAi> {
    Ok(OpenAi::builder("synthetic-key")
        .transport(ResponsesTransport::Https)
        .store(true)
        .api_base_url(url)
        .build()?)
}
async fn arrivals(receiver: &mut mpsc::UnboundedReceiver<(bool, usize)>, wanted: &[(bool, usize)]) {
    let mut seen = Vec::new();
    timeout(Duration::from_secs(10), async {
        while !wanted.iter().all(|event| seen.contains(event)) {
            let event = receiver.recv().await.unwrap();
            println!("HTTP arrival {event:?}");
            seen.push(event);
        }
    })
    .await
    .expect("HTTP provider requests did not overlap");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_background_overlaps_and_preserves_immutable_cutoff_and_tail() -> eyre::Result<()> {
    let (provider, mut receiver, url, server) = fixture(false).await?;
    let dir = tempfile::tempdir()?;
    let state = DurableSession::open(
        SqliteStore::open(dir.path().join("state.sqlite"))?,
        "http-overlap",
    )
    .await?;
    let (agent, events) = Nanocodex::builder(openai(&url)?)
        .instructions("Preserve the original constraint and complete the task.")
        .codex_home(dir.path())
        .workspace(dir.path())
        .context_window_tokens(100_000)
        .durability(state.clone())
        .await?
        .build()?;
    let request = || PromptRequest::new("compact while continuing").request_id("overlap");
    let turn = agent.prompt(request()).await?;
    arrivals(
        &mut receiver,
        &[(false, 1), (true, 1), (false, 2), (false, 3)],
    )
    .await;
    let requests = provider.requests.lock().unwrap().clone();
    let summary = requests
        .iter()
        .find(|body| body.to_string().contains("compaction_trigger"))
        .unwrap();
    assert!(summary.to_string().contains("SEED-CUTOFF"));
    assert!(!summary.to_string().contains("TAIL-ONE"));
    assert!(
        !requests
            .last()
            .unwrap()
            .to_string()
            .contains("opaque-http-summary")
    );
    provider.release_summary.notify_one();
    provider.release_final.notify_one();
    assert_eq!(
        timeout(Duration::from_secs(10), turn.result())
            .await??
            .final_message(),
        "TAIL-FINAL"
    );
    let snapshot = serde_json::to_value(
        timeout(Duration::from_secs(10), agent.snapshot())
            .await
            .expect("committed model snapshot stalled")?,
    )?;
    assert_summary_tail(&snapshot["history"], 3);
    let history = snapshot["history"].to_string();
    assert!(history.contains("opaque-http-summary"), "{history}");
    assert!(history.contains("TAIL-ONE"), "{history}");
    assert!(history.contains("TAIL-FINAL"), "{history}");
    assert_eq!(provider.summaries.load(Ordering::SeqCst), 1);
    provider.release_final.notify_one();
    assert_eq!(
        timeout(Duration::from_secs(10), async {
            agent
                .prompt(
                    PromptRequest::new("continue from installed summary")
                        .request_id("continuation"),
                )
                .await?
                .result()
                .await
        })
        .await
        .expect("continuation request stalled")?
        .final_message(),
        "TAIL-FINAL"
    );
    let next = provider.requests.lock().unwrap().last().unwrap().clone();
    assert!(
        next.get("previous_response_id").is_none(),
        "installed summary must clear the old response chain: {next}"
    );
    let next_input = next["input"].as_array().unwrap();
    assert_eq!(next_input.last().unwrap()["role"], "user");
    assert_summary_tail(&json!(&next_input[..next_input.len() - 1]), 3);
    let before = provider.requests.lock().unwrap().len();
    timeout(Duration::from_secs(10), agent.shutdown())
        .await
        .expect("agent shutdown stalled")?;
    drop((agent, events));
    let reopened = DurableSession::open(
        SqliteStore::open(dir.path().join("state.sqlite"))?,
        "http-overlap",
    )
    .await?;
    let (agent, events) = Nanocodex::builder(openai(&url)?)
        .instructions("Preserve the original constraint and complete the task.")
        .codex_home(dir.path())
        .workspace(dir.path())
        .context_window_tokens(100_000)
        .durability(reopened)
        .await?
        .build()?;
    assert_eq!(
        timeout(Duration::from_secs(10), async {
            agent.prompt(request()).await?.result().await
        })
        .await
        .expect("reopened terminal replay stalled")?
        .final_message(),
        "TAIL-FINAL"
    );
    assert_eq!(provider.requests.lock().unwrap().len(), before);
    println!(
        "overlap=true immutable_cutoff=true complete_tail=true SQLite_reopen_terminal_replay=true next_turn_full_replay=true requests={before}"
    );
    timeout(Duration::from_secs(10), agent.shutdown())
        .await
        .expect("agent shutdown stalled")?;
    drop((agent, events));
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_hard_limit_waits_for_owned_summary_before_next_generation() -> eyre::Result<()> {
    let (provider, mut receiver, url, server) = fixture(true).await?;
    let dir = tempfile::tempdir()?;
    let state = DurableSession::open(
        SqliteStore::open(dir.path().join("state.sqlite"))?,
        "http-hard",
    )
    .await?;
    let (agent, events) = Nanocodex::builder(openai(&url)?)
        .instructions("Preserve the original constraint and complete the task.")
        .codex_home(dir.path())
        .workspace(dir.path())
        .context_window_tokens(100_000)
        .durability(state)
        .await?
        .build()?;
    let turn = agent.prompt("wait at the hard context boundary").await?;
    arrivals(&mut receiver, &[(false, 1), (true, 1), (false, 2)]).await;
    assert!(
        timeout(Duration::from_millis(200), receiver.recv())
            .await
            .is_err(),
        "generation crossed hard limit before summary settled"
    );
    provider.release_summary.notify_one();
    arrivals(&mut receiver, &[(false, 3)]).await;
    let third = provider
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .to_string();
    assert!(third.contains("opaque-http-summary"), "{third}");
    assert!(third.contains("TAIL-ONE"), "{third}");
    provider.release_final.notify_one();
    assert_eq!(
        timeout(Duration::from_secs(10), turn.result())
            .await??
            .final_message(),
        "TAIL-FINAL"
    );
    println!("hard_limit_wait=true next_generation_has_summary_and_complete_tail=true");
    timeout(Duration::from_secs(10), agent.shutdown())
        .await
        .expect("agent shutdown stalled")?;
    drop((agent, events));
    server.abort();
    Ok(())
}

struct LostAckStore {
    inner: SqliteStore,
    armed: Arc<std::sync::atomic::AtomicBool>,
}
impl nanocodex_durability::StateStore for LostAckStore {
    fn read_record<'a>(
        &'a mut self,
        id: &'a str,
        key: &'a str,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        Result<Option<String>, nanocodex_durability::StoreError>,
    > {
        self.inner.read_record(id, key)
    }
    fn acquire<'a>(
        &'a mut self,
        id: &'a str,
        owner: nanocodex_durability::OwnerId,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        Result<nanocodex_durability::OwnedState, nanocodex_durability::StoreError>,
    > {
        self.inner.acquire(id, owner)
    }
    fn replace<'a>(
        &'a mut self,
        id: &'a str,
        owner: &'a nanocodex_durability::OwnerToken,
        revision: u64,
        payload: &'a str,
        records: &'a [nanocodex_durability::StoreRecord],
    ) -> nanocodex_durability::StoreFuture<'a, Result<u64, nanocodex_durability::StoreError>> {
        Box::pin(async move {
            let result = self
                .inner
                .replace(id, owner, revision, payload, records)
                .await?;
            if self.armed.swap(false, Ordering::SeqCst) {
                return Err(nanocodex_durability::StoreError::Backend(
                    "lost foreground receipt acknowledgement after SQLite commit".into(),
                ));
            }
            Ok(result)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_completed_summary_and_foreground_receipt_recover_without_redispatch()
-> eyre::Result<()> {
    use nanocodex_durability::StepStatus;
    let (provider, mut receiver, url, server) = fixture(false).await?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("state.sqlite");
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let state = DurableSession::open(
        LostAckStore {
            inner: SqliteStore::open(&path)?,
            armed: armed.clone(),
        },
        "http-recovery",
    )
    .await?;
    let (agent, events) = Nanocodex::builder(openai(&url)?)
        .instructions("Preserve the original constraint and complete the task.")
        .codex_home(dir.path())
        .workspace(dir.path())
        .context_window_tokens(100_000)
        .durability(state.clone())
        .await?
        .build()?;
    let request =
        || PromptRequest::new("recover the summary and foreground exactly").request_id("recover");
    let turn = agent.prompt(request()).await?;
    arrivals(
        &mut receiver,
        &[(false, 1), (true, 1), (false, 2), (false, 3)],
    )
    .await;
    provider.release_summary.notify_one();
    timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = state.state().await?;
            if snapshot
                .operation("recover")
                .unwrap()
                .steps
                .iter()
                .any(|(id, step)| {
                    id.starts_with("background-compaction-")
                        && matches!(step.status, StepStatus::Completed(_))
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        Ok::<(), nanocodex_durability::Error>(())
    })
    .await??;
    armed.store(true, Ordering::SeqCst);
    provider.release_final.notify_one();
    assert!(
        timeout(Duration::from_secs(10), turn.result())
            .await?
            .is_err()
    );
    assert!(
        !armed.load(Ordering::SeqCst),
        "lost-ACK boundary was not reached"
    );
    // Read the actual SQLite state after process ownership is replaced below.
    let _ = timeout(Duration::from_secs(10), agent.shutdown())
        .await
        .expect("failed agent shutdown stalled");
    drop((agent, events, state));
    let state = DurableSession::open(SqliteStore::open(&path)?, "http-recovery").await?;
    let saved = state.state().await?;
    let steps = &saved.operation("recover").unwrap().steps;
    assert!(matches!(
        steps.get("model-3").unwrap().status,
        StepStatus::Completed(_)
    ));
    assert!(
        steps
            .iter()
            .any(|(id, step)| id.starts_with("background-compaction-")
                && matches!(step.status, StepStatus::Completed(_)))
    );
    let before = provider.requests.lock().unwrap().len();
    let (agent, events) = Nanocodex::builder(openai(&url)?)
        .instructions("Preserve the original constraint and complete the task.")
        .codex_home(dir.path())
        .workspace(dir.path())
        .context_window_tokens(100_000)
        .durability(state)
        .await?
        .build()?;
    let result = timeout(
        Duration::from_secs(10),
        agent.prompt(request()).await?.result(),
    )
    .await??;
    assert_eq!(result.final_message(), "TAIL-FINAL");
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        before,
        "completed receipts must not redispatch"
    );
    let snapshot = serde_json::to_value(
        timeout(Duration::from_secs(10), agent.snapshot())
            .await
            .expect("committed model snapshot stalled")?,
    )?;
    assert_summary_tail(&snapshot["history"], 3);
    let history = snapshot["history"].to_string();
    for exact in ["opaque-http-summary", "TAIL-ONE", "TAIL-FINAL"] {
        assert!(history.contains(exact), "{history}");
    }
    assert_eq!(provider.summaries.load(Ordering::SeqCst), 1);
    let replay = agent.prompt(request()).await?.result().await?;
    assert_eq!(replay.usage(), result.usage());
    assert_eq!(provider.requests.lock().unwrap().len(), before);
    println!(
        "completed_summary_and_model3_lost_ACK_replayed=true zero_additional_HTTP=true exact_tail=true"
    );
    timeout(Duration::from_secs(10), agent.shutdown())
        .await
        .expect("agent shutdown stalled")?;
    drop((agent, events));
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_owner_takeover_fences_old_turn_and_replays_completed_summary() -> eyre::Result<()> {
    use nanocodex_durability::StepStatus;
    let (provider, mut receiver, url, server) = fixture(false).await?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("state.sqlite");
    let state = DurableSession::open(SqliteStore::open(&path)?, "http-fence").await?;
    let (agent, events) = Nanocodex::builder(openai(&url)?)
        .instructions("Preserve the original constraint and complete the task.")
        .codex_home(dir.path())
        .workspace(dir.path())
        .context_window_tokens(100_000)
        .durability(state.clone())
        .await?
        .build()?;
    let request =
        || PromptRequest::new("retain cutoff across owner takeover").request_id("takeover");
    let turn = agent.prompt(request()).await?;
    arrivals(
        &mut receiver,
        &[(false, 1), (true, 1), (false, 2), (false, 3)],
    )
    .await;
    provider.release_summary.notify_one();
    timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = state.state().await?;
            if snapshot
                .operation("takeover")
                .unwrap()
                .steps
                .iter()
                .any(|(id, step)| {
                    id.starts_with("background-compaction-")
                        && matches!(step.status, StepStatus::Completed(_))
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        Ok::<(), nanocodex_durability::Error>(())
    })
    .await??;
    let new_owner = DurableSession::open(SqliteStore::open(&path)?, "http-fence").await?;
    let authoritative = new_owner.state().await?.operations().clone();
    provider.release_final.notify_one();
    let error = timeout(Duration::from_secs(10), turn.result())
        .await?
        .unwrap_err();
    assert!(error.to_string().contains("fenc"), "{error}");
    let _ = timeout(Duration::from_secs(10), agent.shutdown())
        .await
        .expect("failed agent shutdown stalled");
    drop((agent, events, state));
    drop(new_owner);
    let new_owner = DurableSession::open(SqliteStore::open(&path)?, "http-fence").await?;
    assert_eq!(
        new_owner.state().await?.operations(),
        &authoritative,
        "old owner must not publish a summary or foreground receipt"
    );
    let (agent, events) = Nanocodex::builder(openai(&url)?)
        .instructions("Preserve the original constraint and complete the task.")
        .codex_home(dir.path())
        .workspace(dir.path())
        .context_window_tokens(100_000)
        .durability(new_owner)
        .await?
        .build()?;
    let recovered = agent.prompt(request()).await?;
    arrivals(&mut receiver, &[(false, 4)]).await;
    provider.release_final.notify_one();
    assert_eq!(
        timeout(Duration::from_secs(10), recovered.result())
            .await??
            .final_message(),
        "TAIL-FINAL"
    );
    assert_eq!(
        provider.summaries.load(Ordering::SeqCst),
        1,
        "completed summary must replay under the new owner"
    );
    let snapshot = serde_json::to_value(
        timeout(Duration::from_secs(10), agent.snapshot())
            .await
            .expect("recovered model snapshot stalled")?,
    )?;
    assert_summary_tail(&snapshot["history"], 4);
    let history = snapshot["history"].to_string();
    for exact in ["opaque-http-summary", "TAIL-ONE", "TAIL-FINAL"] {
        assert!(history.contains(exact), "{history}");
    }
    println!(
        "owner_takeover_rejects_old_HTTP_publication=true authoritative_state_unchanged=true new_owner_replays_summary=true complete_tail=true"
    );
    timeout(Duration::from_secs(10), agent.shutdown())
        .await
        .expect("agent shutdown stalled")?;
    drop((agent, events));
    server.abort();
    Ok(())
}
