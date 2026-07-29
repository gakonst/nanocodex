use super::*;

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
    let mut history = Vec::new();
    let mut transcript = Vec::new();
    let mut context_baseline = None;
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
        workspace,
        base_instructions,
        history,
        transcript,
        context_baseline,
    })
}

struct MaterializedRollout {
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
