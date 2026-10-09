//! Real ncl over a PTY with real Code Mode and child registry; only Messages inference is synthetic.
#![cfg(unix)]
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[test]
fn ncl_local_subagents_limit_tree_and_parent_continuation() {
    let repository = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(repository.join("scripts/tests/ncl-local-subagents-journey.py"))
        .args(["--binary", local_cli()])
        .current_dir(&repository)
        .output()
        .expect("Python 3 is required for the ncl local subagents journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
