use std::path::Path;

use crate::NanocodexError;

#[expect(
    clippy::unnecessary_wraps,
    reason = "matches the native instruction-loader contract"
)]
pub(super) fn load_instructions(
    _workspace: &Path,
    _global_instructions: Option<&str>,
) -> Result<Option<String>, NanocodexError> {
    Ok(None)
}
