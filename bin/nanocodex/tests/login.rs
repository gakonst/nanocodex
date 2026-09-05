#![allow(dead_code)]

mod auth {
    pub(crate) const fn open_browser(_url: &str) -> std::io::Result<()> {
        Ok(())
    }
}

mod config {
    use std::path::PathBuf;

    pub(crate) fn default_auth_file() -> eyre::Result<PathBuf> {
        Ok(std::env::temp_dir()
            .join("nanocodex-login-tests")
            .join("auth.json"))
    }

    pub(crate) fn default_codex_home() -> eyre::Result<PathBuf> {
        Ok(std::env::temp_dir().join("nanocodex-login-tests"))
    }
}

mod mpp {
    pub(crate) enum MppAdapter {}

    impl MppAdapter {
        pub(crate) fn mcp_payment_provider(
            &self,
            _url: &str,
        ) -> eyre::Result<std::sync::Arc<dyn nanocodex::tools::mcp::McpPaymentProvider>> {
            match *self {}
        }
    }
}

#[path = "../src/login.rs"]
mod login;
#[path = "../src/mcp.rs"]
mod mcp;
