//! Local /btw: a side conversation forked from the main agent's current
//! snapshot and shown in its own pane, plus /collapse back into main.
//!
//! The driver opens the
//! fork pane exactly as for a managed /btw and runs [`run`], which speaks the same
//! [`Request`]/[`Event`] contract as the hosted fork (`super::super::btw`).

use std::sync::{Arc, Mutex, PoisonError};

use nanocodex::Nanocodex;
use nanocodex_agent::{PromptRequest, TurnControl, TurnResult};
use nanocodex_managed::AgentSettings;
use tokio::{sync::mpsc, task::JoinSet};

use super::{Feature, FeatureCommand, FeatureContext, FeaturePrompt, FeatureUpdate};
use crate::nanocodex2::tui::{
    btw::{Event, Request},
    history,
    pane::PaneId,
    transcript::{LocalEvent, TranscriptRecord, TurnId},
};

/// Prepended to the first side question (legacy BTW_BOUNDARY).
pub(crate) const BTW_BOUNDARY: &str = r"You are answering an ephemeral BTW side question.
Treat inherited conversation history only as reference context. Do not resume or complete an
earlier task. Answer only the question after this boundary. Do not modify the workspace unless
that side question explicitly requests a mutation.

BTW question:
";

/// Bounds the inline collapse so a long side thread cannot crowd out main context.
const MAX_INLINE_COLLAPSE_BYTES: usize = 48 * 1024;

/// The open local side thread, shared with /collapse and /split.
#[derive(Clone)]
pub(crate) struct Side {
    pub(crate) pane: PaneId,
    pub(crate) agent: Nanocodex,
    /// A side turn is running.
    pub(crate) busy: bool,
    /// At least one side turn completed.
    pub(crate) completed: bool,
    /// (is_user, text) exchanges for the inline collapse.
    pub(crate) exchanges: Vec<(bool, String)>,
}

static ACTIVE: Mutex<Option<Side>> = Mutex::new(None);

/// The open side thread, if any.
pub(crate) fn active() -> Option<Side> {
    ACTIVE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Forgets the side thread (after /collapse or /split handed it off).
pub(crate) fn release(pane: PaneId) -> Option<Side> {
    let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
    if active.as_ref().is_some_and(|side| side.pane == pane) {
        active.take()
    } else {
        None
    }
}

fn with_side(pane: PaneId, update: impl FnOnce(&mut Side)) {
    if let Some(side) = ACTIVE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
        .filter(|side| side.pane == pane)
    {
        update(side);
    }
}

/// Clears the shared state when the driver aborts or drops the side task.
struct Registration(PaneId);

impl Drop for Registration {
    fn drop(&mut self) {
        drop(release(self.0));
    }
}

/// Runs one local side conversation until its pane closes.
pub(in crate::nanocodex2::tui) async fn run(
    pane: PaneId,
    main: Nanocodex,
    settings: AgentSettings,
    start_sequence: u64,
    mut requests: mpsc::UnboundedReceiver<Request>,
    events: mpsc::UnboundedSender<Event>,
) {
    let (agent, mut agent_events) = match main
        .fork(nanocodex::ForkRequest::latest().side_conversation())
        .await
    {
        Ok(fork) => fork,
        Err(error) => {
            let _ = events.send(Event::Failed {
                pane,
                error: format!("could not fork the local agent: {error}"),
                opening: true,
            });
            return;
        }
    };
    *ACTIVE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Side {
        pane,
        agent: agent.clone(),
        busy: false,
        completed: false,
        exchanges: Vec::new(),
    });
    let _registration = Registration(pane);
    if events
        .send(Event::Ready {
            pane,
            agent_id: agent.session_id().to_owned(),
            settings,
        })
        .is_err()
    {
        return;
    }
    // The pane starts with the parent's transcript; never reuse its sequences.
    let mut sequence = start_sequence.max(1);
    let mut first_prompt = true;
    let mut control: Option<TurnControl> = None;
    let mut turns = JoinSet::<nanocodex_agent::Result<TurnResult>>::new();
    let mut stream_open = true;
    loop {
        tokio::select! {
            request = requests.recv() => match request {
                Some(Request::Submit(prompt)) => {
                    let display = prompt.display_text().to_owned();
                    if let Ok(record) = TranscriptRecord::from_local(
                        sequence,
                        history::unix_ms(),
                        LocalEvent::UserSubmitted { id: TurnId::new(sequence), text: display.clone() },
                    ) {
                        sequence += 1;
                        let _ = events.send(Event::Record { pane, record: Arc::new(record) });
                    }
                    if control.is_some() {
                        let _ = events.send(Event::Failed {
                            pane,
                            error: "The side agent is still answering; wait or press Esc to cancel.".to_owned(),
                            opening: false,
                        });
                        continue;
                    }
                    let prompt = if std::mem::take(&mut first_prompt) {
                        prompt.prepend_text(BTW_BOUNDARY.to_owned())
                    } else {
                        prompt
                    };
                    match agent.prompt(PromptRequest::new(prompt.agent_prompt())).await {
                        Ok(turn) => {
                            control = Some(turn.control());
                            with_side(pane, |side| {
                                side.busy = true;
                                side.exchanges.push((true, display));
                            });
                            turns.spawn(turn.result());
                        }
                        Err(error) => {
                            let _ = events.send(Event::Failed { pane, error: error.to_string(), opening: false });
                            let _ = events.send(Event::Finished(pane));
                        }
                    }
                }
                Some(Request::Cancel) => {
                    if let Some(control) = &control
                        && let Err(error) = control.cancel().await
                    {
                        let _ = events.send(Event::Failed { pane, error: error.to_string(), opening: false });
                    }
                }
                None => {
                    // The pane closed (/close, /collapse, /split): stop the fork itself.
                    if let Some(control) = control.take() {
                        drop(control.cancel().await);
                    }
                    drop(agent.shutdown().await);
                    return;
                }
            },
            event = agent_events.recv(), if stream_open => match event {
                Some(event) => {
                    if matches!(event.kind, nanocodex::agent::events::AgentEventKind::AssistantMessage) {
                        #[derive(serde::Deserialize)]
                        struct Message { text: String }
                        if let Ok(message) = serde_json::from_str::<Message>(event.payload.get()) {
                            with_side(pane, |side| side.exchanges.push((false, message.text)));
                        }
                    }
                    let record = TranscriptRecord::from_agent(sequence, history::unix_ms(), event);
                    sequence += 1;
                    let _ = events.send(Event::Record { pane, record: Arc::new(record) });
                }
                None => stream_open = false,
            },
            Some(finished) = turns.join_next(), if !turns.is_empty() => {
                control = None;
                let completed = matches!(finished, Ok(Ok(_)));
                with_side(pane, |side| {
                    side.busy = false;
                    side.completed |= completed;
                });
                match finished {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        let _ = events.send(Event::Failed { pane, error: error.to_string(), opening: false });
                    }
                    Err(error) => {
                        let _ = events.send(Event::Failed { pane, error: error.to_string(), opening: false });
                    }
                }
                let _ = events.send(Event::Finished(pane));
            }
        }
    }
}

/// /collapse: hand the side thread's findings to main and close its pane.
#[derive(Default)]
pub(crate) struct LocalBtw;

impl Feature for LocalBtw {
    fn name(&self) -> &'static str {
        "btw_local"
    }

    fn command(&mut self, pane: PaneId, command: &FeatureCommand, cx: &FeatureContext<'_>) -> bool {
        if !matches!(command, FeatureCommand::Collapse) {
            return false;
        }
        match collapse_prompt() {
            Ok((side, (display, instruction))) => {
                drop(release(side.pane));
                cx.host.send(FeatureUpdate::ClosePane(side.pane));
                if cx.busy {
                    // Legacy delivers the collapse into the running main turn as a steer.
                    cx.host.send(FeatureUpdate::Steer {
                        pane: Some(PaneId::Main),
                        display,
                        instruction: Some(instruction),
                    });
                } else {
                    cx.host.send(FeatureUpdate::SubmitPrompt(FeaturePrompt {
                        pane: Some(PaneId::Main),
                        display,
                        instruction: Some(instruction),
                        completion: None,
                    }));
                }
                tokio::spawn(async move { drop(side.agent.shutdown().await) });
            }
            Err(error) => cx
                .host
                .error(Some(pane), format!("BTW was not collapsed: {error}")),
        }
        true
    }
}

fn collapse_prompt() -> Result<(Side, (String, String)), &'static str> {
    let side = active().ok_or("/collapse requires an open /btw thread")?;
    if side.busy {
        return Err("BTW has an active turn; wait for it to finish before /collapse");
    }
    if !side.completed {
        return Err("BTW needs one completed turn before /collapse");
    }
    let prompt = match side
        .agent
        .persistence()
        .and_then(|persistence| persistence.rollout)
    {
        Some(rollout) => collapse_btw_prompt(rollout.thread_id()),
        // Claude forks (and Codex without rollouts) have no session another turn
        // can read, so carry the side exchanges inline.
        None => inline_collapse_btw_prompt(&side.exchanges),
    };
    Ok((side, prompt))
}

fn collapse_btw_prompt(thread_id: &str) -> (String, String) {
    let display = format!("BTW Codex thread ID: {thread_id}");
    (
        display,
        format!(
            "The user completed a /btw side exploration in local Codex thread {thread_id}. Read that thread and incorporate its relevant findings into the main task. Use `read_session` with source `local` and session_id `{thread_id}` when available; otherwise locate the local Codex rollout by this thread ID and inspect it with local tools."
        ),
    )
}

fn inline_collapse_btw_prompt(exchanges: &[(bool, String)]) -> (String, String) {
    let first_question = exchanges
        .iter()
        .find_map(|(user, text)| user.then_some(text.trim()))
        .unwrap_or_default();
    let mut display = String::from("Collapsed /btw");
    if !first_question.is_empty() {
        display.push_str(": ");
        let mut end = first_question.len().min(120);
        while !first_question.is_char_boundary(end) {
            end -= 1;
        }
        display.push_str(&first_question[..end]);
        if end < first_question.len() {
            display.push('…');
        }
    }
    // Keep the newest exchanges when the side thread exceeds the budget.
    let mut kept = Vec::new();
    let mut used = 0;
    let mut omitted = false;
    for (user, text) in exchanges.iter().rev() {
        let mut block = format!(
            "{}:\n{}\n\n",
            if *user { "User" } else { "Assistant" },
            text.trim()
        );
        if used + block.len() > MAX_INLINE_COLLAPSE_BYTES {
            omitted = true;
            if kept.is_empty() {
                let mut end = MAX_INLINE_COLLAPSE_BYTES;
                while !block.is_char_boundary(end) {
                    end -= 1;
                }
                block.truncate(end);
                block.push_str("…\n\n");
                kept.push(block);
            }
            break;
        }
        used += block.len();
        kept.push(block);
    }
    kept.reverse();
    let mut transcript = String::new();
    if omitted {
        transcript.push_str("[Earlier BTW exchanges omitted for length.]\n\n");
    }
    for block in kept {
        transcript.push_str(&block);
    }
    (
        display,
        format!(
            "The user finished a /btw side conversation forked from this conversation. It ran separately while this thread continued, so its answers may reflect earlier context and any tool activity in it is not shown. Incorporate its relevant findings into the main task; do not repeat work it already settled unless current evidence contradicts it.\n\n<btw_conversation>\n{transcript}</btw_conversation>"
        ),
    )
}
