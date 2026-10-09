//! Harness and model switch before the first prompt.
//!
//! Port of the legacy worker's change_model/ReplaceBackend path: until the
//! first prompt of a fresh (non-resumed) local session, any /model selection
//! rebuilds the local agent with the selected harness model, so switching
//! between Codex and Claude starts the matching backend (and its Claude host
//! tools, scheduler and auth checks). After the first prompt, or for resumed
//! sessions, the model is fixed and the selection is rejected.

use super::{Feature, FeatureCommand, FeatureContext, FeatureUpdate};
use crate::nanocodex2::tui::pane::PaneId;

#[derive(Default)]
pub(crate) struct Harness;

impl Feature for Harness {
    fn name(&self) -> &'static str {
        "harness"
    }

    fn command(&mut self, pane: PaneId, command: &FeatureCommand, cx: &FeatureContext<'_>) -> bool {
        let FeatureCommand::SwitchModel(id) = command else {
            return false;
        };
        let Some(launch) = cx.launch else {
            return false;
        };
        if cx.prompted || cx.busy || !launch.replaceable {
            cx.host.error(
                Some(pane),
                "The model can only be changed before the first prompt of a new local thread",
            );
            return true;
        }
        let model = match id.parse::<nanocodex::HarnessModel>() {
            Ok(model) => model,
            Err(error) => {
                cx.host
                    .error(Some(pane), format!("Unsupported model {id}: {error}"));
                return true;
            }
        };
        let mut next = launch.clone();
        let thinking = next.args.thinking();
        let fast_mode = next.args.fast_mode();
        next.args.select_tui_model(model, thinking, fast_mode);
        let family = model.family();
        cx.host.notice(
            Some(pane),
            format!("Initializing {model} ({family} harness)"),
        );
        cx.host.send(FeatureUpdate::Relaunch(Box::new(next)));
        true
    }
}
