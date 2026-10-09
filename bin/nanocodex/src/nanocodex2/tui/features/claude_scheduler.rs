//! Claude SessionScheduler pump: cron/loop wakeups and Monitor events.
//!
//! Port of the legacy cron_tick arm. A due prompt fires only from
//! [Feature::idle_tick], which the driver calls while the main agent is idle,
//! no feature overlay (Claude interaction) is open and the composer is empty;
//! user input and queued turns always win. Scheduled prompts are resolved by
//! claude_frontend::automatic (skills, maintenance loop.md) while the
//! transcript shows "[Scheduled id] prompt". Dynamic /loop iterations carry a
//! token that is finished when that exact turn ends. Esc cancels a pending
//! dynamic wakeup (fixed cron schedules are kept).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crossterm::event::{KeyCode, KeyEvent};
use tokio::sync::oneshot;

use super::{Feature, FeatureContext, FeatureHost, FeaturePrompt, FeatureUpdate, KeyOutcome};
use crate::config::{SessionScheduler, claude_frontend};
use crate::nanocodex2::tui::{local::agent::LocalParts, pane::PaneId};

#[derive(Default)]
pub(crate) struct ClaudeScheduler {
    scheduler: Option<Arc<SessionScheduler>>,
    /// A fired prompt has not finished yet; nothing else fires meanwhile.
    in_flight: Arc<AtomicBool>,
}

impl Feature for ClaudeScheduler {
    fn name(&self) -> &'static str {
        "claude_scheduler"
    }

    fn attach(&mut self, parts: &mut LocalParts, _cx: &FeatureContext<'_>) {
        self.scheduler = parts.claude_scheduler.clone();
        self.in_flight = Arc::default();
    }

    fn idle_tick(&mut self, cx: &FeatureContext<'_>) {
        let (Some(scheduler), Some(agent)) = (&self.scheduler, cx.agent) else {
            return;
        };
        if cx.busy || self.in_flight.load(Ordering::Acquire) {
            return;
        }
        let session = agent.session_id().to_owned();
        let due = match scheduler.take_due(&session) {
            Ok(Some(due)) => due,
            Ok(None) => return,
            Err(error) => {
                cx.host.error(
                    Some(PaneId::Main),
                    format!(
                        "Scheduler paused: {error}. Reopen the session after fixing its journal."
                    ),
                );
                self.scheduler = None;
                return;
            }
        };
        let monitor = due.id.starts_with("monitor-");
        let source = if monitor { "Monitor" } else { "Scheduled" };
        let display = format!("[{source} {}] {}", due.id, due.prompt);
        let instruction = if monitor {
            Ok(due.prompt)
        } else {
            claude_frontend::automatic(&session, &due.prompt)
        };
        let instruction = match instruction {
            Ok(instruction) => instruction,
            Err(error) => {
                if let Some(token) = &due.iteration_token {
                    drop(claude_frontend::finish(&session, token, false));
                }
                cx.host.error(
                    Some(PaneId::Main),
                    format!("Scheduled prompt withheld: {error}"),
                );
                return;
            }
        };
        self.in_flight.store(true, Ordering::Release);
        let completion = track(
            cx.host.clone(),
            session,
            due.iteration_token,
            Some(Arc::clone(&self.in_flight)),
        );
        cx.host.send(FeatureUpdate::SubmitPrompt(FeaturePrompt {
            pane: Some(PaneId::Main),
            display,
            instruction: Some(instruction),
            completion: Some(completion),
        }));
    }

    fn user_prompt(
        &mut self,
        text: &str,
        cx: &FeatureContext<'_>,
    ) -> Result<Option<oneshot::Sender<bool>>, String> {
        let Some(agent) = cx.agent else {
            return Ok(None);
        };
        if agent.harness_family() != nanocodex::HarnessFamily::Claude {
            return Ok(None);
        }
        let session = agent.session_id().to_owned();
        let Some(token) = claude_frontend::begin_user_iteration(&session, text)? else {
            return Ok(None);
        };
        Ok(Some(track(cx.host.clone(), session, Some(token), None)))
    }

    fn key(&mut self, key: &KeyEvent, cx: &FeatureContext<'_>) -> KeyOutcome {
        if key.code == KeyCode::Esc
            && let (Some(scheduler), Some(agent)) = (&self.scheduler, cx.agent)
            && let Err(error) = scheduler.stop_wakeup(agent.session_id())
        {
            cx.host
                .error(Some(PaneId::Main), format!("Cannot cancel wakeup: {error}"));
        }
        // Esc keeps its normal meaning (cancel/clear).
        KeyOutcome::Ignored
    }

    fn shutdown(&mut self) {
        self.scheduler = None;
    }
}

/// Returns the completion sender the driver resolves when the turn ends and
/// finishes the loop iteration with that outcome.
fn track(
    host: FeatureHost,
    session: String,
    token: Option<String>,
    in_flight: Option<Arc<AtomicBool>>,
) -> oneshot::Sender<bool> {
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        // A dropped sender means the turn was rejected before it started.
        let completed = receiver.await.unwrap_or(false);
        if let Some(token) = token
            && let Err(error) = claude_frontend::finish(&session, &token, completed)
        {
            host.error(Some(PaneId::Main), format!("loop: {error}"));
        }
        if let Some(in_flight) = in_flight {
            in_flight.store(false, Ordering::Release);
        }
    });
    sender
}
