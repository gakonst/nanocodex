use schemars::r#gen::SchemaSettings;
use serde_json::{Map, Value};

use super::wire::SearchCommands;

pub(super) fn commands_schema() -> Value {
    let schema = SchemaSettings::draft2019_09()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.option_add_null_type = false;
        })
        .into_generator()
        .into_root_schema_for::<SearchCommands>();
    let schema = serde_json::to_value(schema).expect("web search command schema should serialize");
    let Value::Object(mut schema) = schema else {
        unreachable!("web search command schema must be an object");
    };
    let mut tool_schema = Map::new();
    for key in [
        "properties",
        "required",
        "type",
        "additionalProperties",
        "$defs",
        "definitions",
    ] {
        if let Some(value) = schema.remove(key) {
            tool_schema.insert(key.to_owned(), value);
        }
    }
    Value::Object(tool_schema)
}
