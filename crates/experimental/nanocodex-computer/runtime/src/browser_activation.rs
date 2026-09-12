//! Native WebMCP admission. Provider metadata and task metadata have separate
//! native owners; command arguments cannot supply either. No configuration
//! reader is installed until a compatible native settings owner is available.
use crate::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

const PREFERENCE_TTL_MS: f64 = 300_000.0;
const MAX_MODEL_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Unavailable {
    InvalidMetadata,
    ModelTooLarge,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Model {
    #[default]
    Compatible,
    Incompatible {
        model: String,
    },
    Unavailable {
        reason: Unavailable,
    },
}
impl<'de> Deserialize<'de> for Model {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Compatible {},
            Incompatible { model: String },
            Unavailable { reason: Unavailable },
        }
        match Wire::deserialize(deserializer)? {
            Wire::Compatible {} => Ok(Self::Compatible),
            Wire::Unavailable { reason } => Ok(Self::Unavailable { reason }),
            Wire::Incompatible { model }
                if model.len() <= MAX_MODEL_BYTES
                    && model.contains("-luna")
                    && model.to_lowercase() == model =>
            {
                Ok(Self::Incompatible { model })
            }
            Wire::Incompatible { .. } => Err(serde::de::Error::custom(
                "Invalid native WebMCP model projection",
            )),
        }
    }
}
impl Model {
    /// This string is captured by runtime_tasks::Context, not read from the
    /// current global metadata or the browser command's JSON arguments.
    pub(crate) fn from_task_metadata(metadata: &str) -> Self {
        let Ok(metadata) = serde_json::from_str::<Value>(metadata) else {
            return Self::Unavailable {
                reason: Unavailable::InvalidMetadata,
            };
        };
        let Some(model) = normalized_model(&metadata).filter(|model| model.contains("-luna"))
        else {
            return Self::Compatible;
        };
        if model.len() > MAX_MODEL_BYTES {
            return Self::Unavailable {
                reason: Unavailable::ModelTooLarge,
            };
        }
        Self::Incompatible { model }
    }
}
fn normalized_model(metadata: &Value) -> Option<String> {
    let value = metadata.get("x-codex-turn-metadata")?;
    let parsed;
    let value = if let Some(encoded) = value.as_str() {
        parsed = serde_json::from_str::<Value>(encoded).ok()?;
        &parsed
    } else {
        value
    };
    value
        .as_object()?
        .get("model")?
        .as_str()
        .map(str::to_lowercase)
}

type Reader = Box<dyn FnMut() -> Result<Value> + Send>;
pub(crate) struct Owner {
    reader: Option<Reader>,
    clock: Box<dyn FnMut() -> f64 + Send>,
    cached: Option<(Value, f64)>,
}
impl Default for Owner {
    fn default() -> Self {
        Self::new(None, || {
            match SystemTime::now().duration_since(UNIX_EPOCH) {
                Ok(elapsed) => elapsed.as_millis() as f64,
                Err(before) => (-before.duration().as_secs_f64() * 1000.0).floor(),
            }
        })
    }
}
impl Owner {
    pub(crate) fn new(reader: Option<Reader>, clock: impl FnMut() -> f64 + Send + 'static) -> Self {
        Self {
            reader,
            clock: Box::new(clock),
            cached: None,
        }
    }
    fn settings(&mut self, force: bool) -> Result<&Value> {
        if !force
            && let Some((_, expires)) = self.cached.as_ref()
            && (self.clock)() < *expires
        {
            return Ok(&self.cached.as_ref().unwrap().0);
        }
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| Error::action("Native browser preferences are unavailable"))?;
        // A failed refresh does not replace the previous entry. The clock read
        // for the new expiry occurs only after a successful read.
        let value = reader()?;
        let value = if value.is_object() {
            value
        } else {
            serde_json::json!({})
        };
        let expires = (self.clock)() + PREFERENCE_TTL_MS;
        self.cached = Some((value, expires));
        Ok(&self.cached.as_ref().unwrap().0)
    }
    fn enabled(&mut self) -> bool {
        self.settings(false).is_ok_and(|settings| {
            settings
                .get("webmcp_enabled")
                .is_none_or(|value| value.is_null() || value == true)
        })
    }
    pub(crate) fn admit(&mut self, model: &Model, info: &Value, command: &str) -> Result<()> {
        if let Model::Incompatible { model } = model {
            return Err(unsupported(model, command));
        }
        if let Model::Unavailable { reason } = model {
            return Err(Error::invalid(match reason {
                Unavailable::InvalidMetadata => "Invalid native task metadata for WebMCP",
                Unavailable::ModelTooLarge => "Native WebMCP model projection exceeds limit",
            }));
        }
        if !self.enabled()
            || !info["capabilities"]["tab"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == "webmcp"))
        {
            return Err(unsupported(
                info["name"].as_str().unwrap_or("undefined"),
                command,
            ));
        }
        Ok(())
    }
}
fn unsupported(owner: &str, command: &str) -> Error {
    Error::action(format!("{owner} does not support command \"{command}\"."))
}
pub(crate) fn command(method: &str) -> Option<&'static str> {
    match method {
        "webmcp_list" => Some("webmcp_list_tools"),
        "webmcp_invoke" => Some("webmcp_invoke_tool"),
        _ => None,
    }
}

#[cfg(test)]
#[path = "browser_activation_tests.rs"]
mod tests;
