// Derived from clabby/tact@1d9ccaefd1d8613dab020812af04a91cd9b4c52c (Apache-2.0).
// Modified for Nanocodex's reusable native/WASM extension runtime.

//! Message identity, thread correlation, and bounded input validation.

use super::model::{
    AgentId, AgentMessage, AgentTerminalCompletion, AgentThread, MessageDisposition, MessageId,
    MessagePriority, MessagePurpose, MessageSender, ThreadId,
};
use std::collections::{HashMap, HashSet, VecDeque};

pub(super) const MAX_MESSAGE_BYTES: usize = 2 * 1024;
const MAX_RETAINED_MESSAGES: usize = 256;

#[derive(Default)]
pub(super) struct MessageThreads {
    next_message_id: u64,
    threads: HashMap<ThreadId, AgentThread>,
    thread_by_message: HashMap<MessageId, ThreadId>,
    retained_order: VecDeque<MessageId>,
    pending: HashSet<MessageId>,
}

impl MessageThreads {
    pub(super) fn has_pending_for(&self, agent_id: AgentId) -> bool {
        self.pending.iter().any(|id| {
            self.message(*id)
                .is_some_and(|message| message.to == agent_id)
        })
    }

    pub(super) fn prepare(
        &mut self,
        from: MessageSender,
        to: AgentId,
        priority: MessagePriority,
        purpose: MessagePurpose,
        in_reply_to: Option<MessageId>,
        body: String,
    ) -> std::io::Result<AgentMessage> {
        validate_body(&body)?;
        validate_reply(purpose, in_reply_to)?;
        let id = MessageId::next(&mut self.next_message_id);
        let thread_id = match in_reply_to {
            Some(previous_id) => self.reply_thread(previous_id, from, to)?,
            None => ThreadId::for_message(id),
        };
        Ok(AgentMessage {
            id,
            thread_id,
            from,
            to,
            priority,
            purpose,
            in_reply_to,
            body,
            terminal_completion: None,
        })
    }

    pub(super) fn prepare_terminal_completion(
        &mut self,
        from: AgentId,
        to: AgentId,
        completion: AgentTerminalCompletion,
    ) -> AgentMessage {
        let id = MessageId::next(&mut self.next_message_id);
        AgentMessage {
            id,
            thread_id: ThreadId::for_message(id),
            from: MessageSender::Agent { agent_id: from },
            to,
            priority: MessagePriority::Deferred,
            purpose: MessagePurpose::Finding,
            in_reply_to: None,
            body: "A direct child reached a terminal state.".to_owned(),
            terminal_completion: Some(completion),
        }
    }

    pub(super) fn commit(&mut self, message: AgentMessage) -> AgentThread {
        let thread = self
            .threads
            .entry(message.thread_id)
            .or_insert_with(|| AgentThread {
                id: message.thread_id,
                participants: [
                    message.from,
                    MessageSender::Agent {
                        agent_id: message.to,
                    },
                ],
                messages: Vec::new(),
            });
        self.thread_by_message.insert(message.id, message.thread_id);
        self.retained_order.push_back(message.id);
        self.pending.insert(message.id);
        thread.messages.push(message);
        thread.clone()
    }

    pub(super) fn mark_admitted(&mut self, id: MessageId, disposition: MessageDisposition) {
        if disposition == MessageDisposition::Queued {
            return;
        }
        self.pending.remove(&id);
        self.trim_history();
    }

    pub(super) fn mark_terminal(&mut self, id: MessageId) {
        self.pending.remove(&id);
        self.trim_history();
    }

    pub(super) fn thread_for_message(&self, id: MessageId) -> Option<AgentThread> {
        self.thread_by_message
            .get(&id)
            .and_then(|thread_id| self.threads.get(thread_id))
            .cloned()
    }

    pub(super) fn message(&self, id: MessageId) -> Option<AgentMessage> {
        let thread_id = self.thread_by_message.get(&id)?;
        self.threads
            .get(thread_id)?
            .messages
            .iter()
            .find(|message| message.id == id)
            .cloned()
    }

    pub(super) fn rollback(&mut self, id: MessageId) {
        self.pending.remove(&id);
        self.retained_order.retain(|retained| *retained != id);
        self.remove_message(id);
    }

    fn reply_thread(
        &self,
        previous_id: MessageId,
        from: MessageSender,
        to: AgentId,
    ) -> std::io::Result<ThreadId> {
        let thread_id = self.thread_by_message.get(&previous_id).ok_or_else(|| {
            std::io::Error::other(format!("unknown in_reply_to message {previous_id}"))
        })?;
        let thread = self
            .threads
            .get(thread_id)
            .expect("message index should reference an existing thread");
        let previous = thread
            .messages
            .iter()
            .find(|message| message.id == previous_id)
            .expect("message index should reference an existing message");
        let expected_target = previous.from.agent_id().ok_or_else(|| {
            std::io::Error::other(
                "top-level root agents do not accept inbound messages in this experiment",
            )
        })?;
        if from
            != (MessageSender::Agent {
                agent_id: previous.to,
            })
            || to != expected_target
        {
            return Err(std::io::Error::other(format!(
                "message {previous_id} can only be answered by its recipient"
            )));
        }
        Ok(*thread_id)
    }

    fn trim_history(&mut self) {
        while self.retained_order.len() > MAX_RETAINED_MESSAGES {
            let Some(index) = self
                .retained_order
                .iter()
                .position(|id| !self.pending.contains(id))
            else {
                return;
            };
            let id = self
                .retained_order
                .remove(index)
                .expect("the retained message should still exist");
            self.remove_message(id);
        }
    }

    fn remove_message(&mut self, id: MessageId) {
        let Some(thread_id) = self.thread_by_message.remove(&id) else {
            return;
        };
        let remove_thread = self.threads.get_mut(&thread_id).is_some_and(|thread| {
            thread.messages.retain(|message| message.id != id);
            thread.messages.is_empty()
        });
        if remove_thread {
            self.threads.remove(&thread_id);
        }
    }
}

fn validate_reply(purpose: MessagePurpose, in_reply_to: Option<MessageId>) -> std::io::Result<()> {
    match (purpose, in_reply_to) {
        (MessagePurpose::Reply, None) => Err(std::io::Error::other(
            "reply messages require an in_reply_to message ID",
        )),
        (MessagePurpose::Reply, Some(_)) | (_, None) => Ok(()),
        (_, Some(_)) => Err(std::io::Error::other(
            "in_reply_to is only valid for reply messages",
        )),
    }
}

fn validate_body(body: &str) -> std::io::Result<()> {
    if body.trim().is_empty() {
        return Err(std::io::Error::other("message must not be empty"));
    }
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(std::io::Error::other(format!(
            "message exceeds the {MAX_MESSAGE_BYTES}-byte limit"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MAX_MESSAGE_BYTES, MAX_RETAINED_MESSAGES, MessageThreads};
    use crate::{AgentId, MessageDisposition, MessagePriority, MessagePurpose, MessageSender};

    #[test]
    fn replies_inherit_threads_and_require_the_original_recipient() {
        let mut threads = MessageThreads::default();
        let first = threads
            .prepare(
                MessageSender::Agent {
                    agent_id: AgentId::new(1),
                },
                AgentId::new(2),
                MessagePriority::Deferred,
                MessagePurpose::Question,
                None,
                "question".to_owned(),
            )
            .unwrap();
        threads.commit(first.clone());

        let reply = threads
            .prepare(
                MessageSender::Agent {
                    agent_id: AgentId::new(2),
                },
                AgentId::new(1),
                MessagePriority::Deferred,
                MessagePurpose::Reply,
                Some(first.id),
                "answer".to_owned(),
            )
            .unwrap();
        assert_eq!(reply.thread_id, first.thread_id);

        let error = threads
            .prepare(
                MessageSender::Agent {
                    agent_id: AgentId::new(3),
                },
                AgentId::new(1),
                MessagePriority::Deferred,
                MessagePurpose::Reply,
                Some(first.id),
                "spoofed answer".to_owned(),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("only be answered by its recipient")
        );
    }

    #[test]
    fn message_length_is_bounded_in_utf8_bytes() {
        let mut threads = MessageThreads::default();
        let exact = "a".repeat(MAX_MESSAGE_BYTES);
        assert!(
            threads
                .prepare(
                    MessageSender::Root,
                    AgentId::new(1),
                    MessagePriority::Deferred,
                    MessagePurpose::Coordinate,
                    None,
                    exact,
                )
                .is_ok()
        );

        let too_long = "é".repeat(MAX_MESSAGE_BYTES / 2 + 1);
        assert!(
            threads
                .prepare(
                    MessageSender::Root,
                    AgentId::new(1),
                    MessagePriority::Deferred,
                    MessagePurpose::Coordinate,
                    None,
                    too_long,
                )
                .is_err()
        );
    }

    #[test]
    fn reply_metadata_is_consistent() {
        let mut threads = MessageThreads::default();

        let missing_reference = threads
            .prepare(
                MessageSender::Agent {
                    agent_id: AgentId::new(1),
                },
                AgentId::new(2),
                MessagePriority::Deferred,
                MessagePurpose::Reply,
                None,
                "answer".to_owned(),
            )
            .unwrap_err();
        assert!(missing_reference.to_string().contains("require"));

        let unexpected_reference = threads
            .prepare(
                MessageSender::Agent {
                    agent_id: AgentId::new(1),
                },
                AgentId::new(2),
                MessagePriority::Deferred,
                MessagePurpose::Coordinate,
                Some(crate::MessageId::new(1)),
                "answer".to_owned(),
            )
            .unwrap_err();
        assert!(unexpected_reference.to_string().contains("only valid"));
    }

    #[test]
    fn completed_history_is_bounded_without_evicting_pending_messages() {
        let mut threads = MessageThreads::default();
        let mut pending = Vec::new();
        for index in 0..=MAX_RETAINED_MESSAGES {
            let message = threads
                .prepare(
                    MessageSender::Root,
                    AgentId::new(1),
                    MessagePriority::Deferred,
                    MessagePurpose::Coordinate,
                    None,
                    format!("message {index}"),
                )
                .unwrap();
            pending.push(message.id);
            threads.commit(message);
            threads.mark_admitted(pending[index], MessageDisposition::Queued);
        }

        assert!(threads.thread_for_message(pending[0]).is_some());
        threads.mark_terminal(pending[0]);
        assert!(threads.thread_for_message(pending[0]).is_none());
        assert_eq!(threads.retained_order.len(), MAX_RETAINED_MESSAGES);

        for id in pending.into_iter().skip(1) {
            threads.mark_terminal(id);
        }
        assert_eq!(threads.retained_order.len(), MAX_RETAINED_MESSAGES);
    }
}
