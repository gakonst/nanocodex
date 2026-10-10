//! Session identity (CODEX_THREAD_ID, NANOCODEX_ROOT_SESSION_ID) in shell, MCP and
//! hook processes of root, child and branch sessions, through the shipped CLI.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[test]
fn session_identity_cli_journey() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .current_dir(&root)
        .arg(root.join("scripts/tests/session-identity-cli-journey.py"))
        .args(["--binary", local_cli()])
        .output()
        .expect("Python 3 is required for the session identity journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
