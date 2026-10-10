//! The model-visible decision tool.

use std::sync::Arc;

use async_trait::async_trait;
use nanocodex_oai_tools::{Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult};
use serde_json::{Value, json};

use crate::{DecisionModel, DecisionRequest};

const DEFAULT_NAME: &str = "decide";

const DESCRIPTION: &str = "\
Ask a decision model typed questions about shared evidence. It returns \
probabilities, choices, and rubric scores instead of prose, and answers much \
faster than a generative model. Use it for classification, routing, \
filtering, and triage over text or images; do not use it to generate text, \
extract fields, or reason through multi-step problems.

Question types:
- predicate: probability (0 to 1) that the instructions' condition is true.
- choice: one value from 2 to 255 distinct, unordered options, with a \
probability for each option and a confidence.
- score: position on levels ordered lowest to highest; the score is the \
probability-weighted average of zero-based level indices, so it can fall \
between levels.

Give every question a unique name; answers come back in question order with \
that name. Put independent questions in one call. When a question depends on \
an earlier answer, make a separate call. Any answer may be a refusal. Phrase \
instructions around observable criteria, give choices distinct meanings, add \
a fallback choice such as \"other\" when options might not cover the input, \
and decide thresholds from the cost of false positives and false negatives.";

/// A tool that lets an agent ask a [`DecisionModel`] typed questions.
///
/// The tool is named `decide` by default. Its arguments are the JSON form of a
/// [`DecisionRequest`], and its result is the JSON form of the resulting
/// [`Decision`](crate::Decision). Invalid requests and provider failures
/// become failed tool results so the calling model can correct its arguments.
///
/// ```
/// use nanocodex::{Tools, tools::ToolsBuildError};
/// use nanocodex_decisions::{DecisionModel, DecisionTool};
///
/// fn agent_tools(model: impl DecisionModel + 'static) -> Result<Tools, ToolsBuildError> {
///     Tools::builder().tool(DecisionTool::new(model)).build()
/// }
/// ```
#[derive(Clone)]
pub struct DecisionTool {
    model: Arc<dyn DecisionModel>,
    name: String,
}

impl DecisionTool {
    /// Creates a `decide` tool backed by `model`.
    pub fn new(model: impl DecisionModel + 'static) -> Self {
        Self {
            model: Arc::new(model),
            name: DEFAULT_NAME.to_owned(),
        }
    }

    /// Registers the tool under another name, so one agent can hold tools
    /// backed by different decision models.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

impl std::fmt::Debug for DecisionTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionTool")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Tool for DecisionTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(self.name.as_str(), DESCRIPTION, input_schema())
            .with_output_schema(output_schema())
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let request: DecisionRequest = input.decode_json()?;
        let decision = self.model.decide(&request).await?;
        Ok(ToolOutput::json(&decision))
    }
}

/// Closed object schema with the given required properties.
fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

/// Arguments: the JSON form of [`DecisionRequest`].
fn input_schema() -> Value {
    let text = json!({ "type": "string" });
    let choice_value = json!({ "type": ["string", "boolean"] });
    let question = |kind: &str, options: Option<(&str, Value)>| {
        let mut properties = json!({
            "type": { "const": kind },
            "name": {
                "type": "string",
                "description": "Unique name that identifies this question's answer."
            },
            "instructions": {
                "type": "string",
                "description": "What to evaluate, phrased around observable criteria."
            }
        });
        let mut required = vec!["type", "name", "instructions"];
        if let Some((key, schema)) = options {
            properties[key] = schema;
            required.push(key);
        }
        object(properties, &required)
    };
    let choices = json!({
        "type": "array",
        "description": "Distinct options.",
        "minItems": 2,
        "maxItems": 255,
        "items": object(json!({ "value": choice_value, "description": text }), &["value"])
    });
    let levels = json!({
        "type": "array",
        "description": "Levels ordered from lowest (index 0) to highest.",
        "minItems": 1,
        "items": object(json!({ "label": text, "description": text }), &["label"])
    });
    let part = json!({
        "anyOf": [
            object(json!({ "type": { "const": "input_text" }, "text": text }), &["type", "text"]),
            object(
                json!({
                    "type": { "const": "input_image" },
                    "image_url": {
                        "type": "string",
                        "description": "A base64 data: URL or a public HTTP(S) URL."
                    },
                    "detail": { "enum": ["auto", "low", "high", "original"] }
                }),
                &["type", "image_url"],
            )
        ]
    });
    object(
        json!({
            "input": {
                "description": "Evidence shared by every question: text, ordered text and image parts, or a structured record.",
                "anyOf": [
                    text,
                    { "type": "array", "minItems": 1, "items": part },
                    { "type": "object" }
                ]
            },
            "questions": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "anyOf": [
                        question("predicate", None),
                        question("choice", Some(("choices", choices))),
                        question("score", Some(("levels", levels)))
                    ]
                }
            }
        }),
        &["input", "questions"],
    )
}

/// Result: the JSON form of [`Decision`](crate::Decision).
fn output_schema() -> Value {
    let unit = json!({ "type": "number", "minimum": 0, "maximum": 1 });
    let count = json!({ "type": "integer", "minimum": 0 });
    let choice_value = json!({ "type": ["string", "boolean"] });
    let answer = |kind: &str, mut properties: Value, fields: &[&str]| {
        properties["type"] = json!({ "const": kind });
        properties["name"] = json!({ "type": "string" });
        object(properties, &[&["type", "name"], fields].concat())
    };
    object(
        json!({
            "model": { "type": "string" },
            "answers": {
                "type": "array",
                "description": "One answer per question, in question order.",
                "items": {
                    "anyOf": [
                        answer("predicate", json!({ "probability": unit }), &["probability"]),
                        answer(
                            "choice",
                            json!({
                                "choice": choice_value,
                                "confidence": unit,
                                "probabilities": {
                                    "type": "array",
                                    "items": object(
                                        json!({ "value": choice_value, "probability": unit }),
                                        &["value", "probability"],
                                    )
                                }
                            }),
                            &["choice", "confidence", "probabilities"],
                        ),
                        answer(
                            "score",
                            json!({
                                "score": { "type": "number", "minimum": 0 },
                                "confidence": unit,
                                "probabilities": {
                                    "type": "array",
                                    "items": object(
                                        json!({
                                            "index": count,
                                            "label": { "type": "string" },
                                            "probability": unit
                                        }),
                                        &["index", "label", "probability"],
                                    )
                                }
                            }),
                            &["score", "confidence", "probabilities"],
                        ),
                        answer("refusal", json!({}), &[])
                    ]
                }
            },
            "usage": object(
                json!({ "input_tokens": count, "output_tokens": count }),
                &["input_tokens", "output_tokens"],
            )
        }),
        &["model", "answers"],
    )
}
