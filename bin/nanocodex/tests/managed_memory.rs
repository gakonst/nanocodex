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
            .join("nanocodex-managed-memory-tests")
            .join("auth.json"))
    }

    pub(crate) fn default_codex_home() -> eyre::Result<PathBuf> {
        Ok(std::env::temp_dir().join("nanocodex-managed-memory-tests"))
    }
}

#[path = "../src/login.rs"]
mod login;

#[path = "../src/managed_memory.rs"]
mod managed_memory;
