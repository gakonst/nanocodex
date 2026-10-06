//! Pure task-board transitions; the embedding owns atomic checkpoint/receipt persistence.
use nanocodex_claude_tools::ClaudeTasks;
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(js_name = claudeTaskToolSchemas)]
pub fn schemas() -> String {
    serde_json::to_string(&ClaudeTasks::definitions()).expect("tool schemas are JSON values")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    name: String,
    input: Value,
    checkpoint: Option<Value>,
}

#[wasm_bindgen(js_name = claudeTaskToolPlan)]
pub async fn plan(request_json: String) -> Result<String, JsValue> {
    let request: Request = serde_json::from_str(&request_json)
        .map_err(|error| super::js_error(format!("invalid task request: {error}")))?;
    let board = ClaudeTasks::new();
    if let Some(checkpoint) = request.checkpoint {
        board.restore(checkpoint).map_err(super::js_error)?;
    }
    let output = board
        .execute(&request.name, request.input)
        .await
        .map_err(super::js_error)?;
    let checkpoint = board.snapshot().map_err(super::js_error)?;
    serde_json::to_string(&json!({ "output": output, "checkpoint": checkpoint }))
        .map_err(|error| super::js_error(error.to_string()))
}
