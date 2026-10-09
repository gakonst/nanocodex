//! /benchmark [profile]: asks the local agent to run the repository's
//! benchmark workflow. Port of the legacy classify_submission arm: the
//! transcript shows the typed command while the agent receives the private
//! workflow instruction from [crate::benchmark::prompt].

use super::{Feature, FeatureCommand, FeatureContext, FeaturePrompt, FeatureUpdate};
use crate::nanocodex2::tui::pane::PaneId;

#[derive(Default)]
pub(crate) struct Benchmark;

impl Feature for Benchmark {
    fn name(&self) -> &'static str {
        "benchmark"
    }

    fn command(&mut self, pane: PaneId, command: &FeatureCommand, cx: &FeatureContext<'_>) -> bool {
        let FeatureCommand::Benchmark(arguments) = command else {
            return false;
        };
        let profile = Some(arguments.trim()).filter(|profile| !profile.is_empty());
        if profile.is_some_and(|profile| profile.split_whitespace().count() != 1) {
            cx.host.error(Some(pane), "Usage: /benchmark [profile]");
            return true;
        }
        if cx.agent.is_none() {
            cx.host.error(
                Some(pane),
                "Wait for the local agent to start before /benchmark",
            );
            return true;
        }
        let executable = std::env::current_exe().ok();
        let instruction = crate::benchmark::prompt(
            profile,
            std::path::Path::new("nanocodex.toml"),
            None,
            None,
            executable.as_deref(),
        );
        let display = profile.map_or_else(
            || "/benchmark".to_owned(),
            |profile| format!("/benchmark {profile}"),
        );
        cx.host.send(FeatureUpdate::SubmitPrompt(FeaturePrompt {
            pane: Some(pane),
            display,
            instruction: Some(instruction),
            completion: None,
        }));
        true
    }
}
