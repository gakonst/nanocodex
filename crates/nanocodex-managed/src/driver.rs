use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use nanocodex_agent::{
    AgentSessionContext, NanocodexError, Thinking, TurnResult, TurnUsage,
    backend::{
        BackendFuture, BackendPrompt, BackendPromptRoute, BackendTurn, BackendTurnKey,
        LifecycleBackend,
    },
    input::{ImageDetail, Prompt, PromptInput as AgentPromptInput, UserInput},
};
use nanocodex_oai_api::events::{AgentEvent, AgentEventPublisher};
use tokio::sync::{mpsc, watch};
use tower::Service;

use crate::{
    EventCursor, ManagedError, ManagedEvent, ManagedEventData, ManagedEvents, PromptContent,
    PromptInput, TurnState,
    builder::{ManagedRequest, ManagedResponse, backend_error, call, unexpected_response},
};

#[cfg(feature = "tools")]
use crate::attachment::AttachmentSupervisor;

const COMMAND_CAPACITY: usize = 32;

pub(crate) enum Command {
    Submit(
        BackendPrompt,
        tokio::sync::oneshot::Sender<nanocodex_agent::Result<TurnResult>>,
        tokio::sync::oneshot::Sender<nanocodex_agent::Result<String>>,
    ),
    Steer(
        BackendTurnKey,
        Prompt,
        tokio::sync::oneshot::Sender<nanocodex_agent::Result<()>>,
    ),
    Cancel(
        BackendTurnKey,
        tokio::sync::oneshot::Sender<nanocodex_agent::Result<()>>,
    ),
    SetThinking(
        Thinking,
        tokio::sync::oneshot::Sender<nanocodex_agent::Result<()>>,
    ),
    SetFastMode(
        bool,
        tokio::sync::oneshot::Sender<nanocodex_agent::Result<()>>,
    ),
    Disconnect,
    Shutdown,
}

#[derive(Clone)]
pub(crate) struct Shutdown {
    requested: Arc<AtomicBool>,
    outcome: watch::Sender<Option<Result<(), Arc<NanocodexError>>>>,
}

impl Shutdown {
    fn new() -> Self {
        let (outcome, _) = watch::channel(None);
        Self {
            requested: Arc::new(AtomicBool::new(false)),
            outcome,
        }
    }

    fn complete(&self, outcome: Result<(), NanocodexError>) {
        let mut outcome = Some(outcome.map_err(Arc::new));
        self.outcome.send_if_modified(|current| {
            if current.is_some() {
                false
            } else {
                *current = outcome.take();
                true
            }
        });
    }

    async fn wait(&self) -> nanocodex_agent::Result<()> {
        let mut outcome = self.outcome.subscribe();
        loop {
            if let Some(outcome) = outcome.borrow().clone() {
                return outcome.map_err(NanocodexError::Shutdown);
            }
            if outcome.changed().await.is_err() {
                return Err(NanocodexError::AgentStopped);
            }
        }
    }
}

/// Account-managed lifecycle implementation behind the common agent handle.
#[derive(Clone)]
pub struct ManagedAgent {
    commands: mpsc::Sender<Command>,
    shutdown: Shutdown,
}

impl std::fmt::Debug for ManagedAgent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedAgent")
            .finish_non_exhaustive()
    }
}

impl ManagedAgent {
    pub(crate) fn new() -> (Self, mpsc::Receiver<Command>, Shutdown) {
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let shutdown = Shutdown::new();
        (
            Self {
                commands,
                shutdown: shutdown.clone(),
            },
            receiver,
            shutdown,
        )
    }

    async fn request<T>(
        commands: mpsc::Sender<Command>,
        command: impl FnOnce(tokio::sync::oneshot::Sender<nanocodex_agent::Result<T>>) -> Command,
    ) -> nanocodex_agent::Result<T> {
        let (result, receiver) = tokio::sync::oneshot::channel();
        commands
            .send(command(result))
            .await
            .map_err(|_| NanocodexError::AgentStopped)?;
        receiver.await.map_err(|_| NanocodexError::AgentStopped)?
    }
}

impl LifecycleBackend for ManagedAgent {
    fn submit(&self, prompt: BackendPrompt) -> BackendFuture<nanocodex_agent::Result<BackendTurn>> {
        let commands = self.commands.clone();
        Box::pin(async move {
            let (turn_result, turn_receiver) = tokio::sync::oneshot::channel();
            let request_id = Self::request(commands, |result| {
                Command::Submit(prompt, turn_result, result)
            })
            .await?;
            let expected_request_id = request_id.clone();
            Ok(BackendTurn {
                request_id: Some(request_id),
                result: Box::pin(async move {
                    let result = turn_receiver
                        .await
                        .map_err(|_| NanocodexError::TurnStopped)?;
                    match result {
                        Ok(result) if result.request_id() != Some(expected_request_id.as_str()) => {
                            Err(NanocodexError::BackendContract {
                                detail: "managed turn result request ID differed from admission",
                            })
                        }
                        result => result,
                    }
                }),
            })
        })
    }

    fn route(
        &self,
        _prompt: BackendPrompt,
    ) -> BackendFuture<nanocodex_agent::Result<BackendPromptRoute>> {
        Box::pin(async {
            Err(NanocodexError::UnsupportedCapability {
                capability: "route_prompt",
            })
        })
    }

    fn steer(
        &self,
        key: BackendTurnKey,
        prompt: Prompt,
    ) -> BackendFuture<nanocodex_agent::Result<()>> {
        let commands = self.commands.clone();
        Box::pin(async move {
            Self::request(commands, |result| Command::Steer(key, prompt, result)).await
        })
    }

    fn cancel(&self, key: BackendTurnKey) -> BackendFuture<nanocodex_agent::Result<()>> {
        let commands = self.commands.clone();
        Box::pin(
            async move { Self::request(commands, |result| Command::Cancel(key, result)).await },
        )
    }

    fn set_thinking(&self, thinking: Thinking) -> BackendFuture<nanocodex_agent::Result<()>> {
        let commands = self.commands.clone();
        Box::pin(async move {
            Self::request(commands, |result| Command::SetThinking(thinking, result)).await
        })
    }

    fn set_fast_mode(&self, enabled: bool) -> BackendFuture<nanocodex_agent::Result<()>> {
        let commands = self.commands.clone();
        Box::pin(async move {
            Self::request(commands, |result| Command::SetFastMode(enabled, result)).await
        })
    }

    fn compact(&self) -> BackendFuture<nanocodex_agent::Result<()>> {
        unsupported("compact")
    }

    fn append_developer_message(
        &self,
        _text: String,
    ) -> BackendFuture<nanocodex_agent::Result<AgentSessionContext>> {
        unsupported("append_developer_message")
    }

    fn context(&self) -> BackendFuture<nanocodex_agent::Result<AgentSessionContext>> {
        unsupported("context")
    }

    fn spawn(
        &self,
        _options: nanocodex_agent::SpawnOptions,
    ) -> BackendFuture<
        nanocodex_agent::Result<(nanocodex_agent::Nanocodex, nanocodex_agent::AgentEvents)>,
    > {
        unsupported("spawn")
    }

    fn fork(
        &self,
        _completed: Option<TurnResult>,
    ) -> BackendFuture<
        nanocodex_agent::Result<(nanocodex_agent::Nanocodex, nanocodex_agent::AgentEvents)>,
    > {
        unsupported("fork")
    }

    fn flush(&self) -> BackendFuture<nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn disconnect(&self) -> BackendFuture<nanocodex_agent::Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            if shutdown
                .requested
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
                && commands.send(Command::Disconnect).await.is_err()
            {
                shutdown.complete(Err(NanocodexError::AgentStopped));
            }
            shutdown.wait().await
        })
    }

    fn shutdown(&self) -> BackendFuture<nanocodex_agent::Result<()>> {
        let commands = self.commands.clone();
        let shutdown = self.shutdown.clone();
        Box::pin(async move {
            if shutdown
                .requested
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
                && commands.send(Command::Shutdown).await.is_err()
            {
                shutdown.complete(Err(NanocodexError::AgentStopped));
            }
            shutdown.wait().await
        })
    }
}

struct PendingTurn {
    key: BackendTurnKey,
    request_id: String,
    events: AgentEventPublisher,
    completion: tokio::sync::oneshot::Sender<nanocodex_agent::Result<TurnResult>>,
}

pub(crate) struct ManagedDriver<S> {
    service: S,
    agent_id: String,
    stream: ManagedEvents,
    commands: mpsc::Receiver<Command>,
    events: AgentEventPublisher,
    shutdown: Shutdown,
    event_observer: Option<mpsc::UnboundedSender<ManagedEvent>>,
    pending: HashMap<String, PendingTurn>,
    turns_by_key: HashMap<BackendTurnKey, String>,
    next_event_seq: u64,
    #[cfg(feature = "tools")]
    attachment: Option<AttachmentSupervisor>,
}

enum DriverInput {
    Command(Option<Command>),
    Event(Result<ManagedEvent, ManagedError>),
}

impl<S> ManagedDriver<S>
where
    S: Service<ManagedRequest, Response = ManagedResponse> + Send + 'static,
    S::Future: Send + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        service: S,
        agent_id: String,
        stream: ManagedEvents,
        commands: mpsc::Receiver<Command>,
        events: AgentEventPublisher,
        shutdown: Shutdown,
        event_observer: Option<mpsc::UnboundedSender<ManagedEvent>>,
        #[cfg(feature = "tools")] attachment: Option<AttachmentSupervisor>,
    ) -> Self {
        Self {
            service,
            agent_id,
            stream,
            commands,
            events,
            shutdown,
            event_observer,
            pending: HashMap::new(),
            turns_by_key: HashMap::new(),
            next_event_seq: 1,
            #[cfg(feature = "tools")]
            attachment,
        }
    }

    pub(crate) async fn run(mut self) {
        let outcome = loop {
            match self.next().await {
                DriverInput::Command(command) => match command {
                    Some(Command::Submit(prompt, completion, result)) => {
                        drop(result.send(self.submit(prompt, completion).await));
                    }
                    Some(Command::Steer(key, prompt, result)) => {
                        drop(result.send(self.steer(key, prompt).await));
                    }
                    Some(Command::Cancel(key, result)) => {
                        drop(result.send(self.cancel(key).await));
                    }
                    Some(Command::SetThinking(thinking, result)) => {
                        drop(result.send(self.set_thinking(thinking).await));
                    }
                    Some(Command::SetFastMode(enabled, result)) => {
                        drop(result.send(self.set_fast_mode(enabled).await));
                    }
                    Some(Command::Shutdown) => break self.shutdown_active().await,
                    Some(Command::Disconnect) | None => break Ok(()),
                },
                DriverInput::Event(event) => match event {
                    Ok(event) => {
                        if let Err(error) = self.event(event) {
                            break Err(error);
                        }
                    }
                    Err(error) => break Err(backend_error(error)),
                },
            }
        };

        #[cfg(feature = "tools")]
        if let Some(attachment) = self.attachment.take() {
            // Local placement is optional. A fenced or unavailable attachment
            // must not turn an already-durable cloud result into a failed
            // lifecycle shutdown.
            drop(attachment.shutdown().await);
        }
        self.shutdown.complete(outcome);
    }

    async fn next(&mut self) -> DriverInput {
        #[cfg(feature = "tools")]
        {
            tokio::select! {
                command = self.commands.recv() => DriverInput::Command(command),
                event = self.stream.next() => DriverInput::Event(event),
            }
        }
        #[cfg(not(feature = "tools"))]
        {
            tokio::select! {
                command = self.commands.recv() => DriverInput::Command(command),
                event = self.stream.next() => DriverInput::Event(event),
            }
        }
    }

    async fn submit(
        &mut self,
        prompt: BackendPrompt,
        completion: tokio::sync::oneshot::Sender<nanocodex_agent::Result<TurnResult>>,
    ) -> nanocodex_agent::Result<String> {
        let cancel_on_admission = prompt.cancel_on_admission;
        let input = managed_prompt(prompt.prompt)?;
        let request_id = prompt
            .request_id
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        if cancel_on_admission {
            self.cancel_turn(request_id.clone()).await?;
        }
        let turn = match call(
            &mut self.service,
            ManagedRequest::Submit {
                agent_id: self.agent_id.clone(),
                turn_id: Some(request_id.clone()),
                idempotency_key: request_id.clone(),
                input,
            },
        )
        .await?
        {
            ManagedResponse::Submitted(turn) => turn,
            _ => return Err(unexpected_response()),
        };
        let terminal = matches!(
            turn.state,
            TurnState::Completed | TurnState::Cancelled | TurnState::Failed
        ) || turn.terminal.is_some();
        let turn_id = turn.turn_id.clone();
        if turn
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.turn_id() != Some(&turn_id))
        {
            return Err(NanocodexError::BackendContract {
                detail: "managed turn view terminal identified a different turn",
            });
        }
        let retained = if terminal {
            let terminal_cursor = turn.terminal_cursor.as_deref().ok_or_else(|| {
                backend_error(ManagedError::InvalidResponse(
                    "terminal managed turn view omitted its terminal cursor",
                ))
            })?;
            cursor_at_or_before(terminal_cursor, self.stream.cursor().as_str())?
        } else {
            false
        };
        if retained {
            let outcome = retained_result(turn.terminal, &turn_id, &request_id)?;
            drop(completion.send(outcome));
            return Ok(request_id);
        }
        if self.pending.contains_key(&turn_id) {
            return Err(NanocodexError::BackendContract {
                detail: "managed service returned a duplicate active turn id",
            });
        }
        self.turns_by_key.insert(prompt.key, turn_id.clone());
        self.pending.insert(
            turn_id,
            PendingTurn {
                key: prompt.key,
                request_id: request_id.clone(),
                events: prompt.events,
                completion,
            },
        );
        Ok(request_id)
    }

    async fn steer(&mut self, key: BackendTurnKey, prompt: Prompt) -> nanocodex_agent::Result<()> {
        let turn_id = self
            .turns_by_key
            .get(&key)
            .cloned()
            .ok_or(NanocodexError::TurnNotSteerable)?;
        let input = managed_prompt(prompt)?;
        match call(
            &mut self.service,
            ManagedRequest::Steer {
                agent_id: self.agent_id.clone(),
                turn_id: turn_id.clone(),
                input,
            },
        )
        .await?
        {
            ManagedResponse::Steered(action) if action.turn_id == turn_id => Ok(()),
            ManagedResponse::Steered(_) => Err(NanocodexError::BackendContract {
                detail: "managed steer acknowledged a different turn",
            }),
            _ => Err(unexpected_response()),
        }
    }

    async fn cancel(&mut self, key: BackendTurnKey) -> nanocodex_agent::Result<()> {
        let turn_id = self
            .turns_by_key
            .get(&key)
            .cloned()
            .ok_or(NanocodexError::TurnNotCancellable)?;
        self.cancel_turn(turn_id).await
    }

    async fn set_thinking(&mut self, thinking: Thinking) -> nanocodex_agent::Result<()> {
        match call(
            &mut self.service,
            ManagedRequest::SetThinking {
                agent_id: self.agent_id.clone(),
                thinking,
            },
        )
        .await?
        {
            ManagedResponse::Settings(settings) if settings.thinking == thinking => Ok(()),
            ManagedResponse::Settings(_) => Err(NanocodexError::BackendContract {
                detail: "managed thinking update acknowledged a different setting",
            }),
            _ => Err(unexpected_response()),
        }
    }

    async fn set_fast_mode(&mut self, enabled: bool) -> nanocodex_agent::Result<()> {
        match call(
            &mut self.service,
            ManagedRequest::SetFastMode {
                agent_id: self.agent_id.clone(),
                enabled,
            },
        )
        .await?
        {
            ManagedResponse::Settings(settings) if settings.fast_mode == enabled => Ok(()),
            ManagedResponse::Settings(_) => Err(NanocodexError::BackendContract {
                detail: "managed fast-mode update acknowledged a different setting",
            }),
            _ => Err(unexpected_response()),
        }
    }

    async fn shutdown_active(&mut self) -> nanocodex_agent::Result<()> {
        let mut first_error = None;
        let turn_ids = self.pending.keys().cloned().collect::<Vec<_>>();
        for turn_id in turn_ids {
            if let Err(error) = self.cancel_turn(turn_id).await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        while !self.pending.is_empty() {
            let event = self.stream.next().await.map_err(backend_error)?;
            self.event(event)?;
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn cancel_turn(&mut self, turn_id: String) -> nanocodex_agent::Result<()> {
        match call(
            &mut self.service,
            ManagedRequest::Cancel {
                agent_id: self.agent_id.clone(),
                turn_id: turn_id.clone(),
            },
        )
        .await?
        {
            ManagedResponse::Cancelled(action) if action.turn_id == turn_id => Ok(()),
            ManagedResponse::Cancelled(_) => Err(NanocodexError::BackendContract {
                detail: "managed cancel acknowledged a different turn",
            }),
            _ => Err(unexpected_response()),
        }
    }

    fn event(&mut self, event: ManagedEvent) -> nanocodex_agent::Result<()> {
        if let Some(body_turn_id) = event.data.turn_id()
            && event.turn_id.as_deref() != Some(body_turn_id)
        {
            return Err(NanocodexError::BackendContract {
                detail: "managed event envelope and body identified different turns",
            });
        }
        if let Some(observer) = &self.event_observer {
            drop(observer.send(event.clone()));
        }
        if let Some(nested) = event.data.agent_event().map_err(backend_error)? {
            let child_terminal = nested.kind.is_terminal()
                && matches!(
                    event.data,
                    ManagedEventData::Event {
                        agent_id: Some(_),
                        ..
                    }
                );
            let pending = event
                .turn_id
                .as_deref()
                .and_then(|turn_id| self.pending.get(turn_id));
            if let Some(pending) = pending {
                let publisher = if child_terminal {
                    self.events.clone()
                } else {
                    pending.events.clone()
                };
                self.publish_nested(&publisher, nested)?;
            } else {
                let publisher = self.events.clone();
                self.publish_nested(&publisher, nested)?;
            }
        }

        match event.data {
            ManagedEventData::TurnCompleted {
                id,
                final_message,
                usage,
                ..
            } => {
                self.complete(&id, |pending| {
                    let usage = usage
                        .map(serde_json::from_value::<TurnUsage>)
                        .transpose()
                        .map_err(|_| {
                            backend_error(ManagedError::InvalidEvent(
                                "managed turn usage is not an exact TurnUsage".to_owned(),
                            ))
                        })?;
                    Ok(TurnResult::from_backend(
                        Some(pending.request_id.clone()),
                        final_message,
                        usage,
                    ))
                });
            }
            ManagedEventData::TurnCancelled { id } => {
                self.complete(&id, |_| Err(NanocodexError::TurnCancelled));
            }
            ManagedEventData::TurnFailed { id, error } => {
                let outcome = turn_error(id.clone(), "failed", error);
                self.complete(&id, |_| Err(outcome));
            }
            ManagedEventData::StreamFailed { error } => {
                return Err(backend_error(ManagedError::InvalidEvent(format!(
                    "managed stream failed: {error}"
                ))));
            }
            ManagedEventData::Event { .. }
            | ManagedEventData::AgentCreated { .. }
            | ManagedEventData::TurnAccepted { .. }
            | ManagedEventData::TurnCancelling { .. }
            | ManagedEventData::TurnRetryable { .. } => {}
        }
        Ok(())
    }

    fn publish_nested(
        &mut self,
        publisher: &AgentEventPublisher,
        mut event: AgentEvent,
    ) -> nanocodex_agent::Result<()> {
        event.seq = self.next_event_seq;
        self.next_event_seq = self.next_event_seq.checked_add(1).ok_or_else(|| {
            backend_error(ManagedError::InvalidEvent(
                "managed agent event sequence exhausted".to_owned(),
            ))
        })?;
        event.request_id = Arc::from(self.events.request_id());
        publisher.publish(event).map_err(backend_error)
    }

    fn complete(
        &mut self,
        turn_id: &str,
        result: impl FnOnce(&PendingTurn) -> nanocodex_agent::Result<TurnResult>,
    ) {
        if let Some(pending) = self.pending.remove(turn_id) {
            self.turns_by_key.remove(&pending.key);
            let outcome = if pending.events.turn_is_terminal() {
                result(&pending)
            } else {
                Err(NanocodexError::BackendContract {
                    detail: "managed turn result completed before its terminal event",
                })
            };
            drop(pending.completion.send(outcome));
        }
    }
}

fn unsupported<T>(capability: &'static str) -> BackendFuture<nanocodex_agent::Result<T>> {
    Box::pin(async move { Err(NanocodexError::UnsupportedCapability { capability }) })
}

fn managed_prompt(prompt: Prompt) -> nanocodex_agent::Result<PromptInput> {
    if !prompt.transcript().is_empty() {
        return Err(NanocodexError::UnsupportedCapability {
            capability: "prompt_transcript",
        });
    }
    match prompt.instruction {
        AgentPromptInput::Text(text) => Ok(PromptInput::Text(text)),
        AgentPromptInput::Content(items) => items
            .into_iter()
            .map(|item| match item {
                UserInput::Text { text } => Ok(PromptContent::Text { text }),
                UserInput::Image { image_url, detail } => Ok(PromptContent::Image {
                    image_url,
                    detail: detail.map(image_detail),
                }),
                UserInput::Audio { audio_url } => Ok(PromptContent::Audio { audio_url }),
                UserInput::LocalImage { .. } | UserInput::LocalAudio { .. } => {
                    Err(NanocodexError::UnsupportedCapability {
                        capability: "local_media",
                    })
                }
            })
            .collect::<nanocodex_agent::Result<Vec<_>>>()
            .map(PromptInput::Content),
    }
}

fn retained_result(
    terminal: Option<ManagedEventData>,
    turn_id: &str,
    request_id: &str,
) -> nanocodex_agent::Result<nanocodex_agent::Result<TurnResult>> {
    let terminal = terminal.ok_or_else(|| {
        backend_error(ManagedError::InvalidResponse(
            "terminal managed turn view omitted its terminal event",
        ))
    })?;
    if terminal.turn_id() != Some(turn_id) {
        return Err(NanocodexError::BackendContract {
            detail: "managed turn view terminal identified a different turn",
        });
    }
    let result = match terminal {
        ManagedEventData::TurnCompleted {
            final_message,
            usage,
            ..
        } => {
            let usage = usage
                .map(serde_json::from_value::<TurnUsage>)
                .transpose()
                .map_err(|_| {
                    backend_error(ManagedError::InvalidEvent(
                        "managed turn usage is not an exact TurnUsage".to_owned(),
                    ))
                })?;
            Ok(TurnResult::from_backend(
                Some(request_id.to_owned()),
                final_message,
                usage,
            ))
        }
        ManagedEventData::TurnCancelled { .. } => Err(NanocodexError::TurnCancelled),
        ManagedEventData::TurnFailed { id, error } => Err(turn_error(id, "failed", error)),
        _ => {
            return Err(backend_error(ManagedError::InvalidResponse(
                "terminal managed turn view contained a nonterminal event",
            )));
        }
    };
    Ok(result)
}

fn cursor_at_or_before(cursor: &str, observed: &str) -> nanocodex_agent::Result<bool> {
    EventCursor::parse(cursor.to_owned()).map_err(backend_error)?;
    EventCursor::parse(observed.to_owned()).map_err(backend_error)?;
    if observed == "latest" {
        return Ok(false);
    }
    Ok(cursor.len() < observed.len() || (cursor.len() == observed.len() && cursor <= observed))
}

fn image_detail(detail: ImageDetail) -> String {
    match detail {
        ImageDetail::Auto => "auto",
        ImageDetail::Low => "low",
        ImageDetail::High => "high",
        ImageDetail::Original => "original",
    }
    .to_owned()
}

fn turn_error(turn_id: String, state: &'static str, message: String) -> NanocodexError {
    backend_error(ManagedError::Turn {
        turn_id,
        state: state.to_owned(),
        message,
    })
}
