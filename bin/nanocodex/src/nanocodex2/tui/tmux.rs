//! Bounded, expiring metadata for the optional tmux overview. Never reads pane output.
use std::{process::Stdio, time::Duration};
use tokio::{process::Command, task::JoinSet};

pub(super) struct Publisher {
    pane: String,
    pending: JoinSet<()>,
}

impl Publisher {
    pub(super) fn new() -> Option<Self> {
        std::env::var("TMUX").ok()?;
        let pane = std::env::var("TMUX_PANE").ok()?;
        if !pane
            .strip_prefix('%')
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        Some(Self {
            pane,
            pending: JoinSet::new(),
        })
    }

    pub(super) fn publish(&mut self, agent: &str, status: &str, prompt: &str, prompt_at: u64) {
        while self.pending.try_join_next().is_some() {}
        if !self.pending.is_empty() {
            return;
        }
        let metadata = serde_json::json!({
            "version": 1, "agent_id": agent, "status": status,
            "prompt": prompt.chars().take(512).collect::<String>(),
            "updated_at": super::unix_ms(), "prompt_at": prompt_at,
        })
        .to_string();
        let mut command = Command::new("tmux");
        command
            .args([
                "set-option",
                "-p",
                "-t",
                &self.pane,
                "@nanocodex-overview",
                &metadata,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // Never await optional metadata on the input loop. One owned task bounds
        // outstanding work; dropping the publisher aborts it and kills its child.
        self.pending.spawn(async move {
            let _ = tokio::time::timeout(Duration::from_millis(250), command.status()).await;
        });
    }
}
// Records expire after 10 seconds in the reader, including crashes and SIGKILL.
// They live only on the pane and disappear when tmux destroys it.

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pending_publication_is_bounded_and_cancelled_with_its_owner() {
        let mut publisher = Publisher {
            pane: "%0".into(),
            pending: JoinSet::new(),
        };
        let (started, ready) = tokio::sync::oneshot::channel();
        let (cancelled, finished) = tokio::sync::oneshot::channel::<()>();
        publisher.pending.spawn(async move {
            let _ = started.send(());
            std::future::pending::<()>().await;
            drop(cancelled);
        });
        ready.await.unwrap();
        publisher.publish("agent", "idle", "first", 0);
        publisher.publish("agent", "idle", "latest", 1);
        assert_eq!(publisher.pending.len(), 1);

        drop(publisher);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), finished)
                .await
                .expect("dropping the publisher must cancel its work")
                .is_err()
        );
    }
}
