//! `nanocodex cookies` in a build without the optional `browser` feature.
//!
//! The command copies cookies by driving a local Chromium-family browser with
//! the nanocodex-browser automation crate, which ordinary and release builds
//! omit. The command remains so that existing invocations get an actionable
//! error instead of an unknown-subcommand failure.

use std::ffi::OsString;

use clap::Args;
use eyre::{Result, bail};

#[derive(Args)]
pub(crate) struct Cookies {
    /// Arguments accepted by builds with the `browser` feature.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    _arguments: Vec<OsString>,
}

impl Cookies {
    pub(crate) async fn run(self) -> Result<()> {
        bail!(
            "`nanocodex cookies` is not included in this build. Rebuild the CLI with the browser utilities (`cargo build -p nanocodex-bin --features browser`), or capture the current site's cookies into Vault with the Nanocodex Chrome extension."
        )
    }
}
