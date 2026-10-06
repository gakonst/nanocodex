//! Pure notebook transformations; mutations are returned only after all validation.
use crate::portable_files::MAX_FILE as MAX_NOTEBOOK;
use serde_json::{Value, json};
use std::path::Path;
fn field<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing or invalid {key}"))
}
pub fn definitions() -> Vec<Value> {
    vec![json!({
        "name": "NotebookEdit",
        "description": "Replace, insert, or delete a cell in an existing Jupyter notebook. Inserts are after cell_id, or at the beginning when omitted; replacements and deletes require cell_id in this safe subset.",
        "input_schema": {
            "type": "object",
            "properties": {
                "notebook_path": {"type": "string", "description": "Path of an existing .ipynb notebook within the authorized workspace."},
                "new_source": {"type": "string", "description": "New cell source; required even for delete (ignored on delete)."},
                "cell_id": {"type": "string", "description": "Existing cell ID (or zero-based cell index as a string)."},
                "cell_type": {"type": "string", "enum": ["code", "markdown"]},
                "edit_mode": {"type": "string", "enum": ["replace", "insert", "delete"], "default": "replace"}
            },
            "required": ["notebook_path", "new_source"],
            "additionalProperties": false
        }
    })]
}
pub fn edit(input: &Value, before: &[u8]) -> Result<(Vec<u8>, Value), String> {
    let fields = input
        .as_object()
        .ok_or("NotebookEdit input must be an object")?;
    if let Some(key) = fields.keys().find(|key| {
        !matches!(
            key.as_str(),
            "notebook_path" | "new_source" | "cell_id" | "cell_type" | "edit_mode"
        )
    }) {
        return Err(format!("unsupported NotebookEdit option: {key}"));
    }
    let path = Path::new(field(input, "notebook_path")?);
    if path.extension().is_none_or(|ext| ext != "ipynb") {
        return Err("notebook_path must end in .ipynb".into());
    }
    let new_source = field(input, "new_source")?;
    if new_source.len() > 32 * 1024 {
        return Err("new_source exceeds 32 KiB output-safe limit".into());
    }
    let mode = input
        .get("edit_mode")
        .map_or(Some("replace"), Value::as_str)
        .ok_or("invalid edit_mode")?;
    if !matches!(mode, "replace" | "insert" | "delete") {
        return Err("edit_mode must be replace, insert, or delete".into());
    }
    let cell_type = input
        .get("cell_type")
        .map(Value::as_str)
        .transpose_option("cell_type")?;
    if let Some(t) = cell_type
        && !matches!(t, "code" | "markdown")
    {
        return Err("cell_type must be code or markdown".into());
    }
    let id = input
        .get("cell_id")
        .map(Value::as_str)
        .transpose_option("cell_id")?;
    if id == Some("") {
        return Err("cell_id must not be empty".into());
    }

    if before.len() > MAX_NOTEBOOK {
        return Err("notebook exceeds 1 MiB limit".into());
    }
    let mut notebook: Value =
        serde_json::from_slice(&before).map_err(|e| format!("invalid notebook JSON: {e}"))?;
    if notebook.get("nbformat").and_then(Value::as_u64) != Some(4) {
        return Err("NotebookEdit supports nbformat 4 notebooks".into());
    }
    let language = notebook
        .pointer("/metadata/language_info/name")
        .or_else(|| notebook.pointer("/metadata/kernelspec/language"))
        .or_else(|| notebook.pointer("/metadata/kernelspec/name"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let cells = notebook
        .get_mut("cells")
        .and_then(Value::as_array_mut)
        .ok_or("notebook has no cells array")?;
    if mode != "insert" && id.is_none() {
        return Err("cell_id is required for replace and delete".into());
    }
    let index = if let Some(id) = id {
        // An exact ID takes precedence over index notation (including numeric IDs).
        cells
            .iter()
            .position(|cell| cell.get("id").and_then(Value::as_str) == Some(id))
            .or_else(|| id.parse::<usize>().ok().filter(|i| *i < cells.len()))
            .ok_or_else(|| format!("cell_id not found: {id}"))?
    } else {
        0
    };
    let (out_id, out_type, old_source) = match mode {
        "replace" => {
            let cell = cells
                .get_mut(index)
                .ok_or("notebook has no cell to replace")?;
            let obj = cell
                .as_object_mut()
                .ok_or("notebook cell is not an object")?;
            let old_type = obj
                .get("cell_type")
                .and_then(Value::as_str)
                .ok_or("cell has no cell_type")?;
            let old_type = old_type.to_owned();
            let kind = cell_type.unwrap_or(&old_type).to_owned();
            if !matches!(kind.as_str(), "code" | "markdown") {
                return Err("cell_type must be code or markdown".into());
            }
            let old_source = obj.get("source").map(source_text).unwrap_or_default();
            let out_id = obj.get("id").and_then(Value::as_str).map(str::to_owned);
            let as_string = obj.get("source").is_some_and(Value::is_string);
            obj.insert("source".into(), source_value(new_source, as_string));
            if kind != old_type {
                obj.insert("cell_type".into(), Value::String(kind.clone()));
                // nbformat code and markdown cells have different mandatory fields.
                if kind == "code" {
                    obj.remove("attachments");
                    obj.entry("execution_count").or_insert(Value::Null);
                    obj.entry("outputs").or_insert_with(|| json!([]));
                } else {
                    obj.remove("execution_count");
                    obj.remove("outputs");
                }
            }
            (out_id, kind, Some(old_source))
        }
        "insert" => {
            let kind = cell_type.ok_or("cell_type required for insert")?;
            let mut cell = json!({
                "cell_type": kind,
                "metadata": {},
                "source": source_value(new_source, false)
            });
            // nbformat 4.5+ requires a unique cell ID. Keep old notebook and
            // cell metadata untouched while assigning one to the new cell.
            let mut nonce = 0u64;
            let new_cell_id = loop {
                let candidate = format!("cell-{nonce:x}");
                nonce += 1;
                if !cells
                    .iter()
                    .any(|old| old.get("id").and_then(Value::as_str) == Some(&candidate))
                {
                    break candidate;
                }
            };
            cell["id"] = Value::String(new_cell_id.clone());
            if kind == "code" {
                cell["execution_count"] = Value::Null;
                cell["outputs"] = json!([]);
            }
            let at = if id.is_some() { index + 1 } else { 0 };
            cells.insert(at, cell);
            (Some(new_cell_id), kind.to_owned(), None)
        }
        "delete" => {
            if index >= cells.len() {
                return Err("notebook has no cell to delete".into());
            }
            let removed = cells.remove(index);
            let out_id = removed.get("id").and_then(Value::as_str).map(str::to_owned);
            let kind = removed
                .get("cell_type")
                .and_then(Value::as_str)
                .ok_or("deleted cell has no cell_type")?
                .to_owned();
            let old_source = removed.get("source").map(source_text).unwrap_or_default();
            (out_id, kind, Some(old_source))
        }
        _ => unreachable!(),
    };
    let mut output =
        serde_json::to_vec_pretty(&notebook).map_err(|e| format!("serialize notebook: {e}"))?;
    output.push(b'\n');
    if output.len() > MAX_NOTEBOOK {
        return Err("edited notebook exceeds 1 MiB limit".into());
    }
    let mut result = json!({"new_source":new_source,"cell_type":out_type,"language":language,"edit_mode":mode,"notebook_path":path.display().to_string()});
    if let Some(id) = out_id {
        result["cell_id"] = json!(id);
    }
    if let Some(old) = old_source {
        result["old_source"] = json!(old);
    }
    if result.to_string().len() > 64 * 1024 {
        return Err("NotebookEdit output exceeds 64 KiB".into());
    }

    Ok((output, result))
}
trait OptionalString<'a> {
    fn transpose_option(self, key: &str) -> Result<Option<&'a str>, String>;
}
impl<'a> OptionalString<'a> for Option<Option<&'a str>> {
    fn transpose_option(self, key: &str) -> Result<Option<&'a str>, String> {
        self.map_or(Ok(None), |value| {
            value.map(Some).ok_or_else(|| format!("invalid {key}"))
        })
    }
}

fn source_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

fn source_value(source: &str, as_string: bool) -> Value {
    if as_string {
        Value::String(source.to_owned())
    } else {
        Value::Array(
            source
                .split_inclusive('\n')
                .map(|s| Value::String(s.into()))
                .collect(),
        )
    }
}

use crate::portable_files::MAX_OUTPUT as MAX_TEXT;
use crate::{ImageSource, ToolOutput, ToolResultBlock};
use base64::{Engine as _, engine::general_purpose::STANDARD};
const MAX_IMAGE: usize = 5 * 1024 * 1024;
const MAX_MEDIA: usize = 20 * 1024 * 1024;
const MAX_BLOCKS: usize = 256;
fn notebook_text(value: &Value) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Array(parts) => parts
            .iter()
            .map(|p| {
                p.as_str()
                    .ok_or_else(|| "invalid notebook text array".to_string())
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|p| p.concat()),
        _ => Err("invalid notebook text".into()),
    }
}

fn push_text(
    blocks: &mut Vec<ToolResultBlock>,
    used: &mut usize,
    text: String,
) -> Result<(), String> {
    *used += text.len();
    if *used > MAX_TEXT || blocks.len() >= MAX_BLOCKS {
        return Err(
            "notebook output exceeds limit; use offset and limit to select fewer cells".into(),
        );
    }
    blocks.push(ToolResultBlock::Text { text });
    Ok(())
}

pub fn read(
    bytes: &[u8],
    input: &Value,
    image_block: impl Fn(&[u8]) -> Result<ToolResultBlock, String>,
) -> Result<ToolOutput, String> {
    let notebook: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid notebook JSON: {e}"))?;
    if notebook.get("nbformat").and_then(Value::as_u64) != Some(4) {
        return Err("Read supports nbformat 4 notebooks".into());
    }
    let cells = notebook
        .get("cells")
        .and_then(Value::as_array)
        .ok_or("notebook has no cells array")?;
    let offset = input
        .get("offset")
        .map_or(Some(1), Value::as_u64)
        .filter(|&n| n > 0)
        .ok_or("invalid offset")?;
    let limit = input
        .get("limit")
        .map_or(Some(2000), Value::as_u64)
        .filter(|&n| n > 0)
        .ok_or("invalid limit")?;
    let mut blocks = Vec::new();
    let mut text_bytes = 0;
    let mut media_bytes = 0;
    for (index, cell) in cells
        .iter()
        .enumerate()
        .skip(offset.saturating_sub(1).min(usize::MAX as u64) as usize)
        .take(limit.min(2000) as usize)
    {
        let kind = cell
            .get("cell_type")
            .and_then(Value::as_str)
            .ok_or("notebook cell has no cell_type")?;
        let id = cell
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| index.to_string());
        let source = notebook_text(cell.get("source").ok_or("notebook cell has no source")?)?;
        push_text(
            &mut blocks,
            &mut text_bytes,
            format!("Cell {id} ({kind}, index {index})\n{source}\n"),
        )?;
        if let Some(outputs) = cell.get("outputs") {
            for output in outputs
                .as_array()
                .ok_or("notebook outputs must be an array")?
            {
                if let Some(text) = output.get("text") {
                    push_text(&mut blocks, &mut text_bytes, notebook_text(text)?)?;
                }
                if output.get("output_type").and_then(Value::as_str) == Some("error") {
                    push_text(
                        &mut blocks,
                        &mut text_bytes,
                        format!(
                            "{}: {}\n{}\n",
                            output
                                .get("ename")
                                .and_then(Value::as_str)
                                .unwrap_or("Error"),
                            output.get("evalue").and_then(Value::as_str).unwrap_or(""),
                            output
                                .get("traceback")
                                .map(notebook_text)
                                .transpose()?
                                .unwrap_or_default()
                        ),
                    )?;
                }
                if let Some(data) = output.get("data").and_then(Value::as_object) {
                    let mut image = false;
                    for mime in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
                        if let Some(value) = data.get(mime) {
                            let encoded = notebook_text(value)?;
                            if encoded.len() > MAX_IMAGE * 4 / 3 + 1024 {
                                return Err("notebook image exceeds 5 MiB limit".into());
                            }
                            let encoded: String = encoded
                                .chars()
                                .filter(|c| !c.is_ascii_whitespace())
                                .collect();
                            let bytes = STANDARD
                                .decode(encoded)
                                .map_err(|e| format!("invalid notebook image base64: {e}"))?;
                            media_bytes += bytes.len();
                            if media_bytes > MAX_MEDIA || blocks.len() >= MAX_BLOCKS {
                                return Err(
                                    "notebook media output exceeds limit; select fewer cells"
                                        .into(),
                                );
                            }
                            let block = image_block(&bytes)?;
                            // Reject a declared type inconsistent with the actual payload.
                            if let ToolResultBlock::Image {
                                source: ImageSource::Base64 { media_type, .. },
                            } = &block
                                && media_type != mime
                            {
                                return Err(
                                    "notebook image MIME type does not match its bytes".into()
                                );
                            }
                            blocks.push(block);
                            image = true;
                            break;
                        }
                    }
                    if let Some(text) = data.get("text/plain") {
                        push_text(&mut blocks, &mut text_bytes, notebook_text(text)?)?;
                    } else if !image {
                        // Preserve HTML/JSON/SVG as labeled source, never pretend it is raster media.
                        for (mime, value) in data {
                            let text = if mime == "application/json" {
                                value.to_string()
                            } else if value.is_string() || value.is_array() {
                                notebook_text(value)?
                            } else {
                                value.to_string()
                            };
                            push_text(
                                &mut blocks,
                                &mut text_bytes,
                                format!("Output ({mime})\n{text}\n"),
                            )?;
                        }
                    }
                }
            }
        }
    }
    if blocks.is_empty() {
        blocks.push(ToolResultBlock::Text {
            text: "No notebook cells in the requested range.\n".into(),
        });
    }
    Ok(ToolOutput::content(blocks)
        .with_metadata(json!({"kind":"notebook","total_cells":cells.len(),"offset":offset})))
}

/// Portable text rendering fails explicitly when an actual image capability is needed.
pub fn read_text(bytes: &[u8], input: &Value) -> Result<String, String> {
    let result = read(bytes, input, |_| {
        Err(
            "Notebook image Read requires a host media capability; text fallback is forbidden"
                .into(),
        )
    })?;
    match result.content {
        crate::ToolContent::Text(text) => Ok(text),
        crate::ToolContent::Blocks(blocks) => {
            let mut text = String::new();
            for block in blocks {
                match block {
                    ToolResultBlock::Text { text: part } => text.push_str(&part),
                    _ => return Err("Read returned media".into()),
                }
            }
            Ok(text)
        }
    }
}
