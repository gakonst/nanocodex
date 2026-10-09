//! Actual CLI process crash, Messages HTTP, and SQLite media recovery journey.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[test]
fn cli_frozen_image_survives_crash_and_deleted_source() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/tests/claude-prompt-image-journey.py");
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
