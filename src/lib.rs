mod error;
mod model;
mod protocol;
mod responses;
mod shell;
mod tools;

use std::io::{BufRead, Write};

pub use error::{AgentError, HarnessError, ResponsesError, Result};
pub use model::{ModelConfig, ReasoningEffort};
use protocol::{EventWriter, read_task_start};

/// Run one harness request from JSONL input to JSONL output.
///
/// # Errors
///
/// Returns an error when the input envelope is invalid, a mode fails, or an
/// output event cannot be written.
pub async fn run(input: impl BufRead, output: impl Write, config: ModelConfig) -> Result<()> {
    let request = read_task_start(input)?;
    let mut events = EventWriter::new(output, request.request_id);
    model::run(&mut events, &request.task, &config).await
}
