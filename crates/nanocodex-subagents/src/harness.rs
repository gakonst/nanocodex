// Derived from clabby/tact@1d9ccaefd1d8613dab020812af04a91cd9b4c52c (Apache-2.0).
// Modified for Nanocodex's reusable native/WASM extension runtime.

//! Per-agent actor that exclusively owns a child runtime and its active turn.

use super::{
    capacity::{Capacity, TurnCapacity},
    model::{AgentId, AgentMessage, MessageDisposition, MessageId, MessagePriority},
    platform::{self, Task, TaskError},
    runtime::{DelegationChange, Registry, completion_instructions},
};
use nanocodex_agent::input::Prompt;
use nanocodex_agent::{
    ChildSnapshot, Nanocodex, NanocodexError, PromptRequest, Result as AgentResult, TurnControl,
    TurnResult,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Weak},
};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::Instrument;

const COMMAND_CAPACITY: usize = 8;

#[derive(Clone)]
pub(super) struct HarnessHandle {
    registry: Weak<Registry>,
    commands: mpsc::Sender<HarnessCommand>,
    deferred: mpsc::UnboundedSender<DeliveryCommand>,
    urgent: mpsc::UnboundedSender<DeliveryCommand>,
}

struct DeliveryCommand {
    // Admission may outlive the sending caller before the actor accepts it.
    registry: Arc<Registry>,
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
    Snapshot {
        response: oneshot::Sender<std::io::Result<ChildSnapshot>>,
    },
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
    deferred: mpsc::UnboundedReceiver<DeliveryCommand>,
    urgent: mpsc::UnboundedReceiver<DeliveryCommand>,
    pending_deferred: VecDeque<AgentMessage>,
    pending_urgent: VecDeque<AgentMessage>,
    pending_ownership: Option<Arc<Registry>>,
    output_schema: String,
    rehydrated_assignment: Option<String>,
    capacity: Capacity,
    capacity_revision: watch::Receiver<u64>,
    registry: Weak<Registry>,
}

struct ActiveTurn {
    registry: Arc<Registry>,
    control: TurnControl,
    result: Task<AgentResult<TurnResult>>,
    _capacity: TurnCapacity,
}

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        // Retiring a poisoned harness must also drop its detached result waiter.
        // This releases local handles without issuing a durable cancellation.
        self.result.abort();
    }
}

enum HarnessEvent {
    Command(Option<HarnessCommand>),
    Deferred(Option<DeliveryCommand>),
    Urgent(Option<DeliveryCommand>),
    CapacityChanged,
    TurnFinished(Result<AgentResult<TurnResult>, TaskError>),
}

impl HarnessHandle {
    pub(super) async fn snapshot(&self) -> std::io::Result<ChildSnapshot> {
        let (response, result) = oneshot::channel();
        self.commands
            .send(HarnessCommand::Snapshot { response })
            .await
            .map_err(|_| std::io::Error::other("subagent harness is closed"))?;
        result
            .await
            .map_err(|_| std::io::Error::other("subagent snapshot interrupted"))?
    }

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
        let (response, result) = oneshot::channel();
        let (committed, wait_for_commit) = oneshot::channel();
        let command = DeliveryCommand {
            registry: self
                .registry
                .upgrade()
                .ok_or_else(|| std::io::Error::other("subagent registry stopped"))?,
            message,
            committed: Some(wait_for_commit),
            response,
        };
        let sender = match command.message.priority {
            MessagePriority::Deferred => &self.deferred,
            MessagePriority::Urgent => &self.urgent,
        };
        sender
            .send(command)
            .map_err(|_| std::io::Error::other("subagent harness is closed"))?;
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
    rehydrated_assignment: Option<String>,
) -> (HarnessHandle, Task<()>) {
    let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
    let (deferred, deferred_receiver) = mpsc::unbounded_channel();
    let (urgent, urgent_receiver) = mpsc::unbounded_channel();
    let handle = HarnessHandle {
        registry: registry.clone(),
        commands,
        deferred,
        urgent,
    };
    let capacity_revision = capacity.subscribe();
    let task = platform::spawn(
        Harness {
            root_session_id,
            id,
            // Managed children use active-turn and queued-delivery leases, not
            // the independent caller lease supplied by native construction.
            agent: Some(agent.without_caller_ownership()),
            active: None,
            commands: receiver,
            deferred: deferred_receiver,
            urgent: urgent_receiver,
            pending_deferred: VecDeque::new(),
            pending_urgent: VecDeque::new(),
            pending_ownership: None,
            output_schema,
            rehydrated_assignment,
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
            // Completed turns must release their capacity and drain queued work
            // even when deferred admission requests keep arriving.
            result = &mut active.result => HarnessEvent::TurnFinished(result),
            deferred = self.deferred.recv() => HarnessEvent::Deferred(deferred),
        }
    }

    async fn handle(&mut self, command: HarnessCommand) -> bool {
        match command {
            HarnessCommand::Snapshot { response } => {
                let result = match &self.agent {
                    Some(agent) => agent
                        .runtime_snapshot()
                        .await
                        .map_err(std::io::Error::other),
                    None => Err(std::io::Error::other("subagent runtime is unloaded")),
                };
                let _ = response.send(result);
                false
            }
            HarnessCommand::Start {
                prompt,
                capacity,
                response,
            } => {
                let _ = response.send(self.start_turn(prompt, capacity, None).await);
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
        let durable = self
            .registry
            .upgrade()
            .is_some_and(|registry| registry.durable_replay());
        if let (Some(registry), Some(agent)) = (self.registry.upgrade(), self.agent.as_ref())
            && let Some(operation) = registry
                .message_steer_operation(&self.root_session_id, command.message.id)
                .await
        {
            match agent
                .has_steer_receipt(operation, format!("child-message:{}", command.message.id))
                .await
            {
                Ok(true) => {
                    self.admit(
                        command.message.id,
                        command.response,
                        MessageDisposition::Steered,
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    self.reject(command, error.to_string()).await;
                    return;
                }
                Ok(false) => {}
            }
        }
        // Backends without atomic steering receipts retain urgent messages in
        // the durable mailbox until the current turn reaches its boundary.
        let can_steer = !durable || self.agent.as_ref().is_some_and(Nanocodex::durable_steering);
        let steer = if priority == MessagePriority::Urgent && self.active.is_some() && can_steer {
            match self.registry.upgrade() {
                Some(registry) => {
                    match registry
                        .begin_turn_steer(&self.root_session_id, self.id, command.message.id)
                        .await
                    {
                        Ok(steer) => steer,
                        Err(error) => {
                            let _ = command.response.send(Err(error));
                            return;
                        }
                    }
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
                completion_instructions(&self.output_schema)
            );
            let control = &self
                .active
                .as_ref()
                .expect("steering requires an active turn")
                .control;
            let input = Prompt::new(prompt).with_instruction_revision(steer.revision());
            let result = if durable {
                control
                    .steer_with_id(format!("child-message:{}", command.message.id), input)
                    .await
            } else {
                control.steer(input).await
            };
            if let Some(registry) = self.registry.upgrade()
                && let Err(error) = registry
                    .finish_turn_steer(&self.root_session_id, steer, result.is_ok())
                    .await
            {
                let _ = command.response.send(Err(error));
                return;
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
            if let Err(error) = self
                .start_turn(command.message.prompt(), capacity, Some(command.message.id))
                .await
            {
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
        self.pending_ownership = Some(command.registry.clone());
        let queue = match priority {
            MessagePriority::Deferred => &mut self.pending_deferred,
            MessagePriority::Urgent => &mut self.pending_urgent,
        };
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
                self.pending_ownership = None;
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
            match self.start_turn(message.prompt(), capacity, Some(id)).await {
                Ok(()) => {
                    if let Some(registry) = self.registry.upgrade() {
                        let _ = registry
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
        if self.pending_urgent.is_empty() && self.pending_deferred.is_empty() {
            self.pending_ownership = None;
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
        self.pending_ownership = None;
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
            let _ = registry
                .message_rejected(&self.root_session_id, command.message.id)
                .await;
        }
        let _ = command.response.send(Err(std::io::Error::other(reason)));
    }

    async fn publish_message_failure(&self, id: MessageId, error: String) {
        if let Some(registry) = self.registry.upgrade() {
            let _ = registry
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
        let result = registry
            .message_admitted(&self.root_session_id, id, disposition)
            .await
            .map(|()| disposition);
        let _ = response.send(result);
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

    async fn start_turn(
        &mut self,
        prompt: String,
        capacity: TurnCapacity,
        message_id: Option<MessageId>,
    ) -> std::io::Result<()> {
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
        // A first turn interrupted before a committed model boundary still
        // needs its assignment after idle eviction. Include it in the next
        // admitted prompt when rehydrating from memory.
        let prompt = match &self.rehydrated_assignment {
            Some(assignment) => format!("{assignment}\n\n{prompt}"),
            None => prompt,
        };
        let prompt = format!(
            "{prompt}\n\n{}",
            completion_instructions(&self.output_schema)
        );
        let (instruction_revision, prompt, operation_id, cancel_on_admission) = registry
            .admit_child_turn(&self.root_session_id, self.id, prompt, message_id)
            .await?;
        let mut request =
            PromptRequest::new(Prompt::new(prompt).with_instruction_revision(instruction_revision));
        if let Some(operation_id) = operation_id {
            request = request.request_id(operation_id);
        }
        if cancel_on_admission {
            request = request.cancel_on_admission();
        }
        let turn = match agent.prompt(request).await {
            Ok(turn) => turn,
            Err(error) => {
                let error = format!("could not start agent {}: {error}", self.id);
                registry
                    .harness_turn_start_failed(&self.root_session_id, self.id, error.clone())
                    .await?;
                return Err(std::io::Error::other(error));
            }
        };
        self.rehydrated_assignment = None;
        let control = turn.control();
        let result = platform::spawn(turn);
        self.active = Some(ActiveTurn {
            registry,
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
        // Release capacity before draining queued work, but keep the registry
        // through snapshot persistence and transfer to the next execution.
        let ownership = self.active.as_ref().map(|active| active.registry.clone());
        self.active = None;
        self.publish_turn_result(result).await;
        // Do not depend on the capacity watch to notice our own turn ending.
        // If another agent took the released slot, the watch still retries later.
        self.start_pending().await;
        drop(ownership);
    }

    async fn publish_turn_result(&self, result: Result<AgentResult<TurnResult>, TaskError>) {
        let result = result.unwrap_or_else(|error| {
            Err(NanocodexError::InvalidRequest(format!(
                "subagent turn task failed: {error}"
            )))
        });
        if let Some(registry) = self.registry.upgrade() {
            let snapshot = match &self.agent {
                Some(agent) => agent.runtime_snapshot().await.ok(),
                None => None,
            };
            if let Err(error) = registry
                .harness_turn_finished(&self.root_session_id, self.id, result, snapshot)
                .await
            {
                tracing::error!(%error, "durable child settlement failed; cold recovery required");
            }
        }
    }

    async fn close(&mut self) -> std::io::Result<()> {
        self.stop_active().await?;
        let shutdown_result = match self.agent.as_ref() {
            Some(agent) => agent.shutdown().await.map_err(|error| {
                std::io::Error::other(format!("could not close agent {}: {error}", self.id))
            }),
            None => Ok(()),
        };
        self.finish_active().await;
        if let Some(registry) = self.registry.upgrade() {
            registry
                .harness_closed(&self.root_session_id, self.id)
                .await?;
        }
        shutdown_result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MessagePurpose, MessageSender, ThreadId};

    #[tokio::test]
    async fn delivery_transport_accepts_bursts_and_preserves_commit_and_fifo() {
        let (commands, _commands) = mpsc::channel(COMMAND_CAPACITY);
        let (deferred, mut deferred_receiver) = mpsc::unbounded_channel();
        let (urgent, mut urgent_receiver) = mpsc::unbounded_channel();
        let (registry, _, _updates) = crate::channel(128);
        let handle = HarnessHandle {
            registry: Arc::downgrade(&registry),
            commands,
            deferred,
            urgent,
        };
        for (priority, receiver) in [
            (MessagePriority::Deferred, &mut deferred_receiver),
            (MessagePriority::Urgent, &mut urgent_receiver),
        ] {
            let mut deliveries = Vec::new();
            // No receiver runs until every send has succeeded.
            for index in 1..=128 {
                let id = MessageId::new(index);
                deliveries.push(
                    handle
                        .enqueue_delivery(AgentMessage {
                            id,
                            thread_id: ThreadId::for_message(id),
                            from: MessageSender::Root,
                            to: AgentId::new(1),
                            priority,
                            purpose: MessagePurpose::Coordinate,
                            in_reply_to: None,
                            body: format!("finding {index}"),
                        })
                        .unwrap(),
                );
            }
            for (index, delivery) in deliveries.into_iter().enumerate() {
                let mut command = receiver.try_recv().unwrap();
                assert_eq!(command.message.id, MessageId::new(index as u64 + 1));
                assert!(matches!(
                    command.committed.as_mut().unwrap().try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                ));
                let (receipt, ()) = tokio::join!(delivery.release(), async {
                    assert!(command.wait_for_commit().await);
                    command
                        .response
                        .send(Ok(MessageDisposition::Queued))
                        .unwrap();
                });
                assert_eq!(receipt.unwrap(), MessageDisposition::Queued);
            }
            assert!(receiver.try_recv().is_err());
        }
    }
}
