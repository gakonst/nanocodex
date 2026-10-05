//! Upstream MCP meta-tools: `search_tool` and inline `use_tool` with
//! `server__tool` routing. The provider supplies a fresh authorized catalog on
//! every invocation; discovery is not authorization. No ambient server launch,
//! credential handling or remote transport is installed.
use crate::host::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpToolDefinition {
    pub server: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}
impl McpToolDefinition {
    pub fn qualified_name(&self) -> Result<String, String> {
        let segment = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        };
        let qualified = format!("{}__{}", self.server, self.name);
        if !segment(&self.server)
            || !segment(&self.name)
            || self.server.as_bytes()[0].is_ascii_digit()
            || qualified.len() > 256
            || qualified
                .as_bytes()
                .windows(2)
                .filter(|w| *w == b"__")
                .count()
                != 1
            || !self.input_schema.is_object()
        {
            return Err("invalid or ambiguous MCP catalog entry".into());
        }
        Ok(qualified)
    }
}
pub trait XaiMcpProvider: Send + Sync + 'static {
    fn catalog(&self, context: HostContext) -> HostFuture<Result<Vec<McpToolDefinition>, String>>;
    /// Must reauthorize the exact entry and preserve remote isError/media.
    fn call(
        &self,
        request: HostRequest,
        tool: McpToolDefinition,
    ) -> HostFuture<Result<ToolOutput, String>>;
}
pub struct XaiMcp<P: XaiMcpProvider + ?Sized> {
    provider: Arc<P>,
}
impl<P: XaiMcpProvider + ?Sized> XaiMcp<P> {
    pub const fn new(provider: Arc<P>) -> Self {
        Self { provider }
    }
}
impl<P: XaiMcpProvider + ?Sized> XaiHost for XaiMcp<P> {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![
            definition(
                "search_tool",
                "Discover currently authorized MCP tools by server, name and description keywords.",
                json!({"query":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20,"default":5}}),
                &["query"],
            ),
            definition(
                "use_tool",
                "Call a currently authorized MCP tool using its server__tool name and JSON arguments. Only inline input is supported.",
                json!({"tool_name":{"type":"string"},"tool_input":{"type":"object"}}),
                &["tool_name", "tool_input"],
            ),
        ]
    }
    fn call(&self, request: HostRequest) -> HostFuture<Result<ToolOutput, String>> {
        let provider = self.provider.clone();
        Box::pin(async move {
            validate_request(&request)?;
            let catalog = provider.catalog(request.context.clone()).await?;
            if catalog.len() > 4096
                || serde_json::to_vec(&catalog)
                    .map_err(|e| e.to_string())?
                    .len()
                    > 4 * 1024 * 1024
            {
                return Err("MCP catalog exceeds limits".into());
            }
            let mut names = std::collections::HashSet::new();
            let mut entries = Vec::new();
            for tool in catalog {
                let name = tool.qualified_name()?;
                if !names.insert(name.clone()) {
                    return Err("duplicate MCP catalog entry".into());
                }
                entries.push((name, tool));
            }
            match request.tool.as_str() {
                "search_tool" => {
                    fields(&request.input, &["query", "limit"])?;
                    let query = string(&request.input, "query")?.to_lowercase();
                    let limit = number(&request.input, "limit", 5, 20)? as usize;
                    if limit == 0 {
                        return Err("limit must be positive".into());
                    }
                    let terms: Vec<_> = query.split_whitespace().collect();
                    let mut ranked: Vec<_> = entries
                        .into_iter()
                        .filter_map(|(name, tool)| {
                            let haystack = format!("{} {}", name, tool.description).to_lowercase();
                            let score = terms
                                .iter()
                                .filter(|term| haystack.contains(**term))
                                .count();
                            (score > 0 || terms.is_empty()).then_some((score, name, tool))
                        })
                        .collect();
                    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                    let results:Vec<_>=ranked.into_iter().take(limit).map(|(_,name,tool)|json!({"tool_name":name,"description":tool.description,"input_schema":tool.input_schema})).collect();
                    let data = json!({"tools":results});
                    Ok(ToolOutput::text(data.to_string()).with_structured_result(data))
                }
                "use_tool" => {
                    fields(&request.input, &["tool_name", "tool_input"])?;
                    let name = string(&request.input, "tool_name")?;
                    let (_, tool) = entries
                        .into_iter()
                        .find(|(qualified, _)| qualified == name)
                        .ok_or("MCP tool is absent from the current authorized catalog")?;
                    let input = request.input["tool_input"].clone();
                    if !input.is_object() {
                        return Err("tool_input must be a JSON object".into());
                    }
                    provider
                        .call(
                            HostRequest {
                                context: request.context,
                                tool: name.to_owned(),
                                input,
                            },
                            tool,
                        )
                        .await
                }
                _ => Err("MCP adapter tool not installed".into()),
            }
        })
    }
}
