//! One user-only branch/rewind verb for sessions of every harness family.
//!
//! A branch is a new session; the source session is never changed. `--before`
//! keeps history before a user turn, dropping it and every later turn;
//! `--through` keeps history through a completed turn. Restoring workspace
//! files is a separate capability, available when the session recorded native
//! file checkpoints. Never registered as an agent tool.
use std::path::{Path, PathBuf};

use clap::{Args, builder::NonEmptyStringValueParser};
use eyre::{Result, WrapErr as _, eyre};
use nanocodex_durability::{BranchPoint, StoredTurn, TurnStatus};
use serde_json::{Value, json};

#[derive(Args)]
pub(crate) struct Rewind {
    /// Saved session to branch or restore. Omit it with --from.
    #[arg(
        value_parser = NonEmptyStringValueParser::new(),
        required_unless_present = "from",
        conflicts_with = "from"
    )]
    session: Option<String>,

    /// Start a new session from this Codex-format rollout file.
    ///
    /// The file is copied, never changed. The new session's workspace is
    /// --cwd, or the current directory, so rollouts recorded elsewhere work.
    #[arg(long, value_name = "ROLLOUT")]
    from: Option<PathBuf>,

    /// Keep history before this user turn, dropping it and every later turn.
    #[arg(long, value_name = "TURN", value_parser = NonEmptyStringValueParser::new(), conflicts_with = "through")]
    before: Option<String>,

    /// Keep history through this completed turn: a turn ID or a completed-turn
    /// number counting from 1.
    #[arg(long, value_name = "TURN", value_parser = NonEmptyStringValueParser::new())]
    through: Option<String>,

    /// Branch the conversation, restore workspace files, or do both. Defaults
    /// to files for a saved session and to conversation with --from.
    #[arg(long, value_parser = ["files", "conversation", "files-and-conversation"])]
    mode: Option<String>,

    /// Apply the selection. Without it, print a preview and change nothing.
    #[arg(long)]
    restore: bool,

    /// Workspace of a session started --from a rollout file.
    #[arg(long, requires = "from")]
    cwd: Option<PathBuf>,
}

const SCOPE: &str = "The branch is a new session; the original remains recoverable. Historical tools are never replayed, and Bash, MCP and other external effects are not undone. Retention limits may make older boundaries unavailable.";

impl Rewind {
    fn mode(&self) -> &str {
        self.mode.as_deref().unwrap_or(if self.from.is_some() {
            "conversation"
        } else {
            "files"
        })
    }

    pub(crate) async fn run(self) -> Result<()> {
        let home = crate::config::default_codex_home()?;
        let result = match (&self.from, &self.session) {
            (Some(source), _) => self.branch_rollout(&home, source)?,
            (None, Some(session)) if self.mode() == "files" => {
                if self.through.is_some() {
                    return Err(eyre!(
                        "file restore selects the workspace before a turn; use --before"
                    ));
                }
                files(&home, session, self.before.as_deref(), self.restore)?
            }
            (None, Some(session)) => self.conversation(&home, session).await?,
            (None, None) => unreachable!("clap requires a session or --from"),
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
        Ok(())
    }

    fn point(&self, turns: &[StoredTurn]) -> Result<Option<(BranchPoint, usize)>> {
        let unknown =
            || eyre!("unknown or expired user turn; choose a checkpoint from the preview");
        if let Some(turn) = &self.before {
            let index = turns
                .iter()
                .position(|t| t.id == *turn)
                .ok_or_else(unknown)?;
            return Ok(Some((BranchPoint::Before(turn.clone()), index)));
        }
        let Some(turn) = &self.through else {
            return Ok(None);
        };
        let index = match turn.parse::<usize>() {
            Ok(0) => return Err(eyre!("--through counts completed turns from 1")),
            Ok(count) => turns
                .iter()
                .enumerate()
                .filter(|(_, t)| t.status == TurnStatus::Completed)
                .nth(count - 1)
                .map(|(index, _)| index)
                .ok_or_else(|| eyre!("the session has fewer than {count} completed turns"))?,
            Err(_) => turns
                .iter()
                .position(|t| t.id == *turn)
                .ok_or_else(unknown)?,
        };
        if turns[index].status != TurnStatus::Completed {
            return Err(eyre!("--through needs a completed turn"));
        }
        Ok(Some((
            BranchPoint::Through(turns[index].id.clone()),
            index + 1,
        )))
    }

    async fn conversation(&self, home: &Path, session: &str) -> Result<Value> {
        let (store, turns) = match crate::sessions::turns(home, session).await {
            Ok(found) => found,
            // A rollout-only Codex thread branches by copying its rollout.
            Err(_) if self.rollout_thread(home, session).is_some() => {
                let source = self.rollout_thread(home, session).expect("rollout thread");
                return self.branch_rollout(home, &source);
            }
            Err(error) => return Err(error),
        };
        let selection = self.point(&turns)?;
        let mut result = json!({
            "session": session,
            "mode": self.mode(),
            "checkpoints": turns.iter().map(|turn| json!({
                "checkpoint": turn.id,
                "input": turn.input,
                "preview": turn.preview,
                "status": turn.status,
            })).collect::<Vec<_>>(),
            "restored": false,
            "scope": SCOPE,
        });
        let Some((point, first_discarded)) = selection else {
            if self.restore {
                return Err(eyre!(
                    "--restore requires --before <turn-id> or --through <turn>; preview the session first"
                ));
            }
            return Ok(result);
        };
        let selected = match &point {
            BranchPoint::Before(turn) | BranchPoint::Through(turn) => turn.clone(),
            BranchPoint::Latest => unreachable!("rewind always selects a turn"),
        };
        let discarded: Vec<&str> = turns[first_discarded..]
            .iter()
            .map(|turn| turn.id.as_str())
            .collect();
        let with_files = self.mode() == "files-and-conversation";
        let (file_turn, mut files) = if with_files {
            file_selection(home, session, &discarded)?
        } else {
            (None, json!({"restored": false, "changes": []}))
        };
        if !self.restore {
            result["selected_checkpoint"] = json!(selected);
            result["discarded_turns"] = json!(discarded);
            if with_files {
                result["files"] = files;
            }
            return Ok(result);
        }
        let branch = crate::sessions::branch(&store, session, point, None)
            .await
            .wrap_err("branch publication failed; the original session is unchanged")?;
        let id = branch.id().to_owned();
        crate::config::prepare_rewind_branch(home, session, &id).map_err(|error| {
            eyre!("branch {id} was created but its host restrictions were not copied: {error}")
        })?;
        if let Some(turn) = file_turn {
            files =
                crate::config::rewind_files(home, session, Some(&turn), true).map_err(|error| {
                    eyre!("branch {id} was created but file restoration failed: {error}")
                })?;
        }
        Ok(json!({
            "session": session,
            "branch_session": id,
            "selected_checkpoint": selected,
            "mode": self.mode(),
            "restored": true,
            "files": files,
            "resume_command": format!("nanocodex resume {id}"),
            "scope": SCOPE,
        }))
    }

    fn rollout_thread(&self, home: &Path, session: &str) -> Option<PathBuf> {
        nanocodex::agent::rollout::RolloutConfig::new(home)
            .load_session(session)
            .ok()
            .map(|thread| thread.rollout_path().to_path_buf())
    }

    /// Branches a Codex-format rollout by copying it into a new session.
    fn branch_rollout(&self, home: &Path, source: &Path) -> Result<Value> {
        if self.mode() != "conversation" {
            return Err(eyre!(
                "rollout branches carry no file checkpoints; use --mode conversation"
            ));
        }
        if self.before.is_some() {
            return Err(eyre!(
                "rollout files branch only --through a completed turn"
            ));
        }
        let point = crate::rollout_fork::Point::parse(self.through.as_deref())?;
        if !self.restore {
            return Ok(json!({
                "from": source,
                "through": self.through,
                "restored": false,
                "scope": SCOPE,
            }));
        }
        let workspace = match &self.cwd {
            Some(path) => path.clone(),
            None => std::env::current_dir()?,
        }
        .canonicalize()
        .wrap_err("failed to resolve the new session's workspace")?;
        let id = crate::rollout_fork::fork(source, &point, home, &workspace)?;
        Ok(json!({
            "from": source,
            "branch_session": id,
            "through": self.through,
            "restored": true,
            "resume_command": format!("nanocodex resume {id}"),
            "scope": SCOPE,
        }))
    }
}

/// Native file checkpoints are a capability of the session, not of its family.
fn files(home: &Path, session: &str, turn: Option<&str>, restore: bool) -> Result<Value> {
    if restore && turn.is_none() {
        return Err(eyre!(
            "--restore requires --before <turn-id>; preview the session first"
        ));
    }
    crate::config::rewind_files(home, session, turn, restore).map_err(|error| eyre!(error))
}

// The selected user turn may have no file edits. Start at the first file-edit
// checkpoint in its suffix, and validate the entire file chain before mutation.
fn file_selection(home: &Path, session: &str, suffix: &[&str]) -> Result<(Option<String>, Value)> {
    let empty = || (None, json!({"restored": false, "changes": []}));
    let preview = match crate::config::rewind_files(home, session, None, false) {
        Ok(preview) => preview,
        Err(error) if error.starts_with("no native file checkpoints found") => return Ok(empty()),
        Err(error) => return Err(eyre!(error)),
    };
    let turn = preview["checkpoints"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| value["checkpoint"].as_str())
        .find(|candidate| suffix.contains(candidate));
    match turn {
        Some(turn) => Ok((
            Some(turn.to_owned()),
            crate::config::rewind_files(home, session, Some(turn), false)
                .map_err(|error| eyre!(error))?,
        )),
        None => Ok(empty()),
    }
}
