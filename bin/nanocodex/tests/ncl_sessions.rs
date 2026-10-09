//! Local sessions, branches and side threads through the shipped CLI in tmux.
#![cfg(target_os = "linux")]
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[test]
fn ncl_sessions_journey() {
    assert!(
        std::process::Command::new("tmux")
            .arg("-V")
            .status()
            .expect("tmux is required for the local sessions journey")
            .success(),
        "tmux must be runnable for the local sessions journey"
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .current_dir(&root)
        .arg(root.join("scripts/tests/ncl-sessions-journey.py"))
        .args(["--binary", local_cli()])
        .output()
        .expect("Python 3 is required for the ncl sessions journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
