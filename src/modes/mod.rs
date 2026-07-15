mod fix_git;
mod phase0;

use std::{io::Write, time::Instant};

use clap::ValueEnum;

use crate::{
    Result,
    model::{self, ModelConfig},
    protocol::{EventWriter, Task},
};

#[derive(Clone, Copy, Default, ValueEnum)]
pub enum Mode {
    /// Exercise JSONL transport without a model or tools.
    #[default]
    Phase0,
    /// Known-positive control for Terminal-Bench's fix-git task.
    FixGitCheat,
    /// Run the task through `OpenAI`'s Responses API and local tools.
    Model,
}

pub(crate) async fn run<W: Write>(
    mode: Mode,
    events: &mut EventWriter<W>,
    task: &Task,
    model_config: &ModelConfig,
) -> Result<()> {
    match mode {
        Mode::Phase0 => phase0::run(events, task),
        Mode::FixGitCheat => fix_git::run(events, task),
        Mode::Model => model::run(events, task, model_config).await,
    }
}

fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}
