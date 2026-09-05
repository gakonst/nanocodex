// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Managed-agent projections consumed by Tact's session pickers.

use crate::config::{ReasoningEffort, ReasoningMode};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SessionSummary {
    pub(crate) session_id: String,
    pub(crate) started_at_unix_ms: u64,
    pub(crate) model: String,
    pub(crate) effort: ReasoningEffort,
    pub(crate) reasoning_mode: ReasoningMode,
    pub(crate) workspace: PathBuf,
    pub(crate) preview: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct RecentPrompt {
    pub(crate) text: String,
    pub(crate) recorded_at_unix_ms: u64,
    pub(crate) session_id: String,
    pub(crate) workspace: PathBuf,
}

pub(crate) fn format_age(started_at_unix_ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let started = Duration::from_millis(started_at_unix_ms);
    let age = now.saturating_sub(started);
    if age.as_secs() < 60 {
        return "now".to_owned();
    }
    if age.as_secs() < 60 * 60 {
        return format!("{}m", age.as_secs() / 60);
    }
    if age.as_secs() < 24 * 60 * 60 {
        return format!("{}h", age.as_secs() / (60 * 60));
    }
    format!("{}d", age.as_secs() / (24 * 60 * 60))
}
