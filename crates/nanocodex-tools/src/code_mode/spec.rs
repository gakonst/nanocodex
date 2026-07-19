use nanocodex_core::{CustomToolFormat, ToolDefinition};
use serde_json::json;

use super::description;

const GRAMMAR: &str = r"start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE
PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/";

pub(crate) fn exec_spec(definitions: &[ToolDefinition]) -> ToolDefinition {
    ToolDefinition::custom(
        "exec",
        description::exec_description(definitions),
        CustomToolFormat::grammar("lark", GRAMMAR),
    )
}

pub(crate) fn wait_spec() -> ToolDefinition {
    ToolDefinition::function(
        "wait",
        "Waits on a yielded `exec` cell and returns new output or completion.\n- Use `wait` only after `exec` returns `Script running with cell ID ...`.\n- `cell_id` identifies the running `exec` cell to resume.\n- `yield_time_ms` controls how long to wait for more output before yielding again. Defaults to 10000 ms.\n- `max_tokens` limits how much new output this wait call returns. Defaults to 10000 tokens.\n- `terminate: true` stops the running cell; false or omitted waits for output.\n- `wait` returns only the new output since the last yield, or the final completion or termination result for that cell.\n- If the cell is still running, `wait` may yield again with the same `cell_id`.\n- If the cell has already finished, `wait` returns the completed result and closes the cell.",
        json!({
            "type": "object",
            "properties": {
                "cell_id": {
                    "type": "string",
                    "description": "Identifier of the running exec cell."
                },
                "yield_time_ms": {
                    "type": "number",
                    "description": "Wait before yielding more output. Defaults to 10000 ms."
                },
                "max_tokens": {
                    "type": "number",
                    "description": "Output token budget for this wait call. Defaults to 10000 tokens."
                },
                "terminate": {
                    "type": "boolean",
                    "description": "True stops the running cell; false or omitted waits for output."
                }
            },
            "required": ["cell_id"],
            "additionalProperties": false
        }),
    )
}
