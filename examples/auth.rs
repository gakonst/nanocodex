use std::path::PathBuf;

use eyre::{Result, eyre};
use nanocodex::oai::auth::{OpenAiAuth, chatgpt_access_token, load_chatgpt_auth};

pub(crate) fn load_codex_auth() -> Result<OpenAiAuth> {
    if let Ok(access_token) = std::env::var("CODEX_ACCESS_TOKEN")
        && !access_token.trim().is_empty()
    {
        return chatgpt_access_token(access_token.trim().to_owned()).map_err(Into::into);
    }
    let auth_file = default_auth_file()?;
    load_chatgpt_auth(&auth_file).map_err(|error| {
        eyre!(
            "ChatGPT authorization could not be loaded from {}: {error}. Run `nanocodex auth login`",
            auth_file.display()
        )
    })
}

fn default_auth_file() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NANOCODEX_AUTH_FILE") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("CODEX_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path).join("auth.json"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| {
            eyre!("home directory is unavailable; set CODEX_HOME or NANOCODEX_AUTH_FILE")
        })?;
    Ok(PathBuf::from(home).join(".codex/auth.json"))
}
