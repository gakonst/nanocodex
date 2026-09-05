use std::path::PathBuf;

use clap::Args;
use eyre::{Result, WrapErr as _};
use nanocodex_eval::EvaluationObserver;

use super::profile::default_state_dir;

#[derive(Args)]
pub(super) struct Attach {
    /// Initialized evaluation profile to observe.
    profile: String,

    /// Directory containing the profile's state.sqlite3 ledger.
    ///
    /// Defaults to ~/.nanocodex/evals.
    #[arg(long, value_name = "DIRECTORY")]
    state_dir: Option<PathBuf>,
}

impl Attach {
    pub(super) async fn run(self) -> Result<()> {
        let state_dir = self.state_dir.map_or_else(default_state_dir, Ok)?;
        let observer = EvaluationObserver::open(&state_dir, &self.profile).wrap_err_with(|| {
            format!(
                "failed to attach to evaluation profile `{}` in {}",
                self.profile,
                state_dir.display()
            )
        })?;
        crate::tui::attach_evaluation(observer).await
    }
}
