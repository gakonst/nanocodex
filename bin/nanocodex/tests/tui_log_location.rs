//! Persistent user log state through the shipped terminal, independent of cwd.
#[test]
fn tui_log_location_journey() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .current_dir(&root)
        .arg(root.join("scripts/tests/tui-log-location-journey.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_nanocodex")])
        .output()
        .expect("Python 3 is required for the TUI log-location journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
