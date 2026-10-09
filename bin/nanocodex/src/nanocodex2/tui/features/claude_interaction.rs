//! Claude host interactions: AskUserQuestion, ExitPlanMode plan approval
//! and tool permission asks, shown as a modal [InteractionOverlay].
//!
//! One request is shown at a time; the next is received only after
//! the current one is answered, cancelled or withdrawn by its producer. While
//! the overlay is open the Claude scheduler does not fire (overlay_state).

use std::{
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tokio::{sync::oneshot, task::JoinHandle};

use super::{Feature, FeatureContext, FeatureHost, FeatureUpdate};
use crate::config::{InteractionReceiver, PendingInteraction};
use crate::nanocodex2::tui::{
    components::{InteractionOutcome, InteractionOverlay},
    local::agent::LocalParts,
};

/// Producer withdrawal (turn cancelled, permission timeout) is observed at this cadence.
const WITHDRAW_POLL: Duration = Duration::from_millis(200);

#[derive(Default)]
pub(crate) struct ClaudeInteraction {
    task: Option<JoinHandle<()>>,
}

impl Feature for ClaudeInteraction {
    fn name(&self) -> &'static str {
        "claude_interaction"
    }

    fn attach(&mut self, parts: &mut LocalParts, cx: &FeatureContext<'_>) {
        self.shutdown();
        if let Some(receiver) = parts.claude_interactions.take() {
            self.task = Some(tokio::spawn(serve(receiver, cx.host.clone())));
        }
    }

    fn shutdown(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn serve(mut receiver: InteractionReceiver, host: FeatureHost) {
    while let Some(request) = receiver.recv().await {
        if request.is_closed() {
            continue;
        }
        show(request, &host).await;
    }
}

/// Opens one request and waits until it leaves the overlay.
async fn show(request: PendingInteraction, host: &FeatureHost) {
    let request = Arc::new(Mutex::new(request));
    let (finished, mut outcome) = oneshot::channel();
    let overlay = InteractionOverlay::new(Arc::clone(&request), move |outcome| {
        drop(finished.send(outcome));
    });
    host.notice(None, "Claude is waiting for your answer");
    host.send(FeatureUpdate::OpenOverlay(Box::new(overlay)));
    let outcome = loop {
        tokio::select! {
            biased;
            outcome = &mut outcome => break outcome.ok(),
            () = tokio::time::sleep(WITHDRAW_POLL) => {
                if request.lock().unwrap_or_else(PoisonError::into_inner).is_closed() {
                    // An answer closes the request just before it reports.
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    if let Ok(outcome) = outcome.try_recv() {
                        break Some(outcome);
                    }
                    host.send(FeatureUpdate::CloseOverlay);
                    break Some(InteractionOutcome::Withdrawn);
                }
            }
        }
    };
    match outcome {
        Some(InteractionOutcome::Answered(answer)) => {
            host.notice(None, format!("Answered: {answer}"))
        }
        Some(InteractionOutcome::Cancelled) => host.notice(None, "Request cancelled"),
        Some(InteractionOutcome::Withdrawn) => {
            host.error(None, "The request was cancelled; no answer was sent");
        }
        // The overlay was replaced by another feature overlay; dropping the
        // request returns a cancellation to the tool instead of an answer.
        None => host.error(
            None,
            "Claude request dismissed; the tool was told it was cancelled",
        ),
    }
}
