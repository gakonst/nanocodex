//! macOS capture/input is implemented in the platform library linked into this Hand.
pub(crate) fn request(
    input: serde_json::Value,
) -> Result<serde_json::Value, nanocodex_managed::ManagedError> {
    nanocodex_hand::request(input)
        .map_err(|error| nanocodex_managed::ManagedError::Configuration(error.to_string()))
}
