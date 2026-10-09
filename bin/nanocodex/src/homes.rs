//! One shared set of instructions and skills for the Codex and Claude homes.
//!
//! `CODEX_HOME` is canonical; Claude's natural paths become links to it so both
//! harnesses read the same files. Linking never overwrites or deletes and never
//! fails startup.
use nanocodex_home::{AgentHome, LinkAction, LinkMode, LinkOutcome, LinkReport};

fn report(mode: LinkMode) -> Result<LinkReport, String> {
    AgentHome::from_env()
        .map(|home| home.link_natural_paths(mode))
        .map_err(|error| error.to_string())
}

/// Human-readable outcome of one linked path.
pub(crate) fn describe(action: &LinkAction) -> String {
    let link = action.link.display();
    let target = action.target.as_deref().map_or_else(
        || "(no shared source)".to_owned(),
        |path| path.display().to_string(),
    );
    match &action.outcome {
        LinkOutcome::Created => format!("linked {link} -> {target}"),
        LinkOutcome::WouldCreate => format!("would link {link} -> {target}"),
        LinkOutcome::AlreadyLinked => format!("already linked {link} -> {target}"),
        LinkOutcome::BothExist => format!("both exist, left unchanged: {link} and {target}"),
        LinkOutcome::DanglingLink => format!("dangling link: {link}"),
        LinkOutcome::Blocked { reason } => format!("blocked {link}: {reason}"),
        LinkOutcome::Unsupported => format!("linking unsupported on this platform: {link}"),
        LinkOutcome::Failed { error } => format!("failed to link {link}: {error}"),
    }
}

/// Links natural paths before a session starts; problems are only logged.
pub(crate) fn link_at_startup() {
    match report(LinkMode::Apply) {
        Ok(report) => {
            for action in report.changes() {
                tracing::info!(link = %action.link.display(), "{}", describe(action));
            }
            for action in report.problems() {
                tracing::warn!(link = %action.link.display(), "{}", describe(action));
            }
        }
        Err(error) => tracing::warn!("agent homes were not linked: {error}"),
    }
}

/// Links (or previews) natural paths and prints every considered path.
pub(crate) fn link_for_setup(dry_run: bool) {
    let mode = if dry_run {
        LinkMode::DryRun
    } else {
        LinkMode::Apply
    };
    match report(mode) {
        Ok(report) => {
            for action in &report.actions {
                eprintln!("• {}", describe(action));
            }
        }
        Err(error) => eprintln!("• Agent homes were not linked: {error}"),
    }
}
