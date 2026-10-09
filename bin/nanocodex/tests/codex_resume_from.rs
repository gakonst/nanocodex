//! Start Codex threads from a copied rollout and an earlier turn through the shipped CLI.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[test]
fn codex_resume_from_journey() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .current_dir(&root)
        .arg(root.join("scripts/tests/codex-resume-from-journey.py"))
        .args(["--binary", local_cli()])
        .output()
        .expect("Python 3 is required for the resume-from journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
