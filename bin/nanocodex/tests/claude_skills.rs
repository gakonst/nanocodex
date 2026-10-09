//! Actual CLI + HTTP/SSE journey; all skill and filesystem behavior is real.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[test]
fn cli_skills_and_scoped_context() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/tests/claude-skills-cli-journey.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg("--binary")
        .arg(local_cli())
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .expect("python3 is required for the CLI HTTP/SSE fixture");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_named_profiles_forked_skills_and_child_isolation() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/tests/claude-profiles-cli-journey.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg("--binary")
        .arg(local_cli())
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .expect("python3 is required for the CLI HTTP/SSE fixture");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
