//! Shared Codex rollout discovery through the shipped terminal picker.
#[test]
fn codex_resume_picker_journey() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .current_dir(&root)
        .arg(root.join("scripts/tests/codex-resume-picker-journey.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_nanocodex")])
        .output()
        .expect("Python 3 is required for the resume picker journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
