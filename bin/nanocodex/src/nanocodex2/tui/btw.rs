//! Hosted checkpoint fork. The side pane submits only to the forked agent.

use super::{history, pane::PaneId, prompt::Submission, transcript::TranscriptRecord};
use nanocodex_managed::{EventCursor, ManagedClient, ManagedError, ManagedEventData};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::mpsc;

pub(super) enum Request {
    Submit(Submission),
    Cancel,
}

pub(super) enum Event {
    Ready {
        pane: PaneId,
        agent_id: String,
        settings: nanocodex_managed::AgentSettings,
    },
    Record {
        pane: PaneId,
        record: Arc<TranscriptRecord>,
    },
    Finished(PaneId),
    Failed {
        pane: PaneId,
        error: String,
        opening: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Exercise the actual managed fork: no copied transcript or prompt prefix,
    // and no creation, history read, or parent turn submission.
    #[tokio::test]
    async fn hosted_side_turn_is_isolated_and_completes() {
        use axum::{
            Json, Router,
            extract::Path as AxumPath,
            response::sse::{Event as SseEvent, Sse},
            routing::{get, post},
        };
        use serde_json::Value;
        use std::sync::Mutex;
        let submissions = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let signal = Arc::new(tokio::sync::Notify::new());
        let app = Router::new()
            .route("/v1/agents/parent/forks", post(|headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                assert!(!headers.get("idempotency-key").unwrap().is_empty());
                assert!(body.is_empty());
                Json(json!({"agent_id":"side","session_id":"side",
                    "parent_agent_id":"parent", "events_url":"http://localhost/v1/agents/side/events", "websocket_url":"ws://localhost/v1/agents/side/ws"}))
            }))
            .route("/v1/agents/{agent}/turns", post({
                let submissions = submissions.clone(); let signal = signal.clone();
                move |AxumPath(agent): AxumPath<String>, Json(body): Json<Value>| {
                    let submissions = submissions.clone(); let signal = signal.clone();
                    async move {
                        submissions.lock().unwrap().push((agent, body.clone()));
                        signal.notify_one();
                        Json(json!({"turn_id":body["id"],"state":"accepted","input":body["input"],
                            "accepted_cursor":"1","terminal_cursor":null,"created_at":1,
                            "accepted_at":1,"updated_at":1,"attempt_count":1,
                            "retry_at":null,"error":null,"terminal":null}))
                    }
                }
            }))
            .route("/v1/agents/side/events", get({
                let submissions = submissions.clone(); let signal = signal.clone();
                move || {
                    let submissions = submissions.clone(); let signal = signal.clone();
                    async move {
                        Sse::new(futures_util::stream::once(async move {
                            signal.notified().await;
                            let id = submissions.lock().unwrap()[0].1["id"].as_str().unwrap().to_owned();
                            let data = json!({"cursor":"1","type":"turn_completed","id":id,
                                "final_message":"SIDE_REPLY","citations":[]}).to_string();
                            Ok::<_, std::convert::Infallible>(SseEvent::default().id("1").event("turn_completed").data(data))
                        }))
                    }
                }
            }))
            .fallback(|uri: axum::http::Uri| async move {
                eprintln!("unexpected non-fork request: {}", uri);
                axum::http::StatusCode::NOT_FOUND
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let key = nanocodex_managed::ManagedApiKey::parse(format!(
            "ncx_live_{}_{}",
            "a".repeat(12),
            "b".repeat(43)
        ))
        .unwrap();
        let client = ManagedClient::new(origin, key).unwrap();
        let (commands, requests) = mpsc::unbounded_channel();
        let (events, mut updates) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(
            PaneId::Fork(1),
            client,
            "parent".into(),
            nanocodex_managed::AgentSettings::default(),
            PathBuf::from("/work"),
            1,
            requests,
            events,
        ));
        let ready = tokio::time::timeout(std::time::Duration::from_secs(5), updates.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ready, Event::Ready { agent_id, .. } if agent_id == "side"));
        commands
            .send(Request::Submit(Submission::text("side question".into())))
            .unwrap();
        let mut finished = false;
        let mut records = 0;
        for _ in 0..4 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), updates.recv())
                .await
                .unwrap()
                .unwrap();
            if let Event::Failed { error, .. } = &event {
                panic!("side turn failed: {error}");
            }
            if matches!(event, Event::Record { .. }) {
                records += 1;
            }
            if matches!(event, Event::Finished(_)) {
                finished = true;
                break;
            }
        }
        assert!(finished);
        assert_eq!(
            records, 2,
            "question and streamed final answer should each be visible"
        );
        let saved = submissions.lock().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].0, "side");
        assert_eq!(
            saved[0].1["input"],
            json!([{"type":"text", "text":"side question"}])
        );
        task.abort();
        server.abort();
    }
}

pub(super) async fn run(
    pane: PaneId,
    client: ManagedClient,
    parent_agent_id: String,
    settings: nanocodex_managed::AgentSettings,
    workspace: PathBuf,
    start_sequence: u64,
    mut requests: mpsc::UnboundedReceiver<Request>,
    events: mpsc::UnboundedSender<Event>,
) {
    if let Err(error) = run_inner(
        pane,
        client,
        parent_agent_id,
        settings,
        workspace,
        start_sequence,
        &mut requests,
        &events,
    )
    .await
    {
        let _ = events.send(Event::Failed {
            pane,
            error: error.to_string(),
            opening: false,
        });
        let _ = events.send(Event::Finished(pane));
    }
}

async fn run_inner(
    pane: PaneId,
    client: ManagedClient,
    parent_agent_id: String,
    settings: nanocodex_managed::AgentSettings,
    workspace: PathBuf,
    start_sequence: u64,
    requests: &mut mpsc::UnboundedReceiver<Request>,
    events: &mpsc::UnboundedSender<Event>,
) -> Result<(), ManagedError> {
    // A stable identity survives internal transport retries. The server decides
    // the committed boundary atomically; never reconstruct it from SSE/history.
    let request_id = uuid::Uuid::now_v7().to_string();
    let receipt = match client.fork(&parent_agent_id, &request_id).await {
        Ok(receipt) => receipt,
        Err(error) => {
            let message = if matches!(error, ManagedError::Transport(_)) {
                format!(
                    "Fork admission may be uncertain (key {request_id}): {error}. Check managed agents before retrying."
                )
            } else {
                error.to_string()
            };
            let _ = events.send(Event::Failed {
                pane,
                error: message,
                opening: true,
            });
            return Ok(());
        }
    };
    let settings = receipt
        .initial_state
        .as_ref()
        .map_or(settings, |state| state.settings);
    let agent_id = receipt.agent_id;
    let mut stream = client.events(&agent_id, EventCursor::parse("0")?)?;
    if events
        .send(Event::Ready {
            pane,
            agent_id: agent_id.clone(),
            settings,
        })
        .is_err()
    {
        return Ok(());
    }
    let mut active: Option<String> = None;
    // The pane initially shows the parent's transcript snapshot. New local IDs
    // must not reuse any sequence from that snapshot.
    let mut sequence = start_sequence.max(1);
    loop {
        tokio::select! {
            request = requests.recv() => match request {
                Some(Request::Submit(prompt)) => {
                    let display = prompt.display_text().to_owned();
                    let id = uuid::Uuid::now_v7().to_string();
                    let input = prompt.managed_prompt();
                    let record = TranscriptRecord::from_local(sequence, history::unix_ms(),
                        super::transcript::LocalEvent::UserSubmitted { id: super::transcript::TurnId::new(sequence), text: display })
                        .map_err(|error| ManagedError::Configuration(error.to_string()))?;
                    sequence += 1;
                    let _ = events.send(Event::Record { pane, record: Arc::new(record) });
                    match client.submit(&agent_id, Some(&id), &id, &input).await {
                        Ok(_) => active = Some(id),
                        Err(error) => {
                            let message = if matches!(error, ManagedError::Transport(_)) {
                                format!("Side-turn admission may be uncertain: {error}. Check /id and its durable history before retrying.")
                            } else { error.to_string() };
                            let _ = events.send(Event::Failed { pane, error: message, opening: false });
                            let _ = events.send(Event::Finished(pane));
                        }
                    }
                }
                Some(Request::Cancel) => {
                    if let Some(id) = active.as_deref() {
                        if let Err(error) = client.cancel(&agent_id, id).await {
                            let _ = events.send(Event::Failed { pane, error: error.to_string(), opening: false });
                        }
                    }
                }
                None => return Ok(()),
            },
            event = stream.next() => {
                let event = event?;
                // The local question is already displayed. Avoid a duplicate acceptance.
                if matches!(event.data, ManagedEventData::TurnAccepted { .. } | ManagedEventData::AgentCreated { .. }) { continue; }
                let terminal = matches!(&event.data,
                    ManagedEventData::TurnCompleted { id, .. } | ManagedEventData::TurnFailed { id, .. } | ManagedEventData::TurnCancelled { id }
                    if active.as_deref() == Some(id));
                if let Some((record, _)) = history::live_managed_projection(event, &agent_id, &workspace, &mut sequence)? {
                    let _ = events.send(Event::Record { pane, record });
                }
                if terminal {
                    active = None;
                    let _ = events.send(Event::Finished(pane));
                }
            }
        }
    }
}
