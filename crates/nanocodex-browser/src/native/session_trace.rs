use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::{
    BrowserAction, BrowserActionResult, BrowserConsoleEntry, BrowserNetworkRequest,
    BrowserPageError, BrowserSessionTrace, BrowserSessionTraceEntry, BrowserSessionTraceOutcome,
};

use super::{BrowserError, artifacts, capture_dom_snapshot};

pub(super) struct SessionTraceState {
    started: Instant,
    directory: PathBuf,
    events_path: PathBuf,
    screenshots: bool,
    dom_snapshots: bool,
    max_actions: usize,
    action_count: usize,
    screenshot_count: usize,
    dom_snapshot_count: usize,
    truncated: bool,
}

impl SessionTraceState {
    pub(super) async fn start(
        output_dir: &Path,
        sequence: u64,
        screenshots: bool,
        dom_snapshots: bool,
        max_actions: usize,
    ) -> Result<Self, BrowserError> {
        let directory = output_dir.join(format!("session-trace-{sequence}"));
        tokio::fs::create_dir_all(&directory).await?;
        let events_path = directory.join("events.jsonl");
        tokio::fs::write(&events_path, []).await?;
        Ok(Self {
            started: Instant::now(),
            directory,
            events_path,
            screenshots,
            dom_snapshots,
            max_actions,
            action_count: 0,
            screenshot_count: 0,
            dom_snapshot_count: 0,
            truncated: false,
        })
    }

    pub(super) async fn record_success(
        &mut self,
        page: &chromiumoxide::Page,
        sequence: u64,
        duration: Duration,
        action: BrowserAction,
        result: BrowserActionResult,
    ) -> Result<(), BrowserError> {
        if !self.reserve_entry() {
            return Ok(());
        }
        let mut capture_errors = Vec::new();
        let screenshot_path = if self.screenshots {
            match artifacts::capture(
                page,
                &self.directory,
                format!("action-{sequence:06}"),
                false,
                None,
            )
            .await
            {
                Ok(image) => {
                    self.screenshot_count = self.screenshot_count.saturating_add(1);
                    Some(image.path)
                }
                Err(error) => {
                    capture_errors.push(format!("screenshot: {error}"));
                    None
                }
            }
        } else {
            None
        };
        let dom_snapshot_path = if self.dom_snapshots {
            match capture_dom_snapshot(page, Vec::new(), true, true).await {
                Ok(snapshot) => {
                    let path = self.directory.join(format!("dom-{sequence:06}.json"));
                    match serde_json::to_vec(&snapshot) {
                        Ok(encoded) => match tokio::fs::write(&path, encoded).await {
                            Ok(()) => {
                                self.dom_snapshot_count = self.dom_snapshot_count.saturating_add(1);
                                Some(path)
                            }
                            Err(error) => {
                                capture_errors.push(format!("DOM snapshot write: {error}"));
                                None
                            }
                        },
                        Err(error) => {
                            capture_errors.push(format!("DOM snapshot encode: {error}"));
                            None
                        }
                    }
                }
                Err(error) => {
                    capture_errors.push(format!("DOM snapshot: {error}"));
                    None
                }
            }
        } else {
            None
        };
        self.append(BrowserSessionTraceEntry {
            sequence,
            elapsed_ms: duration_millis(self.started.elapsed()),
            duration_ms: duration_millis(duration),
            action,
            outcome: BrowserSessionTraceOutcome::Success {
                result: Box::new(result),
            },
            screenshot_path,
            dom_snapshot_path,
            capture_errors,
        })
        .await
    }

    pub(super) async fn record_failure(
        &mut self,
        sequence: u64,
        duration: Duration,
        action: BrowserAction,
        error: String,
    ) -> Result<(), BrowserError> {
        if !self.reserve_entry() {
            return Ok(());
        }
        self.append(BrowserSessionTraceEntry {
            sequence,
            elapsed_ms: duration_millis(self.started.elapsed()),
            duration_ms: duration_millis(duration),
            action,
            outcome: BrowserSessionTraceOutcome::Failure { error },
            screenshot_path: None,
            dom_snapshot_path: None,
            capture_errors: Vec::new(),
        })
        .await
    }

    pub(super) async fn stop(
        self,
        requests: Vec<BrowserNetworkRequest>,
        console: Vec<BrowserConsoleEntry>,
        errors: Vec<BrowserPageError>,
        dropped_console: u64,
        dropped_errors: u64,
        dropped_requests: u64,
    ) -> Result<BrowserSessionTrace, BrowserError> {
        let network_path = self.directory.join("network.json");
        let diagnostics_path = self.directory.join("diagnostics.json");
        tokio::fs::write(&network_path, serde_json::to_vec(&requests)?).await?;
        tokio::fs::write(
            &diagnostics_path,
            serde_json::to_vec(&TraceDiagnostics {
                console: &console,
                errors: &errors,
                dropped_console,
                dropped_errors,
                dropped_requests,
            })?,
        )
        .await?;
        Ok(BrowserSessionTrace {
            directory: self.directory,
            events_path: self.events_path,
            network_path,
            diagnostics_path,
            action_count: self.action_count,
            screenshot_count: self.screenshot_count,
            dom_snapshot_count: self.dom_snapshot_count,
            duration_ms: duration_millis(self.started.elapsed()),
            truncated: self.truncated,
        })
    }

    const fn reserve_entry(&mut self) -> bool {
        if self.action_count >= self.max_actions {
            self.truncated = true;
            return false;
        }
        self.action_count = self.action_count.saturating_add(1);
        true
    }

    async fn append(&self, entry: BrowserSessionTraceEntry) -> Result<(), BrowserError> {
        let mut encoded = serde_json::to_vec(&entry)?;
        encoded.push(b'\n');
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&self.events_path)
            .await?;
        file.write_all(&encoded).await?;
        file.flush().await?;
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceDiagnostics<'a> {
    console: &'a [BrowserConsoleEntry],
    errors: &'a [BrowserPageError],
    dropped_console: u64,
    dropped_errors: u64,
    dropped_requests: u64,
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
