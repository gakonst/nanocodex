//! Output formats supported by the Responses API.
// Rust guideline compliant 2026-02-21

use serde::Serialize;

use super::JsonSchema;

/// A named JSON Schema enforced on model output.
///
/// Configure it with `OpenAiBuilder::strict_json_schema` to set the `text.format`
/// field on Responses requests. The wire format always enables strict validation.
///
/// # Examples
///
/// ```
/// use nanocodex_oai_api::responses::StrictJsonSchema;
/// use serde_json::json;
///
/// let format = StrictJsonSchema::new(
///     "deployment_answer",
///     json!({
///         "type": "object",
///         "properties": {"region": {"type": "string"}},
///         "required": ["region"],
///         "additionalProperties": false
///     }),
/// );
/// assert_eq!(format.name(), "deployment_answer");
/// ```
#[derive(Clone, Debug, Serialize)]
pub struct StrictJsonSchema {
    #[serde(rename = "type")]
    kind: &'static str,
    strict: bool,
    name: Box<str>,
    schema: JsonSchema,
}

impl StrictJsonSchema {
    /// Creates a strict JSON Schema output format with the given name and schema.
    ///
    /// # Examples
    ///
    /// ```
    /// use nanocodex_oai_api::responses::StrictJsonSchema;
    /// use serde_json::json;
    ///
    /// let format = StrictJsonSchema::new("answer", json!({"type": "string"}));
    /// assert_eq!(format.schema().as_value()["type"], "string");
    /// ```
    #[must_use]
    pub fn new(name: impl Into<Box<str>>, schema: impl Into<JsonSchema>) -> Self {
        Self {
            kind: "json_schema",
            strict: true,
            name: name.into(),
            schema: schema.into(),
        }
    }

    /// Returns the provider-visible schema name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the JSON Schema enforced on model output.
    #[must_use]
    pub const fn schema(&self) -> &JsonSchema {
        &self.schema
    }
}
