//! Bounded, expiring metadata for the optional tmux overview. Never reads pane output.
use std::{process::Stdio, time::Duration};
use tokio::process::Command;

pub(super) struct Publisher {
    pane: String,
}

impl Publisher {
    pub(super) fn new() -> Option<Self> {
        std::env::var("TMUX").ok()?;
        let pane = std::env::var("TMUX_PANE").ok()?;
        if !pane.strip_prefix('%').is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())) {
            return None;
        }
        Some(Self { pane })
    }

    pub(super) async fn publish(&mut self, agent: &str, status: &str, prompt: &str) {
        let metadata = serde_json::json!({
            "version": 1, "agent_id": agent, "status": status,
            "prompt": prompt.chars().take(512).collect::<String>(),
            "updated_at": super::unix_ms(),
        }).to_string();
        let mut command = Command::new("tmux");
        command.args(["set-option", "-p", "-t", &self.pane, "@nanocodex-overview", &metadata])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        // A missing or unresponsive tmux server must not prevent terminal use.
        let _ = tokio::time::timeout(Duration::from_millis(250), command.status()).await;
    }
}
// Records expire after 10 seconds in the reader, including crashes and SIGKILL.
// They live only on the pane and disappear when tmux destroys it.
