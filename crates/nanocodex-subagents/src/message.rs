// Derived from clabby/tact@1d9ccaefd1d8613dab020812af04a91cd9b4c52c (Apache-2.0).
// Modified for Nanocodex's reusable native/WASM extension runtime.

//! Message identity, thread correlation, and bounded input validation.

use super::model::{
    AgentId, AgentMessage, AgentThread, MessageDisposition, MessageId, MessagePriority,
    MessagePurpose, MessageSender, ThreadId,
};
use std::collections::{HashMap, HashSet, VecDeque};

pub(super) const MAX_MESSAGE_BYTES: usize = 2 * 1024;
const MAX_RETAINED_MESSAGES: usize = 256;

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct MessageThreads {
    next_message_id: u64,
    threads: HashMap<ThreadId, AgentThread>,
    thread_by_message: HashMap<MessageId, ThreadId>,
    retained_order: VecDeque<MessageId>,
    pending: HashSet<MessageId>,
}

impl MessageThreads {
    pub(super) fn pending_for(&self, agent_id: AgentId) -> Vec<AgentMessage> {
        let mut messages: Vec<_> = self
            .pending
            .iter()
            .filter_map(|id| self.message(*id))
            .filter(|message| message.to == agent_id)
            .collect();
        messages.sort_by_key(|message| (message.priority != MessagePriority::Urgent, message.id));
        messages
    }

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
            Some(previous_id) => self.reference_thread(previous_id, from, to, purpose)?,
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
        })
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

    fn reference_thread(
        &self,
        previous_id: MessageId,
        from: MessageSender,
        to: AgentId,
        purpose: MessagePurpose,
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
        let same_direction = from == previous.from && to == previous.to;
        let reverse_direction =
            from == (MessageSender::Agent {
                agent_id: previous.to,
            }) && previous.from == (MessageSender::Agent { agent_id: to });
        if purpose == MessagePurpose::Reply {
            if !reverse_direction {
                return Err(std::io::Error::other(format!(
                    "message {previous_id} can only be answered by its recipient; use a non-reply \
                     purpose to continue the same two-party thread in the original direction"
                )));
            }
        } else if !same_direction && !reverse_direction {
            return Err(std::io::Error::other(format!(
                "in_reply_to message {previous_id} belongs to a different two-party thread"
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
        _ => Ok(()),
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
