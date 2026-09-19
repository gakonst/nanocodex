//! Definitions for the four managed Codex memory tools.
use crate::ToolDefinition;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Spec {
    name: String,
    description: String,
    parameters: Value,
    #[serde(rename = "outputSchema")]
    output_schema: Option<Value>,
}
/// Return the pinned definition used by the native managed-memory proxy.
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
