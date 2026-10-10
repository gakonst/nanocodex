//! Local /split: hand the open /btw thread to a sibling terminal pane running
//! `nanocodex resume <thread>` (tmux, Zellij, WezTerm, iTerm, or the platform
//! terminal). The terminal detection and
//! launch commands live in [`super::split_launch`].

use super::{
    Feature, FeatureCommand, FeatureContext, FeatureUpdate, btw_local, split_launch::PreparedSplit,
};
use crate::nanocodex2::tui::pane::PaneId;

#[derive(Default)]
pub(crate) struct Split;

impl Feature for Split {
    fn name(&self) -> &'static str {
        "split"
    }

    fn command(&mut self, pane: PaneId, command: &FeatureCommand, cx: &FeatureContext<'_>) -> bool {
        if !matches!(command, FeatureCommand::Split) {
            return false;
        }
        match prepare(cx) {
            Err(error) => cx.host.error(Some(pane), error),
            Ok((side, thread_id, prepared)) => {
                drop(btw_local::release(side.pane));
                cx.host.send(FeatureUpdate::ClosePane(side.pane));
                let host = cx.host.clone();
                tokio::spawn(async move {
                    // Flush the side thread's rollout before another process resumes it.
                    if let Err(error) = side.agent.shutdown().await {
                        host.error(
                            None,
                            format!(
                                "failed to shut down BTW thread {thread_id}: {error}; try `nanocodex resume {thread_id}` manually"
                            ),
                        );
                        return;
                    }
                    let thread = thread_id.clone();
                    match tokio::task::spawn_blocking(move || prepared.launch(&thread)).await {
                        Ok(Ok(destination)) => {
                            host.notice(None, format!("BTW thread {thread_id} opened in {destination}"));
                        }
                        Ok(Err(error)) => host.error(
                            None,
                            format!(
                                "{error}; thread {thread_id} is saved - run `nanocodex resume {thread_id}` manually"
                            ),
                        ),
                        Err(error) => host.error(None, format!("/split failed: {error}")),
                    }
                });
            }
        }
        true
    }
}

fn prepare(cx: &FeatureContext<'_>) -> Result<(btw_local::Side, String, PreparedSplit), String> {
    let side = btw_local::active().ok_or("/split requires an open /btw thread")?;
    if side.busy {
        return Err("BTW has an active turn; wait for it to finish before /split".to_owned());
    }
    if !side.completed {
        return Err(
            "BTW needs one completed turn before it can be resumed in another terminal".to_owned(),
        );
    }
    // Forks without a rollout keep their history in this process only.
    let thread_id = side
        .agent
        .persistence().and_then(|persistence| persistence.rollout)
        .map(|rollout| rollout.thread_id().to_owned())
        .ok_or("/split needs a resumable session, but this BTW is not saved to disk; use /collapse to bring it into main")?;
    let prepared = PreparedSplit::detect(cx.workspace).map_err(|error| error.to_string())?;
    Ok((side, thread_id, prepared))
}
