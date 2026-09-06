//! Context archives and notes backed by the embedding's workspace.
use std::{future::Future, pin::Pin, sync::Arc};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use nanocodex_oai_api::responses::ResponseItem;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ContextWindow, spec::HistoryNotesAction};

pub type BackendFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Workspace operations, owned by the same host as the agent's files.
pub trait HistoryNotesHost: Send + Sync + 'static {
    fn available(&self, thread_id: String) -> BackendFuture<Result<bool, String>>;
    fn access(
        &self,
        thread_id: String,
        operation: StorageOperation,
    ) -> BackendFuture<Result<Value, String>>;
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum StorageOperation {
    Read { path: String },
    Write { path: String, contents: String },
    List { path: String },
}

#[derive(Clone)]
pub(super) struct Backend {
    pub(super) host: Arc<dyn HistoryNotesHost>,
    pub(super) session_id: String,
    pub(super) agent_name: String,
    pub(super) thread_id: String,
}

#[derive(Deserialize, Serialize)]
struct Archive {
    window: ContextWindow,
    items: Vec<Value>,
}

#[derive(Deserialize, Serialize)]
struct Note {
    text: String,
    created_at: u64,
    updated_at: u64,
}

impl Backend {
    fn root(&self) -> String {
        format!(
            ".nanocodex/context/{}/{}/",
            encode(&self.session_id),
            encode(&self.agent_name)
        )
    }

    async fn read(&self, path: String) -> Result<Option<String>, String> {
        let value = self
            .host
            .access(self.thread_id.clone(), StorageOperation::Read { path })
            .await?;
        if value.is_null() {
            Ok(None)
        } else {
            value
                .as_str()
                .map(|text| Some(text.to_owned()))
                .ok_or_else(|| "Context storage returned invalid text".into())
        }
    }

    async fn write(&self, path: String, contents: String) -> Result<(), String> {
        self.host
            .access(
                self.thread_id.clone(),
                StorageOperation::Write { path, contents },
            )
            .await?;
        Ok(())
    }

    async fn list(&self, path: String) -> Result<Vec<String>, String> {
        serde_json::from_value(
            self.host
                .access(self.thread_id.clone(), StorageOperation::List { path })
                .await?,
        )
        .map_err(|error| error.to_string())
    }

    pub(super) async fn archive(
        &self,
        window: &ContextWindow,
        history: &[ResponseItem],
    ) -> Result<String, String> {
        let archive_id = uuid::Uuid::new_v4().to_string();
        let archive = Archive {
            window: window.clone(),
            items: history
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<_, _>>()
                .map_err(|error| error.to_string())?,
        };
        self.write(
            format!("{}history/{}.json", self.root(), archive_id),
            serde_json::to_string(&archive).map_err(|error| error.to_string())?,
        )
        .await?;
        Ok(archive_id)
    }

    pub(super) async fn hint(&self) -> Result<String, String> {
        let paths = self.list(format!("{}notes", self.root())).await?;
        let names = paths
            .iter()
            .filter_map(|path| path.rsplit('/').next())
            .filter_map(|name| name.strip_suffix(".json"))
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(if names.is_empty() {
            String::new()
        } else {
            format!(
                "Saved progress notes: {}. Read the relevant note before continuing.",
                names.join(", ")
            )
        })
    }

    pub(super) async fn call(
        &self,
        action: HistoryNotesAction,
        args: Value,
        window: &ContextWindow,
        history: &[ResponseItem],
    ) -> Result<Value, String> {
        if action.namespace() == "context_history" {
            self.history(action, &args, window, history).await
        } else {
            self.notes(action, &args).await
        }
    }

    async fn history(
        &self,
        action: HistoryNotesAction,
        args: &Value,
        current: &ContextWindow,
        history: &[ResponseItem],
    ) -> Result<Value, String> {
        if args["agent_name"]
            .as_str()
            .is_some_and(|name| name != self.agent_name)
        {
            return Err(
                "History belongs to the current agent; use session search for other conversations"
                    .into(),
            );
        }
        let mut candidates = Vec::new();
        // Only archives named in committed context state are readable. A replaced
        // owner or a reset whose journal commit failed can leave no visible history.
        for archive_id in current.archives.values() {
            let text = self
                .read(format!("{}history/{archive_id}.json", self.root()))
                .await?
                .ok_or("Archived context window is missing")?;
            let archive: Archive =
                serde_json::from_str(&text).map_err(|error| error.to_string())?;
            candidates.push((
                archive.window.window_number,
                archive.window.context_window_id,
                archive.items.len(),
            ));
        }
        candidates.push((
            current.window_number,
            current.context_window_id.clone(),
            history.len(),
        ));
        candidates.sort_by_key(|(number, _, _)| *number);
        if args["recent_first"].as_bool().unwrap_or(false) {
            candidates.reverse();
        }
        let limit = count(args, "limit", 100);
        if action == HistoryNotesAction::HistoryListWindows {
            return Ok(
                json!({"windows": candidates.into_iter().take(limit).map(|(_, id, count)|
                json!({"window_id":id,"item_count":count})).collect::<Vec<_>>()}),
            );
        }
        let mut results = Vec::new();
        if limit == 0 && action != HistoryNotesAction::HistoryReadItem {
            return Ok(json!({"items":results}));
        }
        for (_, id, _) in candidates {
            if args["window_id"]
                .as_str()
                .is_some_and(|filter| filter != id)
            {
                continue;
            }
            let mut items: Vec<Value> = if id == current.context_window_id {
                history
                    .iter()
                    .map(serde_json::to_value)
                    .collect::<Result<_, _>>()
                    .map_err(|error| error.to_string())?
            } else {
                let text = self
                    .read(format!(
                        "{}history/{}.json",
                        self.root(),
                        current
                            .archives
                            .get(&id)
                            .ok_or("Archived context window is missing")?
                    ))
                    .await?
                    .ok_or("Archived context window is missing")?;
                serde_json::from_str::<Archive>(&text)
                    .map_err(|error| error.to_string())?
                    .items
            };
            let calls: std::collections::HashMap<String, Value> = items
                .iter()
                .filter(|item| item.get("name").is_some())
                .filter_map(|item| {
                    item["call_id"]
                        .as_str()
                        .map(|id| (id.to_owned(), item.clone()))
                })
                .collect();
            if args["recent_first"].as_bool().unwrap_or(false) {
                items.reverse();
            }
            for item in items {
                let item_id = item["id"].as_str().unwrap_or_default();
                let tool = item["call_id"]
                    .as_str()
                    .and_then(|id| calls.get(id))
                    .unwrap_or(&item);
                let role = item["role"].as_str().unwrap_or_else(|| {
                    if item["type"]
                        .as_str()
                        .is_some_and(|kind| kind.ends_with("_output"))
                    {
                        "tool"
                    } else {
                        "assistant"
                    }
                });
                let mut readable = item.clone();
                let mut media = Vec::new();
                for key in ["content", "output"] {
                    if let Some(parts) = readable.get_mut(key).and_then(Value::as_array_mut) {
                        for part in parts {
                            if matches!(
                                part["type"].as_str(),
                                Some("input_image" | "input_audio" | "encrypted_content")
                            ) {
                                media.push(part.clone());
                                *part = json!({"type":part["type"],"content":"Use context_history.read_item to recover this part."});
                            }
                        }
                    }
                }
                let content =
                    serde_json::to_string(&readable).map_err(|error| error.to_string())?;
                if action == HistoryNotesAction::HistoryReadItem {
                    if args["item_id"].as_str() != Some(item_id) {
                        continue;
                    }
                    let offset = count(args, "offset_chars", 0);
                    let length = count(args, "limit_chars", 10_000);
                    return Ok(json!({"window_id":id,"item_id":item_id,
                        "content":content.chars().skip(offset).take(length).collect::<String>(),
                        "total_chars":content.chars().count(),"media":media}));
                }
                if args["role"].as_str().is_some_and(|filter| filter != role)
                    || args["tool_name"]
                        .as_str()
                        .is_some_and(|filter| Some(filter) != tool["name"].as_str())
                    || args["tool_namespace"].as_str().is_some_and(|filter| {
                        tool["name"].as_str().is_none()
                            || filter != tool["namespace"].as_str().unwrap_or("functions")
                    })
                    || args["query"]
                        .as_str()
                        .is_some_and(|query| !content.contains(query))
                {
                    continue;
                }
                let max_chars = count(args, "max_chars_per_item", 1000);
                results.push(json!({"window_id":id,"item_id":item_id,"role":role,
                    "tool_name":tool.get("name"),"truncated_content":content.chars().take(max_chars).collect::<String>()}));
                if results.len() >= limit {
                    return Ok(json!({"items":results}));
                }
            }
        }
        if action == HistoryNotesAction::HistoryReadItem {
            Err("History item was not found".into())
        } else {
            Ok(json!({"items":results}))
        }
    }

    async fn notes(&self, action: HistoryNotesAction, args: &Value) -> Result<Value, String> {
        let root = format!("{}notes/", self.root());
        if let Some(path) = args["path"].as_str() {
            let path = self.note_path(path)?;
            let file = format!("{root}{}.json", encode(&path));
            let previous = self
                .read(file.clone())
                .await?
                .map(|text| serde_json::from_str::<Note>(&text).map_err(|error| error.to_string()))
                .transpose()?;
            if action == HistoryNotesAction::NotesReadFile {
                let note = previous.ok_or("Note was not found")?;
                let lines: Vec<_> = note.text.lines().collect();
                let line = |key: &str, default: usize| {
                    args[key].as_i64().map_or(default, |n| {
                        if n < 0 {
                            (lines.len() as i64 + n).max(0) as usize
                        } else {
                            usize::try_from(n.saturating_sub(1)).unwrap_or(usize::MAX)
                        }
                    })
                };
                let start = line("start_line", 0).min(lines.len());
                let end = line("stop_line", lines.len().saturating_sub(1))
                    .saturating_add(1)
                    .min(lines.len());
                let text = if args["start_line"].is_null() && args["stop_line"].is_null() {
                    note.text
                } else {
                    lines[start..end.max(start)].join("\n")
                };
                return Ok(json!({"path":path,"text":text}));
            }
            let text = args["text"].as_str().ok_or("Note text is required")?;
            let now = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_millis() as u64;
            let note = Note {
                text: if action == HistoryNotesAction::NotesAppendToFile {
                    format!(
                        "{}{text}",
                        previous.as_ref().map_or("", |note| note.text.as_str())
                    )
                } else {
                    text.to_owned()
                },
                created_at: previous.as_ref().map_or(now, |note| note.created_at),
                updated_at: now,
            };
            self.write(
                file,
                serde_json::to_string(&note).map_err(|error| error.to_string())?,
            )
            .await?;
            return Ok(json!({"path":path,"written":true}));
        }
        let prefix = args["prefix"]
            .as_str()
            .or_else(|| args["path_prefix"].as_str())
            .unwrap_or("");
        let prefix = if prefix.is_empty() {
            String::new()
        } else {
            let path = self.note_path(prefix.trim_end_matches('/'))?;
            if prefix.ends_with('/') {
                format!("{path}/")
            } else {
                path
            }
        };
        let mut files = Vec::new();
        for file in self.list(root).await? {
            let Some(name) = file
                .rsplit('/')
                .next()
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            let path = decode(name)?;
            if !path.starts_with(&prefix) {
                continue;
            }
            let text = self.read(file).await?.ok_or("Note was not found")?;
            let note: Note = serde_json::from_str(&text).map_err(|error| error.to_string())?;
            files.push((path, note));
        }
        files.sort_by(
            |(a, left), (b, right)| match args["file_order_by"].as_str() {
                Some("created_at") => left.created_at.cmp(&right.created_at).then(a.cmp(b)),
                Some("updated_at") => left.updated_at.cmp(&right.updated_at).then(a.cmp(b)),
                _ => a.cmp(b),
            },
        );
        if args["file_order"].as_str() == Some("descending") {
            files.reverse();
        }
        if action == HistoryNotesAction::NotesSearchContents {
            if args["recent_file_first"].as_bool().unwrap_or(false) {
                files.sort_by_key(|(_, note)| std::cmp::Reverse(note.created_at));
            }
            let query = args["query"].as_str().ok_or("Search query is required")?;
            let max_matches = count(args, "max_matches_per_file", 20);
            let matches: Vec<_> = files
                .into_iter()
                .filter_map(|(path, note)| {
                    let lines: Vec<_> = note
                        .text
                        .lines()
                        .enumerate()
                        .filter(|(_, line)| line.contains(query))
                        .take(max_matches)
                        .map(|(index, text)| json!({"line":index+1,"text":text}))
                        .collect();
                    (!lines.is_empty()).then(|| json!({"path":path,"matches":lines}))
                })
                .take(count(args, "max_files", 100))
                .collect();
            Ok(json!({"files":matches}))
        } else {
            Ok(
                json!({"files":files.into_iter().take(count(args, "max_results", 100))
                .map(|(path,note)|json!({"path":path,"created_at":note.created_at,"updated_at":note.updated_at})).collect::<Vec<_>>()}),
            )
        }
    }

    fn note_path(&self, path: &str) -> Result<String, String> {
        let relative = if path.starts_with('/') {
            path.strip_prefix(&format!("{}/notes/", self.agent_name))
                .ok_or("Notes belong to the current agent")?
        } else {
            path
        };
        if relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err("Note paths must contain nonempty names without dot components".into());
        }
        Ok(relative.to_owned())
    }
}

fn encode(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(value)
}
fn count(args: &Value, key: &str, default: usize) -> usize {
    args[key].as_u64().map_or(default, |value| {
        usize::try_from(value).unwrap_or(usize::MAX)
    })
}
fn decode(value: &str) -> Result<String, String> {
    String::from_utf8(
        URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}
