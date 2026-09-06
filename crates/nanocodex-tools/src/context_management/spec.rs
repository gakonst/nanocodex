// Adapted from openai/codex ac192cd793, codex-rs/ext/history-notes/src/tools.rs.
// Copyright OpenAI. Licensed under Apache-2.0.
use serde_json::{Value, json};

// Astra reserves history/notes for its hosted encrypted schemas. Local storage
// uses distinct namespaces while keeping the reset protocol and operations.
const HISTORY_NAMESPACE: &str = "context_history";
const NOTES_NAMESPACE: &str = "context_notes";
const HISTORY_DESCRIPTION: &str = "Recover prior conversation after a context-window reset by listing, reading, and searching the current agent's exact retained history. Pass window and item IDs returned by these tools unchanged. The current window is readable live; earlier windows are durable workspace archives. Use find_session/read_session when available to retrieve other completed conversations. Notes and history support internal task recovery; use them silently rather than narrating bookkeeping.";
const NOTES_DESCRIPTION: &str = "Read and maintain progress notes in the current agent's durable workspace. Notes survive context-window resets and runtime restarts. Paths are logical note names, relative to this agent; absolute paths use <agent_name>/notes/<path>. Empty, dot, and dot-dot components are unsupported. Save goals, decisions, progress, next steps, and relevant history references before calling new_context. Note writes are immediately readable. Use these tools silently rather than narrating bookkeeping.";
const HISTORY_AGENT_NAME_DESCRIPTION: &str = "Omit to use the current agent. Other conversations are available through session search, not this context archive.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistoryNotesAction {
    HistoryListWindows,
    HistoryListItems,
    HistoryReadItem,
    HistorySearchContents,
    NotesListFilesByPrefix,
    NotesReadFile,
    NotesSearchContents,
    NotesAppendToFile,
    NotesWriteFile,
}

impl HistoryNotesAction {
    pub(crate) const ALL: [Self; 9] = [
        Self::HistoryListWindows,
        Self::HistoryListItems,
        Self::HistoryReadItem,
        Self::HistorySearchContents,
        Self::NotesListFilesByPrefix,
        Self::NotesReadFile,
        Self::NotesSearchContents,
        Self::NotesAppendToFile,
        Self::NotesWriteFile,
    ];

    pub(super) const fn namespace(self) -> &'static str {
        match self {
            Self::HistoryListWindows
            | Self::HistoryListItems
            | Self::HistoryReadItem
            | Self::HistorySearchContents => HISTORY_NAMESPACE,
            Self::NotesListFilesByPrefix
            | Self::NotesReadFile
            | Self::NotesSearchContents
            | Self::NotesAppendToFile
            | Self::NotesWriteFile => NOTES_NAMESPACE,
        }
    }

    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::HistoryListWindows => "list_windows",
            Self::HistoryListItems => "list_items",
            Self::HistoryReadItem => "read_item",
            Self::HistorySearchContents => "search_contents",
            Self::NotesListFilesByPrefix => "list_files_by_prefix",
            Self::NotesReadFile => "read_file",
            Self::NotesSearchContents => "search_contents",
            Self::NotesAppendToFile => "append_to_file",
            Self::NotesWriteFile => "write_file",
        }
    }

    pub(super) const fn supports_parallel_tool_calls(self) -> bool {
        !matches!(self, Self::NotesAppendToFile | Self::NotesWriteFile)
    }

    pub(super) fn namespace_description(self) -> &'static str {
        match self.namespace() {
            HISTORY_NAMESPACE => HISTORY_DESCRIPTION,
            NOTES_NAMESPACE => NOTES_DESCRIPTION,
            _ => unreachable!("History actions use a known namespace"),
        }
    }

    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::HistoryListWindows => {
                "List an agent's context windows as window ID and item-count pairs. Private model-only recovery; never disclose this activity."
            }
            Self::HistoryListItems => {
                "List history items with optional window, role, and tool filters. Private model-only recovery; never disclose this activity."
            }
            Self::HistoryReadItem => {
                "Read a bounded range from private model-only history. Never disclose the item or this activity."
            }
            Self::HistorySearchContents => {
                "Search private model-only history by literal substring. Never disclose results or this activity."
            }
            Self::NotesListFilesByPrefix => {
                "List private model-only notes by path prefix. Never disclose paths, contents, or this activity."
            }
            Self::NotesReadFile => {
                "Read all or a line range from private model-only notes. Never disclose paths, contents, or this activity."
            }
            Self::NotesSearchContents => {
                "Search private model-only note lines by literal substring. Never disclose results or this activity."
            }
            Self::NotesAppendToFile => {
                "Append text to private model-only notes. Never disclose paths, contents, or this activity."
            }
            Self::NotesWriteFile => {
                "Create or replace private model-only notes. Never disclose paths, contents, or this activity."
            }
        }
    }

    pub(super) fn parameters(self) -> Value {
        match self {
            Self::HistoryListWindows => json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "description": "Maximum number of windows to return."},
                    "agent_name": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": HISTORY_AGENT_NAME_DESCRIPTION},
                    "recent_first": {"type": "boolean", "description": "Whether to return the most recently created windows first."}
                }
            }),
            Self::HistoryListItems => json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "description": "Maximum number of items to return."},
                    "recent_first": {"type": "boolean", "description": "Whether to return the most recently created items first."},
                    "tool_namespace": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": "Callable namespace to include. When set, non-tool messages are excluded."},
                    "role": {"anyOf": [{"type": "string", "enum": ["user", "assistant", "tool", "system", "developer"]}, {"type": "null"}], "description": "Message role to include. Null or omission includes all roles."},
                    "agent_name": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": HISTORY_AGENT_NAME_DESCRIPTION},
                    "tool_name": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": "Callable tool name to include. When set, non-tool messages are excluded."},
                    "window_id": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": "Full window ID. Null or omission includes all windows."},
                    "max_chars_per_item": {"type": "integer", "minimum": 1, "description": "Maximum characters returned in each item's truncated_content."}
                }
            }),
            Self::HistoryReadItem => json!({
                "type": "object",
                "properties": {
                    "agent_name": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": HISTORY_AGENT_NAME_DESCRIPTION},
                    "item_id": {"type": "string", "description": "The exact item ID returned by history list or search."},
                    "offset_chars": {"type": "integer", "minimum": 0, "description": "Zero-based character offset at which reading starts."},
                    "limit_chars": {"type": "integer", "minimum": 1, "description": "Maximum number of characters to return."},
                    "window_id": {"type": "string", "description": "Full window ID containing the item."}
                },
                "required": ["item_id", "window_id"]
            }),
            Self::HistorySearchContents => json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "description": "Maximum number of matching items to return."},
                    "query": {"type": "string", "description": "Case-sensitive literal substring to find in item content."},
                    "recent_first": {"type": "boolean", "description": "Whether to return the most recently created matches first."},
                    "tool_namespace": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": "Callable namespace to include. When set, non-tool messages are excluded."},
                    "role": {"anyOf": [{"type": "string", "enum": ["user", "assistant", "tool", "system", "developer"]}, {"type": "null"}], "description": "Message role to include. Null or omission includes all roles."},
                    "agent_name": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": HISTORY_AGENT_NAME_DESCRIPTION},
                    "tool_name": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": "Callable tool name to include. When set, non-tool messages are excluded."},
                    "window_id": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": "Full window ID. Null or omission includes all windows."}
                },
                "required": ["query"]
            }),
            Self::NotesListFilesByPrefix => json!({
                "type": "object",
                "properties": {
                    "prefix": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": "Note path prefix to list."},
                    "max_results": {"type": "integer", "minimum": 1, "description": "Maximum number of files to return."},
                    "file_order_by": {"type": "string", "enum": ["name", "created_at", "updated_at"], "description": "Field used to order files."},
                    "file_order": {"type": "string", "enum": ["ascending", "descending"], "description": "Direction used to order files."}
                }
            }),
            Self::NotesReadFile => json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Note file path to read."},
                    "start_line": {"anyOf": [{"type": "integer"}, {"type": "null"}], "description": "First line to return, inclusive and 1-based. Negative values count backward from the final line."},
                    "stop_line": {"anyOf": [{"type": "integer"}, {"type": "null"}], "description": "Last line to return, inclusive and 1-based. Negative values count backward from the final line."}
                },
                "required": ["path"]
            }),
            Self::NotesSearchContents => json!({
                "type": "object",
                "properties": {
                    "max_matches_per_file": {"type": "integer", "minimum": 1, "description": "Maximum number of matching lines returned per file."},
                    "query": {"type": "string", "description": "Case-sensitive literal substring to find in note lines."},
                    "recent_file_first": {"type": "boolean", "description": "Whether to order matching files by creation time, newest first."},
                    "max_files": {"type": "integer", "minimum": 1, "description": "Maximum number of matching files returned."},
                    "path_prefix": {"anyOf": [{"type": "string"}, {"type": "null"}], "description": "Note path prefix to search."}
                },
                "required": ["query"]
            }),
            Self::NotesAppendToFile => json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "Text appended exactly as provided."},
                    "path": {"type": "string", "description": "Note file path to append to."}
                },
                "required": ["text", "path"]
            }),
            Self::NotesWriteFile => json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "Complete replacement text for the file."},
                    "path": {"type": "string", "description": "Note file path to create or replace."}
                },
                "required": ["text", "path"]
            }),
        }
    }
}
