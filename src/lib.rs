mod error;
mod model;
mod modes;
mod protocol;
mod responses;
mod shell;

use std::io::{BufRead, Write};

pub use error::{AgentError, HarnessError, ResponsesError, Result};
pub use model::{ModelConfig, ReasoningEffort};
pub use modes::Mode;
use protocol::{EventWriter, read_task_start};

/// Runtime configuration for one accepted JSONL request.
pub struct RunConfig {
    pub mode: Mode,
    pub model: ModelConfig,
}

/// Run one harness request from JSONL input to JSONL output.
///
/// # Errors
///
/// Returns an error when the input envelope is invalid, a mode fails, or an
/// output event cannot be written.
pub async fn run(input: impl BufRead, output: impl Write, config: RunConfig) -> Result<()> {
    let request = read_task_start(input)?;
    let mut events = EventWriter::new(output, request.request_id);
    modes::run(config.mode, &mut events, &request.task, &config.model).await
}
