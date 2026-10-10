//! Prints the shared-context view and natural-path link plan for the current
//! environment (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `HOME`) as JSON.
//!
//! ```sh
//! cargo run -p nanocodex-home --example natural_paths -- [--apply] [WORKSPACE]
//! ```
//!
//! Without `--apply` this is a dry run and changes nothing.

use nanocodex_home::{AgentHome, LinkMode};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut mode = LinkMode::DryRun;
    let mut workspace = std::env::current_dir()?;
    for arg in std::env::args_os().skip(1) {
        if arg == "--apply" {
            mode = LinkMode::Apply;
        } else {
            workspace = arg.into();
        }
    }
    let home = AgentHome::from_env()?;
    let project = home.project(&workspace);
    let output = serde_json::json!({
        "home": home,
        "link": home.link_natural_paths(mode),
        "global_instructions": home.global_instructions(),
        "project_instructions": project.instruction_files().0,
        "skills": home.skills_for(&project),
        "agent_profiles": home.agent_profiles_for(&project),
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
