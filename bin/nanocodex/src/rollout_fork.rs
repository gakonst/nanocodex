//! Start a Codex thread from a saved rollout, optionally at an earlier turn.
//!
//! The source rollout is copied, never changed. The copy gets a new thread ID
//! and the requested workspace, so a rollout recorded on another machine or in
//! another directory can be resumed here. With a turn, history after that
//! completed turn is dropped.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use eyre::{Result, WrapErr as _, eyre};
use serde_json::Value;

/// Where the new thread starts.
pub(crate) enum Point {
    /// Keep the whole rollout.
    End,
    /// Keep history through the completed turn with this ID.
    Turn(String),
    /// Keep history through the Nth completed turn, counting from 1.
    Count(usize),
}

impl Point {
    pub(crate) fn parse(value: Option<&str>) -> Result<Self> {
        let Some(value) = value else {
            return Ok(Self::End);
        };
        match value.parse::<usize>() {
            Ok(0) => Err(eyre!("--at counts completed turns from 1")),
            Ok(count) => Ok(Self::Count(count)),
            Err(_) => Ok(Self::Turn(value.to_owned())),
        }
    }
}

/// Copy `source` into `codex_home` as a new thread rooted at `workspace`.
/// Returns the new thread ID.
pub(crate) fn fork(
    source: &Path,
    point: &Point,
    codex_home: &Path,
    workspace: &Path,
) -> Result<String> {
    let text = fs::read_to_string(source)
        .wrap_err_with(|| format!("failed to read rollout {}", source.display()))?;
    let thread = uuid::Uuid::now_v7().to_string();
    let workspace = workspace.to_string_lossy().into_owned();
    let mut rows = Vec::new();
    let mut completed = 0;
    let mut reached = matches!(point, Point::End);
    let mut meta = false;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut row: Value = serde_json::from_str(line)
            .wrap_err_with(|| format!("{} is not a JSONL rollout", source.display()))?;
        match row["type"].as_str() {
            Some("session_meta") if !meta => {
                meta = true;
                let payload = &mut row["payload"];
                payload["id"] = thread.clone().into();
                payload["session_id"] = thread.clone().into();
                payload["root_session_id"] = thread.clone().into();
                payload["cwd"] = workspace.clone().into();
            }
            Some("turn_context") => row["payload"]["cwd"] = workspace.clone().into(),
            _ => {}
        }
        let finished = row["type"] == "event_msg"
            && matches!(
                row["payload"]["type"].as_str(),
                Some("task_complete" | "turn_aborted")
            );
        rows.push(row);
        if finished {
            completed += 1;
            let turn = rows
                .last()
                .and_then(|row| row["payload"]["turn_id"].as_str());
            let stop = match point {
                Point::End => false,
                Point::Turn(id) => turn == Some(id.as_str()),
                Point::Count(count) => completed == *count,
            };
            if stop {
                reached = true;
                break;
            }
        }
    }
    if !meta {
        return Err(eyre!("{} has no session metadata", source.display()));
    }
    if !reached {
        return Err(match point {
            Point::Turn(id) => eyre!("turn {id} did not complete in {}", source.display()),
            Point::Count(count) => eyre!(
                "{} has {completed} completed turns, fewer than {count}",
                source.display()
            ),
            Point::End => unreachable!("End is always reached"),
        });
    }
    let now = chrono::Local::now();
    let directory: PathBuf = codex_home
        .join("sessions")
        .join(now.format("%Y/%m/%d").to_string());
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!(
        "rollout-{}-{thread}.jsonl",
        now.format("%Y-%m-%dT%H-%M-%S")
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .wrap_err_with(|| format!("failed to create {}", path.display()))?;
    for row in &rows {
        serde_json::to_writer(&mut file, row)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    Ok(thread)
}
