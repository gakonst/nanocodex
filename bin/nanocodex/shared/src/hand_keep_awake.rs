//! Credential-free, persistent macOS Hand power preference and daemon receipt.
use serde_json::{Value, json};
use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};

pub const ENVIRONMENT: &str = "NANOCODEX_HAND_KEEP_AWAKE";

pub fn setting_path(home: &Path) -> PathBuf {
    home.join(".nanocodex/hand-keep-awake.json")
}
pub fn state_path(home: &Path) -> PathBuf {
    home.join(".nanocodex/hand-keep-awake.state.json")
}

pub fn configured(home: &Path) -> io::Result<bool> {
    match fs::read(setting_path(home)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .ok().and_then(|value| value["enabled"].as_bool())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData,
                "Hand keep-awake setting must contain {\"enabled\": true} or {\"enabled\": false}")),
    }
}

/// Atomic replacement, never truncate a preference the daemon is reading.
pub fn write(path: &Path, value: &Value) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing preference directory"))?;
    let mut builder = fs::DirBuilder::new();
    use std::os::unix::fs::DirBuilderExt;
    builder.recursive(true).mode(0o700).create(parent)?;
    if !fs::symlink_metadata(parent)?.is_dir() {
        return Err(io::Error::other(
            "Hand preferences require a real directory",
        ));
    }
    if let Ok(metadata) = fs::symlink_metadata(path)
        && !metadata.is_file()
    {
        return Err(io::Error::other("Hand preference must be a regular file"));
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Accept observations only from the owner currently reported by launchd.
pub fn snapshot(home: &Path, pid: Option<u32>) -> io::Result<Value> {
    let configured = configured(home)?;
    let observed = fs::read(state_path(home))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|state| pid.is_some() && state["daemon_pid"].as_u64() == pid.map(u64::from));
    let overridden = observed
        .as_ref()
        .and_then(|state| state["environment_override"].as_bool());
    let applied = observed
        .as_ref()
        .filter(|state| state["configured"] == configured);
    Ok(json!({
        "configured": configured,
        "enabled": configured && overridden != Some(true),
        "active": applied.and_then(|state| state["active"].as_bool()),
        "can_change": (pid.is_none() || observed.is_some()) && overridden != Some(true),
        "environment_override": overridden,
        "daemon_pid": pid,
        "supported_daemon": observed.is_some(),
        "error": observed.as_ref().and_then(|state| state["error"].as_str()),
    }))
}
