//! Pure native Claude file tools for caller-authorized async workspaces.
use wasm_bindgen::prelude::*;

#[wasm_bindgen(js_name = claudeFileToolSchemas)]
pub fn schemas() -> String {
    serde_json::to_string(&nanocodex_claude_tools::portable_plan::schemas())
        .expect("tool definitions are JSON values")
}

#[wasm_bindgen(js_name = claudeFileToolPlan)]
pub fn plan(request_json: &str) -> Result<String, JsValue> {
    let request = serde_json::from_str(request_json)
        .map_err(|e| super::js_error(format!("invalid file plan request: {e}")))?;
    let result = nanocodex_claude_tools::portable_plan::plan(request).map_err(super::js_error)?;
    serde_json::to_string(&result).map_err(|e| super::js_error(e.to_string()))
}
