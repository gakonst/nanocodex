use clap::{ArgAction, Args};
use eyre::Result;

#[derive(Args, Clone)]
pub(crate) struct MppArgs {
    /// Connect directly to `OpenAI`. This is the default provider.
    #[arg(
        long = "provider.openai",
        id = "provider_openai",
        global = true,
        env = "NANOCODEX_PROVIDER_OPENAI",
        default_value_t = false,
        action = ArgAction::SetTrue
    )]
    _openai: bool,
}

impl MppArgs {
    pub(crate) const fn is_enabled(&self) -> bool {
        false
    }

    pub(crate) async fn start(self) -> Result<Option<MppAdapter>> {
        Ok(None)
    }
}

pub(crate) enum MppAdapter {}

impl MppAdapter {
    pub(crate) const fn api_base_url(&self) -> &str {
        match *self {}
    }

    pub(crate) const fn tool_environment(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        match *self {}
    }

    pub(crate) const fn responses_http_client(&self) -> Result<reqwest::Client> {
        match *self {}
    }

    pub(crate) const fn tool_http_client(&self) -> Result<reqwest::Client> {
        match *self {}
    }

    pub(crate) async fn shutdown(self) -> Result<()> {
        match self {}
    }
}
