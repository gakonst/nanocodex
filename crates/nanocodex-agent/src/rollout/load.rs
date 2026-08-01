use super::*;
use std::collections::HashMap;

/// Lightweight metadata used to discover a resumable rollout before loading it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableSessionSummary {
    thread_id: String,
    workspace: PathBuf,
    updated_at: std::time::SystemTime,
    first_prompt: Option<String>,
}

impl DurableSessionSummary {
    /// Returns the stable thread UUID accepted by [`RolloutConfig::load_session`].
    #[must_use]
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Returns the workspace recorded when the session was created.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Returns the rollout file's last modification time.
    #[must_use]
    pub const fn updated_at(&self) -> std::time::SystemTime {
        self.updated_at
    }

    /// Returns the first visible user prompt when one has been recorded.
    #[must_use]
    pub fn first_prompt(&self) -> Option<&str> {
        self.first_prompt.as_deref()
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
    recorded_by_nanocodex: bool,
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
            recorded_by_nanocodex: materialized.recorded_by_nanocodex,
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

    /// Returns the stable instructions retained by the originating session.
    #[must_use]
    pub fn base_instructions(&self) -> Option<&str> {
        self.snapshot.base_instructions()
    }

    /// Returns whether this rollout can be continued in place by Nanocodex.
    #[must_use]
    pub const fn recorded_by_nanocodex(&self) -> bool {
        self.recorded_by_nanocodex
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

pub(super) fn list_sessions(codex_home: &Path) -> io::Result<Vec<DurableSessionSummary>> {
    let mut paths = Vec::new();
    for root in [
        codex_home.join("sessions"),
        codex_home.join("archived_sessions"),
    ] {
        collect_rollout_paths(&root, &mut paths)?;
    }
    let mut sessions = HashMap::with_capacity(paths.len());
    for path in paths {
        match summarize_rollout(&path) {
            Ok(session) => {
                let replace = sessions.get(session.thread_id()).is_none_or(
                    |existing: &DurableSessionSummary| session.updated_at() > existing.updated_at(),
                );
                if replace {
                    sessions.insert(session.thread_id().to_owned(), session);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidData | io::ErrorKind::Unsupported
                ) => {}
            Err(error) => return Err(error),
        }
    }
    let mut sessions = sessions.into_values().collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.thread_id.cmp(&left.thread_id))
    });
    Ok(sessions)
}

fn collect_rollout_paths(directory: &Path, paths: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_rollout_paths(&entry.path(), paths)?;
        } else if file_type.is_file() && rollout_thread_id(&entry.path()).is_some() {
            paths.push(entry.path());
        }
    }
    Ok(())
}

fn rollout_thread_id(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?.strip_suffix(".jsonl")?;
    let start = name.len().checked_sub(36)?;
    let thread_id = name.get(start..)?;
    (name.as_bytes().get(start.checked_sub(1)?) == Some(&b'-')
        && uuid::Uuid::parse_str(thread_id).is_ok())
    .then_some(thread_id)
}

fn summarize_rollout(path: &Path) -> io::Result<DurableSessionSummary> {
    let filename_thread_id = rollout_thread_id(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid Codex rollout filename {}", path.display()),
        )
    })?;
    let mut thread_id = None;
    let mut workspace = None;
    let mut first_event_prompt = None;
    let mut first_response_prompt = None;
    for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        let value: serde_json::Value = serde_json::from_str(&line).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "failed to decode {} line {} while listing sessions: {error}",
                    path.display(),
                    index + 1
                ),
            )
        })?;
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("session_meta") if workspace.is_none() => {
                let payload = &value["payload"];
                let id = payload.get("id").and_then(serde_json::Value::as_str);
                if id != Some(filename_thread_id) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Codex rollout thread ID does not match filename {}",
                            path.display()
                        ),
                    ));
                }
                validate_legacy_history_mode(payload)?;
                validate_root_session(payload)?;
                thread_id = id.map(str::to_owned);
                workspace = payload
                    .get("cwd")
                    .and_then(serde_json::Value::as_str)
                    .map(PathBuf::from);
            }
            Some("event_msg") if first_event_prompt.is_none() => {
                if let Some(RolloutTranscriptItem::User(prompt)) =
                    visible_rollout_event(&value["payload"])
                {
                    first_event_prompt = Some(prompt);
                }
            }
            Some("response_item") if first_response_prompt.is_none() => {
                first_response_prompt = visible_response_prompt(&value["payload"])
                    .filter(|prompt| !is_injected_context(prompt));
            }
            _ => {}
        }
        if workspace.is_some() && first_event_prompt.is_some() {
            break;
        }
    }
    let thread_id = thread_id.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Codex rollout {} is missing session metadata",
                path.display()
            ),
        )
    })?;
    let workspace = workspace.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Codex rollout {} session metadata is missing its workspace",
                path.display()
            ),
        )
    })?;
    let first_prompt = first_event_prompt
        .or(first_response_prompt)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Codex rollout {} has no resumable user prompt",
                    path.display()
                ),
            )
        })?;
    Ok(DurableSessionSummary {
        thread_id,
        workspace,
        updated_at: std::fs::metadata(path)?.modified()?,
        first_prompt: Some(first_prompt),
    })
}

fn validate_root_session(payload: &serde_json::Value) -> io::Result<()> {
    let is_child = payload
        .get("parent_thread_id")
        .is_some_and(|value| !value.is_null())
        || payload
            .get("forked_from_id")
            .is_some_and(|value| !value.is_null())
        || payload
            .get("thread_source")
            .and_then(serde_json::Value::as_str)
            == Some("subagent")
        || payload
            .get("source")
            .and_then(|source| source.get("subagent"))
            .is_some();
    if is_child {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "child and fork rollouts are not top-level resumable sessions",
        ));
    }
    Ok(())
}

fn is_injected_context(prompt: &str) -> bool {
    prompt.starts_with("# AGENTS.md instructions for ")
        || prompt.starts_with("<environment_context>")
}

fn visible_response_prompt(payload: &serde_json::Value) -> Option<String> {
    if payload.get("type")?.as_str()? != "message" || payload.get("role")?.as_str()? != "user" {
        return None;
    }
    let prompt = payload
        .get("content")?
        .as_array()?
        .iter()
        .filter(|content| {
            content.get("type").and_then(serde_json::Value::as_str) == Some("input_text")
        })
        .filter_map(|content| content.get("text").and_then(serde_json::Value::as_str))
        .collect::<String>();
    (!prompt.is_empty()).then_some(prompt)
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
    let mut base_instructions = None;
    let mut recorded_by_nanocodex = false;
    let mut history = Vec::new();
    let mut pending_history = Vec::new();
    let mut transcript = Vec::new();
    let mut pending_transcript = Vec::new();
    let mut context_baseline = None;
    let mut pending_context_baseline = None;
    let mut model = Model::Sol;
    let mut pending_model = None;
    let mut active_turn = false;
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
                base_instructions = payload["base_instructions"]["text"]
                    .as_str()
                    .map(str::to_owned);
                recorded_by_nanocodex = payload
                    .get("originator")
                    .and_then(serde_json::Value::as_str)
                    == Some("nanocodex");
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
                let target_transcript = if active_turn {
                    &mut pending_transcript
                } else {
                    &mut transcript
                };
                if let Some(item) = visible_tool_call(&value["payload"]) {
                    target_transcript.push(item);
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
                if active_turn {
                    pending_history.push(item);
                } else {
                    history.push(item);
                }
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
                pending_history.clear();
                context_baseline = None;
                pending_context_baseline = None;
            }
            Some("turn_context") => {
                if let Some(selected) = value["payload"]["model"]
                    .as_str()
                    .and_then(|model| model.parse().ok())
                {
                    if active_turn {
                        pending_model = Some(selected);
                    } else {
                        model = selected;
                    }
                }
            }
            Some("world_state") => {
                if let Some(state) = value["payload"]["state"].get("nanocodex_context") {
                    let state = serde_json::from_value(state.clone()).map_err(|error| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "failed to decode context snapshot at {} line {}: {error}",
                                path.display(),
                                index + 1
                            ),
                        )
                    })?;
                    if active_turn {
                        pending_context_baseline = Some(state);
                    } else {
                        context_baseline = Some(state);
                    }
                }
            }
            Some("event_msg") => {
                let payload = &value["payload"];
                match payload.get("type").and_then(serde_json::Value::as_str) {
                    Some("task_started") => {
                        active_turn = true;
                        pending_history.clear();
                        pending_transcript.clear();
                        pending_context_baseline = None;
                        pending_model = None;
                    }
                    Some("task_complete") => {
                        history.append(&mut pending_history);
                        transcript.append(&mut pending_transcript);
                        if let Some(baseline) = pending_context_baseline.take() {
                            context_baseline = Some(baseline);
                        }
                        if let Some(selected) = pending_model.take() {
                            model = selected;
                        }
                        active_turn = false;
                    }
                    Some("turn_aborted") => {
                        pending_history.clear();
                        pending_transcript.clear();
                        pending_context_baseline = None;
                        pending_model = None;
                        active_turn = false;
                    }
                    _ => {
                        let target = if active_turn {
                            &mut pending_transcript
                        } else {
                            &mut transcript
                        };
                        if let Some(item) = visible_rollout_event(payload) {
                            target.push(item);
                        }
                    }
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
        workspace,
        base_instructions,
        history,
        transcript,
        context_baseline,
        recorded_by_nanocodex,
    })
}

struct MaterializedRollout {
    model: Model,
    workspace: String,
    base_instructions: Option<String>,
    history: Vec<ResponseItem>,
    transcript: Vec<RolloutTranscriptItem>,
    context_baseline: Option<ContextBaseline>,
    recorded_by_nanocodex: bool,
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
