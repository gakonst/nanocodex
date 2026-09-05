use super::*;

/// Produces the compact JSON Schema shape used for macro-generated tools.
#[doc(hidden)]
#[must_use]
pub fn schema_for<T: JsonSchema>() -> Value {
    let schema = SchemaSettings::draft2019_09()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.option_add_null_type = false;
        })
        .into_generator()
        .into_root_schema_for::<T>();
    let Value::Object(mut schema) =
        serde_json::to_value(schema).expect("a schemars root schema should serialize to an object")
    else {
        unreachable!("a schemars root schema should be an object");
    };
    let mut tool_schema = Map::new();
    for key in [
        "properties",
        "required",
        "type",
        "additionalProperties",
        "$defs",
        "definitions",
        "enum",
        "const",
        "anyOf",
        "oneOf",
        "allOf",
    ] {
        if let Some(value) = schema.remove(key) {
            tool_schema.insert(key.to_owned(), value);
        }
    }
    Value::Object(tool_schema)
}
