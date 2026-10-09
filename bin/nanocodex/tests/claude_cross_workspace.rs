//! Real mixed-family CLI sessions, Git worktrees, shell effects and child lifecycles.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

fn journey(family: &str) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .current_dir(&root)
        .arg(root.join("scripts/tests/claude-cross-workspace-cli-journey.py"))
        .args(["--binary", local_cli(), "--root-family", family])
        .output()
        .expect("Python 3 is required for the synthetic HTTP providers");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn claude_root_mixed_descendants_snapshot_current_worktree() {
    journey("claude");
}

#[test]
fn codex_root_mixed_descendants_snapshot_current_worktree() {
    journey("codex");
}
