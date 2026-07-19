use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use bm25::{Document, Language, SearchEngine, SearchEngineBuilder};
use nanocodex_core::ToolDefinition;
use rmcp::model::Tool as RmcpTool;
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::sync::watch;

use crate::client::Client;

const DEFAULT_SEARCH_LIMIT: usize = 8;
const MAX_SEARCH_LIMIT: usize = 32;

pub(crate) struct ToolEntry {
    pub canonical_name: String,
    pub server_name: String,
    pub remote_name: String,
    pub definition: ToolDefinition,
    pub search_text: String,
    pub client: Client,
    pub timeout: Duration,
}

#[derive(Default)]
struct Catalog {
    entries: BTreeMap<String, Arc<ToolEntry>>,
    active: HashSet<String>,
    failures: BTreeMap<String, String>,
    search_index: Option<SearchIndex>,
}

struct SearchIndex {
    entries: Vec<Arc<ToolEntry>>,
    engine: SearchEngine<usize>,
}

pub(crate) struct ProviderState {
    catalog: Mutex<Catalog>,
    remaining: watch::Sender<usize>,
    discovery_timeout: Duration,
}

#[derive(Serialize)]
pub(crate) struct SearchResponse {
    tools: Vec<SearchTool>,
    pending_servers: usize,
    failed_servers: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct SearchTool {
    name: String,
    server: String,
    tool: String,
    description: String,
    input_schema: Value,
}

impl ProviderState {
    pub(crate) fn new(server_count: usize, discovery_timeout: Duration) -> Self {
        let (remaining, _) = watch::channel(server_count);
        Self {
            catalog: Mutex::new(Catalog::default()),
            remaining,
            discovery_timeout,
        }
    }

    pub(crate) fn complete_server(
        &self,
        server_name: &str,
        result: Result<Vec<ToolEntry>, String>,
    ) {
        let mut catalog = self.catalog();
        match result {
            Ok(entries) => {
                for entry in entries {
                    if catalog.entries.contains_key(&entry.canonical_name) {
                        catalog.failures.insert(
                            server_name.to_owned(),
                            format!(
                                "MCP tool name collision after normalization: `{}`",
                                entry.canonical_name
                            ),
                        );
                        continue;
                    }
                    catalog
                        .entries
                        .insert(entry.canonical_name.clone(), Arc::new(entry));
                }
            }
            Err(error) => {
                catalog.failures.insert(server_name.to_owned(), error);
            }
        }
        catalog.search_index = None;
        drop(catalog);
        self.remaining.send_modify(|remaining| {
            *remaining = remaining.saturating_sub(1);
        });
    }

    pub(crate) async fn search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<SearchResponse, String> {
        let query = query.trim();
        if query.is_empty() {
            return Err("query must not be empty".to_owned());
        }
        let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        if limit == 0 {
            return Err("limit must be greater than zero".to_owned());
        }
        self.wait_for_startup().await;
        let mut catalog = self.catalog();
        if catalog.search_index.is_none() {
            tracing::info!(
                target: "nanocodex_mcp",
                tool_count = catalog.entries.len(),
                "building MCP tool search index"
            );
            catalog.search_index = Some(SearchIndex::new(catalog.entries.values().cloned()));
        }
        let selected = catalog
            .search_index
            .as_ref()
            .map_or_else(Vec::new, |index| {
                index.search(query, limit.min(MAX_SEARCH_LIMIT))
            });
        for entry in &selected {
            catalog.active.insert(entry.canonical_name.clone());
        }
        tracing::debug!(
            target: "nanocodex_mcp",
            result_count = selected.len(),
            active_count = catalog.active.len(),
            "searched MCP tool catalog"
        );
        let tools = selected.iter().map(|entry| entry.summary()).collect();
        Ok(SearchResponse {
            tools,
            pending_servers: *self.remaining.borrow(),
            failed_servers: catalog.failures.clone(),
        })
    }

    pub(crate) fn available_definitions(&self) -> Vec<ToolDefinition> {
        let catalog = self.catalog();
        catalog
            .active
            .iter()
            .filter_map(|name| catalog.entries.get(name))
            .map(|entry| entry.definition.clone())
            .collect()
    }

    pub(crate) fn active_entry(&self, name: &str) -> Option<Arc<ToolEntry>> {
        let catalog = self.catalog();
        catalog
            .active
            .contains(name)
            .then(|| catalog.entries.get(name).cloned())
            .flatten()
    }

    async fn wait_for_startup(&self) {
        let mut remaining = self.remaining.subscribe();
        if *remaining.borrow() == 0 {
            return;
        }
        let wait = async {
            while *remaining.borrow_and_update() > 0 {
                if remaining.changed().await.is_err() {
                    break;
                }
            }
        };
        drop(tokio::time::timeout(self.discovery_timeout, wait).await);
    }

    fn catalog(&self) -> MutexGuard<'_, Catalog> {
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SearchIndex {
    fn new(entries: impl IntoIterator<Item = Arc<ToolEntry>>) -> Self {
        let entries = entries.into_iter().collect::<Vec<_>>();
        let documents = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| Document::new(index, entry.search_text.clone()));
        let engine =
            SearchEngineBuilder::<usize>::with_documents(Language::English, documents).build();
        Self { entries, engine }
    }

    fn search(&self, query: &str, limit: usize) -> Vec<Arc<ToolEntry>> {
        self.engine
            .search(query, limit)
            .into_iter()
            .filter_map(|result| self.entries.get(result.document.id).cloned())
            .collect()
    }
}

impl ToolEntry {
    pub(crate) fn new(
        server_name: &str,
        tool: &RmcpTool,
        client: Client,
        timeout: Duration,
    ) -> Self {
        let remote_name = tool.name.to_string();
        let canonical_name = canonical_tool_name(server_name, &remote_name);
        let description = tool.description.as_deref().unwrap_or_default().to_owned();
        let mut input_schema = tool.input_schema.as_ref().clone();
        if input_schema.get("properties").is_none_or(Value::is_null) {
            input_schema.insert("properties".to_owned(), Value::Object(Map::new()));
        }
        let definition = ToolDefinition::function(
            canonical_name.clone(),
            description.clone(),
            Value::Object(input_schema.clone()),
        )
        .with_output_schema(json!({
            "type": "object",
            "properties": {
                "content": { "type": "array", "items": { "type": "object" } },
                "structuredContent": tool.output_schema
                    .as_ref()
                    .map_or_else(|| json!({}), |schema| Value::Object(schema.as_ref().clone())),
                "isError": { "type": "boolean" },
                "_meta": { "type": "object" }
            },
            "required": ["content"],
            "additionalProperties": false
        }));
        let mut properties = input_schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        properties.sort();
        let search_text = [
            canonical_name.as_str(),
            server_name,
            remote_name.as_str(),
            tool.title.as_deref().unwrap_or_default(),
            description.as_str(),
            &properties.join(" "),
        ]
        .join(" ");
        Self {
            canonical_name,
            server_name: server_name.to_owned(),
            remote_name,
            definition,
            search_text,
            client,
            timeout,
        }
    }

    fn summary(&self) -> SearchTool {
        SearchTool {
            name: self.canonical_name.clone(),
            server: self.server_name.clone(),
            tool: self.remote_name.clone(),
            description: self.definition.description().to_owned(),
            input_schema: self
                .definition
                .parameters()
                .map_or(Value::Null, |schema| schema.as_value().clone()),
        }
    }
}

fn canonical_tool_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "mcp__{}__{}",
        normalize_name(server_name),
        normalize_name(tool_name)
    )
}

fn normalize_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_names_are_stable_and_javascript_safe() {
        assert_eq!(
            canonical_tool_name("Google Drive", "files/search"),
            "mcp__Google_Drive__files_search"
        );
    }
}
