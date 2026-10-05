//! Explicit user-only native restoration; never registered as an agent tool.
use eyre::{Result, eyre};
use nanocodex_durability::{CheckpointBranch, SqliteStore};
use serde_json::{Value, json};

pub(crate) async fn run(
    session: &str,
    checkpoint: Option<&str>,
    restore: bool,
    mode: &str,
) -> Result<()> {
    let home = crate::config::default_codex_home()?;
    if mode == "files" {
        let result = crate::config::rewind_files(&home, session, checkpoint, restore)
            .map_err(|error| eyre!(error))?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    if !matches!(mode, "conversation" | "files-and-conversation") {
        return Err(eyre!("unknown rewind mode"));
    }
    if !restore {
        let mut result = crate::native_sessions::rewind_preview(&home, session)?;
        result["mode"] = json!(mode);
        if let Some(turn) = checkpoint {
            let turns: Vec<_> = result["checkpoints"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|value| value["checkpoint"].as_str())
                .collect();
            let selected = turns
                .iter()
                .position(|candidate| *candidate == turn)
                .ok_or_else(|| {
                    eyre!("unknown or expired user turn; choose a checkpoint from the preview")
                })?;
            let discarded = json!(turns[selected..]);
            let files = if mode == "files-and-conversation" {
                Some(file_selection(&home, session, &turns[selected..])?.1)
            } else {
                None
            };
            result["selected_checkpoint"] = json!(turn);
            result["discarded_turns"] = discarded;
            if let Some(files) = files {
                result["files"] = files;
            }
        }
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    let turn = checkpoint.ok_or_else(|| {
        eyre!("--restore requires --checkpoint <turn-id>; preview the session first")
    })?;
    // Load routing metadata before acquiring ownership; SQL is read-only.
    let original = crate::native_sessions::load(&home, session)?;
    let store = SqliteStore::open(crate::native_sessions::store_path(&home))?;
    let mut branch = CheckpointBranch::open(store, session).await?;
    let turns: Vec<_> = branch
        .turns()
        .await?
        .into_iter()
        .filter(|turn| turn.input["provider"] == "claude" && turn.input["kind"] == "prompt")
        .collect();
    if !turns.iter().any(|candidate| candidate.id == turn) {
        return Err(eyre!(
            "unknown or expired user turn; choose a checkpoint from the preview"
        ));
    }
    let latest: Value = branch.latest().await?.decode()?;
    let previous = branch
        .before(turn)
        .await?
        .map(|value| value.decode::<Value>())
        .transpose()?;
    let prepared = nanocodex::claude::rewind_checkpoint(previous, latest)?;
    let mut files = json!({"restored":false,"changes":[]});
    let mut file_turn = None;
    if mode == "files-and-conversation" {
        let selected = turns
            .iter()
            .position(|candidate| candidate.id == turn)
            .ok_or_else(|| eyre!("unknown rewind turn"))?;
        let suffix: Vec<_> = turns[selected..]
            .iter()
            .map(|turn| turn.id.as_str())
            .collect();
        (file_turn, files) = file_selection(&home, session, &suffix)?;
    }
    branch.verify_source().await?;
    crate::config::prepare_rewind_branch(&home, session, branch.branch_id())
        .map_err(|error| eyre!(error))?;
    if let Some(turn) = file_turn {
        files = crate::config::rewind_files(&home, session, Some(&turn), true)
            .map_err(|error| eyre!(error))?;
    }
    let id = branch.publish(&prepared).await.map_err(|error| eyre!("branch publication failed: {error}; original session retained; file restoration result: {files}. Inspect before retrying."))?;
    let workspace = prepared["workspace"]
        .as_str()
        .map(std::path::Path::new)
        .or(original.workspace.as_deref());
    let model = prepared["model"]
        .as_str()
        .and_then(|v| v.parse().ok())
        .or(original.model);
    if let (Some(workspace), Some(model)) = (workspace, model) {
        crate::native_sessions::register(&home, &id, workspace, model).map_err(|error| eyre!("branch {id} was created but routing metadata failed: {error}; resume this ID explicitly"))?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"session":session,"branch_session":id,"selected_checkpoint":turn,"mode":mode,"restored":true,"files":files,"resume_command":format!("nanocodex resume {id} --claude"),"scope":"Conversation branched before selected turn; original remains recoverable. Bash, MCP and other external effects are not undone or replayed."})
        )?
    );
    Ok(())
}

// The selected user turn may have no file edits. Start at the first file-edit
// checkpoint in its suffix, and validate the entire file chain before mutation.
fn file_selection(
    home: &std::path::Path,
    session: &str,
    suffix: &[&str],
) -> Result<(Option<String>, Value)> {
    let empty = || (None, json!({"restored":false,"changes":[]}));
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
