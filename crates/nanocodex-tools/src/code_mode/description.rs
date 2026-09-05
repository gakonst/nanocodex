#[cfg(not(target_family = "wasm"))]
use std::{collections::BTreeSet, fmt::Write as _};

use nanocodex_oai_api::{responses::JsonSchema, tools::ToolDefinition};
use serde_json::Value;

#[cfg(not(target_family = "wasm"))]
const DEFERRED_NESTED_TOOLS_GUIDANCE: &str = r"Some deferred nested tools may be omitted from this description. They are still available on the global `tools` object and listed in `ALL_TOOLS`.
To find one, filter `ALL_TOOLS` by `name` and `description`.
Call independent nested tools concurrently in one cell with `Promise.all`.";
// Based on https://modelcontextprotocol.io/specification/draft/schema#calltoolresult.
#[cfg(not(target_family = "wasm"))]
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
#[cfg(not(target_family = "wasm"))]
const EXEC_DESCRIPTION: &str = r#"Run JavaScript code to orchestrate/compose tool calls
- Evaluates the provided JavaScript code in a fresh V8 isolate as an async module.
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
- `image(imageUrlOrItem: string | { image_url: string; detail?: "auto" | "low" | "high" | "original" | null } | ImageContent, detail?: "auto" | "low" | "high" | "original" | null)`: Appends an image item. `image_url` should be a base64-encoded `data:` URL. To forward an MCP tool image, pass an individual `ImageContent` block from `result.content`, for example `image(result.content[0])`. MCP image blocks may request detail with `_meta: { "codex/imageDetail": "original" }`. When provided, the second `detail` argument overrides any detail embedded in the first argument.
- `audio(audioUrlOrItem: string | { audio_url: string } | AudioContent)`: Appends an audio item. `audio_url` should be a base64-encoded `data:` URL. To forward an MCP tool audio block, pass an individual `AudioContent` block from `result.content`, for example `audio(result.content[0])`.
- `generatedImage(result: { image_url: string; output_hint?: string })`: Appends an image-generation result and its optional output hint. HTTP(S) URLs are not supported.
- `store(key: string, value: any)`: stores a serializable value under a string key for later `exec` calls in the same session.
- `load(key: string)`: returns the stored value for a string key, or `undefined` if it is missing.
- `notify(value: string | number | boolean | undefined | null)`: immediately injects an extra `custom_tool_call_output` for the current `exec` call. Values are stringified like `text(...)`.
- `setTimeout(callback: () => void, delayMs?: number)`: schedules a callback to run later and returns a timeout id. Pending timeouts do not keep `exec` alive by themselves; await an explicit promise if you need to wait for one.
- `clearTimeout(timeoutId?: number)`: cancels a timeout created by `setTimeout`.
- `ALL_TOOLS`: metadata for the enabled nested tools as `{ name, description }` entries.
- `yield_control()`: yields the accumulated output to the model immediately while the script keeps running."#;

#[cfg(not(target_family = "wasm"))]
pub(super) fn exec_description(
    definitions: &[ToolDefinition],
    provider_summaries: &[(String, String)],
    has_deferred_tools: bool,
    code_mode_only: bool,
) -> String {
    let mut description = EXEC_DESCRIPTION.to_owned();
    if !provider_summaries.is_empty() {
        description.push_str("\n\nAdditional runtime-provided nested tools:");
        for (name, summary) in provider_summaries {
            let _ = write!(description, "\n- `tools.{name}`: {}", summary.trim());
        }
        description.push_str(
            "\nInspect the matching `ALL_TOOLS` entry for complete guidance before using an unfamiliar runtime-provided tool.",
        );
    }
    if has_deferred_tools {
        let _ = write!(description, "\n\n{DEFERRED_NESTED_TOOLS_GUIDANCE}");
    }
    if !code_mode_only {
        return description;
    }
    if has_deferred_tools
        || definitions.iter().any(|spec| {
            spec.output_schema()
                .and_then(|schema| mcp_structured_content_schema(schema.as_value()))
                .is_some()
        })
    {
        let _ = write!(
            description,
            "\n\nShared MCP Types:\n```ts\n{MCP_TYPESCRIPT_PREAMBLE}\n```"
        );
    }
    let mut rendered_namespaces = BTreeSet::new();
    for spec in definitions {
        if let Some((namespace, _)) = code_mode_namespace_and_name(spec.name())
            && rendered_namespaces.insert(namespace)
        {
            let _ = write!(
                description,
                "\n\n## {namespace}\nTools in the {namespace} namespace."
            );
        }
        let Some(declaration) = exec_tool_declaration(spec) else {
            continue;
        };
        let global_name = normalize_identifier(spec.name());
        let heading = if global_name == spec.name() {
            format!("### `{global_name}`")
        } else {
            format!("### `{global_name}` (`{}`)", spec.name())
        };
        let _ = write!(
            description,
            "\n\n{heading}\n{}\n\n{declaration}",
            spec.description(),
        );
    }
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

#[cfg(not(target_family = "wasm"))]
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

fn render_json_schema_to_typescript(schema: &Value) -> String {
    match schema {
        Value::Bool(false) => "never".to_owned(),
        Value::Object(map) => {
            if let Some(value) = map.get("const") {
                return render_literal(value);
            }
            if let Some(values) = map.get("enum").and_then(Value::as_array) {
                let rendered = values.iter().map(render_literal).collect::<Vec<_>>();
                if !rendered.is_empty() {
                    return rendered.join(" | ");
                }
            }
            for key in ["anyOf", "oneOf"] {
                if let Some(variants) = map.get(key).and_then(Value::as_array) {
                    let rendered = variants
                        .iter()
                        .map(render_json_schema_to_typescript)
                        .collect::<Vec<_>>();
                    if !rendered.is_empty() {
                        return rendered.join(" | ");
                    }
                }
            }
            if let Some(variants) = map.get("allOf").and_then(Value::as_array) {
                let rendered = variants
                    .iter()
                    .map(render_json_schema_to_typescript)
                    .collect::<Vec<_>>();
                if !rendered.is_empty() {
                    return rendered.join(" & ");
                }
            }
            if let Some(schema_type) = map.get("type") {
                if let Some(types) = schema_type.as_array() {
                    let rendered = types
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|schema_type| render_type(map, schema_type))
                        .collect::<Vec<_>>();
                    if !rendered.is_empty() {
                        return rendered.join(" | ");
                    }
                }
                if let Some(schema_type) = schema_type.as_str() {
                    return render_type(map, schema_type);
                }
            }
            if map.contains_key("properties")
                || map.contains_key("additionalProperties")
                || map.contains_key("required")
            {
                return render_object(map);
            }
            if map.contains_key("items") || map.contains_key("prefixItems") {
                return render_array(map);
            }
            "unknown".to_owned()
        }
        _ => "unknown".to_owned(),
    }
}

fn render_type(map: &serde_json::Map<String, Value>, schema_type: &str) -> String {
    match schema_type {
        "string" => "string".to_owned(),
        "number" | "integer" => "number".to_owned(),
        "boolean" => "boolean".to_owned(),
        "null" => "null".to_owned(),
        "array" => render_array(map),
        "object" => render_object(map),
        _ => "unknown".to_owned(),
    }
}

fn render_array(map: &serde_json::Map<String, Value>) -> String {
    if let Some(items) = map.get("items") {
        return format!("Array<{}>", render_json_schema_to_typescript(items));
    }
    if let Some(items) = map.get("prefixItems").and_then(Value::as_array) {
        let items = items
            .iter()
            .map(render_json_schema_to_typescript)
            .collect::<Vec<_>>();
        if !items.is_empty() {
            return format!("[{}]", items.join(", "));
        }
    }
    "unknown[]".to_owned()
}

fn render_object(map: &serde_json::Map<String, Value>) -> String {
    let required = map
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let properties = map
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut properties = properties.iter().collect::<Vec<_>>();
    properties.sort_unstable_by_key(|(name, _)| *name);

    let multiline = properties.iter().any(|(_, value)| {
        value
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|description| !description.is_empty())
    });
    let mut lines = Vec::new();
    for (name, value) in properties {
        if let (true, Some(description)) =
            (multiline, value.get("description").and_then(Value::as_str))
        {
            for line in description
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
            {
                lines.push(format!("  // {line}"));
            }
        }
        let optional = if required.iter().any(|required| required == name) {
            ""
        } else {
            "?"
        };
        let indent = if multiline { "  " } else { "" };
        lines.push(format!(
            "{indent}{}{optional}: {};",
            render_property_name(name),
            render_json_schema_to_typescript(value)
        ));
    }

    if let Some(additional) = map.get("additionalProperties") {
        let property_type = match additional {
            Value::Bool(true) => Some("unknown".to_owned()),
            Value::Bool(false) => None,
            value => Some(render_json_schema_to_typescript(value)),
        };
        if let Some(property_type) = property_type {
            let indent = if multiline { "  " } else { "" };
            lines.push(format!("{indent}[key: string]: {property_type};"));
        }
    } else if lines.is_empty() {
        lines.push("[key: string]: unknown;".to_owned());
    }

    if multiline {
        lines.insert(0, "{".to_owned());
        lines.push("}".to_owned());
        lines.join("\n")
    } else if lines.is_empty() {
        "{}".to_owned()
    } else {
        format!("{{ {} }}", lines.join(" "))
    }
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

fn render_property_name(name: &str) -> String {
    if normalize_identifier(name) == name {
        name.to_owned()
    } else {
        serde_json::to_string(name).unwrap_or_else(|_| "\"unknown\"".to_owned())
    }
}

fn render_literal(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use nanocodex_oai_api::tools::ToolDefinition;
    use serde_json::json;

    use super::{exec_description, render_json_schema_to_typescript};
    use crate::code_mode_order::sort_definitions;

    #[test]
    fn renders_described_object_as_typescript() {
        let schema = json!({
            "type": "object",
            "properties": {
                "choice": {"type": "string", "enum": ["one", "two"]},
                "count": {"type": "integer", "description": "How many."}
            },
            "required": ["choice"],
            "additionalProperties": false
        });
        assert_eq!(
            render_json_schema_to_typescript(&schema),
            "{\n  choice: \"one\" | \"two\";\n  // How many.\n  count?: number;\n}"
        );
    }

    #[test]
    fn renders_nullable_schema_types() {
        let schema = json!({
            "type": ["array", "null"],
            "items": {"type": "string"}
        });
        assert_eq!(
            render_json_schema_to_typescript(&schema),
            "Array<string> | null"
        );
    }

    #[test]
    fn sorts_plain_tools_before_namespaces_and_renders_namespace_header() {
        let mut definitions = vec![
            ToolDefinition::function(
                "image_gen__imagegen",
                "Generate an image.",
                json!({"type": "object"}),
            ),
            ToolDefinition::function("write_stdin", "Write input.", json!({"type": "object"})),
            ToolDefinition::function("apply_patch", "Apply a patch.", json!({"type": "object"})),
        ];
        sort_definitions(&mut definitions);
        assert_eq!(
            definitions
                .iter()
                .map(ToolDefinition::name)
                .collect::<Vec<_>>(),
            ["apply_patch", "write_stdin", "image_gen__imagegen"]
        );

        let description = exec_description(&definitions, &[], false, true);
        let namespace = description
            .find("## image_gen\nTools in the image_gen namespace.")
            .unwrap();
        assert!(description.find("### `write_stdin`").unwrap() < namespace);
        assert!(namespace < description.find("### `image_gen__imagegen`").unwrap());
    }

    #[test]
    fn normal_code_mode_keeps_the_exec_description_terse() {
        let definitions = vec![ToolDefinition::function(
            "update_plan",
            "Update the plan.",
            json!({"type": "object"}),
        )];

        let description = exec_description(&definitions, &[], false, false);

        assert!(
            description.contains(
                "All nested tools are available on the global `tools` object, for example"
            )
        );
        assert!(description.contains(
            "`ALL_TOOLS`: metadata for the enabled nested tools as `{ name, description }` entries."
        ));
        assert!(!description.contains("### `update_plan`"));
        assert!(!description.contains("declare const tools"));
    }

    #[test]
    fn runtime_provider_summaries_are_visible_without_their_schemas() {
        let summaries = vec![(
            "browser".to_owned(),
            "Control the host-managed browser session.".to_owned(),
        )];

        let description = exec_description(&[], &summaries, false, true);

        assert!(description.contains("`tools.browser`: Control the host-managed browser session."));
        assert!(description.contains("Inspect the matching `ALL_TOOLS` entry"));
        assert!(!description.contains("declare const tools: { browser"));
    }
}
