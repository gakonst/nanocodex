use std::fmt::Write as _;

use nanocodex_core::JsonSchema;
use serde_json::Value;

const EXEC_DESCRIPTION: &str = r#"Run JavaScript code to orchestrate/compose tool calls
- Evaluates the provided JavaScript in a fresh QuickJS context on a prewarmed embedded runtime. Yielded cells keep running independently until completion or termination.
- All nested tools are available on the global `tools` object, for example `await tools.exec_command(...)`.
- Nested tool methods take either a string or an object as their input argument.
- Nested tools return either an object or a string, based on the description.
- Node.js globals and modules such as `process`, `require`, and dynamic `import()` are unavailable. Use the provided tools for file-system, process, and network access.
- Accepts raw JavaScript source text, not JSON, quoted strings, or markdown code fences.
- You may optionally start the tool input with a first-line pragma like `// @exec: {"yield_time_ms": 10000, "max_output_tokens": 1000}`.
- `yield_time_ms` asks `exec` to yield early if the script is still running. Defaults to 10000 ms.
- `max_output_tokens` sets the token budget for direct `exec` results. Defaults to 10000 tokens.

- Global helpers:
- `exit()`: Immediately ends the current script successfully (like an early return from the top level).
- `text(value: string | number | boolean | undefined | null)`: Appends a text item. Non-string values are stringified with `JSON.stringify(...)` when possible.
- `image(imageUrlOrItem: string | { image_url: string; detail?: "auto" | "low" | "high" | "original" | null }, detail?: "auto" | "low" | "high" | "original" | null)`: Appends an image item. `image_url` must be a base64-encoded `data:` URL. When provided, the second `detail` argument overrides the detail embedded in the first argument.
- `generatedImage(result: { image_url: string; output_hint?: string })`: Appends an image-generation result and its optional output hint. HTTP(S) URLs are not supported.
- `store(key: string, value: any)`: stores a serializable value under a string key for later `exec` calls in the same session.
- `load(key: string)`: returns the stored value for a string key, or `undefined` if it is missing.
- `notify(value: string | number | boolean | undefined | null)`: immediately injects an extra `custom_tool_call_output` for the current `exec` call. Values are stringified like `text(...)`.
- `setTimeout(callback: () => void, delayMs?: number)`: schedules a callback to run later and returns a timeout id.
- `clearTimeout(timeoutId?: number)`: cancels a timeout created by `setTimeout`.
- `ALL_TOOLS`: metadata for the enabled nested tools as `{ name, description, kind }` entries.
- `yield_control()`: yields the accumulated output to the model immediately while the cell keeps running."#;

pub(super) fn exec_description(definitions: &[nanocodex_core::ToolDefinition]) -> String {
    let mut description = EXEC_DESCRIPTION.to_owned();
    for spec in definitions {
        let input_name = match &spec {
            nanocodex_core::ToolDefinition::Function { .. } => "args",
            nanocodex_core::ToolDefinition::Custom { .. } => "input",
        };
        let input_type = match &spec {
            nanocodex_core::ToolDefinition::Function { .. } => spec
                .parameters()
                .map(JsonSchema::as_value)
                .map_or_else(|| "unknown".to_owned(), render_json_schema_to_typescript),
            nanocodex_core::ToolDefinition::Custom { .. } => "string".to_owned(),
        };
        let output_type = spec
            .output_schema()
            .map(JsonSchema::as_value)
            .map_or_else(|| "unknown".to_owned(), render_json_schema_to_typescript);
        let global_name = normalize_identifier(spec.name());
        let _ = write!(
            description,
            "\n\n### `{global_name}`\n{}\n\nexec tool declaration:\n```ts\n\
declare const tools: {{ {global_name}({input_name}: {input_type}): Promise<{output_type}>; }};\n\
```",
            spec.description().trim(),
        );
    }
    description
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

fn normalize_identifier(name: &str) -> String {
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
    use serde_json::json;

    use super::render_json_schema_to_typescript;

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
}
