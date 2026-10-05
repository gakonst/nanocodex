use std::{collections::BTreeSet, fmt::Write as _};

use nanocodex_oai_api::{responses::JsonSchema, tools::ToolDefinition};
use serde_json::Value;

mod schema_types;
use schema_types::render_json_schema_to_typescript;

const DEFERRED_NESTED_TOOLS_GUIDANCE: &str = r"Some deferred nested tools may be omitted from this description. They are still available on the global `tools` object and listed in `ALL_TOOLS`.
Use `searchTools(query)` to find tools and `describeTool(name)` or `describeNamespace(namespace)` for full guidance and schemas.";
// Based on https://modelcontextprotocol.io/specification/draft/schema#calltoolresult.
const MCP_TYPESCRIPT_PREAMBLE: &str = r#"type Role = "user" | "assistant";
type MetaObject = Record<string, unknown>;
type Annotations = {
  audience?: Role[];
  priority?: number;
  lastModified?: string;
};
type Icon = {
  src: string;
  mimeType?: string;
  sizes?: string[];
  theme?: "light" | "dark";
};
type TextResourceContents = {
  uri: string;
  mimeType?: string;
  _meta?: MetaObject;
  text: string;
};
type BlobResourceContents = {
  uri: string;
  mimeType?: string;
  _meta?: MetaObject;
  blob: string;
};
type TextContent = {
  type: "text";
  text: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ImageContent = {
  type: "image";
  data: string;
  mimeType: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type AudioContent = {
  type: "audio";
  data: string;
  mimeType: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ResourceLink = {
  icons?: Icon[];
  name: string;
  title?: string;
  uri: string;
  description?: string;
  mimeType?: string;
  annotations?: Annotations;
  size?: number;
  _meta?: MetaObject;
  type: "resource_link";
};
type EmbeddedResource = {
  type: "resource";
  resource: TextResourceContents | BlobResourceContents;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ContentBlock =
  | TextContent
  | ImageContent
  | AudioContent
  | ResourceLink
  | EmbeddedResource;
type CallToolResult<TStructured = { [key: string]: unknown }> = {
  _meta?: MetaObject;
  content: ContentBlock[];
  isError?: boolean;
  structuredContent?: TStructured;
  [key: string]: unknown;
};"#;
const EXEC_DESCRIPTION: &str = r#"Run JavaScript code to orchestrate/compose tool calls
- Evaluates the provided JavaScript code in a fresh JavaScript context as an async module.
- All nested tools are available on the global `tools` object, for example `await tools.exec_command(...)`. Tool names are exposed as normalized JavaScript identifiers, for example `await tools.mcp__ologs__get_profile(...)`.
- Nested tool methods take either a string or an object as their input argument.
- Nested tools return either an object or a string, based on the description.
- Runs raw JavaScript -- no Node, no file system, no network access, no console.
- Accepts raw JavaScript source text, not JSON, quoted strings, or markdown code fences.
- You may optionally start the tool input with a first-line pragma like `// @exec: {"yield_time_ms": 10000, "max_output_tokens": 1000}`.
- `yield_time_ms` asks `exec` to yield early if the script is still running. Defaults to 10000 ms.
- `max_output_tokens` sets the token budget for direct `exec` results. Defaults to 10000 tokens.
- When the JS code is fully evaluated, the isolate's lifetime ends and unawaited promises are silently discarded.

- Global helpers:
- `exit()`: Immediately ends the current script successfully (like an early return from the top level).
- `text(value: string | number | boolean | undefined | null)`: Appends a text item. Non-string values are stringified with `JSON.stringify(...)` when possible.
- `image(imageUrlOrItem: string | ({ image_url: string } | { file_id: string }) & { detail?: "auto" | "low" | "high" | "original" | null } | ImageContent, detail?: "auto" | "low" | "high" | "original" | null)`: Appends an image item. `image_url` must be a base64-encoded `data:` URL; `file_id` is an opaque provider image reference, never a local path. Provide exactly one. To forward an MCP tool image, pass an individual `ImageContent` block from `result.content`, for example `image(result.content[0])`. MCP image blocks may request detail with `_meta: { "codex/imageDetail": "original" }`. When provided, the second `detail` argument overrides any detail embedded in the first argument.
- `audio(audioUrlOrItem: string | { audio_url: string } | AudioContent)`: Appends an audio item. `audio_url` should be a base64-encoded `data:` URL. To forward an MCP tool audio block, pass an individual `AudioContent` block from `result.content`, for example `audio(result.content[0])`.
- `generatedImage(result: ({ image_url: string } | { file_id: string }) & { output_hint?: string })`: Appends an image-generation result and its optional output hint. HTTP(S) URLs are not supported.
- `store(key: string, value: any)`: stores a serializable value under a string key for later `exec` calls in the same session.
- `load(key: string)`: returns the stored value for a string key, or `undefined` if it is missing.
- `notify(value: string | number | boolean | undefined | null)`: immediately injects an extra `custom_tool_call_output` for the current `exec` call. Values are stringified like `text(...)`.
- `setTimeout(callback: () => void, delayMs?: number)`: schedules a callback to run later and returns a timeout id. Pending timeouts do not keep `exec` alive by themselves; await an explicit promise if you need to wait for one.
- `clearTimeout(timeoutId?: number)`: cancels a timeout created by `setTimeout`.
- `searchTools(query: string, options?: { limit?: number })`: searches the immutable callable catalog by every case-insensitive whitespace-separated term; returns name, callableName and description (default limit 10, range 1–100).
- `describeTool(name: string)`: returns full description, inputSchema, outputSchema, kind and callableName; accepts public or normalized name; unknown names return undefined.
- `describeNamespace(namespace: string)`: returns full definitions with that exact prefix before the first `__` or `.`; unknown namespaces return an empty array.
- `ALL_TOOLS`: metadata for the enabled nested tools as `{ name, description }` entries.
- `yield_control()`: yields the accumulated output to the model immediately while the script keeps running."#;

pub(crate) fn exec_description(
    definitions: &[ToolDefinition],
    provider_summaries: &[(String, String)],
    has_deferred_tools: bool,
    code_mode_only: bool,
    inline_docs_token_budget: usize,
) -> String {
    let mut description = EXEC_DESCRIPTION.to_owned();
    description.push_str("\n- `ALL_TOOLS` is the catalog of callable nested tools for this execution. A tool exposed separately by the host is not necessarily callable through `tools`; use its direct tool entry when it is absent from this catalog.");
    description.push_str("\n- Nanocodex extension: calling a missing nested tool rejects locally with `TOOL_NOT_AVAILABLE`; it does not dispatch a tool. An unfinished nested call may have executed even when its cell ends; an `outcome: unknown` receipt is not permission to retry it.");
    let mut inline = String::new();
    let byte_budget = inline_docs_token_budget.saturating_mul(4);
    let mut omitted = has_deferred_tools;
    for (name, summary) in provider_summaries {
        let entry = format!(
            "\n\nAdditional runtime-provided nested tool `tools.{name}`: {}",
            summary.trim()
        );
        if inline.len().saturating_add(entry.len()) <= byte_budget {
            inline.push_str(&entry);
        } else {
            omitted = true;
        }
    }
    if code_mode_only {
        let mut rendered_namespaces = BTreeSet::new();
        let mut rendered_mcp_types = false;
        for spec in definitions {
            let Some(declaration) = exec_tool_declaration(spec) else {
                continue;
            };
            let mut entry = String::new();
            let uses_mcp = spec
                .output_schema()
                .and_then(|schema| mcp_structured_content_schema(schema.as_value()))
                .is_some();
            if uses_mcp && !rendered_mcp_types {
                let _ = write!(
                    entry,
                    "\n\nShared MCP Types:\n```ts\n{MCP_TYPESCRIPT_PREAMBLE}\n```"
                );
            }
            let namespace =
                code_mode_namespace_and_name(spec.name()).map(|(namespace, _)| namespace);
            if let Some(namespace) = namespace
                && !rendered_namespaces.contains(namespace)
            {
                let _ = write!(
                    entry,
                    "\n\n## {namespace}\nTools in the {namespace} namespace."
                );
            }
            let global_name = normalize_identifier(spec.name());
            let heading = if global_name == spec.name() {
                format!("### `{global_name}`")
            } else {
                format!("### `{global_name}` (`{}`)", spec.name())
            };
            let _ = write!(
                entry,
                "\n\n{heading}\n{}\n\n{declaration}",
                spec.description()
            );
            if inline.len().saturating_add(entry.len()) <= byte_budget {
                inline.push_str(&entry);
                if let Some(namespace) = namespace {
                    rendered_namespaces.insert(namespace);
                }
                rendered_mcp_types |= uses_mcp;
            } else {
                omitted = true;
            }
        }
    }
    if omitted {
        let _ = write!(description, "\n\n{DEFERRED_NESTED_TOOLS_GUIDANCE}");
    }
    description.push_str(&inline);
    description
}

pub(crate) fn augment_definition_for_code_mode(mut definition: ToolDefinition) -> ToolDefinition {
    let Some(declaration) = exec_tool_declaration(&definition) else {
        return definition;
    };
    match &mut definition {
        ToolDefinition::Function { description, .. }
        | ToolDefinition::Custom { description, .. } => {
            *description = format!("{description}\n\n{declaration}").into();
        }
        ToolDefinition::Namespace { .. } | ToolDefinition::ToolSearch { .. } => {}
    }
    definition
}

fn exec_tool_declaration(spec: &ToolDefinition) -> Option<String> {
    let (input_name, input_type) = match spec {
        ToolDefinition::Function { .. } => (
            "args",
            spec.parameters()
                .map(JsonSchema::as_value)
                .map_or_else(|| "unknown".to_owned(), render_json_schema_to_typescript),
        ),
        ToolDefinition::Custom { .. } => ("input", "string".to_owned()),
        ToolDefinition::Namespace { .. } | ToolDefinition::ToolSearch { .. } => return None,
    };
    let output_type = match spec.output_schema().map(JsonSchema::as_value) {
        Some(schema) => match mcp_structured_content_schema(schema) {
            Some(structured) => {
                let structured = render_json_schema_to_typescript(structured);
                if structured == "unknown" {
                    "CallToolResult".to_owned()
                } else {
                    format!("CallToolResult<{structured}>")
                }
            }
            None => render_json_schema_to_typescript(schema),
        },
        None => "unknown".to_owned(),
    };
    let global_name = normalize_identifier(spec.name());
    Some(format!(
        "exec tool declaration:\n```ts\ndeclare const tools: {{ {global_name}({input_name}: {input_type}): Promise<{output_type}>; }};\n```"
    ))
}

fn code_mode_namespace_and_name(name: &str) -> Option<(&str, &str)> {
    let (namespace, name) = name.split_once("__")?;
    (!namespace.is_empty() && !name.is_empty()).then_some((namespace, name))
}

fn mcp_structured_content_schema(output_schema: &Value) -> Option<&Value> {
    let properties = output_schema.get("properties")?.as_object()?;
    let content_schema = properties.get("content")?.as_object()?;
    if content_schema.get("type").and_then(Value::as_str) != Some("array")
        || content_schema
            .get("items")
            .and_then(Value::as_object)
            .is_none_or(|items| items.get("type").and_then(Value::as_str) != Some("object"))
        || properties
            .get("isError")
            .and_then(Value::as_object)
            .is_none_or(|schema| schema.get("type").and_then(Value::as_str) != Some("boolean"))
        || properties
            .get("_meta")
            .and_then(Value::as_object)
            .is_none_or(|schema| schema.get("type").and_then(Value::as_str) != Some("object"))
    {
        return None;
    }
    Some(
        properties
            .get("structuredContent")
            .unwrap_or(&Value::Bool(true)),
    )
}

pub(crate) fn normalize_identifier(name: &str) -> String {
    let mut identifier = String::new();
    for (index, character) in name.chars().enumerate() {
        let valid = if index == 0 {
            character == '_' || character == '$' || character.is_ascii_alphabetic()
        } else {
            character == '_' || character == '$' || character.is_ascii_alphanumeric()
        };
        identifier.push(if valid { character } else { '_' });
    }
    if identifier.is_empty() {
        "_".to_owned()
    } else {
        identifier
    }
}
