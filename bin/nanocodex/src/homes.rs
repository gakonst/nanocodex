//! The agent homes every builder receives, and the optional natural-path links.
//!
//! `CODEX_HOME` (or `~/.codex`) is canonical and owns sessions; Claude's
//! `CLAUDE_CONFIG_DIR` (or `~/.claude`) is an alias home. Builders of both
//! families resolve user instructions, skills and agent profiles over the
//! union of both homes, so neither depends on links. Linking only makes the
//! same files visible at each family's natural path for the model's own tool
//! calls; it never overwrites or deletes and never fails startup.
use clap::Args;
use eyre::{Result, WrapErr as _};
use nanocodex::claude_tools::{ClaudeAgentProfiles, ClaudeProjectContext, ClaudeSkills};
use nanocodex_home::{AgentHome, LinkAction, LinkMode, LinkOutcome, LinkReport};
use std::path::Path;

/// The homes of this process, resolved like `crate::config::default_codex_home`.
///
/// Unresolvable homes (no `HOME` and no explicit variables) leave builders
/// with project-only context instead of failing a session.
pub(crate) fn resolve() -> Option<AgentHome> {
    AgentHome::from_env()
        .inspect_err(|error| tracing::debug!("agent homes are unavailable: {error}"))
        .ok()
}

/// Workspace skills plus the user skills of both homes.
pub(crate) fn skills(workspace: &Path) -> Result<ClaudeSkills, String> {
    let skills = ClaudeSkills::new(workspace)?;
    Ok(match resolve() {
        Some(home) => skills.with_user_roots(home.skill_roots()),
        None => skills,
    })
}

/// Workspace agent profiles plus the user profiles of both homes.
pub(crate) fn agent_profiles(workspace: &Path) -> Result<ClaudeAgentProfiles, String> {
    let profiles = ClaudeAgentProfiles::new(workspace)?;
    Ok(match resolve() {
        Some(home) => profiles.with_user_roots(home.agent_profile_roots()),
        None => profiles,
    })
}

/// Workspace context preceded by the user instructions of both homes.
pub(crate) fn project_context(workspace: &Path) -> Result<ClaudeProjectContext, String> {
    let context = ClaudeProjectContext::new(workspace)?;
    Ok(match resolve() {
        Some(home) => context.with_global_instructions(home.global_instructions()),
        None => context,
    })
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

fn report(mode: LinkMode) -> Result<LinkReport, String> {
    AgentHome::from_env()
        .map(|home| home.link_natural_paths(mode))
        .map_err(|error| error.to_string())
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

/// Links (or previews) natural paths during setup and prints every path.
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

/// `nanocodex homes`: show both homes and preview or apply the natural-path
/// links, without any other setup work.
#[derive(Args)]
pub(crate) struct Homes {
    /// Create the missing links. Without it, only preview them.
    #[arg(long)]
    apply: bool,

    /// Print the resolved homes and link report as JSON.
    #[arg(long)]
    json: bool,
}

impl Homes {
    pub(crate) fn run(self) -> Result<()> {
        let home = AgentHome::from_env().wrap_err("agent homes are unavailable")?;
        let mode = if self.apply {
            LinkMode::Apply
        } else {
            LinkMode::DryRun
        };
        let report = home.link_natural_paths(mode);
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "codex_home": home.codex_home(),
                    "claude_home": home.claude_home(),
                    "report": report,
                }))?
            );
        } else {
            println!("Codex home:  {}", home.codex_home().display());
            println!("Claude home: {}", home.claude_home().display());
            if report.actions.is_empty() {
                println!("Nothing to link: neither home has shared instructions or skills.");
            }
            for action in &report.actions {
                println!("• {}", describe(action));
            }
            if !self.apply && report.changes().next().is_some() {
                println!("Run `nanocodex homes --apply` to create these links.");
            }
        }
        if report.problems().next().is_some() {
            return Err(eyre::eyre!(
                "some natural paths need attention; nothing was overwritten"
            ));
        }
        Ok(())
    }
}
