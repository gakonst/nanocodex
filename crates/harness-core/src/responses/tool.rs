use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Model-visible tool definition carried by Responses Lite input.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolDefinition {
    Function {
        name: Box<str>,
        description: Box<str>,
        strict: bool,
        parameters: JsonSchema,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_schema: Option<JsonSchema>,
    },
    Custom {
        name: Box<str>,
        description: Box<str>,
        format: CustomToolFormat,
    },
}

impl ToolDefinition {
    #[must_use]
    pub fn function(
        name: impl Into<Box<str>>,
        description: impl Into<Box<str>>,
        parameters: impl Into<JsonSchema>,
    ) -> Self {
        Self::Function {
            name: name.into(),
            description: description.into(),
            strict: false,
            parameters: parameters.into(),
            output_schema: None,
        }
    }

    #[must_use]
    pub fn custom(
        name: impl Into<Box<str>>,
        description: impl Into<Box<str>>,
        format: CustomToolFormat,
    ) -> Self {
        Self::Custom {
            name: name.into(),
            description: description.into(),
            format,
        }
    }

    #[must_use]
    pub fn with_output_schema(mut self, output_schema: impl Into<JsonSchema>) -> Self {
        if let Self::Function {
            output_schema: current,
            ..
        } = &mut self
        {
            *current = Some(output_schema.into());
        }
        self
    }

    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Function { name, .. } | Self::Custom { name, .. } => name,
        }
    }

    #[must_use]
    pub fn description(&self) -> &str {
        match self {
            Self::Function { description, .. } | Self::Custom { description, .. } => description,
        }
    }

    #[must_use]
    pub const fn parameters(&self) -> Option<&JsonSchema> {
        match self {
            Self::Function { parameters, .. } => Some(parameters),
            Self::Custom { .. } => None,
        }
    }

    #[must_use]
    pub const fn output_schema(&self) -> Option<&JsonSchema> {
        match self {
            Self::Function { output_schema, .. } => output_schema.as_ref(),
            Self::Custom { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CustomToolFormat {
    #[serde(rename = "type")]
    pub kind: Box<str>,
    pub syntax: Box<str>,
    pub definition: Box<str>,
}

impl CustomToolFormat {
    #[must_use]
    pub fn grammar(syntax: impl Into<Box<str>>, definition: impl Into<Box<str>>) -> Self {
        Self {
            kind: "grammar".into(),
            syntax: syntax.into(),
            definition: definition.into(),
        }
    }
}

/// Open JSON Schema fragment used at the model/tool boundary.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub struct JsonSchema(Value);

impl JsonSchema {
    #[must_use]
    pub const fn as_value(&self) -> &Value {
        &self.0
    }
}

impl From<Value> for JsonSchema {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

/// Arbitrary JSON retained for protocol fields whose schema is provider-defined.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub struct JsonValue(Value);

impl JsonValue {
    #[must_use]
    pub const fn as_value(&self) -> &Value {
        &self.0
    }
}

impl From<Value> for JsonValue {
    fn from(value: Value) -> Self {
        Self(value)
    }
}
