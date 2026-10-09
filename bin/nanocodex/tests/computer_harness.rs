//! Default Code Mode CUA journey across both CLIs; inference and external MCP are synthetic.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[test]
fn shared_computer_catalog_dispatch_and_permissions_across_harnesses() {
    let repository = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(repository.join("scripts/tests/computer-harness-cli-journey.py"))
        .args(["--binary", local_cli()])
        .current_dir(&repository)
        .output()
        .expect("run computer harness CLI journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
