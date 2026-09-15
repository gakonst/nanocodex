//! Native selection of owned app-state DTOs before cell-realm deserialization.
//! Keys themselves stay in a private engine Set, under that engine's heap limit.
use crate::{Error, Result};
use serde_json::{Value, json};

const NO_SHOT: &str = "computer-use service did not return a screenshot";
const BAD_URL: &str = "computer-use service did not return a screenshot URL";
const BAD_TEXT: &str = "computer-use service did not return screenshot text";
const BAD_INSTRUCTIONS: &str = "computer-use service returned invalid app-specific instructions";
const OPEN: &str = "<app_specific_instructions>\n";
const CLOSE: &str = "\n</app_specific_instructions>\n";

#[derive(Clone, Copy, Default)]
pub(crate) struct Mode {
    mac: bool,
}
impl Mode {
    pub(crate) fn observe(&mut self, method: &str, result: &Result<Value>) {
        if method == "sky.setup"
            && let Ok(value) = result
        {
            self.mac = value.get("target").and_then(Value::as_str) == Some("mac");
        }
    }
    pub(crate) fn request<'a>(&self, method: &str, input: &'a Value) -> Result<Option<&'a str>> {
        if !self.mac
            || method != "sky.execute"
            || input.get("method").and_then(Value::as_str) != Some("get_app_state")
        {
            return Ok(None);
        }
        input
            .get("args")
            .and_then(Value::as_array)
            .and_then(|args| args.first())
            .and_then(|args| args.get("app"))
            .and_then(Value::as_str)
            .map(Some)
            .ok_or_else(|| Error::invalid("App-state request is missing its canonical app"))
    }
}

/// Validation errors used to be thrown by the JS formatter without an own code.
/// Keep them distinct from ordinary provider Errors (which have integer codes).
pub(crate) fn validation_response(message: &'static str) -> String {
    json!({"error":{"message":message,"code":null}}).to_string()
}

pub(crate) struct Plan<'a> {
    requested: &'a str,
    url: Option<String>,
    text: String,
    instructions: Option<String>,
    key: Option<String>,
}
fn take(value: &mut Value, field: &str) -> Value {
    value.get_mut(field).map(Value::take).unwrap_or(Value::Null)
}
fn nonempty(value: Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text),
        _ => None,
    }
}
impl<'a> Plan<'a> {
    pub(crate) fn new(
        requested: &'a str,
        mut value: Value,
    ) -> std::result::Result<Self, &'static str> {
        // serde_json selection sees only actual DTO fields, never JS inherited
        // properties. Null/scalar roots retain the current missing-shot error.
        let shot = value
            .get("skyshot")
            .filter(|v| !v.is_null())
            .ok_or(NO_SHOT)?;
        let url = shot.get("screenshot").and_then(|v| v.get("url"));
        if url.is_some_and(|v| !v.is_null() && !v.is_string()) {
            return Err(BAD_URL);
        }
        if !shot.get("text").is_some_and(Value::is_string) {
            return Err(BAD_TEXT);
        }
        if value
            .get("appSpecificInstructions")
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return Err(BAD_INSTRUCTIONS);
        }

        // Move the owned strings out of the DTO; no cloned tree/guidance DTO is
        // retained alongside the cache. Key selection is after all validation.
        let mut shot = take(&mut value, "skyshot");
        let url = nonempty(take(&mut take(&mut shot, "screenshot"), "url"));
        let Value::String(text) = take(&mut shot, "text") else {
            unreachable!("text was validated before any mutation")
        };
        let mut instructions = nonempty(take(&mut value, "appSpecificInstructions"));
        let key = if instructions.is_some() {
            let mut app = take(&mut value, "app");
            let bundle = nonempty(take(&mut app, "bundleIdentifier"));
            if bundle.as_deref() == Some("com.apple.iWork.Numbers") {
                instructions = None;
                None
            } else {
                bundle.or_else(|| nonempty(app))
            }
        } else {
            None
        };
        Ok(Self {
            requested,
            url,
            text,
            instructions,
            key,
        })
    }
    /// None means no cache query or insertion, including suppressed Numbers.
    pub(crate) fn key(&self) -> Option<&str> {
        self.instructions
            .as_ref()
            .map(|_| self.key.as_deref().unwrap_or(self.requested))
    }
    /// Call only after the engine cache has committed a newly seen key. A later
    /// allocation/output/serialization failure must not undo that insertion.
    pub(crate) fn finish(self, prepend: bool) -> Result<Value> {
        let text = if prepend {
            match self.instructions {
                Some(mut instructions) => {
                    let additional = OPEN
                        .len()
                        .checked_add(CLOSE.len())
                        .and_then(|n| n.checked_add(self.text.len()))
                        .ok_or_else(|| {
                            Error::action("App-state text exceeds the allocation range")
                        })?;
                    instructions
                        .try_reserve_exact(additional)
                        .map_err(|_| Error::action("Cannot allocate app-state text"))?;
                    instructions.insert_str(0, OPEN);
                    instructions.push_str(CLOSE);
                    instructions.push_str(&self.text);
                    instructions
                }
                None => self.text,
            }
        } else {
            self.text
        };
        let mut requested = String::new();
        requested
            .try_reserve_exact(self.requested.len())
            .map_err(|_| Error::action("Cannot allocate app-state app identifier"))?;
        requested.push_str(self.requested);
        Ok(json!({"app":requested,"screenshot":self.url.map(|url|json!({"url":url})),"text":text}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_app_state_validation_uses_dto_fields_in_original_order() {
        for value in [
            Value::Null,
            json!(false),
            json!(17),
            json!("x"),
            json!([]),
            json!({}),
            json!({"skyshot":null}),
        ] {
            assert_eq!(Plan::new("Owned", value).err(), Some(NO_SHOT));
        }
        for value in [json!(false), json!(17), json!("x"), json!([])] {
            assert_eq!(
                Plan::new(
                    "Owned",
                    json!({"skyshot":value,"appSpecificInstructions":17})
                )
                .err(),
                Some(BAD_TEXT)
            );
        }
        let mut value =
            json!({"skyshot":{"text":null,"screenshot":{"url":17}},"appSpecificInstructions":17});
        assert_eq!(Plan::new("Owned", value.clone()).err(), Some(BAD_URL));
        value["skyshot"]["screenshot"] = Value::Null;
        assert_eq!(Plan::new("Owned", value.clone()).err(), Some(BAD_TEXT));
        value["skyshot"]["text"] = json!("");
        assert_eq!(Plan::new("Owned", value).err(), Some(BAD_INSTRUCTIONS));
        for shot in [
            json!({"text":""}),
            json!({"text":"","screenshot":null}),
            json!({"text":"","screenshot":17}),
            json!({"text":"","screenshot":{"url":""}}),
        ] {
            assert_eq!(
                Plan::new("Owned", json!({"skyshot":shot}))
                    .unwrap()
                    .finish(false)
                    .unwrap(),
                json!({"app":"Owned","screenshot":null,"text":""})
            );
        }
    }

    #[test]
    fn native_app_state_no_instructions_or_numbers_never_requests_a_cache_key() {
        for guidance in [Value::Null, json!("")] {
            let plan = Plan::new("Owned", json!({"app":{"bundleIdentifier":"org.owned"},"skyshot":{"text":"tree"},"appSpecificInstructions":guidance})).unwrap();
            assert_eq!(plan.key(), None);
            assert_eq!(plan.finish(false).unwrap()["text"], "tree");
        }
        let value = json!({"app":{"bundleIdentifier":"com.apple.iWork.Numbers"},"skyshot":{"text":"tree"},"appSpecificInstructions":"guide"});
        let plan = Plan::new("Owned", value).unwrap();
        assert_eq!(plan.key(), None);
        assert_eq!(plan.finish(false).unwrap()["text"], "tree");
    }

    #[test]
    fn native_app_state_key_precedence_and_exact_prefix_are_preserved() {
        for (app, expected) in [
            (json!({"bundleIdentifier":"org.owned"}), "org.owned"),
            (json!("Alias"), "Alias"),
            (json!({"bundleIdentifier":""}), "Requested"),
            (json!({"bundleIdentifier":17}), "Requested"),
            (json!([]), "Requested"),
            (Value::Null, "Requested"),
        ] {
            let value = json!({"app":app,"skyshot":{"text":"tree","screenshot":{"url":"opaque:owned"}},"appSpecificInstructions":" \n"});
            let plan = Plan::new("Requested", value).unwrap();
            assert_eq!(plan.key(), Some(expected));
            assert_eq!(
                plan.finish(true).unwrap(),
                json!({"app":"Requested","screenshot":{"url":"opaque:owned"},"text":"<app_specific_instructions>\n \n\n</app_specific_instructions>\ntree"})
            );
        }
    }

    #[test]
    fn native_app_state_mode_uses_successful_setup_and_exact_native_request() {
        let input = json!({"method":"get_app_state","args":[{"app":"Canonical"}]});
        let mut mode = Mode::default();
        assert_eq!(mode.request("sky.execute", &input).unwrap(), None);
        mode.observe("sky.setup", &Ok(json!({"target":"mac"})));
        assert_eq!(
            mode.request("sky.execute", &input).unwrap(),
            Some("Canonical")
        );
        mode.observe("sky.setup", &Err(Error::action("owned provider failure")));
        assert_eq!(
            mode.request("sky.execute", &input).unwrap(),
            Some("Canonical")
        );
        assert_eq!(mode.request("sky.other", &input).unwrap(), None);
        assert_eq!(
            mode.request("sky.execute", &json!({"method":"click"}))
                .unwrap(),
            None
        );
        assert!(
            mode.request("sky.execute", &json!({"method":"get_app_state"}))
                .is_err()
        );
        mode.observe("sky.setup", &Ok(json!({"target":"linux"})));
        assert_eq!(mode.request("sky.execute", &input).unwrap(), None);
    }
}
