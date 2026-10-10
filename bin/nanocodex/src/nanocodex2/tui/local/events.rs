//! Bridges the local agent's event stream into the managed projection.
//!
//! The unified driver renders every transcript update from `ManagedEvent`s, so
//! local mode wraps each `AgentEvent` as `ManagedEventData::Event` with a synthetic
//! monotonic cursor and the request id the driver submitted. A completed or
//! failed run additionally yields the terminal managed event the driver expects.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use nanocodex::agent::events::{AgentEvent, AgentEventKind};
use nanocodex_managed::{ManagedEvent, ManagedEventData};
use tokio::{sync::mpsc, task::JoinHandle};

/// Request ids the driver submitted, in admission order.
#[derive(Clone, Default)]
pub(crate) struct Submissions(Arc<Mutex<VecDeque<String>>>);

impl Submissions {
    pub(crate) fn push(&self, request_id: String) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(request_id);
    }

    pub(crate) fn remove(&self, request_id: &str) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|pending| pending != request_id);
    }

    fn pop(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }
}

/// Turns one local event stream into managed events.
pub(crate) struct Bridge {
    submissions: Submissions,
    cursor: u64,
    current: Option<String>,
    final_message: String,
}

impl Bridge {
    pub(crate) fn new(submissions: Submissions) -> Self {
        Self {
            submissions,
            cursor: 0,
            current: None,
            final_message: String::new(),
        }
    }

    fn envelope(&mut self, data: ManagedEventData) -> ManagedEvent {
        self.cursor = self.cursor.saturating_add(1);
        ManagedEvent {
            cursor: self.cursor.to_string(),
            created_at: Some(unix_seconds()),
            turn_id: self.current.clone(),
            data,
        }
    }

    /// Maps one agent event to the managed events it produces.
    pub(crate) fn map(&mut self, event: &AgentEvent, agent_id: Option<u64>) -> Vec<ManagedEvent> {
        let root = agent_id.is_none();
        if root && matches!(event.kind, AgentEventKind::RunStarted) && self.current.is_none() {
            self.current = self.submissions.pop();
            self.final_message.clear();
        }
        if root && matches!(event.kind, AgentEventKind::AssistantMessage) {
            #[derive(serde::Deserialize)]
            struct Message {
                text: String,
            }
            if let Ok(message) = serde_json::from_str::<Message>(event.payload.get()) {
                self.final_message = message.text;
            }
        }
        let mut out = Vec::with_capacity(2);
        if let Ok(raw) = serde_json::value::to_raw_value(event) {
            out.push(self.envelope(ManagedEventData::Event {
                event: raw,
                agent_id,
            }));
        }
        if root {
            let terminal = match event.kind {
                AgentEventKind::RunCompleted => {
                    self.current
                        .clone()
                        .map(|id| ManagedEventData::TurnCompleted {
                            id,
                            final_message: std::mem::take(&mut self.final_message),
                            usage: None,
                            citations: Vec::new(),
                            usage_error: None,
                        })
                }
                AgentEventKind::RunFailed => self.current.clone().map(|id| {
                    let error = failure_text(event);
                    // Both harnesses report a cancelled run as RunFailed with
                    // status "cancelled" and no message; the text check covers
                    // producers that only describe the cancellation.
                    if run_cancelled(event) || error.to_ascii_lowercase().contains("cancel") {
                        ManagedEventData::TurnCancelled { id }
                    } else {
                        ManagedEventData::TurnFailed { id, error }
                    }
                }),
                _ => None,
            };
            if let Some(terminal) = terminal {
                out.push(self.envelope(terminal));
                self.current = None;
            }
        }
        out
    }
}

fn run_cancelled(event: &AgentEvent) -> bool {
    #[derive(serde::Deserialize)]
    struct Terminal {
        #[serde(default)]
        status: Option<String>,
    }
    serde_json::from_str::<Terminal>(event.payload.get())
        .is_ok_and(|terminal| terminal.status.as_deref() == Some("cancelled"))
}

fn failure_text(event: &AgentEvent) -> String {
    #[derive(serde::Deserialize)]
    struct Failure {
        #[serde(default)]
        error: Option<serde_json::Value>,
        #[serde(default)]
        message: Option<String>,
    }
    match serde_json::from_str::<Failure>(event.payload.get()) {
        Ok(Failure {
            message: Some(message),
            ..
        }) => message,
        Ok(Failure {
            error: Some(serde_json::Value::String(error)),
            ..
        }) => error,
        Ok(Failure {
            error: Some(error), ..
        }) => error.to_string(),
        _ => "turn failed".to_owned(),
    }
}

fn unix_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// Spawns the bridge task; the receiver feeds the driver's managed-event arm.
pub(crate) fn spawn(
    mut events: nanocodex::AgentEvents,
    submissions: Submissions,
) -> (mpsc::UnboundedReceiver<ManagedEvent>, JoinHandle<()>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut bridge = Bridge::new(submissions);
        while let Some(event) = events.recv().await {
            for managed in bridge.map(&event, None) {
                if sender.send(managed).is_err() {
                    return;
                }
            }
        }
    });
    (receiver, task)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, seq: u64, payload: &str) -> AgentEvent {
        serde_json::from_str(&format!(
            r#"{{"protocol_version":1,"request_id":"s","seq":{seq},"type":"{kind}","payload":{payload}}}"#
        ))
        .expect("event")
    }

    #[test]
    fn completed_runs_carry_the_submitted_turn_id_and_final_message() {
        let submissions = Submissions::default();
        submissions.push("turn-1".to_owned());
        let mut bridge = Bridge::new(submissions);
        let started = bridge.map(&event("run.started", 1, "{}"), None);
        assert_eq!(started[0].turn_id.as_deref(), Some("turn-1"));
        bridge.map(
            &event(
                "assistant.message",
                2,
                r#"{"model_call_index":0,"item_id":null,"phase":null,"text":"hi"}"#,
            ),
            None,
        );
        let done = bridge.map(&event("run.completed", 3, "{}"), None);
        assert_eq!(done.len(), 2);
        assert!(matches!(
            &done[1].data,
            ManagedEventData::TurnCompleted { id, final_message, .. }
                if id == "turn-1" && final_message == "hi"
        ));
        assert_eq!(done[1].cursor, "4");
    }
}
