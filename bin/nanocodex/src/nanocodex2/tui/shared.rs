//! Guest attachment uses only the public shared-thread transport. No account,
//! workspace tools, local prompt cache, or owner-only controls are initialized.

use super::*;
use nanocodex_managed::{SharePermission, SharedThreadClient};

const PAGE_SIZE: u16 = 100;
const INITIAL_PAGES: usize = 4;

struct HistoryBatch {
    events: Vec<ManagedEvent>,
    before: Option<String>,
}

enum Update {
    Event(ManagedEvent),
    Disconnected(String),
    Submitted {
        request_id: String,
        result: Result<(), String>,
    },
    History(Result<HistoryBatch, String>),
}

async fn load_history(
    client: &SharedThreadClient,
    mut before: Option<String>,
) -> Result<HistoryBatch, ManagedError> {
    let mut events = Vec::new();
    for _ in 0..INITIAL_PAGES {
        let page = client.history(before.as_deref(), PAGE_SIZE).await?;
        let mut older = page.data;
        older.append(&mut events);
        events = older;
        if !page.has_more {
            before = None;
            break;
        }
        let next = page.next_cursor.ok_or(ManagedError::InvalidResponse(
            "shared history is missing its next cursor",
        ))?;
        if before
            .as_ref()
            .is_some_and(|current| !cursor_before(&next, current))
        {
            return Err(ManagedError::InvalidResponse(
                "shared history cursor did not advance",
            ));
        }
        before = Some(next);
    }
    Ok(HistoryBatch { events, before })
}

fn cursor_before(left: &str, right: &str) -> bool {
    left.len() < right.len() || (left.len() == right.len() && left < right)
}

/// Keep bare share URLs usable without putting a token in shell history.
pub(crate) async fn run_shared(reference: &str) -> Result<(), ManagedError> {
    let mut url = url::Url::parse(reference)
        .map_err(|_| ManagedError::Configuration("invalid shared thread URL".into()))?;
    if url.fragment().is_some() {
        return run_shared_client(SharedThreadClient::from_url(reference)?).await;
    }
    // Validate the origin/path before requesting private input. This client is
    // never sent a request; the placeholder is only for format validation.
    url.set_fragment(Some(
        "token=nsl_0000000000000000000000000000000000000000000",
    ));
    SharedThreadClient::from_url(url.as_str())?;
    let Some(token) = hidden_share_token().await? else {
        return Ok(());
    };
    url.set_fragment(Some(&format!("token={}", token.as_str())));
    run_shared_client(SharedThreadClient::from_url(url.as_str())?).await
}

async fn hidden_share_token() -> Result<Option<zeroize::Zeroizing<String>>, ManagedError> {
    let mut terminal = TerminalSession::enter().await.map_err(terminal_error)?;
    let mut keys = EventStream::new();
    let mut token = zeroize::Zeroizing::new(String::new());
    loop {
        terminal
            .draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new(
                        "Paste shared token (hidden), then Enter. Esc cancels.",
                    ),
                    frame.area(),
                )
            })
            .map_err(terminal_error)?;
        match keys.next().await {
            Some(Ok(Event::Paste(value))) => {
                let value = zeroize::Zeroizing::new(value);
                token.push_str(value.trim());
            }
            Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                KeyCode::Enter => return Ok(Some(token)),
                KeyCode::Backspace => {
                    token.pop();
                }
                KeyCode::Char(c) => token.push(c),
                _ => {}
            },
            None | Some(Err(_)) => return Ok(None),
            _ => {}
        }
        if token.len() > 128 {
            return Err(ManagedError::Configuration("invalid shared token".into()));
        }
    }
}

async fn run_shared_client(client: SharedThreadClient) -> Result<(), ManagedError> {
    let metadata = client.metadata().await?;
    let writable = metadata.permission == SharePermission::Write;
    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let history = load_history(&client, None).await?;
    let mut before = history.before;
    // Older servers expose activity only through visible lifecycle events.
    let mut active = HashSet::new();
    for event in &history.events {
        observe_activity(&mut active, event);
    }
    let mut seen = history
        .events
        .iter()
        .map(|event| event.cursor.clone())
        .collect::<HashSet<_>>();
    let (mut records, mut sequence, _) =
        history_projection(history.events, client.agent_id(), &workspace)?;
    let effort = ReasoningEffort::Low;
    let mut root = RootNode::new(&workspace, effort);
    root.install_session_projection(
        &workspace,
        effort,
        ReasoningMode::Standard,
        ReasoningMode::Standard,
        false,
        RootNode::project_open_session(effort, records.clone()),
    );
    root.set_shared_thread(writable);
    let mut app = AppNode::new(Theme::default(), workspace.clone(), root);
    let mut terminal = TerminalSession::enter().await.map_err(terminal_error)?;
    let mut input = EventStream::new();
    let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, Instant::now());
    let (tx, mut updates) = mpsc::unbounded_channel();
    let mut tasks = JoinSet::new();
    let mut stream = client.events(EventCursor::parse(metadata.latest_event_cursor)?);
    let stream_tx = tx.clone();
    tasks.spawn(async move {
        loop {
            match stream.next().await {
                Ok(event) => {
                    if stream_tx.send(Update::Event(event)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = stream_tx.send(Update::Disconnected(error.to_string()));
                    break;
                }
            }
        }
    });
    request_render(
        app.update(AppEvent::NotifySuccess {
            pane: PaneId::Main,
            message: if writable {
                "Shared thread · write access"
            } else {
                "Shared thread · read only"
            }
            .into(),
        }),
        &mut scheduler,
    );
    request_render(
        app.update(AppEvent::ManagedActiveTurns {
            pane: PaneId::Main,
            count: active.len(),
        }),
        &mut scheduler,
    );
    terminal
        .draw(|frame| app.render(frame))
        .map_err(terminal_error)?;
    scheduler.presented(Instant::now());
    let mut connected = true;
    let mut submitting = false;
    let mut loading_history = false;
    let mut pending_submission: Option<(String, String)> = None;
    let mut confirmed_request: Option<String> = None;
    let mut effects = VecDeque::new();
    let mut animation = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = animation.tick(), if !active.is_empty() || submitting => {
                request_render(app.update(AppEvent::AnimationFrame(Instant::now())), &mut scheduler);
            }
            event = input.next() => match event {
                Some(Ok(event)) => {
                    let update = app.update(AppEvent::Terminal(event));
                    request_render_only(update.render, &mut scheduler);
                    effects.extend(update.effects);
                }
                Some(Err(error)) => return Err(terminal_error(error)),
                None => break,
            },
            Some(update) = updates.recv() => match update {
                Update::Event(event) => {
                    if !seen.insert(event.cursor.clone()) { continue; }
                    observe_activity(&mut active, &event);
                    if let ManagedEventData::TurnAccepted { id, .. } = &event.data
                        && pending_submission.as_ref().is_some_and(|(request, _)| request == id)
                    {
                        confirmed_request = Some(id.clone());
                        pending_submission = None;
                    }
                    if let Some((record, _)) = live_managed_projection(event, client.agent_id(), &workspace, &mut sequence)? {
                        records.push(record.clone());
                        request_render(app.update(AppEvent::ExternalTranscript { pane: PaneId::Main, record }), &mut scheduler);
                    }
                    request_render(app.update(AppEvent::ManagedActiveTurns { pane: PaneId::Main, count: active.len() }), &mut scheduler);
                }
                Update::Disconnected(error) => {
                    connected = false;
                    active.clear();
                    records.clear();
                    before = None;
                    request_render(app.update(AppEvent::HistoryReplayed { pane: PaneId::Main,
                        projection: Box::new(RootNode::project_open_session(effort, Vec::new())) }), &mut scheduler);
                    request_render(app.update(AppEvent::SharedAccess { writable: false }), &mut scheduler);
                    let record = Arc::new(TranscriptRecord::from_local(sequence, unix_ms(), LocalEvent::DisplayError {
                        message: format!("Shared thread unavailable: {error}")
                    }).map_err(|error| ManagedError::Configuration(error.to_string()))?);
                    sequence += 1;
                    records.push(record.clone());
                    request_render(app.update(AppEvent::Transcript { pane: PaneId::Main, record }), &mut scheduler);
                }
                Update::Submitted { request_id, result } => {
                    submitting = false;
                    request_render(app.update(AppEvent::WorkerTurnFinished { pane: PaneId::Main, terminal_expected: false }), &mut scheduler);
                    match result {
                        Ok(()) => {
                            if pending_submission.as_ref().is_some_and(|(id, _)| *id == request_id) {
                                pending_submission = None;
                            }
                        }
                        Err(error) if confirmed_request.as_deref() != Some(&request_id) => {
                            // Preserve both the draft and identity for an explicit retry.
                            if let Some((_, text)) = pending_submission.as_ref().filter(|(id, _)| *id == request_id) {
                                request_render(app.update(AppEvent::EditorDraft { pane: PaneId::Main, draft: text.clone() }), &mut scheduler);
                            }
                            request_render(app.update(AppEvent::NotifyError { pane: PaneId::Main,
                                error: format!("Message delivery not confirmed ({request_id}); retrying this draft will reuse its ID: {error}") }), &mut scheduler);
                        }
                        Err(_) => {} // The durable acceptance event already confirmed delivery.
                    }
                }
                Update::History(result) => {
                    loading_history = false;
                    if !connected { continue; }
                    match result {
                        Ok(batch) => {
                            before = batch.before;
                            let events = batch.events.into_iter().filter(|event| seen.insert(event.cursor.clone())).collect::<Vec<_>>();
                            let (mut older, _) = history_projection_with_sequences(&events, client.agent_id(), &workspace, &mut HashMap::new(), &mut sequence)?;
                            older.append(&mut records);
                            records = older;
                            request_render(app.update(AppEvent::HistoryReplayed { pane: PaneId::Main,
                                projection: Box::new(RootNode::project_open_session(effort, records.clone())) }), &mut scheduler);
                        }
                        Err(error) => request_render(app.update(AppEvent::NotifyError { pane: PaneId::Main, error }), &mut scheduler),
                    }
                }
            },
            completed = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(error)) = completed {
                    let _ = tx.send(Update::Disconnected(format!("Shared thread task stopped: {error}")));
                }
            }
            _ = wait_until(scheduler.deadline()) => {
                if scheduler.is_due(Instant::now()) {
                    terminal.draw(|frame| app.render(frame)).map_err(terminal_error)?;
                    scheduler.presented(Instant::now());
                }
            }
        }
        while let Some(effect) = effects.pop_front() {
            match effect {
                AppEffect::Shutdown
                | AppEffect::Pane {
                    effect: RootEffect::Shutdown,
                    ..
                } => return Ok(()),
                AppEffect::Pane {
                    pane,
                    effect: RootEffect::ShowAgentId,
                } => {
                    request_render(
                        app.update(AppEvent::ShowAgentId {
                            pane,
                            id: client.agent_id().to_owned(),
                        }),
                        &mut scheduler,
                    );
                }
                AppEffect::Pane {
                    effect: RootEffect::LoadOlderHistory,
                    ..
                } if !loading_history && before.is_some() && connected => {
                    loading_history = true;
                    let client = client.clone();
                    let before = before.clone();
                    let tx = tx.clone();
                    tasks.spawn(async move {
                        let result = load_history(&client, before)
                            .await
                            .map_err(|error| error.to_string());
                        let _ = tx.send(Update::History(result));
                    });
                }
                AppEffect::Pane {
                    pane,
                    effect: RootEffect::Submit(prompt),
                } => {
                    if !writable || !connected || submitting || prompt.has_images() {
                        request_render(
                            app.update(AppEvent::WorkerTurnFinished {
                                pane,
                                terminal_expected: false,
                            }),
                            &mut scheduler,
                        );
                        request_render(
                            app.update(AppEvent::NotifyError {
                                pane,
                                error: "This shared thread cannot accept this message.".into(),
                            }),
                            &mut scheduler,
                        );
                        continue;
                    }
                    submitting = true;
                    let client = client.clone();
                    let tx = tx.clone();
                    let text = prompt.display_text().to_owned();
                    let request_id = pending_submission
                        .as_ref()
                        .filter(|(_, input)| input == &text)
                        .map(|(id, _)| id.clone())
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    pending_submission = Some((request_id.clone(), text.clone()));
                    tasks.spawn(async move {
                        let result = client
                            .submit(&text, &request_id)
                            .await
                            .map(|_| ())
                            .map_err(|error| error.to_string());
                        let _ = tx.send(Update::Submitted { request_id, result });
                    });
                }
                _ => {} // Owner settings, tools, Hand attachment, cancellation and credentials are unavailable to guests.
            }
        }
    }
    Ok(())
}

fn observe_activity(active: &mut HashSet<String>, event: &ManagedEvent) {
    match &event.data {
        ManagedEventData::TurnAccepted { id, .. } => {
            active.insert(id.clone());
        }
        ManagedEventData::TurnCompleted { id, .. }
        | ManagedEventData::TurnFailed { id, .. }
        | ManagedEventData::TurnCancelled { id } => {
            active.remove(id);
        }
        _ => {}
    }
}
