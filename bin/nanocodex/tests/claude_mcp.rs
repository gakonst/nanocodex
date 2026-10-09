//! Shipped CLI with synthetic Messages and real external HTTP + stdio MCP servers.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[test]
fn native_mcp_discovery_media_resources_removal_and_replay() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/tests/claude-mcp-cli-journey.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg("--binary")
        .arg(local_cli())
        .output()
        .expect("run Python CLI journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
