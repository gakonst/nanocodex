use super::*;
use std::{collections::HashMap, time::SystemTime};

/// Lightweight metadata for one resumable Codex-compatible rollout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RolloutSessionInfo {
    thread_id: String,
    workspace: Option<String>,
    preview: Option<String>,
    modified_at: SystemTime,
    archived: bool,
}

impl RolloutSessionInfo {
    /// Returns the stable thread UUID accepted by [`RolloutConfig::load_session`].
    #[must_use]
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Returns the workspace recorded in the rollout metadata, when available.
    #[must_use]
    pub fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    /// Returns a bounded single-line preview of the first user prompt.
    #[must_use]
    pub fn preview(&self) -> Option<&str> {
        self.preview.as_deref()
    }

    /// Returns the rollout file's last modification time.
    #[must_use]
    pub const fn modified_at(&self) -> SystemTime {
        self.modified_at
    }

    /// Returns whether the rollout was found below `archived_sessions`.
    #[must_use]
    pub const fn is_archived(&self) -> bool {
        self.archived
    }
}

/// A completed model boundary materialized from a Codex-compatible rollout.
///
/// This value is intentionally single-use: [`Self::into_parts`] transfers the
/// rollout continuation into a builder. Forks and spawned agents always receive
/// fresh rollout files.
#[derive(Debug)]
pub struct DurableSession {
    codex_home: PathBuf,
    thread_id: String,
    rollout_path: PathBuf,
    model: Model,
    snapshot: SessionSnapshot,
    transcript: Vec<RolloutTranscriptItem>,
}

impl DurableSession {
    pub(super) fn load(codex_home: &Path, thread_id: &str) -> io::Result<Self> {
        uuid::Uuid::parse_str(thread_id).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid Codex thread ID `{thread_id}`: {error}"),
            )
        })?;
        let rollout_path = find_rollout_path(codex_home, thread_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no Codex rollout found for thread {thread_id}"),
            )
        })?;
        let materialized = materialize_rollout(&rollout_path, thread_id)?;
        let snapshot = SessionSnapshot::from_rollout(
            materialized.model,
            thread_id.to_owned(),
            materialized.prompt_cache_key,
            materialized.workspace,
            materialized.base_instructions,
            materialized.history,
            materialized.context_baseline,
        )
        .map_err(io::Error::other)?;
        Ok(Self {
            codex_home: codex_home.to_path_buf(),
            thread_id: thread_id.to_owned(),
            rollout_path,
            model: materialized.model,
            snapshot,
            transcript: materialized.transcript,
        })
    }

    /// Returns the stable thread UUID retained across process restarts.
    #[must_use]
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Returns the original workspace restored by this session.
    #[must_use]
    pub fn workspace(&self) -> &str {
        self.snapshot.workspace()
    }

    /// Returns the model selected at the latest committed rollout boundary.
    #[must_use]
    pub const fn model(&self) -> Model {
        self.model
    }

    /// Returns the restored model boundary.
    #[must_use]
    pub const fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    /// Returns the Codex-compatible rollout reopened by this session.
    #[must_use]
    pub fn rollout_path(&self) -> &Path {
        &self.rollout_path
    }

    /// Returns the visible activity used to restore the originating transcript.
    #[must_use]
    pub fn transcript(&self) -> &[RolloutTranscriptItem] {
        &self.transcript
    }

    /// Splits this loaded boundary into the builder inputs needed to continue it.
    #[must_use]
    pub fn into_parts(self) -> (String, SessionSnapshot, RolloutConfig) {
        (
            self.thread_id,
            self.snapshot,
            RolloutConfig::new(self.codex_home).resumed(self.rollout_path),
        )
    }
}

/// User-visible activity reconstructed from a Codex-compatible rollout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RolloutTranscriptItem {
    /// A submitted user prompt.
    User(String),
    /// A reasoning summary displayed while the assistant was working.
    Reasoning(String),
    /// An assistant message displayed by the originating client.
    Assistant(String),
    /// A tool invocation displayed by the originating client.
    Tool {
        /// Stable call identifier from the rollout.
        call_id: String,
        /// Tool name sent by the model.
        name: String,
        /// Serialized tool arguments sent by the model.
        arguments: String,
    },
}

pub(super) fn list_sessions(codex_home: &Path) -> io::Result<Vec<RolloutSessionInfo>> {
    let mut sessions = HashMap::<String, RolloutSessionInfo>::new();
    for (root, archived) in [
        (codex_home.join("sessions"), false),
        (codex_home.join("archived_sessions"), true),
    ] {
        let mut directories = vec![root];
        while let Some(directory) = directories.pop() {
            let entries = match std::fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for entry in entries {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if file_type.is_dir() {
                    directories.push(entry.path());
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let Some(thread_id) = rollout_thread_id(&entry.file_name()) else {
                    continue;
                };
                let Some(candidate) = rollout_session_info(&entry.path(), thread_id, archived)
                else {
                    continue;
                };
                match sessions.entry(candidate.thread_id.clone()) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(candidate);
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry)
                        if prefer_session(&candidate, entry.get()) =>
                    {
                        entry.insert(candidate);
                    }
                    std::collections::hash_map::Entry::Occupied(_) => {}
                }
            }
        }
    }
    let mut sessions = sessions.into_values().collect::<Vec<_>>();
    sort_sessions(&mut sessions);
    Ok(sessions)
}

fn rollout_thread_id(file_name: &std::ffi::OsStr) -> Option<String> {
    let stem = file_name.to_str()?.strip_suffix(".jsonl")?;
    let start = stem.len().checked_sub(36)?;
    let suffix = stem.get(start..)?;
    (start > 0 && stem.as_bytes().get(start - 1) == Some(&b'-'))
        .then(|| uuid::Uuid::parse_str(suffix).ok())
        .flatten()
        .map(|id| id.to_string())
}

fn rollout_session_info(
    path: &Path,
    thread_id: String,
    archived: bool,
) -> Option<RolloutSessionInfo> {
    let expected_id = uuid::Uuid::parse_str(&thread_id).ok()?;
    let modified_at = path.metadata().ok()?.modified().ok()?;
    let mut workspace = None;
    let mut preview = None;
    for line in BufReader::new(File::open(path).ok()?).lines() {
        let Ok(line) = line else {
            return None;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("session_meta") if workspace.is_none() => {
                let payload = value.get("payload")?;
                validate_legacy_history_mode(payload).ok()?;
                let stored_id = payload
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|id| uuid::Uuid::parse_str(id).ok())?;
                if stored_id != expected_id {
                    return None;
                }
                workspace = payload
                    .get("cwd")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
            }
            Some("event_msg") if preview.is_none() => {
                let payload = value.get("payload")?;
                if payload.get("type").and_then(serde_json::Value::as_str) == Some("user_message") {
                    preview = payload
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .and_then(prompt_preview);
                }
            }
            Some("response_item" | "compacted") if workspace.is_some() => {
                return Some(RolloutSessionInfo {
                    thread_id,
                    workspace,
                    preview,
                    modified_at,
                    archived,
                });
            }
            _ => {}
        }
    }
    None
}

pub(super) fn prompt_preview(prompt: &str) -> Option<String> {
    const MAX_CHARS: usize = 160;

    let mut preview = String::new();
    let mut chars = 0;
    let mut pending_space = false;
    for character in prompt.chars() {
        if character.is_whitespace() {
            pending_space = !preview.is_empty();
            continue;
        }
        if character.is_control() {
            continue;
        }
        if pending_space && chars < MAX_CHARS {
            preview.push(' ');
            chars += 1;
        }
        pending_space = false;
        if chars == MAX_CHARS {
            break;
        }
        preview.push(character);
        chars += 1;
    }
    (!preview.is_empty()).then_some(preview)
}

fn prefer_session(candidate: &RolloutSessionInfo, current: &RolloutSessionInfo) -> bool {
    match (candidate.archived, current.archived) {
        (false, true) => true,
        (true, false) => false,
        _ => candidate.modified_at > current.modified_at,
    }
}

fn sort_sessions(sessions: &mut [RolloutSessionInfo]) {
    sessions.sort_by(|left, right| {
        right
            .modified_at
            .cmp(&left.modified_at)
            .then_with(|| left.archived.cmp(&right.archived))
            .then_with(|| left.thread_id.cmp(&right.thread_id))
    });
}

fn find_rollout_path(codex_home: &Path, thread_id: &str) -> io::Result<Option<PathBuf>> {
    let suffix = format!("-{thread_id}.jsonl");
    let compressed_suffix = format!("-{thread_id}.jsonl.zst");
    for root in [
        codex_home.join("sessions"),
        codex_home.join("archived_sessions"),
    ] {
        let mut directories = vec![root];
        while let Some(directory) = directories.pop() {
            let entries = match std::fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for entry in entries {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if file_type.is_dir() {
                    directories.push(entry.path());
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let file_name = entry.file_name();
                let Some(file_name) = file_name.to_str() else {
                    continue;
                };
                if file_name.ends_with(&suffix) {
                    return entry.path().canonicalize().map(Some);
                }
                if file_name.ends_with(&compressed_suffix) {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!(
                            "compressed Codex rollout {} is not supported yet",
                            entry.path().display()
                        ),
                    ));
                }
            }
        }
    }
    Ok(None)
}

fn materialize_rollout(path: &Path, thread_id: &str) -> io::Result<MaterializedRollout> {
    let mut workspace = None;
    let mut prompt_cache_key = None;
    let mut base_instructions = None;
    let mut history = Vec::new();
    let mut transcript = Vec::new();
    let mut context_baseline = None;
    let mut model = Model::Sol;
    for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        let value: serde_json::Value = serde_json::from_str(&line).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "failed to decode {} line {}: {error}",
                    path.display(),
                    index + 1
                ),
            )
        })?;
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("session_meta") if workspace.is_none() => {
                let payload = &value["payload"];
                validate_legacy_history_mode(payload)?;
                if payload.get("id").and_then(serde_json::Value::as_str) != Some(thread_id) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Codex rollout thread ID does not match its filename",
                    ));
                }
                prompt_cache_key = match payload.get("prompt_cache_key") {
                    None => None,
                    Some(serde_json::Value::String(key)) if !key.trim().is_empty() => {
                        Some(key.to_owned())
                    }
                    Some(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Codex rollout session metadata has an invalid prompt cache key",
                        ));
                    }
                };
                base_instructions = payload["base_instructions"]["text"]
                    .as_str()
                    .map(str::to_owned);
                workspace = Some(
                    payload
                        .get("cwd")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Codex rollout session metadata is missing its workspace",
                            )
                        })?
                        .to_owned(),
                );
            }
            Some("response_item") => {
                if let Some(item) = visible_tool_call(&value["payload"]) {
                    transcript.push(item);
                }
                let item = serde_json::from_value(value["payload"].clone()).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "failed to decode response item at {} line {}: {error}",
                            path.display(),
                            index + 1
                        ),
                    )
                })?;
                history.push(item);
            }
            Some("compacted") => {
                history = serde_json::from_value(value["payload"]["replacement_history"].clone())
                    .map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "failed to decode replacement history at {} line {}: {error}",
                            path.display(),
                            index + 1
                        ),
                    )
                })?;
                context_baseline = None;
            }
            Some("turn_context") => {
                if let Some(selected) = value["payload"]["model"]
                    .as_str()
                    .and_then(|model| model.parse().ok())
                {
                    model = selected;
                }
            }
            Some("world_state") => {
                if let Some(state) = value["payload"]["state"].get("nanocodex_context") {
                    context_baseline =
                        Some(serde_json::from_value(state.clone()).map_err(|error| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "failed to decode context snapshot at {} line {}: {error}",
                                    path.display(),
                                    index + 1
                                ),
                            )
                        })?);
                }
            }
            Some("event_msg") => {
                if let Some(item) = visible_rollout_event(&value["payload"]) {
                    transcript.push(item);
                }
            }
            _ => {}
        }
    }
    let workspace = workspace.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex rollout is missing session metadata",
        )
    })?;
    // Genuine Codex rollouts predate Nanocodex's explicit cache identity. Their
    // stable thread UUID is the deterministic lineage fallback; Nanocodex-authored
    // rollouts retain the explicit key recorded in session metadata above.
    let prompt_cache_key = prompt_cache_key.unwrap_or_else(|| thread_id.to_owned());
    let workspace = Path::new(&workspace).canonicalize()?;
    let workspace = workspace.into_os_string().into_string().map_err(|path| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Codex rollout workspace is not valid UTF-8: {}",
                Path::new(&path).display()
            ),
        )
    })?;
    Ok(MaterializedRollout {
        model,
        prompt_cache_key,
        workspace,
        base_instructions,
        history,
        transcript,
        context_baseline,
    })
}

struct MaterializedRollout {
    model: Model,
    prompt_cache_key: String,
    workspace: String,
    base_instructions: Option<String>,
    history: Vec<ResponseItem>,
    transcript: Vec<RolloutTranscriptItem>,
    context_baseline: Option<ContextBaseline>,
}

pub(in crate::rollout) fn visible_rollout_event(
    payload: &serde_json::Value,
) -> Option<RolloutTranscriptItem> {
    match payload.get("type")?.as_str()? {
        "user_message" => visible_text(payload, "message").map(RolloutTranscriptItem::User),
        "agent_reasoning" => visible_text(payload, "text").map(RolloutTranscriptItem::Reasoning),
        "agent_message" => visible_text(payload, "message").map(RolloutTranscriptItem::Assistant),
        "mcp_tool_call_end" => {
            let invocation = payload.get("invocation")?;
            let server = invocation.get("server")?.as_str()?;
            let tool = invocation.get("tool")?.as_str()?;
            Some(RolloutTranscriptItem::Tool {
                call_id: payload.get("call_id")?.as_str()?.to_owned(),
                name: format!("{server}.{tool}"),
                arguments: serde_json::to_string(invocation.get("arguments")?).ok()?,
            })
        }
        "web_search_end" => Some(RolloutTranscriptItem::Tool {
            call_id: payload.get("call_id")?.as_str()?.to_owned(),
            name: "web_search".to_owned(),
            arguments: serde_json::to_string(payload.get("action")?).ok()?,
        }),
        _ => None,
    }
}

pub(super) fn validate_legacy_history_mode(payload: &serde_json::Value) -> io::Result<()> {
    match payload.get("history_mode") {
        None => Ok(()),
        Some(serde_json::Value::String(mode)) if mode == "legacy" => Ok(()),
        Some(serde_json::Value::String(mode)) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Codex rollout history mode `{mode}` is not supported"),
        )),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex rollout history mode must be a string",
        )),
    }
}

fn visible_text(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)?
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

pub(in crate::rollout) fn visible_tool_call(
    payload: &serde_json::Value,
) -> Option<RolloutTranscriptItem> {
    let (name, arguments) = match payload.get("type")?.as_str()? {
        "custom_tool_call" => (
            payload.get("name")?.as_str()?.to_owned(),
            payload.get("input")?.as_str()?.to_owned(),
        ),
        "function_call" => (
            payload.get("name")?.as_str()?.to_owned(),
            payload.get("arguments")?.as_str()?.to_owned(),
        ),
        _ => return None,
    };
    Some(RolloutTranscriptItem::Tool {
        call_id: payload.get("call_id")?.as_str()?.to_owned(),
        name,
        arguments,
    })
}
