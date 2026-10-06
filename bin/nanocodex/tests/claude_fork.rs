//! Shipped CLI and actual loopback Messages transport; only inference is synthetic.
#[test]
fn native_cli_fork_preserves_full_history_and_isolates_child_lifecycle() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("scripts/tests/claude-fork-cli-journey.py"))
        .arg("--binary")
        .arg(env!("CARGO_BIN_EXE_nanocodex"))
        .current_dir(&root)
        .output()
        .expect("python3 executes the shipped CLI fork acceptance journey");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}
