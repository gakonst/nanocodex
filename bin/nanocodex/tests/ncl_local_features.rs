//! Shipped ncl over a PTY: /mcp reload|login, /benchmark and local Realtime /voice.
#![cfg(unix)]
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[test]
fn native_local_features_cli_journey() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .current_dir(&root)
        .arg(root.join("scripts/tests/ncl-local-features-journey.py"))
        .args(["--binary", local_cli()])
        .output()
        .expect("Python 3 is required for the native CLI journey");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}
