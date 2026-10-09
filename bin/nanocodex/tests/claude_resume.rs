//! Real shipped CLI over PTY/HTTP with persisted native state.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[test]
fn native_resume_cli_journey() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .current_dir(&root)
        .arg(root.join("scripts/tests/claude-resume-cli-journey.py"))
        .args(["--binary", local_cli()])
        .output()
        .expect("Python 3 is required for the native CLI journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
