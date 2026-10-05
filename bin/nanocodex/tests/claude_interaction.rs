//! Real shipped CLI over PTY/HTTP with persisted native state.
#[test]
fn native_interaction_cli_journey() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .current_dir(&root)
        .arg(root.join("scripts/tests/claude-interaction-cli-journey.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_nanocodex")])
        .output()
        .expect("Python 3 is required for the native CLI journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
