//! Workspace selection for the headless client.

use std::{
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use nanocodex_tools::attachment::{AttachmentMachine, AttachmentMetadata};
use serde::Deserialize;

const MACHINE_CAPABILITIES: [&str; 5] = ["native", "filesystem", "process", "package", "server"];
static MACHINE_ID: OnceLock<String> = OnceLock::new();

#[derive(Debug)]
pub(crate) struct HostConfig {
    workspace: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HostConfigError {
    #[error("could not determine the config directory; set NANOCODEX_HOME")]
    HomeUnavailable,
    #[error("failed to determine the current directory: {0}")]
    CurrentDirectory(#[source] std::io::Error),
    #[error("failed to read configuration file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse configuration file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid local attachment metadata: {0}")]
    Attachment(String),
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    agent: AgentConfigFile,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AgentConfigFile {
    workspace: Option<PathBuf>,
}

impl HostConfig {
    pub(crate) fn load() -> Result<Self, HostConfigError> {
        let current_dir = env::current_dir().map_err(HostConfigError::CurrentDirectory)?;
        let config_path = config_path()?;
        let config = ConfigFile::read(&config_path)?;
        let config_dir = config_path.parent().unwrap_or(Path::new("."));
        let workspace = config
            .agent
            .workspace
            .map(|path| resolve_path(path, config_dir))
            .unwrap_or(current_dir);
        Ok(Self { workspace })
    }

    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn attachment_metadata(&self) -> Result<AttachmentMetadata, HostConfigError> {
        let id = MACHINE_ID
            .get_or_init(|| uuid::Uuid::new_v4().to_string())
            .clone();
        let name = bounded_display_name(whoami::devicename());
        let workspace = self.workspace.to_string_lossy().into_owned();
        AttachmentMachine::new(id, name, workspace, MACHINE_CAPABILITIES)
            .map(AttachmentMetadata::machine)
            .map_err(|error| HostConfigError::Attachment(error.to_string()))
    }
}

fn bounded_display_name(mut name: String) -> String {
    if name.trim().is_empty() {
        return "Local machine".to_owned();
    }
    while name.len() > 128 {
        name.pop();
    }
    name
}

impl ConfigFile {
    fn read(path: &Path) -> Result<Self, HostConfigError> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(source) if source.kind() == ErrorKind::NotFound => String::new(),
            Err(source) => {
                return Err(HostConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        toml::from_str(&contents).map_err(|source| HostConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }
}

fn config_path() -> Result<PathBuf, HostConfigError> {
    if let Some(home) = env::var_os("NANOCODEX_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home).join("config.toml"));
    }
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".nanocodex2/config.toml"))
        .ok_or(HostConfigError::HomeUnavailable)
}

fn resolve_path(path: PathBuf, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}
