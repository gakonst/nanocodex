//! The Managed2 text-only terminal uses the shared presentation, but never the
//! legacy managed client's control, settings, voice, or local-tool drivers.

use super::*;
use crate::managed2::{Client, WatchEvent};
use nanocodex::agent::events::AgentEvent;
use tokio::sync::mpsc;
use uuid::Uuid;

enum DriverEvent {
    Admitted { agent: String, turn: String },
    Watch { turn: String, event: WatchEvent },
    Failed { turn: Option<String>, error: String },
    Ended,
}

/// Present serialized text turns through the ordinary alternate-screen shell.
pub(crate) async fn run_managed2(agent: Option<String>) -> Result<(), ManagedError> {
    let client = Client::from_environment()?;
    let mut agent = agent
        .map(|id| {
            Uuid::parse_str(&id).map(|id| id.to_string()).map_err(|_| {
                ManagedError::Configuration("Managed2 attach requires an agent UUID".into())
            })
        })
        .transpose()?;
    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut root = RootNode::new(&workspace, ReasoningEffort::Low);
    root.set_managed2_preview();
    root.set_model(Model::Sol);
    let mut app = AppNode::new(Theme::default(), workspace, root);
    let mut terminal = TerminalSession::enter().await.map_err(terminal_error)?;
    let mut input = EventStream::new();
    let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, Instant::now());
    terminal
        .draw(|frame| app.render(frame))
        .map_err(terminal_error)?;
    scheduler.presented(Instant::now());

    let (tx, mut updates) = mpsc::unbounded_channel::<DriverEvent>();
    let mut busy = false;
    let mut active_turn: Option<String> = None;
    let mut sequence = 1_u64;
    let mut stopping = false;
    let mut animation = tokio::time::interval(Duration::from_millis(100));
    while !stopping {
        tokio::select! {
            _ = animation.tick(), if busy => {
                request_render(app.update(AppEvent::AnimationFrame(Instant::now())), &mut scheduler);
            }
            event = input.next() => {
                match event {
                    Some(Ok(event)) => {
                        let update = app.update(AppEvent::Terminal(event));
                        stopping = apply_managed2_update(update, &mut app, &mut terminal,
                            &mut scheduler, &client, &agent, &tx, &mut busy, &mut sequence).await?;
                    }
                    Some(Err(error)) => return Err(terminal_error(error)),
                    None => break,
                }
            }
            Some(event) = updates.recv() => {
                let update = match event {
                    DriverEvent::Admitted { agent: id, turn } => {
                        agent = Some(id);
                        active_turn = Some(turn);
                        None
                    }
                    DriverEvent::Watch { turn, event } if active_turn.as_deref() == Some(&turn) => {
                        match event {
                            WatchEvent::AgentEvent(value) => {
                                // Only render events that match the admitted turn. Stream
                                // reconnects can repeat deltas: the transport emits both an
                                // AgentEvent and a Delta for each assistant.delta frame.
                                match serde_json::from_value::<AgentEvent>(value) {
                                    Ok(event) => {
                                        let record = TranscriptRecord::from_agent(sequence, unix_ms(), event)
                                            .with_managed_turn_id(Some(&turn));
                                        sequence = sequence.saturating_add(1);
                                        Some(app.update(AppEvent::Transcript { pane: PaneId::Main, record: Arc::new(record) }))
                                    }
                                    Err(_) => Some(app.update(AppEvent::NotifyError { pane: PaneId::Main,
                                        error: "Invalid Managed2 agent event".into() })),
                                }
                            }
                            WatchEvent::Delta(_) => None, // already rendered by AgentEvent
                            WatchEvent::Completed(text) => {
                                let record = TranscriptRecord::from_local(sequence, unix_ms(),
                                    LocalEvent::ManagedFinalMessage { turn_id: turn, text })
                                    .map_err(|error| ManagedError::Configuration(error.to_string()))?;
                                sequence = sequence.saturating_add(1);
                                Some(app.update(AppEvent::Transcript { pane: PaneId::Main, record: Arc::new(record) }))
                            }
                            WatchEvent::Failed(message) => {
                                let record = TranscriptRecord::from_local(sequence, unix_ms(),
                                    LocalEvent::ManagedTurnStopped { turn_id: turn, error: Some(message) })
                                    .map_err(|error| ManagedError::Configuration(error.to_string()))?;
                                sequence = sequence.saturating_add(1);
                                Some(app.update(AppEvent::Transcript { pane: PaneId::Main, record: Arc::new(record) }))
                            },
                        }
                    }
                    DriverEvent::Watch { .. } => None,
                    DriverEvent::Failed { turn, error } => {
                        let local = match turn {
                            Some(turn) => LocalEvent::ManagedTurnStopped { turn_id: turn, error: Some(error.clone()) },
                            None => LocalEvent::DisplayError { message: error.clone() },
                        };
                        let record = TranscriptRecord::from_local(sequence, unix_ms(), local)
                            .map_err(|error| ManagedError::Configuration(error.to_string()))?;
                        sequence = sequence.saturating_add(1);
                        request_render(app.update(AppEvent::Transcript { pane: PaneId::Main,
                            record: Arc::new(record) }), &mut scheduler);
                        Some(app.update(AppEvent::NotifyError { pane: PaneId::Main, error }))
                    }
                    DriverEvent::Ended => {
                        active_turn = None;
                        busy = false;
                        Some(app.update(AppEvent::WorkerTurnFinished { pane: PaneId::Main,
                            terminal_expected: false }))
                    }
                };
                if let Some(update) = update {
                    stopping = apply_managed2_update(update, &mut app, &mut terminal,
                        &mut scheduler, &client, &agent, &tx, &mut busy, &mut sequence).await?;
                }
            }
            _ = wait_until(scheduler.deadline()) => {
                if scheduler.is_due(Instant::now()) {
                    terminal.draw(|frame| app.render(frame)).map_err(terminal_error)?;
                    scheduler.presented(Instant::now());
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn apply_managed2_update(
    update: ComponentUpdate<AppEffect>,
    app: &mut AppNode,
    terminal: &mut TerminalSession,
    scheduler: &mut RenderScheduler,
    client: &Client,
    agent: &Option<String>,
    tx: &mpsc::UnboundedSender<DriverEvent>,
    busy: &mut bool,
    sequence: &mut u64,
) -> Result<bool, ManagedError> {
    let mut effects = VecDeque::from(update.effects);
    request_render_only(update.render, scheduler);
    let mut stopping = false;
    while let Some(effect) = effects.pop_front() {
        let update = match effect {
            AppEffect::Shutdown => {
                stopping = true;
                continue;
            }
            AppEffect::SetTheme(_) => {
                scheduler.request_immediate(Instant::now());
                continue;
            }
            AppEffect::Pane {
                pane,
                effect: RootEffect::ShowAgentId,
            } => app.update(AppEvent::ShowAgentId {
                pane,
                id: agent.clone().unwrap_or_else(|| "not created".into()),
            }),
            AppEffect::Pane {
                pane,
                effect: RootEffect::Submit(prompt),
            } => {
                if *busy {
                    app.update(AppEvent::NotifyError {
                        pane,
                        error: "Managed2 turn already active".into(),
                    })
                } else if prompt.has_images() {
                    // The text-only endpoint must never receive multimodal input.
                    let error = app.update(AppEvent::NotifyError {
                        pane,
                        error: "Managed2 accepts text only".into(),
                    });
                    effects.extend(
                        app.update(AppEvent::WorkerTurnFinished {
                            pane,
                            terminal_expected: false,
                        })
                        .effects,
                    );
                    request_render_only(error.render, scheduler);
                    continue;
                } else {
                    *busy = true;
                    let text = prompt.display_text().to_owned();
                    let record = TranscriptRecord::from_local(
                        *sequence,
                        unix_ms(),
                        LocalEvent::UserSubmitted {
                            id: TurnId::new(*sequence),
                            text: text.clone(),
                        },
                    )
                    .map_err(|error| ManagedError::Configuration(error.to_string()))?;
                    *sequence = sequence.saturating_add(1);
                    request_render(
                        app.update(AppEvent::Transcript {
                            pane,
                            record: Arc::new(record),
                        }),
                        scheduler,
                    );
                    let client = client.clone();
                    let agent = agent.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let request_id = Uuid::new_v4();
                        let (id, turn) = match client
                            .submit(agent.as_deref(), &text, request_id)
                            .await
                        {
                            Ok(receipt) => receipt,
                            Err(error) => {
                                let _ = tx.send(DriverEvent::Failed {
                                        turn: None,
                                        error: format!("Managed2 admission failed (request ID {request_id}; preserve it if delivery is uncertain): {error}"),
                                    });
                                let _ = tx.send(DriverEvent::Ended);
                                return;
                            }
                        };
                        let _ = tx.send(DriverEvent::Admitted {
                            agent: id.clone(),
                            turn: turn.clone(),
                        });
                        let (watch_tx, mut watch_rx) = mpsc::unbounded_channel();
                        let mut cursor = "0".to_owned();
                        let forward = async {
                            while let Some(event) = watch_rx.recv().await {
                                if tx
                                    .send(DriverEvent::Watch {
                                        turn: turn.clone(),
                                        event,
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        };
                        let (result, ()) = tokio::join!(
                            client.watch_turn(&id, &turn, &mut cursor, watch_tx),
                            forward
                        );
                        if let Err(error) = result {
                            let _ = tx.send(DriverEvent::Failed {
                                turn: Some(turn),
                                error: error.to_string(),
                            });
                        }
                        let _ = tx.send(DriverEvent::Ended);
                    });
                    continue;
                }
            }
            AppEffect::Pane { pane, .. } => app.update(AppEvent::NotifyError {
                pane,
                error: "This control is unavailable in Managed2 text mode".into(),
            }),
            _ => continue,
        };
        effects.extend(update.effects);
        request_render_only(update.render, scheduler);
    }
    // Leave terminal presentation on the regular demand-driven scheduler.
    let _ = terminal;
    Ok(stopping)
}
