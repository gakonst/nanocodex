//! Discovery for native Claude journals. Discovery is read-only and never claims
//! a durable owner; the normal Claude builder acquires ownership on continuation.
use eyre::{Result, WrapErr, eyre};
use nanocodex::{HarnessFamily, HarnessModel, agent::rollout::RolloutTranscriptItem};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs, io,
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

impl ResumeSession {
    /// Last update of the session journal, in Unix seconds.
    pub(crate) const fn updated(&self) -> u64 {
        self.updated
    }
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
        // The picker previews only the first prompt.
        match inspect(&db, home, &id, 1) {
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
    inspect(&open(home)?, home, id, usize::MAX)
        .wrap_err_with(|| format!("failed to load Claude session {id}"))
}

fn inspect(db: &Connection, home: &Path, id: &str, prompt_limit: usize) -> Result<ResumeSession> {
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
        transcript: transcript(
            &checkpoint,
            &admitted_prompts(db, id, retained, prompt_limit),
        ),
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

/// One ordered part of a user prompt, compared to recognize the checkpoint
/// message that an admitted prompt produced. Media bytes are never compared.
#[derive(Debug, PartialEq, Eq)]
enum PromptPart {
    Text(String),
    Media,
}

/// Harness-authored user-role messages that are never admitted as prompts.
/// Exact recovery notices are retained in the checkpoint itself.
const HARNESS_PREFIXES: &[&str] = &[
    "Continue the current task from the interrupted response.",
    "Host Stop hook requests continuation: ",
    "Harness recovery notice: ",
];

/// Prompt inputs admitted by this journal, in acceptance order. They are the
/// authority for real user turns: hook context, harness notices and
/// continuations share the user role in the checkpoint but are never admitted.
fn admitted_prompts(
    db: &Connection,
    id: &str,
    retained: &Value,
    limit: usize,
) -> Vec<Vec<PromptPart>> {
    let mut operations = retained["operations"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, operation)| {
            Some((
                operation["accepted_order"].as_u64()?,
                operation["input"].as_str()?,
            ))
        })
        .collect::<Vec<_>>();
    operations.sort_unstable_by_key(|(order, _)| *order);
    // Image prompts retain their media in the input; bound the total read.
    let mut budget = 4 * MAX_BYTES;
    let mut prompts = Vec::new();
    for (_, key) in operations {
        if prompts.len() >= limit {
            break;
        }
        let Ok(payload) = read_payload(db, id, key) else {
            continue;
        };
        let Some(remaining) = budget.checked_sub(payload.len()) else {
            break;
        };
        budget = remaining;
        let Ok(input) = serde_json::from_str::<Value>(&payload) else {
            continue;
        };
        if input["provider"] != "claude" || input["kind"] != "prompt" {
            continue;
        }
        prompts.push(match &input["prompt"]["instruction"] {
            Value::String(text) => vec![PromptPart::Text(text.clone())],
            Value::Array(items) => items
                .iter()
                .map(|item| match item["type"].as_str() {
                    Some("text") => {
                        PromptPart::Text(item["text"].as_str().unwrap_or_default().to_owned())
                    }
                    _ => PromptPart::Media,
                })
                .collect(),
            _ => continue,
        });
    }
    prompts
}

fn blocks(message: &Value) -> impl Iterator<Item = &Value> {
    message["content"].as_array().into_iter().flatten()
}

fn prompt_parts(message: &Value) -> Vec<PromptPart> {
    blocks(message)
        .filter_map(|block| match block["type"].as_str()? {
            "text" => Some(PromptPart::Text(block["text"].as_str()?.to_owned())),
            "image" | "document" => Some(PromptPart::Media),
            _ => None,
        })
        .collect()
}

/// The prompt as the composer displayed it: each image replaced its own
/// placeholder, numbered per prompt. Media bytes never enter the transcript.
fn prompt_display(message: &Value) -> String {
    let (mut text, mut images, mut documents) = (String::new(), 0, 0);
    for block in blocks(message) {
        match block["type"].as_str() {
            Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
            Some("image") => {
                images += 1;
                text.push_str(&format!("[Image #{images}]"));
            }
            Some("document") => {
                documents += 1;
                text.push_str(&format!("[Document #{documents}]"));
            }
            _ => {}
        }
    }
    text
}

fn harness_authored(message: &Value, notices: &[&str]) -> bool {
    let mut text = String::new();
    for block in blocks(message) {
        match block["text"].as_str() {
            Some(part) if block["type"] == "text" => text.push_str(part),
            _ => return false,
        }
    }
    notices.contains(&text.as_str()) || HARNESS_PREFIXES.iter().any(|p| text.starts_with(p))
}

fn tool_output(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item["type"].as_str()? {
                "text" => item["text"].as_str().map(str::to_owned),
                "image" => Some("[image]".to_owned()),
                "document" => Some("[document]".to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn transcript(checkpoint: &Value, prompts: &[Vec<PromptPart>]) -> Vec<RolloutTranscriptItem> {
    let mut items = Vec::new();
    let conversation = &checkpoint["conversation"];
    if let Some(summary) = conversation["summary"].as_str().filter(|s| !s.is_empty()) {
        items.push(RolloutTranscriptItem::Assistant(format!(
            "Retained conversation summary:\n{summary}"
        )));
    }
    let notices = conversation["recovery_notices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let messages = conversation["messages"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let receipt = |message: &Value| blocks(message).any(|block| block["type"] == "tool_result");
    // Code Mode child calls retained by the engine without their results. A
    // call started by exec and finished by a later wait keeps its final status.
    let mut children = HashMap::<&str, Vec<&Value>>::new();
    let mut outcomes = HashMap::<&str, &str>::new();
    for round in conversation["code_calls"].as_array().into_iter().flatten() {
        let Some(parent) = round["tool_use_id"].as_str() else {
            continue;
        };
        for call in round["calls"].as_array().into_iter().flatten() {
            if let Some(call_id) = call["call_id"].as_str() {
                outcomes.insert(call_id, call["status"].as_str().unwrap_or("unknown"));
                children.entry(parent).or_default().push(call);
            }
        }
    }
    let mut replayed_children = HashSet::new();
    let mut next_prompt = 0;
    let mut index = 0;
    while let Some(message) = messages.get(index) {
        if message["role"].as_str() == Some("assistant") {
            for block in blocks(message) {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = block["text"].as_str() {
                            items.push(RolloutTranscriptItem::Assistant(text.into()));
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
            index += 1;
            continue;
        }
        if receipt(message) {
            for block in blocks(message).filter(|block| block["type"] == "tool_result") {
                let parent = block["tool_use_id"].as_str().unwrap_or_default();
                for call in children.get(parent).into_iter().flatten() {
                    let call_id = call["call_id"].as_str().unwrap_or_default();
                    if !replayed_children.insert(call_id) {
                        continue;
                    }
                    items.push(RolloutTranscriptItem::Tool {
                        call_id: call_id.into(),
                        name: call["name"].as_str().unwrap_or_default().into(),
                        arguments: call["input"].to_string(),
                    });
                    items.push(match outcomes.get(call_id).copied() {
                        Some("completed") => RolloutTranscriptItem::tool_result(call_id, "", false),
                        Some("failed") => RolloutTranscriptItem::tool_result(call_id, "", true),
                        _ => RolloutTranscriptItem::tool_result(call_id, "Outcome unknown", true),
                    });
                }
                items.push(RolloutTranscriptItem::tool_result(
                    parent,
                    &tool_output(&block["content"]),
                    block["is_error"].as_bool() == Some(true),
                ));
            }
            index += 1;
            continue;
        }
        // One admission writes its hook context, synthetic transcript and
        // prompt as adjacent user messages; only the admitted prompt is shown.
        let end = messages[index..]
            .iter()
            .position(|message| message["role"].as_str() != Some("user") || receipt(message))
            .map_or(messages.len(), |offset| index + offset);
        let run = &messages[index..end];
        index = end;
        let admitted = run.iter().find_map(|message| {
            let parts = prompt_parts(message);
            prompts
                .get(next_prompt..)?
                .iter()
                .position(|prompt| *prompt == parts)
                .map(|offset| (message, offset))
        });
        if let Some((message, offset)) = admitted {
            next_prompt += offset + 1;
            items.push(RolloutTranscriptItem::User(prompt_display(message)));
            continue;
        }
        // Steering input, or a prompt whose journal input was not retained.
        for message in run
            .iter()
            .filter(|message| !harness_authored(message, &notices))
        {
            let text = prompt_display(message);
            if !text.is_empty() {
                items.push(RolloutTranscriptItem::User(text));
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
    let _ = inspect(&db, home, id, 0)?;
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
