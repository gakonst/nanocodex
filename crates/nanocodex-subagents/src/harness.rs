// Derived from clabby/tact@1d9ccaefd1d8613dab020812af04a91cd9b4c52c (Apache-2.0).
// Modified for Nanocodex's reusable native/WASM extension runtime.

//! Per-agent actor that exclusively owns a child runtime and its active turn.

use super::{
    capacity::{Capacity, TurnCapacity},
    model::{AgentId, AgentMessage, MessageDisposition, MessageId, MessagePriority},
    platform::{self, Task, TaskError},
    runtime::{DelegationChange, Registry, completion_instructions},
};
use nanocodex_agent::{Nanocodex, NanocodexError, Result as AgentResult, TurnControl, TurnResult};
use std::{collections::VecDeque, sync::Weak};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::Instrument;

const COMMAND_CAPACITY: usize = 8;
pub(super) const DEFERRED_CAPACITY: usize = 8;
pub(super) const URGENT_CAPACITY: usize = 4;

#[derive(Clone)]
pub(super) struct HarnessHandle {
    commands: mpsc::Sender<HarnessCommand>,
    deferred: mpsc::Sender<DeliveryCommand>,
    urgent: mpsc::Sender<DeliveryCommand>,
}

struct DeliveryCommand {
    message: AgentMessage,
    committed: Option<oneshot::Receiver<()>>,
    response: oneshot::Sender<std::io::Result<MessageDisposition>>,
}

impl DeliveryCommand {
    async fn wait_for_commit(&mut self) -> bool {
        let Some(committed) = self.committed.take() else {
            return true;
        };
        committed.await.is_ok()
    }
}

pub(super) struct EnqueuedDelivery {
    committed: oneshot::Sender<()>,
    response: oneshot::Receiver<std::io::Result<MessageDisposition>>,
}

impl EnqueuedDelivery {
    pub(super) async fn release(self) -> std::io::Result<MessageDisposition> {
        self.committed
            .send(())
            .map_err(|_| std::io::Error::other("subagent harness stopped before delivery"))?;
        self.response
            .await
            .map_err(|_| std::io::Error::other("subagent harness stopped before responding"))?
    }
}

enum HarnessCommand {
    Start {
        prompt: String,
        capacity: TurnCapacity,
        response: oneshot::Sender<std::io::Result<()>>,
    },
    Interrupt {
        response: oneshot::Sender<std::io::Result<()>>,
    },
    Close {
        response: oneshot::Sender<std::io::Result<()>>,
    },
}

struct Harness {
    root_session_id: String,
    id: AgentId,
    agent: Option<Nanocodex>,
    active: Option<ActiveTurn>,
    commands: mpsc::Receiver<HarnessCommand>,
    deferred: mpsc::Receiver<DeliveryCommand>,
    urgent: mpsc::Receiver<DeliveryCommand>,
    pending_deferred: VecDeque<AgentMessage>,
    pending_urgent: VecDeque<AgentMessage>,
    output_schema: String,
    capacity: Capacity,
    capacity_revision: watch::Receiver<u64>,
    registry: Weak<Registry>,
}

struct ActiveTurn {
    control: TurnControl,
    result: Task<AgentResult<TurnResult>>,
    _capacity: TurnCapacity,
}

enum HarnessEvent {
    Command(Option<HarnessCommand>),
    Deferred(Option<DeliveryCommand>),
    Urgent(Option<DeliveryCommand>),
    CapacityChanged,
    TurnFinished(Result<AgentResult<TurnResult>, TaskError>),
}

impl HarnessHandle {
    pub(super) async fn start(
        &self,
        prompt: String,
        capacity: TurnCapacity,
    ) -> std::io::Result<()> {
        self.request(|response| HarnessCommand::Start {
            prompt,
            capacity,
            response,
        })
        .await
    }

    pub(super) async fn interrupt(&self) -> std::io::Result<()> {
        self.request(|response| HarnessCommand::Interrupt { response })
            .await
    }

    pub(super) async fn close(&self) -> std::io::Result<()> {
        self.request(|response| HarnessCommand::Close { response })
            .await
    }

    pub(super) fn enqueue_delivery(
        &self,
        message: AgentMessage,
    ) -> std::io::Result<EnqueuedDelivery> {
        let id = message.id;
        let (response, result) = oneshot::channel();
        let (committed, wait_for_commit) = oneshot::channel();
        let command = DeliveryCommand {
            message,
            committed: Some(wait_for_commit),
            response,
        };
        let sender = match command.message.priority {
            MessagePriority::Deferred => &self.deferred,
            MessagePriority::Urgent => &self.urgent,
        };
        sender.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                std::io::Error::other(format!("agent message mailbox is full for message {id}"))
            }
            mpsc::error::TrySendError::Closed(_) => {
                std::io::Error::other("subagent harness is closed")
            }
        })?;
        Ok(EnqueuedDelivery {
            committed,
            response: result,
        })
    }

    async fn request(
        &self,
        command: impl FnOnce(oneshot::Sender<std::io::Result<()>>) -> HarnessCommand,
    ) -> std::io::Result<()> {
        let (response, result) = oneshot::channel();
        self.commands
            .send(command(response))
            .await
            .map_err(|_| std::io::Error::other("subagent harness is closed"))?;
        result
            .await
            .map_err(|_| std::io::Error::other("subagent harness stopped before responding"))?
    }
}

pub(super) fn spawn(
    root_session_id: String,
    id: AgentId,
    agent: Nanocodex,
    capacity: Capacity,
    registry: Weak<Registry>,
    output_schema: String,
) -> (HarnessHandle, Task<()>) {
    let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
    let (deferred, deferred_receiver) = mpsc::channel(DEFERRED_CAPACITY);
    let (urgent, urgent_receiver) = mpsc::channel(URGENT_CAPACITY);
    let handle = HarnessHandle {
        commands,
        deferred,
        urgent,
    };
    let capacity_revision = capacity.subscribe();
    let task = platform::spawn(
        Harness {
            root_session_id,
            id,
            agent: Some(agent),
            active: None,
            commands: receiver,
            deferred: deferred_receiver,
            urgent: urgent_receiver,
            pending_deferred: VecDeque::new(),
            pending_urgent: VecDeque::new(),
            output_schema,
            capacity,
            capacity_revision,
            registry,
        }
        .run()
        .instrument(tracing::Span::current()),
    );
    (handle, task)
}

impl Harness {
    async fn run(mut self) {
        loop {
            match self.next_event().await {
                HarnessEvent::Command(Some(command)) => {
                    if self.handle(command).await {
                        return;
                    }
                }
                HarnessEvent::Command(None) => {
                    self.fail_pending("subagent harness stopped").await;
                    drop(self.close().await);
                    return;
                }
                HarnessEvent::Deferred(Some(command)) => {
                    self.accept_delivery(command, MessagePriority::Deferred)
                        .await;
                }
                HarnessEvent::Urgent(Some(command)) => {
                    self.accept_delivery(command, MessagePriority::Urgent).await;
                }
                HarnessEvent::Deferred(None) | HarnessEvent::Urgent(None) => {}
                HarnessEvent::CapacityChanged => self.start_pending().await,
                HarnessEvent::TurnFinished(result) => self.turn_finished(result).await,
            }
        }
    }

    async fn next_event(&mut self) -> HarnessEvent {
        let Some(active) = self.active.as_mut() else {
            if self.pending_deferred.is_empty() && self.pending_urgent.is_empty() {
                return tokio::select! {
                    biased;
                    command = self.commands.recv() => HarnessEvent::Command(command),
                    urgent = self.urgent.recv() => HarnessEvent::Urgent(urgent),
                    deferred = self.deferred.recv() => HarnessEvent::Deferred(deferred),
                };
            }
            return tokio::select! {
                biased;
                command = self.commands.recv() => HarnessEvent::Command(command),
                urgent = self.urgent.recv() => HarnessEvent::Urgent(urgent),
                deferred = self.deferred.recv() => HarnessEvent::Deferred(deferred),
                _ = self.capacity_revision.changed() => HarnessEvent::CapacityChanged,
            };
        };
        tokio::select! {
            biased;
            command = self.commands.recv() => HarnessEvent::Command(command),
            urgent = self.urgent.recv() => HarnessEvent::Urgent(urgent),
            deferred = self.deferred.recv() => HarnessEvent::Deferred(deferred),
            result = &mut active.result => HarnessEvent::TurnFinished(result),
        }
    }

    async fn handle(&mut self, command: HarnessCommand) -> bool {
        match command {
            HarnessCommand::Start {
                prompt,
                capacity,
                response,
            } => {
                let _ = response.send(self.start_turn(prompt, capacity).await);
                false
            }
            HarnessCommand::Interrupt { response } => {
                {
                    self.reject_waiting_deliveries("message rejected by agent interruption")
                        .await;
                    self.fail_pending("message cancelled by agent interruption")
                        .await;
                }
                let _ = response.send(self.stop_active().await);
                false
            }
            HarnessCommand::Close { response } => {
                {
                    self.reject_waiting_deliveries("message rejected because the agent closed")
                        .await;
                    self.fail_pending("message cancelled because the agent closed")
                        .await;
                }
                let result = self.close().await;
                let _ = response.send(result);
                true
            }
        }
    }

    async fn accept_delivery(&mut self, mut command: DeliveryCommand, priority: MessagePriority) {
        if !command.wait_for_commit().await {
            return;
        }
        let steer = if priority == MessagePriority::Urgent && self.active.is_some() {
            match self.registry.upgrade() {
                Some(registry) => {
                    registry
                        .begin_turn_steer(&self.root_session_id, self.id)
                        .await
                }
                None => None,
            }
        } else {
            None
        };
        if let Some(steer) = steer {
            let delegation = self.begin_delegation(command.message.id).await;
            let prompt = format!(
                "{}\n\n{}",
                command.message.prompt(),
                completion_instructions(&self.output_schema, steer.token())
            );
            let result = self
                .active
                .as_ref()
                .expect("steering requires an active turn")
                .control
                .steer(prompt)
                .await;
            if let Some(registry) = self.registry.upgrade() {
                registry
                    .finish_turn_steer(&self.root_session_id, steer, result.is_ok())
                    .await;
            }
            match result {
                Ok(()) => {
                    self.admit(
                        command.message.id,
                        command.response,
                        MessageDisposition::Steered,
                    )
                    .await;
                    return;
                }
                Err(NanocodexError::TurnNotSteerable | NanocodexError::TurnStopped) => {
                    self.rollback_delegation(delegation).await;
                }
                Err(error) => {
                    self.rollback_delegation(delegation).await;
                    self.reject(
                        command,
                        format!("could not urgently message agent {}: {error}", self.id),
                    )
                    .await;
                    return;
                }
            }
        }

        if self.active.is_none()
            && self.pending_deferred.is_empty()
            && self.pending_urgent.is_empty()
            && let Ok(capacity) = self.capacity.reserve()
        {
            let delegation = self.begin_delegation(command.message.id).await;
            if let Err(error) = self.start_turn(command.message.prompt(), capacity).await {
                self.rollback_delegation(delegation).await;
                self.reject(command, error.to_string()).await;
                return;
            }
            self.admit(
                command.message.id,
                command.response,
                MessageDisposition::Started,
            )
            .await;
            return;
        }

        self.queue_delivery(command, priority).await;
    }

    async fn queue_delivery(&mut self, command: DeliveryCommand, priority: MessagePriority) {
        let queue = match priority {
            MessagePriority::Deferred => &mut self.pending_deferred,
            MessagePriority::Urgent => &mut self.pending_urgent,
        };
        let limit = match priority {
            MessagePriority::Deferred => DEFERRED_CAPACITY,
            MessagePriority::Urgent => URGENT_CAPACITY,
        };
        if queue.len() >= limit {
            self.reject(
                command,
                format!("{priority:?} mailbox for agent {} is full", self.id),
            )
            .await;
            return;
        }
        let id = command.message.id;
        queue.push_back(command.message);
        self.admit(id, command.response, MessageDisposition::Queued)
            .await;
    }

    async fn start_pending(&mut self) {
        while self.active.is_none() {
            let Some(message) = self
                .pending_urgent
                .front()
                .or_else(|| self.pending_deferred.front())
            else {
                return;
            };
            let Ok(capacity) = self.capacity.reserve() else {
                return;
            };
            let id = message.id;
            let message = if self.pending_urgent.front().is_some() {
                self.pending_urgent.pop_front()
            } else {
                self.pending_deferred.pop_front()
            }
            .expect("a pending message should still exist");
            let delegation = self.begin_delegation(id).await;
            match self.start_turn(message.prompt(), capacity).await {
                Ok(()) => {
                    if let Some(registry) = self.registry.upgrade() {
                        registry
                            .message_delivered(
                                &self.root_session_id,
                                id,
                                MessageDisposition::Started,
                            )
                            .await;
                    }
                }
                Err(error) => {
                    self.rollback_delegation(delegation).await;
                    self.publish_message_failure(id, error.to_string()).await;
                }
            }
        }
    }

    async fn fail_pending(&mut self, reason: &str) {
        let pending = self
            .pending_urgent
            .drain(..)
            .chain(self.pending_deferred.drain(..))
            .map(|message| message.id)
            .collect::<Vec<_>>();
        for id in pending {
            self.publish_message_failure(id, reason.to_owned()).await;
        }
    }

    async fn reject_waiting_deliveries(&mut self, reason: &str) {
        while let Ok(command) = self.urgent.try_recv() {
            self.reject(command, reason.to_owned()).await;
        }
        while let Ok(command) = self.deferred.try_recv() {
            self.reject(command, reason.to_owned()).await;
        }
    }

    async fn reject(&self, mut command: DeliveryCommand, reason: String) {
        if !command.wait_for_commit().await {
            return;
        }
        if let Some(registry) = self.registry.upgrade() {
            registry
                .message_rejected(&self.root_session_id, command.message.id)
                .await;
        }
        let _ = command.response.send(Err(std::io::Error::other(reason)));
    }

    async fn publish_message_failure(&self, id: MessageId, error: String) {
        if let Some(registry) = self.registry.upgrade() {
            registry
                .message_failed(&self.root_session_id, id, error)
                .await;
        }
    }

    async fn admit(
        &self,
        id: MessageId,
        response: oneshot::Sender<std::io::Result<MessageDisposition>>,
        disposition: MessageDisposition,
    ) {
        let Some(registry) = self.registry.upgrade() else {
            let _ = response.send(Err(std::io::Error::other(
                "subagent runtime stopped before admitting the message",
            )));
            return;
        };
        registry
            .message_admitted(&self.root_session_id, id, disposition)
            .await;
        let _ = response.send(Ok(disposition));
    }

    async fn begin_delegation(&self, id: MessageId) -> Option<DelegationChange> {
        let registry = self.registry.upgrade()?;
        registry
            .begin_message_delivery(&self.root_session_id, id)
            .await
    }

    async fn rollback_delegation(&self, change: Option<DelegationChange>) {
        let (Some(registry), Some(change)) = (self.registry.upgrade(), change) else {
            return;
        };
        registry
            .rollback_message_delivery(&self.root_session_id, change)
            .await;
    }

    async fn start_turn(&mut self, prompt: String, capacity: TurnCapacity) -> std::io::Result<()> {
        if self.active.is_some() {
            return Err(std::io::Error::other(format!(
                "agent {} is not idle",
                self.id
            )));
        }
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| std::io::Error::other("subagent runtime is closed"))?;
        let agent = self
            .agent
            .as_ref()
            .ok_or_else(|| std::io::Error::other(format!("agent {} is closed", self.id)))?;
        let Some(turn_token) = registry
            .harness_turn_started(&self.root_session_id, self.id)
            .await
        else {
            return Err(std::io::Error::other(format!(
                "agent {} cannot start another turn",
                self.id
            )));
        };
        let prompt = format!(
            "{prompt}\n\n{}",
            completion_instructions(&self.output_schema, turn_token)
        );
        let turn = match agent.prompt(prompt).await {
            Ok(turn) => turn,
            Err(error) => {
                let error = format!("could not start agent {}: {error}", self.id);
                registry
                    .harness_turn_start_failed(&self.root_session_id, self.id, error.clone())
                    .await;
                return Err(std::io::Error::other(error));
            }
        };
        let control = turn.control();
        let result = platform::spawn(turn);
        self.active = Some(ActiveTurn {
            control,
            result,
            _capacity: capacity,
        });
        Ok(())
    }

    async fn stop_active(&mut self) -> std::io::Result<()> {
        let Some(active) = self.active.as_ref() else {
            return Ok(());
        };
        let cancellation = active.control.cancel().await;
        self.finish_active().await;
        match cancellation {
            Ok(()) | Err(NanocodexError::TurnNotCancellable) => Ok(()),
            Err(error) => Err(std::io::Error::other(format!(
                "could not stop agent {}: {error}",
                self.id
            ))),
        }
    }

    async fn finish_active(&mut self) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        let result = (&mut active.result).await;
        self.publish_turn_result(result).await;
    }

    async fn turn_finished(&mut self, result: Result<AgentResult<TurnResult>, TaskError>) {
        self.active = None;
        self.publish_turn_result(result).await;
    }

    async fn publish_turn_result(&self, result: Result<AgentResult<TurnResult>, TaskError>) {
        let result = result.unwrap_or_else(|error| {
            Err(NanocodexError::InvalidRequest(format!(
                "subagent turn task failed: {error}"
            )))
        });
        if let Some(registry) = self.registry.upgrade() {
            registry
                .harness_turn_finished(&self.root_session_id, self.id, result)
                .await;
        }
    }

    async fn close(&mut self) -> std::io::Result<()> {
        let shutdown_result = match self.agent.take() {
            Some(agent) => agent.shutdown().await.map_err(|error| {
                std::io::Error::other(format!("could not close agent {}: {error}", self.id))
            }),
            None => Ok(()),
        };
        self.finish_active().await;
        if let Some(registry) = self.registry.upgrade() {
            registry
                .harness_closed(&self.root_session_id, self.id)
                .await;
        }
        shutdown_result
    }
}
