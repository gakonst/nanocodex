use std::collections::HashMap;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::value::{RawValue, to_raw_value};

use super::ResponsesError;

/// A request serialized once at the API boundary and ready for transport.
pub struct EncodedRequest(Box<RawValue>);

impl EncodedRequest {
    /// Serializes a request once into compact raw JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be serialized.
    pub fn new<T: Serialize + ?Sized>(request: &T) -> Result<Self, ResponsesError> {
        to_raw_value(request)
            .map(Self)
            .map_err(ResponsesError::EncodeRequest)
    }

    /// Borrows the compact serialized JSON request.
    #[must_use]
    pub fn raw(&self) -> &RawValue {
        &self.0
    }

    /// Returns the encoded request text without copying its allocation.
    #[must_use]
    pub fn into_string(self) -> String {
        String::from(Box::<str>::from(self.0))
    }
}

/// Validates an inbound event while preserving its raw JSON representation.
pub(crate) fn parse_raw_json(text: &str) -> Result<&RawValue, ResponsesError> {
    serde_json::from_str(text).map_err(ResponsesError::InvalidJson)
}

/// Decodes a previously validated raw event into a wire type.
pub(crate) fn decode_event<T: DeserializeOwned>(event: &RawValue) -> Result<T, ResponsesError> {
    serde_json::from_str(event.get()).map_err(|source| ResponsesError::InvalidPayload {
        source,
        event: event.get().to_owned(),
    })
}

pub(crate) fn turn_state_from_event(text: &str) -> Option<String> {
    if text.starts_with(r#"{"type":""#) && !text.starts_with(r#"{"type":"response.metadata""#) {
        return None;
    }
    let Ok(MetadataEvent::Metadata { headers }) = serde_json::from_str(text) else {
        return None;
    };
    headers.into_iter().find_map(|(name, value)| {
        name.eq_ignore_ascii_case(TURN_STATE_HEADER)
            .then_some(value)
    })
}

const TURN_STATE_HEADER: &str = "x-codex-turn-state";

#[derive(Deserialize)]
#[serde(tag = "type")]
enum MetadataEvent {
    #[serde(rename = "response.metadata")]
    Metadata {
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    #[serde(other)]
    Other,
}
