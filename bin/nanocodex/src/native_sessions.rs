//! Discovery for native Claude journals. Discovery is read-only and never claims
//! a durable owner; the normal Claude builder acquires ownership on continuation.
use eyre::{Result, WrapErr, eyre};
use nanocodex::{HarnessFamily, HarnessModel, agent::rollout::RolloutTranscriptItem};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_SESSIONS: usize = 1000;

#[derive(Clone, Debug)]
pub(crate) struct ResumeSession {
    pub(crate) id: String,
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) model: Option<HarnessModel>,
    pub(crate) transcript: Vec<RolloutTranscriptItem>,
    updated: u64,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    id: String,
    workspace: PathBuf,
    model: String,
    updated: u64,
}

pub(crate) fn store_path(home: &Path) -> PathBuf {
    home.join("claude/sessions.sqlite")
}

fn manifest_path(home: &Path, id: &str) -> PathBuf {
    let name: String = id.bytes().map(|b| format!("{b:02x}")).collect();
    home.join("claude/sessions").join(format!("{name}.json"))
}

/// Only non-secret routing metadata is retained here. The journal remains the
/// authority for checkpoint/model data, including interactive model changes.
pub(crate) fn register(home: &Path, id: &str, workspace: &Path, model: HarnessModel) -> Result<()> {
    let path = manifest_path(home, id);
    fs::create_dir_all(path.parent().expect("manifest parent"))?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let manifest = Manifest {
        version: 1,
        id: id.into(),
        workspace: workspace.into(),
        model: model.to_string(),
        updated: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    };
    fs::write(&temporary, serde_json::to_vec(&manifest)?)?;
    fs::rename(&temporary, &path).wrap_err("failed to save Claude session metadata")
}

fn open(home: &Path) -> Result<Connection> {
    let path = store_path(home);
    if !path.is_file() {
        return Err(eyre!(
            "no resumable Claude sessions found under {}",
            home.display()
        ));
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .wrap_err("failed to inspect native Claude sessions")
}

pub(crate) fn discover(home: &Path) -> Result<Vec<ResumeSession>> {
    let db = open(home)?;
    let mut query =
        db.prepare("SELECT state_id FROM nanocodex_durable_states ORDER BY rowid DESC LIMIT ?1")?;
    let ids = query
        .query_map([MAX_SESSIONS as i64 + 1], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ids.len() > MAX_SESSIONS {
        eprintln!(
            "Showing the newest {MAX_SESSIONS} Claude journals; older sessions can be resumed by ID."
        );
    }
    let mut sessions = Vec::new();
    for id in ids.into_iter().take(MAX_SESSIONS) {
        match inspect(&db, home, &id) {
            Ok(session) => sessions.push(session),
            Err(error) => eprintln!("Skipping Claude session {}: {error}", clean(&id)),
        }
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.updated));
    Ok(sessions)
}

pub(crate) fn load(home: &Path, id: &str) -> Result<ResumeSession> {
    if id.len() > 256 {
        return Err(eyre!("Claude session ID is too long"));
    }
    inspect(&open(home)?, home, id).wrap_err_with(|| format!("failed to load Claude session {id}"))
}

fn inspect(db: &Connection, home: &Path, id: &str) -> Result<ResumeSession> {
    // The store API only exposes acquiring owners. Use bounded, read-only SQL
    // here so opening/cancelling the picker cannot fence a running process.
    let state: Option<String> = db.query_row(
        "SELECT payload FROM nanocodex_durable_states WHERE state_id=?1 AND length(CAST(payload AS BLOB)) <= ?2",
        rusqlite::params![id, MAX_BYTES as i64], |row| row.get(0),
    ).optional()?;
    let state: Value = serde_json::from_str(
        &state.ok_or_else(|| eyre!("unknown session or journal exceeds discovery size limit"))?,
    )?;
    let retained = &state["nanocodex_durable_state"];
    if retained["format"].as_u64() != Some(4) {
        return Err(eyre!("unsupported native journal format"));
    }
    let reference = retained["latest_checkpoint"]
        .as_str()
        .ok_or_else(|| eyre!("session has no saved checkpoint yet"))?;
    let checkpoint: Value = serde_json::from_str(&read_payload(db, id, reference)?)?;
    if checkpoint["provider"].as_str() != Some("claude")
        || checkpoint["version"].as_u64() != Some(1)
    {
        return Err(eyre!("not a supported native Claude checkpoint"));
    }
    let path = manifest_path(home, id);
    let manifest = match fs::metadata(&path) {
        Ok(metadata) if metadata.len() <= 64 * 1024 => {
            let value: Manifest = serde_json::from_slice(&fs::read(&path)?)
                .wrap_err("invalid Claude session metadata")?;
            if value.version != 1 || value.id != id {
                return Err(eyre!("Claude session metadata identity/version mismatch"));
            }
            Some(value)
        }
        Ok(_) => return Err(eyre!("Claude session metadata exceeds size limit")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let model = checkpoint["model"]
        .as_str()
        .or_else(|| manifest.as_ref().map(|m| m.model.as_str()))
        .map(|value| -> Result<HarnessModel> {
            let model = value.parse::<HarnessModel>().map_err(|e| eyre!("{e}"))?;
            if model.family() != HarnessFamily::Claude {
                return Err(eyre!("saved model is not Claude"));
            }
            Ok(model)
        })
        .transpose()?;
    let workspace = checkpoint["workspace"]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| manifest.as_ref().map(|m| m.workspace.clone()));
    Ok(ResumeSession {
        id: id.into(),
        workspace,
        model,
        transcript: transcript(&checkpoint),
        updated: manifest.map_or(0, |m| m.updated),
    })
}

fn read_record(db: &Connection, id: &str, key: &str) -> Result<String> {
    db.query_row("SELECT value FROM nanocodex_durable_records WHERE state_id=?1 AND key=?2 AND length(CAST(value AS BLOB)) <= ?3", rusqlite::params![id,key,MAX_BYTES as i64], |row| row.get(0))
        .wrap_err("missing or oversized native checkpoint record")
}

fn read_payload(db: &Connection, id: &str, key: &str) -> Result<String> {
    let record = read_record(db, id, key)?;
    if let Some(value) = record.strip_prefix('=') {
        return Ok(value.into());
    }
    let keys = if let Some(hashes) = record.strip_prefix('#') {
        if hashes.is_empty()
            || hashes.len() % 64 != 0
            || !hashes.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(eyre!("invalid native checkpoint chunk manifest"));
        }
        // Current journals use content-addressed chunks as small as 2 KiB.
        if hashes.len() / 64 > MAX_BYTES / 2048 + 1 {
            return Err(eyre!("native checkpoint exceeds discovery size limit"));
        }
        (0..hashes.len())
            .step_by(64)
            .map(|offset| format!("c:{}", &hashes[offset..offset + 64]))
            .collect::<Vec<_>>()
    } else {
        let count = record
            .strip_prefix('+')
            .ok_or_else(|| eyre!("unsupported native record encoding"))?
            .parse::<usize>()?;
        if count > 128 {
            return Err(eyre!("native checkpoint exceeds discovery size limit"));
        }
        (0..count).map(|index| format!("{key}/{index}")).collect()
    };
    let mut value = String::new();
    for chunk_key in keys {
        let chunk = read_record(db, id, &chunk_key)?;
        if value.len() + chunk.len() > MAX_BYTES {
            return Err(eyre!("native checkpoint exceeds discovery size limit"));
        }
        value.push_str(&chunk);
    }
    Ok(value)
}

fn transcript(checkpoint: &Value) -> Vec<RolloutTranscriptItem> {
    let mut items = Vec::new();
    if let Some(summary) = checkpoint["conversation"]["summary"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        items.push(RolloutTranscriptItem::Assistant(format!(
            "Retained conversation summary:\n{summary}"
        )));
    }
    for message in checkpoint["conversation"]["messages"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let assistant = message["role"].as_str() == Some("assistant");
        for block in message["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(text) = block["text"].as_str() {
                        items.push(if assistant {
                            RolloutTranscriptItem::Assistant(text.into())
                        } else {
                            RolloutTranscriptItem::User(text.into())
                        });
                    }
                }
                Some("tool_use") => items.push(RolloutTranscriptItem::Tool {
                    call_id: block["id"].as_str().unwrap_or_default().into(),
                    name: block["name"].as_str().unwrap_or_default().into(),
                    arguments: block["input"].to_string(),
                }),
                _ => {} // Never expose signed thinking or binary payloads in the picker/TUI.
            }
        }
    }
    items
}

fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(180)
        .collect()
}

pub(crate) fn select(sessions: &[ResumeSession]) -> Result<Option<String>> {
    use legacy_crossterm::{
        cursor,
        event::{self, Event, KeyCode, KeyModifiers},
        execute,
        terminal::{self, ClearType},
    };
    struct Terminal;
    impl Drop for Terminal {
        fn drop(&mut self) {
            let _ = legacy_crossterm::execute!(
                io::stdout(),
                legacy_crossterm::terminal::LeaveAlternateScreen,
                legacy_crossterm::cursor::Show
            );
            let _ = legacy_crossterm::terminal::disable_raw_mode();
        }
    }
    terminal::enable_raw_mode()?;
    let _terminal = Terminal;
    execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
    let mut selected = 0usize;
    loop {
        let mut stdout = io::stdout();
        execute!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::All)
        )?;
        write!(
            stdout,
            "Resume a Claude session\r\n↑/↓ select · enter resume · esc cancel\r\n\r\n"
        )?;
        let rows = usize::from(terminal::size()?.1.saturating_sub(4)).max(1) / 3;
        let rows = rows.max(1);
        let start = selected.saturating_sub(rows - 1);
        for (index, session) in sessions.iter().enumerate().skip(start).take(rows) {
            let workspace = session.workspace.as_ref().map_or_else(
                || "legacy: --cwd required".into(),
                |v| v.display().to_string(),
            );
            let model = session
                .model
                .map_or_else(|| "legacy: --model required".into(), |v| v.to_string());
            let preview = session
                .transcript
                .iter()
                .find_map(|item| match item {
                    RolloutTranscriptItem::User(text) => Some(text.as_str()),
                    _ => None,
                })
                .unwrap_or("(retained session)");
            write!(
                stdout,
                "{} {}\r\n  {} · {}\r\n  {}\r\n",
                if index == selected { ">" } else { " " },
                clean(&session.id),
                clean(&workspace),
                clean(&model),
                clean(preview)
            )?;
        }
        stdout.flush()?;
        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Enter => return Ok(sessions.get(selected).map(|s| s.id.clone())),
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(sessions.len().saturating_sub(1))
                }
                _ => {}
            }
        }
    }
}

/// Read-only preview: unlike branch creation, listing never fences a live owner.
pub(crate) fn rewind_preview(home: &Path, id: &str) -> Result<Value> {
    let db = open(home)?;
    let payload: String = db.query_row(
        "SELECT payload FROM nanocodex_durable_states WHERE state_id=?1 AND length(CAST(payload AS BLOB)) <= ?2",
        rusqlite::params![id, MAX_BYTES as i64], |row| row.get(0),
    ).wrap_err("unknown or oversized native session")?;
    let value: Value = serde_json::from_str(&payload)?;
    let retained = &value["nanocodex_durable_state"];
    if retained["format"].as_u64() != Some(4) {
        return Err(eyre!("unsupported native journal format"));
    }
    // Validate provider and routing metadata using the normal read-only inspector.
    let _ = inspect(&db, home, id)?;
    let mut operations = retained["operations"]
        .as_object()
        .ok_or_else(|| eyre!("invalid operations"))?
        .iter()
        .collect::<Vec<_>>();
    operations.sort_by_key(|(_, op)| op["accepted_order"].as_u64().unwrap_or(0));
    let mut turns = Vec::new();
    for (turn, operation) in operations {
        let key = operation["input"]
            .as_str()
            .ok_or_else(|| eyre!("invalid input reference"))?;
        let input: Value = serde_json::from_str(&read_payload(&db, id, key)?)?;
        if input["provider"] != "claude" || input["kind"] != "prompt" {
            continue;
        }
        turns.push(
            serde_json::json!({"checkpoint":turn,"input":input,"status":operation["status"]}),
        );
    }
    Ok(
        serde_json::json!({"session":id,"checkpoints":turns,"restored":false,"scope":"Branch immediately before the selected user turn. Original conversation remains recoverable; historical tools are never replayed. Retention limits may make older boundaries unavailable."}),
    )
}
