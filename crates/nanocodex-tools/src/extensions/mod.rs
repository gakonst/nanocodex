//! Capability-bound skills, memories and goal extension tools.
//!
//! Providers are opt-in. Embedders bind one authorized account/store and verify
//! each call; model arguments cannot change the store or grant access.
#![allow(missing_docs)]
use crate::{Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, ToolsBuilder};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
pub mod goals;
pub mod memories;
pub mod skills;

/// Explicit provider and per-call authorization boundary for extension tools.
#[async_trait::async_trait]
pub trait ExtensionProvider: Send + Sync {
    fn names(&self) -> &'static [&'static str];
    async fn execute(&self, name: &str, input: Value, context: ToolContext<'_>) -> ToolResult;
}
/// Authorization is checked even when a provider serves a cached snapshot.
#[async_trait::async_trait]
pub trait ExtensionAuthorization: Send + Sync {
    async fn authorize(
        &self,
        name: &str,
        context: ToolContext<'_>,
    ) -> Result<(), crate::contract::ToolError>;
}
/// Install only the tools whose actual provider has been supplied.
pub fn install(
    mut tools: ToolsBuilder,
    provider: Arc<dyn ExtensionProvider>,
    authorization: Arc<dyn ExtensionAuthorization>,
) -> ToolsBuilder {
    for name in provider.names() {
        tools = tools.tool(ExtensionTool {
            name,
            provider: Arc::clone(&provider),
            authorization: Arc::clone(&authorization),
        });
    }
    tools
}
struct ExtensionTool {
    name: &'static str,
    provider: Arc<dyn ExtensionProvider>,
    authorization: Arc<dyn ExtensionAuthorization>,
}
#[derive(Deserialize)]
struct Spec {
    name: String,
    description: String,
    parameters: Value,
    #[serde(rename = "outputSchema")]
    output_schema: Option<Value>,
}
pub fn definition(name: &str) -> ToolDefinition {
    let specs: Vec<Spec> =
        serde_json::from_str(include_str!("specs.json")).expect("checked extension specifications");
    let spec = specs
        .into_iter()
        .find(|s| s.name == name)
        .expect("known extension tool");
    let definition = ToolDefinition::function(spec.name, spec.description, spec.parameters);
    if let Some(schema) = spec.output_schema {
        definition.with_output_schema(schema)
    } else {
        definition
    }
}
#[async_trait::async_trait]
impl Tool for ExtensionTool {
    fn definition(&self) -> ToolDefinition {
        definition(self.name)
    }
    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        self.authorization.authorize(self.name, context).await?;
        self.provider
            .execute(self.name, input.decode_json::<Value>()?, context)
            .await
    }
}
pub(crate) fn error(message: impl Into<String>) -> crate::contract::ToolError {
    std::io::Error::other(message.into()).into()
}
/// Pinned Codex middle truncation with its four-byte token approximation.
fn truncate_memory_text(text: &str, tokens: usize) -> String {
    let budget = tokens.saturating_mul(4);
    if text.len() <= budget {
        return text.to_owned();
    }
    let left = text.floor_char_boundary(budget / 2);
    let right = text.ceil_char_boundary(text.len().saturating_sub(budget - budget / 2));
    format!(
        "{}…{} tokens truncated…{}",
        &text[..left],
        text.len().saturating_sub(budget).div_ceil(4),
        &text[right..]
    )
}
/// Adapt any genuine memory backend, retaining its own private-store access rules.
pub struct MemoryTools<B: memories::MemoriesBackend>(pub B);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    path: Option<String>,
    cursor: Option<String>,
    max_results: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    line_offset: Option<usize>,
    max_lines: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    queries: Vec<String>,
    match_mode: Option<memories::SearchMatchMode>,
    path: Option<String>,
    cursor: Option<String>,
    context_lines: Option<usize>,
    case_sensitive: Option<bool>,
    normalized: Option<bool>,
    max_results: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteArgs {
    filename: String,
    note: String,
}
#[async_trait::async_trait]
impl<B: memories::MemoriesBackend> ExtensionProvider for MemoryTools<B> {
    fn names(&self) -> &'static [&'static str] {
        &[
            "memories__list",
            "memories__search",
            "memories__read",
            "memories__add_ad_hoc_note",
        ]
    }
    async fn execute(&self, name: &str, input: Value, _context: ToolContext<'_>) -> ToolResult {
        use memories::*;
        let result = match name {
            "memories__list" => {
                let a: ListArgs = serde_json::from_value(input)?;
                let limit = a.max_results.unwrap_or(2000).clamp(1, 2000);
                serde_json::to_value(
                    self.0
                        .list(ListMemoriesRequest {
                            path: a.path,
                            cursor: a.cursor,
                            max_results: limit,
                        })
                        .await?,
                )?
            }
            "memories__read" => {
                let a: ReadArgs = serde_json::from_value(input)?;
                serde_json::to_value(
                    self.0
                        .read(ReadMemoryRequest {
                            path: a.path,
                            line_offset: a.line_offset.unwrap_or(1),
                            max_lines: a.max_lines,
                            max_tokens: 20000,
                        })
                        .await?,
                )?
            }
            "memories__search" => {
                let a: SearchArgs = serde_json::from_value(input)?;
                serde_json::to_value(
                    self.0
                        .search(SearchMemoriesRequest {
                            queries: a.queries,
                            match_mode: a.match_mode.unwrap_or(SearchMatchMode::Any),
                            path: a.path,
                            cursor: a.cursor,
                            context_lines: a.context_lines.unwrap_or(0),
                            case_sensitive: a.case_sensitive.unwrap_or(true),
                            normalized: a.normalized.unwrap_or(false),
                            max_results: a.max_results.unwrap_or(200).clamp(1, 200),
                        })
                        .await?,
                )?
            }
            "memories__add_ad_hoc_note" => {
                let a: NoteArgs = serde_json::from_value(input)?;
                serde_json::to_value(
                    self.0
                        .add_ad_hoc_note(AddAdHocMemoryNoteRequest {
                            filename: a.filename,
                            note: a.note,
                        })
                        .await?,
                )?
            }
            _ => return Err(error("unknown memories tool")),
        };
        Ok(ToolOutput::json(&result))
    }
}
